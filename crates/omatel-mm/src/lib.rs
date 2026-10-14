// SPDX-License-Identifier: GPL-3.0-or-later
//! ModemManager 1.24's D-Bus API, the part phone software uses, as zbus 5
//! proxies, with the enum values and a few typed helpers. It works the
//! same against the real ModemManager and against Omarchy Mobile's bridge
//! (`omamobile-mmd`), which serves the same names on the system bus.
//!
//! Member names and signatures follow MM 1.24's `introspection/*.xml`;
//! the values follow `include/ModemManager-enums.h`. Only the members we
//! use are declared (zbus proxies may leave the rest out).

pub mod proxies;

use std::collections::HashMap;

use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

pub use proxies::*;

pub const SERVICE: &str = "org.freedesktop.ModemManager1";
pub const PATH: &str = "/org/freedesktop/ModemManager1";
pub const IFACE_MODEM: &str = "org.freedesktop.ModemManager1.Modem";
pub const IFACE_VOICE: &str = "org.freedesktop.ModemManager1.Modem.Voice";
pub const IFACE_MESSAGING: &str = "org.freedesktop.ModemManager1.Modem.Messaging";

/// MMModemState.
pub mod modem_state {
    pub const FAILED: i32 = -1;
    pub const UNKNOWN: i32 = 0;
    pub const INITIALIZING: i32 = 1;
    pub const LOCKED: i32 = 2;
    pub const DISABLED: i32 = 3;
    pub const DISABLING: i32 = 4;
    pub const ENABLING: i32 = 5;
    pub const ENABLED: i32 = 6;
    pub const SEARCHING: i32 = 7;
    pub const REGISTERED: i32 = 8;
    pub const DISCONNECTING: i32 = 9;
    pub const CONNECTING: i32 = 10;
    pub const CONNECTED: i32 = 11;
}

/// MMModemStateFailedReason.
pub mod failed_reason {
    pub const SIM_MISSING: u32 = 2;
    pub const SIM_ERROR: u32 = 3;
}

/// MMCallState.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallState {
    Unknown,
    Dialing,
    RingingOut,
    RingingIn,
    Active,
    Held,
    Waiting,
    Terminated,
}

impl CallState {
    pub fn from_i32(v: i32) -> CallState {
        match v {
            1 => CallState::Dialing,
            2 => CallState::RingingOut,
            3 => CallState::RingingIn,
            4 => CallState::Active,
            5 => CallState::Held,
            6 => CallState::Waiting,
            7 => CallState::Terminated,
            _ => CallState::Unknown,
        }
    }

    pub fn as_i32(self) -> i32 {
        match self {
            CallState::Unknown => 0,
            CallState::Dialing => 1,
            CallState::RingingOut => 2,
            CallState::RingingIn => 3,
            CallState::Active => 4,
            CallState::Held => 5,
            CallState::Waiting => 6,
            CallState::Terminated => 7,
        }
    }

    /// The name used on org.omamobile.Phone1 ("ringing-in" etc.).
    pub fn name(self) -> &'static str {
        match self {
            CallState::Unknown => "unknown",
            CallState::Dialing => "dialing",
            CallState::RingingOut => "ringing-out",
            CallState::RingingIn => "ringing-in",
            CallState::Active => "active",
            CallState::Held => "held",
            CallState::Waiting => "waiting",
            CallState::Terminated => "terminated",
        }
    }
}

/// MMCallStateReason.
pub mod call_reason {
    pub const UNKNOWN: u32 = 0;
    pub const OUTGOING_STARTED: u32 = 1;
    pub const INCOMING_NEW: u32 = 2;
    pub const ACCEPTED: u32 = 3;
    pub const TERMINATED: u32 = 4;
    pub const REFUSED_OR_BUSY: u32 = 5;
    pub const ERROR: u32 = 6;
}

/// MMCallDirection.
pub mod call_direction {
    pub const UNKNOWN: i32 = 0;
    pub const INCOMING: i32 = 1;
    pub const OUTGOING: i32 = 2;
}

/// MMSmsState.
pub mod sms_state {
    pub const UNKNOWN: u32 = 0;
    pub const STORED: u32 = 1;
    pub const RECEIVING: u32 = 2;
    pub const RECEIVED: u32 = 3;
    pub const SENDING: u32 = 4;
    pub const SENT: u32 = 5;
}

