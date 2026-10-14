// SPDX-License-Identifier: GPL-3.0-or-later
//! omatel-agent: the one background process behind Phone and Messages
//! (the owner's answer to Q12 of the telephony plan). It runs as a systemd
//! user unit from login and:
//!
//! - watches ModemManager on the system bus (the real one, or Omarchy
//!   Mobile's `omamobile-mmd` bridge) and follows the modem as it comes
//!   and goes;
//! - signals incoming calls, rings (through feedbackd when it runs), raises
//!   the incoming-call notification and opens Phone's answer page;
//! - hangs up on blocked numbers, writes the call log from call state
//!   changes and raises missed-call notifications;
//! - stores every incoming SMS (committed to disk, then deleted from the
//!   modem; duplicates filtered), notifies per conversation with an inline
//!   reply, sends messages and records delivery reports;
//! - serves `org.omamobile.Phone1` and `org.omamobile.Messages1` on the
//!   session bus for the apps and the shell ([`omatel_api`]).
//!
//! Logs carry states, counts and object paths, never a number, a name or
//! message text.

mod audio;
mod calls;
mod iface;
mod sms;
mod util;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use omatel_api::settings::Settings;
use omatel_api::CallInfo;
use omatel_contacts::{number, AddressBook};
use omatel_mm as mm;
use omatel_notify::Notifier;
use omatel_store::{BlockList, CallLog, MessageStore};
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

pub use util::Launcher;

/// How the agent finds out whether the phone is locked (for notification
/// privacy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockProbe {
    /// Ask logind (the user's display session's `LockedHint`) and the
    /// session's `org.freedesktop.ScreenSaver`.
    System,
    /// Tests: always this answer.
    Fixed(bool),
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The stores' directory (`~/.local/share/omatel`).
    pub data_dir: PathBuf,
    /// The address book root (`~/.local/share/contacts`).
    pub contacts_dir: PathBuf,
    /// `~/.config/omatel/settings.toml`.
    pub settings_path: PathBuf,
    /// How to open the apps (answer page, a conversation).
    pub launcher: Launcher,
    pub lock_probe: LockProbe,
}

impl Config {
    pub fn from_environment() -> Config {
        Config {
            data_dir: omatel_store::data_dir(),
            contacts_dir: omatel_contacts::default_root(),
            settings_path: omatel_api::settings::path(),
            launcher: Launcher::Spawn,
            lock_probe: LockProbe::System,
        }
    }
}

/// A call the agent follows.
#[derive(Debug, Clone)]
pub(crate) struct Tracked {
    pub info: CallInfo,
    pub path: OwnedObjectPath,
    /// Declined here (logged as rejected rather than missed).
    pub declined: bool,
    /// From a blocked number: hung up, logged, never shown.
    pub blocked: bool,
    /// Rings silently (an unknown caller, when they don't ring).
    pub silenced: bool,
    /// Post-dial digits ("," part of the dial string), sent once active.
    pub post_dial: String,
    pub feedback: Option<u32>,
    pub notification: Option<u32>,
    pub logged: bool,
}

/// What a notification id stands for, to route its actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Conversation(i64),
    IncomingCall(String),
    MissedCalls { number: String },
}

#[derive(Default)]
pub(crate) struct Notes {
    pub by_id: HashMap<u32, Target>,
    pub by_conversation: HashMap<i64, u32>,
    pub missed: Option<u32>,
}

/// The modem the agent is attached to.
#[derive(Clone)]
pub(crate) struct ModemHandle {
    pub voice: Option<mm::VoiceProxy<'static>>,
    pub messaging: Option<mm::MessagingProxy<'static>>,
}

pub struct Agent {
    pub(crate) cfg: Config,
    pub(crate) system: Connection,
    pub(crate) session: Connection,
    pub(crate) messages: Mutex<MessageStore>,
    pub(crate) log: Mutex<CallLog>,
    pub(crate) blocked: Mutex<BlockList>,
    pub(crate) contacts: Mutex<AddressBook>,
    pub(crate) notifier: Notifier,
    pub(crate) modem: Mutex<Option<ModemHandle>>,
    pub(crate) calls: Mutex<HashMap<String, Tracked>>,
    pub(crate) notes: Mutex<Notes>,
    pub(crate) modem_state: Mutex<String>,
    pub(crate) emergency: Mutex<Vec<String>>,
    pub(crate) muted: Mutex<bool>,
    pub(crate) route: Mutex<String>,
    /// SMS objects being handled now (Added and List can both name one).
    pub(crate) handling: Mutex<HashSet<String>>,
    /// The agent's runtime. zbus runs D-Bus method handlers on its own
    /// executor thread, where tokio isn't available: handlers hand their
    /// work to this runtime ([`Agent::on_runtime`]).
    pub(crate) rt: tokio::runtime::Handle,
}

