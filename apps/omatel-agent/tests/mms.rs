// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent as mmsd-tng's client, end to end: the scripted mmsd-tng
//! (`omatel_mock::mmsd`) and the notification server on a private
//! "session" bus, the scripted ModemManager on a private "system" bus,
//! and the agent driven through `org.omamobile.Messages1`. Every number is
//! a fictional 555-01xx one.

use std::time::Duration;

use futures_util::StreamExt;
use omatel_agent::{Config, Launcher, LockProbe};
use omatel_api::Messages1Proxy;
use omatel_contacts::{AddressBook, Contact, Phone, PhoneKind};
use omatel_mock::bus::PrivateBus;
use omatel_mock::mm::MockModem;
use omatel_mock::mmsd::{Incoming, MockMmsd};
use omatel_mock::notifications::MockNotifications;
use omatel_store::{MessageState, MessageStore};

const FRIEND: &str = "+12025550100";
const OTHER: &str = "+12025550102";
const STRANGER: &str = "+12025550150";
const ME: &str = "+12025550199";

struct Rig {
    _system: PrivateBus,
    session: PrivateBus,
    dir: tempfile::TempDir,
    modem: MockModem,
    notes: MockNotifications,
    messages: Messages1Proxy<'static>,
}

impl Rig {
    fn store(&self) -> MessageStore {
        MessageStore::open(&self.dir.path().join("data/messages.db")).unwrap()
    }

    fn write_settings(&self, text: &str) {
        std::fs::create_dir_all(self.dir.path().join("config")).unwrap();
        std::fs::write(self.dir.path().join("config/settings.toml"), text).unwrap();
    }

    /// A mock mmsd-tng on the session bus, its name not taken yet.
    async fn mmsd(&self) -> MockMmsd {
        let m = MockMmsd::serve(self.session.connect().await.unwrap())
            .await
            .unwrap();
        m.set_own_number(ME);
        m
    }

    async fn mmsd_running(&self) -> MockMmsd {
        let m = self.mmsd().await;
        m.claim().await.unwrap();
        eventually(|| async { self.messages.mms_ready().await.unwrap_or(false) }).await;
        m
    }

    /// A file to attach, in the test's directory.
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let p = self.dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    }
}

/// mmsd-tng installed (its user unit in place) but not started: the
/// phones this is for.
async fn rig() -> Rig {
    rig_with(true).await
}

