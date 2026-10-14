// SPDX-License-Identifier: GPL-3.0-or-later
//! The address book Phone and Messages share: a vCard folder in the vdir
//! layout (one `.vcf` per contact), `~/.local/share/contacts/<book>/`,
//! which a vdir sync tool (vdirsyncer, pimsync) can keep in step with a
//! CardDAV server. Every book under the root is read; new contacts go into
//! the `default` book. Files are written atomically (a temporary file,
//! fsync, rename), so a sync tool never sees half a card.
//!
//! Nothing here logs a name or a number.

pub mod number;
pub mod vcard;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The category that marks a favourite (starred) contact.
pub const STARRED: &str = "Starred";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhoneKind {
    Mobile,
    Home,
    Work,
    Main,
    Fax,
    Other,
}

impl PhoneKind {
    pub const ALL: [PhoneKind; 6] = [
        PhoneKind::Mobile,
        PhoneKind::Home,
        PhoneKind::Work,
        PhoneKind::Main,
        PhoneKind::Fax,
        PhoneKind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PhoneKind::Mobile => "Mobile",
            PhoneKind::Home => "Home",
            PhoneKind::Work => "Work",
            PhoneKind::Main => "Main",
            PhoneKind::Fax => "Fax",
            PhoneKind::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    pub number: String,
    pub kind: PhoneKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    pub address: String,
    /// "home", "work" or empty.
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contact {
    pub uid: String,
    pub name: String,
    pub phones: Vec<Phone>,
    pub emails: Vec<Email>,
    pub note: String,
    pub categories: Vec<String>,
    /// Lines we don't edit (PHOTO, ADR, ORG, X-...), written back as read.
    pub extra: Vec<String>,
    /// The N value as read; rewritten from the name once that changes.
    pub n_raw: Option<String>,
    pub name_changed: bool,
    /// The file the contact came from (None for a new contact).
    pub path: Option<PathBuf>,
}

impl Contact {
    pub fn new(name: &str) -> Contact {
        Contact {
            uid: new_uid(),
            name: name.trim().to_string(),
            name_changed: true,
            ..Contact::default()
        }
    }

    pub fn set_name(&mut self, name: &str) {
        if self.name != name.trim() {
            self.name = name.trim().to_string();
            self.name_changed = true;
        }
    }

    pub fn starred(&self) -> bool {
        self.categories.iter().any(|c| {
            c.eq_ignore_ascii_case(STARRED)
                || c.eq_ignore_ascii_case("favorites")
                || c.eq_ignore_ascii_case("favourites")
        })
    }

    pub fn set_starred(&mut self, on: bool) {
        if on == self.starred() {
            return;
        }
        if on {
            self.categories.push(STARRED.into());
        } else {
            self.categories.retain(|c| {
                !(c.eq_ignore_ascii_case(STARRED)
                    || c.eq_ignore_ascii_case("favorites")
                    || c.eq_ignore_ascii_case("favourites"))
            });
        }
    }

    /// The name to show: the name, or the first number.
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.phones
                .first()
                .map(|p| p.number.clone())
                .unwrap_or_default()
        }
    }

    /// The contact's photo as bytes, when the card carries one inline
    /// (`PHOTO;ENCODING=b:` or a `data:` URI).
    pub fn photo_bytes(&self) -> Option<Vec<u8>> {
        let line = self
            .extra
            .iter()
            .find(|l| l.to_ascii_uppercase().starts_with("PHOTO"))?;
        let (_, value) = line.split_once(':')?;
        let b64 = if let Some(rest) = value.strip_prefix("data:") {
            rest.split_once("base64,")?.1
        } else {
            value
        };
        base64_decode(b64)
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\r' | b'\n' | b'\t' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A random-enough UID for a new card (time and process, no MAC address).
pub fn new_uid() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    // Mix in the address of a stack variable for a little extra entropy.
    let local = 0u8;
    let addr = &local as *const u8 as usize as u64;
    format!(
        "omatel-{:x}-{:x}-{:x}-{:x}",
        t.as_secs(),
        t.subsec_nanos(),
        std::process::id() as u64 ^ (addr >> 4),
        seq
    )
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("no such contact")]
    NotFound,
}

