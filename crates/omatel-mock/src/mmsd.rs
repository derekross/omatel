// SPDX-License-Identifier: GPL-3.0-or-later
//! A scripted mmsd-tng on a (private) session bus: the manager at
//! `/org/ofono/mms`, the ModemManager plugin's service at
//! `/org/ofono/mms/modemmanager` and one object per message under it.
//!
//! The network side is the test's: [`MockMmsd::receive`] delivers an MMS
//! (its parts written into one "PDU" file, as mmsd-tng reports them:
//! (id, type, file, offset, length)), [`MockMmsd::set_status`] moves a
//! sent message on. `SendMessage` announces the new message before it
//! answers, as mmsd-tng does, then reports it sent (or, with
//! [`MockMmsd::set_fail_sends`], failed). What clients did is recorded.
//!
//! Signal and property shapes follow mmsd-tng 2.6.4's `src/service.c`:
//! `PropertyChanged("status", <s>)` with a lower-case name, `Attachments`
//! as `a(ssstt)`, dates as `%Y-%m-%dT%H:%M:%S%z`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{fdo, interface, Connection, ObjectServer};

pub const SERVICE: &str = "org.ofono.mms";
pub const ROOT: &str = "/org/ofono/mms";
pub const MODEMMANAGER: &str = "/org/ofono/mms/modemmanager";

type Props = HashMap<String, OwnedValue>;

/// A message a client sent through the mock, with the attachments' bytes
/// as they were when SendMessage was called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMms {
    pub path: String,
    pub recipients: Vec<String>,
    pub delivery_report: Option<bool>,
    /// (content id, content type, bytes).
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

#[derive(Default)]
struct Shared {
    next: u32,
    messages: Vec<(String, Props)>,
    sent: Vec<SentMms>,
    deleted: Vec<String>,
    fail_sends: bool,
    own_number: String,
    max_size: i32,
}

type State = Arc<Mutex<Shared>>;

/// A running mock mmsd-tng.
#[derive(Clone)]
pub struct MockMmsd {
    conn: Connection,
    state: State,
    dir: Arc<tempfile::TempDir>,
}

fn props_value(props: &Props) -> HashMap<String, OwnedValue> {
    props
        .iter()
        .map(|(k, v)| (k.clone(), v.try_clone().expect("no fds")))
        .collect()
}

fn owned<'a, T: Into<Value<'a>>>(v: T) -> OwnedValue {
    OwnedValue::try_from(v.into()).expect("no fds")
}

struct Manager;

#[interface(name = "org.ofono.mms.Manager")]
impl Manager {
    fn get_services(&self) -> Vec<(OwnedObjectPath, Props)> {
        vec![(
            OwnedObjectPath::try_from(MODEMMANAGER).expect("path"),
            HashMap::new(),
        )]
    }
}

struct Service {
    state: State,
}

