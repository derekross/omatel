// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure helpers for the Phone app: `tel:` URIs, time and duration text,
//! and how a call-log entry is drawn. No GTK here, so it is unit tested.

use chrono::{Datelike, Local, NaiveDateTime, TimeZone};
use omatel_contacts::number;
use omatel_store::{CallDirection, CallOutcome};

/// The number in a `tel:` or `callto:` URI (or a bare number), ready for
/// the dialpad: percent-decoded, parameters (`;phone-context=…`) dropped,
/// visual separators removed. None when nothing dialable is left.
pub fn parse_dial_uri(uri: &str) -> Option<String> {
    let s = uri.trim();
    let lower = s.to_ascii_lowercase();
    let rest = if let Some(r) = ["tel:", "callto:"]
        .iter()
        .find_map(|p| lower.starts_with(p).then(|| &s[p.len()..]))
    {
        r.trim_start_matches('/')
    } else if lower.contains(':') {
        return None; // another scheme
    } else {
        s
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(rest);
    // RFC 3966 parameters follow ';'. A post-dial part written as
    // ";postd=123" is kept as a pause.
    let mut parts = decoded.split(';');
    let mut number_part = parts.next().unwrap_or("").to_string();
    for p in parts {
        if let Some(v) = p.strip_prefix("postd=") {
            number_part.push(',');
            number_part.push_str(v);
        }
    }
    let out = number::sanitize_dial_string(&number_part);
    (!out.trim_start_matches('+').is_empty()).then_some(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 {
            if let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A call's length: "0:42", "12:05", "1:02:03".
pub fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// When something happened, as a call log shows it.
pub fn relative_time_naive(then: NaiveDateTime, now: NaiveDateTime) -> String {
    let delta = now.signed_duration_since(then);
    if delta.num_seconds() < 60 && delta.num_seconds() > -60 {
        return "Just now".into();
    }
    if delta.num_minutes() > 0 && delta.num_minutes() < 60 {
        return format!("{} min ago", delta.num_minutes());
    }
    let (td, nd) = (then.date(), now.date());
    if td == nd {
        return then.format("%H:%M").to_string();
    }
    if nd.pred_opt() == Some(td) {
        return "Yesterday".into();
    }
    let days = (nd - td).num_days();
    if (2..7).contains(&days) {
        return then.format("%a").to_string();
    }
    if td.year() == nd.year() {
        then.format("%b %-d").to_string()
    } else {
        then.format("%b %-d, %Y").to_string()
    }
}

/// [`relative_time_naive`] for ms since the epoch, in local time.
pub fn relative_time(then_ms: i64, now_ms: i64) -> String {
    let to = |ms: i64| {
        Local
            .timestamp_millis_opt(ms)
            .single()
            .map(|d| d.naive_local())
    };
    match (to(then_ms), to(now_ms)) {
        (Some(t), Some(n)) => relative_time_naive(t, n),
        _ => String::new(),
    }
}

/// Date and time for the call details page.
pub fn full_time(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%a %b %-d, %H:%M").to_string())
        .unwrap_or_default()
}

/// How a call-log entry looks: icon, whether it's drawn in the warning
/// colour, and a word for it.
pub fn outcome_look(
    direction: CallDirection,
    outcome: CallOutcome,
) -> (&'static str, bool, &'static str) {
    match (direction, outcome) {
        (_, CallOutcome::Missed) => ("call-missed-symbolic", true, "Missed"),
        (_, CallOutcome::Blocked) => ("action-unavailable-symbolic", true, "Blocked"),
        (_, CallOutcome::Rejected) => ("call-stop-symbolic", false, "Declined"),
        (CallDirection::In, _) => ("call-incoming-symbolic", false, "Incoming"),
        (CallDirection::Out, CallOutcome::Answered) => {
            ("call-outgoing-symbolic", false, "Outgoing")
        }
        (CallDirection::Out, CallOutcome::Unanswered) => {
            ("call-outgoing-symbolic", false, "No answer")
        }
        (CallDirection::Out, _) => ("call-outgoing-symbolic", true, "Failed"),
    }
}

/// The status line of the in-call page for an agent call state.
pub fn call_status(state: &str, talked_secs: Option<i64>) -> String {
    match state {
        "dialing" => "Calling…".into(),
        "ringing-out" => "Ringing…".into(),
        "ringing-in" | "waiting" => "Incoming call".into(),
        "active" => format_duration(talked_secs.unwrap_or(0)),
        "held" => "On hold".into(),
        "terminated" => "Call ended".into(),
        _ => "Connecting…".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn tel_uris() {
        assert_eq!(
            parse_dial_uri("tel:+1-202-555-0100").as_deref(),
            Some("+12025550100")
        );
        assert_eq!(
            parse_dial_uri("TEL:%2B1%20202%20555%200101").as_deref(),
            Some("+12025550101")
        );
        assert_eq!(
            parse_dial_uri("tel:555-0102;phone-context=example.org").as_deref(),
            Some("5550102")
        );
        assert_eq!(
            parse_dial_uri("tel:2025550103;postd=pp12").as_deref(),
            Some("2025550103,,,12")
        );
        assert_eq!(
            parse_dial_uri("callto://+12025550104").as_deref(),
            Some("+12025550104")
        );
        assert_eq!(
            parse_dial_uri("202 555 0105").as_deref(),
            Some("2025550105")
        );
        assert_eq!(parse_dial_uri("sms:+12025550100"), None);
        assert_eq!(parse_dial_uri("tel:"), None);
        assert_eq!(parse_dial_uri("tel:+"), None);
        assert_eq!(parse_dial_uri("tel:*%23"), Some("*#".into()));
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(42), "0:42");
        assert_eq!(format_duration(725), "12:05");
        assert_eq!(format_duration(3723), "1:02:03");
        assert_eq!(format_duration(-5), "0:00");
    }

    #[test]
    fn relative_times() {
        let now = at(14, 15, 30);
        assert_eq!(relative_time_naive(at(14, 15, 30), now), "Just now");
        assert_eq!(relative_time_naive(at(14, 15, 18), now), "12 min ago");
        assert_eq!(relative_time_naive(at(14, 9, 5), now), "09:05");
        assert_eq!(relative_time_naive(at(13, 22, 0), now), "Yesterday");
        assert_eq!(relative_time_naive(at(10, 8, 0), now), "Sat");
        assert_eq!(relative_time_naive(at(1, 8, 0), now), "Oct 1");
        let old = NaiveDate::from_ymd_opt(2025, 3, 4)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap();
        assert_eq!(relative_time_naive(old, now), "Mar 4, 2025");
    }

    #[test]
    fn outcome_icons() {
        use CallDirection::*;
        use CallOutcome::*;
        assert_eq!(
            outcome_look(In, Missed),
            ("call-missed-symbolic", true, "Missed")
        );
        assert_eq!(outcome_look(In, Answered).0, "call-incoming-symbolic");
        assert_eq!(outcome_look(Out, Answered).0, "call-outgoing-symbolic");
        assert_eq!(outcome_look(Out, Unanswered).2, "No answer");
        assert!(outcome_look(Out, Failed).1);
        assert_eq!(outcome_look(In, Rejected).2, "Declined");
        assert_eq!(outcome_look(In, Blocked).2, "Blocked");
    }

    #[test]
    fn statuses() {
        assert_eq!(call_status("dialing", None), "Calling…");
        assert_eq!(call_status("ringing-out", None), "Ringing…");
        assert_eq!(call_status("active", Some(65)), "1:05");
        assert_eq!(call_status("terminated", None), "Call ended");
        assert_eq!(call_status("weird", None), "Connecting…");
    }
}
