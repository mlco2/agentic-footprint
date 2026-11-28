//! Integration tests for `afoot-store`'s public interface: raw event
//! insertion/dedup, offset persistence across close/reopen, and derived-vs-raw
//! table isolation.

use afoot_events::{fixtures, Envelope, Payload};
use afoot_store::Store;
use std::path::Path;

/// The instant every fixture event carries unless a test overrides it —
/// the store's ordering tests are about the tiebreaker, so a shared `ts`
/// is the interesting default, not an accident.
const TS: &str = "2026-07-25T12:00:00Z";

fn llm_call_envelope(event_id: &str, session_id: &str) -> Envelope {
    fixtures::envelope(
        event_id,
        session_id,
        TS,
        Payload::LlmCall(fixtures::llm_call()),
    )
}

fn sample_envelope(event_id: &str, session_id: &str) -> Envelope {
    fixtures::envelope(
        event_id,
        session_id,
        TS,
        Payload::SessionMeta(fixtures::session_meta()),
    )
}

#[test]
fn inserting_same_event_twice_dedups_on_event_id() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    let evt = sample_envelope("evt-1", "session-1");

    let first = store
        .insert_events(std::slice::from_ref(&evt))
        .expect("first insert");
    assert_eq!(first, 1);

    let second = store.insert_events(&[evt]).expect("second insert (dup)");
    assert_eq!(second, 0);

    let events = store
        .events_for_session("session-1")
        .expect("read back session events");
    assert_eq!(events.len(), 1);
}

#[test]
fn events_for_session_returns_in_ts_order() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");

    let mut later = sample_envelope("evt-later", "session-1");
    later.ts = "2026-07-25T12:00:10Z".to_string();
    let mut earlier = sample_envelope("evt-earlier", "session-1");
    earlier.ts = "2026-07-25T12:00:00Z".to_string();

    store
        .insert_events(&[later.clone(), earlier.clone()])
        .expect("insert both");

    let events = store
        .events_for_session("session-1")
        .expect("read back session events");
    assert_eq!(events, vec![earlier, later]);
}

#[test]
fn offsets_survive_close_and_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.db");

    {
        let mut store = Store::open(&db_path).expect("open on-disk store");
        store
            .set_offset("otlp-cc", "session-1", 4096)
            .expect("set offset");
    } // store (and its Connection) dropped here, closing the db

    let store = Store::open(&db_path).expect("reopen on-disk store");
    let offset = store
        .get_offset("otlp-cc", "session-1")
        .expect("get offset");
    assert_eq!(offset, 4096);
}

/// `afoot watch` is resident and writing while the user runs `afoot report` over
/// the same state dir. Under the rollback journal that pair excludes itself
/// (`database is locked`); WAL is what makes it a normal case rather than a
/// spurious failure, so the mode is asserted on the *file* and the overlap
/// is exercised for real.
#[test]
fn a_file_store_is_wal_so_a_reader_and_a_writer_can_overlap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.db");
    let mut writer = Store::open(&db_path).expect("open on-disk store");
    writer
        .insert_events(&[llm_call_envelope("evt-1", "session-1")])
        .expect("first insert");

    let mode: String = rusqlite::Connection::open(&db_path)
        .expect("second connection")
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal mode");
    assert_eq!(mode.to_lowercase(), "wal");

    // An open read transaction on another connection — `afoot report` mid-query.
    let reader = rusqlite::Connection::open(&db_path).expect("reader connection");
    reader
        .execute_batch("BEGIN DEFERRED; SELECT count(*) FROM raw_events;")
        .expect("start a read transaction");

    writer
        .insert_events(&[llm_call_envelope("evt-2", "session-1")])
        .expect("a writer must not be locked out by a concurrent reader");

    reader
        .execute_batch("COMMIT;")
        .expect("end read transaction");
    assert_eq!(
        writer
            .events_for_session("session-1")
            .expect("read back")
            .len(),
        2
    );
}

