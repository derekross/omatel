// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent end to end against the scripted ModemManager (on a private
//! "system" bus) and the recording notification server (on a private
//! "session" bus), driven through its own session-bus API exactly as the
//! apps and the shell use it. Every number is a fictional 555-01xx one;
//! no emergency number ever reaches the mock modem.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use omatel_agent::{Config, Launcher, LockProbe};
use omatel_api::{Messages1Proxy, Phone1Proxy};
use omatel_contacts::{AddressBook, Contact, Phone, PhoneKind};
use omatel_mock::bus::PrivateBus;
use omatel_mock::mm::MockModem;
use omatel_mock::notifications::MockNotifications;
use omatel_store::{BlockList, CallLog, CallOutcome, MessageState, MessageStore};

const FRIEND: &str = "+12025550100";
const STRANGER: &str = "+12025550150";

struct Rig {
    _system: PrivateBus,
    _session: PrivateBus,
    dir: tempfile::TempDir,
    modem: MockModem,
    notes: MockNotifications,
    phone: Phone1Proxy<'static>,
    messages: Messages1Proxy<'static>,
    launches: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Rig {
    fn log(&self) -> CallLog {
        CallLog::open(&self.dir.path().join("data/calls.db")).unwrap()
    }

    fn store(&self) -> MessageStore {
        MessageStore::open(&self.dir.path().join("data/messages.db")).unwrap()
    }

    fn launched(&self) -> Vec<Vec<String>> {
        self.launches.lock().unwrap().clone()
    }

    fn write_settings(&self, text: &str) {
        std::fs::create_dir_all(self.dir.path().join("config")).unwrap();
        std::fs::write(self.dir.path().join("config/settings.toml"), text).unwrap();
    }
}

async fn rig_with(locked: bool, inline_reply: bool) -> Rig {
    let system = PrivateBus::start().unwrap();
    let session = PrivateBus::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut book = AddressBook::open(dir.path().join("contacts"), "US");
    let mut c = Contact::new("Alex Example");
    c.phones.push(Phone {
        number: "(202) 555-0100".into(),
        kind: PhoneKind::Mobile,
    });
    book.save(&c).unwrap();

    let modem = MockModem::start(system.connect().await.unwrap())
        .await
        .unwrap();
    let notes = MockNotifications::start(session.connect().await.unwrap(), inline_reply)
        .await
        .unwrap();
    let launches = Arc::new(Mutex::new(Vec::new()));
    let cfg = Config {
        data_dir: dir.path().join("data"),
        contacts_dir: dir.path().join("contacts"),
        settings_path: dir.path().join("config/settings.toml"),
        launcher: Launcher::Record(launches.clone()),
        lock_probe: LockProbe::Fixed(locked),
    };
    let sys = system.connect().await.unwrap();
    let ses = session.connect().await.unwrap();
    tokio::spawn(async move {
        omatel_agent::run(cfg, sys, ses).await.unwrap();
    });
    let client = session.connect().await.unwrap();
    // Wait for the agent's names.
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
    let phone = Phone1Proxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    let messages = Messages1Proxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    let rig = Rig {
        _system: system,
        _session: session,
        dir,
        modem,
        notes,
        phone,
        messages,
        launches,
    };
    eventually(|| async { rig.phone.modem_state().await.ok().as_deref() == Some("ready") }).await;
    rig
}

async fn rig() -> Rig {
    rig_with(false, true).await
}

/// Poll until `f` holds (5 s at most).
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

#[tokio::test]
async fn incoming_call_answered_logged_and_deleted() {
    let r = rig().await;
    let mut incoming = r.phone.receive_incoming_call().await.unwrap();
    let mut states = r.phone.receive_call_state_changed().await.unwrap();
    let mut log_changes = r.phone.receive_call_log_changed().await.unwrap();
    let path = r.modem.ring(FRIEND).await.unwrap();

    let sig = next(&mut incoming).await;
    let a = sig.args().unwrap();
    assert_eq!(a.id, path.as_str());
    assert_eq!(a.number, FRIEND);
    assert_eq!(a.name, "Alex Example");
    let first = next(&mut states).await.args().unwrap().call;
    assert_eq!(first.state, "ringing-in");

    eventually(|| async { r.notes.open().iter().any(|n| n.summary == "Incoming call") }).await;
    let note = r
        .notes
        .open()
        .into_iter()
        .find(|n| n.summary == "Incoming call")
        .unwrap();
    assert!(note.has_action("answer") && note.has_action("decline"));
    assert_eq!(note.body, "Alex Example");
    eventually(|| async {
        r.launched().contains(&vec![
            "omaphone".into(),
            "--incoming".into(),
            path.to_string(),
        ])
    })
    .await;

    let calls = r.phone.list_calls().await.unwrap();
    assert_eq!(calls.len(), 1);
    r.phone.answer(path.as_str()).await.unwrap();
    let active = next(&mut states).await.args().unwrap().call;
    assert_eq!(active.state, "active");
    assert!(active.answered_ms > 0);
    // Answering withdraws the incoming-call notification.
    eventually(|| async { !r.notes.open().iter().any(|n| n.summary == "Incoming call") }).await;

    r.phone.send_dtmf(path.as_str(), "42").await.unwrap();
    assert_eq!(r.modem.dtmf(), vec!["42"]);
    assert!(r.phone.send_dtmf(path.as_str(), "4a").await.is_err());
    r.phone.set_mute(true).await.unwrap();
    assert!(r.phone.muted().await.unwrap());
    r.phone.set_route("speaker").await.unwrap();
    assert_eq!(r.phone.route().await.unwrap(), "speaker");
    assert!(r.phone.set_route("ceiling").await.is_err());
    assert!(!r.phone.audio_routing().await.unwrap());

    r.phone.hangup(path.as_str()).await.unwrap();
    let ended = next(&mut states).await.args().unwrap().call;
    assert_eq!(ended.state, "terminated");
    next(&mut log_changes).await;
    let log = r.log().recent(10).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].outcome, CallOutcome::Answered);
    assert_eq!(log[0].number, FRIEND);
    eventually(|| async { r.modem.deleted_calls() == vec![path.to_string()] }).await;
    // Mute and route go back after the last call.
    eventually(|| async { !r.phone.muted().await.unwrap() }).await;
    assert_eq!(r.phone.route().await.unwrap(), "earpiece");
    assert!(r.phone.list_calls().await.unwrap().is_empty());
}