async fn rig_with(installed: bool) -> Rig {
    let system = PrivateBus::start().unwrap();
    let session = PrivateBus::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut book = AddressBook::open(dir.path().join("contacts"), "US");
    let mut c = Contact::new("Alex Example");
    c.phones.push(Phone {
        number: "(202) 555-0100".into(),
        kind: PhoneKind::Mobile,
        label: String::new(),
    });
    book.save(&c).unwrap();
    let modem = MockModem::start(system.connect().await.unwrap())
        .await
        .unwrap();
    let notes = MockNotifications::start(session.connect().await.unwrap(), true)
        .await
        .unwrap();
    let cfg = Config {
        data_dir: dir.path().join("data"),
        contacts_dir: dir.path().join("contacts"),
        settings_path: dir.path().join("config/settings.toml"),
        launcher: Launcher::Off,
        lock_probe: LockProbe::Fixed(false),
        dnd_path: dir.path().join("omarchy/notifications.json"),
        mms_units: vec![dir.path().join("systemd/user/mmsd-tng.service")],
    };
    if installed {
        std::fs::create_dir_all(dir.path().join("systemd/user")).unwrap();
        std::fs::write(dir.path().join("systemd/user/mmsd-tng.service"), "").unwrap();
    }
    let sys = system.connect().await.unwrap();
    let ses = session.connect().await.unwrap();
    tokio::spawn(async move {
        omatel_agent::run(cfg, sys, ses).await.unwrap();
    });
    let client = session.connect().await.unwrap();
    let dbus = zbus::fdo::DBusProxy::new(&client).await.unwrap();
    for _ in 0..200 {
        if dbus
            .name_has_owner("org.omamobile.Messages1".try_into().unwrap())
            .await
            .unwrap()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let messages = Messages1Proxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    let rig = Rig {
        _system: system,
        session,
        dir,
        modem,
        notes,
        messages,
    };
    eventually(|| async { rig.messages.ready().await.unwrap_or(false) }).await;
    rig
}

async fn eventually<F, Fut>(f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..250 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not reached in 5 s");
}

async fn next<S: futures_util::Stream + Unpin>(s: &mut S) -> S::Item {
    tokio::time::timeout(Duration::from_secs(5), s.next())
        .await
        .expect("timed out")
        .expect("stream ended")
}

fn text(t: &str) -> (String, String, Vec<u8>) {
    (
        "text_0001.txt".into(),
        "text/plain;charset=utf-8".into(),
        t.as_bytes().to_vec(),
    )
}

fn image(id: &str, bytes: &[u8]) -> (String, String, Vec<u8>) {
    (id.into(), "image/jpeg".into(), bytes.to_vec())
}

#[tokio::test]
async fn incoming_group_mms_is_stored_with_its_picture_and_notified() {
    let r = rig().await;
    let mmsd = r.mmsd_running().await;
    let mut changed = r.messages.receive_messages_changed().await.unwrap();
    let path = mmsd
        .receive(&Incoming {
            sender: FRIEND.into(),
            recipients: vec![ME.into(), OTHER.into()],
            date: "2026-10-14T09:30:00+0000".into(),
            parts: vec![
                (
                    "smil".into(),
                    "application/smil".into(),
                    b"<smil/>".to_vec(),
                ),
                text("See you there"),
                image("<photo.jpg>", b"JPEGDATA"),
            ],
        })
        .await
        .unwrap();
    let conv = next(&mut changed).await.args().unwrap().conversation;
    // Stored, then deleted from mmsd-tng.
    eventually(|| async { mmsd.deleted().contains(&path.to_string()) }).await;
    let store = r.store();
    let c = store.conversation(conv).unwrap().unwrap();
    assert_eq!(c.address, format!("{FRIEND},{OTHER}"));
    let msgs = store.messages(conv, 10, None).unwrap();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.transport, "mms");
    assert_eq!(m.body, "See you there");
    assert_eq!(m.sender, FRIEND);
    assert_eq!(m.sent_at, Some(1_791_970_200_000));

    let parts = r.messages.list_parts(m.id).await.unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].mime, "image/jpeg");
    assert_eq!(parts[0].size, 8);
    assert_eq!(parts[0].name(), "photo.jpg");
    assert_eq!(std::fs::read(&parts[0].path).unwrap(), b"JPEGDATA");

    eventually(|| async {
        r.notes
            .open()
            .iter()
            .any(|n| n.body == "Alex: See you there")
    })
    .await;
    let n = r
        .notes
        .open()
        .into_iter()
        .find(|n| n.body == "Alex: See you there")
        .unwrap();
    assert_eq!(n.summary, "Alex Example, (202) 555-0102");
}

#[tokio::test]
async fn a_picture_alone_says_picture_and_comes_to_a_one_to_one_thread() {
    let r = rig().await;
    let mmsd = r.mmsd_running().await;
    // No own number known: the single recipient is this phone.
    mmsd.set_own_number("");
    let mut changed = r.messages.receive_messages_changed().await.unwrap();
    mmsd.receive(&Incoming {
        sender: STRANGER.into(),
        recipients: vec![ME.into()],
        date: "2026-10-14T09:30:00+0000".into(),
        parts: vec![image("", b"PIXELS")],
    })
    .await
    .unwrap();
    let conv = next(&mut changed).await.args().unwrap().conversation;
    let c = r.store().conversation(conv).unwrap().unwrap();
    assert_eq!(c.address, STRANGER);
    let m = &r.store().messages(conv, 10, None).unwrap()[0];
    assert_eq!(m.body, "");
    assert_eq!(m.sender, "");
    let parts = r.messages.list_parts(m.id).await.unwrap();
    assert_eq!(parts[0].name(), "part.jpg");
    eventually(|| async {
        r.notes
            .open()
            .iter()
            .any(|n| n.summary == "(202) 555-0150" && n.body == "Picture")
    })
    .await;
}

#[tokio::test]
async fn mms_waiting_in_mmsd_is_imported_when_it_starts_and_cleaned_up() {
    let r = rig().await;
    assert!(!r.messages.mms_ready().await.unwrap());
    // mmsd-tng holds a message before the agent can see it.
    let mmsd = r.mmsd().await;
    let path = mmsd
        .receive(&Incoming {
            sender: FRIEND.into(),
            recipients: vec![ME.into()],
            date: "2026-10-14T09:30:00+0000".into(),
            parts: vec![text("waiting for you")],
        })
        .await
        .unwrap();
    mmsd.claim().await.unwrap();
    eventually(|| async { mmsd.deleted().contains(&path.to_string()) }).await;
    assert!(r.messages.mms_ready().await.unwrap());
    let store = r.store();
    let hits = store.search("waiting", 5).unwrap();
    assert_eq!(hits.len(), 1);
    // Imported from the list: its time is the network's.
    assert_eq!(hits[0].message.at, 1_791_970_200_000);

    // Deleted in Messages (straight in the store): forgotten, and the
    // agent drops the note once mmsd-tng no longer has the message.
    store.delete_conversation(hits[0].conversation_id).unwrap();
    assert_eq!(store.forgotten_mms().unwrap(), vec![path.to_string()]);
    mmsd.stop().await.unwrap();
    eventually(|| async { !r.messages.mms_ready().await.unwrap_or(true) }).await;
    mmsd.claim().await.unwrap();
    eventually(|| async { r.store().forgotten_mms().unwrap().is_empty() }).await;
}