/// `:memory:` has no file to write a `-wal` sidecar to; SQLite keeps it in
/// `memory` mode and the open path must not treat that as a failure.
#[test]
fn an_in_memory_store_opens_without_wal() {
    let store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    assert_eq!(store.get_offset("c", "s").expect("get offset"), 0);
}

/// The read-path indexes of schema v2 must arrive on an *existing* v1
/// database, not only on a freshly created one — every user who ran an
/// earlier build has a v1 file, and a migration that only works on empty
/// state is a migration that has never run where it matters.
#[test]
fn a_v1_database_migrates_in_place_and_keeps_its_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.db");

    let seeded = sample_envelope("evt-1", "session-1");
    let seeded_json =
        serde_json::to_string(&serde_json::to_value(&seeded).expect("envelope to value"))
            .expect("value to string");

    // A database exactly as schema version 1 left it: the four tables, no
    // indexes, `schema_migrations` stopped at 1.
    {
        let conn = rusqlite::Connection::open(&db_path).expect("create v1 database");
        conn.execute_batch(
            "CREATE TABLE raw_events (
                 event_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                 type TEXT NOT NULL, ts TEXT NOT NULL, json TEXT NOT NULL);
             CREATE TABLE ingest_offsets (
                 collector TEXT NOT NULL, session_id TEXT NOT NULL,
                 offset INTEGER NOT NULL, PRIMARY KEY (collector, session_id));
             CREATE TABLE impact_estimates (
                 event_id TEXT PRIMARY KEY, json TEXT NOT NULL,
                 methodology_version TEXT NOT NULL);
             CREATE TABLE impact_joins (unit_key TEXT PRIMARY KEY, json TEXT NOT NULL);
             CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
             INSERT INTO schema_migrations (version) VALUES (1);",
        )
        .expect("build v1 schema");
        conn.execute(
            "INSERT INTO raw_events (event_id, session_id, type, ts, json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                seeded.event_id,
                seeded.session_id,
                "session_meta",
                seeded.ts,
                seeded_json
            ],
        )
        .expect("seed a v1 row");
    }

    let store = Store::open(&db_path).expect("a v1 database must open and migrate");

    // The row written under v1 is still there and still deserializes.
    assert_eq!(
        store
            .events_for_session("session-1")
            .expect("read back the pre-migration row"),
        vec![seeded],
        "migrating must not rewrite or drop existing raw events"
    );

    let indexes = || -> Vec<String> {
        let conn = rusqlite::Connection::open(&db_path).expect("inspect db");
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index'
                 AND name IN ('raw_events_session', 'raw_events_type', 'impact_joins_session_id')
                 ORDER BY name",
            )
            .expect("prepare index query");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query indexes");
        rows.map(|r| r.expect("index name")).collect()
    };
    assert_eq!(
        indexes(),
        vec![
            "impact_joins_session_id".to_string(),
            "raw_events_session".to_string(),
            "raw_events_type".to_string(),
        ],
        "v2 and v4 must leave all three read-path indexes on an upgraded database"
    );

    drop(store);

    // Re-opening is a no-op: the version is recorded once, and the
    // `IF NOT EXISTS` statements don't rebuild anything.
    drop(Store::open(&db_path).expect("reopen an already-migrated database"));
    let conn = rusqlite::Connection::open(&db_path).expect("inspect db");
    let versions: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(
        versions, 5,
        "expected exactly schema versions 1 through 5 recorded"
    );
    assert_eq!(indexes().len(), 3);
}

/// Joins are looked up through their `session_id` column (schema v4),
/// which `upsert_join` fills from the record's `unit.session_id`.
#[test]
fn joins_are_found_by_session_through_the_session_column() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.db");
    let mut store = Store::open(&db_path).expect("open on-disk store");

    store
        .upsert_join(
            "session:session-1",
            &serde_json::json!({"unit": {"level": "session", "session_id": "session-1"}}),
        )
        .expect("upsert join");
    store
        .upsert_join(
            "session:session-2",
            &serde_json::json!({"unit": {"level": "session", "session_id": "session-2"}}),
        )
        .expect("upsert other session's join");

    let joins = store.joins_for_session("session-1").expect("read joins");
    assert_eq!(joins.len(), 1);
    assert_eq!(joins[0].0, "session:session-1");
}