#[interface(name = "org.ofono.mms.Service")]
impl Service {
    fn get_messages(&self) -> Vec<(OwnedObjectPath, Props)> {
        self.state
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|(p, props)| {
                (
                    OwnedObjectPath::try_from(p.as_str()).expect("path"),
                    props_value(props),
                )
            })
            .collect()
    }

    fn get_properties(&self) -> Props {
        let max = self.state.lock().unwrap().max_size;
        HashMap::from([
            ("UseDeliveryReports".to_string(), owned(false)),
            ("AutoCreateSMIL".to_string(), owned(true)),
            ("TotalMaxAttachmentSize".to_string(), owned(max)),
            ("MaxAttachments".to_string(), owned(25i32)),
            ("NotificationInds".to_string(), owned(0i32)),
        ])
    }

    async fn send_message(
        &self,
        recipients: Vec<String>,
        options: OwnedValue,
        attachments: Vec<(String, String, String)>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        if recipients.is_empty() {
            return Err(fdo::Error::InvalidArgs("no recipients".into()));
        }
        let mut files = Vec::new();
        for (id, ct, file) in &attachments {
            let bytes = std::fs::read(file)
                .map_err(|e| fdo::Error::InvalidArgs(format!("cannot read {file}: {e}")))?;
            files.push((id.clone(), ct.clone(), bytes));
        }
        let delivery_report = bool_option(&options, "DeliveryReport");
        let (path, props, fail) = {
            let mut s = self.state.lock().unwrap();
            s.next += 1;
            let path = format!("{MODEMMANAGER}/out{:04}", s.next);
            let props: Props = HashMap::from([
                ("Status".to_string(), owned("draft")),
                ("Date".to_string(), owned("2026-11-14T10:00:00+0000")),
                ("Subject".to_string(), owned("")),
                (
                    "Delivery Report".to_string(),
                    owned(delivery_report.unwrap_or(false)),
                ),
                ("Modem Number".to_string(), owned(s.own_number.clone())),
                ("Recipients".to_string(), owned(recipients.clone())),
                ("Attachments".to_string(), owned(Vec::<Att>::new())),
            ]);
            s.messages.push((path.clone(), props_value(&props)));
            s.sent.push(SentMms {
                path: path.clone(),
                recipients,
                delivery_report,
                attachments: files,
            });
            (path, props, s.fail_sends)
        };
        let op = OwnedObjectPath::try_from(path.as_str()).expect("path");
        server
            .at(
                op.clone(),
                MessageIface {
                    state: self.state.clone(),
                },
            )
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        // mmsd-tng signals the message before SendMessage returns.
        Service::message_added(&emitter, op.clone(), props).await?;
        let conn = emitter.connection().clone();
        let state = self.state.clone();
        let p = path.clone();
        // The method runs on zbus's executor, not tokio's (as in mm.rs).
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let status = if fail { "sending_failed" } else { "sent" };
            let _ = zbus::block_on(status_signal(&conn, &state, &p, status));
        });
        Ok(op)
    }

    #[zbus(signal)]
    async fn message_added(
        emitter: &SignalEmitter<'_>,
        path: OwnedObjectPath,
        properties: Props,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn message_removed(
        emitter: &SignalEmitter<'_>,
        path: OwnedObjectPath,
    ) -> zbus::Result<()>;
}

/// An `a{sv}` option of SendMessage.
fn bool_option(options: &OwnedValue, key: &str) -> Option<bool> {
    let map = HashMap::<String, OwnedValue>::try_from(options.try_clone().ok()?).ok()?;
    match &**map.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::Value(inner) => match **inner {
            Value::Bool(b) => Some(b),
            _ => None,
        },
        _ => None,
    }
}

/// One entry of `Attachments`: (id, content type, file, offset, length).
type Att = (String, String, String, u64, u64);

struct MessageIface {
    state: State,
}

#[interface(name = "org.ofono.mms.Message")]
impl MessageIface {
    async fn delete(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let path = header
            .path()
            .map(|p| p.to_string())
            .ok_or_else(|| fdo::Error::Failed("no path".into()))?;
        {
            let mut s = self.state.lock().unwrap();
            s.messages.retain(|(p, _)| *p != path);
            s.deleted.push(path.clone());
        }
        let op = OwnedObjectPath::try_from(path.as_str()).expect("path");
        let _ = server.remove::<MessageIface, _>(op.clone()).await;
        let emitter = SignalEmitter::new(conn, MODEMMANAGER)?;
        Service::message_removed(&emitter, op).await?;
        Ok(())
    }

    fn mark_read(&self) -> fdo::Result<()> {
        Ok(())
    }

    #[zbus(signal)]
    async fn property_changed(
        emitter: &SignalEmitter<'_>,
        name: &str,
        value: Value<'_>,
    ) -> zbus::Result<()>;
}

async fn status_signal(
    conn: &Connection,
    state: &State,
    path: &str,
    status: &str,
) -> zbus::Result<()> {
    {
        let mut s = state.lock().unwrap();
        if let Some((_, props)) = s.messages.iter_mut().find(|(p, _)| p == path) {
            props.insert("Status".into(), owned(status));
        }
    }
    let emitter = SignalEmitter::new(conn, path.to_string())?;
    // mmsd-tng names it in lower case (emit_msg_status_changed).
    MessageIface::property_changed(&emitter, "status", Value::from(status)).await
}

/// An MMS for [`MockMmsd::receive`].
#[derive(Debug, Clone, Default)]
pub struct Incoming {
    pub sender: String,
    pub recipients: Vec<String>,
    pub date: String,
    /// (content id, content type, bytes); a text/plain part is the text.
    pub parts: Vec<(String, String, Vec<u8>)>,
}

impl MockMmsd {
    /// Serve the mock on `conn` without taking mmsd-tng's name yet:
    /// messages added now are what `GetMessages` finds at [`MockMmsd::claim`].
    pub async fn serve(conn: Connection) -> zbus::Result<MockMmsd> {
        let state: State = Arc::new(Mutex::new(Shared {
            max_size: 1_100_000,
            ..Shared::default()
        }));
        let server = conn.object_server();
        server.at(ROOT, Manager).await?;
        server
            .at(
                MODEMMANAGER,
                Service {
                    state: state.clone(),
                },
            )
            .await?;
        let dir = tempfile::Builder::new()
            .prefix("omatel-mmsd-")
            .tempdir()
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(MockMmsd {
            conn,
            state,
            dir: Arc::new(dir),
        })
    }

