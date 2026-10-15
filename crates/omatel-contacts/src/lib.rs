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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The category that marks a favorite (starred) contact.
pub const STARRED: &str = "Starred";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PhoneKind {
    Mobile,
    Home,
    Work,
    Main,
    Fax,
    #[default]
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Phone {
    pub number: String,
    pub kind: PhoneKind,
    /// A custom label (Google's and Apple's `X-ABLabel`), shown instead of
    /// the kind when set; empty otherwise.
    pub label: String,
}

impl Phone {
    /// The label to show: the custom one, or the kind's.
    pub fn label_text(&self) -> String {
        if self.label.is_empty() {
            self.kind.label().to_string()
        } else {
            vcard::label_text(&self.label)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Email {
    pub address: String,
    /// "home", "work" or empty.
    pub kind: String,
    /// A custom label (`X-ABLabel`), empty when there is none.
    pub label: String,
}

/// A contact file as it was read: its modification time, size and inode.
/// A sync tool or an editor that rewrites the file changes at least one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    modified: Option<SystemTime>,
    size: u64,
    inode: u64,
}

impl FileStamp {
    fn from_metadata(m: &fs::Metadata) -> FileStamp {
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(m);
        #[cfg(not(unix))]
        let inode = 0;
        FileStamp {
            modified: m.modified().ok(),
            size: m.len(),
            inode,
        }
    }

    /// The file's stamp now; None when it is gone.
    pub fn of(path: &Path) -> Option<FileStamp> {
        fs::metadata(path)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| FileStamp::from_metadata(&m))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contact {
    pub uid: String,
    pub name: String,
    pub phones: Vec<Phone>,
    pub emails: Vec<Email>,
    pub note: String,
    pub categories: Vec<String>,
    /// Lines we don't edit (ADR, ORG, X-...), written back as read. A
    /// contact from the address book carries no PHOTO line here (see
    /// `has_photo`); one parsed with [`vcard::parse`] does.
    pub extra: Vec<String>,
    /// Whether the card has a PHOTO. The book keeps the photo in the file
    /// and writes it back on save; [`AddressBook::photo`] reads it.
    pub has_photo: bool,
    /// The N value as read; rewritten from the name once that changes.
    pub n_raw: Option<String>,
    pub name_changed: bool,
    /// The file the contact came from (None for a new contact).
    pub path: Option<PathBuf>,
    /// The file's stamp when it was read; a save refuses to overwrite a
    /// file whose stamp has changed since.
    pub stamp: Option<FileStamp>,
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
                // Other apps' British category name.
                || c.eq_ignore_ascii_case("favourites") // spelling: kept
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
                    || c.eq_ignore_ascii_case("favourites")) // spelling: kept
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

    /// The contact's photo as bytes, when `extra` carries one inline
    /// (`PHOTO;ENCODING=b:` or a `data:` URI). Contacts from the address
    /// book don't: use [`AddressBook::photo`] for those.
    pub fn photo_bytes(&self) -> Option<Vec<u8>> {
        let line = self.extra.iter().find(|l| vcard::is_photo_line(l))?;
        let line = vcard::parse_line(line)?;
        let value = line.value.as_str();
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
    /// The contact's file changed on disk after it was read (a sync tool,
    /// another app or an editor); nothing was written.
    #[error("the contact changed elsewhere")]
    Conflict,
}

/// What an import did, card by card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// New contacts.
    pub added: usize,
    /// Cards that added numbers, emails or other details to a contact
    /// already there (the same UID, or the same name and a shared number).
    pub merged: usize,
    /// Cards with nothing new, and cards that aren't contacts (no name and
    /// no number, contact groups).
    pub skipped: usize,
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
    /// UID -> index into contacts (the first card with that UID).
    by_uid: HashMap<String, usize>,
    /// Every contact file as last read.
    files: HashMap<PathBuf, FileStamp>,
}

/// `$XDG_DATA_HOME/contacts`, or `~/.local/share/contacts`.
pub fn default_root() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("contacts");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    Path::new(&home).join(".local/share/contacts")
}

fn hidden(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// A contact file: `*.vcf`, not hidden (our temporary files are hidden).
fn is_card_file(p: &Path) -> bool {
    !hidden(p) && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vcf"))
}

/// Read one contact file, without its photo.
fn load_file(path: &Path) -> Option<Contact> {
    use std::io::Read;
    let read = || -> std::io::Result<(Vec<u8>, FileStamp)> {
        let mut f = fs::File::open(path)?;
        // The stamp of the file we read, not of whatever replaces it.
        let stamp = FileStamp::from_metadata(&f.metadata()?);
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        Ok((bytes, stamp))
    };
    let (bytes, stamp) = match read() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "unreadable contact file");
            return None;
        }
    };
    // One card per file in a vdir; take the first.
    let mut c = vcard::parse(&vcard::text_from_bytes(&bytes))
        .into_iter()
        .next()?;
    if c.uid.is_empty() {
        c.uid = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    c.extra.retain(|l| !vcard::is_photo_line(l));
    c.path = Some(path.to_path_buf());
    c.stamp = Some(stamp);
    Some(c)
}

/// A name compared for merging: case and spacing ignored.
fn name_key(name: &str) -> String {
    name.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Properties a card has at most one of: a merge keeps the existing one.
const SINGLE: &[&str] = &[
    "BDAY",
    "ANNIVERSARY",
    "ORG",
    "TITLE",
    "ROLE",
    "NICKNAME",
    "PHOTO",
    "LOGO",
    "GENDER",
    "KIND",
    "SORT-STRING",
    "X-ABSHOWAS",
    "X-ABUID",
    "X-PHONETIC-FIRST-NAME",
    "X-PHONETIC-LAST-NAME",
];

/// Merge `card` into `into`: union the numbers, emails, categories and
/// kept lines, fill an empty name, and append a new note. True if `into`
/// gained anything.
fn merge_card(into: &mut Contact, card: Contact, region: &str) -> bool {
    let mut changed = false;
    if into.name.is_empty() && !card.name.is_empty() {
        into.name = card.name;
        into.n_raw = card.n_raw;
        into.name_changed = into.n_raw.is_none();
        changed = true;
    }
    for p in card.phones {
        match into
            .phones
            .iter_mut()
            .find(|q| number::same_number(&q.number, &p.number, region))
        {
            Some(q) => {
                if q.kind == PhoneKind::Other && p.kind != PhoneKind::Other {
                    q.kind = p.kind;
                    changed = true;
                }
                if q.label.is_empty() && !p.label.is_empty() {
                    q.label = p.label;
                    changed = true;
                }
            }
            None => {
                into.phones.push(p);
                changed = true;
            }
        }
    }
    for e in card.emails {
        if !into
            .emails
            .iter()
            .any(|x| x.address.eq_ignore_ascii_case(&e.address))
        {
            into.emails.push(e);
            changed = true;
        }
    }
    let note = card.note.trim();
    if !note.is_empty() && !into.note.contains(note) {
        if !into.note.is_empty() {
            into.note.push('\n');
        }
        into.note.push_str(note);
        changed = true;
    }
    for cat in card.categories {
        if !into.categories.iter().any(|x| x.eq_ignore_ascii_case(&cat)) {
            into.categories.push(cat);
            changed = true;
        }
    }
    changed |= merge_extra(into, card.extra);
    changed
}

/// The kept lines of a merged card, grouped ones (`item3.ADR` with its
/// `item3.X-ABLabel`) as one unit under a group name `into` doesn't use.
fn merge_extra(into: &mut Contact, extra: Vec<String>) -> bool {
    let parsed: Vec<vcard::Line> = into
        .extra
        .iter()
        .filter_map(|l| vcard::parse_line(l))
        .collect();
    let mut used: HashSet<String> = parsed
        .iter()
        .filter_map(|l| l.group.as_ref().map(|g| g.to_ascii_lowercase()))
        .collect();
    let has_name = |n: &str| parsed.iter().any(|l| l.name == n);
    let has_value = |l: &vcard::Line| {
        parsed
            .iter()
            .any(|x| x.name == l.name && x.value == l.value)
    };
    // Units in order: (group, lines).
    let mut units: Vec<(Option<String>, Vec<vcard::Line>)> = Vec::new();
    for line in extra.iter().filter_map(|l| vcard::parse_line(l)) {
        let g = line.group.as_ref().map(|g| g.to_ascii_lowercase());
        match units.iter_mut().find(|(ug, _)| g.is_some() && *ug == g) {
            Some((_, lines)) => lines.push(line),
            None => units.push((g, vec![line])),
        }
    }
    let mut changed = false;
    let mut next = 0;
    for (group, lines) in units {
        // A unit is new when one of its lines (labels aside) is.
        let fresh = lines.iter().filter(|l| l.name != "X-ABLABEL").any(|l| {
            if l.name == "PHOTO" {
                !into.has_photo
            } else if SINGLE.contains(&l.name.as_str()) {
                !has_name(&l.name)
            } else {
                !has_value(l)
            }
        });
        if !fresh {
            continue;
        }
        let new_group = group.map(|_| loop {
            next += 1;
            let g = format!("item{next}");
            if used.insert(g.clone()) {
                break g;
            }
        });
        for l in lines {
            if l.name == "PHOTO" {
                into.has_photo = true;
            }
            let raw = match (&new_group, &l.group) {
                (Some(g), Some(old)) => format!("{g}{}", &l.raw[old.len()..]),
                _ => l.raw,
            };
            into.extra.push(raw);
        }
        changed = true;
    }
    changed
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
            by_uid: HashMap::new(),
            files: HashMap::new(),
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
            .filter(|p| p.is_dir() && !hidden(p))
            .collect();
        dirs.sort();
        dirs
    }

    /// The directories to watch for changes: the root (for books created
    /// or removed) and every book in it. An app watches these, and after
    /// a change calls [`AddressBook::reload_if_changed`] and this again,
    /// so a book created after it started is watched too.
    pub fn watch_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.root.clone()];
        dirs.extend(self.book_dirs());
        dirs
    }

    /// Every contact file now, with its stamp.
    fn scan(&self) -> HashMap<PathBuf, FileStamp> {
        let mut out = HashMap::new();
        for dir in self.book_dirs() {
            for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if !is_card_file(&p) {
                    continue;
                }
                if let Some(s) = FileStamp::of(&p) {
                    out.insert(p, s);
                }
            }
        }
        out
    }

    /// Read every book again from scratch.
    pub fn reload(&mut self) {
        self.contacts.clear();
        self.files.clear();
        self.reload_if_changed();
    }

    /// Reparse the contact files that were added, changed (in place or
    /// replaced) or removed since they were read; the others stay as they
    /// are. True if anything changed.
    pub fn reload_if_changed(&mut self) -> bool {
        let now = self.scan();
        if now == self.files {
            return false;
        }
        let stale: HashSet<&PathBuf> = self
            .files
            .iter()
            .filter(|(p, s)| now.get(*p) != Some(*s))
            .map(|(p, _)| p)
            .collect();
        let keep: Vec<Contact> = std::mem::take(&mut self.contacts)
            .into_iter()
            .filter(|c| {
                c.path
                    .as_ref()
                    .is_some_and(|p| !stale.contains(p) && now.contains_key(p))
            })
            .collect();
        let have: HashSet<PathBuf> = keep.iter().filter_map(|c| c.path.clone()).collect();
        let mut contacts = keep;
        let mut files = HashMap::with_capacity(now.len());
        let mut reread = 0;
        for (p, s) in now {
            if have.contains(&p) {
                files.insert(p, s);
                continue;
            }
            reread += 1;
            match load_file(&p) {
                Some(c) => {
                    files.insert(p, c.stamp.unwrap_or(s));
                    contacts.push(c);
                }
                // Not a contact (yet): remember it so it isn't read again
                // until it changes.
                None => {
                    files.insert(p, s);
                }
            }
        }
        contacts.sort_by_cached_key(|c| (c.display_name().to_lowercase(), c.path.clone()));
        self.contacts = contacts;
        self.files = files;
        self.index();
        tracing::debug!(
            contacts = self.contacts.len(),
            reread,
            "address book loaded"
        );
        true
    }

    fn index(&mut self) {
        self.by_tail.clear();
        self.by_uid.clear();
        for (i, c) in self.contacts.iter().enumerate() {
            self.by_uid.entry(c.uid.clone()).or_insert(i);
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
        self.by_uid.get(uid).map(|&i| &self.contacts[i])
    }

    /// A contact's photo, read from its file (None when it has none, or
    /// only a link to one).
    pub fn photo(&self, uid: &str) -> Option<Vec<u8>> {
        let c = self.get(uid).filter(|c| c.has_photo)?;
        let bytes = fs::read(c.path.as_ref()?).ok()?;
        let text = vcard::text_from_bytes(&bytes);
        let card = Contact {
            extra: vcard::photo_lines(&text),
            ..Contact::default()
        };
        card.photo_bytes()
    }

    pub fn starred(&self) -> Vec<&Contact> {
        self.contacts.iter().filter(|c| c.starred()).collect()
    }

    /// The contact that owns a number.
    pub fn lookup(&self, raw: &str) -> Option<Match> {
        let key = number::tail_key(raw);
        let candidates = self.by_tail.get(&key)?;
        // Parsed once: the candidates share the last digits, and parsing
        // is the costly part.
        let want = number::to_e164(raw, &self.region);
        for &i in candidates {
            let c = &self.contacts[i];
            for p in &c.phones {
                let same = match (&want, number::to_e164(&p.number, &self.region)) {
                    (Some(a), Some(b)) => *a == b,
                    _ => number::same_number(&p.number, raw, &self.region),
                };
                if same {
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

    /// Save a contact (new or edited) to its file, atomically. An edited
    /// contact whose file changed on disk since it was read is not
    /// written: [`Error::Conflict`]. Reload and edit it again.
    pub fn save(&mut self, contact: &Contact) -> Result<(), Error> {
        let mut c = contact.clone();
        if c.uid.is_empty() {
            c.uid = new_uid();
        }
        let path = match &c.path {
            Some(p) => {
                check_unchanged(&c, p)?;
                p.clone()
            }
            None => {
                let dir = self.root.join(&self.default_book);
                fs::create_dir_all(&dir)?;
                dir.join(format!("{}.vcf", file_safe(&c.uid)))
            }
        };
        write_card(&c, &path)?;
        self.reload_if_changed();
        Ok(())
    }

    pub fn delete(&mut self, uid: &str) -> Result<(), Error> {
        let c = self.get(uid).ok_or(Error::NotFound)?;
        if let Some(p) = &c.path {
            fs::remove_file(p)?;
        }
        self.reload_if_changed();
        Ok(())
    }

    /// Import every card in a `.vcf` (one or many contacts, as Android,
    /// Google Contacts and Apple's Contacts export them). A card merges
    /// into the contact with its UID or, failing that, the contact with
    /// the same name and at least one of its numbers (a card without
    /// numbers: one of its emails), also one added earlier in the same
    /// file. Numbers, emails and details are united; nothing is dropped.
    /// Other cards become new contacts in the default book, with a UID
    /// generated when they have none.
    pub fn import(&mut self, text: &str) -> Result<ImportReport, Error> {
        self.reload_if_changed();
        let total = vcard::count_cards(text);
        let cards = vcard::parse(text);
        let mut report = ImportReport {
            skipped: total.saturating_sub(cards.len()),
            ..ImportReport::default()
        };
        // The contacts this import adds or changes, written at the end.
        struct Work {
            contact: Contact,
            fresh: bool,
            changed: bool,
        }
        let mut work: Vec<Work> = Vec::new();
        let mut work_uid: HashMap<String, usize> = HashMap::new();
        let mut work_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut pulled: HashMap<usize, usize> = HashMap::new();
        let mut book_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, c) in self.contacts.iter().enumerate() {
            book_name.entry(name_key(&c.name)).or_default().push(i);
        }
        let region = self.region.clone();
        // A shared number, or for cards without one a shared email.
        let shares_number = |a: &Contact, b: &Contact| {
            a.phones.iter().any(|p| {
                b.phones
                    .iter()
                    .any(|q| number::same_number(&p.number, &q.number, &region))
            }) || (b.phones.is_empty()
                && a.emails.iter().any(|e| {
                    b.emails
                        .iter()
                        .any(|f| e.address.eq_ignore_ascii_case(&f.address))
                }))
        };
        for card in cards {
            let key = name_key(&card.name);
            let mut target: Option<usize> = None;
            let mut from_book: Option<usize> = None;
            if !card.uid.is_empty() {
                target = work_uid.get(&card.uid).copied();
                if target.is_none() {
                    from_book = self.by_uid.get(&card.uid).copied();
                }
            }
            if target.is_none() && from_book.is_none() {
                target = work_name.get(&key).and_then(|ws| {
                    ws.iter()
                        .copied()
                        .find(|&w| shares_number(&work[w].contact, &card))
                });
                if target.is_none() {
                    from_book = book_name.get(&key).and_then(|bs| {
                        bs.iter()
                            .copied()
                            .find(|&b| shares_number(&self.contacts[b], &card))
                    });
                }
            }
            if let Some(b) = from_book {
                target = Some(*pulled.entry(b).or_insert_with(|| {
                    let c = self.contacts[b].clone();
                    work_uid.insert(c.uid.clone(), work.len());
                    work_name
                        .entry(name_key(&c.name))
                        .or_default()
                        .push(work.len());
                    work.push(Work {
                        contact: c,
                        fresh: false,
                        changed: false,
                    });
                    work.len() - 1
                }));
            }
            match target {
                Some(w) => {
                    if merge_card(&mut work[w].contact, card, &self.region) {
                        work[w].changed = true;
                        report.merged += 1;
                    } else {
                        report.skipped += 1;
                    }
                }
                None => {
                    let mut c = card;
                    if c.uid.is_empty() {
                        c.uid = new_uid();
                    }
                    work_uid.insert(c.uid.clone(), work.len());
                    work_name.entry(key).or_default().push(work.len());
                    work.push(Work {
                        contact: c,
                        fresh: true,
                        changed: true,
                    });
                    report.added += 1;
                }
            }
        }
        let dir = self.root.join(&self.default_book);
        for w in work.iter_mut().filter(|w| w.changed) {
            let c = &mut w.contact;
            let path = match (&c.path, w.fresh) {
                (Some(p), false) => {
                    check_unchanged(c, p)?;
                    p.clone()
                }
                _ => {
                    fs::create_dir_all(&dir)?;
                    let mut path = dir.join(format!("{}.vcf", file_safe(&c.uid)));
                    while path.exists() {
                        c.uid = new_uid();
                        path = dir.join(format!("{}.vcf", file_safe(&c.uid)));
                    }
                    path
                }
            };
            write_card(c, &path)?;
        }
        self.reload_if_changed();
        tracing::info!(
            added = report.added,
            merged = report.merged,
            skipped = report.skipped,
            "contacts imported"
        );
        Ok(report)
    }

    /// All contacts as one `.vcf`, photos included.
    pub fn export(&self) -> String {
        self.contacts.iter().map(card_text).collect()
    }
}

/// Fail with [`Error::Conflict`] when the contact's file changed on disk
/// since the contact was read.
fn check_unchanged(c: &Contact, path: &Path) -> Result<(), Error> {
    match c.stamp {
        Some(s) if FileStamp::of(path) != Some(s) => Err(Error::Conflict),
        _ => Ok(()),
    }
}

/// The card as written: a photo the contact keeps in its file goes back in.
fn card_text(c: &Contact) -> String {
    if c.has_photo && !c.extra.iter().any(|l| vcard::is_photo_line(l)) {
        if let Some(bytes) = c.path.as_ref().and_then(|p| fs::read(p).ok()) {
            let mut full = c.clone();
            full.extra
                .extend(vcard::photo_lines(&vcard::text_from_bytes(&bytes)));
            return vcard::write(&full);
        }
    }
    vcard::write(c)
}

fn write_card(c: &Contact, path: &Path) -> Result<(), Error> {
    let text = card_text(c);
    write_atomic(path, text.as_bytes())?;
    Ok(())
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
                label: String::new(),
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
        let r = b.import(vcf).unwrap();
        assert_eq!((r.added, r.merged, r.skipped), (2, 0, 0));
        // Again: the UID matches one, the name and number the other.
        let r = b.import(vcf).unwrap();
        assert_eq!((r.added, r.merged, r.skipped), (0, 0, 2));
        assert_eq!(b.contacts().len(), 2);
        assert!(b.export().matches("BEGIN:VCARD").count() == 2);
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

    fn write_card_file(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Rewrite a file in place (same inode), as an editor might, and move
    /// its time on so the change shows even on a coarse clock.
    fn edit_in_place(path: &Path, text: &str) {
        let before = fs::metadata(path).unwrap().modified().unwrap();
        let mut f = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f.set_modified(before + std::time::Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn reload_if_changed_sees_in_place_edits_and_removals() {
        let (d, mut b) = book();
        let a = d.path().join("default/a.vcf");
        let c = d.path().join("default/c.vcf");
        write_card_file(
            &a,
            "BEGIN:VCARD\nVERSION:3.0\nUID:a\nFN:Ann One\nEND:VCARD\n",
        );
        write_card_file(
            &c,
            "BEGIN:VCARD\nVERSION:3.0\nUID:c\nFN:Cid One\nEND:VCARD\n",
        );
        assert!(b.reload_if_changed());
        assert!(!b.reload_if_changed());
        // Same size, same inode: only the time and the content differ.
        edit_in_place(
            &a,
            "BEGIN:VCARD\nVERSION:3.0\nUID:a\nFN:Ann Two\nEND:VCARD\n",
        );
        assert!(b.reload_if_changed());
        assert_eq!(b.get("a").unwrap().name, "Ann Two");
        assert_eq!(b.get("c").unwrap().name, "Cid One");
        fs::remove_file(&c).unwrap();
        assert!(b.reload_if_changed());
        assert!(b.get("c").is_none());
        assert_eq!(b.contacts().len(), 1);
    }

    #[test]
    fn watch_dirs_include_books_created_later() {
        let (d, b) = book();
        assert_eq!(b.watch_dirs(), vec![d.path().to_path_buf()]);
        fs::create_dir_all(d.path().join("work")).unwrap();
        assert_eq!(
            b.watch_dirs(),
            vec![d.path().to_path_buf(), d.path().join("work")]
        );
    }

    #[test]
    fn save_refuses_a_card_changed_on_disk() {
        let (d, mut b) = book();
        let a = d.path().join("default/a.vcf");
        write_card_file(
            &a,
            "BEGIN:VCARD\nVERSION:3.0\nUID:a\nFN:Ann One\nEND:VCARD\n",
        );
        b.reload();
        let mut mine = b.get("a").unwrap().clone();
        edit_in_place(
            &a,
            "BEGIN:VCARD\nVERSION:3.0\nUID:a\nFN:Ann Syn\nEND:VCARD\n",
        );
        mine.set_name("Ann Mine");
        assert!(matches!(b.save(&mine), Err(Error::Conflict)));
        assert!(fs::read_to_string(&a).unwrap().contains("Ann Syn"));
        // Reloaded, the edit goes through.
        b.reload_if_changed();
        let mut mine = b.get("a").unwrap().clone();
        mine.set_name("Ann Mine");
        b.save(&mine).unwrap();
        assert_eq!(b.get("a").unwrap().name, "Ann Mine");
        // A card removed meanwhile is a conflict too.
        let mine = b.get("a").unwrap().clone();
        fs::remove_file(&a).unwrap();
        assert!(matches!(b.save(&mine), Err(Error::Conflict)));
    }

    #[test]
    fn photos_stay_in_the_file() {
        let (d, mut b) = book();
        let a = d.path().join("default/a.vcf");
        write_card_file(
            &a,
            "BEGIN:VCARD\nVERSION:3.0\nUID:a\nFN:Pat Photo\nPHOTO;ENCODING=b;TYPE=PNG:aGVs\n bG8=\nEND:VCARD\n",
        );
        b.reload();
        let c = b.get("a").unwrap().clone();
        assert!(c.has_photo);
        assert!(c.extra.is_empty());
        assert_eq!(b.photo("a").unwrap(), b"hello");
        let mut c = c;
        c.set_name("Pat Renamed");
        b.save(&c).unwrap();
        let text = fs::read_to_string(&a).unwrap();
        assert!(text.contains("FN:Pat Renamed"));
        assert!(text.contains("PHOTO;ENCODING=b;TYPE=PNG:aGVsbG8="));
        assert_eq!(b.photo("a").unwrap(), b"hello");
        assert!(b.export().contains("PHOTO;ENCODING=b"));
    }

    #[test]
    fn import_merges_by_name_and_number_and_keeps_everything() {
        let (_d, mut b) = book();
        let mut known = contact("Robin Example", &["+1 202 555 0140"]);
        known.note = "Known".into();
        b.save(&known).unwrap();
        let vcf = "BEGIN:VCARD\nVERSION:3.0\nFN:robin  example\nTEL;TYPE=CELL:(202) 555-0140\n\
item1.TEL:202-555-0141\nitem1.X-ABLabel:Boat\nEMAIL;TYPE=INTERNET:robin@example.com\n\
item2.ADR;TYPE=HOME:;;1 Example Way;Springfield;;;\nitem2.X-ABLabel:Cabin\nBDAY:2000-01-01\nEND:VCARD\n\
BEGIN:VCARD\nVERSION:3.0\nFN:Robin Example\nTEL:+1 202 555 0199\nEND:VCARD\n";
        let r = b.import(vcf).unwrap();
        // The second card shares the name but no number: a new contact.
        assert_eq!((r.added, r.merged, r.skipped), (1, 1, 0));
        let merged = b
            .contacts()
            .iter()
            .find(|c| c.uid == known.uid)
            .unwrap()
            .clone();
        assert_eq!(merged.name, "Robin Example");
        assert_eq!(merged.phones.len(), 2);
        assert_eq!(merged.phones[1].label, "Boat");
        assert_eq!(merged.phones[1].label_text(), "Boat");
        assert_eq!(merged.emails[0].address, "robin@example.com");
        assert_eq!(merged.note, "Known");
        assert!(merged.extra.iter().any(|l| l.ends_with(".X-ABLabel:Cabin")));
        assert!(merged.extra.iter().any(|l| l.starts_with("BDAY")));
        // Nothing new the second time.
        let r = b.import(vcf).unwrap();
        assert_eq!((r.added, r.merged, r.skipped), (0, 0, 2));
        assert_eq!(b.contacts().len(), 2);
    }
}