#[test]
fn unknown_offset_defaults_to_zero() {
    let store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    let offset = store
        .get_offset("no-such-collector", "no-such-session")
        .expect("get offset for unknown pair");
    assert_eq!(offset, 0);
}

#[test]
fn wipe_derived_leaves_raw_events_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.db");

    let mut store = Store::open(&db_path).expect("open on-disk store");
    let evt = sample_envelope("evt-1", "session-1");
    store.insert_events(&[evt]).expect("insert raw event");

    let estimate = afoot_store::ImpactEstimate {
        event_id: "evt-1".to_string(),
        methodology_version: "v0".to_string(),
        json: serde_json::json!({"gwp_g_co2eq": {"min": 1.0, "max": 2.0}}),
    };
    store.upsert_estimate(&estimate).expect("upsert estimate");

    // Verify derived row exists before wipe
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open db to inspect");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM impact_estimates", [], |row| {
                row.get(0)
            })
            .expect("query impact_estimates count");
        assert!(
            count > 0,
            "derived row must exist before wipe_derived (found {})",
            count
        );
    }

    store.wipe_derived().expect("wipe derived");

    // Verify derived row is deleted after wipe
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open db to inspect");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM impact_estimates", [], |row| {
                row.get(0)
            })
            .expect("query impact_estimates count");
        assert_eq!(count, 0, "derived row must be deleted after wipe_derived");
    }

    // Verify raw event survives
    let events = store
        .events_for_session("session-1")
        .expect("read back session events");
    assert_eq!(events.len(), 1, "raw events must survive wipe_derived");
}

#[test]
fn session_summaries_counts_by_type_and_spans_ts_range() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");

    let mut meta = sample_envelope("evt-meta", "session-1");
    meta.ts = "2026-07-25T12:00:00Z".to_string();

    let mut later = sample_envelope("evt-later", "session-1");
    later.ts = "2026-07-25T12:00:10Z".to_string();

    let other_session = sample_envelope("evt-other", "session-2");

    store
        .insert_events(&[meta.clone(), later.clone(), other_session.clone()])
        .expect("insert events");

    let summaries = store.session_summaries().expect("session summaries");
    assert_eq!(summaries.len(), 2);

    let s1 = summaries
        .iter()
        .find(|s| s.session_id == "session-1")
        .expect("session-1 present");
    assert_eq!(s1.counts, vec![("session_meta".to_string(), 2)]);
    assert_eq!(s1.first_ts, "2026-07-25T12:00:00Z");
    assert_eq!(s1.last_ts, "2026-07-25T12:00:10Z");

    let s2 = summaries
        .iter()
        .find(|s| s.session_id == "session-2")
        .expect("session-2 present");
    assert_eq!(s2.counts, vec![("session_meta".to_string(), 1)]);
    assert_eq!(s2.first_ts, s2.last_ts);
}

#[test]
fn session_summaries_empty_store_returns_empty_vec() {
    let store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    let summaries = store.session_summaries().expect("session summaries");
    assert!(summaries.is_empty());
}

