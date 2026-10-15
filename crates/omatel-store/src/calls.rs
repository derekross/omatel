// SPDX-License-Identifier: GPL-3.0-or-later
//! The call log (`calls.db`), written by the agent from call state changes
//! (the modem keeps no history), and the favorites order.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::{open, Error, Result, EXPORT_VERSION};

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE calls (
        id INTEGER PRIMARY KEY,
        number TEXT NOT NULL,
        raw_number TEXT NOT NULL,
        direction TEXT NOT NULL CHECK (direction IN ('in', 'out')),
        outcome TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        answered_at INTEGER,
        ended_at INTEGER,
        duration INTEGER NOT NULL DEFAULT 0,
        seen INTEGER NOT NULL DEFAULT 1
    );
    CREATE INDEX calls_started ON calls (started_at DESC);
    CREATE INDEX calls_number ON calls (number);
    CREATE TABLE favourites ( -- spelling: kept (schema 1 as shipped; schema 2 renames it)
        uid TEXT PRIMARY KEY,
        position INTEGER NOT NULL
    );",
    // Schema 2: US English. A rename, so databases from 0.1.0 keep their
    // order.
    "ALTER TABLE favourites RENAME TO favorites; -- spelling: kept",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallDirection {
    In,
    Out,
}

impl CallDirection {
    fn as_str(self) -> &'static str {
        match self {
            CallDirection::In => "in",
            CallDirection::Out => "out",
        }
    }
}

/// How a call ended, for the log's icon and the missed-call count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallOutcome {
    /// Talked (either direction).
    Answered,
    /// Incoming, rang out or the caller gave up.
    Missed,
    /// Incoming, declined here.
    Rejected,
    /// Incoming from a blocked number, hung up at once.
    Blocked,
    /// Outgoing, never answered (busy, no answer, canceled).
    Unanswered,
    /// Outgoing, the network or modem refused it.
    Failed,
}