#[tokio::test]
async fn missed_call_notifies_and_is_marked_seen() {
    let r = rig().await;
    let path = r.modem.ring(STRANGER).await.unwrap();
    eventually(|| async { r.phone.list_calls().await.unwrap().len() == 1 }).await;
    r.modem.remote_hangup(&path).await.unwrap();
    eventually(|| async { r.phone.missed_calls().await.unwrap() == 1 }).await;
    let log = r.log().recent(10).unwrap();
    assert_eq!(log[0].outcome, CallOutcome::Missed);
    assert!(!log[0].seen);
    eventually(|| async { r.notes.open().iter().any(|n| n.summary == "Missed call") }).await;
    let n = r
        .notes
        .open()
        .into_iter()
        .find(|n| n.summary == "Missed call")
        .unwrap();
    assert!(n.has_action("call-back") && n.has_action("message"));
    assert_eq!(n.body, "(202) 555-0150");
    // "Message" opens Messages on that number and clears the badge.
    r.notes.invoke(n.id, "message").await.unwrap();
    eventually(|| async {
        r.launched()
            .contains(&vec!["omamessages".into(), format!("sms:{STRANGER}")])
    })
    .await;
    eventually(|| async { r.phone.missed_calls().await.unwrap() == 0 }).await;
    eventually(|| async { r.notes.open().iter().all(|n| n.summary != "Missed call") }).await;
}

#[tokio::test]
async fn declined_and_blocked_calls() {
    let r = rig().await;
    let path = r.modem.ring(FRIEND).await.unwrap();
    eventually(|| async { r.phone.list_calls().await.unwrap().len() == 1 }).await;
    r.phone.decline(path.as_str()).await.unwrap();
    eventually(|| async { r.log().recent(10).unwrap().len() == 1 }).await;
    assert_eq!(r.log().recent(1).unwrap()[0].outcome, CallOutcome::Rejected);

    BlockList::open(&r.dir.path().join("data/blocked.db"))
        .unwrap()
        .add("202-555-0150", "US")
        .unwrap();
    let mut incoming = r.phone.receive_incoming_call().await.unwrap();
    let blocked = r.modem.ring(STRANGER).await.unwrap();
    eventually(|| async { r.modem.hung_up().contains(&blocked.to_string()) }).await;
    eventually(|| async { r.log().recent(10).unwrap().len() == 2 }).await;
    assert_eq!(r.log().recent(1).unwrap()[0].outcome, CallOutcome::Blocked);
    // Never announced.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), incoming.next())
            .await
            .is_err()
    );
    assert!(r.notes.shown().iter().all(|n| n.body != "(202) 555-0150"));
}

