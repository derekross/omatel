// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings shared by the agent and both apps, in
//! `~/.config/omatel/settings.toml`. The apps write them; the agent reads
//! them again before each decision that depends on them, so a change needs
//! no restart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Show the sender and text of a message in notifications while the
    /// phone is locked. Off: "New message" only (the owner's Q7 default).
    pub lockscreen_details: bool,
    /// Notify about new messages at all.
    pub message_notifications: bool,
    /// Ask the network for delivery reports on sent messages.
    pub delivery_reports: bool,
    /// Ring for callers who aren't in the contacts? Off: they are
    /// silenced (logged as missed, no ring).
    pub ring_unknown_callers: bool,
    /// Open the Phone app's answer page for an incoming call.
    pub show_incoming_call_page: bool,
    /// The region for numbers written without a country code (two
    /// letters); empty: from the locale.
    pub region: String,
    /// Replies offered when declining a call with a message.
    pub quick_replies: Vec<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            lockscreen_details: false,
            message_notifications: true,
            delivery_reports: true,
            ring_unknown_callers: true,
            show_incoming_call_page: true,
            region: String::new(),
            quick_replies: vec![
                "Can't talk now. Call you later?".into(),
                "I'll call you back.".into(),
                "I'm on my way.".into(),
                "Can you text me instead?".into(),
            ],
        }
    }
}

/// `$XDG_CONFIG_HOME/omatel/settings.toml`.
pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(&std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("omatel/settings.toml")
}

impl Settings {
    /// Read the settings; defaults for a missing or unreadable file.
    pub fn load() -> Settings {
        Settings::load_from(&path())
    }

    pub fn load_from(p: &Path) -> Settings {
        match std::fs::read_to_string(p) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "settings unreadable; using defaults");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&path())
    }

    pub fn save_to(&self, p: &Path) -> std::io::Result<()> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, p)
    }

    /// The region to use: the setting, else the locale's.
    pub fn region(&self, locale_default: impl FnOnce() -> String) -> String {
        if self.region.len() == 2 {
            self.region.to_ascii_uppercase()
        } else {
            locale_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_save_and_partial_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("omatel/settings.toml");
        assert_eq!(Settings::load_from(&p), Settings::default());
        assert!(!Settings::default().lockscreen_details);
        let s = Settings {
            lockscreen_details: true,
            region: "gb".into(),
            ..Settings::default()
        };
        s.save_to(&p).unwrap();
        let back = Settings::load_from(&p);
        assert_eq!(back, s);
        assert_eq!(back.region(|| "US".into()), "GB");
        std::fs::write(&p, "delivery_reports = false\n").unwrap();
        let partial = Settings::load_from(&p);
        assert!(!partial.delivery_reports);
        assert!(partial.message_notifications);
        assert_eq!(partial.region(|| "US".into()), "US");
        std::fs::write(&p, "not = [toml").unwrap();
        assert_eq!(Settings::load_from(&p), Settings::default());
    }
}
