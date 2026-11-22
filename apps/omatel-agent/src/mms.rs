// SPDX-License-Identifier: GPL-3.0-or-later
//! MMS through mmsd-tng, which serves `org.ofono.mms` on the session bus
//! and does the network side (the WAP push, the MMSC over the mobile data
//! bearer). The agent is its client:
//!
//! - **Receiving.** Each message mmsd-tng reports (`GetMessages` when it
//!   appears, then `MessageAdded`) is stored like an SMS: its text from
//!   the text/plain parts, every other part copied out of mmsd-tng's PDU
//!   file into `parts/<uuid>/` in the data directory. Once committed, the
//!   message is deleted from mmsd-tng, as an SMS is from the modem: a
//!   crash in between stores nothing twice, because the store keys MMS by
//!   mmsd-tng's object path.
//! - **Groups.** The conversation is everyone on the message (the sender
//!   and the recipients) but the phone's own numbers, which mmsd-tng
//!   reports as `Modem Number` and ModemManager as `OwnNumbers`.
//! - **Sending.** Text and attachments go to `SendMessage` (mmsd-tng
//!   builds the SMIL); the stored message follows the status mmsd-tng
//!   signals (`sent`, `sending_failed`, `delivered`) and is deleted there
//!   once nothing more can happen to it.
//! - **Deleting.** The apps delete messages straight in the store, which
//!   lists their mmsd-tng paths and files by trigger; [`sweep_mms`] and
//!   [`sweep_files`] act on those lists.
//!
//! Logs carry object paths and counts, never a number or text.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use omatel_contacts::number;
use omatel_store::{Direction, MessageState, NewMms, NewPart};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{MatchRule, MessageStream};

use crate::{iface, sms, Agent};

pub const SERVICE: &str = "org.ofono.mms";
pub const ROOT: &str = "/org/ofono/mms";
const IFACE_MANAGER: &str = "org.ofono.mms.Manager";
const IFACE_SERVICE: &str = "org.ofono.mms.Service";
const IFACE_MESSAGE: &str = "org.ofono.mms.Message";

/// How long a sent MMS stays in mmsd-tng for its delivery report.
const DELIVERY_WAIT_MS: i64 = 48 * 3600 * 1000;
/// How often deleted messages are cleaned up in mmsd-tng and on disk.
const SWEEP_EVERY: Duration = Duration::from_secs(600);
/// A part bigger than this is not an MMS part; it isn't copied.
const MAX_PART: u64 = 64 << 20;

/// Why a message failed, as the stored message (and Messages) shows it.
pub const NOT_RUNNING: &str = "The MMS service (mmsd-tng) isn't running";
/// Why an MMS is refused on a phone with no MMS service at all.
pub const NO_SERVICE: &str = "This phone has no MMS service (mmsd-tng isn't installed)";

type Props = HashMap<String, OwnedValue>;

#[zbus::proxy(
    interface = "org.ofono.mms.Manager",
    default_service = "org.ofono.mms",
    default_path = "/org/ofono/mms"
)]
trait MmsManager {
    fn get_services(&self) -> zbus::Result<Vec<(OwnedObjectPath, Props)>>;
}

#[zbus::proxy(interface = "org.ofono.mms.Service", default_service = "org.ofono.mms")]
trait MmsService {
    fn get_messages(&self) -> zbus::Result<Vec<(OwnedObjectPath, Props)>>;
    fn get_properties(&self) -> zbus::Result<Props>;
    fn send_message(
        &self,
        recipients: &[&str],
        options: &Value<'_>,
        attachments: &[(&str, &str, &str)],
    ) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(interface = "org.ofono.mms.Message", default_service = "org.ofono.mms")]
trait MmsMessage {
    fn delete(&self) -> zbus::Result<()>;
}

/// What the agent knows of mmsd-tng while it runs.
#[derive(Default)]
pub(crate) struct MmsState {
    /// Its services (the ModemManager plugin's: `/org/ofono/mms/modemmanager`).
    pub services: Vec<OwnedObjectPath>,
    /// The messages it holds now.
    pub held: HashSet<String>,
    /// Sent messages waiting for a delivery report, with when they went
    /// (ms): they stay in mmsd-tng until the report or the deadline.
    pub reports: HashMap<String, i64>,
    /// Its `Modem Number`s: the phone's own numbers, for group members.
    pub own: HashSet<String>,
}