#[test]
fn llm_calls_without_estimate_excludes_already_estimated_and_non_llm_call_events() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");

    let pending = llm_call_envelope("evt-pending", "session-1");
    let already_estimated = llm_call_envelope("evt-estimated", "session-1");
    let non_llm_call = sample_envelope("evt-meta", "session-1");

    store
        .insert_events(&[
            pending.clone(),
            already_estimated.clone(),
            non_llm_call.clone(),
        ])
        .expect("insert events");

    store
        .upsert_estimate(&afoot_store::ImpactEstimate {
            event_id: "evt-estimated".to_string(),
            methodology_version: "ecologits-0.11.1".to_string(),
            json: serde_json::json!({"status": "ok"}),
        })
        .expect("upsert estimate for evt-estimated");

    let result = store
        .llm_calls_without_estimate()
        .expect("query pending llm_calls");

    assert_eq!(result, vec![pending]);

    // The count must agree with the list it summarizes — it exists so a
    // caller that only wants the number doesn't load every envelope, and
    // a number that disagreed with the backlog would be worse than slow.
    assert_eq!(
        store
            .count_llm_calls_without_estimate()
            .expect("count pending llm_calls"),
        1
    );
}

#[test]
fn count_llm_calls_without_estimate_is_zero_on_an_empty_store() {
    let store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    assert_eq!(
        store
            .count_llm_calls_without_estimate()
            .expect("count pending llm_calls"),
        0
    );
}

#[test]
fn opaque_events_are_deduplicated_and_persisted_separately() {
    let mut store = Store::open(Path::new(":memory:")).expect("open store");
    let json = serde_json::json!({
        "schema_version": "0.1.0",
        "event_id": "opaque-event-0001",
        "ts": "2026-07-25T00:00:00Z",
        "collector": {"name": "future", "version": "1.0.0"},
        "session_id": "sess-opaque",
        "type": "future_fact",
        "payload": {"new_field": 42}
    });
    let event = afoot_events::OpaqueEvent {
        schema_version: "0.1.0".to_string(),
        event_id: "opaque-event-0001".to_string(),
        ts: "2026-07-25T00:00:00Z".to_string(),
        collector: afoot_events::Collector {
            name: "future".to_string(),
            version: "1.0.0".to_string(),
        },
        session_id: "sess-opaque".to_string(),
        type_tag: "future_fact".to_string(),
        json,
    };
    assert_eq!(
        store
            .insert_opaque_events(std::slice::from_ref(&event))
            .unwrap(),
        1
    );
    assert_eq!(store.insert_opaque_events(&[event]).unwrap(), 0);
    assert_eq!(store.count_opaque_events().unwrap(), 1);
    assert!(store.events_for_session("sess-opaque").unwrap().is_empty());
}

/// `estimates_for_events` batches its lookups, so the batch boundary is a
/// real edge: an id list longer than one chunk must still come back whole,
/// and still keyed by event id.
#[test]
fn estimates_for_events_returns_every_row_across_batch_boundaries() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");

    // Comfortably more than one batch (500), and an odd count so the last
    // batch is partial.
    let ids: Vec<String> = (0..1_201).map(|i| format!("evt-{i:05}")).collect();
    for (i, event_id) in ids.iter().enumerate() {
        // Every third id is left un-estimated, so absence is exercised
        // inside a batch rather than only at its edges.
        if i % 3 == 0 {
            continue;
        }
        store
            .upsert_estimate(&afoot_store::ImpactEstimate {
                event_id: event_id.clone(),
                methodology_version: "ecologits-0.11.1".to_string(),
                json: serde_json::json!({"status": "ok", "n": i}),
            })
            .expect("upsert estimate");
    }

    let found = store
        .estimates_for_events(&ids)
        .expect("read estimates across batches");

    let expected: Vec<&String> = ids
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 3 != 0)
        .map(|(_, id)| id)
        .collect();
    assert_eq!(found.len(), expected.len());
    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        expected,
        "results stay keyed and ordered by event id whatever order the batches returned"
    );
    assert_eq!(
        found["evt-00001"],
        serde_json::json!({"status": "ok", "n": 1})
    );
    assert!(
        !found.contains_key("evt-00000"),
        "an un-estimated event has no entry"
    );
}

#[test]
fn llm_calls_without_estimate_empty_store_returns_empty_vec() {
    let store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    let result = store
        .llm_calls_without_estimate()
        .expect("query pending llm_calls");
    assert!(result.is_empty());
}

