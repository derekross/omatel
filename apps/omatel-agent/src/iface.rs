// SPDX-License-Identifier: GPL-3.0-or-later
//! `org.omamobile.Phone1` and `org.omamobile.Messages1` on the session bus
//! (the contract is documented in `omatel_api`), and the routing of
//! notification actions and replies.

use std::sync::Arc;

use futures_util::StreamExt;
use omatel_api::{CallInfo, MESSAGES_NAME, MESSAGES_PATH, PHONE_NAME, PHONE_PATH, ROUTES};
use omatel_contacts::number;
use omatel_notify::Event;
use zbus::object_server::SignalEmitter;
use zbus::{interface, DBusError};

use crate::calls::{self, DialError};
use crate::{audio, sms, Agent, Target};

#[derive(Debug, DBusError)]
#[zbus(prefix = "org.omamobile.Phone1.Error")]
pub enum PhoneError {
    #[zbus(error)]
    ZBus(zbus::Error),
    EmergencyUnsupported(String),
    NoModem(String),
    InvalidNumber(String),
    NoSuchCall(String),
    Failed(String),
}

impl From<DialError> for PhoneError {
    fn from(e: DialError) -> PhoneError {
        match e {
            DialError::Invalid => PhoneError::InvalidNumber("Not a phone number".into()),
            DialError::Emergency => {
                PhoneError::EmergencyUnsupported(omatel_api::EMERGENCY_WARNING.into())
            }
            DialError::NoModem => PhoneError::NoModem("No modem is available".into()),
            DialError::Failed(m) => PhoneError::Failed(m),
        }
    }
}

fn no_such_call() -> PhoneError {
    PhoneError::NoSuchCall("No such call".into())
}

pub struct Phone {
    agent: Arc<Agent>,
}

#[interface(name = "org.omamobile.Phone1")]
impl Phone {
    async fn dial(&self, number: &str) -> Result<String, PhoneError> {
        let (a, n) = (self.agent.clone(), number.to_string());
        Ok(self
            .agent
            .on_runtime(async move { calls::dial(&a, &n).await })
            .await?)
    }

    async fn answer(&self, id: &str) -> Result<(), PhoneError> {
        let (a, i) = (self.agent.clone(), id.to_string());
        if self
            .agent
            .on_runtime(async move { calls::answer(&a, &i).await })
            .await?
        {
            Ok(())
        } else {
            Err(no_such_call())
        }
    }

    async fn decline(&self, id: &str) -> Result<(), PhoneError> {
        let (a, i) = (self.agent.clone(), id.to_string());
        if self
            .agent
            .on_runtime(async move { calls::decline(&a, &i).await })
            .await?
        {
            Ok(())
        } else {
            Err(no_such_call())
        }
    }

    async fn hangup(&self, id: &str) -> Result<(), PhoneError> {
        let (a, i) = (self.agent.clone(), id.to_string());
        if self
            .agent
            .on_runtime(async move { calls::hangup(&a, &i).await })
            .await?
        {
            Ok(())
        } else {
            Err(no_such_call())
        }
    }

    async fn send_dtmf(&self, id: &str, keys: &str) -> Result<(), PhoneError> {
        if keys.is_empty()
            || !keys
                .chars()
                .all(|c| c.is_ascii_digit() || c == '*' || c == '#')
        {
            return Err(PhoneError::InvalidNumber(
                "Touch tones are 0-9, * and #".into(),
            ));
        }
        let (a, i, k) = (self.agent.clone(), id.to_string(), keys.to_string());
        if self
            .agent
            .on_runtime(async move { calls::send_dtmf(&a, &i, &k).await })
            .await?
        {
            Ok(())
        } else {
            Err(no_such_call())
        }
    }

    async fn set_mute(
        &self,
        mute: bool,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), PhoneError> {
        let applied = audio::set_mute(&self.agent.session, mute).await;
        tracing::info!(mute, applied, "microphone mute");
        *self.agent.muted.lock().unwrap() = mute;
        self.muted_changed(&emitter).await?;
        Ok(())
    }

