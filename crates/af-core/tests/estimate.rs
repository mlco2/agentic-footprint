//! Golden-transcript coverage for `estimate_pending`: drives the fake
//! sidecar fixture (`tests/fixtures/fake_sidecar.py --replay`) against the
//! handwritten transcript `tests/fixtures/sidecar/estimator.jsonl` so this
//! test exercises the exact request/response framing without needing
//! ecologits installed. See `python/tests/test_estimator.py` for coverage
//! against the real sidecar (gated on ecologits being importable).

use std::path::{Path, PathBuf};

use af_core::{estimate_pending, EstimationOutcome, EstimationRegion};
use af_events::{fixtures, Envelope, LlmCall, Payload, Usage};
use af_sidecar::Sidecar;
use af_store::Store;

fn python3() -> PathBuf {
    PathBuf::from("python3")
}

fn fake_sidecar_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake_sidecar.py")
}

fn transcript_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sidecar/estimator.jsonl")
}

fn spawn_replay_sidecar() -> Sidecar {
    let script = fake_sidecar_path();
    let transcript = transcript_path();
    Sidecar::spawn(
        &python3(),
        script.to_str().expect("utf8 path"),
        &["--replay", transcript.to_str().expect("utf8 path")],
    )
    .expect("spawn fake_sidecar.py --replay")
}

fn llm_call_event(event_id: &str, model_id_requested: &str) -> Envelope {
    llm_call_event_with_usage(event_id, model_id_requested, Some(500))
}

fn llm_call_event_with_usage(
    event_id: &str,
    model_id_requested: &str,
    output_tokens: Option<u64>,
) -> Envelope {
    fixtures::envelope(
        event_id,
        "session-1",
        "2026-07-25T12:00:00Z",
        Payload::LlmCall(LlmCall {
            // The model id and the token count are what this suite is
            // about: the transcript's request order is keyed on the first,
            // and `None` tokens is the "nothing to estimate from" case.
            model_id_requested: model_id_requested.to_string(),
            usage: Usage {
                output_tokens,
                ..Default::default()
            },
            duration_ms: Some(1200),
            ..fixtures::llm_call()
        }),
    )
}

/// Reads back one row from `impact_estimates` directly via a fresh
/// connection (bypassing af-store's own API, same pattern as
/// `af-store/tests/store.rs`'s `wipe_derived` coverage) so this test
/// doesn't need a new af-store read accessor just to assert on upserted
/// rows.
fn read_estimate_row(db_path: &Path, event_id: &str) -> (String, String) {
    let conn = rusqlite::Connection::open(db_path).expect("open db to inspect");
    conn.query_row(
        "SELECT methodology_version, json FROM impact_estimates WHERE event_id = ?1",
        [event_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap_or_else(|e| panic!("no impact_estimates row for {event_id}: {e}"))
}

#[test]
fn estimate_pending_batches_known_and_unknown_models_against_golden_transcript() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.db");

    let mut store = Store::open(&db_path).expect("open store");
    let known = llm_call_event("evt-known", "claude-opus-4-1-20250805");
    let unknown = llm_call_event("evt-unknown", "not-a-real-model-xyz");
    let no_usage = llm_call_event_with_usage("evt-no-usage", "claude-opus-4-1-20250805", None);
    store
        .insert_events(&[known.clone(), unknown.clone(), no_usage.clone()])
        .expect("seed llm_call events");

    let mut sidecar = spawn_replay_sidecar();

    let region = EstimationRegion::explicit("WOR", "test");
    let outcome = estimate_pending(&mut store, &mut sidecar, &region).expect("estimate_pending");

    assert_eq!(
        outcome,
        EstimationOutcome {
            estimated: 1,
            unknown_model: 1,
            missing_zone: 0,
            missing_usage: 1,
            errors: 0,
        }
    );

    // No more pending work: a second pass finds nothing left to send.
    assert!(store
        .llm_calls_without_estimate()
        .expect("query pending")
        .is_empty());

    let (known_version, known_json) = read_estimate_row(&db_path, "evt-known");
    assert_eq!(known_version, "ecologits-0.11.1");
    let known_value: serde_json::Value = serde_json::from_str(&known_json).unwrap();
    assert_eq!(known_value["status"], "ok");
    assert_eq!(known_value["impacts"]["energy"]["unit"], "kWh");
    assert_eq!(known_value["impacts"]["water"]["unit"], "L");
    assert_eq!(known_value["remote_region"]["id"], "WOR");
    assert_eq!(known_value["remote_region"]["source"], "test");

    let (unknown_version, unknown_json) = read_estimate_row(&db_path, "evt-unknown");
    assert_eq!(unknown_version, "ecologits-unknown");
    let unknown_value: serde_json::Value = serde_json::from_str(&unknown_json).unwrap();
    assert_eq!(unknown_value["status"], "unknown_model");

    // No usage: never sent to the sidecar (the golden transcript has no
    // request for it), and upserted with methodology_version "none" —
    // still stamped with remote-region provenance, since a row is a row.
    let (no_usage_version, no_usage_json) = read_estimate_row(&db_path, "evt-no-usage");
    assert_eq!(no_usage_version, "none");
    let no_usage_value: serde_json::Value = serde_json::from_str(&no_usage_json).unwrap();
    assert_eq!(
        no_usage_value,
        serde_json::json!({
            "status": "missing_usage",
            "remote_region": {"id": "WOR", "source": "test"}
        })
    );
}
