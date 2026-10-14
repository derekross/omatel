// SPDX-License-Identifier: GPL-3.0-or-later
//! The block list both apps share (`blocked.db`). A blocked number's calls
//! are hung up and logged as blocked; its messages are kept, hidden, and
//! raise no notification.

use std::path::Path;

use omatel_contacts::number;
use rusqlite::{params, Connection};

use crate::{now_ms, open, Result};

const MIGRATIONS: &[&str] = &["CREATE TABLE blocked (
        number TEXT PRIMARY KEY,
        raw TEXT NOT NULL,
        added_at INTEGER NOT NULL
    );"];

pub struct BlockList {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Blocked {
    pub number: String,
    pub raw: String,
    pub added_at: i64,
}

impl BlockList {
    pub fn open(path: &Path) -> Result<BlockList> {
        Ok(BlockList {
            conn: open(Some(path), MIGRATIONS)?,
        })
    }

    pub fn open_in_memory() -> Result<BlockList> {
        Ok(BlockList {
            conn: open(None, MIGRATIONS)?,
        })
    }

    pub fn add(&self, raw: &str, region: &str) -> Result<()> {
        let key = number::normalize(raw, region);
        self.conn.execute(
            "INSERT OR IGNORE INTO blocked (number, raw, added_at) VALUES (?1, ?2, ?3)",
            params![key, raw.trim(), now_ms()],
        )?;
        Ok(())
    }

    /// Unblock every entry that names this number.
    pub fn remove(&self, raw: &str, region: &str) -> Result<usize> {
        let mut n = 0;
        for b in self.list()? {
            if b.number == raw || number::same_number(&b.number, raw, region) {
                n += self
                    .conn
                    .execute("DELETE FROM blocked WHERE number = ?1", [&b.number])?;
            }
        }
        Ok(n)
    }

    pub fn list(&self) -> Result<Vec<Blocked>> {
        let mut st = self
            .conn
            .prepare("SELECT number, raw, added_at FROM blocked ORDER BY added_at DESC")?;
        let rows = st.query_map([], |r| {
            Ok(Blocked {
                number: r.get(0)?,
                raw: r.get(1)?,
                added_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn is_blocked(&self, raw: &str, region: &str) -> Result<bool> {
        if raw.trim().is_empty() {
            return Ok(false);
        }
        Ok(self
            .list()?
            .iter()
            .any(|b| number::same_number(&b.number, raw, region)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_match_and_unblock() {
        let b = BlockList::open_in_memory().unwrap();
        b.add("(202) 555-0150", "US").unwrap();
        b.add("+12025550150", "US").unwrap(); // the same key: ignored
        assert_eq!(b.list().unwrap().len(), 1);
        assert!(b.is_blocked("+1 202 555 0150", "US").unwrap());
        assert!(!b.is_blocked("+12025550151", "US").unwrap());
        assert!(!b.is_blocked("", "US").unwrap());
        b.add("SPAMCO", "US").unwrap();
        assert!(b.is_blocked("spamco", "US").unwrap());
        assert_eq!(b.remove("2025550150", "US").unwrap(), 1);
        assert!(!b.is_blocked("+12025550150", "US").unwrap());
    }

    #[test]
    fn file_is_private() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/blocked.db");
        BlockList::open(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dmode = std::fs::metadata(p.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dmode, 0o700);
    }
}