    async fn set_route(
        &self,
        route: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), PhoneError> {
        if !ROUTES.contains(&route) {
            return Err(PhoneError::Failed(format!(
                "Routes are {}",
                ROUTES.join(", ")
            )));
        }
        let applied = match route {
            "speaker" => audio::set_speaker(&self.agent.session, true).await,
            "earpiece" => audio::set_speaker(&self.agent.session, false).await,
            // Bluetooth headset calls come with the PipeWire rebuild (Q10).
            _ => false,
        };
        tracing::info!(route, applied, "audio route");
        *self.agent.route.lock().unwrap() = route.to_string();
        self.route_changed(&emitter).await?;
        Ok(())
    }

    fn list_calls(&self) -> Vec<CallInfo> {
        calls::list(&self.agent)
    }

    async fn mark_missed_seen(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), PhoneError> {
        let a = self.agent.clone();
        self.agent
            .on_runtime(async move { mark_missed_seen(&a).await })
            .await;
        self.missed_calls_changed(&emitter).await?;
        Ok(())
    }

    #[zbus(property)]
    fn muted(&self) -> bool {
        *self.agent.muted.lock().unwrap()
    }

    #[zbus(property)]
    fn route(&self) -> String {
        self.agent.route.lock().unwrap().clone()
    }

    #[zbus(property)]
    async fn audio_routing(&self) -> bool {
        audio::routing_available(&self.agent.session).await
    }

    #[zbus(property)]
    fn modem_state(&self) -> String {
        self.agent.modem_state.lock().unwrap().clone()
    }

    #[zbus(property)]
    fn emergency_numbers(&self) -> Vec<String> {
        self.agent.emergency_numbers()
    }

    /// Ringing calls the agent silenced (an unknown caller, or Do Not
    /// Disturb): logged and shown quietly, never rung.
    #[zbus(property)]
    fn silenced_calls(&self) -> Vec<String> {
        let calls = self.agent.calls.lock().unwrap();
        let mut ids: Vec<String> = calls
            .values()
            .filter(|t| t.silenced && t.info.is_ringing_in())
            .map(|t| t.info.id.clone())
            .collect();
        ids.sort();
        ids
    }

    #[zbus(property)]
    fn missed_calls(&self) -> u32 {
        self.agent.log.lock().unwrap().unseen_missed().unwrap_or(0) as u32
    }

    #[zbus(signal)]
    async fn incoming_call(
        emitter: &SignalEmitter<'_>,
        id: &str,
        number: &str,
        name: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn call_state_changed(emitter: &SignalEmitter<'_>, call: CallInfo) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn call_log_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

pub struct Messages {
    agent: Arc<Agent>,
}

fn store_error(e: omatel_store::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!("message store: {e}"))
}

#[interface(name = "org.omamobile.Messages1")]
impl Messages {
    async fn send(&self, number: &str, text: &str) -> zbus::fdo::Result<(i64, i64)> {
        if text.trim().is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs("Empty message".into()));
        }
        if number.trim().is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs("Missing number".into()));
        }
        let address = number::normalize(number, &self.agent.region());
        let conv = self
            .agent
            .messages
            .lock()
            .unwrap()
            .conversation_for(&address, &[number.trim()])
            .map_err(store_error)?;
        let (a, t) = (self.agent.clone(), text.to_string());
        let id = self
            .agent
            .on_runtime(async move { sms::send(&a, conv, &t).await })
            .await
            .map_err(store_error)?;
        Ok((conv, id))
    }

    async fn send_to_conversation(&self, conversation: i64, text: &str) -> zbus::fdo::Result<i64> {
        if text.trim().is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs("Empty message".into()));
        }
        let exists = self
            .agent
            .messages
            .lock()
            .unwrap()
            .conversation(conversation)
            .map_err(store_error)?
            .is_some();
        if !exists {
            return Err(zbus::fdo::Error::InvalidArgs("No such conversation".into()));
        }
        let (a, t) = (self.agent.clone(), text.to_string());
        self.agent
            .on_runtime(async move { sms::send(&a, conversation, &t).await })
            .await
            .map_err(store_error)
    }

    async fn retry(&self, message: i64) -> zbus::fdo::Result<()> {
        let a = self.agent.clone();
        if self
            .agent
            .on_runtime(async move { sms::retry(&a, message).await })
            .await
        {
            Ok(())
        } else {
            Err(zbus::fdo::Error::InvalidArgs(
                "Not a failed outgoing message".into(),
            ))
        }
    }

    async fn mark_read(&self, conversation: i64) {
        let a = self.agent.clone();
        self.agent
            .on_runtime(async move { sms::mark_read(&a, conversation).await })
            .await;
    }

    async fn open_conversation(&self, number: &str) -> zbus::fdo::Result<()> {
        let address = number::normalize(number, &self.agent.region());
        let conv = self
            .agent
            .messages
            .lock()
            .unwrap()
            .conversation_for(&address, &[number.trim()])
            .map_err(store_error)?;
        self.agent
            .cfg
            .launcher
            .launch(&["omamessages", "--conversation", &conv.to_string()]);
        Ok(())
    }

    #[zbus(property)]
    fn ready(&self) -> bool {
        self.agent.modem_state.lock().unwrap().as_str() == "ready"
            && self
                .agent
                .modem_handle()
                .is_some_and(|m| m.messaging.is_some())
    }

    #[zbus(property)]
    fn modem_state(&self) -> String {
        self.agent.modem_state.lock().unwrap().clone()
    }

    #[zbus(signal)]
    async fn messages_changed(emitter: &SignalEmitter<'_>, conversation: i64) -> zbus::Result<()>;
}

