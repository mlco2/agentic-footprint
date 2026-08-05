//! End-to-end coverage for `af statusline`, the presentation surface
//! `statusline/ecologits-bar.sh` calls once per status-line refresh.
//!
//! The store is seeded exactly the way `tests/report_join.rs` seeds it — the
//! Task 5 spool fixture driven through `af report` with the estimator
//! sidecar replaced by `tests/fixtures/fake_sidecar.py --replay` — so the
//! five numbers asserted here are derivable from the same golden
//! transcript, and a change to the join arithmetic breaks both tests with a
//! concrete expected value.
//!
//! Three properties:
//!
//! 1. **The numbers.** Range means, sourced per the design log's criteria
//!    rules, hand-computed below from the fixture.
//! 2. **The format.** One line, five space-separated plain decimals, in
//!    `gwp water energy adpe pe` order — never scientific notation, because
//!    the bar feeds them straight to `awk`.
//! 3. **Never breaks, never mutates.** Missing store, unknown session,
//!    garbage stdin: exit 0 and `0 0 0 0 0`. And a run against a state dir
//!    with no database leaves it with no database.

use std::fs;
use std::path::Path;

use assert_cmd::Command;

mod common;
use common::{close, populate, seed_state_dir, FRA_GWP, J_PER_KWH, SESSION_J};

// ---------------------------------------------------------------------------
// Expected arithmetic, from `tests/report_join.rs`'s hand-computed fixture
// derivation plus `tests/fixtures/sidecar/report-join.jsonl`:
//
//   session local energy = 30.5 J = 30.5/3.6e6 kWh   (point value)
//   FRA gwp factor       = 0.0511 kgCO2eq/kWh        (point value)
//   remote gwp    total  = [0.000125, 0.00025]  kgCO2eq
//   remote energy total  = [0.00025,  0.0005 ]  kWh
//   remote water  total  = [2.5e-5,   5e-5   ]  L
//   remote adpe   total  = [1.25e-9,  2.5e-9 ]  kgSbeq
//   remote pe     total  = [0.0025,   0.005  ]  MJ
//
// Both llm_calls estimated `ok` and the zone factors are available, so
// `combined_total` carries gwp and energy; water/adpe/pe are remote-only.
// ---------------------------------------------------------------------------

/// `(combined_min + combined_max) / 2` with the local point value on both
/// ends: local_gwp + mean([0.000125, 0.00025]).
fn expected_gwp() -> f64 {
    let local = SESSION_J / J_PER_KWH * FRA_GWP;
    ((local + 0.000125) + (local + 0.00025)) / 2.0
}

/// local energy + mean([0.00025, 0.0005]).
fn expected_energy() -> f64 {
    let local = SESSION_J / J_PER_KWH;
    ((local + 0.00025) + (local + 0.0005)) / 2.0
}

const EXPECTED_WATER: f64 = (2.5e-5 + 5e-5) / 2.0;
const EXPECTED_ADPE: f64 = (1.25e-9 + 2.5e-9) / 2.0;
const EXPECTED_PE: f64 = (0.0025 + 0.005) / 2.0;

/// The exact line the seeded fixture must produce. Hardcoded rather than
/// re-derived in-test so that the *formatting* half of the contract is
/// locked too: plain decimals, never an exponent, one space between fields.
/// The numeric half is checked independently against the constants above.
///
/// The digits are the shortest round-tripping decimal for each `f64`, not a
/// rounded presentation — `0.000037500000000000003` really is the sum
/// `(2.5e-5 + 5e-5) / 2` in binary floating point, and the command rounds
/// nothing on the way out (the bar's formatters decide display precision).
const EXPECTED_LINE: &str = "0.00018793293055555555 0.000037500000000000003 \
     0.0003834722222222222 0.0000000018750000000000002 0.00375";