#[tokio::test]
async fn outgoing_call_with_post_dial_tones() {
    let r = rig().await;
    let mut states = r.phone.receive_call_state_changed().await.unwrap();
    let id = r.phone.dial("+1 (202) 555-0101,123#").await.unwrap();
    let mut seen = Vec::new();
    while seen.last().map(String::as_str) != Some("active") {
        seen.push(next(&mut states).await.args().unwrap().call.state);
    }
    assert_eq!(seen.first().map(String::as_str), Some("unknown"));
    assert!(seen.contains(&"dialing".to_string()));
    eventually(|| async { r.modem.dtmf() == vec!["123#"] }).await;
    r.phone.hangup("").await.unwrap();
    eventually(|| async { r.log().recent(10).unwrap().len() == 1 }).await;
    let c = &r.log().recent(1).unwrap()[0];
    assert_eq!(c.outcome, CallOutcome::Answered);
    assert_eq!(c.number, "+12025550101");
    let _ = id;

    // Busy: the far end refuses.
    r.modem.set_auto_answer(false);
    let id = r.phone.dial("2025550102").await.unwrap();
    eventually(|| async {
        r.phone
            .list_calls()
            .await
            .unwrap()
            .iter()
            .any(|c| c.state == "ringing-out")
    })
    .await;
    r.modem
        .remote_hangup(&id.as_str().try_into().unwrap())
        .await
        .unwrap();
    eventually(|| async { r.log().recent(10).unwrap().len() == 2 }).await;
    assert_eq!(
        r.log().recent(1).unwrap()[0].outcome,
        CallOutcome::Unanswered
    );
    assert!(r.phone.dial("not a number").await.is_err());
}

#[tokio::test]
async fn emergency_numbers_are_refused_and_never_reach_the_modem() {
    let r = rig().await;
    for n in ["911", "112", "999", "911,1"] {
        let e = r.phone.dial(n).await.unwrap_err();
        assert!(e.to_string().contains("EmergencyUnsupported"), "{n}: {e}");
    }
    assert!(r.modem.calls().is_empty());
    let list = r.phone.emergency_numbers().await.unwrap();
    assert!(list.contains(&"911".to_string()) && list.contains(&"112".to_string()));
}

#[tokio::test]
async fn incoming_sms_stored_deleted_notified_and_replied() {
    let r = rig().await;
    let mut changed = r.messages.receive_messages_changed().await.unwrap();
    let path = r
        .modem
        .receive_sms(FRIEND, "Dinner at seven?", "2026-10-14T09:30:00+00:00")
        .await
        .unwrap();
    let conv = next(&mut changed).await.args().unwrap().conversation;
    eventually(|| async { r.modem.deleted_sms().contains(&path.to_string()) }).await;
    let store = r.store();
    let msgs = store.messages(conv, 10, None).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].body, "Dinner at seven?");
    assert_eq!(msgs[0].sent_at, Some(1_791_970_200_000));

    eventually(|| async { r.notes.open().iter().any(|n| n.summary == "Alex Example") }).await;
    let n = r
        .notes
        .open()
        .into_iter()
        .find(|n| n.summary == "Alex Example")
        .unwrap();
    assert_eq!(n.body, "Dinner at seven?");
    assert!(n.has_action("inline-reply"));
    assert_eq!(n.hint_str("category").as_deref(), Some("im.received"));
    assert_eq!(
        n.hint_str("x-omamobile-public-summary").as_deref(),
        Some("New message")
    );

    // A second message updates the same notification.
    r.modem.receive_sms(FRIEND, "Or eight?", "").await.unwrap();
    eventually(|| async {
        r.notes
            .open()
            .iter()
            .any(|m| m.id == n.id && m.summary == "Alex Example (2)")
    })
    .await;

    r.notes.reply(n.id, "Seven works").await.unwrap();
    eventually(|| async { r.modem.sent().len() == 1 }).await;
    let sent = &r.modem.sent()[0];
    assert_eq!(sent.text, "Seven works");
    assert_eq!(
        sent.number,
        "(202) 555-0100".replace("(202) 555-0100", FRIEND)
    );
    eventually(|| async { r.notes.open().iter().all(|m| m.id != n.id) }).await;
    eventually(|| async {
        let s = r.store();
        let m = s.messages(conv, 10, None).unwrap();
        m.len() == 3 && m.iter().all(|m| m.read) && m[2].state != MessageState::Sending
    })
    .await;
}

