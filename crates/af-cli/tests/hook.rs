//! Behavioral tests for `af hook`, the built-in Claude Code hooks
//! collector — the same scenarios `collectors/claude-code/test_hooks.sh`
//! pins for the sh shim, driven through the real binary so they run on
//! every platform the binary targets (this suite is the Windows-side
//! behavioral gate for the collector).
//!
//! Every scenario drives `af hook` with a tempdir `AF_STATE_DIR` and a
//! hook-event JSON on stdin, then asserts on the spool lines and the
//! open-span scratch files. Every emitted line must parse as a known
//! Contract #1 event (`af_events::parse_line`), which includes the
//! embedded-schema validation.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::{json, Value};

const SESSION: &str = "sess-hook-test";

fn state_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Runs `af hook` with `payload` on stdin and asserts the always-exit-0
/// contract along the way.
fn run_hook(state: &Path, payload: &Value) {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.env("AF_STATE_DIR", state)
        .arg("hook")
        .write_stdin(payload.to_string())
        .assert()
        .success();
}

fn spool_path(state: &Path, session: &str) -> PathBuf {
    state
        .join("spool")
        .join(format!("cc-hooks.{session}.jsonl"))
}

fn openspan_path(state: &Path, session: &str, tool_use_id: &str) -> PathBuf {
    state
        .join("tmp")
        .join("openspans")
        .join(session)
        .join(tool_use_id)
}

/// The session's spool lines, each required to be a *known* valid
/// Contract #1 event, returned as raw JSON values for field asserts.
fn spool_events(state: &Path, session: &str) -> Vec<Value> {
    let contents = std::fs::read_to_string(spool_path(state, session)).unwrap_or_default();
    contents
        .lines()
        .map(|line| {
            // parse_line returns a typed Envelope only for schema-valid,
            // known event types — exactly the bar every emitted line must
            // clear.
            af_events::parse_line(line)
                .unwrap_or_else(|reject| panic!("spool line rejected ({reject}): {line}"));
            serde_json::from_str(line).unwrap()
        })
        .collect()
}

#[test]
fn session_start_emits_bootstrap_span_then_meta() {
    let state = state_dir();
    run_hook(
        state.path(),
        &json!({"session_id": SESSION, "hook_event_name": "SessionStart"}),
    );

    let events = spool_events(state.path(), SESSION);
    assert_eq!(
        events.len(),
        2,
        "expected bootstrap + meta, got {events:#?}"
    );

    let boot = &events[0];
    assert_eq!(boot["type"], "action_span");
    assert_eq!(
        boot["payload"]["span_id"],
        format!("session-boot-{SESSION}")
    );
    assert_eq!(boot["payload"]["tool_name"], "__session__");
    assert_eq!(boot["payload"]["tool_kind"], "other");
    assert_eq!(boot["payload"]["execution_locus"], "local");
    assert_eq!(boot["payload"]["t_start"], boot["payload"]["t_end"]);
    assert_eq!(boot["payload"]["status"], "ok");
    // Spawned directly by this test process (no shell wrapper), so the
    // recorded parent pid must be this very process — the property that
    // makes process-tree attribution work when Claude Code is the parent.
    assert_eq!(
        boot["payload"]["pids"],
        json!([std::process::id()]),
        "bootstrap pids must be the direct parent process"
    );

    let meta = &events[1];
    assert_eq!(meta["type"], "session_meta");
    assert_eq!(meta["payload"]["agent_app"]["name"], "claude-code");
    assert_eq!(meta["ts"], boot["ts"], "one invocation, one instant");
}