/// Follow mmsd-tng: attach whenever it runs (it may start after the agent).
pub async fn watch(agent: Arc<Agent>) {
    loop {
        check_installed(&agent).await;
        match follow(&agent).await {
            Ok(()) => tracing::info!("MMS service gone"),
            Err(e) => tracing::warn!(error = %e, "MMS service watch ended"),
        }
        detach(&agent).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Delete the files of deleted messages now and then, mmsd-tng or not.
pub async fn sweep_files_now_and_then(agent: Arc<Agent>) {
    loop {
        sweep_files(&agent);
        tokio::time::sleep(SWEEP_EVERY).await;
    }
}

async fn follow(agent: &Arc<Agent>) -> anyhow::Result<()> {
    let dbus = zbus::fdo::DBusProxy::new(&agent.session).await?;
    let mut owners = dbus
        .receive_name_owner_changed_with_args(&[(0, SERVICE)])
        .await?;
    let owner = loop {
        if let Ok(o) = dbus.get_name_owner(SERVICE.try_into()?).await {
            break o.to_string();
        }
        match owners.next().await {
            Some(_) => continue,
            None => anyhow::bail!("the session bus went away"),
        }
    };
    // Subscribed before listing, so nothing falls between the two.
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(owner.as_str())?
        .path_namespace(ROOT)?
        .build();
    let mut signals = MessageStream::for_match_rule(rule, &agent.session, Some(256)).await?;
    let manager = MmsManagerProxy::new(&agent.session).await?;
    for (path, _) in manager.get_services().await? {
        attach_service(agent, path).await?;
    }
    sweep_mms(agent).await;
    let mut sweeps =
        tokio::time::interval_at(tokio::time::Instant::now() + SWEEP_EVERY, SWEEP_EVERY);
    loop {
        tokio::select! {
            Some(msg) = signals.next() => {
                if let Ok(m) = msg {
                    on_signal(agent, &m).await;
                }
            }
            Some(sig) = owners.next() => {
                if let Ok(a) = sig.args() {
                    if a.new_owner().as_ref().map(|o| o.as_str()) != Some(owner.as_str()) {
                        return Ok(());
                    }
                }
            }
            _ = sweeps.tick() => sweep_mms(agent).await,
            else => return Ok(()),
        }
    }
}

async fn detach(agent: &Arc<Agent>) {
    let was = {
        let mut s = agent.mms.lock().unwrap();
        let was = !s.services.is_empty();
        *s = MmsState::default();
        was
    };
    if was {
        iface::mms_ready_changed(agent).await;
    }
}

/// Whether an MMS can be handed to mmsd-tng now.
pub fn ready(agent: &Agent) -> bool {
    !agent.mms.lock().unwrap().services.is_empty()
}

/// Whether this phone has an MMS service at all: mmsd-tng runs now, or is
/// installed (its user unit in one of `Config::mms_units`, or its bus name
/// activatable). Without one (no MMS stack on the phone), Messages shows no
/// MMS, and the agent refuses MMS up front instead of keeping a message
/// that can never go as failed. With one that isn't running yet, an MMS is
/// kept as failed for Retry.
pub fn available(agent: &Agent) -> bool {
    ready(agent) || agent.mms_installed.load(Ordering::Relaxed)
}

/// The places mmsd-tng's user unit is installed to: the package's, the
/// administrator's and the user's own.
pub fn unit_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr/lib/systemd/user/mmsd-tng.service"),
        PathBuf::from("/etc/systemd/user/mmsd-tng.service"),
    ];
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(c) = config {
        paths.push(c.join("systemd/user/mmsd-tng.service"));
    }
    paths
}

/// Look for an installed mmsd-tng again, and announce a change.
async fn check_installed(agent: &Arc<Agent>) {
    let unit = agent.cfg.mms_units.iter().any(|p| p.exists());
    let activatable = match zbus::fdo::DBusProxy::new(&agent.session).await {
        Ok(dbus) => dbus
            .list_activatable_names()
            .await
            .map(|names| names.iter().any(|n| n.as_str() == SERVICE))
            .unwrap_or(false),
        Err(_) => false,
    };
    let now = unit || activatable;
    if agent.mms_installed.swap(now, Ordering::Relaxed) != now {
        iface::mms_ready_changed(agent).await;
    }
}

async fn attach_service(agent: &Arc<Agent>, path: OwnedObjectPath) -> anyhow::Result<()> {
    if agent.mms.lock().unwrap().services.contains(&path) {
        return Ok(());
    }
    let service = MmsServiceProxy::builder(&agent.session)
        .path(path.clone())?
        .build()
        .await?;
    let messages = service.get_messages().await?;
    agent.mms.lock().unwrap().services.push(path.clone());
    tracing::info!(path = %path, messages = messages.len(), "MMS service attached");
    iface::mms_ready_changed(agent).await;
    for (p, props) in &messages {
        agent.mms.lock().unwrap().held.insert(p.to_string());
        handle_message(agent, p.as_str(), props, false).await;
    }
    // Ours that mmsd-tng no longer has (it lost its store): not sent.
    let held: HashSet<String> = messages.iter().map(|(p, _)| p.to_string()).collect();
    let in_flight = agent
        .messages
        .lock()
        .unwrap()
        .mms_in_flight()
        .unwrap_or_default();
    for (id, p) in in_flight {
        if p.starts_with(path.as_str()) && !held.contains(&p) {
            fail(agent, id, "The MMS service lost it").await;
        }
    }
    Ok(())
}