#[tokio::test]
async fn duplicate_sms_is_stored_once_and_wap_push_is_left_alone() {
    let r = rig().await;
    let a = r
        .modem
        .receive_sms(STRANGER, "same text", "2026-10-14T10:00:00+00:00")
        .await
        .unwrap();
    let b = r
        .modem
        .receive_sms(STRANGER, "same text", "2026-10-14T10:00:00+00:00")
        .await
        .unwrap();
    eventually(|| async {
        let d = r.modem.deleted_sms();
        d.contains(&a.to_string()) && d.contains(&b.to_string())
    })
    .await;
    let store = r.store();
    let convs = store.conversations(false).unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(store.messages(convs[0].id, 10, None).unwrap().len(), 1);

    let wap = r.modem.receive_wap_push(STRANGER).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(r.modem.messages().contains(&wap.to_string()));
    assert!(!r.modem.deleted_sms().contains(&wap.to_string()));
}

#[tokio::test]
async fn send_with_delivery_report_and_retry_after_failure() {
    let r = rig().await;
    let (conv, id) = r.messages.send("202-555-0101", "On my way").await.unwrap();
    eventually(|| async { r.modem.sent().len() == 1 }).await;
    let sent = r.modem.sent()[0].clone();
    assert!(sent.delivery_report);
    assert_eq!(sent.number, "202-555-0101");
    eventually(|| async { r.store().message(id).unwrap().unwrap().state == MessageState::Sent })
        .await;
    r.modem.deliver(&sent.path, 0).await.unwrap();
    eventually(|| async {
        r.store().message(id).unwrap().unwrap().state == MessageState::Delivered
    })
    .await;
    eventually(|| async { r.modem.deleted_sms().contains(&sent.path) }).await;

    r.modem.set_fail_sends(true);
    let id2 = r
        .messages
        .send_to_conversation(conv, "second")
        .await
        .unwrap();
    eventually(|| async { r.store().message(id2).unwrap().unwrap().state == MessageState::Failed })
        .await;
    r.modem.set_fail_sends(false);
    r.messages.retry(id2).await.unwrap();
    eventually(|| async { r.store().message(id2).unwrap().unwrap().state == MessageState::Sent })
        .await;
    assert!(r.messages.retry(id2).await.is_err());
    assert!(r.messages.send("", "x").await.is_err());
    assert!(r.messages.send(FRIEND, "  ").await.is_err());
    assert!(r.messages.ready().await.unwrap());

    // Without delivery reports the modem's copy goes right after sending.
    r.write_settings("delivery_reports = false\n");
    let id3 = r
        .messages
        .send_to_conversation(conv, "third")
        .await
        .unwrap();
    eventually(|| async { r.store().message(id3).unwrap().unwrap().state == MessageState::Sent })
        .await;
    let last = r.modem.sent().last().unwrap().clone();
    assert!(!last.delivery_report);
    eventually(|| async { r.modem.deleted_sms().contains(&last.path) }).await;
}

#[tokio::test]
async fn locked_phone_shows_new_message_only() {
    let r = rig_with(true, true).await;
    r.modem
        .receive_sms(FRIEND, "secret plans", "")
        .await
        .unwrap();
    eventually(|| async { !r.notes.open().is_empty() }).await;
    let n = &r.notes.open()[0];
    assert_eq!(n.summary, "New message");
    assert_eq!(n.body, "");
    assert!(!n.has_action("inline-reply"));
    assert!(r.notes.shown().iter().all(|n| !n.body.contains("secret")));

    // With details allowed on the lock screen, the next one has them.
    r.write_settings("lockscreen_details = true\n");
    r.modem.receive_sms(FRIEND, "more plans", "").await.unwrap();
    eventually(|| async { r.notes.open().iter().any(|n| n.body.contains("more plans")) }).await;
    let n = r
        .notes
        .open()
        .into_iter()
        .find(|n| n.body.contains("more plans"))
        .unwrap();
    assert!(n.hint_str("x-omamobile-public-summary").is_none());
}

