//! The reject log: why something the receiver was sent could not become a
//! fact. Never the payload — see [`RejectRow`].

use rusqlite::params;

use crate::{Error, Result, Store};

/// How many rejects are kept; older ones are deleted on insert.
pub const REJECT_RING: usize = 1000;

/// One reject. `attribute_keys` are attribute *names*; values are never
/// stored, because a rejected OTLP batch can carry tool arguments, command
/// output or account identifiers.
#[derive(Debug, Clone, PartialEq)]
pub struct RejectRow {
    pub ts: String,
    pub source: String,
    pub reason: String,
    pub attribute_keys: Vec<String>,
    pub bytes: u64,
}

impl Store {
    /// Appends `rows` and trims the log to [`REJECT_RING`], in one
    /// transaction.
    pub fn insert_rejects(&mut self, rows: &[RejectRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.0.savepoint()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO rejects (ts, source, reason, attribute_keys, bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for row in rows {
                let keys = serde_json::to_string(&row.attribute_keys).map_err(Error::from)?;
                stmt.execute(params![
                    row.ts,
                    row.source,
                    row.reason,
                    keys,
                    row.bytes as i64
                ])?;
            }
        }
        tx.execute(
            "DELETE FROM rejects WHERE id <= (SELECT MAX(id) FROM rejects) - ?1",
            params![REJECT_RING as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The most recent `limit` rejects, newest first.
    pub fn recent_rejects(&self, limit: usize) -> Result<Vec<RejectRow>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.0.prepare(
            "SELECT ts, source, reason, attribute_keys, bytes FROM rejects
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (ts, source, reason, keys, bytes) = row?;
            out.push(RejectRow {
                ts,
                source,
                reason,
                attribute_keys: serde_json::from_str(&keys).map_err(Error::from)?,
                bytes: bytes.max(0) as u64,
            });
        }
        Ok(out)
    }
}
