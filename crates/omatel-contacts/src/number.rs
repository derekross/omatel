// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone numbers: E.164 normalization and display through the
//! `phonenumber` crate (a libphonenumber port, Apache-2.0, compatible with
//! GPL-3.0), and a loose match for numbers that don't parse, the way
//! Android's PhoneNumberUtils.compare falls back to trailing digits.

use phonenumber::country;
use phonenumber::Mode;

/// The region used for numbers written without a country code. From
/// `OMATEL_REGION`, else the locale (`LC_ALL`, `LC_TELEPHONE`, `LANG`:
/// `en_GB.UTF-8` gives GB), else US.
pub fn default_region() -> String {
    if let Ok(r) = std::env::var("OMATEL_REGION") {
        if r.len() == 2 {
            return r.to_ascii_uppercase();
        }
    }
    for var in ["LC_ALL", "LC_TELEPHONE", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            if let Some(r) = region_from_locale(&v) {
                return r;
            }
        }
    }
    "US".into()
}

fn region_from_locale(locale: &str) -> Option<String> {
    let base = locale.split(['.', '@']).next()?;
    let (_, region) = base.split_once('_')?;
    (region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| region.to_ascii_uppercase())
}

fn country_id(region: &str) -> Option<country::Id> {
    region.parse().ok()
}

/// Keep what a dial string may hold: digits, a leading +, * and #, and
/// the post-dial pause (`,`) and wait (`;`) characters.
pub fn sanitize_dial_string(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        match c {
            '0'..='9' | '*' | '#' | ',' | ';' => out.push(c),
            '+' if out.is_empty() => out.push(c),
            'p' | 'P' => out.push(','),
            'w' | 'W' => out.push(';'),
            _ => {}
        }
    }
    out
}

/// The number without its post-dial part: "+12025550100,123" -> "+12025550100".
pub fn dial_part(s: &str) -> (&str, &str) {
    match s.find([',', ';']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// E.164 form ("+12025550100") of a number, or None when it doesn't parse
/// as a phone number (a short code, an alphanumeric sender).
pub fn to_e164(raw: &str, region: &str) -> Option<String> {
    let digits = raw.chars().filter(|c| c.is_ascii_digit()).count();
    if !(5..=17).contains(&digits) || raw.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    let n = phonenumber::parse(country_id(region), raw).ok()?;
    let s = n.format().mode(Mode::E164).to_string();
    (s.len() > 4).then_some(s)
}

/// The key a conversation or call-log entry is stored under: E.164 when
/// the number parses, otherwise the sender as given (short codes and
/// alphanumeric senders), trimmed.
pub fn normalize(raw: &str, region: &str) -> String {
    to_e164(raw, region).unwrap_or_else(|| {
        let t = raw.trim();
        if t.chars().any(|c| c.is_alphabetic()) {
            t.to_string()
        } else {
            t.chars()
                .filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#'))
                .collect()
        }
    })
}

/// A number for people to read: national format for the home region,
/// international otherwise; as given when it doesn't parse.
pub fn display(raw: &str, region: &str) -> String {
    let Some(e164) = to_e164(raw, region) else {
        return raw.trim().to_string();
    };
    match phonenumber::parse(None, &e164) {
        Ok(n) => {
            let home = n.country().id() == country_id(region);
            n.format()
                .mode(if home {
                    Mode::National
                } else {
                    Mode::International
                })
                .to_string()
        }
        Err(_) => raw.trim().to_string(),
    }
}

/// The trailing digits used for the loose match.
const MIN_MATCH: usize = 7;

/// Whether two numbers name the same line: equal E.164 forms, or, when
/// either doesn't parse, the same last seven or more digits.
pub fn same_number(a: &str, b: &str, region: &str) -> bool {
    if let (Some(x), Some(y)) = (to_e164(a, region), to_e164(b, region)) {
        return x == y;
    }
    let da: Vec<char> = a.chars().filter(char::is_ascii_digit).collect();
    let db: Vec<char> = b.chars().filter(char::is_ascii_digit).collect();
    if da.is_empty() || db.is_empty() {
        return a.trim().eq_ignore_ascii_case(b.trim());
    }
    if da.len() < MIN_MATCH || db.len() < MIN_MATCH {
        return da == db;
    }
    let n = da.len().min(db.len());
    da[da.len() - n..] == db[db.len() - n..]
}

/// The loose lookup key: the last seven digits.
pub fn tail_key(raw: &str) -> String {
    let d: Vec<char> = raw.chars().filter(char::is_ascii_digit).collect();
    d[d.len().saturating_sub(MIN_MATCH)..].iter().collect()
}

/// The numbers that always count as emergency numbers (3GPP TS 22.101
/// §10.1.1: 112 and 911 always, and without a SIM also these).
pub const FIXED_EMERGENCY: &[&str] = &["112", "911", "000", "08", "110", "118", "119", "999"];

/// Whether a dial string is an emergency number, against the fixed list
/// and the modem's (SIM and network) list.
pub fn is_emergency(dial: &str, modem_list: &[String]) -> bool {
    let (number, _) = dial_part(dial);
    let digits: String = number
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    if digits.is_empty() {
        return false;
    }
    FIXED_EMERGENCY.contains(&digits.as_str()) || modem_list.iter().any(|e| e.trim() == digits)
}

/// The letters on each dialpad key (ITU E.161).
pub fn t9_digit(c: char) -> Option<char> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        '0'..='9' => c,
        'a'..='c' => '2',
        'd'..='f' => '3',
        'g'..='i' => '4',
        'j'..='l' => '5',
        'm'..='o' => '6',
        'p'..='s' => '7',
        't'..='v' => '8',
        'w'..='z' => '9',
        _ => return fold_accent(c).and_then(t9_digit),
    })
}

