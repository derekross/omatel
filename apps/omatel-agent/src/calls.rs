// SPDX-License-Identifier: GPL-3.0-or-later
//! Calls: follow each Call object from CallAdded to its end, ring and
//! notify for incoming ones, hang up on blocked numbers, place outgoing
//! calls (never emergency ones, yet), and write the call log.

use std::sync::Arc;

use futures_util::StreamExt;
use omatel_api::CallInfo;
use omatel_contacts::number;
use omatel_mm as mm;
use omatel_notify::{Notification, Urgency, DEFAULT_ACTION};
use omatel_store::{CallDirection, CallOutcome, CallRecord};
use zbus::zvariant::OwnedObjectPath;

use crate::util::now_ms;
use crate::{audio, iface, Agent, Target, Tracked};

/// The name Omarchy Mobile's shell call card owns while it runs
/// (omamobile-shell's omamobile-call-watch).
const CALL_CARD: &str = "org.omamobile.CallCard";

async fn proxy(agent: &Agent, path: &str) -> zbus::Result<mm::CallProxy<'static>> {
    mm::CallProxy::builder(&agent.system)
        .path(path.to_string())?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

/// Start following a call (idempotent).
pub async fn track(agent: Arc<Agent>, path: OwnedObjectPath) {
    let key = path.to_string();
    if agent.calls.lock().unwrap().contains_key(&key) {
        return;
    }
    let call = match proxy(&agent, &key).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %key, error = %e, "cannot follow the call");
            return;
        }
    };
    // Subscribe before reading the state, so no change slips between.
    let Ok(mut changes) = call.receive_call_state_changed().await else {
        return;
    };
    let (Ok(number), Ok(direction), Ok(state)) = (
        call.number().await,
        call.direction().await,
        call.state().await,
    ) else {
        tracing::warn!(path = %key, "call vanished before it could be read");
        return;
    };
    let state = mm::CallState::from_i32(state);
    if state == mm::CallState::Terminated {
        // A leftover from before we started: nothing to show or log.
        if let Some(v) = agent.modem_handle().and_then(|m| m.voice) {
            let _ = v.delete_call(&path.as_ref()).await;
        }
        return;
    }
    let incoming = direction == mm::call_direction::INCOMING;
    let info = CallInfo {
        id: key.clone(),
        number: number.clone(),
        name: agent.name_for(&number),
        direction: if incoming { "incoming" } else { "outgoing" }.into(),
        state: state.name().into(),
        started_ms: now_ms(),
        answered_ms: if state == mm::CallState::Active {
            now_ms()
        } else {
            0
        },
    };
    {
        let mut calls = agent.calls.lock().unwrap();
        if calls.contains_key(&key) {
            return;
        }
        calls.insert(
            key.clone(),
            Tracked {
                info,
                path: path.clone(),
                declined: false,
                blocked: false,
                silenced: false,
                post_dial: String::new(),
                feedback: None,
                notification: None,
                logged: false,
            },
        );
    }
    tracing::info!(path = %key, incoming, state = state.name(), "call");
    if incoming && matches!(state, mm::CallState::RingingIn | mm::CallState::Waiting) {
        ringing(&agent, &key).await;
    } else {
        emit_state(&agent, &key).await;
    }
    let watcher = agent.clone();
    tokio::spawn(async move {
        while let Some(sig) = changes.next().await {
            let Ok(a) = sig.args() else { continue };
            let new = mm::CallState::from_i32(a.new);
            changed(&watcher, &key, new, a.reason).await;
            if new == mm::CallState::Terminated {
                break;
            }
        }
    });
}

async fn emit_state(agent: &Agent, key: &str) {
    let info = agent.calls.lock().unwrap().get(key).map(|t| t.info.clone());
    if let Some(info) = info {
        iface::call_state_changed(agent, info).await;
    }
}

