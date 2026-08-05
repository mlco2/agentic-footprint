//! Contract tests for the schema gate in [`af_events::parse_line`].
//!
//! Two directions, both load-bearing:
//!
//! - **Positive**: every line of the committed acceptance fixtures — real
//!   captures from the codecarbon sampler, the claude-code hook shim and
//!   the OTLP receiver — must parse. If tightening validation ever starts
//!   rejecting events our own collectors emit, that is a release blocker,
//!   not a test to relax.
//! - **Negative**: the constraints the Rust types cannot express
//!   (`energy_j` minimum, `event_id` minLength, RFC 3339 timestamps,
//!   non-empty `components`) must be rejected as
//!   [`RejectReason::Schema`], not silently accepted.

use af_events::{parse_line, RejectReason};
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/acceptance")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture at {path:?}: {e}"))
}

/// Every non-empty line of `name` must survive `parse_line`.
fn assert_fixture_parses(name: &str) {
    let text = fixture(name);
    let mut parsed = 0usize;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Ok(_) => parsed += 1,
            Err(reason) => panic!(
                "acceptance fixture {name} line {} was rejected: {reason}\nline: {line}",
                i + 1
            ),
        }
    }
    assert!(parsed > 0, "fixture {name} contributed no events");
}

#[test]
fn codecarbon_acceptance_fixture_parses() {
    assert_fixture_parses("codecarbon.4848dec5-894b-43c9-806a-b7991cb5b216.jsonl");
}

#[test]
fn cc_hooks_acceptance_fixture_parses() {
    assert_fixture_parses("cc-hooks.4848dec5-894b-43c9-806a-b7991cb5b216.jsonl");
}

#[test]
fn otlp_acceptance_fixture_parses() {
    assert_fixture_parses("otlp-cc.4848dec5-894b-43c9-806a-b7991cb5b216.jsonl");
}

/// A well-formed envelope with one field swapped out, so each negative
/// case differs from a known-good line by exactly the thing under test.
fn energy_line(energy_j: &str) -> String {
    format!(
        r#"{{"schema_version":"0.1.0","event_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","ts":"2026-07-25T12:00:00Z","collector":{{"name":"codecarbon","version":"0.1.0"}},"session_id":"sess-1","type":"energy_sample","payload":{{"t_start":"2026-07-25T12:00:00Z","t_end":"2026-07-25T12:00:10Z","components":[{{"kind":"cpu","energy_j":{energy_j},"method":"rapl"}}]}}}}"#
    )
}

fn assert_schema_reject(line: &str, expect_in_message: &str) {
    match parse_line(line) {
        Err(RejectReason::Schema(msg)) => assert!(
            msg.contains(expect_in_message),
            "expected schema reject mentioning {expect_in_message:?}, got: {msg}"
        ),
        other => panic!("expected RejectReason::Schema, got {other:?}\nline: {line}"),
    }
}

#[test]
fn baseline_energy_line_is_accepted() {
    parse_line(&energy_line("12.5")).expect("the unmodified baseline must parse");
}

#[test]
fn negative_energy_is_a_schema_reject() {
    // The Rust type is `f64`, so serde alone would happily accept -500.
    assert_schema_reject(&energy_line("-500"), "/payload/components/0/energy_j");
}

#[test]
fn empty_components_is_a_schema_reject() {
    let line = energy_line("12.5").replace(
        r#""components":[{"kind":"cpu","energy_j":12.5,"method":"rapl"}]"#,
        r#""components":[]"#,
    );
    assert_schema_reject(&line, "/payload/components");
}

#[test]
fn short_event_id_is_a_schema_reject() {
    let line = energy_line("12.5").replace("01ARZ3NDEKTSV4RRFFQ69G5FAV", "evt-1");
    assert_schema_reject(&line, "/event_id");
}

#[test]
fn malformed_timestamp_is_a_schema_reject() {
    let line = energy_line("12.5").replace(r#""ts":"2026-07-25T12:00:00Z""#, r#""ts":"yesterday""#);
    assert_schema_reject(&line, "/ts");
}

#[test]
fn malformed_interval_timestamp_is_a_schema_reject() {
    let line = energy_line("12.5").replace(
        r#""t_end":"2026-07-25T12:00:10Z""#,
        r#""t_end":"2026-07-25 12:00:10""#,
    );
    assert_schema_reject(&line, "/payload/t_end");
}

#[test]
fn negative_token_count_is_a_schema_reject() {
    let line = r#"{"schema_version":"0.1.0","event_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","ts":"2026-07-25T12:00:00Z","collector":{"name":"cc-hooks","version":"0.1.0"},"session_id":"sess-1","type":"llm_call","payload":{"provider":"anthropic","model_id_requested":"claude-sonnet-5","usage":{"input_tokens":-5},"usage_source":"api_response"}}"#;
    assert_schema_reject(line, "/payload/usage/input_tokens");
}

#[test]
fn unknown_attribution_field_is_a_schema_reject() {
    let line = energy_line("12.5").replace(
        r#""session_id":"sess-1""#,
        r#""session_id":"sess-1","attribution":{"not_a_field":"x"}"#,
    );
    assert_schema_reject(&line, "/attribution");
}

/// Version mismatch must stay distinguishable from a schema violation:
/// the spool routes the two differently.
#[test]
fn unsupported_version_is_not_reported_as_a_schema_reject() {
    let line =
        energy_line("12.5").replace(r#""schema_version":"0.1.0""#, r#""schema_version":"0.2.0""#);
    match parse_line(&line) {
        Err(RejectReason::UnknownVersion(v)) => assert_eq!(v, "0.2.0"),
        other => panic!("expected RejectReason::UnknownVersion, got {other:?}"),
    }
}

/// The compiled validator is process-wide; a second call must not pay for
/// compilation again. This asserts the `OnceLock` is actually shared
/// rather than rebuilt, which is the whole reason per-line validation is
/// affordable.
#[test]
fn validator_is_compiled_once() {
    let first = af_events::validate::events_validator();
    let second = af_events::validate::events_validator();
    assert!(
        std::ptr::eq(first, second),
        "events_validator must hand back one process-wide instance"
    );
}
