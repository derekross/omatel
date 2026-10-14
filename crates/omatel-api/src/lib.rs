// SPDX-License-Identifier: GPL-3.0-or-later
//! The contract between omatel's agent and everything else on the session
//! bus: the apps, the shell's lock-screen call card (T5) and the
//! emergency dialpad. The agent (`omatel-agent`) serves both names; the
//! shell never talks to the modem directly.
//!
//! **`org.omamobile.Phone1`** at `/org/omamobile/Phone1`
//!
//! | Member | Kind | |
//! | --- | --- | --- |
//! | `Dial(s number) -> s id` | method | refuses emergency numbers until §3.3 of the plan passes (`...Error.EmergencyUnsupported`) |
//! | `Answer(s id)`, `Decline(s id)`, `Hangup(s id)` | methods | `Hangup("")` ends every call |
//! | `SendDtmf(s id, s keys)` | method | `0-9 * #` |
//! | `SetMute(b)`, `SetRoute(s)` | methods | through callaudiod when it runs; recorded only until call audio (T4) |
//! | `ListCalls() -> a(sssssxx)` | method | [`CallInfo`] for every call the modem has |
//! | `MarkMissedSeen()` | method | clears the missed-call badge and notification |
//! | `Muted b`, `Route s`, `AudioRouting b`, `ModemState s`, `EmergencyNumbers as`, `MissedCalls u` | properties | |
//! | `IncomingCall(s id, s number, s name)` | signal | a call starts ringing (not for blocked numbers) |
//! | `CallStateChanged((sssssxx))` | signal | [`CallInfo`] after any change |
//! | `CallLogChanged()` | signal | the call log gained or lost entries |
//!
//! **`org.omamobile.Messages1`** at `/org/omamobile/Messages1`
//!
//! | Member | Kind | |
//! | --- | --- | --- |
//! | `Send(s number, s text) -> (x conversation, x message)` | method | the call card's canned replies use it |
//! | `SendToConversation(x conversation, s text) -> x message` | method | |
//! | `Retry(x message)` | method | a failed outgoing message, sent again as new |
//! | `MarkRead(x conversation)` | method | also withdraws its notification |
//! | `OpenConversation(s number)` | method | opens Messages on that thread |
//! | `Ready b`, `ModemState s` | properties | `Ready`: the modem can send |
//! | `MessagesChanged(x conversation)` | signal | a message arrived or changed state |
//!
//! Numbers and texts travel on the user's own session bus only; the agent
//! never logs them.

pub mod settings;

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

pub const PHONE_NAME: &str = "org.omamobile.Phone1";
pub const PHONE_PATH: &str = "/org/omamobile/Phone1";
pub const MESSAGES_NAME: &str = "org.omamobile.Messages1";
pub const MESSAGES_PATH: &str = "/org/omamobile/Messages1";

/// D-Bus error names the agent returns.
pub mod errors {
    pub const EMERGENCY_UNSUPPORTED: &str = "org.omamobile.Phone1.Error.EmergencyUnsupported";
    pub const NO_MODEM: &str = "org.omamobile.Phone1.Error.NoModem";
    pub const INVALID_NUMBER: &str = "org.omamobile.Phone1.Error.InvalidNumber";
    pub const NO_SUCH_CALL: &str = "org.omamobile.Phone1.Error.NoSuchCall";
    pub const FAILED: &str = "org.omamobile.Phone1.Error.Failed";
}

/// The text shown wherever an emergency call would be tried, until the
/// test protocol in the telephony plan (§3.3) has passed on a carrier.
pub const EMERGENCY_WARNING: &str =
    "Emergency calls (911, 112) don't work on this phone yet. Keep another phone for emergencies.";