/// A contact found for a number, for call logs and conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub uid: String,
    pub name: String,
    pub kind: PhoneKind,
}

/// A dialpad suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub uid: String,
    pub name: String,
    pub number: String,
    pub kind: PhoneKind,
}

pub struct AddressBook {
    root: PathBuf,
    default_book: String,
    region: String,
    contacts: Vec<Contact>,
    /// tail_key (last 7 digits) -> indices into contacts.
    by_tail: HashMap<String, Vec<usize>>,
    stamp: Vec<(PathBuf, Option<SystemTime>)>,
}

/// `$XDG_DATA_HOME/contacts`, or `~/.local/share/contacts`.
pub fn default_root() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("contacts");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    Path::new(&home).join(".local/share/contacts")
}

impl AddressBook {
    /// Open the books under `root` (created on the first save).
    pub fn open(root: impl Into<PathBuf>, region: &str) -> AddressBook {
        let mut b = AddressBook {
            root: root.into(),
            default_book: "default".into(),
            region: region.to_string(),
            contacts: Vec::new(),
            by_tail: HashMap::new(),
            stamp: Vec::new(),
        };
        b.reload();
        b
    }

    pub fn open_default() -> AddressBook {
        AddressBook::open(default_root(), &number::default_region())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    fn book_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs
    }

    fn current_stamp(&self) -> Vec<(PathBuf, Option<SystemTime>)> {
        let mut stamp = vec![(
            self.root.clone(),
            fs::metadata(&self.root).and_then(|m| m.modified()).ok(),
        )];
        for d in self.book_dirs() {
            let m = fs::metadata(&d).and_then(|m| m.modified()).ok();
            stamp.push((d, m));
        }
        stamp
    }

