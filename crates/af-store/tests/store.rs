//! Integration tests for `af-store`'s public interface: raw event
//! insertion/dedup, offset persistence across close/reopen, and derived-vs-raw
//! table isolation.

use af_events::{fixtures, Envelope, Payload};
use af_store::Store;
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

/// `af watch` is resident and writing while the user runs `af report` over
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

    // An open read transaction on another connection — `af report` mid-query.
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
                 AND name IN ('raw_events_session', 'raw_events_type', 'impact_joins_session')
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
            "impact_joins_session".to_string(),
            "raw_events_session".to_string(),
            "raw_events_type".to_string(),
        ],
        "v2 must create all three read-path indexes on an upgraded database"
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
        versions, 3,
        "expected exactly schema versions 1 through 3 recorded"
    );
    assert_eq!(indexes().len(), 3);
}

/// The expression index is on `json_extract(...)`, which SQLite only
/// accepts if it is deterministic — a wrong expression here fails at
/// `CREATE INDEX` time, so this asserts the query it exists for still
/// returns the right rows through it.
#[test]
fn joins_are_still_found_by_session_through_the_expression_index() {
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

    let estimate = af_store::ImpactEstimate {
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
        .upsert_estimate(&af_store::ImpactEstimate {
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
    let event = af_events::OpaqueEvent {
        schema_version: "0.1.0".to_string(),
        event_id: "opaque-event-0001".to_string(),
        ts: "2026-07-25T00:00:00Z".to_string(),
        collector: af_events::Collector {
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
            .upsert_estimate(&af_store::ImpactEstimate {
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
        .upsert_estimate(&af_store::ImpactEstimate {
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
