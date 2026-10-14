// SPDX-License-Identifier: GPL-3.0-or-later
//! A tiny test script to drive the app without hands, for screenshots in
//! a headless compositor and for trying it on a phone someone is using:
//! `--script "wait 1; open 0; shot /tmp/thread.png; back; quit"`.
//!
//! Steps: `wait SECONDS`, `open N` (the Nth conversation in the list,
//! from 0), `open-archived`, `compose`, `type TEXT` (into the composer,
//! or the To field on the compose page), `search TEXT` (empty clears),
//! `settings`, `close-settings`, `blocked`, `back`, `menu N` (the
//! long-press menu of row N), `close-menu`, `shot PATH` and `quit`.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Wait(Duration),
    Open(usize),
    OpenArchived,
    Compose,
    Type(String),
    Search(String),
    Settings,
    CloseSettings,
    Blocked,
    Back,
    Menu(usize),
    CloseMenu,
    Shot(PathBuf),
    Quit,
}

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
                .map_err(|_| format!("{cmd}: needs a row number, got {arg:?}"))
        };
        steps.push(match cmd {
            "wait" => Step::Wait(Duration::from_secs_f64(
                arg.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| format!("wait: needs seconds, got {arg:?}"))?,
            )),
            "open" => Step::Open(index()?),
            "open-archived" => Step::OpenArchived,
            "compose" => Step::Compose,
            "type" => Step::Type(arg.to_string()),
            "search" => Step::Search(arg.to_string()),
            "settings" => Step::Settings,
            "close-settings" => Step::CloseSettings,
            "blocked" => Step::Blocked,
            "back" => Step::Back,
            "menu" => Step::Menu(index()?),
            "close-menu" => Step::CloseMenu,
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
            "wait 0.5; open 2; open-archived\ncompose; type Hello there; search din;\
             search; settings; close-settings; blocked; back; menu 1; close-menu; shot /tmp/a.png; quit",
        )
        .unwrap();
        assert_eq!(
            s,
            vec![
                Step::Wait(Duration::from_millis(500)),
                Step::Open(2),
                Step::OpenArchived,
                Step::Compose,
                Step::Type("Hello there".into()),
                Step::Search("din".into()),
                Step::Search(String::new()),
                Step::Settings,
                Step::CloseSettings,
                Step::Blocked,
                Step::Back,
                Step::Menu(1),
                Step::CloseMenu,
                Step::Shot("/tmp/a.png".into()),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn rejects_bad_steps() {
        assert!(parse("open x").is_err());
        assert!(parse("wait -1").is_err());
        assert!(parse("shot").is_err());
        assert!(parse("dance").is_err());
        assert_eq!(parse(" ; ;").unwrap(), vec![]);
    }
}
