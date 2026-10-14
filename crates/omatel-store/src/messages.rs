// SPDX-License-Identifier: GPL-3.0-or-later
//! Conversations and messages (`messages.db`).
//!
//! A conversation is keyed by its `address`: the normalised number of a
//! one-to-one thread, or the sorted, comma-joined numbers of a group (MMS,
//! T3). Each message carries a `transport` (`sms`, later `mms`), so other
//! transports fit without a new table. Incoming messages carry a
//! `dedupe_key` (number, network timestamp, text): the agent stores a
//! message, then deletes it from the modem, and a crash between the two
//! must give a duplicate that this key filters, never a lost message.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::{now_ms, open, Error, Result, EXPORT_VERSION};

const MIGRATIONS: &[&str] = &["CREATE TABLE conversations (
        id INTEGER PRIMARY KEY,
        address TEXT NOT NULL UNIQUE,
        title TEXT NOT NULL DEFAULT '',
        pinned INTEGER NOT NULL DEFAULT 0,
        archived INTEGER NOT NULL DEFAULT 0,
        muted INTEGER NOT NULL DEFAULT 0,
        draft TEXT NOT NULL DEFAULT '',
        draft_at INTEGER,
        last_at INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE participants (
        id INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
        number TEXT NOT NULL,
        raw_number TEXT NOT NULL,
        UNIQUE (conversation_id, number)
    );
    CREATE TABLE messages (
        id INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
        direction TEXT NOT NULL CHECK (direction IN ('in', 'out')),
        transport TEXT NOT NULL DEFAULT 'sms',
        sender TEXT NOT NULL DEFAULT '',
        body TEXT NOT NULL DEFAULT '',
        at INTEGER NOT NULL,
        sent_at INTEGER,
        delivered_at INTEGER,
        state TEXT NOT NULL,
        read INTEGER NOT NULL DEFAULT 0,
        mm_path TEXT,
        mm_reference INTEGER,
        mms_path TEXT,
        dedupe_key TEXT UNIQUE,
        error TEXT
    );
    CREATE INDEX messages_conversation ON messages (conversation_id, at);
    CREATE INDEX messages_mm_path ON messages (mm_path);
    CREATE TABLE parts (
        id INTEGER PRIMARY KEY,
        message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
        mime TEXT NOT NULL,
        file TEXT NOT NULL,
        size INTEGER NOT NULL DEFAULT 0
    );
    CREATE VIRTUAL TABLE messages_fts USING fts5 (body, content = 'messages', content_rowid = 'id');
    CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts (rowid, body) VALUES (new.id, new.body);
    END;
    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts (messages_fts, rowid, body) VALUES ('delete', old.id, old.body);
    END;
    CREATE TRIGGER messages_au AFTER UPDATE OF body ON messages BEGIN
        INSERT INTO messages_fts (messages_fts, rowid, body) VALUES ('delete', old.id, old.body);
        INSERT INTO messages_fts (rowid, body) VALUES (new.id, new.body);
    END;"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
        }
    }
}

/// Where a message stands. Outgoing: sending -> sent -> delivered, or
/// failed. Incoming: received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageState {
    Received,
    Sending,
    Sent,
    Delivered,
    Failed,
}

impl MessageState {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageState::Received => "received",
            MessageState::Sending => "sending",
            MessageState::Sent => "sent",
            MessageState::Delivered => "delivered",
            MessageState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> MessageState {
        match s {
            "received" => MessageState::Received,
            "sending" => MessageState::Sending,
            "sent" => MessageState::Sent,
            "delivered" => MessageState::Delivered,
            _ => MessageState::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub conversation_id: i64,
    pub direction: Direction,
    #[serde(default = "sms")]
    pub transport: String,
    /// The sender in a group conversation; empty otherwise.
    #[serde(default)]
    pub sender: String,
    pub body: String,
    /// When it arrived here (incoming) or was written (outgoing), ms.
    pub at: i64,
    /// The network's timestamp (incoming) or when the modem took it.
    pub sent_at: Option<i64>,
    pub delivered_at: Option<i64>,
    pub state: MessageState,
    pub read: bool,
    #[serde(skip)]
    pub mm_path: Option<String>,
    #[serde(skip)]
    pub error: Option<String>,
}

fn sms() -> String {
    "sms".into()
}

const MESSAGE_COLUMNS: &str =
    "m.id, m.conversation_id, m.direction, m.transport, m.sender, m.body, \
     m.at, m.sent_at, m.delivered_at, m.state, m.read, m.mm_path, m.error";

fn message(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        direction: if r.get::<_, String>(2)? == "in" {
            Direction::In
        } else {
            Direction::Out
        },
        transport: r.get(3)?,
        sender: r.get(4)?,
        body: r.get(5)?,
        at: r.get(6)?,
        sent_at: r.get(7)?,
        delivered_at: r.get(8)?,
        state: MessageState::parse(&r.get::<_, String>(9)?),
        read: r.get(10)?,
        mm_path: r.get(11)?,
        error: r.get(12)?,
    })
}

