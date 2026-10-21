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

    // A failure verdict carries no methodology of its own; it is stamped
    // with the estimator's, so a later ecologits can re-ask it (and the
    // same one never does).
    let (unknown_version, unknown_json) = read_estimate_row(&db_path, "evt-unknown");
    assert_eq!(unknown_version, "ecologits-0.11.1");
    assert_eq!(
        store
            .count_llm_calls_to_estimate(Some("ecologits-0.11.1"))
            .expect("count pending"),
        0
    );
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

/// A call ecologits did not know under an older pin is asked again once the
/// estimator runs a different methodology — the upgrade path for a model
/// that was unknown when its calls were made.
#[test]
fn stale_unknown_model_is_reestimated_under_a_new_methodology() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("state.db");
    let mut store = Store::open(&db_path).expect("open store");
    store
        .insert_events(&[llm_call_event("evt-stale", "claude-opus-5-5")])
        .expect("seed llm_call");
    store
        .upsert_estimate(&af_store::ImpactEstimate {
            event_id: "evt-stale".to_string(),
            methodology_version: "ecologits-0.11.1".to_string(),
            json: serde_json::json!({"status": "unknown_model"}),
        })
        .expect("seed stale verdict");

    // Default mode: every model is estimable, methodology "test".
    let script = fake_sidecar_path();
    let mut sidecar = Sidecar::spawn(&python3(), script.to_str().expect("utf8 path"), &[])
        .expect("spawn fake_sidecar.py");

    let region = EstimationRegion::explicit("WOR", "test");
    let outcome = estimate_pending(&mut store, &mut sidecar, &region).expect("estimate_pending");
    assert_eq!(outcome.estimated, 1, "{outcome:?}");

    let (version, json) = read_estimate_row(&db_path, "evt-stale");
    assert_eq!(version, "ecologits-test");
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["status"], "ok");
}

/// The resident estimator computes off the writer's connection and hands
/// the rows back: computing is pure with respect to the store.
#[test]
fn compute_estimates_returns_the_rows_estimate_events_would_write() {
    let script = fake_sidecar_path();
    let mut sidecar = Sidecar::spawn(&python3(), script.to_str().expect("utf8 path"), &[])
        .expect("spawn fake_sidecar.py");
    let region = EstimationRegion::explicit("WOR", "test");
    let events = [
        llm_call_event("evt-a", "claude-opus-5-5"),
        llm_call_event_with_usage("evt-b", "claude-opus-5-5", None),
    ];

    let computed =
        af_core::compute_estimates(&mut sidecar, &region, &events, Some("ecologits-test"));

    assert!(computed.error.is_none(), "{:?}", computed.error);
    assert_eq!(computed.outcome.estimated, 1);
    assert_eq!(computed.outcome.missing_usage, 1);
    let rows: Vec<(&str, &str, &str)> = computed
        .rows
        .iter()
        .map(|row| {
            (
                row.event_id.as_str(),
                row.methodology_version.as_str(),
                row.json["status"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("evt-a", "ecologits-test", "ok"),
            ("evt-b", "none", "missing_usage")
        ]
    );
}
