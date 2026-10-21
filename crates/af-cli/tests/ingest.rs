//! End-to-end test of the ingest pipeline through `af report --format json`,
//! using the handwritten fixtures under `tests/fixtures/spool/` (repo
//! root): `basic-session/` (one file, 10 well-formed events covering all
//! five Contract #1 payload types) and `adversarial/` (a valid `llm_call`,
//! an invalid-JSON line, an unknown-`type` line, and a duplicate
//! `event_id`).
//!
//! Fixtures are copied into a fresh tempdir spool per test run — never
//! point `AF_STATE_DIR` at the repo fixtures directly, since ingest mutates
//! spool-adjacent state (`state.db`) in place.

use std::path::Path;

use serde_json::Value;

mod common;
use common::{run_af, seed_state_dir_with_adversarial};

fn run_report(state_dir: &Path) -> Value {
    let (stdout, _) = run_af(state_dir, &["report", "--format", "json"], false);
    serde_json::from_slice(&stdout).unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}"))
}

fn event_counts<'a>(report: &'a Value, session_id: &str) -> &'a serde_json::Map<String, Value> {
    report["sessions"]
        .as_array()
        .expect("sessions is an array")
        .iter()
        .find(|s| s["session_id"] == session_id)
        .unwrap_or_else(|| panic!("session {session_id} not found in {report:#}"))
        .get("event_counts")
        .expect("session has event_counts")
        .as_object()
        .expect("event_counts is an object")
}

/// Spool rejects recorded in the store. Metadata only: there is no column
/// a raw line could be in, and nothing is written next to the database.
fn count_spool_rejects(state_dir: &Path) -> usize {
    assert!(
        !state_dir.join("rejected").exists(),
        "rejected lines must not be copied to disk"
    );
    af_store::Store::open_read_only(&state_dir.join("state.db"))
        .expect("open db")
        .recent_rejects(af_store::REJECT_RING)
        .expect("read rejects")
        .iter()
        .filter(|row| row.source.starts_with("spool:") && row.bytes > 0)
        .count()
}

#[test]
fn ingest_pipeline_and_report_facts_summary() {
    let dir = seed_state_dir_with_adversarial();

    let first = run_report(dir.path());

    let basic_counts = event_counts(&first, "sess-basic");
    let total: u64 = basic_counts.values().map(|v| v.as_u64().unwrap()).sum();
    assert_eq!(
        total, 10,
        "basic-session must have 10 total events, got {basic_counts:?}"
    );
    assert_eq!(basic_counts["session_meta"], 1);
    assert_eq!(basic_counts["llm_call"], 2);
    assert_eq!(basic_counts["energy_sample"], 3);
    assert_eq!(basic_counts["action_span"], 2);
    assert_eq!(basic_counts["process_sample"], 2);

    let adv_counts = event_counts(&first, "sess-adv");
    assert_eq!(
        adv_counts["llm_call"], 1,
        "duplicate event_id must be counted once"
    );
    assert_eq!(
        adv_counts.len(),
        1,
        "only the llm_call type should have survived ingest for sess-adv, got {adv_counts:?}"
    );

    assert_eq!(
        count_spool_rejects(dir.path()),
        1,
        "expected only invalid JSON to be rejected; unknown types are preserved"
    );

    // Second run against the same (now-ingested) spool: offsets have
    // already advanced past every line, including the rejects, so nothing
    // new is ingested or rejected and the report output is unchanged.
    let second = run_report(dir.path());
    assert_eq!(
        first, second,
        "report output must be idempotent across ingest runs"
    );
    assert_eq!(
        count_spool_rejects(dir.path()),
        1,
        "second run must not record the reject again"
    );
}