impl CallOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CallOutcome::Answered => "answered",
            CallOutcome::Missed => "missed",
            CallOutcome::Rejected => "rejected",
            CallOutcome::Blocked => "blocked",
            CallOutcome::Unanswered => "unanswered",
            CallOutcome::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> CallOutcome {
        match s {
            "answered" => CallOutcome::Answered,
            "missed" => CallOutcome::Missed,
            "rejected" => CallOutcome::Rejected,
            "blocked" => CallOutcome::Blocked,
            "unanswered" => CallOutcome::Unanswered,
            _ => CallOutcome::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecord {
    #[serde(default)]
    pub id: i64,
    /// The normalized number (E.164 when it parses).
    pub number: String,
    /// The number as the network gave it.
    pub raw_number: String,
    pub direction: CallDirection,
    pub outcome: CallOutcome,
    /// Milliseconds since the epoch.
    pub started_at: i64,
    pub answered_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// Seconds talked.
    pub duration: i64,
    /// False for a missed call nobody has looked at yet.
    #[serde(default = "yes")]
    pub seen: bool,
}

fn yes() -> bool {
    true
}

fn record(r: &Row) -> rusqlite::Result<CallRecord> {
    Ok(CallRecord {
        id: r.get(0)?,
        number: r.get(1)?,
        raw_number: r.get(2)?,
        direction: if r.get::<_, String>(3)? == "in" {
            CallDirection::In
        } else {
            CallDirection::Out
        },
        outcome: CallOutcome::parse(&r.get::<_, String>(4)?),
        started_at: r.get(5)?,
        answered_at: r.get(6)?,
        ended_at: r.get(7)?,
        duration: r.get(8)?,
        seen: r.get(9)?,
    })
}

const COLUMNS: &str =
    "id, number, raw_number, direction, outcome, started_at, answered_at, ended_at, duration, seen";

pub struct CallLog {
    conn: Connection,
}

#[derive(Serialize, Deserialize)]
struct Export {
    format: String,
    version: u32,
    calls: Vec<CallRecord>,
}

impl CallLog {
    pub fn open(path: &Path) -> Result<CallLog> {
        Ok(CallLog {
            conn: open(Some(path), MIGRATIONS)?,
        })
    }

    pub fn open_in_memory() -> Result<CallLog> {
        Ok(CallLog {
            conn: open(None, MIGRATIONS)?,
        })
    }

    pub fn insert(&self, c: &CallRecord) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO calls (number, raw_number, direction, outcome, started_at, answered_at,
                                ended_at, duration, seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                c.number,
                c.raw_number,
                c.direction.as_str(),
                c.outcome.as_str(),
                c.started_at,
                c.answered_at,
                c.ended_at,
                c.duration,
                c.seen
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest calls first.
    pub fn recent(&self, limit: usize) -> Result<Vec<CallRecord>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM calls ORDER BY started_at DESC, id DESC LIMIT ?1"
        ))?;
        let rows = st.query_map([limit as i64], record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The calls with one (normalized) number, newest first.
    pub fn for_number(&self, number: &str) -> Result<Vec<CallRecord>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM calls WHERE number = ?1 ORDER BY started_at DESC, id DESC"
        ))?;
        let rows = st.query_map([number], record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<CallRecord>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM calls WHERE id = ?1"),
                [id],
                record,
            )
            .optional()?)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM calls WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn delete_number(&self, number: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM calls WHERE number = ?1", [number])?)
    }

    pub fn clear(&self) -> Result<()> {
        self.conn.execute("DELETE FROM calls", [])?;
        Ok(())
    }

    /// Missed calls nobody has looked at.
    pub fn unseen_missed(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM calls WHERE outcome = 'missed' AND seen = 0",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn mark_all_seen(&self) -> Result<()> {
        self.conn
            .execute("UPDATE calls SET seen = 1 WHERE seen = 0", [])?;
        Ok(())
    }

    /// Numbers called or calling most often, for suggestions.
    pub fn frequent(&self, limit: usize) -> Result<Vec<(String, String, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT number, max(raw_number), count(*) AS n FROM calls
             WHERE outcome IN ('answered', 'unanswered')
             GROUP BY number ORDER BY n DESC, max(started_at) DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The favorites order: contact UIDs, first to last.
    pub fn favorites_order(&self) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT uid FROM favorites ORDER BY position")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_favorites_order(&self, uids: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM favorites", [])?;
        for (i, u) in uids.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO favorites (uid, position) VALUES (?1, ?2)",
                params![u, i as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The whole log as our versioned JSON (docs/export-format.md).
    pub fn export_json(&self) -> Result<String> {
        let mut calls = self.recent(usize::MAX >> 1)?;
        calls.reverse();
        Ok(serde_json::to_string_pretty(&Export {
            format: "omatel-calls".into(),
            version: EXPORT_VERSION,
            calls,
        })?)
    }

    /// Import an export; calls already present (same number, direction and
    /// start) are skipped. Returns how many were added.
    pub fn import_json(&self, text: &str) -> Result<usize> {
        let e: Export = serde_json::from_str(text)?;
        if e.format != "omatel-calls" || e.version > EXPORT_VERSION {
            return Err(Error::Format("not an omatel call log export".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut added = 0;
        for c in e.calls {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM calls WHERE number = ?1 AND direction = ?2 AND started_at = ?3)",
                params![c.number, c.direction.as_str(), c.started_at],
                |r| r.get(0),
            )?;
            if !exists {
                tx.execute(
                    "INSERT INTO calls (number, raw_number, direction, outcome, started_at,
                                        answered_at, ended_at, duration, seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
                    params![
                        c.number,
                        c.raw_number,
                        c.direction.as_str(),
                        c.outcome.as_str(),
                        c.started_at,
                        c.answered_at,
                        c.ended_at,
                        c.duration
                    ],
                )?;
                added += 1;
            }
        }
        tx.commit()?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(number: &str, dir: CallDirection, outcome: CallOutcome, at: i64) -> CallRecord {
        CallRecord {
            id: 0,
            number: number.into(),
            raw_number: number.into(),
            direction: dir,
            outcome,
            started_at: at,
            answered_at: None,
            ended_at: Some(at + 1000),
            duration: 0,
            seen: outcome != CallOutcome::Missed,
        }
    }

    #[test]
    fn log_order_missed_and_delete() {
        let log = CallLog::open_in_memory().unwrap();
        log.insert(&call(
            "+12025550100",
            CallDirection::In,
            CallOutcome::Missed,
            1000,
        ))
        .unwrap();
        let id = log
            .insert(&call(
                "+12025550101",
                CallDirection::Out,
                CallOutcome::Answered,
                2000,
            ))
            .unwrap();
        let recent = log.recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].number, "+12025550101");
        assert_eq!(log.unseen_missed().unwrap(), 1);
        log.mark_all_seen().unwrap();
        assert_eq!(log.unseen_missed().unwrap(), 0);
        assert_eq!(log.for_number("+12025550100").unwrap().len(), 1);
        log.delete(id).unwrap();
        assert_eq!(log.recent(10).unwrap().len(), 1);
        assert_eq!(log.get(id).unwrap(), None);
    }

    #[test]
    fn export_import_round_trip() {
        let a = CallLog::open_in_memory().unwrap();
        a.insert(&call(
            "+12025550100",
            CallDirection::In,
            CallOutcome::Answered,
            1,
        ))
        .unwrap();
        a.insert(&call(
            "+12025550102",
            CallDirection::Out,
            CallOutcome::Failed,
            2,
        ))
        .unwrap();
        let json = a.export_json().unwrap();
        assert!(json.contains("\"omatel-calls\""));
        let b = CallLog::open_in_memory().unwrap();
        assert_eq!(b.import_json(&json).unwrap(), 2);
        assert_eq!(b.import_json(&json).unwrap(), 0);
        assert_eq!(b.recent(10).unwrap()[0].outcome, CallOutcome::Failed);
        assert!(b
            .import_json("{\"format\":\"x\",\"version\":1,\"calls\":[]}")
            .is_err());
    }

    #[test]
    fn schema_1_database_keeps_its_favorites() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("calls.db");
        {
            let conn = Connection::open(&p).unwrap();
            crate::migrate(&conn, &MIGRATIONS[..1]).unwrap();
            conn.execute(
                "INSERT INTO favourites (uid, position) VALUES ('a', 0)", // spelling: kept
                [],
            )
            .unwrap();
        }
        let log = CallLog::open(&p).unwrap();
        assert_eq!(log.favorites_order().unwrap(), vec!["a"]);
    }

    #[test]
    fn favorites_and_frequent() {
        let log = CallLog::open_in_memory().unwrap();
        log.set_favorites_order(&["b".into(), "a".into()]).unwrap();
        assert_eq!(log.favorites_order().unwrap(), vec!["b", "a"]);
        for t in 0..3 {
            log.insert(&call(
                "+12025550103",
                CallDirection::Out,
                CallOutcome::Answered,
                t,
            ))
            .unwrap();
        }
        log.insert(&call(
            "+12025550104",
            CallDirection::Out,
            CallOutcome::Answered,
            9,
        ))
        .unwrap();
        let f = log.frequent(5).unwrap();
        assert_eq!(f[0].0, "+12025550103");
        assert_eq!(f[0].2, 3);
    }
}
