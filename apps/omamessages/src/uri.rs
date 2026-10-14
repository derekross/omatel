// SPDX-License-Identifier: GPL-3.0-or-later
//! `sms:`, `smsto:`, `mms:` and `mmsto:` links (RFC 5724 and the forms
//! browsers and other phones use): `sms:+12025550100?body=Hello%20there`,
//! `smsto:+12025550100`, `sms:+1...,+1...` (a group: MMS comes with T3,
//! so for now the first recipient), iOS's `sms:NUMBER&body=...`.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmsUri {
    pub recipients: Vec<String>,
    pub body: Option<String>,
}

const SCHEMES: &[&str] = &["smsto:", "mmsto:", "sms:", "mms:"];

/// Whether the text is one of our links (not a plain number).
pub fn is_sms_uri(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    SCHEMES.iter().any(|p| lower.starts_with(p))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(b) = s
                .get(i + 1..i + 3)
                .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
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

/// Parse a link; None when it isn't one of our schemes.
pub fn parse(uri: &str) -> Option<SmsUri> {
    let s = uri.trim();
    let lower = s.to_ascii_lowercase();
    let scheme = SCHEMES.iter().find(|p| lower.starts_with(*p))?;
    let rest = &s[scheme.len()..];
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (who, query) = match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => match rest.find("&body=").or_else(|| rest.find(";body=")) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        },
    };
    let recipients = percent_decode(who)
        .split(',')
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    let body = query.and_then(|q| {
        q.split(['&', ';']).find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            k.eq_ignore_ascii_case("body").then(|| percent_decode(v))
        })
    });
    Some(SmsUri { recipients, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_body() {
        assert_eq!(
            parse("sms:+12025550100?body=Hello%20there"),
            Some(SmsUri {
                recipients: vec!["+12025550100".into()],
                body: Some("Hello there".into())
            })
        );
        assert_eq!(
            parse("SMSTO:+12025550101").unwrap().recipients,
            vec!["+12025550101"]
        );
        assert_eq!(parse("smsto:+12025550101").unwrap().body, None);
    }

    #[test]
    fn groups_ios_and_encoded_plus() {
        let u = parse("sms:+12025550100,%2B12025550101?body=a%26b").unwrap();
        assert_eq!(u.recipients, vec!["+12025550100", "+12025550101"]);
        assert_eq!(u.body.as_deref(), Some("a&b"));
        let u = parse("sms:+12025550102&body=On%20my%20way").unwrap();
        assert_eq!(u.recipients, vec!["+12025550102"]);
        assert_eq!(u.body.as_deref(), Some("On my way"));
        let u = parse("mmsto://+12025550103").unwrap();
        assert_eq!(u.recipients, vec!["+12025550103"]);
        let u = parse("sms:?body=just%20text").unwrap();
        assert!(u.recipients.is_empty());
        assert_eq!(u.body.as_deref(), Some("just text"));
        // A bad escape stays as written; UTF-8 escapes decode.
        assert_eq!(
            parse("sms:1?body=100%25%zz%C3%A9").unwrap().body.as_deref(),
            Some("100%%zzé")
        );
    }

    #[test]
    fn other_schemes() {
        assert_eq!(parse("tel:+12025550100"), None);
        assert!(!is_sms_uri("+12025550100"));
        assert!(is_sms_uri(" mms:+12025550100"));
    }
}