    /// Read every book again.
    pub fn reload(&mut self) {
        let mut contacts = Vec::new();
        for dir in self.book_dirs() {
            let mut files: Vec<PathBuf> = fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vcf")))
                .collect();
            files.sort();
            for f in files {
                match fs::read_to_string(&f) {
                    Ok(text) => {
                        // One card per file in a vdir; take the first.
                        if let Some(mut c) = vcard::parse(&text).into_iter().next() {
                            if c.uid.is_empty() {
                                c.uid = f
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                            }
                            c.path = Some(f);
                            contacts.push(c);
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "unreadable contact file"),
                }
            }
        }
        contacts.sort_by_key(|c| c.display_name().to_lowercase());
        self.contacts = contacts;
        self.index();
        self.stamp = self.current_stamp();
        tracing::debug!(contacts = self.contacts.len(), "address book loaded");
    }

    /// Reload when a book directory changed (a file added, removed or
    /// renamed, as sync tools and our own saves do). True if it did.
    pub fn reload_if_changed(&mut self) -> bool {
        if self.current_stamp() != self.stamp {
            self.reload();
            true
        } else {
            false
        }
    }

    fn index(&mut self) {
        self.by_tail.clear();
        for (i, c) in self.contacts.iter().enumerate() {
            for p in &c.phones {
                let key = number::tail_key(&p.number);
                if !key.is_empty() {
                    self.by_tail.entry(key).or_default().push(i);
                }
            }
        }
    }

    pub fn contacts(&self) -> &[Contact] {
        &self.contacts
    }

    pub fn get(&self, uid: &str) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.uid == uid)
    }

    pub fn starred(&self) -> Vec<&Contact> {
        self.contacts.iter().filter(|c| c.starred()).collect()
    }

    /// The contact that owns a number.
    pub fn lookup(&self, raw: &str) -> Option<Match> {
        let key = number::tail_key(raw);
        let candidates = self.by_tail.get(&key)?;
        for &i in candidates {
            let c = &self.contacts[i];
            for p in &c.phones {
                if number::same_number(&p.number, raw, &self.region) {
                    return Some(Match {
                        uid: c.uid.clone(),
                        name: c.display_name(),
                        kind: p.kind,
                    });
                }
            }
        }
        None
    }

    /// The name for a number, if a contact has it.
    pub fn name_for(&self, raw: &str) -> Option<String> {
        self.lookup(raw).map(|m| m.name)
    }

    /// Contacts whose name, number, or email contains the query
    /// (case-insensitive; digits match numbers ignoring punctuation).
    pub fn search(&self, query: &str) -> Vec<&Contact> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.contacts.iter().collect();
        }
        let qd: String = q.chars().filter(char::is_ascii_digit).collect();
        self.contacts
            .iter()
            .filter(|c| {
                c.name.to_lowercase().contains(&q)
                    || c.emails
                        .iter()
                        .any(|e| e.address.to_lowercase().contains(&q))
                    || (qd.len() >= 2
                        && c.phones.iter().any(|p| {
                            p.number
                                .chars()
                                .filter(char::is_ascii_digit)
                                .collect::<String>()
                                .contains(&qd)
                        }))
            })
            .collect()
    }

    /// Smart-dial suggestions for digits typed on the dialpad: names that
    /// match by T9, then numbers that contain the digits. Best first.
    pub fn smart_dial(&self, digits: &str, limit: usize) -> Vec<Suggestion> {
        let digits: String = digits.chars().filter(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(u32, usize, &Contact, &Phone)> = Vec::new();
        for c in &self.contacts {
            let name_score = number::t9_name_score(&digits, &c.name);
            for p in &c.phones {
                let pd: String = p.number.chars().filter(char::is_ascii_digit).collect();
                let num_score = pd.find(&digits).map(|pos| 20 + pos as u32);
                if let Some(s) = name_score.into_iter().chain(num_score).min() {
                    scored.push((s, scored.len(), c, p));
                }
            }
        }
        scored.sort_by_key(|(s, order, c, _)| (*s, !c.starred(), *order));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, _, c, p)| Suggestion {
                uid: c.uid.clone(),
                name: c.display_name(),
                number: p.number.clone(),
                kind: p.kind,
            })
            .collect()
    }

    /// Save a contact (new or edited) to its file, atomically.
    pub fn save(&mut self, contact: &Contact) -> Result<(), Error> {
        let mut c = contact.clone();
        if c.uid.is_empty() {
            c.uid = new_uid();
        }
        let path = match &c.path {
            Some(p) => p.clone(),
            None => {
                let dir = self.root.join(&self.default_book);
                fs::create_dir_all(&dir)?;
                dir.join(format!("{}.vcf", file_safe(&c.uid)))
            }
        };
        write_atomic(&path, vcard::write(&c).as_bytes())?;
        c.path = Some(path);
        c.name_changed = false;
        self.reload();
        Ok(())
    }

    pub fn delete(&mut self, uid: &str) -> Result<(), Error> {
        let c = self.get(uid).ok_or(Error::NotFound)?;
        if let Some(p) = &c.path {
            fs::remove_file(p)?;
        }
        self.reload();
        Ok(())
    }

    /// Import every card in a `.vcf` (one or many contacts, as Android's
    /// export writes them). Cards whose UID is already present replace
    /// nothing and are skipped. Returns how many were added.
    pub fn import(&mut self, text: &str) -> Result<usize, Error> {
        let cards = vcard::parse(text);
        let dir = self.root.join(&self.default_book);
        fs::create_dir_all(&dir)?;
        let mut added = 0;
        for mut c in cards {
            if !c.uid.is_empty() && self.get(&c.uid).is_some() {
                continue;
            }
            if c.uid.is_empty() {
                c.uid = new_uid();
            }
            let path = dir.join(format!("{}.vcf", file_safe(&c.uid)));
            if path.exists() {
                continue;
            }
            write_atomic(&path, vcard::write(&c).as_bytes())?;
            added += 1;
        }
        self.reload();
        Ok(added)
    }

    /// All contacts as one `.vcf`.
    pub fn export(&self) -> String {
        self.contacts.iter().map(vcard::write).collect()
    }
}