/// Runs `af statusline` with `stdin` and returns its stdout. Requires exit
/// 0 — the command has no failing exit path by contract.
fn statusline(state_dir: &Path, stdin: &str) -> String {
    let output = Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", state_dir)
        .arg("statusline")
        .write_stdin(stdin.to_string())
        .output()
        .expect("run af statusline");
    assert!(
        output.status.success(),
        "af statusline must always exit 0 (got {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

fn fields(line: &str) -> Vec<f64> {
    let trimmed = line
        .strip_suffix('\n')
        .expect("exactly one line, newline-terminated");
    assert!(
        !trimmed.contains('\n'),
        "statusline must print exactly one line, got {line:?}"
    );
    let fields: Vec<&str> = trimmed.split(' ').collect();
    assert_eq!(fields.len(), 5, "expected 5 fields in {trimmed:?}");
    for f in &fields {
        assert!(
            !f.contains('e') && !f.contains('E'),
            "field {f:?} uses scientific notation, which the bar's awk cannot parse"
        );
    }
    fields
        .iter()
        .map(|f| {
            f.parse::<f64>()
                .unwrap_or_else(|_| panic!("field {f:?} is not a number"))
        })
        .collect()
}

#[test]
fn statusline_prints_the_session_range_means_from_the_seeded_store() {
    let dir = seed_state_dir();
    populate(dir.path());

    let line = statusline(dir.path(), r#"{"session_id":"sess-basic"}"#);

    let values = fields(&line);
    close(values[0], expected_gwp(), "gwp (combined_total mean)");
    close(values[1], EXPECTED_WATER, "water (remote mean)");
    close(values[2], expected_energy(), "energy (combined_total mean)");
    close(values[3], EXPECTED_ADPE, "adpe (remote mean)");
    close(values[4], EXPECTED_PE, "pe (remote mean)");

    assert_eq!(
        line,
        format!("{EXPECTED_LINE}\n"),
        "the exact rendered line is part of the contract"
    );

    // Trailing whitespace/newlines around the JSON are what a shell
    // heredoc (`<<<"$input"`) actually delivers.
    assert_eq!(
        statusline(dir.path(), "{\"session_id\":\"sess-basic\"}\n"),
        line,
        "a trailing newline on stdin must not change the answer"
    );
}

#[test]
fn statusline_ignores_the_extra_fields_claude_code_sends() {
    let dir = seed_state_dir();
    populate(dir.path());

    // A realistic status JSON payload, not just the one key we read.
    let input = r#"{"hook_event_name":"Status","session_id":"sess-basic",
        "transcript_path":"/tmp/nope.jsonl","cwd":"/tmp",
        "model":{"id":"claude-opus-4-6","display_name":"Opus 4.6"},
        "workspace":{"current_dir":"/tmp","project_dir":"/tmp"},
        "cost":{"total_lines_added":3}}"#;
    assert_eq!(statusline(dir.path(), input), format!("{EXPECTED_LINE}\n"));
}

#[test]
fn a_session_with_no_stored_join_prints_zeros() {
    let dir = seed_state_dir();
    populate(dir.path());

    assert_eq!(
        statusline(dir.path(), r#"{"session_id":"sess-never-seen"}"#),
        "0 0 0 0 0\n"
    );
}

#[test]
fn an_absent_store_prints_zeros_and_is_not_created() {
    let dir = tempfile::tempdir().expect("tempdir");

    assert_eq!(
        statusline(dir.path(), r#"{"session_id":"sess-basic"}"#),
        "0 0 0 0 0\n"
    );
    assert!(
        !dir.path().join("state.db").exists(),
        "the statusline must never create the database"
    );
    assert_eq!(
        fs::read_dir(dir.path()).expect("read state dir").count(),
        0,
        "the statusline must not write anything into the state dir"
    );
}

#[test]
fn garbage_stdin_prints_zeros_and_exits_zero() {
    let dir = seed_state_dir();
    populate(dir.path());

    for input in [
        "",
        "\n",
        "not json at all",
        "{",
        "{}",
        "[]",
        r#"{"session_id":""}"#,
        r#"{"session_id":42}"#,
    ] {
        assert_eq!(
            statusline(dir.path(), input),
            "0 0 0 0 0\n",
            "input {input:?} must degrade to zeros"
        );
    }
}

#[test]
fn statusline_does_not_mutate_the_store() {
    let dir = seed_state_dir();
    populate(dir.path());

    let db = dir.path().join("state.db");
    let before = fs::read(&db).expect("read db before");

    // Add a spool line the statusline must NOT ingest: a statusline that
    // ingested would both cost the user latency and race `af watch`.
    let spool = dir.path().join("spool/cc-hooks.sess-basic.jsonl");
    let mut appended = fs::read_to_string(&spool).expect("read spool");
    appended.push_str(
        r#"{"schema_version":"0.1.0","event_id":"evt-statusline-must-not-ingest","ts":"2026-07-25T14:00:20Z","collector":{"name":"cc-hooks","version":"0.1.0"},"session_id":"sess-later","type":"session_meta","payload":{"agent_app":{"name":"claude-code"}}}"#,
    );
    appended.push('\n');
    fs::write(&spool, appended).expect("append to spool");

    let line = statusline(dir.path(), r#"{"session_id":"sess-basic"}"#);
    assert_eq!(line, format!("{EXPECTED_LINE}\n"));

    assert_eq!(
        fs::read(&db).expect("read db after"),
        before,
        "af statusline must not write to state.db"
    );
    assert_eq!(
        statusline(dir.path(), r#"{"session_id":"sess-later"}"#),
        "0 0 0 0 0\n",
        "the appended event must not have been ingested"
    );
}