#[tokio::test]
async fn group_send_goes_as_mms_and_follows_mmsd() {
    let r = rig().await;
    let mmsd = r.mmsd_running().await;
    let pic = r.file("cat.png", b"PNGBYTES");
    let (conv, id) = r
        .messages
        .send_with_attachments(&[OTHER, FRIEND], "Hello all", &[(&pic, "image/png")])
        .await
        .unwrap();
    // The caller's file may go: the agent has its own copy.
    std::fs::remove_file(&pic).unwrap();
    eventually(|| async { mmsd.sent().len() == 1 }).await;
    let sent = &mmsd.sent()[0];
    assert_eq!(sent.recipients, vec![FRIEND, OTHER]);
    assert_eq!(sent.delivery_report, Some(true));
    assert_eq!(
        sent.attachments,
        vec![
            (
                "text.txt".into(),
                "text/plain".into(),
                b"Hello all".to_vec()
            ),
            ("cat.png".into(), "image/png".into(), b"PNGBYTES".to_vec()),
        ]
    );
    let c = r.store().conversation(conv).unwrap().unwrap();
    assert_eq!(c.numbers, vec![FRIEND, OTHER]);
    let m = r.store().message(id).unwrap().unwrap();
    assert_eq!(m.transport, "mms");
    assert_eq!(m.body, "Hello all");
    let parts = r.messages.list_parts(id).await.unwrap();
    assert_eq!(std::fs::read(&parts[0].path).unwrap(), b"PNGBYTES");

    // mmsd-tng's "sent", then the delivery report.
    eventually(|| async { r.store().message(id).unwrap().unwrap().state == MessageState::Sent })
        .await;
    assert!(mmsd.held().contains(&sent.path), "kept for its report");
    mmsd.set_status(&sent.path, "delivered").await.unwrap();
    eventually(|| async {
        r.store().message(id).unwrap().unwrap().state == MessageState::Delivered
    })
    .await;
    eventually(|| async { mmsd.deleted().contains(&sent.path) }).await;

    // A plain text to the group is an MMS too (SMS has no groups).
    let id2 = r
        .messages
        .send_to_conversation(conv, "One more thing")
        .await
        .unwrap();
    eventually(|| async { mmsd.sent().len() == 2 }).await;
    assert_eq!(
        mmsd.sent()[1].attachments,
        vec![(
            "text.txt".into(),
            "text/plain".into(),
            b"One more thing".to_vec()
        )]
    );
    assert_eq!(r.store().message(id2).unwrap().unwrap().transport, "mms");
    assert!(r.modem.sent().is_empty());
    // The text file handed to mmsd-tng is not kept.
    assert!(!r
        .dir
        .path()
        .join(format!("data/parts/out-{id2}/text.txt"))
        .exists());
}

#[tokio::test]
async fn text_to_one_person_stays_sms() {
    let r = rig().await;
    let mmsd = r.mmsd_running().await;
    let (_, id) = r
        .messages
        .send_with_attachments(&[FRIEND], "just text", &[])
        .await
        .unwrap();
    eventually(|| async { r.modem.sent().len() == 1 }).await;
    assert_eq!(r.modem.sent()[0].text, "just text");
    assert_eq!(r.store().message(id).unwrap().unwrap().transport, "sms");
    assert!(mmsd.sent().is_empty());
    assert!(r
        .messages
        .send_with_attachments(&[], "nobody", &[])
        .await
        .is_err());
    assert!(r
        .messages
        .send_with_attachments(&[FRIEND], "", &[("/nonexistent/x.png", "image/png")])
        .await
        .is_err());
}

