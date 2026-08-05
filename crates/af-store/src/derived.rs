//! Derived tables: `impact_estimates` and `impact_joins`. Everything here
//! is rebuildable from raw data plus the estimation methodology —
//! [`Store::wipe_derived`] clears both tables without touching raw data, so
//! a methodology change can be replayed from scratch.
//!
//! `impact_joins` is keyed by an opaque `unit_key` built by the join
//! assembler (`af-core::join`): `session:<session_id>`,
//! `task:<session_id>:<task_id>`, `tool_call:<session_id>:<span_id>`. The
//! key is deliberately *not* parsed here — [`Store::joins_for_session`]
//! filters on the stored record's own `unit.session_id` via SQLite's
//! `json_extract`, so a future key scheme costs no migration and no query
//! change.

use af_events::Envelope;
use rusqlite::params;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{Error, Result, Store};

/// Ids per `WHERE event_id IN (...)` batch in [`Store::estimates_for_events`].
///
/// SQLite's compiled-in host-parameter limit is 999 on older builds (32766
/// on current ones); 500 sits comfortably under the floor, so the query is
/// safe against whichever `libsqlite3` the binary ends up linked to, and is
/// already far past the point where per-statement overhead matters.
const ESTIMATE_BATCH: usize = 500;

/// One impact estimate for a raw event. `json` carries the full estimate
/// blob (ranges, methodology detail); `methodology_version` and `event_id`
/// are broken out because they're queried/joined on directly.
///
/// No separate `status` column: the Task 7 estimator sidecar's full
/// response — including its `status` (`ok`/`unknown_model`/`missing_zone`/
/// `error`) — is stored verbatim in `json`, plus the `zone` the pass ran
/// under (stamped by `af-core::estimate_pending`, since the sidecar does
/// not echo it back). Failed/unestimable calls are upserted just like
/// successful ones (queryable, never silently dropped or retried forever)
/// without widening this struct or the table schema.
///
/// Nothing mints a schema-valid Contract #2 `impact_estimate` record from
/// these blobs. The join assembler (`af-core::join`) reads them back and
/// *tallies* their raw `status` strings into
/// `remote_estimated.estimate_status_counts`, treating an absent row as
/// `pending`; it validates neither the blob nor the tally against the
/// derived schema, and `estimation_status` is never materialised as a
/// field anywhere. That remains open if Contract #2 is made normative.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactEstimate {
    pub event_id: String,
    pub methodology_version: String,
    pub json: serde_json::Value,
}

impl Store {
    /// Inserts or replaces the impact estimate for `e.event_id`.
    pub fn upsert_estimate(&mut self, e: &ImpactEstimate) -> Result<()> {
        self.0.execute(
            "INSERT INTO impact_estimates (event_id, json, methodology_version)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(event_id) DO UPDATE SET
                json = excluded.json,
                methodology_version = excluded.methodology_version",
            params![e.event_id, e.json.to_string(), e.methodology_version],
        )?;
        Ok(())
    }

    /// Returns the stored estimate blobs for `event_ids`, keyed by event id.
    ///
    /// Events with no `impact_estimates` row are simply absent from the map
    /// — that absence is what the join assembler reports as
    /// `estimation_status: "pending"`, so it must not be conflated with a
    /// stored `{"status": ...}` failure blob.
    ///
    /// Fetched in chunked `WHERE event_id IN (...)` batches rather than one
    /// round trip per id: a session with a few thousand `llm_call`s paid a
    /// few thousand statement executions for what SQLite can answer in a
    /// handful of index lookups. The result is a `BTreeMap`, so it is keyed
    /// and ordered by event id whatever order the rows come back in — the
    /// batching cannot perturb what the caller sees.
    pub fn estimates_for_events(&self, event_ids: &[String]) -> Result<BTreeMap<String, Value>> {
        let mut out = BTreeMap::new();
        for chunk in event_ids.chunks(ESTIMATE_BATCH) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let mut stmt = self.0.prepare(&format!(
                "SELECT event_id, json FROM impact_estimates WHERE event_id IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (event_id, json) = row?;
                out.insert(event_id, serde_json::from_str(&json).map_err(Error::from)?);
            }
        }
        Ok(out)
    }

