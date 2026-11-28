//! Raw tables: `raw_events` (append-only, deduplicated Contract #1 events)
//! and `ingest_offsets` (per-collector/session tail cursors). Collectors'
//! facts land here unmodified; nothing in this module ever rewrites or
//! interprets a `json` blob beyond what's needed to populate the indexed
//! `session_id`/`type`/`ts` columns.

use afoot_events::{Envelope, OpaqueEvent};
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeMap;

use crate::{Error, Result, Store};

/// Per-session facts summary: event counts by type plus the session's
/// earliest/latest event timestamp. Built entirely from `raw_events` via
/// SQL aggregation — never loads full event rows into memory.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub session_id: String,
    /// `(type, count)` pairs, ordered by type name.
    pub counts: Vec<(String, u64)>,
    pub first_ts: String,
    pub last_ts: String,
}

/// A session with tool activity but no `llm_call` — see
/// [`Store::sessions_without_llm_calls`].
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryGap {
    pub session_id: String,
    /// Action spans other than the `__session__` bootstrap span.
    pub tool_spans: u64,
    pub last_ts: String,
}

impl Store {
    /// Sessions active since `since` (RFC 3339, compared as stored) that
    /// recorded tool spans but no `llm_call`: hooks arrived, token
    /// telemetry never did. The usual cause is an agent process started
    /// before its OTLP settings were written — hooks reload while it runs,
    /// its exporter does not. Most recent first.
    pub fn sessions_without_llm_calls(&self, since: &str) -> Result<Vec<TelemetryGap>> {
        let mut stmt = self.0.prepare(
            "SELECT session_id,
                    SUM(type = 'action_span'
                        AND COALESCE(json_extract(json, '$.payload.tool_name'), '') <> '__session__')
                        AS tool_spans,
                    MAX(ts) AS last_ts
             FROM raw_events
             GROUP BY session_id
             HAVING SUM(type = 'llm_call') = 0
                AND tool_spans > 0
                AND last_ts >= ?1
             ORDER BY last_ts DESC, session_id ASC",
        )?;
        let rows = stmt.query_map([since], |row| {
            Ok(TelemetryGap {
                session_id: row.get(0)?,
                tool_spans: row.get::<_, i64>(1)? as u64,
                last_ts: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Every `llm_call` whose stored timestamp is between `from` and `to`
    /// **as text**, oldest first.
    ///
    /// Stored timestamps do not all carry the same precision, and text
    /// order is not time order across them: a caller cutting on instants
    /// passes bounds wider than its period and compares parsed values.
    pub fn llm_calls_with_ts_between(&self, from: &str, to: &str) -> Result<Vec<Envelope>> {
        let mut stmt = self.0.prepare(
            "SELECT json FROM raw_events
             WHERE type = 'llm_call' AND ts >= ?1 AND ts <= ?2
             ORDER BY ts ASC, event_id ASC",
        )?;
        let rows = stmt.query_map(params![from, to], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?).map_err(Error::from)?);
        }
        Ok(events)
    }

    /// For each collector and session with an event whose stored timestamp
    /// is at or after `from` **as text**: `(collector, session, timestamps)`,
    /// the timestamps as stored. Text order is not time order across
    /// precisions (see [`Self::llm_calls_with_ts_between`]): pass a bound
    /// earlier than the instant meant and compare parsed values.
    pub fn sessions_with_events_since(&self, from: &str) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.0.prepare(
            "SELECT DISTINCT json_extract(json, '$.collector.name'), session_id, ts
             FROM raw_events
             WHERE ts >= ?1 AND json_extract(json, '$.collector.name') IS NOT NULL",
        )?;
        let rows = stmt.query_map([from], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The latest timestamp of any stored event, as stored.
    pub fn last_event_ts(&self) -> Result<Option<String>> {
        Ok(self
            .0
            .query_row("SELECT MAX(ts) FROM raw_events", [], |row| {
                row.get::<_, Option<String>>(0)
            })?)
    }

    /// Distinct sessions that have events from `collector`, sorted.
    pub fn session_ids_for_collector(&self, collector: &str) -> Result<Vec<String>> {
        let mut stmt = self.0.prepare(
            "SELECT DISTINCT session_id FROM raw_events
             WHERE json_extract(json, '$.collector.name') = ?1
             ORDER BY session_id",
        )?;
        let rows = stmt.query_map([collector], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn insert_opaque_events(&mut self, events: &[OpaqueEvent]) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let tx = self.0.savepoint()?;
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO opaque_events
                 (event_id, schema_version, session_id, type, ts, collector, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for event in events {
                inserted += stmt.execute(params![
                    event.event_id,
                    event.schema_version,
                    event.session_id,
                    event.type_tag,
                    event.ts,
                    event.collector.name,
                    event.json.to_string(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    pub fn count_opaque_events(&self) -> Result<u64> {
        let count: i64 = self
            .0
            .query_row("SELECT COUNT(*) FROM opaque_events", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Inserts `evts` into `raw_events`, deduplicating on `event_id`
    /// (`INSERT OR IGNORE`) inside a single transaction. Returns the number
    /// of rows actually inserted — events already present are silently
    /// skipped and don't count.
    pub fn insert_events(&mut self, evts: &[Envelope]) -> Result<usize> {
        if evts.is_empty() {
            return Ok(0);
        }
        let tx = self.0.savepoint()?;
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO raw_events (event_id, session_id, type, ts, json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for evt in evts {
                // Serialized through a `Value` deliberately, not with
                // `to_string` on the envelope directly: `serde_json::Map` is
                // a `BTreeMap` here, so the intermediate value canonicalizes
                // the object's keys into sorted order. That makes the stored
                // blob a function of the event's *content* alone — reordering
                // a field in the Rust struct does not rewrite the meaning of
                // every row already on disk, and two runs over the same event
                // store the same bytes. The `type` column comes off the
                // payload discriminant instead of being fished back out of
                // the value it was just written into.
                let json = serde_json::to_string(&serde_json::to_value(evt).map_err(Error::from)?)
                    .map_err(Error::from)?;

                let changed = stmt.execute(params![
                    evt.event_id,
                    evt.session_id,
                    evt.type_tag(),
                    evt.ts,
                    json
                ])?;
                inserted += changed;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Returns all events for `session`, ordered by `ts` then `event_id`
    /// ascending.
    ///
    /// The `event_id` tiebreaker is not cosmetic: collectors routinely emit
    /// several events with the same `ts` (millisecond precision, a
    /// `PostToolUse` closing a span and its `process_sample`), and SQLite
    /// promises nothing about how it breaks a tie. Downstream this order
    /// decides the order floating-point energy is summed in, so without it
    /// two runs over identical data could produce join records differing in
    /// the last bit — and `afoot replay`'s byte-identical guarantee would be
    /// luck rather than a property.
    ///
    /// A stored row that fails to deserialize back into an [`Envelope`] is
    /// treated as data corruption and surfaced as an error, not skipped —
    /// every row in `raw_events` was written from a validated `Envelope` by
    /// [`Store::insert_events`], so a deserialization failure means the
    /// on-disk state itself is broken.
    pub fn events_for_session(&self, session: &str) -> Result<Vec<Envelope>> {
        let mut stmt = self.0.prepare(
            "SELECT json FROM raw_events WHERE session_id = ?1
                 ORDER BY ts ASC, event_id ASC",
        )?;

        let rows = stmt.query_map(params![session], |row| row.get::<_, String>(0))?;

        let mut events = Vec::new();
        for row in rows {
            let json = row?;
            let envelope: Envelope = serde_json::from_str(&json).map_err(Error::from)?;
            events.push(envelope);
        }
        Ok(events)
    }

    /// Returns every distinct non-empty `session_meta.geo_zone` present in
    /// `raw_events`, sorted ascending.
    ///
    /// The zone is user-configured and never auto-detected (Contract #1),
    /// so this is the only place the control plane can learn it without
    /// asking. Several sessions may declare different zones; the caller
    /// decides what to do about that rather than having a choice made for
    /// it here.
    pub fn declared_geo_zones(&self) -> Result<Vec<String>> {
        let mut stmt = self.0.prepare(
            "SELECT DISTINCT json_extract(json, '$.payload.geo_zone') AS zone
             FROM raw_events
             WHERE type = 'session_meta' AND zone IS NOT NULL AND zone <> ''
             ORDER BY zone ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut zones = Vec::new();
        for row in rows {
            zones.push(row?);
        }
        Ok(zones)
    }

    /// Returns the stored tail offset for `(collector, session)`, or `0` if
    /// no offset has been recorded yet.
    pub fn get_offset(&self, collector: &str, session: &str) -> Result<u64> {
        // Cached: a full reconciliation asks once per spool file.
        let offset: Option<i64> = self
            .0
            .prepare_cached(
                "SELECT offset FROM ingest_offsets WHERE collector = ?1 AND session_id = ?2",
            )?
            .query_row(params![collector, session], |row| row.get(0))
            .optional()?;
        Ok(offset.unwrap_or(0) as u64)
    }

    /// Upserts the tail offset for `(collector, session)`.
    pub fn set_offset(&mut self, collector: &str, session: &str, offset: u64) -> Result<()> {
        self.0.execute(
            "INSERT INTO ingest_offsets (collector, session_id, offset)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(collector, session_id) DO UPDATE SET offset = excluded.offset",
            params![collector, session, offset as i64],
        )?;
        Ok(())
    }

    /// Returns one [`SessionSummary`] per distinct `session_id` present in
    /// `raw_events`, sorted by `session_id`, computed via two aggregate
    /// queries (counts by type, and min/max `ts`) rather than by loading
    /// every event into memory.
    pub fn session_summaries(&self) -> Result<Vec<SessionSummary>> {
        let mut counts_by_session: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
        {
            let mut stmt = self.0.prepare(
                "SELECT session_id, type, COUNT(*) FROM raw_events
                 GROUP BY session_id, type
                 ORDER BY session_id, type",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as u64,
                ))
            })?;
            for row in rows {
                let (session_id, type_tag, count) = row?;
                counts_by_session
                    .entry(session_id)
                    .or_default()
                    .push((type_tag, count));
            }
        }

        let mut summaries = Vec::new();
        {
            let mut stmt = self.0.prepare(
                "SELECT session_id, MIN(ts), MAX(ts) FROM raw_events
                 GROUP BY session_id
                 ORDER BY session_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (session_id, first_ts, last_ts) = row?;
                let counts = counts_by_session.remove(&session_id).unwrap_or_default();
                summaries.push(SessionSummary {
                    session_id,
                    counts,
                    first_ts,
                    last_ts,
                });
            }
        }

        Ok(summaries)
    }
}