#[tokio::test]
async fn without_mmsd_an_mms_fails_clearly_and_retry_sends_it() {
    let r = rig().await;
    r.write_settings("delivery_reports = false\n");
    let pic = r.file("a.jpg", b"JPEG");
    let mut changed = r.messages.receive_messages_changed().await.unwrap();
    let err = r
        .messages
        .send_with_attachments(&[FRIEND], "", &[(&pic, "image/jpeg")])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("MmsUnavailable"), "{err}");
    let conv = next(&mut changed).await.args().unwrap().conversation;
    eventually(|| async {
        r.store()
            .messages(conv, 5, None)
            .unwrap()
            .first()
            .is_some_and(|m| m.state == MessageState::Failed)
    })
    .await;
    let m = r.store().messages(conv, 5, None).unwrap()[0].clone();
    assert_eq!(
        m.error.as_deref(),
        Some("The MMS service (mmsd-tng) isn't running")
    );

    let mmsd = r.mmsd_running().await;
    r.messages.retry(m.id).await.unwrap();
    eventually(|| async { mmsd.sent().len() == 1 }).await;
    eventually(|| async { r.store().message(m.id).unwrap().unwrap().state == MessageState::Sent })
        .await;
    // No report asked for: mmsd-tng's copy goes once sent.
    let path = mmsd.sent()[0].path.clone();
    eventually(|| async { mmsd.deleted().contains(&path) }).await;
}

/// A phone with no MMS service at all (mmsd-tng not installed): the agent
/// says so, refuses an attachment or a group message up front and stores
/// nothing; text to one person still goes as SMS. Once mmsd-tng turns up,
/// MMS is available.
#[tokio::test]
async fn without_an_mms_service_mms_is_refused_and_nothing_stored() {
    let r = rig_with(false).await;
    eventually(|| async { r.messages.ready().await.unwrap_or(false) }).await;
    assert!(!r.messages.mms_available().await.unwrap());
    assert!(!r.messages.mms_ready().await.unwrap());
    let pic = r.file("a.jpg", b"JPEG");
    let err = r
        .messages
        .send_with_attachments(&[FRIEND], "", &[(&pic, "image/jpeg")])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("MmsUnavailable"), "{err}");
    let err = r
        .messages
        .send_with_attachments(&[FRIEND, OTHER], "hi all", &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("MmsUnavailable"), "{err}");
    let store = r.store();
    assert!(store.conversations(false).unwrap().is_empty());
    // Nothing was copied in for a message that was never stored.
    assert!(!r.dir.path().join("data/parts").exists());

    let (conv, id) = r
        .messages
        .send_with_attachments(&[FRIEND], "plain text", &[])
        .await
        .unwrap();
    eventually(|| async { !r.modem.sent().is_empty() }).await;
    let m = r.store().message(id).unwrap().unwrap();
    assert_eq!((m.conversation_id, m.transport.as_str()), (conv, "sms"));

    // Messages follows the property, so its change is announced.
    let props = zbus::fdo::PropertiesProxy::builder(r.messages.inner().connection())
        .destination("org.omamobile.Messages1")
        .unwrap()
        .path("/org/omamobile/Messages1")
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut changes = props.receive_properties_changed().await.unwrap();
    let _mmsd = r.mmsd_running().await;
    assert!(r.messages.mms_available().await.unwrap());
    loop {
        let c = next(&mut changes).await;
        let args = c.args().unwrap();
        if let Some(v) = args.changed_properties().get("MmsAvailable") {
            assert!(bool::try_from(v).unwrap());
            break;
        }
    }
}

#[tokio::test]
async fn a_refused_mms_is_failed_and_too_big_is_explained() {
    let r = rig().await;
    let mmsd = r.mmsd_running().await;
    mmsd.set_fail_sends(true);
    let (_, id) = r
        .messages
        .send_with_attachments(&[FRIEND, OTHER], "fails", &[])
        .await
        .unwrap();
    eventually(|| async { r.store().message(id).unwrap().unwrap().state == MessageState::Failed })
        .await;
    let path = mmsd.sent()[0].path.clone();
    eventually(|| async { mmsd.deleted().contains(&path) }).await;

    mmsd.set_fail_sends(false);
    mmsd.set_max_size(4);
    let pic = r.file("big.jpg", b"TOO MANY BYTES");
    let (_, id) = r
        .messages
        .send_with_attachments(&[FRIEND], "", &[(&pic, "image/jpeg")])
        .await
        .unwrap();
    eventually(|| async { r.store().message(id).unwrap().unwrap().state == MessageState::Failed })
        .await;
    let m = r.store().message(id).unwrap().unwrap();
    assert!(m.error.unwrap().starts_with("Too big for MMS"));
    assert_eq!(mmsd.sent().len(), 1);
}
