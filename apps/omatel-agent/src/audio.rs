// SPDX-License-Identifier: GPL-3.0-or-later
//! Ringing and call audio, through the services phone stacks share:
//!
//! - **feedbackd** (`org.sigxcpu.Feedback`) plays the `phone-incoming-call`
//!   event (ringtone and vibration) as GNOME Calls does. Without it the
//!   phone doesn't ring; the notification and the answer page still show.
//! - **callaudiod** (`org.mobian_project.CallAudio`) switches the audio
//!   profile for a call, the speaker and the microphone mute. It belongs
//!   to the call-audio phase (T4 of the telephony plan: earpiece mode, the
//!   UCM verb, callaudiod). Until it runs, mute and the route are recorded
//!   and reported, but change nothing (`AudioRouting` is false).

use std::collections::HashMap;

use zbus::zvariant::Value;
use zbus::Connection;

#[zbus::proxy(
    interface = "org.sigxcpu.Feedback",
    default_service = "org.sigxcpu.Feedback",
    default_path = "/org/sigxcpu/Feedback"
)]
trait Feedback {
    fn trigger_feedback(
        &self,
        app_id: &str,
        event: &str,
        hints: HashMap<&str, Value<'_>>,
        timeout: i32,
    ) -> zbus::Result<u32>;
    fn end_feedback(&self, id: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.mobian_project.CallAudio",
    default_service = "org.mobian_project.CallAudio",
    default_path = "/org/mobian_project/CallAudio"
)]
trait CallAudio {
    fn select_mode(&self, mode: u32) -> zbus::Result<bool>;
    fn enable_speaker(&self, enable: bool) -> zbus::Result<bool>;
    fn mute_mic(&self, mute: bool) -> zbus::Result<bool>;
}

pub(crate) async fn has_owner(conn: &Connection, name: &str) -> bool {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(name) else {
        return false;
    };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

/// Start ringing; returns feedbackd's id to stop it with.
pub async fn ring(session: &Connection) -> Option<u32> {
    if !has_owner(session, "org.sigxcpu.Feedback").await {
        tracing::info!("no feedbackd: the incoming call doesn't ring");
        return None;
    }
    let p = FeedbackProxy::new(session).await.ok()?;
    match p
        .trigger_feedback(
            "org.omamobile.Phone",
            "phone-incoming-call",
            HashMap::new(),
            -1,
        )
        .await
    {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "feedbackd refused the ring");
            None
        }
    }
}

pub async fn stop_ring(session: &Connection, id: u32) {
    if let Ok(p) = FeedbackProxy::new(session).await {
        let _ = p.end_feedback(id).await;
    }
}

/// Whether call audio can be routed (callaudiod is running).
pub async fn routing_available(session: &Connection) -> bool {
    has_owner(session, "org.mobian_project.CallAudio").await
}

/// Call mode on (1) or back to the default profile (0).
pub async fn call_mode(session: &Connection, on: bool) {
    if !routing_available(session).await {
        return;
    }
    if let Ok(p) = CallAudioProxy::new(session).await {
        if let Err(e) = p.select_mode(u32::from(on)).await {
            tracing::warn!(error = %e, "callaudiod: mode");
        }
    }
}

pub async fn set_mute(session: &Connection, mute: bool) -> bool {
    if !routing_available(session).await {
        return false;
    }
    match CallAudioProxy::new(session).await {
        Ok(p) => p.mute_mic(mute).await.unwrap_or(false),
        Err(_) => false,
    }
}

pub async fn set_speaker(session: &Connection, on: bool) -> bool {
    if !routing_available(session).await {
        return false;
    }
    match CallAudioProxy::new(session).await {
        Ok(p) => p.enable_speaker(on).await.unwrap_or(false),
        Err(_) => false,
    }
}
