// SPDX-License-Identifier: GPL-3.0-or-later
//! A small vCard reader and writer: versions 2.1 (as old Android exports
//! write it, quoted-printable and all), 3.0 and 4.0 in, 3.0 out. The
//! properties the apps edit (FN, N, TEL, EMAIL, NOTE, CATEGORIES) are
//! parsed; every other line is kept verbatim and written back, so a
//! contact synced from elsewhere loses nothing when it is edited here.

use crate::{Contact, Email, Phone, PhoneKind};

/// One unfolded content line: `group.NAME;PARAM=V:value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub name: String,
    /// Parameters as (NAME, value) in upper-case names; a vCard 2.1 bare
    /// parameter (`TEL;CELL:`) becomes ("TYPE", "CELL").
    pub params: Vec<(String, String)>,
    pub value: String,
    /// The line as read, for properties we keep without parsing.
    pub raw: String,
}

impl Line {
    fn param<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.params
            .iter()
            .filter(move |(n, _)| n == name)
            .flat_map(|(_, v)| v.split(','))
    }

    fn types(&self) -> Vec<String> {
        self.param("TYPE").map(|t| t.to_ascii_uppercase()).collect()
    }
}

/// Undo line folding (RFC 6350 §3.2) and vCard 2.1 quoted-printable soft
/// line breaks.
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut qp_continue = false;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if qp_continue {
            if let Some(last) = out.last_mut() {
                last.pop(); // the soft break's '='
                last.push_str(line);
                qp_continue = line.ends_with('=');
                continue;
            }
        }
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            out.last_mut().unwrap().push_str(&line[1..]);
        } else if !line.is_empty() {
            out.push(line.to_string());
        }
        let last = out.last().map(String::as_str).unwrap_or("");
        qp_continue = last.ends_with('=') && last.to_ascii_uppercase().contains("QUOTED-PRINTABLE");
    }
    out
}

fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if c == sep && !quoted => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn parse_line(raw: &str) -> Option<Line> {
    // The value starts at the first ':' outside a quoted parameter.
    let mut quoted = false;
    let colon = raw.char_indices().find_map(|(i, c)| match c {
        '"' => {
            quoted = !quoted;
            None
        }
        ':' if !quoted => Some(i),
        _ => None,
    })?;
    let (head, value) = (&raw[..colon], &raw[colon + 1..]);
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next()?;
    let name = name.rsplit('.').next().unwrap_or(name).to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (
                k.trim().to_ascii_uppercase(),
                v.trim_matches('"').to_string(),
            ),
            None => ("TYPE".to_string(), p.trim().to_string()),
        })
        .collect::<Vec<_>>();
    let mut line = Line {
        name,
        params,
        value: value.to_string(),
        raw: raw.to_string(),
    };
    if line
        .param("ENCODING")
        .any(|e| e.eq_ignore_ascii_case("QUOTED-PRINTABLE"))
    {
        line.value = decode_qp(&line.value);
    }
    Some(line)
}