async fn ringing(agent: &Arc<Agent>, key: &str) {
    let Some(info) = agent.calls.lock().unwrap().get(key).map(|t| t.info.clone()) else {
        return;
    };
    let region = agent.region();
    let blocked = agent
        .blocked
        .lock()
        .unwrap()
        .is_blocked(&info.number, &region)
        .unwrap_or(false);
    if blocked {
        tracing::info!(
            path = key,
            "incoming call from a blocked number: hanging up"
        );
        if let Some(t) = agent.calls.lock().unwrap().get_mut(key) {
            t.blocked = true;
        }
        match proxy(agent, key).await {
            Ok(c) => {
                if let Err(e) = c.hangup().await {
                    tracing::warn!(error = %e, "hang-up of a blocked call failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "hang-up of a blocked call failed"),
        }
        return;
    }
    let settings = agent.settings();
    // Silenced: logged and shown quietly, no ring. Unknown callers when the
    // setting says so, and anyone but a starred contact during Omarchy's
    // Do Not Disturb (Android's default for calls).
    let dnd = agent.do_not_disturb();
    let silenced = (!settings.ring_unknown_callers && !agent.is_contact(&info.number))
        || (dnd && !agent.is_starred(&info.number));
    // Recorded before the signals go out, so a client that reads
    // SilencedCalls on IncomingCall (the shell's call card) sees it.
    if let Some(t) = agent.calls.lock().unwrap().get_mut(key) {
        t.silenced = silenced;
    }
    if silenced {
        iface::silenced_calls_changed(agent).await;
    }
    // Omarchy Mobile's shell shows ringing calls itself (its call card owns
    // this name): no toast and no answer page of ours on top of it. A
    // silenced call keeps its quiet notification, which the card doesn't
    // replace.
    let card = audio::has_owner(&agent.session, CALL_CARD).await;
    iface::incoming_call(agent, &info).await;
    iface::call_state_changed(agent, info.clone()).await;

    let who = if info.name.is_empty() {
        agent.display_number(&info.number)
    } else {
        info.name.clone()
    };
    let mut n = Notification::new(
        if silenced {
            "Silenced call"
        } else {
            "Incoming call"
        },
        who,
    )
    .action(DEFAULT_ACTION, "Open")
    .action("answer", "Answer")
    .action("decline", "Decline");
    n.icon = "call-start-symbolic".into();
    n.category = "call.incoming".into();
    n.urgency = if silenced {
        Urgency::Low
    } else {
        Urgency::Critical
    };
    n.timeout = 0;
    n.public_summary = Some("Incoming call".into());
    let note = if card && !silenced {
        None
    } else {
        match agent.notifier.show(&n).await {
            Ok(id) => {
                agent
                    .notes
                    .lock()
                    .unwrap()
                    .by_id
                    .insert(id, Target::IncomingCall(key.to_string()));
                Some(id)
            }
            Err(e) => {
                tracing::warn!(error = %e, "no incoming-call notification");
                None
            }
        }
    };
    let feedback = if silenced {
        None
    } else {
        audio::ring(&agent.session).await
    };
    let still_ringing = {
        let mut calls = agent.calls.lock().unwrap();
        match calls.get_mut(key) {
            Some(t) => {
                t.silenced = silenced;
                t.notification = note;
                t.feedback = feedback;
                t.info.is_ringing_in()
            }
            None => false,
        }
    };
    if !still_ringing {
        // It ended while we were setting up.
        stop_alerting(agent, feedback, note).await;
        return;
    }
    if settings.show_incoming_call_page && !silenced && !card {
        agent.cfg.launcher.launch(&["omaphone", "--incoming", key]);
    }
}

async fn stop_alerting(agent: &Agent, feedback: Option<u32>, note: Option<u32>) {
    if let Some(f) = feedback {
        audio::stop_ring(&agent.session, f).await;
    }
    if let Some(n) = note {
        let _ = agent.notifier.close(n).await;
        agent.notes.lock().unwrap().by_id.remove(&n);
    }
}

async fn changed(agent: &Arc<Agent>, key: &str, state: mm::CallState, reason: u32) {
    if state == mm::CallState::Terminated {
        ended(agent, key, reason).await;
        return;
    }
    let (feedback, note, post_dial, first_active) = {
        let mut calls = agent.calls.lock().unwrap();
        let Some(t) = calls.get_mut(key) else { return };
        t.info.state = state.name().into();
        let mut first_active = false;
        if state == mm::CallState::Active && t.info.answered_ms == 0 {
            t.info.answered_ms = now_ms();
            first_active = true;
        }
        if t.info.is_ringing_in() {
            (None, None, String::new(), false)
        } else {
            (
                t.feedback.take(),
                t.notification.take(),
                if first_active {
                    std::mem::take(&mut t.post_dial)
                } else {
                    String::new()
                },
                first_active,
            )
        }
    };
    tracing::info!(path = key, state = state.name(), "call state");
    stop_alerting(agent, feedback, note).await;
    if first_active {
        audio::call_mode(&agent.session, true).await;
    }
    emit_state(agent, key).await;
    if !post_dial.is_empty() {
        if let Ok(c) = proxy(agent, key).await {
            if let Err(e) = c.send_dtmf(&post_dial).await {
                tracing::warn!(error = %e, "post-dial tones failed");
            }
        }
    }
}

/// A call ended (or its modem went away): log it once and forget it.
pub async fn ended(agent: &Arc<Agent>, key: &str, reason: u32) {
    let Some(mut t) = agent.calls.lock().unwrap().remove(key) else {
        return;
    };
    if t.logged {
        return;
    }
    t.logged = true;
    t.info.state = "terminated".into();
    stop_alerting(agent, t.feedback.take(), t.notification.take()).await;
    iface::call_state_changed(agent, t.info.clone()).await;

    let now = now_ms();
    let answered = t.info.answered_ms > 0;
    let incoming = t.info.is_incoming();
    let outcome = if answered {
        CallOutcome::Answered
    } else if incoming && t.blocked {
        CallOutcome::Blocked
    } else if incoming && t.declined {
        CallOutcome::Rejected
    } else if incoming {
        CallOutcome::Missed
    } else if reason == mm::call_reason::ERROR {
        CallOutcome::Failed
    } else {
        CallOutcome::Unanswered
    };
    let region = agent.region();
    let record = CallRecord {
        id: 0,
        number: number::normalize(&t.info.number, &region),
        raw_number: t.info.number.clone(),
        direction: if incoming {
            CallDirection::In
        } else {
            CallDirection::Out
        },
        outcome,
        started_at: t.info.started_ms,
        answered_at: answered.then_some(t.info.answered_ms),
        ended_at: Some(now),
        duration: if answered {
            ((now - t.info.answered_ms) / 1000).max(0)
        } else {
            0
        },
        seen: outcome != CallOutcome::Missed,
    };
    let logged = agent.log.lock().unwrap().insert(&record);
    match logged {
        Ok(_) => tracing::info!(path = key, outcome = outcome.as_str(), "call logged"),
        Err(e) => tracing::warn!(error = %e, "call not logged"),
    }
    iface::call_log_changed(agent).await;
    if outcome == CallOutcome::Missed {
        missed_notification(agent, &t.info).await;
        iface::missed_calls_changed(agent).await;
    }
    if agent.calls.lock().unwrap().is_empty() {
        audio::call_mode(&agent.session, false).await;
        iface::reset_audio(agent).await;
    }
    if let Some(v) = agent.modem_handle().and_then(|m| m.voice) {
        let _ = v.delete_call(&t.path.as_ref()).await;
    }
}

async fn missed_notification(agent: &Arc<Agent>, info: &CallInfo) {
    let count = agent.log.lock().unwrap().unseen_missed().unwrap_or(1);
    let who = if info.name.is_empty() {
        agent.display_number(&info.number)
    } else {
        info.name.clone()
    };
    let (summary, body) = if count <= 1 {
        ("Missed call".to_string(), who)
    } else {
        (format!("{count} missed calls"), format!("Latest: {who}"))
    };
    let mut n = Notification::new(summary, body)
        .action(DEFAULT_ACTION, "Open")
        .action("call-back", "Call back")
        .action("message", "Message");
    n.icon = "call-missed-symbolic".into();
    n.category = "call.unanswered".into();
    n.resident = true;
    n.public_summary = Some("Missed call".into());
    let old = agent.notes.lock().unwrap().missed;
    n.replaces = old.unwrap_or(0);
    match agent.notifier.show(&n).await {
        Ok(id) => {
            let mut notes = agent.notes.lock().unwrap();
            notes.missed = Some(id);
            notes.by_id.insert(
                id,
                Target::MissedCalls {
                    number: info.number.clone(),
                },
            );
        }
        Err(e) => tracing::warn!(error = %e, "no missed-call notification"),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DialError {
    Invalid,
    Emergency,
    NoModem,
    Failed(String),
}

/// Place a call. Emergency numbers are refused: emergency calling isn't
/// supported until the telephony plan's §3.3 tests pass.
pub async fn dial(agent: &Arc<Agent>, raw: &str) -> Result<String, DialError> {
    let dial = number::sanitize_dial_string(raw);
    let (main, post) = number::dial_part(&dial);
    if main.chars().filter(|c| c.is_ascii_digit()).count() == 0 {
        return Err(DialError::Invalid);
    }
    if number::is_emergency(main, &agent.emergency_numbers()) {
        tracing::warn!(
            "an emergency number was dialled: refused (emergency calls aren't supported yet)"
        );
        return Err(DialError::Emergency);
    }
    let voice = agent
        .modem_handle()
        .and_then(|m| m.voice)
        .ok_or(DialError::NoModem)?;
    let path = voice
        .create_call(mm::call_properties(main))
        .await
        .map_err(|e| DialError::Failed(e.to_string()))?;
    track(agent.clone(), path.clone()).await;
    let key = path.to_string();
    let post: String = post
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '*' | '#'))
        .collect();
    if let Some(t) = agent.calls.lock().unwrap().get_mut(&key) {
        t.post_dial = post;
    }
    let call = proxy(agent, &key)
        .await
        .map_err(|e| DialError::Failed(e.to_string()))?;
    if let Err(e) = call.start().await {
        tracing::warn!(path = %key, error = %e, "the call could not be placed");
        ended(agent, &key, mm::call_reason::ERROR).await;
        return Err(DialError::Failed(e.to_string()));
    }
    tracing::info!(path = %key, "outgoing call placed");
    Ok(key)
}

fn known(agent: &Agent, id: &str) -> bool {
    agent.calls.lock().unwrap().contains_key(id)
}

pub async fn answer(agent: &Arc<Agent>, id: &str) -> zbus::Result<bool> {
    if !known(agent, id) {
        return Ok(false);
    }
    proxy(agent, id).await?.accept().await?;
    Ok(true)
}

pub async fn decline(agent: &Arc<Agent>, id: &str) -> zbus::Result<bool> {
    match agent.calls.lock().unwrap().get_mut(id) {
        Some(t) => t.declined = true,
        None => return Ok(false),
    }
    proxy(agent, id).await?.hangup().await?;
    Ok(true)
}

pub async fn hangup(agent: &Arc<Agent>, id: &str) -> zbus::Result<bool> {
    let ids: Vec<String> = if id.is_empty() {
        agent.calls.lock().unwrap().keys().cloned().collect()
    } else if known(agent, id) {
        vec![id.to_string()]
    } else {
        return Ok(false);
    };
    for i in ids {
        // Hanging up a ringing call declines it.
        if let Some(t) = agent.calls.lock().unwrap().get_mut(&i) {
            if t.info.is_ringing_in() {
                t.declined = true;
            }
        }
        proxy(agent, &i).await?.hangup().await?;
    }
    Ok(true)
}

pub async fn send_dtmf(agent: &Arc<Agent>, id: &str, keys: &str) -> zbus::Result<bool> {
    if !known(agent, id) {
        return Ok(false);
    }
    proxy(agent, id).await?.send_dtmf(keys).await?;
    Ok(true)
}

pub fn list(agent: &Agent) -> Vec<CallInfo> {
    let mut v: Vec<CallInfo> = agent
        .calls
        .lock()
        .unwrap()
        .values()
        .map(|t| t.info.clone())
        .collect();
    v.sort_by_key(|c| c.started_ms);
    v
}
