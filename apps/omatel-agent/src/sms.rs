// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages: store what arrives (on disk first, then deleted from the
//! modem), notify per conversation, send, and follow delivery reports.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use omatel_contacts::number;
use omatel_mm as mm;
use omatel_notify::{Notification, DEFAULT_ACTION};
use omatel_store::{Direction, MessageState, NewIncoming};
use zbus::zvariant::OwnedObjectPath;

use crate::{iface, Agent, Target};

/// How long a sent message's delivery report is awaited.
const DELIVERY_WAIT: Duration = Duration::from_secs(48 * 3600);

async fn sms_proxy(agent: &Agent, path: &OwnedObjectPath) -> zbus::Result<mm::SmsProxy<'static>> {
    mm::SmsProxy::builder(&agent.system)
        .path(path.clone())?
        .build()
        .await
}

/// A message the modem holds: store it if it is a received text.
pub async fn handle_incoming(agent: Arc<Agent>, path: OwnedObjectPath) {
    let key = path.to_string();
    if !agent.handling.lock().unwrap().insert(key.clone()) {
        return;
    }
    let mut retried = false;
    loop {
        match store_one(&agent, &path).await {
            Ok(()) => break,
            Err(e) if !retried => {
                tracing::warn!(path = %key, error = %e, "SMS not stored yet; trying again in 30 s");
                retried = true;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Err(e) => {
                tracing::error!(path = %key, error = %e, "SMS not stored; it stays on the modem");
                break;
            }
        }
    }
    agent.handling.lock().unwrap().remove(&key);
}

async fn store_one(agent: &Arc<Agent>, path: &OwnedObjectPath) -> anyhow::Result<()> {
    let mut info = mm::SmsInfo::read(&agent.system, path.clone()).await?;
    if info.state == mm::sms_state::RECEIVING {
        // A multipart message still arriving: wait for the rest.
        let p = sms_proxy(agent, path).await?;
        let mut changes = p.receive_state_changed().await;
        let wait = async {
            while let Some(c) = changes.next().await {
                if c.get().await.ok() == Some(mm::sms_state::RECEIVED) {
                    break;
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(120), wait).await;
        info = mm::SmsInfo::read(&agent.system, path.clone()).await?;
    }
    if info.is_binary() {
        tracing::info!(path = %path, "binary SMS (WAP push): left for the MMS service");
        return Ok(());
    }
    if !info.is_received_text() {
        return Ok(());
    }
    let region = agent.region();
    let address = number::normalize(&info.number, &region);
    let stored = agent
        .messages
        .lock()
        .unwrap()
        .insert_incoming(&NewIncoming {
            address: &address,
            raw_number: &info.number,
            body: &info.text,
            sent_at: mm::parse_timestamp(&info.timestamp),
            fallback_key: path.as_str(),
            received_at: None,
        })?;
    // On disk now (synchronous=FULL): the modem's copy can go.
    if let Some(m) = agent.modem_handle().and_then(|m| m.messaging) {
        if let Err(e) = m.delete(&path.as_ref()).await {
            tracing::warn!(path = %path, error = %e, "stored SMS not deleted from the modem");
        }
    }
    match stored {
        Some((conversation, id)) => {
            tracing::info!(path = %path, conversation, message = id, "SMS stored");
            iface::messages_changed(agent, conversation).await;
            notify(agent, conversation).await;
        }
        None => tracing::info!(path = %path, "SMS already stored (a duplicate)"),
    }
    Ok(())
}

/// Show, update or withdraw a conversation's notification.
pub async fn notify(agent: &Arc<Agent>, conversation: i64) {
    let settings = agent.settings();
    let (conv, unread) = {
        let store = agent.messages.lock().unwrap();
        let Ok(Some(c)) = store.conversation(conversation) else {
            return;
        };
        let recent = store
            .messages(conversation, 20, None)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.direction == Direction::In && !m.read)
            .collect::<Vec<_>>();
        (c, recent)
    };
    let existing = agent
        .notes
        .lock()
        .unwrap()
        .by_conversation
        .get(&conversation)
        .copied();
    if unread.is_empty() {
        if let Some(id) = existing {
            close(agent, id).await;
        }
        return;
    }
    let raw = conv
        .numbers
        .first()
        .cloned()
        .unwrap_or(conv.address.clone());
    let blocked = agent
        .blocked
        .lock()
        .unwrap()
        .is_blocked(&raw, &agent.region())
        .unwrap_or(false);
    if !settings.message_notifications || conv.muted || blocked {
        return;
    }
    let name = agent.name_for(&raw);
    let who = if name.is_empty() {
        agent.display_number(&raw)
    } else {
        name
    };
    let private = !settings.lockscreen_details && agent.is_locked().await;
    let mut n = if private {
        Notification::new("New message", "")
    } else {
        let summary = if unread.len() > 1 {
            format!("{who} ({})", unread.len())
        } else {
            who
        };
        let lines: Vec<&str> = unread
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|m| m.body.as_str())
            .collect();
        Notification::new(summary, lines.join("\n"))
    };
    n = n.action(DEFAULT_ACTION, "Open");
    if !private {
        if agent.notifier.supports_inline_reply().await {
            n.reply_placeholder = Some("Reply".into());
        } else {
            n = n.action("reply", "Reply");
        }
    }
    n = n.action("mark-read", "Mark as read");
    n.icon = "mail-unread-symbolic".into();
    n.category = "im.received".into();
    n.resident = true;
    if !settings.lockscreen_details {
        n.public_summary = Some("New message".into());
    }
    n.replaces = existing.unwrap_or(0);
    match agent.notifier.show(&n).await {
        Ok(id) => {
            let mut notes = agent.notes.lock().unwrap();
            notes.by_conversation.insert(conversation, id);
            notes.by_id.insert(id, Target::Conversation(conversation));
        }
        Err(e) => tracing::warn!(error = %e, "no message notification"),
    }
}

pub async fn close(agent: &Agent, id: u32) {
    let _ = agent.notifier.close(id).await;
    let mut notes = agent.notes.lock().unwrap();
    if let Some(Target::Conversation(c)) = notes.by_id.remove(&id) {
        notes.by_conversation.remove(&c);
    }
}

/// Mark a conversation read and withdraw its notification.
pub async fn mark_read(agent: &Arc<Agent>, conversation: i64) {
    let changed = agent
        .messages
        .lock()
        .unwrap()
        .mark_read(conversation)
        .unwrap_or(0);
    let id = agent
        .notes
        .lock()
        .unwrap()
        .by_conversation
        .get(&conversation)
        .copied();
    if let Some(id) = id {
        close(agent, id).await;
    }
    if changed > 0 {
        iface::messages_changed(agent, conversation).await;
    }
}

/// Queue a text to a conversation; it is sent in the background.
pub async fn send(agent: &Arc<Agent>, conversation: i64, text: &str) -> omatel_store::Result<i64> {
    let id = agent
        .messages
        .lock()
        .unwrap()
        .insert_outgoing(conversation, text)?;
    iface::messages_changed(agent, conversation).await;
    tokio::spawn(deliver(agent.clone(), conversation, id));
    Ok(id)
}

/// Send a failed message again, in its place in the thread.
pub async fn retry(agent: &Arc<Agent>, id: i64) -> bool {
    let msg = agent.messages.lock().unwrap().message(id).ok().flatten();
    let Some(m) = msg else { return false };
    if m.direction != Direction::Out || m.state != MessageState::Failed {
        return false;
    }
    let _ = agent
        .messages
        .lock()
        .unwrap()
        .set_state(id, MessageState::Sending, None);
    iface::messages_changed(agent, m.conversation_id).await;
    tokio::spawn(deliver(agent.clone(), m.conversation_id, id));
    true
}

async fn fail(agent: &Arc<Agent>, conversation: i64, id: i64, why: &str) {
    tracing::info!(message = id, why, "SMS not sent");
    let _ = agent
        .messages
        .lock()
        .unwrap()
        .set_state(id, MessageState::Failed, Some(why));
    iface::messages_changed(agent, conversation).await;
}

async fn deliver(agent: Arc<Agent>, conversation: i64, id: i64) {
    let (numbers, body) = {
        let store = agent.messages.lock().unwrap();
        let c = store.conversation(conversation).ok().flatten();
        let m = store.message(id).ok().flatten();
        match (c, m) {
            (Some(c), Some(m)) => (c.numbers, m.body),
            _ => return,
        }
    };
    let Some(to) = numbers.first().cloned() else {
        return fail(&agent, conversation, id, "no recipient").await;
    };
    let Some(messaging) = agent.modem_handle().and_then(|m| m.messaging) else {
        return fail(&agent, conversation, id, "no modem").await;
    };
    let report = agent.settings().delivery_reports;
    let path = match messaging
        .create(mm::sms_properties(&to, &body, report))
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "the modem refused the message");
            return fail(&agent, conversation, id, "the modem refused it").await;
        }
    };
    let _ = agent
        .messages
        .lock()
        .unwrap()
        .set_mm_path(id, Some(path.as_str()));
    let sms = match sms_proxy(&agent, &path).await {
        Ok(p) => p,
        Err(_) => return fail(&agent, conversation, id, "the modem lost it").await,
    };
    let mut delivery = sms.receive_delivery_state_changed().await;
    if let Err(e) = sms.send().await {
        tracing::warn!(path = %path, error = %e, "SMS send failed");
        let _ = messaging.delete(&path.as_ref()).await;
        let _ = agent.messages.lock().unwrap().set_mm_path(id, None);
        return fail(&agent, conversation, id, "not sent").await;
    }
    tracing::info!(path = %path, message = id, "SMS sent");
    let _ = agent
        .messages
        .lock()
        .unwrap()
        .set_state(id, MessageState::Sent, None);
    iface::messages_changed(&agent, conversation).await;
    if report {
        let wait = async {
            while let Some(c) = delivery.next().await {
                match c.get().await.map(mm::Delivery::from_u32) {
                    Ok(mm::Delivery::Delivered) => return Some(MessageState::Delivered),
                    Ok(mm::Delivery::Failed) => return Some(MessageState::Failed),
                    _ => {}
                }
            }
            None
        };
        match tokio::time::timeout(DELIVERY_WAIT, wait).await {
            Ok(Some(state)) => {
                tracing::info!(path = %path, message = id, state = state.as_str(), "delivery report");
                let why = (state == MessageState::Failed).then_some("not delivered");
                let _ = agent.messages.lock().unwrap().set_state(id, state, why);
                iface::messages_changed(&agent, conversation).await;
            }
            _ => tracing::info!(path = %path, "no delivery report"),
        }
    }
    let _ = messaging.delete(&path.as_ref()).await;
    let _ = agent.messages.lock().unwrap().set_mm_path(id, None);
}
