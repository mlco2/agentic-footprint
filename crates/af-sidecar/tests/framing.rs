//! Framing tests against `tests/fixtures/fake_sidecar.py` (repo root),
//! run with plain `python3`. Covers: request/response id matching,
//! discarding a stray unmatched-id response, timeout on a silent
//! sidecar, and the error a request gets once the child is killed out
//! from under it.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::json;

use af_sidecar::Sidecar;

fn python3() -> PathBuf {
    PathBuf::from("python3")
}

fn fake_sidecar_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake_sidecar.py")
}

fn spawn_fake() -> Sidecar {
    let script = fake_sidecar_path();
    Sidecar::spawn(&python3(), script.to_str().expect("utf8 path"), &[])
        .expect("spawn fake_sidecar.py")
}

#[cfg(unix)]
fn kill_process(pid: u32) {
    let status = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .expect("run `kill`");
    assert!(status.success(), "kill -9 {pid} failed");
}

#[cfg(windows)]
fn kill_process(pid: u32) {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()
        .expect("run `taskkill`");
    assert!(status.success(), "taskkill /PID {pid} /F failed");
}

#[test]
fn request_response_matches_by_id() {
    let mut sidecar = spawn_fake();
    let resp = sidecar
        .request(&json!({"op": "echo", "value": 42}))
        .expect("request should succeed");
    assert_eq!(resp["op"], "echo");
    assert_eq!(resp["echo"]["value"], 42);
}

#[test]
fn request_waits_out_a_delayed_response_within_timeout() {
    let mut sidecar = spawn_fake();
    sidecar.set_timeout(Duration::from_secs(2));
    let resp = sidecar
        .request(&json!({"op": "sleep", "secs": 0.2}))
        .expect("delayed response should still arrive within the timeout");
    assert_eq!(resp["op"], "sleep");
}

#[test]
fn send_is_fire_and_forget_and_its_response_is_discarded_by_the_next_request() {
    let mut sidecar = spawn_fake();
    // send() never injects an "id", so the fake sidecar echoes back
    // {"id": null, ...}. That response must not corrupt the next
    // request()'s id-matched read — it should be discarded (with a
    // stderr warning) and the loop should keep reading until the real
    // match arrives.
    sidecar
        .send(&json!({"op": "echo", "note": "no id needed"}))
        .expect("send should not block on a response");

    let resp = sidecar
        .request(&json!({"op": "echo", "value": 1}))
        .expect("request after a stray send() response should still match by id");
    assert_eq!(resp["echo"]["value"], 1);
}

#[test]
fn request_times_out_on_silent_sidecar() {
    let mut sidecar = spawn_fake();
    sidecar.set_timeout(Duration::from_millis(200));

    let start = Instant::now();
    let err = sidecar
        .request(&json!({"op": "silent"}))
        .expect_err("a sidecar that never responds must time out");
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(200),
        "returned before the timeout elapsed: {elapsed:?}"
    );
    assert!(
        err.to_string().contains("timed out"),
        "unexpected error message: {err}"
    );
}

/// A sidecar killed out from under the handle must surface as a failed
/// request, not as a silent hang or a fabricated answer — that error is
/// what a supervisor keys its restart policy off. Recovery is by dropping
/// the handle and spawning a fresh one (what `af watch` does), so it is
/// exercised here too.
#[test]
fn a_killed_sidecar_fails_its_next_request_and_a_fresh_spawn_recovers() {
    let mut sidecar = spawn_fake();

    // Sanity: alive and answering before we kill it.
    let resp = sidecar
        .request(&json!({"op": "echo", "value": "before"}))
        .expect("initial request should succeed");
    assert_eq!(resp["echo"]["value"], "before");

    // Force-stop the child process from outside the Sidecar API: SIGKILL on
    // Unix and taskkill /F on Windows.
    let pid = sidecar.pid();
    kill_process(pid);

    // Give the OS a moment to terminate the child and close the pipe so the
    // reader thread observes EOF.
    std::thread::sleep(Duration::from_millis(200));

    sidecar.set_timeout(Duration::from_millis(500));
    let err = sidecar
        .request(&json!({"op": "echo", "value": "dead"}))
        .expect_err("request against a killed sidecar must fail");
    let _ = err;

    drop(sidecar);

    let mut fresh = spawn_fake();
    let resp = fresh
        .request(&json!({"op": "echo", "value": "after"}))
        .expect("a freshly spawned sidecar should answer again");
    assert_eq!(resp["echo"]["value"], "after");
}