fn fold_accent(c: char) -> Option<char> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'a',
        'ç' | 'Ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'i',
        'ñ' | 'Ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'u',
        'ý' | 'ÿ' | 'Ý' => 'y',
        'ß' => 's',
        _ => return None,
    })
}

/// The letters printed under a dialpad digit.
pub fn t9_letters(digit: char) -> &'static str {
    match digit {
        '2' => "ABC",
        '3' => "DEF",
        '4' => "GHI",
        '5' => "JKL",
        '6' => "MNO",
        '7' => "PQRS",
        '8' => "TUV",
        '9' => "WXYZ",
        '0' => "+",
        _ => "",
    }
}

/// How well dialpad digits match a name, smart-dial style: a prefix of
/// any word (scored by how early the word is) or of the initials. None
/// when it doesn't match. Lower is better.
pub fn t9_name_score(digits: &str, name: &str) -> Option<u32> {
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let words: Vec<String> = name
        .split(|c: char| c.is_whitespace() || c == '-' || c == '.')
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().filter_map(t9_digit).collect())
        .collect();
    // A run of words: "jo sm" typed as 56 76 continues across words.
    for (i, _) in words.iter().enumerate() {
        let joined: String = words[i..].concat();
        if joined.starts_with(digits) {
            return Some(i as u32);
        }
    }
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    if digits.len() >= 2 && initials.starts_with(digits) {
        return Some(10);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_from_the_locale() {
        assert_eq!(region_from_locale("en_GB.UTF-8").as_deref(), Some("GB"));
        assert_eq!(region_from_locale("de_DE@euro").as_deref(), Some("DE"));
        assert_eq!(region_from_locale("C"), None);
        assert_eq!(region_from_locale("C.UTF-8"), None);
    }

    #[test]
    fn e164_and_display() {
        assert_eq!(
            to_e164("(202) 555-0100", "US").as_deref(),
            Some("+12025550100")
        );
        assert_eq!(
            to_e164("+1 202 555 0100", "GB").as_deref(),
            Some("+12025550100")
        );
        assert_eq!(to_e164("ACME Bank", "US"), None);
        assert_eq!(to_e164("911", "US"), None);
        assert_eq!(display("+12025550100", "US"), "(202) 555-0100");
        assert_eq!(display("+12025550100", "GB"), "+1 202-555-0100");
        assert_eq!(display("ACME", "US"), "ACME");
    }

    #[test]
    fn normalize_keeps_short_codes_and_names() {
        assert_eq!(normalize("202-555-0100", "US"), "+12025550100");
        assert_eq!(normalize(" 7726 ", "US"), "7726");
        assert_eq!(normalize("ACME Bank", "US"), "ACME Bank");
    }

    #[test]
    fn loose_match() {
        assert!(same_number("+12025550100", "(202) 555-0100", "US"));
        assert!(same_number("2025550100", "+1 202 555 0100", "US"));
        assert!(!same_number("+12025550100", "+12025550101", "US"));
        assert!(same_number("7726", "7726", "US"));
        assert!(!same_number("7726", "17726", "US"));
        assert!(same_number("ACME", "acme", "US"));
        assert_eq!(tail_key("+1 (202) 555-0100"), "5550100");
    }

    #[test]
    fn dial_strings() {
        assert_eq!(
            sanitize_dial_string("+1 (202) 555-0100 p 12#"),
            "+12025550100,12#"
        );
        assert_eq!(sanitize_dial_string("1+2"), "12");
        assert_eq!(dial_part("+12025550100,123"), ("+12025550100", ",123"));
    }

    #[test]
    fn emergency_numbers() {
        assert!(is_emergency("911", &[]));
        assert!(is_emergency("112", &[]));
        assert!(is_emergency("999", &[]));
        assert!(is_emergency("911,1", &[]));
        assert!(!is_emergency("9110", &[]));
        assert!(!is_emergency("+12025550100", &[]));
        assert!(is_emergency("117", &["117".into()]));
        assert!(!is_emergency("", &[]));
    }

    #[test]
    fn smart_dial() {
        assert_eq!(t9_name_score("25", "Alex Example"), Some(0)); // "al"
        assert_eq!(t9_name_score("39", "Alex Example"), Some(1)); // "ex"
        assert_eq!(t9_name_score("23", "Alex Example"), Some(10)); // initials
        assert_eq!(t9_name_score("2539", "Alex Example"), Some(0)); // "alex" -> "alexe"
        assert_eq!(t9_name_score("8", "Alex Example"), None);
        assert_eq!(t9_name_score("96", "Zoë"), Some(0));
        assert_eq!(t9_letters('7'), "PQRS");
    }
}
