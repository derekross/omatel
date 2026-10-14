// SPDX-License-Identifier: GPL-3.0-or-later
//! How many SMS a text takes (3GPP TS 23.038 and 23.040): GSM 7-bit text
//! fits 160 characters in one message and 153 per part when split, with
//! the extension characters (`^{}\[~]|€`) counting twice; anything outside
//! the GSM alphabet (most accents, every emoji) switches the whole text to
//! UCS-2, 70 per message and 67 per part, counted in UTF-16 units.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Gsm7,
    Ucs2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Count {
    pub encoding: Encoding,
    /// Units used (septets or UTF-16 units).
    pub units: usize,
    pub segments: usize,
    /// Units left in the last segment.
    pub remaining: usize,
}

const BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?\
¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
const EXTENSION: &str = "^{}\\[~]|€\u{c}";

fn gsm_units(c: char) -> Option<usize> {
    if BASIC.contains(c) {
        Some(1)
    } else if EXTENSION.contains(c) {
        Some(2)
    } else {
        None
    }
}

pub fn count(text: &str) -> Count {
    let gsm: Option<usize> = text.chars().map(gsm_units).sum();
    let (encoding, units, single, part) = match gsm {
        Some(n) => (Encoding::Gsm7, n, 160, 153),
        None => (Encoding::Ucs2, text.encode_utf16().count(), 70, 67),
    };
    let (segments, capacity) = if units <= single {
        (1, single)
    } else {
        let s = units.div_ceil(part);
        (s, s * part)
    };
    Count {
        encoding,
        units,
        segments,
        remaining: capacity - units,
    }
}

/// The counter under the composer, shown once a text nears one message:
/// "12 left · 2 SMS". None below 140 units.
pub fn label(text: &str) -> Option<String> {
    let c = count(text);
    let near = match c.encoding {
        Encoding::Gsm7 => 140,
        Encoding::Ucs2 => 60,
    };
    if c.units < near {
        return None;
    }
    Some(if c.segments == 1 {
        format!("{} left", c.remaining)
    } else {
        format!("{} left · {} SMS", c.remaining, c.segments)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gsm_single_and_split() {
        let c = count(&"a".repeat(160));
        assert_eq!(
            (c.encoding, c.segments, c.remaining),
            (Encoding::Gsm7, 1, 0)
        );
        let c = count(&"a".repeat(161));
        assert_eq!((c.segments, c.remaining), (2, 306 - 161));
        let c = count(&"a".repeat(306));
        assert_eq!((c.segments, c.remaining), (2, 0));
        assert_eq!(count(&"a".repeat(307)).segments, 3);
        assert_eq!(count("").segments, 1);
    }

    #[test]
    fn extension_characters_count_twice() {
        let c = count("€[]");
        assert_eq!((c.encoding, c.units), (Encoding::Gsm7, 6));
        let c = count(&format!("{}{}", "a".repeat(159), "€"));
        assert_eq!(c.segments, 2);
    }

    #[test]
    fn ucs2_for_emoji_and_other_scripts() {
        let c = count("On my way 🚗");
        assert_eq!(c.encoding, Encoding::Ucs2);
        assert_eq!(c.units, 12); // the emoji is a surrogate pair
        let c = count(&"ç".repeat(70)); // ç is not in GSM (Ç is)
        assert_eq!((c.encoding, c.segments), (Encoding::Ucs2, 1));
        assert_eq!(count(&"ç".repeat(71)).segments, 2);
        assert_eq!(count("Ça va? très bien").encoding, Encoding::Gsm7);
    }

    #[test]
    fn counter_label() {
        assert_eq!(label("short"), None);
        assert_eq!(label(&"a".repeat(150)).as_deref(), Some("10 left"));
        assert_eq!(label(&"a".repeat(200)).as_deref(), Some("106 left · 2 SMS"));
        assert_eq!(label(&"🙂".repeat(31)).as_deref(), Some("8 left"));
    }
}
