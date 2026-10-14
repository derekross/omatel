// SPDX-License-Identifier: GPL-3.0-or-later
//! The SQLite stores behind Phone and Messages, in `~/.local/share/omatel/`:
//!
//! - `messages.db`: conversations, participants, messages (with a
//!   `transport` column: `sms` now, `mms` from T3, room for anything
//!   later), MMS parts, and an FTS5 index over message text;
//! - `calls.db`: the call log and the favourites order;
//! - `blocked.db`: one block list, shared by both apps.
//!
//! Every database runs in WAL mode with `synchronous=FULL`, so a commit is
//! on disk when it returns: the agent commits an incoming SMS before it
//! deletes it from the modem. Schemas are versioned with `user_version`
//! and migrated forward on open.
//!
//! These files hold numbers and message text: they never go into a log.

pub mod blocked;
pub mod calls;
pub mod messages;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

pub use blocked::{BlockList, Blocked};
pub use calls::{CallDirection, CallLog, CallOutcome, CallRecord};
pub use messages::{
    Conversation, Direction, Message, MessageState, MessageStore, NewIncoming, SearchHit,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// `$XDG_DATA_HOME/omatel`, or `~/.local/share/omatel`.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("omatel");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    Path::new(&home).join(".local/share/omatel")
}

/// Milliseconds since the epoch, now.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Open a database file (creating its directory, mode 0700, and the file,
/// mode 0600) and bring its schema up to date. `None` opens in memory.
fn open(path: Option<&Path>, migrations: &[&str]) -> Result<Connection> {
    let conn = match path {
        Some(p) => {
            if let Some(dir) = p.parent() {
                if !dir.exists() {
                    std::fs::create_dir_all(dir)?;
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
                }
            }
            let fresh = !p.exists();
            let conn = Connection::open(p)?;
            if fresh {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
            }
            conn
        }
        None => Connection::open_in_memory()?,
    };
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn, migrations)?;
    Ok(conn)
}

fn migrate(conn: &Connection, migrations: &[&str]) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version as usize > migrations.len() {
        return Err(Error::Format(format!(
            "database schema {version} is newer than this program ({})",
            migrations.len()
        )));
    }
    for (i, sql) in migrations.iter().enumerate().skip(version as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
        tracing::info!(schema = i + 1, "database migrated");
    }
    Ok(())
}

/// The version stamped into export files.
pub const EXPORT_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_a_newer_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 9).unwrap();
        assert!(migrate(&conn, &["CREATE TABLE t (x);"]).is_err());
    }

    #[test]
    fn migrates_forward_once() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, &["CREATE TABLE a (x);"]).unwrap();
        migrate(&conn, &["CREATE TABLE a (x);", "CREATE TABLE b (y);"]).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);
    }
}
