// SPDX-License-Identifier: GPL-3.0-or-later
//! An org.freedesktop.Notifications server for tests: it records every
//! notification and lets the test press an action or type an inline reply
//! (the `NotificationReplied` extension Quickshell implements).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::{fdo, interface, Connection};

pub const PATH: &str = "/org/freedesktop/Notifications";

#[derive(Debug, Clone)]
pub struct Shown {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub replaces_id: u32,
    pub summary: String,
    pub body: String,
    /// (key, label) pairs.
    pub actions: Vec<(String, String)>,
    pub hints: HashMap<String, OwnedValue>,
    pub closed: bool,
}

impl Shown {
    pub fn hint_str(&self, key: &str) -> Option<String> {
        self.hints
            .get(key)
            .and_then(|v| String::try_from(v.clone()).ok())
    }

    pub fn has_action(&self, key: &str) -> bool {
        self.actions.iter().any(|(k, _)| k == key)
    }
}

#[derive(Default)]
struct Shared {
    next: u32,
    shown: Vec<Shown>,
}

#[derive(Clone)]
pub struct MockNotifications {
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
    inline_reply: bool,
}

struct Server {
    shared: Arc<Mutex<Shared>>,
    inline_reply: bool,
}

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        let mut caps = vec!["body".to_string(), "actions".into(), "persistence".into()];
        if self.inline_reply {
            caps.push("inline-reply".into());
        }
        caps
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let mut s = self.shared.lock().unwrap();
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            s.next += 1;
            s.next
        };
        let actions = actions
            .chunks(2)
            .map(|c| (c[0].clone(), c.get(1).cloned().unwrap_or_default()))
            .collect();
        s.shown.push(Shown {
            id,
            app_name,
            app_icon,
            replaces_id,
            summary,
            body,
            actions,
            hints,
            closed: false,
        });
        id
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        {
            let mut s = self.shared.lock().unwrap();
            for n in s.shown.iter_mut().filter(|n| n.id == id) {
                n.closed = true;
            }
        }
        Self::notification_closed(&emitter, id, 3).await?;
        Ok(())
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "omatel-mock".into(),
            "omatel".into(),
            "0".into(),
            "1.2".into(),
        )
    }

    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, key: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn notification_replied(
        emitter: &SignalEmitter<'_>,
        id: u32,
        text: &str,
    ) -> zbus::Result<()>;
}

impl MockNotifications {
    /// Serve on `conn` and take the org.freedesktop.Notifications name.
    pub async fn start(conn: Connection, inline_reply: bool) -> zbus::Result<MockNotifications> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        conn.object_server()
            .at(
                PATH,
                Server {
                    shared: shared.clone(),
                    inline_reply,
                },
            )
            .await?;
        conn.request_name("org.freedesktop.Notifications").await?;
        Ok(MockNotifications {
            conn,
            shared,
            inline_reply,
        })
    }

    pub fn inline_reply(&self) -> bool {
        self.inline_reply
    }

    /// Everything shown so far, oldest first (a replacement is a new entry
    /// with the same id).
    pub fn shown(&self) -> Vec<Shown> {
        self.shared.lock().unwrap().shown.clone()
    }

    /// The latest version of each notification that is still open.
    pub fn open(&self) -> Vec<Shown> {
        let s = self.shared.lock().unwrap();
        let mut latest: Vec<Shown> = Vec::new();
        for n in &s.shown {
            latest.retain(|m| m.id != n.id);
            latest.push(n.clone());
        }
        latest.into_iter().filter(|n| !n.closed).collect()
    }

    async fn emitter(&self) -> zbus::Result<SignalEmitter<'static>> {
        SignalEmitter::new(&self.conn, PATH).map(|e| e.into_owned())
    }

    /// The user presses an action button (or the body: "default").
    pub async fn invoke(&self, id: u32, key: &str) -> zbus::Result<()> {
        Server::action_invoked(&self.emitter().await?, id, key).await
    }

    /// The user types a reply in the notification and sends it.
    pub async fn reply(&self, id: u32, text: &str) -> zbus::Result<()> {
        Server::notification_replied(&self.emitter().await?, id, text).await
    }

    /// The user dismisses a notification.
    pub async fn dismiss(&self, id: u32) -> zbus::Result<()> {
        {
            let mut s = self.shared.lock().unwrap();
            for n in s.shown.iter_mut().filter(|n| n.id == id) {
                n.closed = true;
            }
        }
        Server::notification_closed(&self.emitter().await?, id, 2).await
    }
}