pub async fn serve(agent: &Arc<Agent>) -> zbus::Result<()> {
    let server = agent.session.object_server();
    server
        .at(
            PHONE_PATH,
            Phone {
                agent: agent.clone(),
            },
        )
        .await?;
    server
        .at(
            MESSAGES_PATH,
            Messages {
                agent: agent.clone(),
            },
        )
        .await?;
    agent.session.request_name(PHONE_NAME).await?;
    agent.session.request_name(MESSAGES_NAME).await?;
    Ok(())
}

fn emitter<'a>(agent: &'a Agent, path: &'a str) -> Option<SignalEmitter<'a>> {
    SignalEmitter::new(&agent.session, path).ok()
}

pub async fn incoming_call(agent: &Agent, info: &CallInfo) {
    if let Some(e) = emitter(agent, PHONE_PATH) {
        let _ = Phone::incoming_call(&e, &info.id, &info.number, &info.name).await;
    }
}

pub async fn call_state_changed(agent: &Agent, info: CallInfo) {
    if let Some(e) = emitter(agent, PHONE_PATH) {
        let _ = Phone::call_state_changed(&e, info).await;
    }
}

pub async fn call_log_changed(agent: &Agent) {
    if let Some(e) = emitter(agent, PHONE_PATH) {
        let _ = Phone::call_log_changed(&e).await;
    }
}

pub async fn messages_changed(agent: &Agent, conversation: i64) {
    if let Some(e) = emitter(agent, MESSAGES_PATH) {
        let _ = Messages::messages_changed(&e, conversation).await;
    }
}

pub async fn silenced_calls_changed(agent: &Agent) {
    if let Ok(i) = agent
        .session
        .object_server()
        .interface::<_, Phone>(PHONE_PATH)
        .await
    {
        let _ = i
            .get()
            .await
            .silenced_calls_changed(i.signal_emitter())
            .await;
    }
}

pub async fn missed_calls_changed(agent: &Agent) {
    if let Ok(i) = agent
        .session
        .object_server()
        .interface::<_, Phone>(PHONE_PATH)
        .await
    {
        let _ = i.get().await.missed_calls_changed(i.signal_emitter()).await;
    }
}