/// MMSmsPduType (the GSM ones).
pub mod pdu_type {
    pub const UNKNOWN: u32 = 0;
    pub const DELIVER: u32 = 1;
    pub const SUBMIT: u32 = 2;
    pub const STATUS_REPORT: u32 = 3;
}

/// MMSmsDeliveryState, grouped the way a messaging UI needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// No report (yet), or the modem doesn't know.
    Unknown,
    /// The recipient's phone has it.
    Delivered,
    /// The network is still trying.
    Pending,
    /// The network gave up.
    Failed,
}

impl Delivery {
    /// 0x00-0x1f completed, 0x20-0x3f temporary (still trying),
    /// 0x40-0x7f permanent or temporary-fatal errors, 0x100 unknown
    /// (3GPP TS 23.040 §9.2.3.15, as MM maps it).
    pub fn from_u32(v: u32) -> Delivery {
        match v {
            0x00..=0x1f => Delivery::Delivered,
            0x20..=0x3f => Delivery::Pending,
            0x40..=0xff => Delivery::Failed,
            _ => Delivery::Unknown,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("D-Bus: {0}")]
    Dbus(#[from] zbus::Error),
    #[error("D-Bus: {0}")]
    Fdo(#[from] zbus::fdo::Error),
    #[error("no modem")]
    NoModem,
}

pub type Result<T> = std::result::Result<T, Error>;

/// A modem object and the interfaces it has.
#[derive(Debug, Clone)]
pub struct ModemObject {
    pub path: OwnedObjectPath,
    pub has_voice: bool,
    pub has_messaging: bool,
}

/// The modems ModemManager exports now: voice-capable ones first.
pub async fn modems(conn: &Connection) -> Result<Vec<ModemObject>> {
    let om = zbus::fdo::ObjectManagerProxy::builder(conn)
        .destination(SERVICE)?
        .path(PATH)?
        .build()
        .await?;
    let objects = om.get_managed_objects().await?;
    let mut out: Vec<ModemObject> = objects
        .into_iter()
        .filter_map(|(path, ifaces)| {
            let names: Vec<String> = ifaces.keys().map(|k| k.to_string()).collect();
            names.iter().any(|n| n == IFACE_MODEM).then(|| ModemObject {
                path,
                has_voice: names.iter().any(|n| n == IFACE_VOICE),
                has_messaging: names.iter().any(|n| n == IFACE_MESSAGING),
            })
        })
        .collect();
    out.sort_by_key(|m| (!m.has_voice, m.path.to_string()));
    Ok(out)
}

/// Wait until ModemManager's name has an owner (it may start after us).
pub async fn wait_for_service(conn: &Connection) -> Result<()> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let mut changes = dbus.receive_name_owner_changed().await?;
    if dbus
        .name_has_owner(SERVICE.try_into().expect("valid name"))
        .await?
    {
        return Ok(());
    }
    while let Some(sig) = changes.next().await {
        if let Ok(args) = sig.args() {
            if args.name() == SERVICE && args.new_owner().is_some() {
                return Ok(());
            }
        }
    }
    Err(Error::NoModem)
}

/// The properties for `Voice.CreateCall`.
pub fn call_properties(number: &str) -> HashMap<&'static str, Value<'_>> {
    HashMap::from([("number", Value::from(number))])
}

/// The properties for `Messaging.Create` (a text SMS).
pub fn sms_properties<'a>(
    number: &'a str,
    text: &'a str,
    delivery_report: bool,
) -> HashMap<&'static str, Value<'a>> {
    HashMap::from([
        ("number", Value::from(number)),
        ("text", Value::from(text)),
        ("delivery-report-request", Value::from(delivery_report)),
    ])
}

/// An SMS object's contents, read in one go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmsInfo {
    pub path: OwnedObjectPath,
    pub state: u32,
    pub pdu_type: u32,
    pub number: String,
    pub text: String,
    pub data: Vec<u8>,
    pub timestamp: String,
    pub delivery_state: u32,
    pub message_reference: u32,
}

