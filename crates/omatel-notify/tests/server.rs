// SPDX-License-Identifier: GPL-3.0-or-later
//! The client against the recording notification server on a private bus.

use std::time::Duration;

use futures_util::StreamExt;
use omatel_mock::bus::PrivateBus;
use omatel_mock::notifications::MockNotifications;
use omatel_notify::*;

#[tokio::test]
async fn show_replace_reply_and_close() {
    let bus = PrivateBus::start().unwrap();
    let server = MockNotifications::start(bus.connect().await.unwrap(), true)
        .await
        .unwrap();
    let conn = bus.connect().await.unwrap();
    let n = Notifier::new(&conn).await.unwrap();
    assert!(n.supports_inline_reply().await);
    let mut events = n.events().await.unwrap();

    let mut note = Notification::new(MESSAGES, "A sender", "first").action(DEFAULT_ACTION, "Open");
    note.reply_placeholder = Some("Reply".into());
    note.category = "im.received".into();
    note.public_summary = Some("New message".into());
    let id = n.show(&note).await.unwrap();
    note.replaces = id;
    note.body = "second".into();
    assert_eq!(n.show(&note).await.unwrap(), id);
    let open = server.open();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].body, "second");
    assert!(open[0].has_action("inline-reply"));
    assert_eq!(open[0].hint_str("category").as_deref(), Some("im.received"));
    assert_eq!(
        open[0].hint_str("x-omamobile-public-summary").as_deref(),
        Some("New message")
    );
    assert_eq!(
        open[0].hint_str("desktop-entry").as_deref(),
        Some("org.omamobile.Messages")
    );
    assert_eq!(open[0].app_name, "Messages");
    assert_eq!(open[0].app_icon, "org.omamobile.Messages");

    server.reply(id, "typed reply").await.unwrap();
    let e = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        e,
        Event::Replied {
            id,
            text: "typed reply".into()
        }
    );
    server.invoke(id, "default").await.unwrap();
    let e = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        e,
        Event::Action {
            id,
            key: "default".into()
        }
    );
    n.close(id).await.unwrap();
    let e = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(e, Event::Closed { id: i, .. } if i == id));
    assert!(server.open().is_empty());
}

#[tokio::test]
async fn without_inline_reply_support() {
    let bus = PrivateBus::start().unwrap();
    let _server = MockNotifications::start(bus.connect().await.unwrap(), false)
        .await
        .unwrap();
    let conn = bus.connect().await.unwrap();
    let n = Notifier::new(&conn).await.unwrap();
    assert!(!n.supports_inline_reply().await);
}

#[tokio::test]
async fn each_notification_names_its_own_app() {
    let bus = PrivateBus::start().unwrap();
    let server = MockNotifications::start(bus.connect().await.unwrap(), false)
        .await
        .unwrap();
    let conn = bus.connect().await.unwrap();
    let n = Notifier::new(&conn).await.unwrap();
    n.show(&Notification::new(PHONE, "Missed call", "a caller"))
        .await
        .unwrap();
    n.show(&Notification::new(MESSAGES, "A sender", "text"))
        .await
        .unwrap();
    let shown = server.open();
    let names: Vec<(&str, &str, Option<String>)> = shown
        .iter()
        .map(|s| {
            (
                s.app_name.as_str(),
                s.app_icon.as_str(),
                s.hint_str("desktop-entry"),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            (
                "Phone",
                "org.omamobile.Phone",
                Some("org.omamobile.Phone".into())
            ),
            (
                "Messages",
                "org.omamobile.Messages",
                Some("org.omamobile.Messages".into())
            ),
        ]
    );
}
