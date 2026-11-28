//! Derived tables: `impact_estimates` and `impact_joins`. Everything here
//! is rebuildable from raw data plus the estimation methodology —
//! [`Store::wipe_derived`] clears both tables without touching raw data, so
//! a methodology change can be replayed from scratch.
//!
//! `impact_joins` is keyed by an opaque `unit_key` built by the join
//! assembler (`afoot-core::join`): `session:<session_id>`,
//! `task:<session_id>:<task_id>`, `tool_call:<session_id>:<span_id>`. The
//! key is deliberately *not* parsed here — [`Store::joins_for_session`]
//! filters on the stored record's own `unit.session_id` via SQLite's
//! `json_extract`, so a future key scheme costs no migration and no query
//! change.

use afoot_events::Envelope;
use rusqlite::{params, OptionalExtension};
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
/// under (stamped by `afoot-core::estimate_pending`, since the sidecar does
/// not echo it back). Failed/unestimable calls are upserted just like
/// successful ones (queryable, never silently dropped or retried forever)
/// without widening this struct or the table schema.
///
/// Nothing mints a schema-valid Contract #2 `impact_estimate` record from
/// these blobs. The join assembler (`afoot-core::join`) reads them back and
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
        self.upsert_estimates(std::slice::from_ref(e))
    }

    /// Upserts every row in `rows` under one savepoint: all of them, or —
    /// on an error — none. Nested in a caller's transaction it shares that
    /// transaction's commit.
    pub fn upsert_estimates(&mut self, rows: &[ImpactEstimate]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.0.savepoint()?;
        {
            let mut upsert = tx.prepare(
                "INSERT INTO impact_estimates (event_id, json, methodology_version)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(event_id) DO UPDATE SET
                    json = excluded.json,
                    methodology_version = excluded.methodology_version",
            )?;
            for e in rows {
                upsert.execute(params![
                    e.event_id,
                    e.json.to_string(),
                    e.methodology_version
                ])?;
            }
        }
        tx.commit()?;
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
        self.upsert_joins(&[(unit_key.to_string(), json.clone())])
            .map(|_| ())
    }

    /// Writes `joins` in **one** transaction and returns how many rows were
    /// inserted or actually changed.
    ///
    /// A record identical to the stored one is not rewritten (`WHERE ... IS
    /// NOT excluded.json`), so rebuilding a session whose inputs did not
    /// change costs reads, not page writes — unless its `session_id` column
    /// is missing (a row an older binary wrote), which is then filled. Records are serialized through
    /// `serde_json`'s sorted map, so equal values give equal text.
    pub fn upsert_joins(&mut self, joins: &[(String, Value)]) -> Result<usize> {
        if joins.is_empty() {
            return Ok(0);
        }
        let tx = self.0.savepoint()?;
        let mut changed = 0usize;
        {
            // The session column is set apart from the record: an UPDATE
            // that names an indexed column rewrites its index entry even
            // when the value is the same, and the record changes on almost
            // every rebuild while its session never does.
            let mut upsert = tx.prepare(
                "INSERT INTO impact_joins (unit_key, json, session_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(unit_key) DO UPDATE SET json = excluded.json
                 WHERE impact_joins.json IS NOT excluded.json",
            )?;
            let mut set_session = tx.prepare(
                "UPDATE impact_joins SET session_id = ?2
                 WHERE unit_key = ?1 AND session_id IS NOT ?2",
            )?;
            for (unit_key, json) in joins {
                let session = json.pointer("/unit/session_id").and_then(Value::as_str);
                let record = upsert.execute(params![unit_key, json.to_string(), session])?;
                let column = set_session.execute(params![unit_key, session])?;
                changed += (record + column).min(1);
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// The stored join for one unit key, by primary key: what a reader that
    /// shows one record (the status line, every redraw) needs, without
    /// reading and parsing the rest of the session's records.
    pub fn join_by_key(&self, unit_key: &str) -> Result<Option<Value>> {
        let json: Option<String> = self
            .0
            .prepare_cached("SELECT json FROM impact_joins WHERE unit_key = ?1")?
            .query_row(params![unit_key], |row| row.get(0))
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(Error::from))
            .transpose()
    }

    /// Returns every stored join for `session`, as `(unit_key, record)`
    /// pairs ordered by `unit_key` — the emission order the read model
    /// depends on, fixed in SQL rather than left to a caller's sort.
    pub fn joins_for_session(&self, session: &str) -> Result<Vec<(String, Value)>> {
        let mut stmt = self.0.prepare(
            "SELECT unit_key, json FROM impact_joins
             WHERE session_id = ?1
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
        let tx = self.0.savepoint()?;
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
    /// (`afoot-core::estimate_pending`) work queue.
    ///
    /// The one place raw and derived tables are joined at the SQL level
    /// (see the crate-level doc) — a read-only LEFT JOIN, so it doesn't
    /// compromise "derived is fully rebuildable from raw".
    pub fn llm_calls_without_estimate(&self) -> Result<Vec<Envelope>> {
        self.llm_calls_to_estimate(None)
    }

    /// The estimator's work queue: `llm_call` events with no estimate, plus
    /// — when `current_methodology` (e.g. `"ecologits-0.11.2"`) is known —
    /// events whose `unknown_model`/`missing_zone` verdict was given under
    /// a different methodology version. Those verdicts are facts about the
    /// ecologits release that gave them (its model registry, its zones),
    /// so a newer release deserves the question again. Successful
    /// estimates and transient `error`s are never re-queued here.
    pub fn llm_calls_to_estimate(
        &self,
        current_methodology: Option<&str>,
    ) -> Result<Vec<Envelope>> {
        let mut stmt = self.0.prepare(&format!(
            "SELECT r.json FROM raw_events r
             LEFT JOIN impact_estimates e ON e.event_id = r.event_id
             WHERE {PENDING_LLM_CALL}
             ORDER BY r.ts ASC, r.event_id ASC"
        ))?;
        let rows = stmt.query_map(params![current_methodology], |row| row.get::<_, String>(0))?;

        let mut events = Vec::new();
        for row in rows {
            let json = row?;
            let envelope: Envelope = serde_json::from_str(&json).map_err(Error::from)?;
            events.push(envelope);
        }
        Ok(events)
    }

    /// The whole queue's event ids, in queue order: what a resident
    /// estimator starts from, without holding every pending envelope.
    pub fn llm_call_ids_to_estimate(
        &self,
        current_methodology: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut stmt = self.0.prepare(&format!(
            "SELECT r.event_id FROM raw_events r
             LEFT JOIN impact_estimates e ON e.event_id = r.event_id
             WHERE {PENDING_LLM_CALL}
             ORDER BY r.ts ASC, r.event_id ASC"
        ))?;
        let rows = stmt.query_map(params![current_methodology], |row| row.get::<_, String>(0))?;
        rows.map(|row| row.map_err(Error::from)).collect()
    }

    /// Which of `event_ids` are pending under the queue's predicate, as
    /// envelopes in `(ts, event_id)` order. Point lookups by id: the
    /// resident estimator asks about the calls it was just handed instead
    /// of rescanning every stored call to find them.
    pub fn llm_calls_pending_among(
        &self,
        event_ids: &[String],
        current_methodology: Option<&str>,
    ) -> Result<Vec<Envelope>> {
        let mut stmt = self.0.prepare_cached(&format!(
            "SELECT r.ts, r.json FROM raw_events r
             LEFT JOIN impact_estimates e ON e.event_id = r.event_id
             WHERE r.event_id = ?2 AND {PENDING_LLM_CALL}"
        ))?;
        let mut found = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for event_id in event_ids {
            if !seen.insert(event_id.as_str()) {
                continue;
            }
            let row = stmt
                .query_row(params![current_methodology, event_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .optional()?;
            if let Some((ts, json)) = row {
                let envelope: Envelope = serde_json::from_str(&json).map_err(Error::from)?;
                found.push((ts, envelope));
            }
        }
        found.sort_by(|(a_ts, a), (b_ts, b)| (a_ts, &a.event_id).cmp(&(b_ts, &b.event_id)));
        Ok(found.into_iter().map(|(_, envelope)| envelope).collect())
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
        self.count_llm_calls_to_estimate(None)
    }

    /// [`Store::llm_calls_to_estimate`]'s count, over the same
    /// predicate.
    pub fn count_llm_calls_to_estimate(&self, current_methodology: Option<&str>) -> Result<u64> {
        let count: i64 = self.0.query_row(
            &format!(
                "SELECT COUNT(*) FROM raw_events r
                 LEFT JOIN impact_estimates e ON e.event_id = r.event_id
                 WHERE {PENDING_LLM_CALL}"
            ),
            params![current_methodology],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
}

/// The pending-`llm_call` predicate over `raw_events r LEFT JOIN
/// impact_estimates e`, with the current methodology version bound as `?1`
/// (NULL: only never-estimated calls). Shared by the queue and its count so
/// the two cannot disagree.
const PENDING_LLM_CALL: &str = "r.type = 'llm_call' AND (
        e.event_id IS NULL
        OR (?1 IS NOT NULL
            AND e.methodology_version <> ?1
            AND json_extract(e.json, '$.status') IN ('unknown_model', 'missing_zone'))
    )";