/// A conversation as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub id: i64,
    pub address: String,
    /// The participants' numbers as first seen (for display and sending).
    pub numbers: Vec<String>,
    pub title: String,
    pub pinned: bool,
    pub archived: bool,
    pub muted: bool,
    pub draft: String,
    pub last_at: i64,
    pub unread: i64,
    pub last: Option<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub message: Message,
    pub conversation_id: i64,
    pub address: String,
}

/// An incoming SMS to store.
#[derive(Debug, Clone)]
pub struct NewIncoming<'a> {
    /// Normalised sender (the conversation key).
    pub address: &'a str,
    /// The sender as the network gave it.
    pub raw_number: &'a str,
    pub body: &'a str,
    /// The network timestamp, ms, if known.
    pub sent_at: Option<i64>,
    /// Something that tells two identical texts apart when the network
    /// gave no timestamp (the modem's object path).
    pub fallback_key: &'a str,
    /// When it arrived, ms; None for now (imports and demos set it).
    pub received_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct ExportConversation {
    address: String,
    numbers: Vec<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    draft: String,
    messages: Vec<Message>,
}

#[derive(Serialize, Deserialize)]
struct Export {
    format: String,
    version: u32,
    conversations: Vec<ExportConversation>,
}

pub struct MessageStore {
    conn: Connection,
}