/// One call, as the agent reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CallInfo {
    /// The modem's object path for the call; pass it back to Answer etc.
    pub id: String,
    pub number: String,
    /// The contact's name, or empty.
    pub name: String,
    /// "incoming" or "outgoing".
    pub direction: String,
    /// "dialing", "ringing-out", "ringing-in", "active", "held",
    /// "waiting", "terminated" or "unknown".
    pub state: String,
    /// When the call appeared, ms since the epoch.
    pub started_ms: i64,
    /// When it was answered, ms since the epoch; 0 if not (yet).
    pub answered_ms: i64,
}

impl CallInfo {
    pub fn is_incoming(&self) -> bool {
        self.direction == "incoming"
    }

    pub fn is_ringing_in(&self) -> bool {
        self.state == "ringing-in" || self.state == "waiting"
    }

    pub fn is_ended(&self) -> bool {
        self.state == "terminated"
    }

    pub fn display(&self) -> &str {
        if self.name.is_empty() {
            &self.number
        } else {
            &self.name
        }
    }
}

/// Audio routes for `SetRoute`.
pub const ROUTES: &[&str] = &["earpiece", "speaker", "bluetooth"];

#[zbus::proxy(
    interface = "org.omamobile.Phone1",
    default_service = "org.omamobile.Phone1",
    default_path = "/org/omamobile/Phone1"
)]
pub trait Phone1 {
    fn dial(&self, number: &str) -> zbus::Result<String>;
    fn answer(&self, id: &str) -> zbus::Result<()>;
    fn decline(&self, id: &str) -> zbus::Result<()>;
    fn hangup(&self, id: &str) -> zbus::Result<()>;
    fn send_dtmf(&self, id: &str, keys: &str) -> zbus::Result<()>;
    fn set_mute(&self, mute: bool) -> zbus::Result<()>;
    fn set_route(&self, route: &str) -> zbus::Result<()>;
    fn list_calls(&self) -> zbus::Result<Vec<CallInfo>>;
    fn mark_missed_seen(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn muted(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn route(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn audio_routing(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn modem_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn emergency_numbers(&self) -> zbus::Result<Vec<String>>;
    #[zbus(property)]
    fn missed_calls(&self) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn incoming_call(&self, id: String, number: String, name: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn call_state_changed(&self, call: CallInfo) -> zbus::Result<()>;
    #[zbus(signal)]
    fn call_log_changed(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.omamobile.Messages1",
    default_service = "org.omamobile.Messages1",
    default_path = "/org/omamobile/Messages1"
)]
pub trait Messages1 {
    fn send(&self, number: &str, text: &str) -> zbus::Result<(i64, i64)>;
    fn send_to_conversation(&self, conversation: i64, text: &str) -> zbus::Result<i64>;
    fn retry(&self, message: i64) -> zbus::Result<()>;
    fn mark_read(&self, conversation: i64) -> zbus::Result<()>;
    fn open_conversation(&self, number: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn ready(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn modem_state(&self) -> zbus::Result<String>;

    #[zbus(signal)]
    fn messages_changed(&self, conversation: i64) -> zbus::Result<()>;
}

/// The modem states the agent reports in `ModemState`, for status lines.
pub fn modem_state_label(state: &str) -> &'static str {
    match state {
        "ready" => "Ready",
        "no-modem" => "No modem",
        "no-sim" => "No SIM",
        "locked" => "SIM locked",
        "searching" => "Searching for network",
        "disabled" => "Mobile network off",
        "emergency-only" => "Emergency calls only",
        "failed" => "Modem failed",
        _ => "Starting",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Type;

    #[test]
    fn call_info_signature() {
        assert_eq!(CallInfo::SIGNATURE.to_string(), "(sssssxx)");
    }

    #[test]
    fn call_info_helpers() {
        let c = CallInfo {
            id: "/c/1".into(),
            number: "+12025550100".into(),
            name: String::new(),
            direction: "incoming".into(),
            state: "ringing-in".into(),
            started_ms: 0,
            answered_ms: 0,
        };
        assert!(c.is_incoming() && c.is_ringing_in() && !c.is_ended());
        assert_eq!(c.display(), "+12025550100");
        assert_eq!(modem_state_label("no-sim"), "No SIM");
    }
}