fn file_safe(uid: &str) -> String {
    uid.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> (tempfile::TempDir, AddressBook) {
        let dir = tempfile::tempdir().unwrap();
        let b = AddressBook::open(dir.path(), "US");
        (dir, b)
    }

    fn contact(name: &str, numbers: &[&str]) -> Contact {
        let mut c = Contact::new(name);
        for n in numbers {
            c.phones.push(Phone {
                number: n.to_string(),
                kind: PhoneKind::Mobile,
            });
        }
        c
    }

    #[test]
    fn save_lookup_and_delete() {
        let (_d, mut b) = book();
        b.save(&contact("Alex Example", &["(202) 555-0100"]))
            .unwrap();
        b.save(&contact(
            "Sam Sample",
            &["+1 202 555 0101", "+44 20 7946 0000"],
        ))
        .unwrap();
        assert_eq!(b.contacts().len(), 2);
        assert_eq!(b.name_for("+12025550100").as_deref(), Some("Alex Example"));
        assert_eq!(b.name_for("2025550101").as_deref(), Some("Sam Sample"));
        assert_eq!(b.name_for("+12025550199"), None);
        let uid = b.contacts()[0].uid.clone();
        b.delete(&uid).unwrap();
        assert_eq!(b.contacts().len(), 1);
    }

    #[test]
    fn edit_keeps_file_and_unknown_lines() {
        let (d, mut b) = book();
        fs::create_dir_all(d.path().join("synced")).unwrap();
        fs::write(
            d.path().join("synced/x.vcf"),
            "BEGIN:VCARD\nVERSION:3.0\nUID:x1\nFN:Old Name\nORG:Example Org\nTEL:+12025550102\nEND:VCARD\n",
        )
        .unwrap();
        b.reload();
        let mut c = b.get("x1").unwrap().clone();
        c.set_name("New Name");
        c.set_starred(true);
        b.save(&c).unwrap();
        let text = fs::read_to_string(d.path().join("synced/x.vcf")).unwrap();
        assert!(text.contains("FN:New Name"));
        assert!(text.contains("ORG:Example Org"));
        assert!(text.contains("CATEGORIES:Starred"));
        assert_eq!(b.starred().len(), 1);
    }

    #[test]
    fn import_many_and_skip_known() {
        let (_d, mut b) = book();
        let vcf = "BEGIN:VCARD\nVERSION:3.0\nUID:a\nFN:A Person\nTEL:202-555-0110\nEND:VCARD\n\
BEGIN:VCARD\nVERSION:2.1\nN:Person;B;;;\nTEL;CELL:202-555-0111\nEND:VCARD\n";
        assert_eq!(b.import(vcf).unwrap(), 2);
        assert_eq!(b.import(vcf).unwrap(), 1); // the UID-less card again
        assert_eq!(b.contacts().len(), 3);
        assert!(b.export().matches("BEGIN:VCARD").count() == 3);
    }

    #[test]
    fn search_and_smart_dial() {
        let (_d, mut b) = book();
        b.save(&contact("Alex Example", &["202-555-0100"])).unwrap();
        b.save(&contact("Jo Smith", &["202-555-0120"])).unwrap();
        let mut star = contact("Jo Sample", &["202-555-0130"]);
        star.set_starred(true);
        b.save(&star).unwrap();
        assert_eq!(b.search("alex").len(), 1);
        assert_eq!(b.search("555-01").len(), 3);
        assert_eq!(b.search("").len(), 3);
        let s = b.smart_dial("56", 5); // "jo"
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "Jo Sample"); // starred first on a tie
        let s = b.smart_dial("0120", 5);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Jo Smith");
        assert!(b.smart_dial("", 5).is_empty());
    }

    #[test]
    fn reload_if_changed_sees_new_files() {
        let (d, mut b) = book();
        assert!(!b.reload_if_changed());
        fs::create_dir_all(d.path().join("default")).unwrap();
        fs::write(
            d.path().join("default/n.vcf"),
            "BEGIN:VCARD\nVERSION:3.0\nFN:New\nEND:VCARD\n",
        )
        .unwrap();
        assert!(b.reload_if_changed());
        assert_eq!(b.contacts().len(), 1);
    }

    #[test]
    fn photo_from_inline_base64() {
        let c = &vcard::parse(
            "BEGIN:VCARD\nVERSION:3.0\nFN:P\nPHOTO;ENCODING=b;TYPE=PNG:aGVsbG8=\nEND:VCARD\n",
        )[0];
        assert_eq!(c.photo_bytes().unwrap(), b"hello");
        let c = &vcard::parse(
            "BEGIN:VCARD\nVERSION:4.0\nFN:P\nPHOTO:data:image/png;base64,aGVsbG8=\nEND:VCARD\n",
        )[0];
        assert_eq!(c.photo_bytes().unwrap(), b"hello");
    }
}