pub async fn modem_state_changed(agent: &Agent) {
    let server = agent.session.object_server();
    if let Ok(i) = server.interface::<_, Phone>(PHONE_PATH).await {
        let p = i.get().await;
        let _ = p.modem_state_changed(i.signal_emitter()).await;
        let _ = p.emergency_numbers_changed(i.signal_emitter()).await;
    }
    if let Ok(i) = server.interface::<_, Messages>(MESSAGES_PATH).await {
        let m = i.get().await;
        let _ = m.modem_state_changed(i.signal_emitter()).await;
        let _ = m.ready_changed(i.signal_emitter()).await;
    }
}

/// After the last call: mute off, back to the earpiece.
pub async fn reset_audio(agent: &Agent) {
    let changed = {
        let mut m = agent.muted.lock().unwrap();
        let mut r = agent.route.lock().unwrap();
        let changed = *m || r.as_str() != "earpiece";
        *m = false;
        *r = "earpiece".into();
        changed
    };
    if !changed {
        return;
    }
    if let Ok(i) = agent
        .session
        .object_server()
        .interface::<_, Phone>(PHONE_PATH)
        .await
    {
        let p = i.get().await;
        let _ = p.muted_changed(i.signal_emitter()).await;
        let _ = p.route_changed(i.signal_emitter()).await;
    }
}

pub async fn mark_missed_seen(agent: &Agent) {
    let _ = agent.log.lock().unwrap().mark_all_seen();
    let id = agent.notes.lock().unwrap().missed.take();
    if let Some(id) = id {
        agent.notes.lock().unwrap().by_id.remove(&id);
        let _ = agent.notifier.close(id).await;
    }
}

/// Route notification buttons and inline replies.
pub async fn notification_events(agent: Arc<Agent>) {
    let mut events = match agent.notifier.events().await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "notification actions unavailable");
            return;
        }
    };
    while let Some(ev) = events.next().await {
        match ev {
            Event::Action { id, key } => {
                let target = agent.notes.lock().unwrap().by_id.get(&id).cloned();
                if let Some(t) = target {
                    tracing::info!(id, key = %key, "notification action");
                    action(&agent, t, &key).await;
                }
            }
            Event::Replied { id, text } => {
                let target = agent.notes.lock().unwrap().by_id.get(&id).cloned();
                if let Some(Target::Conversation(c)) = target {
                    tracing::info!(id, conversation = c, "inline reply");
                    if !text.trim().is_empty() {
                        let _ = sms::send(&agent, c, &text).await;
                    }
                    sms::mark_read(&agent, c).await;
                }
            }
            Event::Closed { id, .. } => {
                let mut notes = agent.notes.lock().unwrap();
                if let Some(Target::Conversation(c)) = notes.by_id.remove(&id) {
                    notes.by_conversation.remove(&c);
                }
                if notes.missed == Some(id) {
                    notes.missed = None;
                }
            }
        }
    }
}

async fn action(agent: &Arc<Agent>, target: Target, key: &str) {
    let launch = |args: &[&str]| agent.cfg.launcher.launch(args);
    match target {
        Target::Conversation(c) => match key {
            "mark-read" => sms::mark_read(agent, c).await,
            _ => launch(&["omamessages", "--conversation", &c.to_string()]),
        },
        Target::IncomingCall(call) => match key {
            "answer" => {
                if let Err(e) = calls::answer(agent, &call).await {
                    tracing::warn!(error = %e, "answer from the notification failed");
                }
                launch(&["omaphone", "--in-call"]);
            }
            "decline" => {
                if let Err(e) = calls::decline(agent, &call).await {
                    tracing::warn!(error = %e, "decline from the notification failed");
                }
            }
            _ => launch(&["omaphone", "--incoming", &call]),
        },
        Target::MissedCalls { number } => {
            match key {
                "call-back" => match calls::dial(agent, &number).await {
                    Ok(_) => launch(&["omaphone", "--in-call"]),
                    Err(e) => {
                        tracing::warn!(error = ?e, "call back failed");
                        launch(&["omaphone", "--recents"]);
                    }
                },
                "message" => launch(&["omamessages", &format!("sms:{number}")]),
                _ => launch(&["omaphone", "--recents"]),
            }
            mark_missed_seen(agent).await;
            missed_calls_changed(agent).await;
        }
    }
}