    /// Inserts or replaces the `impact_join` record stored under `unit_key`.
    pub fn upsert_join(&mut self, unit_key: &str, json: &Value) -> Result<()> {
        self.0.execute(
            "INSERT INTO impact_joins (unit_key, json)
             VALUES (?1, ?2)
             ON CONFLICT(unit_key) DO UPDATE SET json = excluded.json",
            params![unit_key, json.to_string()],
        )?;
        Ok(())
    }

    /// Returns every stored join whose record names `session` in
    /// `unit.session_id`, as `(unit_key, record)` pairs ordered by
    /// `unit_key` — the emission order the read model depends on, fixed in
    /// SQL rather than left to a caller's sort.
    pub fn joins_for_session(&self, session: &str) -> Result<Vec<(String, Value)>> {
        let mut stmt = self.0.prepare(
            "SELECT unit_key, json FROM impact_joins
             WHERE json_extract(json, '$.unit.session_id') = ?1
             ORDER BY unit_key ASC",
        )?;
        let rows = stmt.query_map(params![session], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (unit_key, json) = row?;
            out.push((unit_key, serde_json::from_str(&json).map_err(Error::from)?));
        }
        Ok(out)
    }

    /// Deletes all rows from `impact_estimates` and `impact_joins`, leaving
    /// `raw_events` and `ingest_offsets` untouched.
    pub fn wipe_derived(&mut self) -> Result<()> {
        let tx = self.0.transaction()?;
        tx.execute("DELETE FROM impact_estimates", [])?;
        tx.execute("DELETE FROM impact_joins", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Returns all `llm_call` events in `raw_events` that don't yet have a
    /// row in `impact_estimates`, ordered by `ts` then `event_id` ascending
    /// (same tiebreaker rationale as [`Store::events_for_session`]: the
    /// order fixes the sidecar request sequence, which golden-transcript
    /// tests replay). This is the estimator's
    /// (`af-core::estimate_pending`) work queue.
    ///
    /// The one place raw and derived tables are joined at the SQL level
    /// (see the crate-level doc) — a read-only LEFT JOIN, so it doesn't
    /// compromise "derived is fully rebuildable from raw".
    pub fn llm_calls_without_estimate(&self) -> Result<Vec<Envelope>> {
        self.llm_calls_without_estimate_limit(None)
    }

    /// The oldest pending `llm_call` events, capped for a bounded resident
    /// worker. `None` preserves the full synchronous report/replay behavior.
    pub fn llm_calls_without_estimate_limit(&self, limit: Option<usize>) -> Result<Vec<Envelope>> {
        let limit = limit.map(|value| value.min(i64::MAX as usize) as i64);
        let mut stmt = self.0.prepare(
            "SELECT r.json FROM raw_events r
             LEFT JOIN impact_estimates e ON e.event_id = r.event_id
             WHERE r.type = 'llm_call' AND e.event_id IS NULL
             ORDER BY r.ts ASC, r.event_id ASC
             LIMIT COALESCE(?1, -1)",
        )?;
        let rows = stmt.query_map([limit], |row| row.get::<_, String>(0))?;

        let mut events = Vec::new();
        for row in rows {
            let json = row?;
            let envelope: Envelope = serde_json::from_str(&json).map_err(Error::from)?;
            events.push(envelope);
        }
        Ok(events)
    }

    /// How many `llm_call` events still have no `impact_estimates` row —
    /// [`Store::llm_calls_without_estimate`]'s count, over the same
    /// LEFT JOIN, without materializing and deserializing every envelope.
    ///
    /// Exists because the honest headline of a degraded estimation pass is
    /// a *number*, and computing it by loading the whole backlog into
    /// `Vec<Envelope>` just to call `.len()` makes the cost of reporting
    /// "3 calls are pending" scale with a backlog nobody is about to read.
    pub fn count_llm_calls_without_estimate(&self) -> Result<u64> {
        let count: i64 = self.0.query_row(
            "SELECT COUNT(*) FROM raw_events r
             LEFT JOIN impact_estimates e ON e.event_id = r.event_id
             WHERE r.type = 'llm_call' AND e.event_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
}