impl Agent {
    /// Run `f` on the agent's tokio runtime and wait for it, from any
    /// executor (a D-Bus method handler runs on zbus's).
    pub(crate) async fn on_runtime<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        self.rt
            .spawn(f)
            .await
            .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()))
    }

    pub(crate) fn settings(&self) -> Settings {
        Settings::load_from(&self.cfg.settings_path)
    }

    pub(crate) fn region(&self) -> String {
        self.settings().region(number::default_region)
    }

    /// The contact name for a number (empty when unknown).
    pub(crate) fn name_for(&self, raw: &str) -> String {
        let mut book = self.contacts.lock().unwrap();
        book.reload_if_changed();
        book.name_for(raw).unwrap_or_default()
    }

    pub(crate) fn is_contact(&self, raw: &str) -> bool {
        let mut book = self.contacts.lock().unwrap();
        book.reload_if_changed();
        book.lookup(raw).is_some()
    }

    /// The number as people read it, for notifications.
    pub(crate) fn display_number(&self, raw: &str) -> String {
        number::display(raw, &self.region())
    }

    pub(crate) fn modem_handle(&self) -> Option<ModemHandle> {
        self.modem.lock().unwrap().clone()
    }

    pub(crate) fn emergency_numbers(&self) -> Vec<String> {
        let mut out: Vec<String> = number::FIXED_EMERGENCY
            .iter()
            .map(|s| s.to_string())
            .collect();
        for n in self.emergency.lock().unwrap().iter() {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
        out
    }

    pub(crate) async fn is_locked(&self) -> bool {
        match self.cfg.lock_probe {
            LockProbe::Fixed(v) => v,
            LockProbe::System => util::probe_locked(&self.system, &self.session).await,
        }
    }

    async fn set_modem_state(&self, state: &str) {
        let changed = {
            let mut s = self.modem_state.lock().unwrap();
            if *s == state {
                false
            } else {
                *s = state.to_string();
                true
            }
        };
        if changed {
            tracing::info!(state, "modem state");
            iface::modem_state_changed(self).await;
        }
    }
}

/// Run the agent until the process is stopped.
pub async fn run(cfg: Config, system: Connection, session: Connection) -> anyhow::Result<()> {
    let agent = start(cfg, system, session).await?;
    watch_modems(agent).await;
    Ok(())
}

/// Open the stores, serve the session-bus API and start the notification
/// listener; the caller then runs [`watch_modems`].
pub async fn start(
    cfg: Config,
    system: Connection,
    session: Connection,
) -> anyhow::Result<Arc<Agent>> {
    let messages = MessageStore::open(&cfg.data_dir.join("messages.db"))?;
    let interrupted = messages.fail_interrupted()?;
    if !interrupted.is_empty() {
        tracing::warn!(
            count = interrupted.len(),
            "sends interrupted by a stop, marked failed"
        );
    }
    let log = CallLog::open(&cfg.data_dir.join("calls.db"))?;
    let blocked = BlockList::open(&cfg.data_dir.join("blocked.db"))?;
    let settings = Settings::load_from(&cfg.settings_path);
    let contacts = AddressBook::open(&cfg.contacts_dir, &settings.region(number::default_region));
    let notifier = Notifier::new(&session, "omatel", "org.omamobile.Messages").await?;
    let agent = Arc::new(Agent {
        cfg,
        system,
        session,
        messages: Mutex::new(messages),
        log: Mutex::new(log),
        blocked: Mutex::new(blocked),
        contacts: Mutex::new(contacts),
        notifier,
        modem: Mutex::new(None),
        calls: Mutex::new(HashMap::new()),
        notes: Mutex::new(Notes::default()),
        modem_state: Mutex::new("no-modem".into()),
        emergency: Mutex::new(Vec::new()),
        muted: Mutex::new(false),
        route: Mutex::new("earpiece".into()),
        handling: Mutex::new(HashSet::new()),
        rt: tokio::runtime::Handle::current(),
    });
    iface::serve(&agent).await?;
    tokio::spawn(iface::notification_events(agent.clone()));
    tracing::info!("serving org.omamobile.Phone1 and org.omamobile.Messages1");
    Ok(agent)
}