#[test]
fn joins_round_trip_and_are_scoped_to_their_session() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");

    let record = |session: &str, level: &str| {
        serde_json::json!({
            "unit": {"level": level, "session_id": session},
            "t_start": "2026-07-25T12:00:00Z",
            "t_end": "2026-07-25T12:00:10Z",
            "attribution_policy": "l2_cpu_time",
        })
    };

    // Deliberately inserted out of order: the accessor's `ORDER BY
    // unit_key` is what fixes emission order, not the write order.
    store
        .upsert_join(
            "tool_call:session-1:span-b",
            &record("session-1", "tool_call"),
        )
        .expect("upsert tool_call join");
    store
        .upsert_join("session:session-1", &record("session-1", "session"))
        .expect("upsert session join");
    store
        .upsert_join("session:session-2", &record("session-2", "session"))
        .expect("upsert other session's join");

    let joins = store
        .joins_for_session("session-1")
        .expect("read joins for session-1");
    assert_eq!(
        joins.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        vec!["session:session-1", "tool_call:session-1:span-b"],
        "joins come back sorted by unit_key and only for the asked-for session"
    );
    assert_eq!(joins[0].1, record("session-1", "session"));

    // Upsert replaces in place rather than accumulating rows.
    let revised = serde_json::json!({
        "unit": {"level": "session", "session_id": "session-1"},
        "t_start": "2026-07-25T12:00:00Z",
        "t_end": "2026-07-25T12:00:20Z",
        "attribution_policy": "l1_wall_clock",
    });
    store
        .upsert_join("session:session-1", &revised)
        .expect("re-upsert session join");
    let joins = store
        .joins_for_session("session-1")
        .expect("read joins after re-upsert");
    assert_eq!(joins.len(), 2);
    assert_eq!(joins[0].1, revised);

    store.wipe_derived().expect("wipe derived");
    assert!(store
        .joins_for_session("session-1")
        .expect("read joins after wipe")
        .is_empty());
}

#[test]
fn estimates_for_events_omits_events_with_no_stored_estimate() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    store
        .upsert_estimate(&afoot_store::ImpactEstimate {
            event_id: "evt-1".to_string(),
            methodology_version: "ecologits-0.11.1".to_string(),
            json: serde_json::json!({"status": "ok"}),
        })
        .expect("upsert estimate");

    let found = store
        .estimates_for_events(&["evt-1".to_string(), "evt-missing".to_string()])
        .expect("read estimates");

    assert_eq!(found.len(), 1, "an un-estimated event has no entry at all");
    assert_eq!(found["evt-1"], serde_json::json!({"status": "ok"}));
    assert!(
        !found.contains_key("evt-missing"),
        "absence must stay distinguishable from a stored failure status"
    );
}

#[test]
fn declared_geo_zones_are_distinct_sorted_and_skip_sessions_without_one() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");

    let with_zone = |event_id: &str, session: &str, zone: Option<&str>| {
        let mut evt = sample_envelope(event_id, session);
        if let Payload::SessionMeta(meta) = &mut evt.payload {
            meta.geo_zone = zone.map(str::to_string);
        }
        evt
    };

    store
        .insert_events(&[
            with_zone("evt-1", "session-1", Some("FRA")),
            with_zone("evt-2", "session-2", Some("USA")),
            with_zone("evt-3", "session-3", Some("FRA")),
            with_zone("evt-4", "session-4", None),
        ])
        .expect("insert session_meta events");

    assert_eq!(
        store.declared_geo_zones().expect("read declared zones"),
        vec!["FRA".to_string(), "USA".to_string()]
    );
}

fn estimate(event_id: &str, version: &str, status: &str) -> afoot_store::ImpactEstimate {
    afoot_store::ImpactEstimate {
        event_id: event_id.to_string(),
        methodology_version: version.to_string(),
        json: serde_json::json!({"status": status}),
    }
}