impl SmsInfo {
    pub async fn read(conn: &Connection, path: OwnedObjectPath) -> Result<SmsInfo> {
        let all = zbus::fdo::PropertiesProxy::builder(conn)
            .destination(SERVICE)?
            .path(path.clone())?
            .build()
            .await?
            .get_all(
                "org.freedesktop.ModemManager1.Sms"
                    .try_into()
                    .expect("name"),
            )
            .await?;
        let u = |k: &str| all.get(k).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        let s = |k: &str| {
            all.get(k)
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default()
        };
        let data = all
            .get("Data")
            .and_then(|v: &OwnedValue| Vec::<u8>::try_from(v.clone()).ok())
            .unwrap_or_default();
        Ok(SmsInfo {
            path,
            state: u("State"),
            pdu_type: u("PduType"),
            number: s("Number"),
            text: s("Text"),
            data,
            timestamp: s("Timestamp"),
            delivery_state: all
                .get("DeliveryState")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0x100),
            message_reference: u("MessageReference"),
        })
    }

    /// A received text message (not a status report, not binary data such
    /// as a WAP push, which belongs to mmsd-tng).
    pub fn is_received_text(&self) -> bool {
        self.pdu_type != pdu_type::SUBMIT
            && self.pdu_type != pdu_type::STATUS_REPORT
            && self.state == sms_state::RECEIVED
            && !(self.text.is_empty() && !self.data.is_empty())
    }

    /// Binary data with no text: a WAP push (MMS notification) or another
    /// application-port message, left for the service that owns it.
    pub fn is_binary(&self) -> bool {
        self.text.is_empty() && !self.data.is_empty()
    }
}

/// Parse an MM timestamp (ISO 8601 as MM writes it: `2026-10-14T09:30:00+02:00`,
/// or with a two-digit offset `+02`, or `Z`) into ms since the epoch.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp_millis());
    }
    // A two-digit offset ("+02") or none at all.
    let fixed = if s.len() >= 3 && (s.ends_with(|c: char| c.is_ascii_digit())) {
        let tail = &s[s.len() - 3..];
        if tail.starts_with(['+', '-']) {
            format!("{s}:00")
        } else {
            format!("{s}Z")
        }
    } else {
        s.to_string()
    };
    chrono::DateTime::parse_from_rfc3339(&fixed)
        .ok()
        .map(|t| t.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_states_round_trip() {
        for v in 0..8 {
            assert_eq!(CallState::from_i32(v).as_i32(), v);
        }
        assert_eq!(CallState::from_i32(99), CallState::Unknown);
        assert_eq!(CallState::RingingIn.name(), "ringing-in");
    }

    #[test]
    fn delivery_groups() {
        assert_eq!(Delivery::from_u32(0), Delivery::Delivered);
        assert_eq!(Delivery::from_u32(0x02), Delivery::Delivered);
        assert_eq!(Delivery::from_u32(0x20), Delivery::Pending);
        assert_eq!(Delivery::from_u32(0x41), Delivery::Failed);
        assert_eq!(Delivery::from_u32(0x100), Delivery::Unknown);
    }

    #[test]
    fn timestamps() {
        assert_eq!(
            parse_timestamp("2026-10-14T09:30:00+00:00"),
            Some(1_791_970_200_000)
        );
        assert_eq!(
            parse_timestamp("2026-10-14T11:30:00+02"),
            Some(1_791_970_200_000)
        );
        assert_eq!(
            parse_timestamp("2026-10-14T09:30:00Z"),
            Some(1_791_970_200_000)
        );
        assert_eq!(
            parse_timestamp("2026-10-14T09:30:00"),
            Some(1_791_970_200_000)
        );
        assert_eq!(parse_timestamp(""), None);
        assert_eq!(parse_timestamp("yesterday"), None);
    }

    #[test]
    fn sms_kinds() {
        let mut s = SmsInfo {
            path: OwnedObjectPath::try_from("/x").unwrap(),
            state: sms_state::RECEIVED,
            pdu_type: pdu_type::DELIVER,
            number: "+12025550100".into(),
            text: "hi".into(),
            data: vec![],
            timestamp: String::new(),
            delivery_state: 0x100,
            message_reference: 0,
        };
        assert!(s.is_received_text());
        s.text.clear();
        s.data = vec![1, 2];
        assert!(s.is_binary() && !s.is_received_text());
        s.data.clear();
        s.state = sms_state::RECEIVING;
        assert!(!s.is_received_text());
    }
}
