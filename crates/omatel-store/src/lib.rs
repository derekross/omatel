// SPDX-License-Identifier: GPL-3.0-or-later
//! The SQLite stores behind Phone and Messages, in `~/.local/share/omatel/`:
//!
//! - `messages.db`: conversations, participants, messages (with a
//!   `transport` column: `sms` now, `mms` from T3, room for anything
//!   later), MMS parts, and an FTS5 index over message text;
//! - `calls.db`: the call log and the favorites order;
//! - `blocked.db`: one block list, shared by both apps.
//!
//! Every database runs in WAL mode with `synchronous=FULL`, so a commit is
//! on disk when it returns: the agent commits an incoming SMS before it
//! deletes it from the modem. Schemas are versioned with `user_version`
//! and migrated forward on open. Before a file is migrated, a copy of it at
//! its old schema is kept next to it (`messages.db.schema1`, once per
//! schema), and a schema whose changes can be undone can be taken back
//! ([`MessageStore::downgrade`]) for an older agent.
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
    group_address, Conversation, Direction, Message, MessageState, MessageStore, NewIncoming,
    NewMms, NewPart, Part, SearchHit,
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
/// mode 0600) and bring its schema up to date, keeping a copy at the old
/// schema first. `None` opens in memory.
fn open(path: Option<&Path>, migrations: &[&str]) -> Result<Connection> {
    let conn = connect(path)?;
    if let Some(p) = path {
        keep_before_migrating(&conn, p, migrations.len())?;
    }
    migrate(&conn, migrations)?;
    Ok(conn)
}

/// Open a database with the stores' settings, without touching its schema.
fn connect(path: Option<&Path>) -> Result<Connection> {
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
    Ok(conn)
}

fn user_version(conn: &Connection) -> Result<usize> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    Ok(v.max(0) as usize)
}

/// Before an existing database is migrated, keep a consistent copy of it
/// at its current schema (`<file>.schema<N>`, mode 0600), unless one is
/// there already: an older agent can open that copy.
fn keep_before_migrating(conn: &Connection, path: &Path, latest: usize) -> Result<()> {
    let version = user_version(conn)?;
    if version == 0 || version >= latest {
        return Ok(());
    }
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".schema{version}"));
    let copy = PathBuf::from(name);
    if copy.exists() {
        return Ok(());
    }
    conn.execute("VACUUM INTO ?1", [copy.to_string_lossy()])?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(schema = version, "database kept before migrating");
    Ok(())
}

/// Take a database back to schema `to` with each newer schema's undo
/// (`downs[i]` undoes `migrations[i]`; `None`: it can't be undone).
fn downgrade(conn: &Connection, latest: usize, downs: &[Option<&str>], to: usize) -> Result<()> {
    let version = user_version(conn)?;
    if version > latest {
        return Err(Error::Format(format!(
            "database schema {version} is newer than this program ({latest})"
        )));
    }
    for v in (to + 1..=version).rev() {
        let Some(sql) = downs.get(v - 1).copied().flatten() else {
            return Err(Error::Format(format!("schema {v} can't be taken back")));
        };
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (v - 1) as i64)?;
        tx.commit()?;
        tracing::info!(schema = v - 1, "database taken back");
    }
    Ok(())
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
    fn takes_back_only_what_can_be_undone() {
        let conn = Connection::open_in_memory().unwrap();
        let up = ["CREATE TABLE a (x);", "CREATE TABLE b (y);"];
        migrate(&conn, &up).unwrap();
        assert!(downgrade(&conn, 2, &[None, Some("DROP TABLE b;")], 0).is_err());
        assert_eq!(user_version(&conn).unwrap(), 1);
        downgrade(&conn, 2, &[None, Some("DROP TABLE b;")], 1).unwrap();
        migrate(&conn, &up).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 2);
        conn.pragma_update(None, "user_version", 3).unwrap();
        assert!(downgrade(&conn, 2, &[None, Some("DROP TABLE b;")], 1).is_err());
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
