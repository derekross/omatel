// SPDX-License-Identifier: GPL-3.0-or-later
//! omatel-mm's proxies against the scripted ModemManager on a private bus:
//! an incoming call, an outgoing call, SMS in and out, a delivery report.
//! Every number is a fictional 555-01xx one.

use std::time::Duration;

use futures_util::StreamExt;
use omatel_mm::*;
use omatel_mock::bus::PrivateBus;
use omatel_mock::mm::{MockModem, MODEM};
use zbus::Connection;

const CALLER: &str = "+12025550123";
const OTHER: &str = "+12025550199";

async fn setup() -> (PrivateBus, MockModem, Connection) {
    let bus = PrivateBus::start().unwrap();
    let mock = MockModem::start(bus.connect().await.unwrap())
        .await
        .unwrap();
    let client = bus.connect().await.unwrap();
    (bus, mock, client)
}

async fn voice(c: &Connection) -> VoiceProxy<'static> {
    VoiceProxy::builder(c)
        .path(MODEM)
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn messaging(c: &Connection) -> MessagingProxy<'static> {
    MessagingProxy::builder(c)
        .path(MODEM)
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn call(c: &Connection, path: &zbus::zvariant::OwnedObjectPath) -> CallProxy<'static> {
    CallProxy::builder(c)
        .path(path.clone())
        .unwrap()
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap()
}

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("timed out")
}

#[tokio::test]
async fn finds_the_modem() {
    let (_bus, _mock, c) = setup().await;
    wait_for_service(&c).await.unwrap();
    let m = modems(&c).await.unwrap();
    assert_eq!(m.len(), 1);
    assert!(m[0].has_voice && m[0].has_messaging);
    let modem = ModemProxy::builder(&c)
        .path(MODEM)
        .unwrap()
        .build()
        .await
        .unwrap();
    assert_eq!(modem.state().await.unwrap(), modem_state::REGISTERED);
    let sim = SimProxy::builder(&c)
        .path(modem.sim().await.unwrap())
        .unwrap()
        .build()
        .await
        .unwrap();
    assert!(sim
        .emergency_numbers()
        .await
        .unwrap()
        .contains(&"911".to_string()));
}

#[tokio::test]
async fn incoming_call_answer_dtmf_hangup() {
    let (_bus, mock, c) = setup().await;
    let v = voice(&c).await;
    let mut added = v.receive_call_added().await.unwrap();
    let path = mock.ring(CALLER).await.unwrap();
    let got = within(added.next()).await.unwrap();
    assert_eq!(got.args().unwrap().path, path);
    let cp = call(&c, &path).await;
    assert_eq!(
        CallState::from_i32(cp.state().await.unwrap()),
        CallState::RingingIn
    );
    assert_eq!(cp.direction().await.unwrap(), call_direction::INCOMING);
    assert_eq!(cp.number().await.unwrap(), CALLER);
    let mut changes = cp.receive_call_state_changed().await.unwrap();
    cp.accept().await.unwrap();
    let ch = within(changes.next()).await.unwrap();
    let a = ch.args().unwrap();
    assert_eq!((a.old, a.new), (3, 4));
    cp.send_dtmf("1").await.unwrap();
    assert!(cp.send_dtmf("x").await.is_err());
    assert_eq!(mock.dtmf(), vec!["1"]);
    cp.hangup().await.unwrap();
    let ch = within(changes.next()).await.unwrap();
    assert_eq!(ch.args().unwrap().new, 7);
    v.delete_call(&path.as_ref()).await.unwrap();
    assert!(v.list_calls().await.unwrap().is_empty());
}

#[tokio::test]
async fn outgoing_call_rings_out_and_is_answered() {
    let (_bus, mock, c) = setup().await;
    let v = voice(&c).await;
    let path = v.create_call(call_properties(OTHER)).await.unwrap();
    let cp = call(&c, &path).await;
    assert_eq!(cp.direction().await.unwrap(), call_direction::OUTGOING);
    let mut changes = cp.receive_call_state_changed().await.unwrap();
    cp.start().await.unwrap();
    let mut seen = Vec::new();
    while seen.last() != Some(&4) {
        seen.push(within(changes.next()).await.unwrap().args().unwrap().new);
    }
    assert_eq!(seen, vec![1, 2, 4]);
    mock.remote_hangup(&path).await.unwrap();
    assert_eq!(within(changes.next()).await.unwrap().args().unwrap().new, 7);
    // A refused call ends with REFUSED_OR_BUSY.
    mock.set_auto_answer(false);
    let p2 = v.create_call(call_properties(OTHER)).await.unwrap();
    let c2 = call(&c, &p2).await;
    let mut ch2 = c2.receive_call_state_changed().await.unwrap();
    c2.start().await.unwrap();
    within(ch2.next()).await.unwrap();
    within(ch2.next()).await.unwrap();
    mock.remote_hangup(&p2).await.unwrap();
    let a = within(ch2.next()).await.unwrap();
    assert_eq!(a.args().unwrap().reason, call_reason::REFUSED_OR_BUSY);
}

#[tokio::test]
async fn sms_in_out_and_delivery_report() {
    let (_bus, mock, c) = setup().await;
    let m = messaging(&c).await;
    let mut added = m.receive_added().await.unwrap();
    let path = mock
        .receive_sms(CALLER, "hello from the test", "2026-10-14T09:30:00+00:00")
        .await
        .unwrap();
    let a = within(added.next()).await.unwrap();
    assert!(a.args().unwrap().received);
    let info = SmsInfo::read(&c, path.clone()).await.unwrap();
    assert!(info.is_received_text());
    assert_eq!(info.number, CALLER);
    assert_eq!(info.text, "hello from the test");
    assert_eq!(parse_timestamp(&info.timestamp), Some(1_791_970_200_000));
    m.delete(&path.as_ref()).await.unwrap();
    assert_eq!(mock.deleted_sms(), vec![path.to_string()]);

    let out = m
        .create(sms_properties(OTHER, "a reply", true))
        .await
        .unwrap();
    let sp = SmsProxy::builder(&c)
        .path(out.clone())
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut delivery = sp.receive_delivery_state_changed().await;
    sp.send().await.unwrap();
    assert_eq!(sp.state().await.unwrap(), sms_state::SENT);
    let sent = mock.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].delivery_report);
    assert_eq!(sent[0].text, "a reply");
    // The first item of a property stream is the current value.
    let first = within(delivery.next()).await.unwrap().get().await.unwrap();
    assert_eq!(Delivery::from_u32(first), Delivery::Unknown);
    mock.deliver(out.as_str(), 0).await.unwrap();
    let v = within(delivery.next()).await.unwrap().get().await.unwrap();
    assert_eq!(Delivery::from_u32(v), Delivery::Delivered);

    mock.set_fail_sends(true);
    let bad = m
        .create(sms_properties(OTHER, "won't go", false))
        .await
        .unwrap();
    let bp = SmsProxy::builder(&c)
        .path(bad)
        .unwrap()
        .build()
        .await
        .unwrap();
    assert!(bp.send().await.is_err());
}

#[tokio::test]
async fn wap_push_is_binary() {
    let (_bus, mock, c) = setup().await;
    let path = mock.receive_wap_push(CALLER).await.unwrap();
    let info = SmsInfo::read(&c, path).await.unwrap();
    assert!(info.is_binary());
    assert!(!info.is_received_text());
}
