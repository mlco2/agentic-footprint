use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn reports_cli_version() {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("af "));
}

#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn test_help_shows_subcommands() {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.arg("--help")
        .assert()
        .stdout(predicate::str::contains("report"));
}

#[cfg(not(feature = "experimental-opencode"))]
#[test]
fn test_default_setup_help_excludes_experimental_opencode() {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex,claude-code"))
        .stdout(predicate::str::contains("opencode").not());
}

#[cfg(feature = "experimental-opencode")]
#[test]
fn test_experimental_setup_help_includes_opencode() {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex,claude-code,opencode"));
}

#[cfg(unix)]
#[test]
fn test_setup_applies_codex_and_claude_configuration_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let project = dir.path().join("project");
    let codex_home = dir.path().join("codex-home");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&codex_home).unwrap();
    for name in ["codex", "claude", "jq"] {
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let state = dir.path().join("state");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
    let receiver = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        }
    });

    let mut setup = Command::cargo_bin("af").unwrap();
    setup
        .env("PATH", &path)
        .env("CODEX_HOME", &codex_home)
        .env("AF_STATE_DIR", &state)
        .env("AF_SERVICE_MANAGER", "unsupported")
        .args(["setup", "--yes", "--agents", "codex,claude-code"])
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--project")
        .arg(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains("Setup complete"));

    let codex = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
    assert!(codex.contains(&endpoint));
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.join(".claude/settings.json")).unwrap())
            .unwrap();
    assert_eq!(settings["env"]["OTEL_LOGS_EXPORTER"], "otlp");
    let hook = state.join("integrations/claude-code/af-hook.sh");
    assert!(hook.is_file());
    assert_ne!(
        std::fs::metadata(hook).unwrap().permissions().mode() & 0o100,
        0
    );

    let mut check = Command::cargo_bin("af").unwrap();
    check
        .env("PATH", path)
        .env("CODEX_HOME", codex_home)
        .env("AF_STATE_DIR", state)
        .env("AF_SERVICE_MANAGER", "unsupported")
        .args(["setup", "--check", "--agents", "codex,claude-code"])
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--project")
        .arg(project)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "All selected installed agents are configured",
        ));
    receiver.join().unwrap();
}

#[cfg(unix)]
#[test]
fn test_setup_dry_run_stops_before_agent_inspection_when_receiver_is_unhealthy() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
    drop(listener);

    let mut setup = Command::cargo_bin("af").unwrap();
    setup
        .env("AF_STATE_DIR", dir.path().join("state"))
        .env("AF_SERVICE_MANAGER", "unsupported")
        .args(["setup", "--dry-run", "--endpoint"])
        .arg(endpoint)
        .arg("--project")
        .arg(dir.path().join("project-does-not-exist"))
        .assert()
        .success()
        .stdout(predicate::str::contains("receiver: unavailable"))
        .stdout(predicate::str::contains("Start the receiver manually"))
        .stdout(predicate::str::contains(
            "Agent configuration is not inspected until the receiver is healthy.",
        ))
        .stdout(predicate::str::contains("Detected agents:").not());
}

#[cfg(feature = "experimental-opencode")]
#[test]
fn test_opencode_offline_mode_is_finite_and_surfaces_sequence_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("gap.sse");
    std::fs::write(
        &input,
        r#"data: {"id":"evt_gap_123456789","type":"session.unknown","durable":{"aggregateID":"ses_gap","seq":2},"data":{"timestamp":1,"sessionID":"ses_gap"}}

"#,
    )
    .unwrap();
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.env("AF_STATE_DIR", dir.path())
        .args(["collect", "opencode", "--session-id", "ses_gap"])
        .arg("--input")
        .arg(input)
        .arg("--no-session-meta")
        .assert()
        .success()
        .stdout("2\n")
        .stderr(predicate::str::contains(
            "sequence gap for ses_gap: expected 1, received 2",
        ));
}

#[test]
fn test_report_json_with_empty_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.env("AF_STATE_DIR", dir.path())
        .arg("report")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"sessions\":[]"));
}

/// `af watch` is resident now — it does not exit on its own, so this suite
/// only checks its surface. Its behaviour is covered by `tests/watch.rs`,
/// which runs it as a real process against a tempdir and signals it.
#[test]
fn test_watch_flags_are_documented() {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.args(["watch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--debug"))
        .stdout(predicate::str::contains("--no-sidecars"))
        .stdout(predicate::str::contains("--otlp-addr"))
        .stdout(predicate::str::contains("--debug-addr"))
        .stdout(predicate::str::contains("--interval"));
}

/// `--otlp-addr` and `--no-otlp` contradict each other; clap must reject the
/// pair rather than silently letting one win.
#[test]
fn test_watch_rejects_contradictory_otlp_flags() {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.args(["watch", "--no-otlp", "--otlp-addr", "127.0.0.1:4318"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_python_without_action_is_a_usage_error() {
    // `python` now requires a `setup`/`doctor` subcommand; clap itself
    // rejects a bare `af python` with its standard exit code 2, distinct
    // from this project's own "not implemented" stub convention.
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.arg("python").assert().code(2);
}

#[test]
fn test_python_doctor_on_empty_state_dir_lists_actionable_findings() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.env("AF_STATE_DIR", dir.path())
        .arg("python")
        .arg("doctor")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("[error]"))
        .stdout(predicate::str::contains("venv directory missing"))
        .stdout(predicate::str::contains("fix:"));
}

/// `af statusline` is implemented as of Task 14; its behaviour lives in
/// `tests/statusline.rs`. What this suite keeps is the surface: it is a
/// documented subcommand, and it never breaks a status line — no stdin at
/// all still exits 0 with one line of zeros.
#[test]
fn test_statusline_never_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.env("AF_STATE_DIR", dir.path())
        .arg("statusline")
        .write_stdin("")
        .assert()
        .success()
        .stdout("0 0 0 0 0\n");
}

#[test]
fn test_validate_line_accepts_a_valid_event() {
    let line = r#"{"schema_version":"0.1.0","event_id":"evt-0123456789abcdef","ts":"2026-07-25T14:00:00Z","collector":{"name":"cc-hooks","version":"0.1.0"},"session_id":"sess-x","type":"session_meta","payload":{"agent_app":{"name":"claude-code"}}}"#;
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.arg("validate-line")
        .write_stdin(format!("{line}\n"))
        .assert()
        .success();
}

#[test]
fn test_validate_line_rejects_malformed_json_with_reason_on_stderr() {
    let mut cmd = Command::cargo_bin("af").unwrap();
    cmd.arg("validate-line")
        .write_stdin("not json\n")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("invalid JSON"));
}
