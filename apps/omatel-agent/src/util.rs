// SPDX-License-Identifier: GPL-3.0-or-later
//! Small helpers: opening the apps, the lock state, modem state names.

use std::sync::{Arc, Mutex};

use omatel_mm::{failed_reason, modem_state};
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

/// How the agent opens Phone and Messages.
#[derive(Debug, Clone)]
pub enum Launcher {
    /// Start the app (it forwards to a running instance).
    Spawn,
    /// Tests: record the command lines instead.
    Record(Arc<Mutex<Vec<Vec<String>>>>),
    /// Never open anything.
    Off,
}

impl Launcher {
    pub fn launch(&self, args: &[&str]) {
        match self {
            Launcher::Spawn => {
                let Some((program, rest)) = args.split_first() else {
                    return;
                };
                match std::process::Command::new(program)
                    .args(rest)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(mut child) => {
                        // Reap it without blocking the agent.
                        std::thread::spawn(move || {
                            let _ = child.wait();
                        });
                    }
                    Err(e) => tracing::warn!(program, error = %e, "cannot open the app"),
                }
            }
            Launcher::Record(log) => log
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect()),
            Launcher::Off => {}
        }
    }
}

/// The modem's state as `org.omamobile.Phone1.ModemState` names it.
pub fn modem_state_name(state: i32, reason: u32, emergency_only: bool) -> &'static str {
    if emergency_only && state >= modem_state::ENABLED {
        return "emergency-only";
    }
    match state {
        modem_state::FAILED if reason == failed_reason::SIM_MISSING => "no-sim",
        modem_state::FAILED => "failed",
        modem_state::LOCKED => "locked",
        modem_state::DISABLED | modem_state::DISABLING => "disabled",
        modem_state::REGISTERED.. => "ready",
        modem_state::ENABLING | modem_state::ENABLED | modem_state::SEARCHING => "searching",
        _ => "starting",
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1/user/self"
)]
trait LoginUser {
    #[zbus(property)]
    fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait LoginSession {
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/org/freedesktop/ScreenSaver"
)]
trait ScreenSaver {
    fn get_active(&self) -> zbus::Result<bool>;
}

/// Whether the phone is locked: logind's LockedHint on the user's display
/// session, or an active org.freedesktop.ScreenSaver. Unknown counts as
/// unlocked; the notification still carries the lock-screen summary hint,
/// so a lock screen that reads it stays private either way.
pub async fn probe_locked(system: &Connection, session: &Connection) -> bool {
    let logind = async {
        let user = LoginUserProxy::new(system).await.ok()?;
        let (_, path) = user.display().await.ok()?;
        if path.as_str() == "/" {
            return None;
        }
        let s = LoginSessionProxy::builder(system)
            .path(path)
            .ok()?
            .build()
            .await
            .ok()?;
        s.locked_hint().await.ok()
    };
    if logind.await == Some(true) {
        return true;
    }
    let saver = async {
        let p = ScreenSaverProxy::new(session).await.ok()?;
        tokio::time::timeout(std::time::Duration::from_millis(300), p.get_active())
            .await
            .ok()?
            .ok()
    };
    saver.await == Some(true)
}

/// Milliseconds since the epoch.
pub fn now_ms() -> i64 {
    omatel_store::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names() {
        assert_eq!(modem_state_name(-1, 2, false), "no-sim");
        assert_eq!(modem_state_name(-1, 1, false), "failed");
        assert_eq!(modem_state_name(2, 0, false), "locked");
        assert_eq!(modem_state_name(3, 0, false), "disabled");
        assert_eq!(modem_state_name(7, 0, false), "searching");
        assert_eq!(modem_state_name(8, 0, false), "ready");
        assert_eq!(modem_state_name(11, 0, false), "ready");
        assert_eq!(modem_state_name(8, 0, true), "emergency-only");
        assert_eq!(modem_state_name(0, 0, false), "starting");
    }

    #[test]
    fn record_launches() {
        let log = Arc::new(Mutex::new(Vec::new()));
        Launcher::Record(log.clone()).launch(&["omaphone", "--incoming", "/x"]);
        Launcher::Off.launch(&["omaphone"]);
        assert_eq!(
            *log.lock().unwrap(),
            vec![vec![
                "omaphone".to_string(),
                "--incoming".into(),
                "/x".into()
            ]]
        );
    }
}
