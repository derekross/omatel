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
    let n = Notifier::new(&conn, "Messages", "org.omamobile.Messages")
        .await
        .unwrap();
    assert!(n.supports_inline_reply().await);
    let mut events = n.events().await.unwrap();

    let mut note = Notification::new("A sender", "first").action(DEFAULT_ACTION, "Open");
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
    let n = Notifier::new(&conn, "Phone", "org.omamobile.Phone")
        .await
        .unwrap();
    assert!(!n.supports_inline_reply().await);
}