#[test]
fn pre_then_post_closes_an_observed_span() {
    let state = state_dir();
    let pre = json!({
        "session_id": SESSION,
        "hook_event_name": "PreToolUse",
        "tool_use_id": "toolu_01",
        "tool_name": "Bash",
    });
    run_hook(state.path(), &pre);

    let open = openspan_path(state.path(), SESSION, "toolu_01");
    let record: Value = serde_json::from_str(&std::fs::read_to_string(&open).unwrap()).unwrap();
    let observed_start = record["t_start"].as_str().unwrap().to_string();
    assert_eq!(record["tool_name"], "Bash");
    assert!(
        spool_events(state.path(), SESSION).is_empty(),
        "PreToolUse must not write to the spool"
    );

    let post = json!({
        "session_id": SESSION,
        "hook_event_name": "PostToolUse",
        "tool_use_id": "toolu_01",
        "tool_name": "Bash",
    });
    run_hook(state.path(), &post);

    let events = spool_events(state.path(), SESSION);
    assert_eq!(events.len(), 1);
    let span = &events[0];
    assert_eq!(span["type"], "action_span");
    assert_eq!(span["payload"]["span_id"], "toolu_01");
    assert_eq!(span["payload"]["tool_kind"], "bash");
    assert_eq!(span["payload"]["execution_locus"], "local");
    assert_eq!(span["payload"]["t_start"], observed_start.as_str());
    assert_eq!(span["payload"]["t_end"], span["ts"]);
    assert_eq!(span["payload"]["status"], "ok");
    assert_eq!(span["attribution"]["tool_call_id"], "toolu_01");
    assert!(!open.exists(), "the open-span file must be consumed");
}

#[test]
fn failure_event_maps_interrupt_to_cancelled_and_else_to_error() {
    for (is_interrupt, expected) in [(true, "cancelled"), (false, "error")] {
        let state = state_dir();
        run_hook(
            state.path(),
            &json!({
                "session_id": SESSION,
                "hook_event_name": "PreToolUse",
                "tool_use_id": "toolu_f",
                "tool_name": "Bash",
            }),
        );
        run_hook(
            state.path(),
            &json!({
                "session_id": SESSION,
                "hook_event_name": "PostToolUseFailure",
                "tool_use_id": "toolu_f",
                "tool_name": "Bash",
                "is_interrupt": is_interrupt,
            }),
        );
        let events = spool_events(state.path(), SESSION);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]["payload"]["status"], expected,
            "is_interrupt={is_interrupt}"
        );
    }
}

#[test]
fn post_without_open_span_closes_an_unknown_point_span() {
    let state = state_dir();
    run_hook(
        state.path(),
        &json!({
            "session_id": SESSION,
            "hook_event_name": "PostToolUse",
            "tool_use_id": "toolu_lost",
            "tool_name": "Read",
        }),
    );
    let events = spool_events(state.path(), SESSION);
    assert_eq!(events.len(), 1);
    let span = &events[0];
    // Never fabricate a start we didn't observe: point span, unknown wins
    // over the known ok outcome.
    assert_eq!(span["payload"]["t_start"], span["payload"]["t_end"]);
    assert_eq!(span["payload"]["status"], "unknown");
    assert_eq!(span["payload"]["tool_kind"], "file_op");
}

#[test]
fn post_with_corrupt_open_span_closes_honestly_and_removes_the_file() {
    let state = state_dir();
    let open = openspan_path(state.path(), SESSION, "toolu_bad");
    std::fs::create_dir_all(open.parent().unwrap()).unwrap();
    std::fs::write(&open, "{not json").unwrap();

    run_hook(
        state.path(),
        &json!({
            "session_id": SESSION,
            "hook_event_name": "PostToolUse",
            "tool_use_id": "toolu_bad",
            "tool_name": "Bash",
        }),
    );
    let events = spool_events(state.path(), SESSION);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["status"], "unknown");
    assert_eq!(
        events[0]["payload"]["t_start"],
        events[0]["payload"]["t_end"]
    );
    assert!(
        !open.exists(),
        "a corrupt open-span file must still be removed"
    );
}

