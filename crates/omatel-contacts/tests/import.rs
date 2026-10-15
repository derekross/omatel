// SPDX-License-Identifier: GPL-3.0-or-later
//! Importing the vCards Google Contacts and Apple's Contacts (iCloud)
//! export, from synthetic fixtures that copy their quirks: groups with
//! X-ABLabel, Apple's built-in labels, folded base64 photos, `type=pref`,
//! cards without a UID, contact groups and duplicate cards.

use omatel_contacts::{AddressBook, Contact, PhoneKind};

const GOOGLE: &str = include_str!("fixtures/google-contacts.vcf");
const APPLE: &str = include_str!("fixtures/apple-icloud.vcf");

fn book() -> (tempfile::TempDir, AddressBook) {
    let dir = tempfile::tempdir().unwrap();
    let b = AddressBook::open(dir.path(), "US");
    (dir, b)
}

fn by_name<'a>(b: &'a AddressBook, name: &str) -> &'a Contact {
    b.contacts()
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no contact {name}"))
}

#[test]
fn google_export() {
    let (d, mut b) = book();
    let r = b.import(GOOGLE).unwrap();
    // Sam's second card merges into the first.
    assert_eq!((r.added, r.merged, r.skipped), (3, 1, 0));
    assert_eq!(b.contacts().len(), 3);

    let sam = by_name(&b, "Sam Sample");
    assert!(!sam.uid.is_empty(), "a UID is generated");
    let numbers: Vec<(&str, PhoneKind, &str)> = sam
        .phones
        .iter()
        .map(|p| (p.number.as_str(), p.kind, p.label.as_str()))
        .collect();
    assert_eq!(
        numbers,
        vec![
            ("+1 202-555-0110", PhoneKind::Mobile, ""),
            ("(202) 555-0111", PhoneKind::Other, "Studio"),
            ("+1 202-555-0114", PhoneKind::Work, ""),
        ]
    );
    assert_eq!(sam.emails.len(), 2);
    assert_eq!(sam.emails[0].kind, "home");
    assert_eq!(sam.note, "Met at the example conference, 2024\nLikes tea");
    assert!(sam.starred(), "Google's \"starred\" category");
    assert!(sam.has_photo, "a photo link");
    assert_eq!(b.photo(&sam.uid), None, "a link, not inline data");
    for kept in ["ADR;TYPE=HOME", "ORG:Example Org", "TITLE:Tester", "BDAY:"] {
        assert!(sam.extra.iter().any(|l| l.starts_with(kept)), "{kept}");
    }
    // The file on disk is a complete vCard khard can read.
    let text = std::fs::read_to_string(sam.path.as_ref().unwrap()).unwrap();
    assert!(text.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:"));
    assert!(text.contains("PHOTO:https://photos.example.com/sam.jpg"));
    assert!(text.contains(".X-ABLabel:Studio"));
    assert!(sam
        .path
        .as_ref()
        .unwrap()
        .starts_with(d.path().join("default")));

    let nameless = b
        .contacts()
        .iter()
        .find(|c| c.name.is_empty())
        .expect("the card with a number only");
    assert_eq!(nameless.display_name(), "+1 202-555-0115");
    assert_eq!(b.name_for("+12025550113").as_deref(), Some("Taylor Test"));

    // The same export again changes nothing.
    let r = b.import(GOOGLE).unwrap();
    assert_eq!((r.added, r.merged, r.skipped), (0, 0, 4));
    assert_eq!(b.contacts().len(), 3);
}

#[test]
fn apple_export() {
    let (_d, mut b) = book();
    let r = b.import(APPLE).unwrap();
    // Alex twice (merged), Jordan, the company; the group is skipped.
    assert_eq!((r.added, r.merged, r.skipped), (3, 1, 1));
    assert_eq!(b.contacts().len(), 3);

    let alex = by_name(&b, "Alex Example");
    let numbers: Vec<(&str, PhoneKind, String)> = alex
        .phones
        .iter()
        .map(|p| (p.number.as_str(), p.kind, p.label_text()))
        .collect();
    assert_eq!(
        numbers,
        vec![
            ("(202) 555-0100", PhoneKind::Mobile, "Mobile".to_string()),
            (
                "+1 202 555 0101",
                PhoneKind::Other,
                "Boat phone".to_string()
            ),
            ("+1 (202) 555-0102", PhoneKind::Work, "Work".to_string()),
            ("202-555-0103", PhoneKind::Fax, "Home fax".to_string()),
        ]
    );
    let emails: Vec<&str> = alex.emails.iter().map(|e| e.address.as_str()).collect();
    assert_eq!(emails, ["alex@example.com", "alex.work@example.org"]);
    assert_eq!(
        alex.note,
        "Fictional person, made up for tests\nSecond line"
    );
    // The photo stays in the file, read on demand.
    assert!(alex.has_photo);
    assert!(!alex.extra.iter().any(|l| l.starts_with("PHOTO")));
    let photo = b.photo(&alex.uid).expect("the inline photo");
    assert!(photo.starts_with(b"\x89PNG\r\n\x1a\n"));
    // Address and URL keep their groups and labels.
    assert!(alex.extra.iter().any(|l| l.ends_with(".X-ABADR:us")));
    assert!(alex
        .extra
        .iter()
        .any(|l| l.ends_with(".X-ABLabel:_$!<HomePage>!$_")));

    let jordan = by_name(&b, "Jordan Sample");
    assert_eq!(jordan.phones[0].kind, PhoneKind::Mobile);
    let shop = by_name(&b, "Example Hardware");
    assert_eq!(shop.emails[0].address, "shop@example.com");
    assert_eq!(b.name_for("2025550101").as_deref(), Some("Alex Example"));

    // An edit keeps the photo, labels and lines nobody edits here.
    let mut alex = alex.clone();
    alex.set_name("Alex Edited");
    b.save(&alex).unwrap();
    let text = std::fs::read_to_string(alex.path.as_ref().unwrap()).unwrap();
    assert!(text.contains("PHOTO;ENCODING=b;TYPE=PNG:iVBORw0KGgo"));
    assert!(text.contains(".X-ABLabel:Boat phone"));
    assert!(text.contains("BDAY;value=date:1990-01-01"));
    assert_eq!(b.photo(&alex.uid), Some(photo));

    let r = b.import(APPLE).unwrap();
    // Alex is now "Alex Edited": the name no longer matches, and Apple's
    // cards have no UID, so he comes back as a new contact. The others
    // are already there.
    assert_eq!((r.added, r.merged, r.skipped), (1, 1, 3));
}

#[test]
fn google_then_apple_merge_across_exports() {
    let (_d, mut b) = book();
    b.import(GOOGLE).unwrap();
    let apple_sam = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Sample;Sam;;;\r\nFN:Sam Sample\r\n\
item1.TEL;type=pref:+12025550110\r\nitem1.X-ABLabel:_$!<Mobile>!$_\r\n\
TEL;type=HOME:202-555-0116\r\nEMAIL;type=INTERNET:SAM@EXAMPLE.COM\r\nEND:VCARD\r\n";
    let r = b.import(apple_sam).unwrap();
    assert_eq!((r.added, r.merged, r.skipped), (0, 1, 0));
    let sam = by_name(&b, "Sam Sample");
    assert_eq!(sam.phones.len(), 4);
    assert_eq!(sam.emails.len(), 2, "the same address, other case");
}
