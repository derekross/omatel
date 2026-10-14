// SPDX-License-Identifier: GPL-3.0-or-later
//! Text for the list and the thread: times relative to now, day
//! separators, snippets and delivery states. Pure, so it is tested
//! without a display.

use chrono::{DateTime, Datelike, TimeZone};
use omatel_store::{Direction, Message, MessageState};

fn at<Tz: TimeZone>(ms: i64, tz: &Tz) -> DateTime<Tz> {
    tz.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(|| tz.timestamp_millis_opt(0).unwrap())
}

fn days_between<Tz: TimeZone>(then: &DateTime<Tz>, now: &DateTime<Tz>) -> i64 {
    (now.date_naive() - then.date_naive()).num_days()
}

/// The clock time, "2:05 PM".
pub fn clock<Tz: TimeZone>(ms: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    at(ms, tz).format("%-I:%M %p").to_string()
}

/// The time in the conversation list: the clock today, "Yesterday", the
/// weekday this week, "Oct 3" this year, "Oct 3, 2025" before.
pub fn list_time<Tz: TimeZone>(ms: i64, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let tz = now.timezone();
    let then = at(ms, &tz);
    match days_between(&then, now) {
        d if d <= 0 => clock(ms, &tz),
        1 => "Yesterday".into(),
        2..=6 => then.format("%a").to_string(),
        _ if then.year() == now.year() => then.format("%b %-d").to_string(),
        _ => then.format("%b %-d, %Y").to_string(),
    }
}

/// The separator above the first message of a day in a thread.
pub fn day_separator<Tz: TimeZone>(ms: i64, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let tz = now.timezone();
    let then = at(ms, &tz);
    match days_between(&then, now) {
        d if d <= 0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => then.format("%A").to_string(),
        _ if then.year() == now.year() => then.format("%B %-d").to_string(),
        _ => then.format("%B %-d, %Y").to_string(),
    }
}

/// Whether two times fall on different local days (a separator between).
pub fn new_day<Tz: TimeZone>(prev_ms: i64, ms: i64, tz: &Tz) -> bool {
    at(prev_ms, tz).date_naive() != at(ms, tz).date_naive()
}

/// One line for the conversation list: the draft if there is one, else
/// the last message ("You: " for ours), newlines folded, cut to `max`.
/// Returns (prefix, text); the prefix is styled apart.
pub fn snippet(last: Option<&Message>, draft: &str, max: usize) -> (&'static str, String) {
    let (prefix, text) = if !draft.trim().is_empty() {
        ("Draft: ", draft)
    } else {
        match last {
            Some(m) if m.direction == Direction::Out && m.state == MessageState::Failed => {
                ("Not sent: ", m.body.as_str())
            }
            Some(m) if m.direction == Direction::Out => ("You: ", m.body.as_str()),
            Some(m) => ("", m.body.as_str()),
            None => ("", ""),
        }
    };
    let folded: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = if folded.chars().count() > max {
        let mut s: String = folded.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    } else {
        folded
    };
    (prefix, cut)
}

/// The line under one of our messages: "2:05 PM · Delivered".
pub fn delivery<Tz: TimeZone>(m: &Message, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let time = clock(m.at, tz);
    match (m.direction, m.state) {
        (Direction::In, _) => time,
        (_, MessageState::Sending) => "Sending…".into(),
        (_, MessageState::Sent) => format!("{time} · Sent"),
        (_, MessageState::Delivered) => format!("{time} · Delivered"),
        (_, MessageState::Failed) => "Not sent · Tap to retry".into(),
        (_, MessageState::Received) => time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};

    fn msg(direction: Direction, state: MessageState, body: &str, at: i64) -> Message {
        Message {
            id: 1,
            conversation_id: 1,
            direction,
            transport: "sms".into(),
            sender: String::new(),
            body: body.into(),
            at,
            sent_at: None,
            delivered_at: None,
            state,
            read: true,
            mm_path: None,
            error: None,
        }
    }

    // Wednesday 2026-10-14 15:30 UTC.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, 15, 30, 0).unwrap()
    }

    fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn list_times() {
        let n = now();
        assert_eq!(list_time(ms(2026, 10, 14, 9, 5), &n), "9:05 AM");
        assert_eq!(list_time(ms(2026, 10, 14, 14, 5), &n), "2:05 PM");
        assert_eq!(list_time(ms(2026, 10, 13, 23, 59), &n), "Yesterday");
        assert_eq!(list_time(ms(2026, 10, 10, 8, 0), &n), "Sat");
        assert_eq!(list_time(ms(2026, 3, 3, 8, 0), &n), "Mar 3");
        assert_eq!(list_time(ms(2025, 12, 31, 8, 0), &n), "Dec 31, 2025");
        // A clock a little ahead of ours still says today.
        assert_eq!(list_time(ms(2026, 10, 14, 15, 31), &n), "3:31 PM");
    }

    #[test]
    fn days_follow_the_time_zone() {
        let tz = FixedOffset::west_opt(5 * 3600).unwrap();
        let n = now().with_timezone(&tz); // 10:30 local
                                          // 02:00 UTC on the 14th is 21:00 on the 13th, five hours west.
        assert_eq!(day_separator(ms(2026, 10, 14, 2, 0), &n), "Yesterday");
        assert!(new_day(ms(2026, 10, 14, 2, 0), ms(2026, 10, 14, 6, 0), &tz));
        assert!(!new_day(
            ms(2026, 10, 14, 6, 0),
            ms(2026, 10, 14, 9, 0),
            &tz
        ));
    }

    #[test]
    fn separators() {
        let n = now();
        assert_eq!(day_separator(ms(2026, 10, 14, 1, 0), &n), "Today");
        assert_eq!(day_separator(ms(2026, 10, 12, 1, 0), &n), "Monday");
        assert_eq!(day_separator(ms(2026, 9, 2, 1, 0), &n), "September 2");
        assert_eq!(day_separator(ms(2024, 9, 2, 1, 0), &n), "September 2, 2024");
    }

    #[test]
    fn snippets() {
        let m = msg(Direction::Out, MessageState::Sent, "See you\nthere", 0);
        assert_eq!(snippet(Some(&m), "", 40), ("You: ", "See you there".into()));
        assert_eq!(
            snippet(Some(&m), "half a thought", 40),
            ("Draft: ", "half a thought".into())
        );
        let i = msg(Direction::In, MessageState::Received, "abcdefghij", 0);
        assert_eq!(snippet(Some(&i), " ", 5), ("", "abcd…".into()));
        let f = msg(Direction::Out, MessageState::Failed, "late", 0);
        assert_eq!(snippet(Some(&f), "", 40).0, "Not sent: ");
        assert_eq!(snippet(None, "", 10), ("", String::new()));
    }

    #[test]
    fn delivery_lines() {
        let at = ms(2026, 10, 14, 14, 5);
        let d = |s| delivery(&msg(Direction::Out, s, "x", at), &Utc);
        assert_eq!(d(MessageState::Sending), "Sending…");
        assert_eq!(d(MessageState::Sent), "2:05 PM · Sent");
        assert_eq!(d(MessageState::Delivered), "2:05 PM · Delivered");
        assert_eq!(d(MessageState::Failed), "Not sent · Tap to retry");
        let i = msg(Direction::In, MessageState::Received, "x", at);
        assert_eq!(delivery(&i, &Utc), "2:05 PM");
    }
}
