// SPDX-License-Identifier: GPL-3.0-or-later
//! A tiny test script for driving the app without hands, for screenshots
//! in a headless compositor and for checks on a phone someone is using:
//! `--script "wait 1; tab keypad; type 2025; shot /tmp/k.png; quit"`.
//!
//! Steps: `wait SECONDS`, `tab favorites|recents|contacts|keypad`,
//! `type DIGITS` (into the dialpad), `open-contact N` (the Nth contact,
//! from 1), `edit-contact`, `new-contact`, `back`, `details N` (the Nth
//! recent call), `fake-incoming NUMBER` and `fake-call NUMBER STATE`
//! (a call shown without the agent: dialing, active or ended),
//! `emergency` (the emergency page), `shot PATH` and `quit`.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Wait(Duration),
    Tab(String),
    Type(String),
    OpenContact(usize),
    EditContact,
    NewContact,
    Back,
    Details(usize),
    FakeIncoming(String),
    FakeCall(String, String),
    Emergency,
    Shot(PathBuf),
    Quit,
}

pub const TABS: &[&str] = &["favorites", "recents", "contacts", "keypad"];

pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for part in text
        .split([';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let (cmd, arg) = part
            .split_once(char::is_whitespace)
            .map(|(c, a)| (c, a.trim()))
            .unwrap_or((part, ""));
        let index = || {
            arg.parse::<usize>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| format!("{cmd}: needs a number from 1, got {arg:?}"))
        };
        steps.push(match cmd {
            "wait" => Step::Wait(Duration::from_secs_f64(
                arg.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| format!("wait: needs seconds, got {arg:?}"))?,
            )),
            "tab" if TABS.contains(&arg) => Step::Tab(arg.into()),
            "tab" => return Err(format!("tab: one of {}, got {arg:?}", TABS.join(", "))),
            "type" if !arg.is_empty() => Step::Type(arg.into()),
            "type" => return Err("type: needs digits".into()),
            "open-contact" => Step::OpenContact(index()?),
            "edit-contact" => Step::EditContact,
            "new-contact" => Step::NewContact,
            "back" => Step::Back,
            "details" => Step::Details(index()?),
            "fake-incoming" if !arg.is_empty() => Step::FakeIncoming(arg.into()),
            "fake-incoming" => return Err("fake-incoming: needs a number".into()),
            "fake-call" => {
                let mut it = arg.split_whitespace();
                match (it.next(), it.next()) {
                    (Some(n), Some(s)) if ["dialing", "active", "ended"].contains(&s) => {
                        Step::FakeCall(n.into(), s.into())
                    }
                    _ => {
                        return Err(format!(
                            "fake-call: needs NUMBER and dialing|active|ended, got {arg:?}"
                        ))
                    }
                }
            }
            "emergency" => Step::Emergency,
            "shot" if !arg.is_empty() => Step::Shot(PathBuf::from(arg)),
            "shot" => return Err("shot: needs a path".into()),
            "quit" => Step::Quit,
            other => return Err(format!("unknown step {other:?}")),
        });
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_step() {
        let s = parse(
            "wait 0.5; tab keypad; type 2025; open-contact 2\nedit-contact; new-contact; back;
             details 1; fake-incoming +12025550100; fake-call +12025550101 active; emergency;
             shot /tmp/x.png; quit",
        )
        .unwrap();
        assert_eq!(s.len(), 13);
        assert_eq!(s[0], Step::Wait(Duration::from_millis(500)));
        assert_eq!(s[1], Step::Tab("keypad".into()));
        assert_eq!(s[2], Step::Type("2025".into()));
        assert_eq!(s[3], Step::OpenContact(2));
        assert_eq!(s[9], Step::FakeCall("+12025550101".into(), "active".into()));
        assert_eq!(s[11], Step::Shot("/tmp/x.png".into()));
        assert_eq!(s[12], Step::Quit);
    }

    #[test]
    fn rejects_bad_steps() {
        assert!(parse("tab settings").is_err());
        assert!(parse("wait -1").is_err());
        assert!(parse("open-contact 0").is_err());
        assert!(parse("details x").is_err());
        assert!(parse("fake-call +12025550100 ringing").is_err());
        assert!(parse("shot").is_err());
        assert!(parse("dance").is_err());
        assert_eq!(parse(" ; ;").unwrap(), vec![]);
    }
}