/// An `unknown_model`/`missing_zone` verdict belongs to the methodology
/// that gave it: once the estimator runs a different ecologits version the
/// call is worth asking about again. Without this, a model added in a
/// later ecologits release stayed unknown for every call made before the
/// upgrade.
#[test]
fn failed_estimates_from_another_methodology_are_pending_again() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    let ids = [
        "evt-new",
        "evt-ok-old",
        "evt-unknown-old",
        "evt-zone-old",
        "evt-unknown-legacy",
        "evt-unknown-current",
        "evt-error-old",
    ];
    let events: Vec<Envelope> = ids
        .iter()
        .map(|id| llm_call_envelope(id, "session-1"))
        .collect();
    store.insert_events(&events).expect("insert events");
    for row in [
        estimate("evt-ok-old", "ecologits-0.11.1", "ok"),
        estimate("evt-unknown-old", "ecologits-0.11.1", "unknown_model"),
        estimate("evt-zone-old", "ecologits-0.11.1", "missing_zone"),
        // Rows written before failure verdicts carried a version.
        estimate("evt-unknown-legacy", "ecologits-unknown", "unknown_model"),
        estimate("evt-unknown-current", "ecologits-0.11.2", "unknown_model"),
        estimate("evt-error-old", "ecologits-0.11.1", "error"),
    ] {
        store.upsert_estimate(&row).expect("upsert estimate");
    }

    let pending: Vec<String> = store
        .llm_calls_to_estimate(Some("ecologits-0.11.2"))
        .expect("query pending")
        .into_iter()
        .map(|event| event.event_id)
        .collect();
    assert_eq!(
        pending,
        // (ts, event_id) order; every fixture shares one ts.
        [
            "evt-new",
            "evt-unknown-legacy",
            "evt-unknown-old",
            "evt-zone-old"
        ]
    );
    assert_eq!(
        store
            .count_llm_calls_to_estimate(Some("ecologits-0.11.2"))
            .expect("count pending"),
        4
    );

    // Without a known current methodology nothing is re-queued.
    assert_eq!(
        store
            .count_llm_calls_to_estimate(None)
            .expect("count pending"),
        1
    );
}

fn span_envelope(event_id: &str, session_id: &str, ts: &str, tool_name: &str) -> Envelope {
    let mut span = fixtures::action_span(event_id, ts, ts);
    span.tool_name = tool_name.to_string();
    fixtures::envelope(event_id, session_id, ts, Payload::ActionSpan(span))
}

/// A Claude Code process started before telemetry was configured keeps
/// firing hooks (they reload live) but never exports token usage: its
/// sessions show tool activity and no llm_call. `afoot doctor` surfaces them.
#[test]
fn sessions_with_tool_activity_but_no_llm_calls_are_telemetry_gaps() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    store
        .insert_events(&[
            // Gap: two tool spans, no llm_call.
            span_envelope(
                "gap-boot",
                "sess-gap",
                "2026-10-10T10:00:00Z",
                "__session__",
            ),
            span_envelope("gap-1", "sess-gap", "2026-10-10T10:01:00Z", "Bash"),
            span_envelope("gap-2", "sess-gap", "2026-10-10T10:02:00Z", "Edit"),
            // Healthy: tool span and llm_call.
            span_envelope("ok-1", "sess-ok", "2026-10-10T10:01:00Z", "Bash"),
            fixtures::envelope(
                "ok-call",
                "sess-ok",
                "2026-10-10T10:01:30Z",
                Payload::LlmCall(fixtures::llm_call()),
            ),
            // Empty session: only the bootstrap span.
            span_envelope(
                "empty-boot",
                "sess-empty",
                "2026-10-10T10:00:00Z",
                "__session__",
            ),
            // Gap, but older than the window.
            span_envelope("old-1", "sess-old", "2026-09-01T10:00:00Z", "Bash"),
        ])
        .expect("insert events");

    let gaps = store
        .sessions_without_llm_calls("2026-10-01T00:00:00Z")
        .expect("query gaps");
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].session_id, "sess-gap");
    assert_eq!(gaps[0].tool_spans, 2);
    assert_eq!(gaps[0].last_ts, "2026-10-10T10:02:00Z");
}