#[tokio::test]
async fn muted_blocked_and_disabled_notifications() {
    let r = rig_with(false, false).await;
    // Without inline reply the notification offers a Reply button.
    r.modem.receive_sms(FRIEND, "hello", "").await.unwrap();
    eventually(|| async { !r.notes.open().is_empty() }).await;
    let n = r.notes.open()[0].clone();
    assert!(n.has_action("reply") && !n.has_action("inline-reply"));
    r.notes.invoke(n.id, "default").await.unwrap();
    let store = r.store();
    let conv = store.conversations(false).unwrap()[0].id;
    eventually(|| async {
        r.launched().contains(&vec![
            "omamessages".into(),
            "--conversation".into(),
            conv.to_string(),
        ])
    })
    .await;
    r.messages.mark_read(conv).await.unwrap();
    eventually(|| async { r.notes.open().is_empty() }).await;
    assert_eq!(r.store().conversation(conv).unwrap().unwrap().unread, 0);

    store.set_muted(conv, true).unwrap();
    let before = r.notes.shown().len();
    r.modem.receive_sms(FRIEND, "muted one", "").await.unwrap();
    eventually(|| async { r.store().unread_total().unwrap() == 1 }).await;

    BlockList::open(&r.dir.path().join("data/blocked.db"))
        .unwrap()
        .add(STRANGER, "US")
        .unwrap();
    r.modem
        .receive_sms(STRANGER, "blocked one", "")
        .await
        .unwrap();
    eventually(|| async { r.store().unread_total().unwrap() == 2 }).await;

    r.write_settings("message_notifications = false\n");
    r.modem
        .receive_sms("+12025550160", "nobody hears", "")
        .await
        .unwrap();
    eventually(|| async { r.store().unread_total().unwrap() == 3 }).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(r.notes.shown().len(), before);
}

#[tokio::test]
async fn modem_appearing_later_and_open_conversation() {
    let system = PrivateBus::start().unwrap();
    let session = PrivateBus::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _notes = MockNotifications::start(session.connect().await.unwrap(), true)
        .await
        .unwrap();
    let launches = Arc::new(Mutex::new(Vec::new()));
    let cfg = Config {
        data_dir: dir.path().join("data"),
        contacts_dir: dir.path().join("contacts"),
        settings_path: dir.path().join("settings.toml"),
        launcher: Launcher::Record(launches.clone()),
        lock_probe: LockProbe::Fixed(false),
    };
    let (sys, ses) = (
        system.connect().await.unwrap(),
        session.connect().await.unwrap(),
    );
    tokio::spawn(async move { omatel_agent::run(cfg, sys, ses).await.unwrap() });
    let client = session.connect().await.unwrap();
    let phone = Phone1Proxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    let messages = Messages1Proxy::builder(&client)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    eventually(|| async { phone.modem_state().await.ok().as_deref() == Some("no-modem") }).await;
    assert!(phone
        .dial("2025550101")
        .await
        .unwrap_err()
        .to_string()
        .contains("NoModem"));
    // Sending without a modem fails the message (the user can retry).
    let (_, id) = messages.send(FRIEND, "queued?").await.unwrap();
    let store = MessageStore::open(&dir.path().join("data/messages.db")).unwrap();
    eventually(|| async { store.message(id).unwrap().unwrap().state == MessageState::Failed })
        .await;

    let _modem = MockModem::start(system.connect().await.unwrap())
        .await
        .unwrap();
    eventually(|| async { phone.modem_state().await.ok().as_deref() == Some("ready") }).await;
    messages.open_conversation("(202) 555-0100").await.unwrap();
    let conv = store.find_conversation(FRIEND).unwrap().unwrap();
    assert!(launches.lock().unwrap().contains(&vec![
        "omamessages".into(),
        "--conversation".into(),
        conv.to_string()
    ]));
}
