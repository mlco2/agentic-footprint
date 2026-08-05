//! Shared wiring for the end-to-end suites that drive the real `af` binary
//! against a tempdir `AF_STATE_DIR`.
//!
//! Everything here was written three times over — once each in
//! `report_join.rs`, `statusline.rs` and `ingest.rs` — and the copies had
//! begun to differ in ways that mattered: which fixtures got seeded, whether
//! a developer's exported `AF_ESTIMATOR_*` could leak into a run that was
//! supposed to have no estimator at all. One copy, so a suite that seeds
//! differently is doing so visibly.
//!
//! The fixture arithmetic constants live here for the same reason: two of
//! the suites assert numbers derived from the *same* fixture and the *same*
//! golden transcript, and a change to either must break both.

// Each integration test binary compiles this module and uses a subset of
// it; the rest is dead code from that binary's point of view, which is a
// fact about `mod common` and not about the code.
#![allow(dead_code)]

pub mod live;

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

// ---------------------------------------------------------------------------
// Fixture arithmetic, hand-computed from
// `tests/fixtures/spool/basic-session/cc-hooks.sess-basic.jsonl` (the
// derivation is written out in `report_join.rs`) and
// `tests/fixtures/sidecar/report-join.jsonl`.
// ---------------------------------------------------------------------------

/// Every joule the basic-session fixture's three energy samples report:
/// 9.8 + 11.2 + 9.5.
pub const SESSION_J: f64 = 30.5;
/// Joules per kWh.
pub const J_PER_KWH: f64 = 3.6e6;
/// `gwp_kg_per_kwh` for zone FRA in the golden transcript (point value).
pub const FRA_GWP: f64 = 0.0511;

/// The repository root, from this crate's manifest directory.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `tests/fixtures/spool/` at the repo root.
pub fn fixtures_dir() -> PathBuf {
    repo_root().join("tests/fixtures/spool")
}

/// Seeds a fresh tempdir's `spool/` with the basic-session fixture only.
///
/// The adversarial fixture is deliberately left out: it carries its own
/// `llm_call`, which would consume a response from the golden transcript
/// and make the expected numbers depend on ingest ordering across files.
/// The suite that wants it asks for it by name.
pub fn seed_state_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    copy_fixture(
        dir.path(),
        "basic-session/cc-hooks.sess-basic.jsonl",
        "cc-hooks.sess-basic.jsonl",
    );
    dir
}

/// The basic-session fixture plus the adversarial one (a valid `llm_call`,
/// an invalid-JSON line, an unknown-`type` line, and a duplicate
/// `event_id`), for the suite that asserts what ingest rejects.
pub fn seed_state_dir_with_adversarial() -> tempfile::TempDir {
    let dir = seed_state_dir();
    copy_fixture(
        dir.path(),
        "adversarial/bad.sess-adv.jsonl",
        "bad.sess-adv.jsonl",
    );
    dir
}

/// Never point `AF_STATE_DIR` at the repo fixtures directly: ingest mutates
/// spool-adjacent state (`rejected/`, `state.db`) in place.
fn copy_fixture(state_dir: &Path, from: &str, to: &str) {
    let spool_dir = state_dir.join("spool");
    fs::create_dir_all(&spool_dir).expect("create spool dir");
    fs::copy(fixtures_dir().join(from), spool_dir.join(to))
        .unwrap_or_else(|err| panic!("copy fixture {from}: {err}"));
}

/// Builds one `af` invocation against `state_dir`.
///
/// `with_estimator` wires the estimator sidecar to
/// `tests/fixtures/fake_sidecar.py --replay
/// tests/fixtures/sidecar/report-join.jsonl` — the golden transcript, so no
/// ecologits, no venv and no network. Without it the `AF_ESTIMATOR_*`
/// variables are actively **removed**: a developer's exported environment
/// must not silently un-degrade a degradation test.
pub fn af_command(state_dir: &Path, args: &[&str], with_estimator: bool) -> Command {
    let mut cmd = Command::cargo_bin("af").expect("af binary");
    cmd.env("AF_STATE_DIR", state_dir)
        .env_remove("AF_LOCAL_GRID_ZONE")
        .env_remove("AF_ZONE")
        .env_remove("AF_REMOTE_REGION")
        .args(args);
    if with_estimator {
        let transcript = repo_root().join("tests/fixtures/sidecar/report-join.jsonl");
        cmd.env(
            "AF_ESTIMATOR_SCRIPT",
            repo_root().join("tests/fixtures/fake_sidecar.py"),
        )
        .env("AF_ESTIMATOR_PYTHON", "python3")
        .env(
            "AF_ESTIMATOR_ARGS",
            format!("--replay\n{}", transcript.display()),
        );
    } else {
        cmd.env_remove("AF_ESTIMATOR_SCRIPT")
            .env_remove("AF_ESTIMATOR_PYTHON")
            .env_remove("AF_ESTIMATOR_ARGS");
    }
    cmd
}

/// Runs one `af` subcommand and requires it to succeed. Returns
/// `(stdout, stderr)` with stdout as raw bytes — the determinism assertions
/// compare it byte-for-byte, so it must never go through a lossy conversion
/// or a JSON round-trip.
pub fn run_af(state_dir: &Path, args: &[&str], with_estimator: bool) -> (Vec<u8>, String) {
    let output = af_command(state_dir, args, with_estimator)
        .output()
        .expect("run af");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "af {args:?} failed: stderr={stderr}"
    );
    (output.stdout, stderr)
}

/// Runs the full report pipeline once against the golden transcript, so the
/// store holds real estimates and joins.
pub fn populate(state_dir: &Path) {
    run_af(state_dir, &["report", "--format", "json"], true);
}

#[track_caller]
pub fn close(actual: f64, expected: f64, what: &str) {
    let tolerance = expected.abs().max(1.0) * 1e-9;
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: expected {expected}, got {actual}"
    );
}