/// A 64-bit FNV-1a hash, for the dedupe key (stable across builds).
fn fnv(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// An FTS5 query from words a person typed: every word must appear, as a
/// prefix ("hel wor" finds "hello world").
fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

impl MessageStore {
    pub fn open(path: &Path) -> Result<MessageStore> {
        Ok(MessageStore {
            conn: open(Some(path), MIGRATIONS)?,
        })
    }

    pub fn open_in_memory() -> Result<MessageStore> {
        Ok(MessageStore {
            conn: open(None, MIGRATIONS)?,
        })
    }

    /// The conversation for an address, created if new.
    pub fn conversation_for(&self, address: &str, raw_numbers: &[&str]) -> Result<i64> {
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM conversations WHERE address = ?1",
                [address],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO conversations (address, created_at) VALUES (?1, ?2)",
            params![address, now_ms()],
        )?;
        let id = tx.last_insert_rowid();
        let keys: Vec<&str> = address.split(',').collect();
        for (i, key) in keys.iter().enumerate() {
            let raw = raw_numbers.get(i).copied().unwrap_or(key);
            tx.execute(
                "INSERT OR IGNORE INTO participants (conversation_id, number, raw_number) VALUES (?1, ?2, ?3)",
                params![id, key, raw],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn find_conversation(&self, address: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM conversations WHERE address = ?1",
                [address],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Store an incoming SMS. `Ok(None)` when it is a duplicate of one
    /// already stored. Committed (and synced) when this returns.
    pub fn insert_incoming(&self, m: &NewIncoming) -> Result<Option<(i64, i64)>> {
        let stamp = match m.sent_at {
            Some(t) => t.to_string(),
            None => format!("path:{}", m.fallback_key),
        };
        let key = format!("{}|{}|{:016x}", m.address, stamp, fnv(m.body));
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE dedupe_key = ?1)",
            [&key],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(None);
        }
        let conv = self.conversation_for(m.address, &[m.raw_number])?;
        let now = m.received_at.unwrap_or_else(now_ms);
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO messages (conversation_id, direction, body, at, sent_at, state, read, dedupe_key)
             VALUES (?1, 'in', ?2, ?3, ?4, 'received', 0, ?5)",
            params![conv, m.body, now, m.sent_at, key],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE conversations SET last_at = max(last_at, ?2), archived = 0 WHERE id = ?1",
            params![conv, now],
        )?;
        tx.commit()?;
        Ok(Some((conv, id)))
    }

    /// Add an outgoing message in the `sending` state.
    pub fn insert_outgoing(&self, conversation: i64, body: &str) -> Result<i64> {
        self.insert_outgoing_at(conversation, body, now_ms())
    }

    /// Add an outgoing message written at `now` (ms), for imports and demos.
    pub fn insert_outgoing_at(&self, conversation: i64, body: &str, now: i64) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO messages (conversation_id, direction, body, at, state, read)
             VALUES (?1, 'out', ?2, ?3, 'sending', 1)",
            params![conversation, body, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE conversations SET last_at = max(last_at, ?2), draft = '', draft_at = NULL WHERE id = ?1",
            params![conversation, now],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn set_state(&self, id: i64, state: MessageState, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE messages SET state = ?2, error = ?3,
                 sent_at = CASE WHEN ?2 = 'sent' AND sent_at IS NULL THEN ?4 ELSE sent_at END,
                 delivered_at = CASE WHEN ?2 = 'delivered' THEN ?4 ELSE delivered_at END
             WHERE id = ?1",
            params![id, state.as_str(), error, now],
        )?;
        Ok(())
    }

    /// Remember (or forget, with None) the modem's object for a message
    /// while its delivery report is awaited.
    pub fn set_mm_path(&self, id: i64, path: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET mm_path = ?2 WHERE id = ?1",
            params![id, path],
        )?;
        Ok(())
    }

    pub fn by_mm_path(&self, path: &str) -> Result<Option<Message>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.mm_path = ?1"),
                [path],
                message,
            )
            .optional()?)
    }

    pub fn message(&self, id: i64) -> Result<Option<Message>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.id = ?1"),
                [id],
                message,
            )
            .optional()?)
    }

    /// Outgoing messages left in `sending` (the agent stopped mid-send):
    /// marked failed so the user can retry. Returns their IDs.
    pub fn fail_interrupted(&self) -> Result<Vec<i64>> {
        let mut st = self
            .conn
            .prepare("SELECT id FROM messages WHERE direction = 'out' AND state = 'sending'")?;
        let ids: Vec<i64> = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in &ids {
            self.set_state(*id, MessageState::Failed, Some("interrupted"))?;
        }
        Ok(ids)
    }

    fn summary(&self, r: &Row) -> rusqlite::Result<Conversation> {
        Ok(Conversation {
            id: r.get(0)?,
            address: r.get(1)?,
            numbers: Vec::new(),
            title: r.get(2)?,
            pinned: r.get(3)?,
            archived: r.get(4)?,
            muted: r.get(5)?,
            draft: r.get(6)?,
            last_at: r.get(7)?,
            unread: r.get(8)?,
            last: None,
        })
    }

    fn fill(&self, mut c: Conversation) -> Result<Conversation> {
        let mut st = self.conn.prepare_cached(
            "SELECT raw_number FROM participants WHERE conversation_id = ?1 ORDER BY id",
        )?;
        c.numbers = st
            .query_map([c.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        c.last = self
            .conn
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.conversation_id = ?1
                     ORDER BY m.at DESC, m.id DESC LIMIT 1"
                ),
                [c.id],
                message,
            )
            .optional()?;
        Ok(c)
    }

    const CONVERSATION_SELECT: &'static str =
        "SELECT c.id, c.address, c.title, c.pinned, c.archived, c.muted, c.draft, c.last_at,
                (SELECT count(*) FROM messages m WHERE m.conversation_id = c.id
                    AND m.direction = 'in' AND m.read = 0)
         FROM conversations c";

    /// The conversation list: pinned first, then newest first. Archived
    /// conversations only when `archived` is true (and then only those).
    pub fn conversations(&self, archived: bool) -> Result<Vec<Conversation>> {
        let mut st = self.conn.prepare(&format!(
            "{} WHERE c.archived = ?1 AND (c.last_at > 0 OR c.draft != '')
             ORDER BY c.pinned DESC, max(c.last_at, coalesce(c.draft_at, 0)) DESC",
            Self::CONVERSATION_SELECT
        ))?;
        let rows: Vec<Conversation> = st
            .query_map([archived], |r| self.summary(r))?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter().map(|c| self.fill(c)).collect()
    }

    pub fn conversation(&self, id: i64) -> Result<Option<Conversation>> {
        let c = self
            .conn
            .query_row(
                &format!("{} WHERE c.id = ?1", Self::CONVERSATION_SELECT),
                [id],
                |r| self.summary(r),
            )
            .optional()?;
        c.map(|c| self.fill(c)).transpose()
    }

    /// Messages of a conversation, oldest first: the newest `limit`, or
    /// the `limit` before message `before` (for scrolling back).
    pub fn messages(
        &self,
        conversation: i64,
        limit: usize,
        before: Option<i64>,
    ) -> Result<Vec<Message>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.conversation_id = ?1
               AND (?2 IS NULL OR m.at < (SELECT at FROM messages WHERE id = ?2)
                    OR (m.at = (SELECT at FROM messages WHERE id = ?2) AND m.id < ?2))
             ORDER BY m.at DESC, m.id DESC LIMIT ?3"
        ))?;
        let mut rows: Vec<Message> = st
            .query_map(params![conversation, before, limit as i64], message)?
            .collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Full-text search over every message, newest first.
    pub fn search(&self, text: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(q) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS}, c.address FROM messages_fts f
             JOIN messages m ON m.id = f.rowid JOIN conversations c ON c.id = m.conversation_id
             WHERE messages_fts MATCH ?1 ORDER BY m.at DESC LIMIT ?2"
        ))?;
        let rows = st.query_map(params![q, limit as i64], |r| {
            let m = message(r)?;
            Ok(SearchHit {
                conversation_id: m.conversation_id,
                address: r.get(13)?,
                message: m,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn unread_total(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM messages WHERE direction = 'in' AND read = 0",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn mark_read(&self, conversation: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE messages SET read = 1 WHERE conversation_id = ?1 AND read = 0",
            [conversation],
        )?)
    }

    fn set_flag(&self, conversation: i64, column: &str, on: bool) -> Result<()> {
        self.conn.execute(
            &format!("UPDATE conversations SET {column} = ?2 WHERE id = ?1"),
            params![conversation, on],
        )?;
        Ok(())
    }

    pub fn set_pinned(&self, conversation: i64, on: bool) -> Result<()> {
        self.set_flag(conversation, "pinned", on)
    }

    pub fn set_archived(&self, conversation: i64, on: bool) -> Result<()> {
        self.set_flag(conversation, "archived", on)
    }

    pub fn set_muted(&self, conversation: i64, on: bool) -> Result<()> {
        self.set_flag(conversation, "muted", on)
    }

    pub fn set_draft(&self, conversation: i64, text: &str) -> Result<()> {
        let at = (!text.trim().is_empty()).then(now_ms);
        self.conn.execute(
            "UPDATE conversations SET draft = ?2, draft_at = ?3 WHERE id = ?1",
            params![conversation, text, at],
        )?;
        Ok(())
    }

    pub fn delete_conversation(&self, conversation: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM conversations WHERE id = ?1", [conversation])?;
        Ok(())
    }

    pub fn delete_message(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM messages WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Everything as our versioned JSON (docs/export-format.md).
    pub fn export_json(&self) -> Result<String> {
        let mut out = Vec::new();
        let mut ids: Vec<i64> = {
            let mut st = self
                .conn
                .prepare("SELECT id FROM conversations ORDER BY id")?;
            let r = st.query_map([], |r| r.get(0))?;
            r.collect::<rusqlite::Result<_>>()?
        };
        ids.sort();
        for id in ids {
            let Some(c) = self.conversation(id)? else {
                continue;
            };
            out.push(ExportConversation {
                address: c.address,
                numbers: c.numbers,
                title: c.title,
                pinned: c.pinned,
                archived: c.archived,
                muted: c.muted,
                draft: c.draft,
                messages: self.messages(id, usize::MAX >> 1, None)?,
            });
        }
        Ok(serde_json::to_string_pretty(&Export {
            format: "omatel-messages".into(),
            version: EXPORT_VERSION,
            conversations: out,
        })?)
    }

    /// Import an export, merging into conversations by address; messages
    /// already present (same direction, time and text) are skipped.
    /// Returns how many messages were added.
    pub fn import_json(&self, text: &str) -> Result<usize> {
        let e: Export = serde_json::from_str(text)?;
        if e.format != "omatel-messages" || e.version > EXPORT_VERSION {
            return Err(Error::Format("not an omatel messages export".into()));
        }
        let mut added = 0;
        for c in e.conversations {
            let raws: Vec<&str> = c.numbers.iter().map(String::as_str).collect();
            let id = self.conversation_for(&c.address, &raws)?;
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE conversations SET pinned = ?2, archived = ?3, muted = ?4,
                     title = CASE WHEN title = '' THEN ?5 ELSE title END WHERE id = ?1",
                params![id, c.pinned, c.archived, c.muted, c.title],
            )?;
            for m in &c.messages {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM messages WHERE conversation_id = ?1 AND direction = ?2
                         AND at = ?3 AND body = ?4)",
                    params![id, m.direction.as_str(), m.at, m.body],
                    |r| r.get(0),
                )?;
                if exists {
                    continue;
                }
                let state = match m.state {
                    MessageState::Sending => MessageState::Failed,
                    s => s,
                };
                tx.execute(
                    "INSERT INTO messages (conversation_id, direction, transport, sender, body, at,
                                           sent_at, delivered_at, state, read)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id,
                        m.direction.as_str(),
                        m.transport,
                        m.sender,
                        m.body,
                        m.at,
                        m.sent_at,
                        m.delivered_at,
                        state.as_str(),
                        m.read
                    ],
                )?;
                tx.execute(
                    "UPDATE conversations SET last_at = max(last_at, ?2) WHERE id = ?1",
                    params![id, m.at],
                )?;
                added += 1;
            }
            tx.commit()?;
        }
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming<'a>(addr: &'a str, body: &'a str, at: i64) -> NewIncoming<'a> {
        NewIncoming {
            address: addr,
            raw_number: addr,
            body,
            sent_at: Some(at),
            fallback_key: "/x",
            received_at: None,
        }
    }

    #[test]
    fn incoming_dedupes_and_counts_unread() {
        let s = MessageStore::open_in_memory().unwrap();
        let (c, _) = s
            .insert_incoming(&incoming("+12025550100", "hello there", 1))
            .unwrap()
            .unwrap();
        assert!(s
            .insert_incoming(&incoming("+12025550100", "hello there", 1))
            .unwrap()
            .is_none());
        // The same text later is a new message.
        s.insert_incoming(&incoming("+12025550100", "hello there", 2))
            .unwrap()
            .unwrap();
        let list = s.conversations(false).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].unread, 2);
        assert_eq!(list[0].numbers, vec!["+12025550100"]);
        assert_eq!(s.unread_total().unwrap(), 2);
        assert_eq!(s.mark_read(c).unwrap(), 2);
        assert_eq!(s.conversation(c).unwrap().unwrap().unread, 0);
        // Without a network timestamp the modem path tells texts apart.
        let mut m = incoming("+12025550100", "same", 0);
        m.sent_at = None;
        m.fallback_key = "/a";
        assert!(s.insert_incoming(&m).unwrap().is_some());
        assert!(s.insert_incoming(&m).unwrap().is_none());
        m.fallback_key = "/b";
        assert!(s.insert_incoming(&m).unwrap().is_some());
    }

    #[test]
    fn outgoing_states_and_mm_path() {
        let s = MessageStore::open_in_memory().unwrap();
        let c = s
            .conversation_for("+12025550101", &["202-555-0101"])
            .unwrap();
        s.set_draft(c, "half a thought").unwrap();
        let id = s.insert_outgoing(c, "on my way").unwrap();
        assert_eq!(s.conversation(c).unwrap().unwrap().draft, "");
        s.set_mm_path(id, Some("/org/freedesktop/ModemManager1/SMS/3"))
            .unwrap();
        s.set_state(id, MessageState::Sent, None).unwrap();
        let m = s
            .by_mm_path("/org/freedesktop/ModemManager1/SMS/3")
            .unwrap()
            .unwrap();
        assert_eq!(m.state, MessageState::Sent);
        assert!(m.sent_at.is_some());
        s.set_state(id, MessageState::Delivered, None).unwrap();
        assert!(s.message(id).unwrap().unwrap().delivered_at.is_some());
        let id2 = s.insert_outgoing(c, "second").unwrap();
        assert_eq!(s.fail_interrupted().unwrap(), vec![id2]);
        assert_eq!(s.message(id2).unwrap().unwrap().state, MessageState::Failed);
        let conv = s.conversation(c).unwrap().unwrap();
        assert_eq!(conv.numbers, vec!["202-555-0101"]);
        assert_eq!(conv.last.unwrap().body, "second");
    }

    #[test]
    fn flags_order_and_archive() {
        let s = MessageStore::open_in_memory().unwrap();
        let (a, _) = s
            .insert_incoming(&incoming("+12025550110", "alpha", 1))
            .unwrap()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let (b, _) = s
            .insert_incoming(&incoming("+12025550111", "b", 2))
            .unwrap()
            .unwrap();
        assert_eq!(
            s.conversations(false)
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![b, a]
        );
        s.set_pinned(a, true).unwrap();
        assert_eq!(s.conversations(false).unwrap()[0].id, a);
        s.set_archived(b, true).unwrap();
        assert_eq!(s.conversations(false).unwrap().len(), 1);
        assert_eq!(s.conversations(true).unwrap()[0].id, b);
        // A new message un-archives.
        s.insert_incoming(&incoming("+12025550111", "again", 3))
            .unwrap();
        assert_eq!(s.conversations(true).unwrap().len(), 0);
        s.set_muted(a, true).unwrap();
        assert!(s.conversation(a).unwrap().unwrap().muted);
        s.delete_conversation(a).unwrap();
        assert!(s.conversation(a).unwrap().is_none());
        assert!(s.search("alpha", 10).unwrap().is_empty());
    }

    #[test]
    fn paging_back() {
        let s = MessageStore::open_in_memory().unwrap();
        let c = s.conversation_for("+12025550120", &[]).unwrap();
        let ids: Vec<i64> = (0..5)
            .map(|i| s.insert_outgoing(c, &format!("m{i}")).unwrap())
            .collect();
        let last2 = s.messages(c, 2, None).unwrap();
        assert_eq!(
            last2.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![ids[3], ids[4]]
        );
        let before = s.messages(c, 2, Some(ids[3])).unwrap();
        assert_eq!(
            before.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![ids[1], ids[2]]
        );
    }

    #[test]
    fn full_text_search() {
        let s = MessageStore::open_in_memory().unwrap();
        s.insert_incoming(&incoming("+12025550130", "Dinner at seven?", 1))
            .unwrap();
        s.insert_incoming(&incoming("+12025550131", "the parcel arrived", 2))
            .unwrap();
        let c = s.conversation_for("+12025550130", &[]).unwrap();
        s.insert_outgoing(c, "Seven works, see you at dinner")
            .unwrap();
        assert_eq!(s.search("dinner", 10).unwrap().len(), 2);
        assert_eq!(s.search("din sev", 10).unwrap().len(), 2);
        assert_eq!(s.search("parc", 10).unwrap()[0].address, "+12025550131");
        assert!(s.search("\"quoted", 10).unwrap().is_empty());
        assert!(s.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn export_import_round_trip() {
        let a = MessageStore::open_in_memory().unwrap();
        a.insert_incoming(&incoming("+12025550140", "one", 1))
            .unwrap();
        let c = a.conversation_for("+12025550140", &[]).unwrap();
        a.insert_outgoing(c, "two").unwrap();
        a.set_pinned(c, true).unwrap();
        let json = a.export_json().unwrap();
        let b = MessageStore::open_in_memory().unwrap();
        assert_eq!(b.import_json(&json).unwrap(), 2);
        assert_eq!(b.import_json(&json).unwrap(), 0);
        let list = b.conversations(false).unwrap();
        assert!(list[0].pinned);
        assert_eq!(b.messages(list[0].id, 10, None).unwrap().len(), 2);
        // An outgoing message caught mid-send comes back as failed.
        assert_eq!(
            b.messages(list[0].id, 10, None).unwrap()[1].state,
            MessageState::Failed
        );
        assert!(b.search("two", 5).unwrap().len() == 1);
    }
}