#[test]
fn rewriting_identical_joins_changes_no_rows() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    let joins = vec![
        (
            "session:s1".to_string(),
            serde_json::json!({"unit": {"level": "session", "session_id": "s1"}, "energy_j": 1.0}),
        ),
        (
            "tool_call:s1:a".to_string(),
            serde_json::json!({"unit": {"level": "tool_call", "session_id": "s1"}, "energy_j": 2.0}),
        ),
    ];
    assert_eq!(store.upsert_joins(&joins).expect("first write"), 2);
    assert_eq!(
        store.upsert_joins(&joins).expect("identical rewrite"),
        0,
        "an unchanged join must not be rewritten"
    );

    let mut revised = joins.clone();
    revised[1].1["energy_j"] = serde_json::json!(3.0);
    assert_eq!(store.upsert_joins(&revised).expect("one change"), 1);

    let stored = store.joins_for_session("s1").expect("read back");
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].1["energy_j"], serde_json::json!(3.0));
}

fn reject(n: u64) -> afoot_store::RejectRow {
    afoot_store::RejectRow {
        ts: format!("2026-10-17T12:00:{:02}Z", n % 60),
        source: "otlp".to_string(),
        reason: format!("reason {n}"),
        attribute_keys: vec!["event.name".to_string()],
        bytes: n,
    }
}

#[test]
fn rejects_are_kept_as_a_bounded_ring_newest_first() {
    let mut store = Store::open(Path::new(":memory:")).expect("open");
    let rows: Vec<_> = (0..(afoot_store::REJECT_RING as u64 + 5))
        .map(reject)
        .collect();
    store.insert_rejects(&rows).expect("insert");

    let recent = store.recent_rejects(usize::MAX).expect("read");
    assert_eq!(
        recent.len(),
        afoot_store::REJECT_RING,
        "the ring never grows past its cap"
    );
    assert_eq!(
        recent[0],
        reject(afoot_store::REJECT_RING as u64 + 4),
        "newest first"
    );
    assert_eq!(recent.last().unwrap().bytes, 5, "the oldest five fell off");
    assert_eq!(store.recent_rejects(2).expect("limited").len(), 2);
}

#[test]
fn sessions_are_listed_per_collector() {
    let mut store = Store::open(Path::new(":memory:")).expect("open");
    let mut a = llm_call_envelope("evt-a-0000000001", "s-a");
    a.collector.name = "otlp-cc".to_string();
    let mut b = llm_call_envelope("evt-b-0000000001", "s-b");
    b.collector.name = "otlp-codex".to_string();
    store.insert_events(&[a, b]).expect("insert");
    assert_eq!(
        store.session_ids_for_collector("otlp-cc").unwrap(),
        vec!["s-a"]
    );
    assert!(store
        .session_ids_for_collector("cc-hooks")
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn the_database_file_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.db");
    std::fs::write(&path, b"").expect("pre-create with the default umask");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut store = Store::open(&path).expect("open");
    store
        .insert_events(&[llm_call_envelope("evt-perm-00000001", "s")])
        .expect("write so the WAL exists");
    for file in ["state.db", "state.db-wal", "state.db-shm"] {
        let mode = std::fs::metadata(dir.path().join(file))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{file}");
    }
}

#[test]
fn writes_inside_one_transaction_land_together_or_not_at_all() {
    let mut store = Store::open(Path::new(":memory:")).expect("open");
    let failed: Result<(), afoot_store::Error> = store.in_transaction(|store| {
        store.insert_events(&[llm_call_envelope("evt-tx-0000000001", "s")])?;
        store.set_offset("cc-hooks", "s", 42)?;
        Err(afoot_store::Error::Sqlite(rusqlite::Error::InvalidQuery))
    });
    assert!(failed.is_err());
    assert!(store.events_for_session("s").expect("read").is_empty());
    assert_eq!(store.get_offset("cc-hooks", "s").expect("offset"), 0);

    store
        .in_transaction(|store| {
            store.insert_events(&[llm_call_envelope("evt-tx-0000000001", "s")])?;
            store.set_offset("cc-hooks", "s", 42)
        })
        .expect("commit");
    assert_eq!(store.events_for_session("s").expect("read").len(), 1);
    assert_eq!(store.get_offset("cc-hooks", "s").expect("offset"), 42);
}

#[test]
fn a_transaction_inside_a_transaction_rolls_back_only_itself() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&dir.path().join("state.db")).expect("open");

    store
        .in_transaction(|store| -> Result<(), afoot_store::Error> {
            store.insert_events(&[llm_call_envelope("evt-nest-000000001", "s")])?;
            let inner: Result<(), afoot_store::Error> = store.in_transaction(|store| {
                store.set_offset("cc-hooks", "s", 42)?;
                Err(afoot_store::Error::Sqlite(rusqlite::Error::InvalidQuery))
            });
            assert!(inner.is_err());
            store.in_transaction(|store| store.set_offset("cc-hooks", "t", 7))
        })
        .expect("outer commit");

    assert_eq!(store.events_for_session("s").expect("read").len(), 1);
    assert_eq!(store.get_offset("cc-hooks", "s").expect("offset"), 0);
    assert_eq!(store.get_offset("cc-hooks", "t").expect("offset"), 7);
}

