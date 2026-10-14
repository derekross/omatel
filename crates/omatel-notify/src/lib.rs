// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop notifications over `org.freedesktop.Notifications` with
//! buttons and an **inline reply**: the `inline-reply` action and the
//! `NotificationReplied(u id, s text)` signal, the freedesktop extension
//! that KDE and Quickshell implement (GTK's GNotification has no reply).
//! Omarchy's notification server advertises it once our shell patch lands
//! (telephony plan §7.2, T5); until then [`Notifier::supports_inline_reply`]
//! is false and the agent offers a "Reply" button that opens Messages.
//!
//! Notifications carry `desktop-entry` and `category` hints, and, for the
//! lock screen, `x-omamobile-private` with `x-omamobile-public-summary`:
//! what a lock screen should show instead of the sender and text.

use std::collections::HashMap;

use futures_util::stream::{self, Stream, StreamExt};
use zbus::zvariant::Value;
use zbus::Connection;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
    fn close_notification(&self, id: u32) -> zbus::Result<()>;
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn notification_replied(&self, id: u32, text: String) -> zbus::Result<()>;
}

/// The action key of an inline reply.
pub const INLINE_REPLY: &str = "inline-reply";
/// The action key a server sends when the notification itself is clicked.
pub const DEFAULT_ACTION: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    Critical = 2,
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub summary: String,
    pub body: String,
    pub icon: String,
    /// Buttons: (key, label). Put [`DEFAULT_ACTION`] first for a click on
    /// the notification itself.
    pub actions: Vec<(String, String)>,
    /// Offer a reply field with this placeholder text.
    pub reply_placeholder: Option<String>,
    /// Replace this notification (0: a new one).
    pub replaces: u32,
    pub urgency: Urgency,
    /// `im.received`, `call.incoming`, `call.unanswered`, ...
    pub category: String,
    /// Stay after an action is invoked (a call's Answer/Decline don't).
    pub resident: bool,
    /// What a lock screen shows instead of summary and body, when set.
    pub public_summary: Option<String>,
    /// ms; -1 the server's default, 0 never expires.
    pub timeout: i32,
}

impl Notification {
    pub fn new(summary: impl Into<String>, body: impl Into<String>) -> Notification {
        Notification {
            summary: summary.into(),
            body: body.into(),
            icon: String::new(),
            actions: Vec::new(),
            reply_placeholder: None,
            replaces: 0,
            urgency: Urgency::Normal,
            category: String::new(),
            resident: false,
            public_summary: None,
            timeout: -1,
        }
    }

    pub fn action(mut self, key: &str, label: &str) -> Notification {
        self.actions.push((key.into(), label.into()));
        self
    }

    /// The flat action list D-Bus wants: key, label, key, label...
    pub fn action_list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (k, l) in &self.actions {
            out.push(k.clone());
            out.push(l.clone());
        }
        if let Some(p) = &self.reply_placeholder {
            out.push(INLINE_REPLY.into());
            out.push(p.clone());
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Action { id: u32, key: String },
    Replied { id: u32, text: String },
    Closed { id: u32, reason: u32 },
}

#[derive(Clone)]
pub struct Notifier {
    proxy: NotificationsProxy<'static>,
    app_name: String,
    desktop_entry: String,
}

impl Notifier {
    pub async fn new(
        conn: &Connection,
        app_name: &str,
        desktop_entry: &str,
    ) -> zbus::Result<Notifier> {
        Ok(Notifier {
            proxy: NotificationsProxy::new(conn).await?,
            app_name: app_name.into(),
            desktop_entry: desktop_entry.into(),
        })
    }

    pub async fn capabilities(&self) -> Vec<String> {
        self.proxy.get_capabilities().await.unwrap_or_default()
    }

    pub async fn supports_inline_reply(&self) -> bool {
        self.capabilities().await.iter().any(|c| c == INLINE_REPLY)
    }

    /// Show (or replace) a notification; returns its id.
    pub async fn show(&self, n: &Notification) -> zbus::Result<u32> {
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("desktop-entry", Value::from(self.desktop_entry.as_str()));
        hints.insert("urgency", Value::U8(n.urgency as u8));
        hints.insert("sender-pid", Value::I64(std::process::id() as i64));
        if !n.category.is_empty() {
            hints.insert("category", Value::from(n.category.as_str()));
        }
        if n.resident {
            hints.insert("resident", Value::from(true));
        }
        if let Some(p) = &n.reply_placeholder {
            hints.insert("x-kde-reply-placeholder-text", Value::from(p.as_str()));
        }
        if let Some(p) = &n.public_summary {
            hints.insert("x-omamobile-private", Value::from(true));
            hints.insert("x-omamobile-public-summary", Value::from(p.as_str()));
        }
        let actions = n.action_list();
        let actions: Vec<&str> = actions.iter().map(String::as_str).collect();
        self.proxy
            .notify(
                &self.app_name,
                n.replaces,
                &n.icon,
                &n.summary,
                &n.body,
                &actions,
                hints,
                n.timeout,
            )
            .await
    }

    pub async fn close(&self, id: u32) -> zbus::Result<()> {
        self.proxy.close_notification(id).await
    }

    /// Actions, replies and closes, for every notification on the bus
    /// (filter by the ids you showed).
    pub async fn events(&self) -> zbus::Result<impl Stream<Item = Event> + Unpin + use<>> {
        let actions = self
            .proxy
            .receive_action_invoked()
            .await?
            .filter_map(|s| async move {
                let a = s.args().ok()?;
                Some(Event::Action {
                    id: a.id,
                    key: a.action_key,
                })
            });
        let replies = self
            .proxy
            .receive_notification_replied()
            .await?
            .filter_map(|s| async move {
                let a = s.args().ok()?;
                Some(Event::Replied {
                    id: a.id,
                    text: a.text,
                })
            });
        let closed = self
            .proxy
            .receive_notification_closed()
            .await?
            .filter_map(|s| async move {
                let a = s.args().ok()?;
                Some(Event::Closed {
                    id: a.id,
                    reason: a.reason,
                })
            });
        Ok(stream::select(
            stream::select(actions.boxed(), replies.boxed()),
            closed.boxed(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_list_with_reply() {
        let mut n = Notification::new("s", "b")
            .action(DEFAULT_ACTION, "Open")
            .action("mark-read", "Mark as read");
        n.reply_placeholder = Some("Reply".into());
        assert_eq!(
            n.action_list(),
            vec![
                "default",
                "Open",
                "mark-read",
                "Mark as read",
                "inline-reply",
                "Reply"
            ]
        );
    }
}
