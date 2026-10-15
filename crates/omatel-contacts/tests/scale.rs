// SPDX-License-Identifier: GPL-3.0-or-later
//! A book of 5000 generated contacts: how long loading takes (printed,
//! never asserted, so a slow machine doesn't fail it), and that lookups,
//! search and incremental reloads stay correct at that size.
//!
//! `cargo test -p omatel-contacts --release --test scale -- --nocapture`
//! shows the times.

use std::time::Instant;

use omatel_contacts::{vcard, AddressBook, Contact, Phone, PhoneKind};

const N: usize = 5000;

/// Fictional numbers only: +1 NPA 555-01xx, 50 area codes of 100 each.
/// They share their last seven digits across area codes, so lookups go
/// through the full comparison, not just the index.
fn number(i: usize) -> String {
    format!("+1 {} 555 01{:02}", 201 + i / 100, i % 100)
}

fn person(i: usize) -> Contact {
    let mut c = Contact::new(&format!("Person{i:04} Example"));
    c.uid = format!("scale-{i}");
    c.phones.push(Phone {
        number: number(i),
        kind: PhoneKind::Mobile,
        label: String::new(),
    });
    if i.is_multiple_of(7) {
        // Some cards carry a photo, as synced cards do.
        c.extra
            .push(format!("PHOTO;ENCODING=b;TYPE=PNG:{}", "QUFB".repeat(2000)));
        c.has_photo = true;
    }
    c
}

#[test]
fn five_thousand_contacts() {
    let dir = tempfile::tempdir().unwrap();
    let books = ["default", "work", "synced"];
    for (i, b) in books.iter().enumerate() {
        let d = dir.path().join(b);
        std::fs::create_dir_all(&d).unwrap();
        for k in (i..N).step_by(books.len()) {
            std::fs::write(d.join(format!("scale-{k}.vcf")), vcard::write(&person(k))).unwrap();
        }
    }

    let t = Instant::now();
    let mut book = AddressBook::open(dir.path(), "US");
    let load = t.elapsed();
    assert_eq!(book.contacts().len(), N);

    let t = Instant::now();
    for i in 0..N {
        let m = book.lookup(&number(i)).expect("every number is found");
        assert_eq!(m.uid, format!("scale-{i}"));
    }
    let lookups = t.elapsed();
    // Other spellings of the same number; numbers nobody has.
    assert_eq!(book.lookup("(201) 555-0100").unwrap().uid, "scale-0");
    assert_eq!(book.lookup("+12505550199").unwrap().uid, "scale-4999");
    assert!(book.lookup("+12515550100").is_none());
    assert!(book.lookup("+442079460000").is_none());
    assert_eq!(book.get("scale-1234").unwrap().name, "Person1234 Example");
    assert_eq!(book.search("person0042").len(), 1);

    // Photos are flagged, not held in memory.
    assert!(book.get("scale-7").unwrap().has_photo);
    assert!(book
        .contacts()
        .iter()
        .all(|c| !c.extra.iter().any(|l| l.starts_with("PHOTO"))));
    assert_eq!(book.photo("scale-7").unwrap().len(), 6000);

    let t = Instant::now();
    assert!(!book.reload_if_changed());
    let idle = t.elapsed();

    // One edit: only that card changes.
    let path = dir.path().join("work/scale-1.vcf");
    let mut one = book.get("scale-1").unwrap().clone();
    one.set_name("Renamed Example");
    std::fs::write(&path, vcard::write(&one)).unwrap();
    let t = Instant::now();
    assert!(book.reload_if_changed());
    let one_edit = t.elapsed();
    assert_eq!(
        book.name_for(&number(1)).as_deref(),
        Some("Renamed Example")
    );
    assert_eq!(book.contacts().len(), N);

    println!(
        "{N} contacts: load {load:?}, {N} lookups {lookups:?}, \
         unchanged check {idle:?}, reload after one edit {one_edit:?}"
    );
}