async fn on_signal(agent: &Arc<Agent>, msg: &zbus::Message) {
    let header = msg.header();
    let (Some(iface), Some(member), Some(path)) =
        (header.interface(), header.member(), header.path())
    else {
        return;
    };
    let body = msg.body();
    match (iface.as_str(), member.as_str()) {
        (IFACE_MANAGER, "ServiceAdded") => {
            if let Ok((p, _)) = body.deserialize::<(OwnedObjectPath, Props)>() {
                if let Err(e) = attach_service(agent, p).await {
                    tracing::warn!(error = %e, "MMS service not attached");
                }
            }
        }
        (IFACE_MANAGER, "ServiceRemoved") => {
            if let Ok(p) = body.deserialize::<OwnedObjectPath>() {
                agent.mms.lock().unwrap().services.retain(|s| *s != p);
                iface::mms_ready_changed(agent).await;
            }
        }
        (IFACE_SERVICE, "MessageAdded") => {
            if let Ok((p, props)) = body.deserialize::<(OwnedObjectPath, Props)>() {
                agent.mms.lock().unwrap().held.insert(p.to_string());
                handle_message(agent, p.as_str(), &props, true).await;
            }
        }
        (IFACE_SERVICE, "MessageRemoved") => {
            if let Ok(p) = body.deserialize::<OwnedObjectPath>() {
                let mut s = agent.mms.lock().unwrap();
                s.held.remove(p.as_str());
                s.reports.remove(p.as_str());
            }
        }
        (IFACE_SERVICE, "MessageSendError") => {
            if let Ok(d) = body.deserialize::<Props>() {
                let p = prop_str(&d, "MessagePath");
                let found = agent
                    .messages
                    .lock()
                    .unwrap()
                    .by_mms_path(&p)
                    .ok()
                    .flatten();
                if let Some(m) = found {
                    tracing::info!(path = %p, "MMS center unreachable");
                    fail(
                        agent,
                        m.id,
                        "Couldn't reach the MMS center; check the MMS settings",
                    )
                    .await;
                }
            }
        }
        (IFACE_SERVICE, "MessageReceiveError") => {
            // "From" is a number: not logged.
            tracing::warn!("an MMS couldn't be downloaded from the MMS center");
        }
        (IFACE_MESSAGE, "PropertyChanged") => {
            if let Ok((name, value)) = body.deserialize::<(String, OwnedValue)>() {
                // mmsd-tng sends "status" (its documentation says "Status").
                if name.eq_ignore_ascii_case("status") {
                    if let Ok(s) = <&str>::try_from(&value) {
                        status_changed(agent, path.as_str(), s).await;
                    }
                }
            }
        }
        _ => {}
    }
}

/// A message mmsd-tng reported. `live`: from MessageAdded (it arrived
/// now), not from the list at start-up.
async fn handle_message(agent: &Arc<Agent>, path: &str, props: &Props, live: bool) {
    let own = prop_str(props, "Modem Number");
    if !own.is_empty() {
        agent.mms.lock().unwrap().own.insert(own);
    }
    let status = prop_str(props, "Status");
    let result = match status.as_str() {
        "received" => incoming(agent, path, props, live).await,
        "draft" | "sent" | "sending_failed" | "delivered" => {
            outgoing(agent, path, props, &status).await
        }
        // A notification not downloaded yet has no properties.
        "" => Ok(()),
        other => {
            tracing::info!(path, status = other, "MMS not downloaded; left in mmsd-tng");
            Ok(())
        }
    };
    if let Err(e) = result {
        tracing::warn!(path, error = %e, "MMS not stored; it stays in mmsd-tng");
    }
}

async fn incoming(agent: &Arc<Agent>, path: &str, props: &Props, live: bool) -> anyhow::Result<()> {
    let (stored, forgotten) = known(agent, path);
    if stored || forgotten {
        // Stored before a restart, or deleted here: mmsd-tng's copy goes.
        delete_held(agent, path).await;
        return Ok(());
    }
    let region = agent.region();
    let sender = prop_str(props, "Sender");
    let recipients = prop_strs(props, "Recipients");
    let members = members(&sender, &recipients, &own_numbers(agent, &region), &region);
    let group = members.len() > 1;
    let conversation = agent
        .messages
        .lock()
        .unwrap()
        .conversation_for_members(&members)?;
    let (body, parts) = content(&agent.cfg.data_dir, path, &attachments(props))?;
    let sent_at = parse_date(&prop_str(props, "Date"));
    let now = crate::util::now_ms();
    let at = if live {
        now
    } else {
        sent_at.filter(|t| *t <= now).unwrap_or(now)
    };
    let from = number::normalize(&sender, &region);
    let stored = agent.messages.lock().unwrap().insert_mms(
        &NewMms {
            conversation,
            direction: Direction::In,
            sender: if group { &from } else { "" },
            body: &body,
            at,
            sent_at,
            state: MessageState::Received,
            mms_path: path,
        },
        &parts,
    )?;
    // On disk now (synchronous=FULL): mmsd-tng's copy can go.
    delete_held(agent, path).await;
    if let Some(id) = stored {
        tracing::info!(
            path,
            conversation,
            message_id = id,
            parts = parts.len(),
            "MMS stored"
        );
        iface::messages_changed(agent, conversation).await;
        sms::notify(agent, conversation).await;
    }
    Ok(())
}

