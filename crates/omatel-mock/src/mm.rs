// SPDX-License-Identifier: GPL-3.0-or-later
//! A scripted ModemManager on a (private) bus: one registered modem with
//! Voice and Messaging, a SIM, and calls and messages that tests drive.
//!
//! The modem side is the test's: [`MockModem::ring`] makes an incoming
//! call, [`MockModem::remote_answer`] and [`MockModem::remote_hangup`] are
//! the far end, [`MockModem::receive_sms`] delivers a message and
//! [`MockModem::deliver`] a delivery report. Outgoing calls ring out and,
//! with `auto_answer`, are answered after a moment. What clients did is
//! recorded: touch tones, sent messages, deletions.
//!
//! The object layout, property types and enum values follow ModemManager
//! 1.24, as `omamobile-mmd` serves them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::fdo;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
use zbus::{interface, Connection, ObjectServer};

pub const SERVICE: &str = "org.freedesktop.ModemManager1";
pub const ROOT: &str = "/org/freedesktop/ModemManager1";
pub const MODEM: &str = "/org/freedesktop/ModemManager1/Modem/0";
pub const SIM: &str = "/org/freedesktop/ModemManager1/SIM/0";

const CALL_DIALING: i32 = 1;
const CALL_RINGING_OUT: i32 = 2;
const CALL_RINGING_IN: i32 = 3;
const CALL_ACTIVE: i32 = 4;
const CALL_TERMINATED: i32 = 7;
const REASON_OUTGOING_STARTED: i32 = 1;
const REASON_INCOMING_NEW: i32 = 2;
const REASON_ACCEPTED: i32 = 3;
const REASON_TERMINATED: i32 = 4;
const REASON_REFUSED_OR_BUSY: i32 = 5;

/// A message a client sent through the mock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub path: String,
    pub number: String,
    pub text: String,
    pub delivery_report: bool,
}

#[derive(Default)]
struct Shared {
    next_call: u32,
    next_sms: u32,
    calls: Vec<OwnedObjectPath>,
    messages: Vec<OwnedObjectPath>,
    sent: Vec<Sent>,
    dtmf: Vec<String>,
    deleted_sms: Vec<String>,
    deleted_calls: Vec<String>,
    accepted: Vec<String>,
    hung_up: Vec<String>,
    auto_answer: bool,
    fail_sends: bool,
    next_reference: u32,
}

type State = Arc<Mutex<Shared>>;

/// A running mock ModemManager. Dropping it leaves the objects on the bus
/// until the connection closes.
#[derive(Clone)]
pub struct MockModem {
    conn: Connection,
    state: State,
}

struct Manager;

#[interface(name = "org.freedesktop.ModemManager1")]
impl Manager {
    fn scan_devices(&self) -> fdo::Result<()> {
        Ok(())
    }

    #[zbus(property)]
    fn version(&self) -> String {
        "1.24.2".into()
    }
}

struct ModemIface {
    state: i32,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem")]
impl ModemIface {
    fn enable(&self, _enable: bool) -> fdo::Result<()> {
        Ok(())
    }

