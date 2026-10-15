// SPDX-License-Identifier: GPL-3.0-or-later
//! Looks shared by Phone and Messages: the Omarchy palette (theme.rs, from
//! gliff by way of omacam) and the app stylesheet with the phone's large
//! touch targets (dialpad keys, call buttons, message bubbles).

pub mod theme;

use gtk4 as gtk;

pub use theme::{follow_omarchy_theme, Mode, Palette, Rgb};

/// The stylesheet both apps use, on top of libadwaita's. Colors come from
/// libadwaita's variables, so they follow the Omarchy palette.
pub const APP_CSS: &str = include_str!("app.css");

/// Load [`APP_CSS`] plus an app's own CSS, and start following the Omarchy
/// theme. Keep the returned monitor alive.
pub fn install(extra_css: &str) -> Option<gtk::gio::FileMonitor> {
    if let Some(display) = gtk::gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&format!("{APP_CSS}\n{extra_css}"));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION - 1,
        );
    }
    follow_omarchy_theme()
}

/// Initials for an avatar-less contact row: "Alex Example" -> "AE". A
/// bare number has none ("#"), so digits never pose as initials.
pub fn initials(name: &str) -> String {
    let mut out: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .filter(|c| c.is_alphabetic())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if out.is_empty() {
        out.push('#');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("Alex Example"), "AE");
        assert_eq!(initials("  zoë  "), "Z");
        assert_eq!(initials("+1 555 0100"), "#");
        assert_eq!(initials("(202) 555-0150"), "#");
        assert_eq!(initials(""), "#");
        assert_eq!(initials("Ann Bee Cee"), "AB");
    }
}