    /// Take `org.ofono.mms`: clients see mmsd-tng start.
    pub async fn claim(&self) -> zbus::Result<()> {
        self.conn.request_name(SERVICE).await?;
        Ok(())
    }

    /// Serve and claim the name at once.
    pub async fn start(conn: Connection) -> zbus::Result<MockMmsd> {
        let m = MockMmsd::serve(conn).await?;
        m.claim().await?;
        Ok(m)
    }

    /// Give up the name: clients see mmsd-tng stop.
    pub async fn stop(&self) -> zbus::Result<()> {
        self.conn.release_name(SERVICE).await?;
        Ok(())
    }

    /// The phone's number as mmsd-tng reports it (`Modem Number`).
    pub fn set_own_number(&self, n: &str) {
        self.state.lock().unwrap().own_number = n.to_string();
    }

    /// Make every following send fail (or succeed again).
    pub fn set_fail_sends(&self, on: bool) {
        self.state.lock().unwrap().fail_sends = on;
    }

    /// The carrier's total attachment size limit (bytes).
    pub fn set_max_size(&self, bytes: i32) {
        self.state.lock().unwrap().max_size = bytes;
    }

    /// An MMS arrives: stored, registered, announced with MessageAdded.
    pub async fn receive(&self, m: &Incoming) -> zbus::Result<OwnedObjectPath> {
        let (path, props) = {
            let mut s = self.state.lock().unwrap();
            s.next += 1;
            let uuid = format!("in{:04}", s.next);
            let file: PathBuf = self.dir.path().join(&uuid);
            let mut pdu = b"MMS-HEADER".to_vec();
            let mut atts: Vec<Att> = Vec::new();
            for (id, ct, bytes) in &m.parts {
                atts.push((
                    id.clone(),
                    ct.clone(),
                    file.to_string_lossy().into_owned(),
                    pdu.len() as u64,
                    bytes.len() as u64,
                ));
                pdu.extend_from_slice(bytes);
            }
            std::fs::write(&file, &pdu).map_err(|e| zbus::Error::Failure(e.to_string()))?;
            let props: Props = HashMap::from([
                ("Status".to_string(), owned("received")),
                ("Date".to_string(), owned(m.date.clone())),
                ("Subject".to_string(), owned("")),
                ("Sender".to_string(), owned(m.sender.clone())),
                ("Recipients".to_string(), owned(m.recipients.clone())),
                ("Modem Number".to_string(), owned(s.own_number.clone())),
                ("Attachments".to_string(), owned(atts)),
            ]);
            let path = format!("{MODEMMANAGER}/{uuid}");
            s.messages.push((path.clone(), props_value(&props)));
            (path, props)
        };
        let op = OwnedObjectPath::try_from(path.as_str()).expect("path");
        self.conn
            .object_server()
            .at(
                op.clone(),
                MessageIface {
                    state: self.state.clone(),
                },
            )
            .await?;
        let emitter = SignalEmitter::new(&self.conn, MODEMMANAGER)?;
        Service::message_added(&emitter, op.clone(), props).await?;
        Ok(op)
    }

    /// Announce a message again (mmsd-tng reporting one twice).
    pub async fn announce_again(&self, path: &str) -> zbus::Result<()> {
        let props = {
            let s = self.state.lock().unwrap();
            s.messages
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, props)| props_value(props))
        };
        let Some(props) = props else {
            return Ok(());
        };
        let emitter = SignalEmitter::new(&self.conn, MODEMMANAGER)?;
        Service::message_added(&emitter, OwnedObjectPath::try_from(path)?, props).await
    }

    /// Move a message on ("sent", "delivered", "sending_failed").
    pub async fn set_status(&self, path: &str, status: &str) -> zbus::Result<()> {
        status_signal(&self.conn, &self.state, path, status).await
    }

    pub fn sent(&self) -> Vec<SentMms> {
        self.state.lock().unwrap().sent.clone()
    }

    pub fn deleted(&self) -> Vec<String> {
        self.state.lock().unwrap().deleted.clone()
    }

    /// The messages the mock still holds.
    pub fn held(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
}