#[test]
fn pending_among_answers_the_queue_predicate_for_named_calls_only() {
    let mut store = Store::open(Path::new(":memory:")).expect("open in-memory store");
    let events: Vec<Envelope> = ["evt-new", "evt-ok-old", "evt-unknown-old", "evt-other"]
        .iter()
        .map(|id| llm_call_envelope(id, "session-1"))
        .collect();
    store.insert_events(&events).expect("insert events");
    for row in [
        estimate("evt-ok-old", "ecologits-0.11.1", "ok"),
        estimate("evt-unknown-old", "ecologits-0.11.1", "unknown_model"),
    ] {
        store.upsert_estimate(&row).expect("upsert estimate");
    }
    let asked = [
        "evt-new".to_string(),
        "evt-ok-old".to_string(),
        "evt-unknown-old".to_string(),
        "evt-not-stored".to_string(),
    ];

    let pending = store
        .llm_calls_pending_among(&asked, Some("ecologits-0.11.2"))
        .expect("pending among");
    let ids: Vec<&str> = pending
        .iter()
        .map(|event| event.event_id.as_str())
        .collect();
    // `evt-other` is pending too, but was not asked about.
    assert_eq!(ids, ["evt-new", "evt-unknown-old"]);

    let pending = store
        .llm_calls_pending_among(&asked, None)
        .expect("pending among");
    let ids: Vec<&str> = pending
        .iter()
        .map(|event| event.event_id.as_str())
        .collect();
    assert_eq!(ids, ["evt-new"]);

    // The whole queue, as ids, in queue order.
    assert_eq!(
        store
            .llm_call_ids_to_estimate(Some("ecologits-0.11.2"))
            .expect("queue ids"),
        ["evt-new", "evt-other", "evt-unknown-old"]
    );
}

#[test]
fn a_join_is_read_by_its_key_alone() {
    let mut store = Store::open(Path::new(":memory:")).expect("open");
    store
        .upsert_joins(&[
            (
                "session:s".to_string(),
                serde_json::json!({"unit": {"level": "session", "session_id": "s"}}),
            ),
            (
                "tool_call:s:a".to_string(),
                serde_json::json!({"unit": {"level": "tool_call", "session_id": "s"}}),
            ),
        ])
        .expect("upsert");
    let found = store.join_by_key("session:s").expect("read");
    assert_eq!(found.expect("present")["unit"]["level"], "session");
    assert!(store.join_by_key("session:other").expect("read").is_none());
}