/// One of ours (or another client's) that mmsd-tng sends.
async fn outgoing(
    agent: &Arc<Agent>,
    path: &str,
    props: &Props,
    status: &str,
) -> anyhow::Result<()> {
    // A send in progress records its path when SendMessage returns, and
    // mmsd-tng signals the new message before that: wait for it.
    let _sending = agent.mms_send.lock().await;
    if props_bool(props, "Delivery Report") && status == "sent" {
        let since = parse_date(&prop_str(props, "Date")).unwrap_or_else(crate::util::now_ms);
        agent
            .mms
            .lock()
            .unwrap()
            .reports
            .entry(path.to_string())
            .or_insert(since);
    }
    let (stored, forgotten) = known(agent, path);
    if forgotten {
        delete_held(agent, path).await;
        return Ok(());
    }
    if !stored {
        import_outgoing(agent, path, props, status)?;
    }
    status_changed(agent, path, status).await;
    Ok(())
}

/// An MMS another client sent through mmsd-tng: shown in its thread.
fn import_outgoing(
    agent: &Arc<Agent>,
    path: &str,
    props: &Props,
    status: &str,
) -> anyhow::Result<()> {
    let region = agent.region();
    let own = own_numbers(agent, &region);
    let members = members("", &prop_strs(props, "Recipients"), &own, &region);
    let store = agent.messages.lock().unwrap();
    let conversation = store.conversation_for_members(&members)?;
    let (body, parts) = content(&agent.cfg.data_dir, path, &attachments(props))?;
    let sent_at = parse_date(&prop_str(props, "Date"));
    store.insert_mms(
        &NewMms {
            conversation,
            direction: Direction::Out,
            sender: "",
            body: &body,
            at: sent_at.unwrap_or_else(crate::util::now_ms),
            sent_at,
            state: state_for(status).unwrap_or(MessageState::Sending),
            mms_path: path,
        },
        &parts,
    )?;
    tracing::info!(path, conversation, "MMS sent elsewhere, stored");
    Ok(())
}

fn state_for(status: &str) -> Option<MessageState> {
    match status {
        "draft" => Some(MessageState::Sending),
        "sent" => Some(MessageState::Sent),
        "delivered" => Some(MessageState::Delivered),
        "sending_failed" => Some(MessageState::Failed),
        _ => None,
    }
}

/// mmsd-tng's status for one of our messages changed.
async fn status_changed(agent: &Arc<Agent>, path: &str, status: &str) {
    let Some(state) = state_for(status) else {
        // "delivery_update,<number>=<status>" per recipient, and "read".
        return;
    };
    let found = agent
        .messages
        .lock()
        .unwrap()
        .by_mms_path(path)
        .ok()
        .flatten();
    let Some(m) = found else { return };
    if m.direction != Direction::Out {
        return;
    }
    // A failure already explained (MessageSendError) keeps its reason.
    if m.state != state && !(m.state == MessageState::Failed && state == MessageState::Failed) {
        tracing::info!(
            path,
            message_id = m.id,
            state = state.as_str(),
            "MMS status"
        );
        let why = (state == MessageState::Failed).then_some("The MMS center didn't take it");
        let _ = agent.messages.lock().unwrap().set_state(m.id, state, why);
        iface::messages_changed(agent, m.conversation_id).await;
    }
    let done = match state {
        MessageState::Sent => !agent.mms.lock().unwrap().reports.contains_key(path),
        MessageState::Delivered | MessageState::Failed => true,
        _ => false,
    };
    if done {
        delete_held(agent, path).await;
    }
}

/// (stored here, deleted here) for an mmsd-tng path.
fn known(agent: &Agent, path: &str) -> (bool, bool) {
    let store = agent.messages.lock().unwrap();
    let stored = store.by_mms_path(path).ok().flatten().is_some();
    let forgotten = store
        .forgotten_mms()
        .unwrap_or_default()
        .iter()
        .any(|p| p == path);
    (stored, forgotten)
}

/// Delete a message from mmsd-tng, if it still has it.
async fn delete_held(agent: &Agent, path: &str) {
    if !agent.mms.lock().unwrap().held.contains(path) {
        return;
    }
    match message_proxy(agent, path).await {
        Ok(p) => match p.delete().await {
            Ok(()) => {
                let mut s = agent.mms.lock().unwrap();
                s.held.remove(path);
                s.reports.remove(path);
            }
            Err(e) => tracing::warn!(path, error = %e, "MMS not deleted from mmsd-tng"),
        },
        Err(e) => tracing::warn!(path, error = %e, "MMS not deleted from mmsd-tng"),
    }
}

async fn message_proxy(agent: &Agent, path: &str) -> zbus::Result<MmsMessageProxy<'static>> {
    MmsMessageProxy::builder(&agent.session)
        .path(path.to_string())?
        .build()
        .await
}

/// Delete from mmsd-tng what was deleted here, and sent messages whose
/// delivery report never came.
pub async fn sweep_mms(agent: &Arc<Agent>) {
    if !ready(agent) {
        return;
    }
    let forgotten = agent
        .messages
        .lock()
        .unwrap()
        .forgotten_mms()
        .unwrap_or_default();
    for p in forgotten {
        delete_held(agent, &p).await;
        if !agent.mms.lock().unwrap().held.contains(&p) {
            let _ = agent.messages.lock().unwrap().drop_forgotten_mms(&p);
        }
    }
    let now = crate::util::now_ms();
    let overdue: Vec<String> = agent
        .mms
        .lock()
        .unwrap()
        .reports
        .iter()
        .filter(|(_, since)| now - **since > DELIVERY_WAIT_MS)
        .map(|(p, _)| p.clone())
        .collect();
    for p in overdue {
        tracing::info!(path = %p, "no MMS delivery report");
        agent.mms.lock().unwrap().reports.remove(&p);
        delete_held(agent, &p).await;
    }
}