    #[zbus(property)]
    fn state(&self) -> i32 {
        self.state
    }
    #[zbus(property)]
    fn state_failed_reason(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn unlock_required(&self) -> u32 {
        1
    }
    #[zbus(property)]
    fn sim(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(SIM).expect("path")
    }
    #[zbus(property)]
    fn own_numbers(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    fn signal_quality(&self) -> (u32, bool) {
        (64, true)
    }
    #[zbus(property)]
    fn access_technologies(&self) -> u32 {
        1 << 14
    }
    #[zbus(property)]
    fn power_state(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn equipment_identifier(&self) -> String {
        String::new()
    }
}

struct Modem3gppIface;

#[interface(name = "org.freedesktop.ModemManager1.Modem.Modem3gpp")]
impl Modem3gppIface {
    #[zbus(property)]
    fn registration_state(&self) -> u32 {
        1
    }
    #[zbus(property)]
    fn operator_name(&self) -> String {
        "Test Network".into()
    }
    #[zbus(property)]
    fn operator_code(&self) -> String {
        "00101".into()
    }
    #[zbus(property)]
    fn imei(&self) -> String {
        String::new()
    }
}

struct SimIface;

#[interface(name = "org.freedesktop.ModemManager1.Sim")]
impl SimIface {
    #[zbus(property)]
    fn active(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn operator_name(&self) -> String {
        "Test Network".into()
    }
    #[zbus(property)]
    fn emergency_numbers(&self) -> Vec<String> {
        vec!["112".into(), "911".into()]
    }
    #[zbus(property)]
    fn imsi(&self) -> String {
        String::new()
    }
}

struct VoiceIface {
    state: State,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Voice")]
impl VoiceIface {
    fn list_calls(&self) -> Vec<OwnedObjectPath> {
        self.state.lock().unwrap().calls.clone()
    }

    async fn create_call(
        &self,
        properties: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        let number = properties
            .get("number")
            .and_then(|v| String::try_from(v.clone()).ok())
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| fdo::Error::InvalidArgs("Missing number".into()))?;
        Ok(new_call(&self.state, conn, server, &number, false).await?)
    }

    async fn delete_call(
        &self,
        path: ObjectPath<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let owned = OwnedObjectPath::from(path.clone());
        {
            let mut s = self.state.lock().unwrap();
            if !s.calls.contains(&owned) {
                return Err(fdo::Error::Failed("No such call".into()));
            }
            s.calls.retain(|p| p != &owned);
            s.deleted_calls.push(path.to_string());
        }
        server.remove::<CallIface, _>(&path).await?;
        self.calls_changed(&emitter).await?;
        Self::call_deleted(&emitter, path).await?;
        Ok(())
    }

    async fn hangup_all(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        let calls = self.state.lock().unwrap().calls.clone();
        for c in calls {
            let _ = set_call_state(conn, &c, CALL_TERMINATED, REASON_TERMINATED).await;
        }
        Ok(())
    }

    #[zbus(property)]
    fn calls(&self) -> Vec<OwnedObjectPath> {
        self.state.lock().unwrap().calls.clone()
    }

    #[zbus(property)]
    fn emergency_only(&self) -> bool {
        false
    }

    #[zbus(signal)]
    async fn call_added(emitter: &SignalEmitter<'_>, path: ObjectPath<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn call_deleted(emitter: &SignalEmitter<'_>, path: ObjectPath<'_>) -> zbus::Result<()>;
}

struct CallIface {
    state: i32,
    reason: i32,
    direction: i32,
    number: String,
    shared: State,
}

#[interface(name = "org.freedesktop.ModemManager1.Call")]
impl CallIface {
    async fn start(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        if self.direction != 2 || self.state != 0 {
            return Err(fdo::Error::Failed("This call can't be started".into()));
        }
        self.transition(&emitter, CALL_DIALING, REASON_OUTGOING_STARTED)
            .await?;
        let path = OwnedObjectPath::from(emitter.path().to_owned());
        let auto = self.shared.lock().unwrap().auto_answer;
        let conn = conn.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let _ = zbus::block_on(set_call_state(
                &conn,
                &path,
                CALL_RINGING_OUT,
                REASON_OUTGOING_STARTED,
            ));
            if auto {
                std::thread::sleep(Duration::from_millis(300));
                let _ = zbus::block_on(set_call_state(&conn, &path, CALL_ACTIVE, REASON_ACCEPTED));
            }
        });
        Ok(())
    }

    async fn accept(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        if self.state != CALL_RINGING_IN {
            return Err(fdo::Error::Failed("This call is not ringing".into()));
        }
        self.shared
            .lock()
            .unwrap()
            .accepted
            .push(emitter.path().to_string());
        self.transition(&emitter, CALL_ACTIVE, REASON_ACCEPTED)
            .await
    }

    async fn hangup(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        if self.state == CALL_TERMINATED {
            return Err(fdo::Error::Failed("This call has ended".into()));
        }
        self.shared
            .lock()
            .unwrap()
            .hung_up
            .push(emitter.path().to_string());
        self.transition(&emitter, CALL_TERMINATED, REASON_TERMINATED)
            .await
    }

    fn send_dtmf(&self, dtmf: &str) -> fdo::Result<()> {
        if self.state != CALL_ACTIVE {
            return Err(fdo::Error::Failed("Touch tones need an active call".into()));
        }
        if dtmf.is_empty()
            || !dtmf
                .chars()
                .all(|c| c.is_ascii_digit() || c == '*' || c == '#')
        {
            return Err(fdo::Error::InvalidArgs(
                "Touch tones are 0-9, * and #".into(),
            ));
        }
        self.shared.lock().unwrap().dtmf.push(dtmf.to_string());
        Ok(())
    }

    #[zbus(property)]
    fn state(&self) -> i32 {
        self.state
    }
    #[zbus(property)]
    fn state_reason(&self) -> i32 {
        self.reason
    }
    #[zbus(property)]
    fn direction(&self) -> i32 {
        self.direction
    }
    #[zbus(property)]
    fn number(&self) -> String {
        self.number.clone()
    }
    #[zbus(property)]
    fn multiparty(&self) -> bool {
        false
    }

    #[zbus(signal, name = "StateChanged")]
    async fn state_changed_signal(
        emitter: &SignalEmitter<'_>,
        old: i32,
        new: i32,
        reason: u32,
    ) -> zbus::Result<()>;
}

impl CallIface {
    async fn transition(
        &mut self,
        emitter: &SignalEmitter<'_>,
        state: i32,
        reason: i32,
    ) -> fdo::Result<()> {
        let old = self.state;
        if old == state || old == CALL_TERMINATED {
            return Ok(());
        }
        self.state = state;
        self.reason = reason;
        self.state_changed(emitter).await?;
        self.state_reason_changed(emitter).await?;
        Self::state_changed_signal(emitter, old, state, reason as u32).await?;
        Ok(())
    }
}

async fn set_call_state(
    conn: &Connection,
    path: &OwnedObjectPath,
    state: i32,
    reason: i32,
) -> zbus::Result<()> {
    let iface = conn
        .object_server()
        .interface::<_, CallIface>(path.as_str())
        .await?;
    let emitter = iface.signal_emitter().clone();
    let mut call = iface.get_mut().await;
    let result = call.transition(&emitter, state, reason).await;
    drop(call);
    result.map_err(zbus::Error::from)
}

async fn new_call(
    state: &State,
    conn: &Connection,
    server: &ObjectServer,
    number: &str,
    incoming: bool,
) -> zbus::Result<OwnedObjectPath> {
    let path = {
        let mut s = state.lock().unwrap();
        let n = s.next_call;
        s.next_call += 1;
        OwnedObjectPath::try_from(format!("/org/freedesktop/ModemManager1/Call/{n}"))?
    };
    let call = CallIface {
        state: if incoming { CALL_RINGING_IN } else { 0 },
        reason: if incoming { REASON_INCOMING_NEW } else { 0 },
        direction: if incoming { 1 } else { 2 },
        number: number.to_string(),
        shared: state.clone(),
    };
    server.at(path.as_str(), call).await?;
    state.lock().unwrap().calls.push(path.clone());
    let voice = server.interface::<_, VoiceIface>(MODEM).await?;
    let emitter = voice.signal_emitter().clone();
    voice.get().await.calls_changed(&emitter).await?;
    VoiceIface::call_added(&emitter, path.as_ref()).await?;
    let _ = conn;
    Ok(path)
}

struct MessagingIface {
    state: State,
}

#[interface(name = "org.freedesktop.ModemManager1.Modem.Messaging")]
impl MessagingIface {
    fn list(&self) -> Vec<OwnedObjectPath> {
        self.state.lock().unwrap().messages.clone()
    }

    async fn create(
        &self,
        properties: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<OwnedObjectPath> {
        let get = |k: &str| {
            properties
                .get(k)
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default()
        };
        let (number, text) = (get("number"), get("text"));
        if number.trim().is_empty() {
            return Err(fdo::Error::InvalidArgs("Missing number".into()));
        }
        if text.is_empty() {
            return Err(fdo::Error::InvalidArgs("Missing text".into()));
        }
        let report = properties
            .get("delivery-report-request")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        let sms = SmsIface {
            state: 0,
            pdu_type: 2,
            number,
            text,
            data: Vec::new(),
            timestamp: String::new(),
            delivery_state: 0x100,
            reference: 0,
            report,
            shared: self.state.clone(),
        };
        Ok(add_sms(&self.state, server, sms, false).await?)
    }

    async fn delete(
        &self,
        path: ObjectPath<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let owned = OwnedObjectPath::from(path.clone());
        {
            let mut s = self.state.lock().unwrap();
            if !s.messages.contains(&owned) {
                return Err(fdo::Error::Failed("No such SMS".into()));
            }
            s.messages.retain(|p| p != &owned);
            s.deleted_sms.push(path.to_string());
        }
        server.remove::<SmsIface, _>(&path).await?;
        self.messages_changed(&emitter).await?;
        Self::deleted(&emitter, path).await?;
        Ok(())
    }

    #[zbus(property)]
    fn messages(&self) -> Vec<OwnedObjectPath> {
        self.state.lock().unwrap().messages.clone()
    }

    #[zbus(property)]
    fn supported_storages(&self) -> Vec<u32> {
        vec![2]
    }

    #[zbus(property)]
    fn default_storage(&self) -> u32 {
        2
    }

    #[zbus(signal)]
    async fn added(
        emitter: &SignalEmitter<'_>,
        path: ObjectPath<'_>,
        received: bool,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn deleted(emitter: &SignalEmitter<'_>, path: ObjectPath<'_>) -> zbus::Result<()>;
}

async fn add_sms(
    state: &State,
    server: &ObjectServer,
    sms: SmsIface,
    received: bool,
) -> zbus::Result<OwnedObjectPath> {
    let path = {
        let mut s = state.lock().unwrap();
        let n = s.next_sms;
        s.next_sms += 1;
        OwnedObjectPath::try_from(format!("/org/freedesktop/ModemManager1/SMS/{n}"))?
    };
    server.at(path.as_str(), sms).await?;
    state.lock().unwrap().messages.push(path.clone());
    let messaging = server.interface::<_, MessagingIface>(MODEM).await?;
    let emitter = messaging.signal_emitter().clone();
    messaging.get().await.messages_changed(&emitter).await?;
    MessagingIface::added(&emitter, path.as_ref(), received).await?;
    Ok(path)
}

struct SmsIface {
    state: u32,
    pdu_type: u32,
    number: String,
    text: String,
    data: Vec<u8>,
    timestamp: String,
    delivery_state: u32,
    reference: u32,
    report: bool,
    shared: State,
}

#[interface(name = "org.freedesktop.ModemManager1.Sms")]
impl SmsIface {
    async fn send(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        if self.pdu_type != 2 {
            return Err(fdo::Error::Failed("A received SMS cannot be sent".into()));
        }
        if self.state == 5 {
            return Err(fdo::Error::Failed("This SMS was already sent".into()));
        }
        self.state = 4;
        self.state_changed(&emitter).await?;
        let fail = {
            let mut s = self.shared.lock().unwrap();
            if !s.fail_sends {
                s.next_reference += 1;
                self.reference = s.next_reference;
                s.sent.push(Sent {
                    path: emitter.path().to_string(),
                    number: self.number.clone(),
                    text: self.text.clone(),
                    delivery_report: self.report,
                });
            }
            s.fail_sends
        };
        if fail {
            self.state = 0;
            self.state_changed(&emitter).await?;
            return Err(fdo::Error::Failed("The network refused the SMS".into()));
        }
        self.state = 5;
        self.state_changed(&emitter).await?;
        self.message_reference_changed(&emitter).await?;
        Ok(())
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property)]
    fn pdu_type(&self) -> u32 {
        self.pdu_type
    }
    #[zbus(property)]
    fn number(&self) -> String {
        self.number.clone()
    }
    #[zbus(property)]
    fn text(&self) -> String {
        self.text.clone()
    }
    #[zbus(property)]
    fn data(&self) -> Vec<u8> {
        self.data.clone()
    }
    #[zbus(property)]
    fn timestamp(&self) -> String {
        self.timestamp.clone()
    }
    #[zbus(property)]
    fn delivery_state(&self) -> u32 {
        self.delivery_state
    }
    #[zbus(property)]
    fn message_reference(&self) -> u32 {
        self.reference
    }
    #[zbus(property)]
    fn delivery_report_request(&self) -> bool {
        self.report
    }
    #[zbus(property)]
    fn storage(&self) -> u32 {
        2
    }
    #[zbus(property)]
    fn class(&self) -> i32 {
        -1
    }
}

impl MockModem {
    /// Serve the mock on `conn` and take ModemManager's bus name.
    pub async fn start(conn: Connection) -> zbus::Result<MockModem> {
        let state: State = Arc::new(Mutex::new(Shared {
            auto_answer: true,
            ..Shared::default()
        }));
        let server = conn.object_server();
        server.at(ROOT, fdo::ObjectManager).await?;
        server.at(ROOT, Manager).await?;
        server
            .at(
                MODEM,
                ModemIface {
                    state: 8, // registered
                },
            )
            .await?;
        server.at(MODEM, Modem3gppIface).await?;
        server
            .at(
                MODEM,
                VoiceIface {
                    state: state.clone(),
                },
            )
            .await?;
        server
            .at(
                MODEM,
                MessagingIface {
                    state: state.clone(),
                },
            )
            .await?;
        server.at(SIM, SimIface).await?;
        conn.request_name(SERVICE).await?;
        Ok(MockModem { conn, state })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Whether outgoing calls are answered by the far end (default yes).
    pub fn set_auto_answer(&self, on: bool) {
        self.state.lock().unwrap().auto_answer = on;
    }

    /// Make every following SMS send fail (or succeed again).
    pub fn set_fail_sends(&self, on: bool) {
        self.state.lock().unwrap().fail_sends = on;
    }

    /// An incoming call, ringing.
    pub async fn ring(&self, number: &str) -> zbus::Result<OwnedObjectPath> {
        new_call(
            &self.state,
            &self.conn,
            self.conn.object_server(),
            number,
            true,
        )
        .await
    }

    /// The far end answers an outgoing call.
    pub async fn remote_answer(&self, call: &OwnedObjectPath) -> zbus::Result<()> {
        set_call_state(&self.conn, call, CALL_ACTIVE, REASON_ACCEPTED).await
    }

    /// The far end hangs up (or, before answering, refuses or gives up).
    pub async fn remote_hangup(&self, call: &OwnedObjectPath) -> zbus::Result<()> {
        let state = self.call_state(call).await?;
        let reason = if state == CALL_ACTIVE {
            REASON_TERMINATED
        } else {
            REASON_REFUSED_OR_BUSY
        };
        set_call_state(&self.conn, call, CALL_TERMINATED, reason).await
    }

    pub async fn call_state(&self, call: &OwnedObjectPath) -> zbus::Result<i32> {
        let iface = self
            .conn
            .object_server()
            .interface::<_, CallIface>(call.as_str())
            .await?;
        let s = iface.get().await.state;
        Ok(s)
    }

    /// A text message arrives. `timestamp` as MM writes it, or empty.
    pub async fn receive_sms(
        &self,
        number: &str,
        text: &str,
        timestamp: &str,
    ) -> zbus::Result<OwnedObjectPath> {
        let sms = SmsIface {
            state: 3,
            pdu_type: 1,
            number: number.into(),
            text: text.into(),
            data: Vec::new(),
            timestamp: timestamp.into(),
            delivery_state: 0x100,
            reference: 0,
            report: false,
            shared: self.state.clone(),
        };
        add_sms(&self.state, self.conn.object_server(), sms, true).await
    }

    /// A WAP push (an MMS notification): binary data, no text.
    pub async fn receive_wap_push(&self, number: &str) -> zbus::Result<OwnedObjectPath> {
        let sms = SmsIface {
            state: 3,
            pdu_type: 1,
            number: number.into(),
            text: String::new(),
            data: b"\x06application/vnd.wap.mms-message\0".to_vec(),
            timestamp: String::new(),
            delivery_state: 0x100,
            reference: 0,
            report: false,
            shared: self.state.clone(),
        };
        add_sms(&self.state, self.conn.object_server(), sms, true).await
    }

    /// A delivery report for a sent message: `state` 0 delivered, 0x41 failed.
    pub async fn deliver(&self, sms: &str, state: u32) -> zbus::Result<()> {
        let iface = self
            .conn
            .object_server()
            .interface::<_, SmsIface>(sms)
            .await?;
        let emitter = iface.signal_emitter().clone();
        let mut s = iface.get_mut().await;
        s.delivery_state = state;
        s.delivery_state_changed(&emitter).await
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.state.lock().unwrap().sent.clone()
    }

    pub fn dtmf(&self) -> Vec<String> {
        self.state.lock().unwrap().dtmf.clone()
    }

    pub fn deleted_sms(&self) -> Vec<String> {
        self.state.lock().unwrap().deleted_sms.clone()
    }

    pub fn deleted_calls(&self) -> Vec<String> {
        self.state.lock().unwrap().deleted_calls.clone()
    }

    pub fn messages(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|p| p.to_string())
            .collect()
    }

    pub fn calls(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|p| p.to_string())
            .collect()
    }

    pub fn accepted(&self) -> Vec<String> {
        self.state.lock().unwrap().accepted.clone()
    }

    pub fn hung_up(&self) -> Vec<String> {
        self.state.lock().unwrap().hung_up.clone()
    }
}

/// The mock's control interface on the bus, for the standalone
/// `omatel-mock-mm` (screenshots, trying the apps by hand):
/// `org.omatel.MockModem` at `/org/omatel/MockModem`.
pub struct Control {
    pub modem: MockModem,
}

#[interface(name = "org.omatel.MockModem")]
impl Control {
    async fn ring(&self, number: &str) -> fdo::Result<OwnedObjectPath> {
        Ok(self.modem.ring(number).await?)
    }

    async fn remote_answer(&self, call: OwnedObjectPath) -> fdo::Result<()> {
        Ok(self.modem.remote_answer(&call).await?)
    }

    async fn remote_hangup(&self, call: OwnedObjectPath) -> fdo::Result<()> {
        Ok(self.modem.remote_hangup(&call).await?)
    }

    async fn receive_sms(&self, number: &str, text: &str) -> fdo::Result<OwnedObjectPath> {
        Ok(self.modem.receive_sms(number, text, "").await?)
    }

    async fn deliver(&self, sms: OwnedObjectPath, state: u32) -> fdo::Result<()> {
        Ok(self.modem.deliver(sms.as_str(), state).await?)
    }

    fn set_auto_answer(&self, on: bool) {
        self.modem.set_auto_answer(on);
    }
}
