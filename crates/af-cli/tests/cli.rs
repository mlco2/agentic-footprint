mod common;

use assert_cmd::Command;
#[cfg(unix)]
use common::{fake_runtime, fake_runtime_with};
use predicates::prelude::*;

/// What a healthy af receiver answers: setup tells it from any other OTLP
/// endpoint by the identity header.
#[cfg(unix)]
const AF_RECEIVER_OK: &str = concat!(
    "HTTP/1.1 200 OK\r\nX-Agentic-Footprint: ",
    env!("CARGO_PKG_VERSION"),
    "\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
);

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
            stream.write_all(AF_RECEIVER_OK.as_bytes()).unwrap();
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

/// A receiver that answers every connection with `200 OK`, for as long as
/// the test process lives.
#[cfg(unix)]
fn always_ok_receiver() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(AF_RECEIVER_OK.as_bytes());
        }
    });
    endpoint
}

#[cfg(unix)]
fn stub_agents(bin: &std::path::Path) {
    std::fs::create_dir_all(bin).unwrap();
    for name in ["codex", "claude", "jq"] {
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).unwrap();
    }
}

/// Regression: with `alias codex='CODEX_HOME="$PWD/.codex" codex'`, every
/// Codex session reads `<project>/.codex/config.toml`. Setup configured
/// only the home in its own environment, `--check` reported success, and
/// no Codex event ever reached the receiver.
#[cfg(unix)]
#[test]
fn test_setup_configures_and_checks_a_per_project_codex_home() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_agents(&bin);
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(project.join(".codex")).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let state = dir.path().join("state");
    let endpoint = always_ok_receiver();
    let af = |args: &[&str]| {
        let mut command = Command::cargo_bin("af").unwrap();
        command
            .env("PATH", &path)
            .env("HOME", &home)
            .env_remove("CODEX_HOME")
            .env("AF_STATE_DIR", &state)
            .env("AF_SERVICE_MANAGER", "unsupported")
            .args(args)
            .args(["--agents", "codex", "--endpoint", &endpoint, "--project"])
            .arg(&project);
        command
    };

    // The user home alone being configured is not enough.
    std::fs::write(
        home.join(".codex/config.toml"),
        format!(
            "[otel]\nenvironment = \"dev\"\nlog_user_prompt = false\nmetrics_exporter = \"none\"\n\
             trace_exporter = \"none\"\nexporter = {{ otlp-http = {{ endpoint = \"{endpoint}\", protocol = \"json\" }} }}\n"
        ),
    )
    .unwrap();
    af(&["setup", "--check"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            project.join(".codex/config.toml").display().to_string(),
        ));

    af(&["setup", "--yes"]).assert().success();
    let written = std::fs::read_to_string(project.join(".codex/config.toml")).unwrap();
    let table: toml::Table = written.parse().unwrap();
    assert_eq!(
        table["otel"]["exporter"]["otlp-http"]["endpoint"].as_str(),
        Some(endpoint.as_str())
    );

    af(&["setup", "--check"]).assert().success();
}

/// The resident service runs `af watch --require-receiver`: a port that is
/// already taken must end the process (so launchd/systemd retry) instead of
/// leaving a live watch with no receiver that the manager never restarts.
#[cfg(unix)]
#[test]
fn test_watch_requiring_a_receiver_exits_when_the_port_is_taken() {
    let state = tempfile::tempdir().unwrap();
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let mut watch = Command::cargo_bin("af").unwrap();
    watch
        .env("AF_STATE_DIR", state.path())
        .args([
            "watch",
            "--no-sidecars",
            "--require-receiver",
            "--otlp-addr",
            &addr,
        ])
        .timeout(std::time::Duration::from_secs(20))
        .assert()
        .code(1)
        .stderr(predicate::str::contains("OTLP receiver unavailable"));
    drop(taken);
}

/// A fake af receiver that answers every connection, for `af doctor`.
#[cfg(unix)]
fn always_af_receiver() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(AF_RECEIVER_OK.as_bytes());
        }
    });
    endpoint
}

/// The symptom behind "Claude Code connects only sometimes": a process
/// started before setup fires hooks but exports no token telemetry.
/// `af doctor` must name the session and the remedy.
#[cfg(unix)]
#[test]
fn test_doctor_flags_sessions_with_hooks_but_no_token_telemetry() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../collectors/claude-code/test-data");
    for name in [
        "session_start.real.json",
        "pretooluse_bash.real.json",
        "posttooluse_bash.real.json",
    ] {
        Command::cargo_bin("af")
            .unwrap()
            .env("AF_STATE_DIR", &state)
            .arg("hook")
            .write_stdin(std::fs::read(data.join(name)).unwrap())
            .assert()
            .success();
    }
    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", &state)
        .env_remove("AF_ESTIMATOR_SCRIPT")
        .args(["report", "--format", "json"])
        .assert()
        .success();

    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", &state)
        .env("HOME", &home)
        .env_remove("CODEX_HOME")
        .args(["doctor", "--endpoint", &always_af_receiver()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "76cb257d-0251-4eca-825c-42ab7dff67cd",
        ))
        .stdout(predicate::str::contains("restart"));
}

#[cfg(unix)]
#[test]
fn test_doctor_reports_installed_runtime_versions_below_the_floors() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    fake_runtime_with(&dir.path().join("state"), "0.0.1", "0.0.1");
    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", dir.path().join("state"))
        .env("HOME", &home)
        .env_remove("CODEX_HOME")
        .args(["doctor", "--endpoint", &always_af_receiver()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("! runtime"))
        .stdout(predicate::str::contains("0.0.1"));
}

#[cfg(unix)]
#[test]
fn test_doctor_reports_a_missing_runtime_with_one_remedy() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", dir.path().join("state"))
        .env("HOME", &home)
        .env_remove("CODEX_HOME")
        .args(["doctor", "--endpoint", &always_af_receiver()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("! runtime"))
        .stdout(predicate::str::contains("af self update"))
        .stdout(predicate::str::contains("af python").not());
}

#[cfg(unix)]
#[test]
fn test_doctor_passes_on_a_healthy_empty_install() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    fake_runtime(&dir.path().join("state"));
    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", dir.path().join("state"))
        .env("HOME", &home)
        .env_remove("CODEX_HOME")
        .args(["doctor", "--endpoint", &always_af_receiver()])
        .assert()
        .success()
        .stdout(predicate::str::contains("No problems found"));
}

#[cfg(unix)]
#[test]
fn af_report_makes_the_state_dir_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", &state)
        .args(["report", "--format", "json"])
        .assert()
        .success();
    let mode = std::fs::metadata(&state).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}