/// Delete the attachment files of deleted messages (and their emptied
/// directories). Only files under `parts/` are ever touched.
pub fn sweep_files(agent: &Agent) {
    let files = agent
        .messages
        .lock()
        .unwrap()
        .forgotten_files()
        .unwrap_or_default();
    for f in files {
        if is_part_file(&f) {
            let path = agent.cfg.data_dir.join(&f);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(error = %e, "attachment not deleted");
                    continue;
                }
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir); // only when empty
            }
        }
        let _ = agent.messages.lock().unwrap().drop_forgotten_file(&f);
    }
}

fn is_part_file(f: &str) -> bool {
    f.starts_with("parts/") && !f.split('/').any(|c| c == ".." || c.is_empty())
}

// ---- sending -----------------------------------------------------------

#[derive(Debug)]
pub enum SendError {
    Store(omatel_store::Error),
    /// The caller's mistake: a file that isn't there, a missing type.
    Invalid(String),
}

impl From<omatel_store::Error> for SendError {
    fn from(e: omatel_store::Error) -> SendError {
        SendError::Store(e)
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Store(e) => write!(f, "message store: {e}"),
            SendError::Invalid(m) => f.write_str(m),
        }
    }
}

/// Queue an MMS to a conversation: stored with copies of the attachment
/// files (path, MIME type), then sent in the background.
pub async fn send(
    agent: &Arc<Agent>,
    conversation: i64,
    text: &str,
    files: &[(String, String)],
) -> Result<i64, SendError> {
    for (path, mime) in files {
        let ok = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0);
        if !ok {
            return Err(SendError::Invalid(format!(
                "Can't read the attachment {}",
                file_name(path)
            )));
        }
        if !mime.contains('/') {
            return Err(SendError::Invalid("An attachment has no MIME type".into()));
        }
    }
    let id = agent
        .messages
        .lock()
        .unwrap()
        .insert_outgoing_mms(conversation, text)?;
    match copy_in(&agent.cfg.data_dir, id, files) {
        Ok(parts) => agent.messages.lock().unwrap().add_parts(id, &parts)?,
        Err(e) => {
            tracing::warn!(error = %e, "attachment not copied");
            let _ = agent.messages.lock().unwrap().delete_message(id);
            sweep_files(agent);
            return Err(SendError::Invalid("Can't read an attachment".into()));
        }
    }
    iface::messages_changed(agent, conversation).await;
    tokio::spawn(deliver(agent.clone(), conversation, id));
    Ok(id)
}

/// Send a failed MMS again, in its place in the thread.
pub async fn retry(agent: &Arc<Agent>, id: i64, conversation: i64) {
    let old = agent.messages.lock().unwrap().mms_path(id).ok().flatten();
    let _ = agent.messages.lock().unwrap().forget_mms_path(id);
    if let Some(p) = old {
        delete_held(agent, &p).await;
    }
    let _ = agent
        .messages
        .lock()
        .unwrap()
        .set_state(id, MessageState::Sending, None);
    iface::messages_changed(agent, conversation).await;
    tokio::spawn(deliver(agent.clone(), conversation, id));
}

async fn fail(agent: &Arc<Agent>, id: i64, why: &str) {
    tracing::info!(message_id = id, why, "MMS not sent");
    let conversation = {
        let store = agent.messages.lock().unwrap();
        let _ = store.set_state(id, MessageState::Failed, Some(why));
        store.message(id).ok().flatten().map(|m| m.conversation_id)
    };
    if let Some(c) = conversation {
        iface::messages_changed(agent, c).await;
    }
}

