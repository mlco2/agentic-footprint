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
/// spool-adjacent state (`state.db`) in place.
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

// ---------------------------------------------------------------------------
// Fakes for the network and the managed Python runtime
// ---------------------------------------------------------------------------

/// Serves `routes` (path -> (status, body)) until the test process exits;
/// `{base}` in a body is replaced by the server's own URL. Unrouted paths
/// answer 404.
pub fn fake_server(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake server");
    let base = format!("http://{}", listener.local_addr().expect("local addr"));
    let routes: Vec<(String, u16, Vec<u8>)> = routes
        .into_iter()
        .map(|(path, status, body)| {
            let body = match String::from_utf8(body) {
                Ok(text) => text.replace("{base}", &base).into_bytes(),
                Err(error) => error.into_bytes(),
            };
            (path.to_string(), status, body)
        })
        .collect();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buffer = [0_u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]);
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let (status, body) = routes
                .iter()
                .find(|(route, _, _)| *route == path)
                .map(|(_, status, body)| (*status, body.clone()))
                .unwrap_or((404, b"not found".to_vec()));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    base
}

/// A PyPI JSON document listing `(version, upload time)` releases.
pub fn pypi_releases(entries: &[(&str, &str)]) -> Vec<u8> {
    let releases: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .map(|(version, uploaded)| {
            (
                version.to_string(),
                serde_json::json!([{"upload_time_iso_8601": uploaded, "yanked": false}]),
            )
        })
        .collect();
    serde_json::json!({ "releases": releases })
        .to_string()
        .into_bytes()
}

/// This binary's `(ecologits, codecarbon)` floors, from the manifest it
/// embeds.
pub fn floors() -> (String, String) {
    let floors = af_sidecar::floors();
    (floors.ecologits, floors.codecarbon)
}

/// Executables under `bin` that exit 0, standing in for agents on `PATH`.
#[cfg(unix)]
pub fn stub_executables(bin: &Path, names: &[&str]) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(bin).expect("create bin dir");
    for name in names {
        let stub = bin.join(name);
        fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).expect("chmod stub");
    }
}

/// The `/debug/health` collectors row named `name`.
pub fn collector<'a>(health: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    health["collectors"]
        .as_array()?
        .iter()
        .find(|row| row["name"] == name)
}

/// A managed venv under `state` whose interpreter answers every import
/// check and reports these versions on the version query.
#[cfg(unix)]
pub fn fake_runtime_with(state: &Path, ecologits: &str, codecarbon: &str) {
    use std::os::unix::fs::PermissionsExt;
    let python = state.join("venv/bin/python");
    fs::create_dir_all(python.parent().expect("venv bin dir")).expect("create venv");
    fs::write(
        &python,
        format!("#!/bin/sh\nprintf '{ecologits}\\n{codecarbon}\\n'\n"),
    )
    .expect("write fake interpreter");
    fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).expect("chmod interpreter");
}

/// A healthy fake runtime at exactly this binary's floors.
#[cfg(unix)]
pub fn fake_runtime(state: &Path) {
    let (ecologits, codecarbon) = floors();
    fake_runtime_with(state, &ecologits, &codecarbon);
}