#[test]
fn stop_sweeps_only_this_sessions_open_spans() {
    let state = state_dir();
    for tool_use in ["toolu_a", "toolu_b"] {
        run_hook(
            state.path(),
            &json!({
                "session_id": SESSION,
                "hook_event_name": "PreToolUse",
                "tool_use_id": tool_use,
                "tool_name": "WebSearch",
            }),
        );
    }
    // A concurrent session's in-flight span must be invisible to the sweep.
    run_hook(
        state.path(),
        &json!({
            "session_id": "sess-other",
            "hook_event_name": "PreToolUse",
            "tool_use_id": "toolu_other",
            "tool_name": "Bash",
        }),
    );

    run_hook(
        state.path(),
        &json!({"session_id": SESSION, "hook_event_name": "Stop"}),
    );

    let events = spool_events(state.path(), SESSION);
    assert_eq!(events.len(), 2, "one closing span per open span");
    for event in &events {
        // The sweep never knows the outcome; tool_name is recovered from
        // the open-span file so classification still works.
        assert_eq!(event["payload"]["status"], "unknown");
        assert_eq!(event["payload"]["tool_kind"], "web");
        assert_eq!(event["payload"]["execution_locus"], "remote");
    }
    let span_ids: Vec<&str> = events
        .iter()
        .map(|event| event["payload"]["span_id"].as_str().unwrap())
        .collect();
    assert_eq!(span_ids, ["toolu_a", "toolu_b"]);

    let own_dir = openspan_path(state.path(), SESSION, "x")
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(!own_dir.exists(), "the swept session dir must be removed");
    assert!(
        openspan_path(state.path(), "sess-other", "toolu_other").exists(),
        "another session's open span must survive the sweep"
    );
}

#[test]
fn sweep_skips_corrupt_files_but_still_cleans_up() {
    let state = state_dir();
    run_hook(
        state.path(),
        &json!({
            "session_id": SESSION,
            "hook_event_name": "PreToolUse",
            "tool_use_id": "toolu_good",
            "tool_name": "Grep",
        }),
    );
    let corrupt = openspan_path(state.path(), SESSION, "toolu_corrupt");
    std::fs::write(&corrupt, "]]]").unwrap();

    run_hook(
        state.path(),
        &json!({"session_id": SESSION, "hook_event_name": "SessionEnd"}),
    );

    let events = spool_events(state.path(), SESSION);
    assert_eq!(events.len(), 1, "only the parseable span is emitted");
    assert_eq!(events[0]["payload"]["span_id"], "toolu_good");
    assert!(!corrupt.exists(), "corrupt files are removed, not emitted");
}

#[test]
fn garbage_stdin_exits_zero_and_emits_nothing() {
    let state = state_dir();
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.env("AF_STATE_DIR", state.path())
        .arg("hook")
        .write_stdin("this is not json")
        .assert()
        .success();
    assert!(
        !state.path().join("spool").exists(),
        "nothing may be emitted"
    );
}

#[test]
fn unknown_event_is_a_forward_compatible_no_op() {
    let state = state_dir();
    run_hook(
        state.path(),
        &json!({"session_id": SESSION, "hook_event_name": "SomeFutureEvent"}),
    );
    assert!(!state.path().join("spool").exists());
}

#[test]
fn envelope_field_order_matches_the_contract() {
    let state = state_dir();
    run_hook(
        state.path(),
        &json!({
            "session_id": SESSION,
            "hook_event_name": "PostToolUse",
            "tool_use_id": "toolu_ord",
            "tool_name": "Bash",
        }),
    );
    let contents = std::fs::read_to_string(spool_path(state.path(), SESSION)).unwrap();
    let line = contents.lines().next().unwrap();
    let order = [
        "\"schema_version\"",
        "\"event_id\"",
        "\"ts\"",
        "\"collector\"",
        "\"session_id\"",
        "\"attribution\"",
        "\"type\"",
        "\"payload\"",
    ];
    let mut last = 0;
    for key in order {
        let position = line[last..]
            .find(key)
            .unwrap_or_else(|| panic!("{key} missing or out of order in {line}"));
        last += position;
    }
}

/// The same conformance vectors that pin `sanitize_id` across the sh hook,
/// af-otlp, and af_sampler, applied to the path components this collector
/// builds from raw ids.
#[test]
fn session_ids_are_sanitized_per_the_shared_vectors() {
    let vectors: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/sanitize-vectors.json"),
        )
        .unwrap(),
    )
    .unwrap();

    for vector in vectors {
        let raw = vector["raw"].as_str().unwrap();
        let sanitized = vector["sanitized"].as_str().unwrap();
        let state = state_dir();
        run_hook(
            state.path(),
            &json!({"session_id": raw, "hook_event_name": "SessionStart"}),
        );
        assert!(
            spool_path(state.path(), sanitized).exists(),
            "raw session id {raw:?} must land in spool file for {sanitized:?}"
        );
        let events = spool_events(state.path(), sanitized);
        assert_eq!(events[0]["session_id"], sanitized);
    }
}