pub(crate) async fn deliver(agent: Arc<Agent>, conversation: i64, id: i64) {
    let (numbers, body, parts) = {
        let store = agent.messages.lock().unwrap();
        let c = store.conversation(conversation).ok().flatten();
        let m = store.message(id).ok().flatten();
        let parts = store.parts(id).unwrap_or_default();
        match (c, m) {
            (Some(c), Some(m)) => (c.numbers, m.body, parts),
            _ => return,
        }
    };
    if numbers.is_empty() {
        return fail(&agent, id, "No recipient").await;
    }
    let service = agent.mms.lock().unwrap().services.first().cloned();
    let Some(service) = service else {
        return fail(&agent, id, NOT_RUNNING).await;
    };
    let proxy = match MmsServiceProxy::builder(&agent.session)
        .path(service)
        .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
    {
        Ok(b) => b.build().await,
        Err(e) => Err(e),
    };
    let Ok(proxy) = proxy else {
        return fail(&agent, id, NOT_RUNNING).await;
    };
    let size: i64 = parts.iter().map(|p| p.size).sum::<i64>() + body.len() as i64;
    if let Some(why) = over_limits(&proxy, size, parts.len()).await {
        return fail(&agent, id, &why).await;
    }
    let dir = agent.cfg.data_dir.join(format!("parts/out-{id}"));
    let mut files: Vec<(String, String, String)> = Vec::new();
    let text_file = dir.join("text.txt");
    if !body.trim().is_empty() {
        if let Err(e) = write_private(&text_file, body.as_bytes()) {
            tracing::warn!(error = %e, "MMS text not written");
            return fail(&agent, id, "Couldn't prepare the message").await;
        }
        files.push((
            "text.txt".into(),
            "text/plain".into(),
            text_file.to_string_lossy().into_owned(),
        ));
    }
    for p in &parts {
        let path = p.path(&agent.cfg.data_dir);
        files.push((
            file_name(&p.file).to_string(),
            p.mime.clone(),
            path.to_string_lossy().into_owned(),
        ));
    }
    let report = agent.settings().delivery_reports;
    let options = Value::from(HashMap::from([("DeliveryReport", Value::from(report))]));
    let recipients: Vec<&str> = numbers.iter().map(String::as_str).collect();
    let attachments: Vec<(&str, &str, &str)> = files
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    let result = {
        let _sending = agent.mms_send.lock().await;
        let r = proxy
            .send_message(&recipients, &options, &attachments)
            .await;
        if let Ok(path) = &r {
            let _ = agent
                .messages
                .lock()
                .unwrap()
                .set_mms_path(id, path.as_str());
            if report {
                agent
                    .mms
                    .lock()
                    .unwrap()
                    .reports
                    .insert(path.to_string(), crate::util::now_ms());
            }
        }
        r
    };
    // mmsd-tng read the files during the call; the text copy can go.
    let _ = std::fs::remove_file(&text_file);
    match result {
        Ok(path) => tracing::info!(path = %path, message_id = id, "MMS handed to mmsd-tng"),
        Err(e) => {
            tracing::warn!(message_id = id, error = %e, "mmsd-tng refused the MMS");
            fail(&agent, id, &refusal(&e)).await;
        }
    }
}

/// The carrier's limits as mmsd-tng knows them, checked first so the
/// reason is clear (mmsd-tng only answers "invalid arguments").
async fn over_limits(proxy: &MmsServiceProxy<'_>, size: i64, count: usize) -> Option<String> {
    let props = proxy.get_properties().await.ok()?;
    let max_size = props
        .get("TotalMaxAttachmentSize")
        .and_then(|v| i32::try_from(v).ok());
    let max_count = props
        .get("MaxAttachments")
        .and_then(|v| i32::try_from(v).ok());
    if let Some(max) = max_size.filter(|m| *m > 0 && size > *m as i64) {
        return Some(format!(
            "Too big for MMS: {} KB, the limit is {} KB",
            (size + 1023) / 1024,
            max / 1024
        ));
    }
    // The text goes as one more attachment.
    if let Some(max) = max_count.filter(|m| *m > 0 && count + 1 > *m as usize) {
        return Some(format!("Too many attachments: at most {}", max - 1));
    }
    None
}

fn refusal(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) if name.as_str().ends_with("InvalidArgs") => {
            "The MMS service refused it (a number, or the attachments)".into()
        }
        zbus::Error::MethodError(name, _, _)
            if name.as_str().ends_with("ServiceUnknown") || name.as_str().ends_with("NoReply") =>
        {
            NOT_RUNNING.into()
        }
        _ => "The MMS service refused it".into(),
    }
}

/// Copy the caller's files into `parts/out-<id>/`.
fn copy_in(data_dir: &Path, id: i64, files: &[(String, String)]) -> std::io::Result<Vec<NewPart>> {
    let dir = format!("parts/out-{id}");
    let mut used = HashSet::new();
    let mut parts = Vec::new();
    for (path, mime) in files {
        let mime = base_mime(mime);
        let name = unique_name(&safe_name(file_name(path), &mime), &mut used);
        let rel = format!("{dir}/{name}");
        let bytes = std::fs::read(path)?;
        write_private(&data_dir.join(&rel), &bytes)?;
        parts.push(NewPart {
            mime,
            file: rel,
            size: bytes.len() as i64,
        });
    }
    Ok(parts)
}

// ---- reading what mmsd-tng reports ---------------------------------------

/// One entry of a message's `Attachments`: a slice of mmsd-tng's PDU file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attachment {
    pub id: String,
    pub content_type: String,
    pub file: String,
    pub offset: u64,
    pub len: u64,
}

fn prop_str(props: &Props, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or_default()
        .to_string()
}

