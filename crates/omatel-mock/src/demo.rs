// SPDX-License-Identifier: GPL-3.0-or-later
//! Fictional demo data for screenshots and trying the apps on a laptop:
//! contacts, a call log and conversations. Every name is made up and every
//! number is in the 555-01xx range reserved for fiction.

use std::path::Path;

use omatel_contacts::{AddressBook, Contact, Email, Phone, PhoneKind};
use omatel_store::{
    CallDirection, CallLog, CallOutcome, CallRecord, MessageState, MessageStore, NewIncoming,
};

pub const PEOPLE: &[(&str, &str, bool)] = &[
    ("Alex Example", "+12025550100", true),
    ("Sam Sample", "+12025550101", true),
    ("Jordan Placeholder", "+12025550102", false),
    ("Riley Testcase", "+12025550103", true),
    ("Casey Fictional", "+12025550104", false),
    ("Morgan Demo", "+12025550105", false),
    ("Taylor Mockwell", "+12025550106", false),
    ("Avery Sampleton", "+12025550107", false),
    ("Dana Notreal", "+12025550108", false),
    ("Jamie Example", "+12025550109", false),
];

/// An unknown caller (not in the contacts).
pub const STRANGER: &str = "+12025550150";

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

/// Write the demo data under `data_home` (`$XDG_DATA_HOME`): contacts in
/// `contacts/default/`, stores in `omatel/`. `now` is ms since the epoch.
pub fn seed(data_home: &Path, now: i64) -> anyhow::Result<()> {
    let mut book = AddressBook::open(data_home.join("contacts"), "US");
    for (i, (name, number, starred)) in PEOPLE.iter().enumerate() {
        let mut c = Contact::new(name);
        c.uid = format!("demo-{i}");
        c.phones.push(Phone {
            number: number.to_string(),
            kind: PhoneKind::Mobile,
            label: String::new(),
        });
        if i == 0 {
            c.phones.push(Phone {
                number: "+12025550110".into(),
                kind: PhoneKind::Work,
                label: String::new(),
            });
            c.emails.push(Email {
                address: "alex@example.org".into(),
                kind: "home".into(),
                label: String::new(),
            });
            c.note = "Fictional person for the demo.".into();
        }
        c.set_starred(*starred);
        book.save(&c)?;
    }

    let dir = data_home.join("omatel");
    let log = CallLog::open(&dir.join("calls.db"))?;
    let call = |n: &str, d: CallDirection, o: CallOutcome, ago: i64, secs: i64, seen: bool| {
        let start = now - ago;
        CallRecord {
            id: 0,
            number: n.into(),
            raw_number: n.into(),
            direction: d,
            outcome: o,
            started_at: start,
            answered_at: (o == CallOutcome::Answered).then_some(start + 4000),
            ended_at: Some(start + 4000 + secs * 1000),
            duration: secs,
            seen,
        }
    };
    use CallDirection::*;
    use CallOutcome::*;
    for c in [
        call(PEOPLE[1].1, In, Missed, 12 * MIN, 0, false),
        call(STRANGER, In, Missed, 2 * HOUR, 0, true),
        call(PEOPLE[0].1, Out, Answered, 5 * HOUR, 312, true),
        call(PEOPLE[3].1, In, Answered, DAY + 3 * HOUR, 75, true),
        call(PEOPLE[2].1, Out, Unanswered, DAY + 5 * HOUR, 0, true),
        call(PEOPLE[4].1, In, Rejected, 2 * DAY, 0, true),
        call(PEOPLE[0].1, In, Answered, 3 * DAY, 1260, true),
        call(PEOPLE[5].1, Out, Answered, 4 * DAY, 48, true),
    ] {
        log.insert(&c)?;
    }

    let store = MessageStore::open(&dir.join("messages.db"))?;
    let incoming = |n: &str, body: &str, at: i64| -> anyhow::Result<i64> {
        let (conv, id) = store
            .insert_incoming(&NewIncoming {
                address: n,
                raw_number: n,
                body,
                sent_at: Some(at),
                fallback_key: "",
                received_at: Some(at),
            })?
            .expect("fresh");
        let _ = id;
        Ok(conv)
    };
    let out = |conv: i64, body: &str, state: MessageState, at: i64| -> anyhow::Result<()> {
        let id = store.insert_outgoing_at(conv, body, at)?;
        store.set_state(id, state, None)?;
        Ok(())
    };
    let a = incoming(
        PEOPLE[3].1,
        "Are we still on for the hike on Saturday?",
        now - 3 * DAY,
    )?;
    out(
        a,
        "Yes! Meet at the trailhead at 9.",
        MessageState::Delivered,
        now - 3 * DAY + 20 * MIN,
    )?;
    store.mark_read(a)?;
    let b = incoming(PEOPLE[2].1, "The parcel arrived, thanks.", now - 2 * DAY)?;
    store.mark_read(b)?;
    store.set_archived(b, true)?;
    let c = store.conversation_for(PEOPLE[4].1, &[PEOPLE[4].1])?;
    out(
        c,
        "Running ten minutes late, sorry",
        MessageState::Failed,
        now - DAY,
    )?;
    let d = incoming(PEOPLE[0].1, "Dinner at seven?", now - 5 * HOUR)?;
    out(
        d,
        "Seven works. Where?",
        MessageState::Delivered,
        now - 5 * HOUR + 2 * MIN,
    )?;
    incoming(PEOPLE[0].1, "The usual place 🍜", now - 4 * HOUR)?;
    out(d, "See you there", MessageState::Sent, now - 4 * HOUR + MIN)?;
    store.mark_read(d)?;
    store.set_pinned(d, true)?;
    let e = incoming(
        STRANGER,
        "Your code for Example Service is 123456",
        now - HOUR,
    )?;
    store.set_muted(e, true)?;
    let f = incoming(PEOPLE[1].1, "Call me when you get a moment", now - 15 * MIN)?;
    incoming(PEOPLE[1].1, "No rush", now - 14 * MIN)?;
    let _ = f;
    let g = store.conversation_for(PEOPLE[5].1, &[PEOPLE[5].1])?;
    store.set_draft(g, "Happy birthday! 🎂")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn seeds_into_a_temporary_home() {
        let d = tempfile::tempdir().unwrap();
        super::seed(d.path(), 1_800_000_000_000).unwrap();
        let book = omatel_contacts::AddressBook::open(d.path().join("contacts"), "US");
        assert_eq!(book.contacts().len(), super::PEOPLE.len());
        let store = omatel_store::MessageStore::open(&d.path().join("omatel/messages.db")).unwrap();
        assert!(store.conversations(false).unwrap().len() >= 5);
        assert_eq!(store.conversations(true).unwrap().len(), 1);
        let log = omatel_store::CallLog::open(&d.path().join("omatel/calls.db")).unwrap();
        assert_eq!(log.unseen_missed().unwrap(), 1);
    }
}