/// Follow ModemManager: attach to its modem whenever there is one.
pub async fn watch_modems(agent: Arc<Agent>) {
    loop {
        if let Err(e) = mm::wait_for_service(&agent.system).await {
            tracing::warn!(error = %e, "cannot watch ModemManager");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        match attach_when_present(&agent).await {
            Ok(()) => tracing::info!("modem gone"),
            Err(e) => tracing::warn!(error = %e, "modem watch ended"),
        }
        detach(&agent).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn attach_when_present(agent: &Arc<Agent>) -> anyhow::Result<()> {
    let om = zbus::fdo::ObjectManagerProxy::builder(&agent.system)
        .destination(mm::SERVICE)?
        .path(mm::PATH)?
        .build()
        .await?;
    let mut added = om.receive_interfaces_added().await?;
    let mut removed = om.receive_interfaces_removed().await?;
    let dbus = zbus::fdo::DBusProxy::new(&agent.system).await?;
    let mut owners = dbus.receive_name_owner_changed().await?;

    let modem = loop {
        if let Some(m) = mm::modems(&agent.system).await?.into_iter().next() {
            break m;
        }
        agent.set_modem_state("no-modem").await;
        tokio::select! {
            _ = added.next() => {}
            Some(sig) = owners.next() => {
                if let Ok(a) = sig.args() {
                    if a.name() == mm::SERVICE && a.new_owner().is_none() {
                        return Ok(());
                    }
                }
            }
        }
    };
    tracing::info!(path = %modem.path, voice = modem.has_voice, messaging = modem.has_messaging, "modem found");

    let modem_proxy = mm::ModemProxy::builder(&agent.system)
        .path(modem.path.clone())?
        .build()
        .await?;
    let voice = if modem.has_voice {
        Some(
            mm::VoiceProxy::builder(&agent.system)
                .path(modem.path.clone())?
                .build()
                .await?,
        )
    } else {
        None
    };
    let messaging = if modem.has_messaging {
        Some(
            mm::MessagingProxy::builder(&agent.system)
                .path(modem.path.clone())?
                .build()
                .await?,
        )
    } else {
        None
    };
    *agent.modem.lock().unwrap() = Some(ModemHandle {
        voice: voice.clone(),
        messaging: messaging.clone(),
    });
    refresh_modem_state(agent, &modem_proxy, voice.as_ref()).await;
    refresh_emergency(agent, &modem_proxy).await;

    let mut state_changes = modem_proxy.receive_state_changed().await;
    let mut call_added = match &voice {
        Some(v) => Some(v.receive_call_added().await?),
        None => None,
    };
    let mut sms_added = match &messaging {
        Some(m) => Some(m.receive_added().await?),
        None => None,
    };
    // What was there before we attached.
    if let Some(v) = &voice {
        for path in v.list_calls().await.unwrap_or_default() {
            calls::track(agent.clone(), path).await;
        }
    }
    if let Some(m) = &messaging {
        for path in m.list().await.unwrap_or_default() {
            tokio::spawn(sms::handle_incoming(agent.clone(), path));
        }
    }

    loop {
        tokio::select! {
            Some(_) = state_changes.next() => {
                refresh_modem_state(agent, &modem_proxy, voice.as_ref()).await;
                refresh_emergency(agent, &modem_proxy).await;
            }
            Some(sig) = async { match call_added.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                if let Ok(a) = sig.args() {
                    calls::track(agent.clone(), a.path.clone()).await;
                }
            }
            Some(sig) = async { match sms_added.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                // `received` is false for messages the modem had stored
                // (the bridge's first fetch, MM's storage at start-up):
                // those are ours to keep too. Our own sends are SUBMIT
                // PDUs, which handle_incoming leaves alone.
                if let Ok(a) = sig.args() {
                    tokio::spawn(sms::handle_incoming(agent.clone(), a.path.clone()));
                }
            }
            Some(sig) = removed.next() => {
                if let Ok(a) = sig.args() {
                    if a.object_path.as_str() == modem.path.as_str()
                        && a.interfaces.iter().any(|i| i.as_str() == mm::IFACE_MODEM)
                    {
                        return Ok(());
                    }
                }
            }
            Some(sig) = owners.next() => {
                if let Ok(a) = sig.args() {
                    if a.name() == mm::SERVICE && a.new_owner().is_none() {
                        return Ok(());
                    }
                }
            }
            else => return Ok(()),
        }
    }
}

async fn refresh_modem_state(
    agent: &Agent,
    modem: &mm::ModemProxy<'static>,
    voice: Option<&mm::VoiceProxy<'static>>,
) {
    let state = modem.state().await.unwrap_or(mm::modem_state::UNKNOWN);
    let reason = modem.state_failed_reason().await.unwrap_or(0);
    let emergency_only = match voice {
        Some(v) => v.emergency_only().await.unwrap_or(false),
        None => false,
    };
    agent
        .set_modem_state(util::modem_state_name(state, reason, emergency_only))
        .await;
}

async fn refresh_emergency(agent: &Agent, modem: &mm::ModemProxy<'static>) {
    let Ok(sim) = modem.sim().await else {
        return;
    };
    if sim.as_str() == "/" {
        return;
    }
    let Ok(proxy) = mm::SimProxy::builder(&agent.system)
        .path(sim)
        .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
    else {
        return;
    };
    if let Ok(p) = proxy.build().await {
        if let Ok(list) = p.emergency_numbers().await {
            *agent.emergency.lock().unwrap() = list;
        }
    }
}

async fn detach(agent: &Arc<Agent>) {
    *agent.modem.lock().unwrap() = None;
    // Calls on a modem that went away have ended.
    let paths: Vec<String> = agent.calls.lock().unwrap().keys().cloned().collect();
    for p in paths {
        calls::ended(agent, &p, mm::call_reason::ERROR).await;
    }
    agent.set_modem_state("no-modem").await;
}