fn prop_strs(props: &Props, key: &str) -> Vec<String> {
    match props.get(key).map(|v| &**v) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn props_bool(props: &Props, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

pub(crate) fn attachments(props: &Props) -> Vec<Attachment> {
    let Some(Value::Array(a)) = props.get("Attachments").map(|v| &**v) else {
        return Vec::new();
    };
    let s = |v: Option<&Value>| match v {
        Some(Value::Str(s)) => s.to_string(),
        _ => String::new(),
    };
    let n = |v: Option<&Value>| match v {
        Some(Value::U64(n)) => *n,
        _ => 0,
    };
    a.iter()
        .filter_map(|v| match v {
            Value::Structure(st) => {
                let f = st.fields();
                Some(Attachment {
                    id: s(f.first()),
                    content_type: s(f.get(1)),
                    file: s(f.get(2)),
                    offset: n(f.get(3)),
                    len: n(f.get(4)),
                })
            }
            _ => None,
        })
        .collect()
}

/// The message's text (its text/plain parts) and its other parts, copied
/// into `parts/<uuid>/` under `data_dir`.
pub(crate) fn content(
    data_dir: &Path,
    path: &str,
    attachments: &[Attachment],
) -> std::io::Result<(String, Vec<NewPart>)> {
    let dir = format!(
        "parts/{}",
        safe_component(path.rsplit('/').next().unwrap_or(""))
    );
    let mut texts = Vec::new();
    let mut parts = Vec::new();
    let mut used = HashSet::new();
    for a in attachments {
        let (mime, charset) = split_content_type(&a.content_type);
        if mime == "application/smil" {
            continue;
        }
        let bytes = read_slice(Path::new(&a.file), a.offset, a.len)?;
        if mime == "text/plain" {
            texts.push(decode_text(&bytes, charset.as_deref()));
            continue;
        }
        let name = unique_name(&safe_name(&a.id, &mime), &mut used);
        let rel = format!("{dir}/{name}");
        write_private(&data_dir.join(&rel), &bytes)?;
        parts.push(NewPart {
            mime,
            file: rel,
            size: bytes.len() as i64,
        });
    }
    let text = texts
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((text, parts))
}

fn read_slice(file: &Path, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    if len > MAX_PART {
        return Err(std::io::Error::other("part too large"));
    }
    let mut f = std::fs::File::open(file)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0; len as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write a file only its owner can read, creating its directory.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// "text/plain; charset=utf-8" -> ("text/plain", Some("utf-8")); mmsd-tng
/// passes some types on with a `Content-Type: "..."` wrapper.
pub(crate) fn split_content_type(ct: &str) -> (String, Option<String>) {
    let ct = ct.trim();
    let ct = ct.strip_prefix("Content-Type:").unwrap_or(ct).trim();
    let mut fields = ct.split(';');
    let mime = base_mime(fields.next().unwrap_or(""));
    let charset = fields.find_map(|f| {
        let (k, v) = f.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches('"').to_ascii_lowercase())
    });
    (mime, charset)
}

fn base_mime(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_ascii_lowercase()
}

/// Text in the charsets MMS uses: UTF-8 (the default and nearly all),
/// ASCII, Latin-1 and UTF-16 with a byte-order mark.
pub(crate) fn decode_text(bytes: &[u8], charset: Option<&str>) -> String {
    let text = decode_charset(bytes, charset);
    // Some phones end a text part with a NUL.
    text.trim_end_matches('\0').to_string()
}

fn decode_charset(bytes: &[u8], charset: Option<&str>) -> String {
    match charset {
        Some("iso-8859-1" | "latin1" | "windows-1252") => {
            bytes.iter().map(|b| *b as char).collect()
        }
        Some(c) if c.starts_with("utf-16") => {
            let (le, body) = match bytes {
                [0xff, 0xfe, rest @ ..] => (true, rest),
                [0xfe, 0xff, rest @ ..] => (false, rest),
                _ => (c.ends_with("le"), bytes),
            };
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|p| {
                    if le {
                        u16::from_le_bytes([p[0], p[1]])
                    } else {
                        u16::from_be_bytes([p[0], p[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A path component made of safe characters.
fn safe_component(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let out = out.trim_matches('.').to_string();
    if out.is_empty() {
        "part".into()
    } else {
        out
    }
}

/// A file name for a part: its content id cleaned up (they are often
/// "<image1.jpg>" or empty), with an extension from its type if it has none.
pub(crate) fn safe_name(id: &str, mime: &str) -> String {
    let id = file_name(id.trim().trim_matches(['<', '>', '"']));
    let mut name = safe_component(id);
    if !name.contains('.') {
        name.push_str(extension(mime));
    }
    name
}

fn extension(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/heic" => ".heic",
        "image/bmp" => ".bmp",
        "video/mp4" => ".mp4",
        "video/3gpp" => ".3gp",
        "audio/amr" => ".amr",
        "audio/mpeg" => ".mp3",
        "audio/mp4" => ".m4a",
        "text/x-vcard" | "text/vcard" => ".vcf",
        "text/x-vcalendar" | "text/calendar" => ".ics",
        "application/pdf" => ".pdf",
        _ => ".bin",
    }
}

fn unique_name(name: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = name.to_string();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut n = 2;
    while !used.insert(candidate.clone()) {
        candidate = format!("{stem}-{n}{ext}");
        n += 1;
    }
    candidate
}

/// mmsd-tng's dates ("2026-11-14T10:20:48-0400"), ms.
pub(crate) fn parse_date(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M:%S%z")
        .ok()
        .map(|t| t.timestamp_millis())
        .or_else(|| omatel_mm::parse_timestamp(s))
}

fn own_numbers(agent: &Agent, region: &str) -> Vec<String> {
    let mut own: Vec<String> = agent.own_numbers.lock().unwrap().clone();
    own.extend(agent.mms.lock().unwrap().own.iter().cloned());
    own.iter()
        .map(|n| number::normalize(n, region))
        .filter(|n| !n.is_empty())
        .collect()
}

/// The members of an MMS conversation, (normalized, as given): the
/// sender and the recipients but the phone's own numbers. When the own
/// number isn't known, a single recipient is taken to be this phone.
pub(crate) fn members(
    sender: &str,
    recipients: &[String],
    own: &[String],
    region: &str,
) -> Vec<(String, String)> {
    let norm = |raw: &str| number::normalize(raw, region);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |raw: &str| {
        let n = norm(raw);
        if !n.is_empty() && !own.contains(&n) && !out.iter().any(|(k, _)| *k == n) {
            out.push((n, raw.trim().to_string()));
        }
    };
    if !sender.is_empty() {
        add(sender);
    }
    let unknown_self = own.is_empty() && !sender.is_empty() && recipients.len() == 1;
    if !unknown_self {
        for r in recipients {
            add(r);
        }
    }
    if out.is_empty() && !sender.is_empty() {
        out.push((norm(sender), sender.trim().to_string()));
    }
    out
}

/// A one-line stand-in for a message with no text: "Picture" or
/// "Attachment".
pub fn describe_parts(parts: &[omatel_store::Part]) -> &'static str {
    match parts {
        [] => "",
        [p] if p.is_image() => "Picture",
        ps if ps.iter().all(|p| p.is_image()) => "Pictures",
        [_] => "Attachment",
        _ => "Attachments",
    }
}

/// The absolute path of a stored part, for clients.
pub fn part_path(agent: &Agent, file: &str) -> PathBuf {
    agent.cfg.data_dir.join(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(
            split_content_type("text/plain; charset=UTF-8"),
            ("text/plain".into(), Some("utf-8".into()))
        );
        assert_eq!(
            split_content_type("Content-Type: \"text/plain\";charset=utf-8"),
            ("text/plain".into(), Some("utf-8".into()))
        );
        assert_eq!(
            split_content_type("image/JPEG"),
            ("image/jpeg".into(), None)
        );
    }

    #[test]
    fn texts_in_their_charsets() {
        assert_eq!(decode_text(b"hi there\0", None), "hi there");
        assert_eq!(decode_text(b"caf\xe9", Some("iso-8859-1")), "café");
        assert_eq!(decode_text(b"\xff\xfeh\0i\0", Some("utf-16")), "hi");
        assert_eq!(decode_text("ünï".as_bytes(), Some("utf-8")), "ünï");
    }

    #[test]
    fn part_names() {
        assert_eq!(safe_name("<image1.jpg>", "image/jpeg"), "image1.jpg");
        assert_eq!(safe_name("", "image/png"), "part.png");
        assert_eq!(safe_name("../../etc/passwd", "text/x-vcard"), "passwd.vcf");
        assert_eq!(safe_name("my photo", "image/jpeg"), "my_photo.jpg");
        let mut used = HashSet::new();
        assert_eq!(unique_name("a.jpg", &mut used), "a.jpg");
        assert_eq!(unique_name("a.jpg", &mut used), "a-2.jpg");
        assert_eq!(unique_name("a.jpg", &mut used), "a-3.jpg");
        assert!(is_part_file("parts/x/a.jpg"));
        assert!(!is_part_file("parts/../messages.db"));
        assert!(!is_part_file("/etc/passwd"));
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_date("2026-10-14T09:30:00+0000"),
            Some(1_791_970_200_000)
        );
        assert_eq!(
            parse_date("2026-10-14T05:30:00-0400"),
            Some(1_791_970_200_000)
        );
        assert_eq!(parse_date("nonsense"), None);
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn group_members_leave_out_this_phone() {
        let own = s(&["+12025550199"]);
        let m = members(
            "+12025550101",
            &s(&["+12025550199", "+12025550102"]),
            &own,
            "US",
        );
        assert_eq!(
            m.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["+12025550101", "+12025550102"]
        );
        // Sent to this phone alone, own number unknown: one-to-one.
        let m = members("+12025550101", &s(&["+12025550199"]), &[], "US");
        assert_eq!(m.len(), 1);
        // Our own outgoing message: the recipients.
        let m = members("", &s(&["+12025550101", "+12025550102"]), &own, "US");
        assert_eq!(m.len(), 2);
        // Only ourselves (a note to self): still somewhere to put it.
        let m = members("+12025550199", &s(&["+12025550199"]), &own, "US");
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn content_reads_slices_and_skips_smil() {
        let dir = tempfile::tempdir().unwrap();
        let pdu = dir.path().join("pdu");
        std::fs::write(&pdu, b"HEADER<smil/>hello\0PNGDATA").unwrap();
        let f = pdu.to_string_lossy().to_string();
        let att = |id: &str, ct: &str, off: u64, len: u64| Attachment {
            id: id.into(),
            content_type: ct.into(),
            file: f.clone(),
            offset: off,
            len,
        };
        let data = dir.path().join("data");
        let (text, parts) = content(
            &data,
            "/org/ofono/mms/modemmanager/abc123",
            &[
                att("smil", "application/smil", 6, 7),
                att("text.txt", "text/plain;charset=utf-8", 13, 6),
                att("<pic>", "image/png", 19, 7),
            ],
        )
        .unwrap();
        assert_eq!(text, "hello");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].file, "parts/abc123/pic.png");
        assert_eq!(parts[0].size, 7);
        assert_eq!(
            std::fs::read(data.join(&parts[0].file)).unwrap(),
            b"PNGDATA"
        );
    }
}