fn decode_qp(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() + 1 {
            let hex = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Unescape a text value (`\n`, `\,`, `\;`, `\\`).
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Split a structured value (N) on unescaped ';'.
fn components(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.last_mut().unwrap().push('\\');
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ';' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out.into_iter().map(|c| unescape(&c)).collect()
}

/// Split CATEGORIES on unescaped ','.
fn list(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ',' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn phone_kind(types: &[String]) -> PhoneKind {
    let has = |t: &str| types.iter().any(|x| x == t);
    if has("CELL") || has("MOBILE") || has("IPHONE") {
        PhoneKind::Mobile
    } else if has("FAX") {
        PhoneKind::Fax
    } else if has("WORK") {
        PhoneKind::Work
    } else if has("HOME") {
        PhoneKind::Home
    } else if has("MAIN") || has("PREF") && types.len() == 1 {
        PhoneKind::Main
    } else {
        PhoneKind::Other
    }
}

/// Parse every `BEGIN:VCARD ... END:VCARD` in `text`. Cards without a
/// name and without a number are skipped.
pub fn parse(text: &str) -> Vec<Contact> {
    let mut cards = Vec::new();
    let mut current: Option<Vec<Line>> = None;
    for raw in unfold(text) {
        let Some(line) = parse_line(&raw) else {
            continue;
        };
        match (
            line.name.as_str(),
            line.value.trim().to_ascii_uppercase().as_str(),
        ) {
            ("BEGIN", "VCARD") => current = Some(Vec::new()),
            ("END", "VCARD") => {
                if let Some(lines) = current.take() {
                    if let Some(c) = from_lines(lines) {
                        cards.push(c);
                    }
                }
            }
            _ => {
                if let Some(lines) = current.as_mut() {
                    lines.push(line);
                }
            }
        }
    }
    cards
}

fn from_lines(lines: Vec<Line>) -> Option<Contact> {
    let mut c = Contact::default();
    let mut n_value: Option<Vec<String>> = None;
    for line in lines {
        match line.name.as_str() {
            "VERSION" | "PRODID" | "REV" => {}
            "UID" => c.uid = line.value.trim().to_string(),
            "FN" => c.name = unescape(&line.value).trim().to_string(),
            "N" => {
                n_value = Some(components(&line.value));
                c.n_raw = Some(line.value.clone());
            }
            "TEL" => {
                let number = line
                    .value
                    .trim()
                    .trim_start_matches("tel:")
                    .trim()
                    .to_string();
                if !number.is_empty() {
                    let types = line.types();
                    c.phones.push(Phone {
                        number,
                        kind: phone_kind(&types),
                    });
                }
            }
            "EMAIL" => {
                let address = unescape(&line.value).trim().to_string();
                if !address.is_empty() {
                    let types = line.types();
                    c.emails.push(Email {
                        address,
                        kind: if types.iter().any(|t| t == "WORK") {
                            "work".into()
                        } else if types.iter().any(|t| t == "HOME") {
                            "home".into()
                        } else {
                            String::new()
                        },
                    });
                }
            }
            "NOTE" => {
                if !c.note.is_empty() {
                    c.note.push('\n');
                }
                c.note.push_str(&unescape(&line.value));
            }
            "CATEGORIES" => c.categories.extend(list(&line.value)),
            "X-OMATEL-STARRED" | "X-STARRED" => {
                if matches!(line.value.trim(), "1" | "TRUE" | "true" | "yes") {
                    c.categories.push(crate::STARRED.into());
                }
            }
            _ => c.extra.push(line.raw),
        }
    }
    if c.name.is_empty() {
        if let Some(n) = &n_value {
            // family;given;additional;prefix;suffix
            let get = |i: usize| n.get(i).map(String::as_str).unwrap_or("").trim();
            c.name = [get(3), get(1), get(2), get(0), get(4)]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    if c.name.is_empty() {
        c.name = c
            .emails
            .first()
            .map(|e| e.address.clone())
            .unwrap_or_default();
    }
    if c.name.is_empty() && c.phones.is_empty() {
        return None;
    }
    Some(c)
}

/// Fold a content line at 75 octets, never inside a UTF-8 sequence.
fn fold(line: &str, out: &mut String) {
    let mut width = 0;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += len;
    }
    out.push_str("\r\n");
}

/// Write a contact as vCard 3.0.
pub fn write(c: &Contact) -> String {
    let mut out = String::new();
    let mut put = |l: String| fold(&l, &mut out);
    put("BEGIN:VCARD".into());
    put("VERSION:3.0".into());
    put(format!("UID:{}", c.uid));
    put(format!("FN:{}", escape(&c.name)));
    match &c.n_raw {
        Some(n) if !c.name_changed => put(format!("N:{n}")),
        _ => {
            let (given, family) = match c.name.rsplit_once(' ') {
                Some((g, f)) => (g.trim(), f.trim()),
                None => (c.name.trim(), ""),
            };
            put(format!("N:{};{};;;", escape(family), escape(given)));
        }
    }
    for p in &c.phones {
        let ty = match p.kind {
            PhoneKind::Mobile => "CELL",
            PhoneKind::Home => "HOME",
            PhoneKind::Work => "WORK",
            PhoneKind::Main => "MAIN",
            PhoneKind::Fax => "FAX",
            PhoneKind::Other => "VOICE",
        };
        put(format!(
            "TEL;TYPE={ty}:{}",
            p.number.replace(['\r', '\n'], "")
        ));
    }
    for e in &c.emails {
        match e.kind.as_str() {
            "" => put(format!("EMAIL;TYPE=INTERNET:{}", escape(&e.address))),
            k => put(format!(
                "EMAIL;TYPE=INTERNET,{}:{}",
                k.to_ascii_uppercase(),
                escape(&e.address)
            )),
        }
    }
    if !c.note.is_empty() {
        put(format!("NOTE:{}", escape(&c.note)));
    }
    if !c.categories.is_empty() {
        let cats: Vec<String> = c.categories.iter().map(|s| escape(s)).collect();
        put(format!("CATEGORIES:{}", cats.join(",")));
    }
    for raw in &c.extra {
        put(raw.clone());
    }
    put("END:VCARD".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_v3_with_folding_and_escapes() {
        let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:abc\r\nFN:Alex Example\r\nN:Example;Alex;;;\r\n\
TEL;TYPE=CELL:+1 202-555-0100\r\nTEL;TYPE=WORK,VOICE:(202) 555-0101\r\nEMAIL;TYPE=INTERNET:alex@ex\r\n ample.org\r\n\
NOTE:line one\\nline two\\, with comma\r\nCATEGORIES:Friends,Starred\r\nX-CUSTOM;FOO=bar:kept\r\nEND:VCARD\r\n";
        let cards = parse(text);
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.uid, "abc");
        assert_eq!(c.name, "Alex Example");
        assert_eq!(c.phones.len(), 2);
        assert_eq!(c.phones[0].kind, PhoneKind::Mobile);
        assert_eq!(c.phones[1].kind, PhoneKind::Work);
        assert_eq!(c.emails[0].address, "alex@example.org");
        assert_eq!(c.note, "line one\nline two, with comma");
        assert!(c.starred());
        assert_eq!(c.extra, vec!["X-CUSTOM;FOO=bar:kept".to_string()]);
    }

    #[test]
    fn reads_android_v21_quoted_printable() {
        let text = "BEGIN:VCARD\nVERSION:2.1\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:Exampl=C3=A9;Zo=C3=\n=AB;;;\nTEL;CELL:202-555-0102\nTEL;HOME:202-555-0103\nEND:VCARD\n";
        let cards = parse(text);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].name, "Zoë Examplé");
        assert_eq!(cards[0].phones[0].kind, PhoneKind::Mobile);
        assert_eq!(cards[0].phones[1].kind, PhoneKind::Home);
    }

    #[test]
    fn several_cards_and_nameless_ones() {
        let text = "BEGIN:VCARD\nVERSION:4.0\nFN:One\nEND:VCARD\nBEGIN:VCARD\nVERSION:4.0\nTEL;VALUE=uri:tel:+12025550104\nEND:VCARD\nBEGIN:VCARD\nVERSION:4.0\nEND:VCARD\n";
        let cards = parse(text);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[1].phones[0].number, "+12025550104");
        assert_eq!(cards[1].name, "");
    }

    #[test]
    fn round_trips_and_keeps_unknown_lines() {
        let text = "BEGIN:VCARD\nVERSION:3.0\nUID:u1\nFN:Sam Sample\nN:Sample;Sam;;;\nTEL;TYPE=CELL:+12025550105\nPHOTO;ENCODING=b;TYPE=JPEG:AAAA\nitem1.X-ABLABEL:custom\nEND:VCARD\n";
        let c = &parse(text)[0];
        let written = write(c);
        assert!(written.contains("PHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\n"));
        assert!(written.contains("item1.X-ABLABEL:custom\r\n"));
        assert!(written.contains("N:Sample;Sam;;;\r\n"));
        let again = &parse(&written)[0];
        assert_eq!(again.name, c.name);
        assert_eq!(again.phones, c.phones);
        assert_eq!(again.extra, c.extra);
    }

    #[test]
    fn folds_long_lines_on_char_boundaries() {
        let mut c = Contact {
            uid: "u".into(),
            name: "Ö".repeat(60),
            ..Contact::default()
        };
        c.name_changed = true;
        let w = write(&c);
        for line in w.split("\r\n") {
            assert!(line.len() <= 75, "{line}");
        }
        assert_eq!(parse(&w)[0].name, c.name);
    }

    #[test]
    fn escape_roundtrip() {
        let s = "a,b;c\\d\ne";
        assert_eq!(unescape(&escape(s)), s);
    }
}
