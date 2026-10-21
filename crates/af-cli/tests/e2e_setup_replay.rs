//! Hermetic end-to-end proof that **after `af setup`** both agents are
//! connected: real recorded Claude Code hook payloads and OTLP exports, and
//! a real Codex 0.153.2 OTLP export, are delivered *the way the agents would
//! deliver them given the configuration setup wrote* — and come out the
//! other end as estimated inference impacts in `af report` and the
//! statusline.
//!
//! What is real: `af setup`, `af watch` (receiver, spool, ingest, estimator
//! worker, join), the installed sh hook shim, `af report`, `af statusline`,
//! and every input byte (see the fixture READMEs). What is faked: the agent
//! binaries (`claude`/`codex` are stubs, present only so setup detects
//! them) and the estimator, which is `tests/fixtures/fake_sidecar.py` with
//! `AF_FAKE_KNOWN_MODELS` — it only estimates the models in
//! `tests/fixtures/models/agent-models.json`, the list
//! `python/tests/test_model_coverage.py` holds against the real pinned
//! ecologits. A provider or model id the normalizers emit but ecologits
//! would not know therefore fails here, not silently in a user's report.
//!
//! The delivery targets are read back out of the written files, never taken
//! from this test's own variables: a setup that writes the wrong endpoint,
//! the wrong Codex home, or a hook command that does not run fails here.
//!
//! Unix only: the shim runs under `sh`, and shutdown uses `SIGTERM`.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use assert_cmd::Command as AfCommand;
use common::collector;
use common::live::{repo_root, LiveWatch};
use serde_json::Value;

const SETTLE: Duration = Duration::from_secs(60);

/// `session.id` of the recorded Claude Code OTLP export.
const CLAUDE_SESSION: &str = "a238442e-16d9-4c00-8a66-635495f32251";
/// `conversation.id` of the recorded Codex OTLP export.
const CODEX_SESSION: &str = "01a14525-5c46-74a2-9da1-5f4349e45cdb";

/// The recorded real Claude Code hook payloads, in the order Claude Code
/// fires them for one Bash tool call.
const CLAUDE_HOOKS: &[&str] = &[
    "session_start.real.json",
    "pretooluse_bash.real.json",
    "posttooluse_bash.real.json",
    "stop.real.json",
    "sessionend.real.json",
];

struct Machine {
    _root: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    state: PathBuf,
    path: OsString,
}

impl Machine {
    /// A fresh user account: an empty `$HOME`, a project directory, and
    /// `claude`/`codex` stubs first on `PATH` (setup only checks they exist).
    fn new() -> Machine {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let project = root.path().join("project");
        let bin = root.path().join("bin");
        for dir in [&home, &project, &bin] {
            std::fs::create_dir_all(dir).expect("create dir");
        }
        common::stub_executables(&bin, &["claude", "codex"]);
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .expect("join PATH");
        // The default state dir for this HOME — what the hook shim and `af`
        // resolve without AF_STATE_DIR, as on a real machine.
        let state = home.join(".local/state/agentic-footprint");
        Machine {
            _root: root,
            home,
            project,
            state,
            path,
        }
    }

    fn estimator_env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("HOME", self.home.clone().into()),
            ("PATH", self.path.clone()),
            (
                "AF_ESTIMATOR_SCRIPT",
                repo_root().join("tests/fixtures/fake_sidecar.py").into(),
            ),
            ("AF_ESTIMATOR_PYTHON", "python3".into()),
            ("AF_ESTIMATOR_ARGS", "".into()),
            (
                "AF_FAKE_KNOWN_MODELS",
                repo_root()
                    .join("tests/fixtures/models/agent-models.json")
                    .into(),
            ),
        ]
    }

    fn af(&self, args: &[&str]) -> AfCommand {
        let mut command = AfCommand::cargo_bin("af").expect("af binary");
        command.env_clear().envs(self.estimator_env()).args(args);
        command
    }
}

#[test]
fn events_flow_from_setup_written_config_to_estimated_reports() {
    let machine = Machine::new();
    let env = machine.estimator_env();
    let env_refs: Vec<(&str, &OsStr)> = env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();

    // Receiver first, as the installer does; setup then finds it healthy.
    let watch = LiveWatch::start_with_env(&machine.state, &[], &env_refs);
    let endpoint = format!("http://{}/v1/logs", watch.otlp_addr);

    let setup = machine
        .af(&[
            "setup",
            "--yes",
            "--global",
            "--agents",
            "codex,claude-code",
            "--endpoint",
            &endpoint,
            "--project",
        ])
        .arg(&machine.project)
        .output()
        .expect("run af setup");
    assert!(
        setup.status.success(),
        "af setup failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&setup.stdout),
        String::from_utf8_lossy(&setup.stderr)
    );

    // --- Claude Code, driven by what ~/.claude/settings.json says ---------
    let settings: Value = serde_json::from_slice(
        &std::fs::read(machine.home.join(".claude/settings.json")).expect("read settings"),
    )
    .expect("parse settings");
    let claude_env: BTreeMap<String, String> = settings["env"]
        .as_object()
        .expect("settings env")
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
        .collect();
    assert_eq!(claude_env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
    assert_eq!(claude_env["OTEL_LOGS_EXPORTER"], "otlp");
    assert_eq!(claude_env["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/json");

    for name in CLAUDE_HOOKS {
        let mut payload: Value = serde_json::from_slice(
            &std::fs::read(
                repo_root()
                    .join("collectors/claude-code/test-data")
                    .join(name),
            )
            .expect("read hook payload"),
        )
        .expect("parse hook payload");
        // One session across both channels, as in a real Claude Code run.
        payload["session_id"] = Value::String(CLAUDE_SESSION.to_string());
        let event = payload["hook_event_name"].as_str().expect("hook event");
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_else(|| panic!("no hook registered for {event}"));
        run_hook(&machine, &claude_env, command, &payload);
    }

    // OTEL_EXPORTER_OTLP_ENDPOINT is a base URL: the OTLP/HTTP exporter
    // appends the per-signal path.
    let claude_logs = format!("{}/v1/logs", claude_env["OTEL_EXPORTER_OTLP_ENDPOINT"]);
    post_json(
        &claude_logs,
        &repo_root().join("crates/af-otlp/tests/fixtures/otlp/logs-api-request.json"),
    );

    // --- Codex, driven by what ~/.codex/config.toml says -------------------
    let codex: toml::Table = std::fs::read_to_string(machine.home.join(".codex/config.toml"))
        .expect("read codex config")
        .parse()
        .expect("parse codex config");
    let exporter = &codex["otel"]["exporter"]["otlp-http"];
    assert_eq!(exporter["protocol"].as_str(), Some("json"));
    let codex_logs = exporter["endpoint"].as_str().expect("codex endpoint");
    let captures = repo_root().join("tests/fixtures/otlp/codex-0.153.2");
    let mut bodies: Vec<PathBuf> = std::fs::read_dir(&captures)
        .expect("read codex captures")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension() == Some(OsStr::new("json")))
        .collect();
    bodies.sort();
    assert!(!bodies.is_empty());
    for body in &bodies {
        post_json(codex_logs, body);
    }

    // --- Every collector arrived, nothing rejected -------------------------
    let health = watch.poll_json("/debug/health", SETTLE, |health| {
        ["cc-hooks", "otlp-cc", "otlp-codex"].iter().all(|name| {
            collector(health, name).is_some_and(|row| row["events"].as_u64().unwrap_or(0) > 0)
        })
    });
    for name in ["cc-hooks", "otlp-cc", "otlp-codex"] {
        let row = collector(&health, name).expect("collector row");
        assert_eq!(
            row["rejected"], 0,
            "{name} rejected events; health: {health}"
        );
    }
    watch.terminate();

    // --- Both sessions are estimated, none unknown -------------------------
    let report = machine
        .af(&["report", "--format", "json"])
        .output()
        .expect("run af report");
    assert!(
        report.status.success(),
        "af report failed: {}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report: Value = serde_json::from_slice(&report.stdout).expect("parse report");
    for session in [CLAUDE_SESSION, CODEX_SESSION] {
        let remote = &session_join(&report, session)["remote_estimated"];
        assert!(
            remote["llm_calls"].as_u64().unwrap_or(0) > 0,
            "{session}: no llm_calls joined: {remote}"
        );
        let statuses = remote["estimate_status_counts"]
            .as_object()
            .expect("estimate_status_counts");
        assert_eq!(
            statuses.keys().collect::<Vec<_>>(),
            ["ok"],
            "{session}: every llm_call must be estimated: {remote}"
        );
        assert!(
            remote["impacts"]["energy"]["total"]["max"]
                .as_f64()
                .unwrap_or(0.0)
                > 0.0,
            "{session}: no inference energy: {remote}"
        );
    }
    let claude_counts = &session_entry(&report, CLAUDE_SESSION)["event_counts"];
    assert!(
        claude_counts["action_span"].as_u64().unwrap_or(0) > 0,
        "Claude hook spans never reached the store: {claude_counts}"
    );

    // --- The statusline shows it -------------------------------------------
    let statusline = machine
        .af(&["statusline"])
        .write_stdin(format!(r#"{{"session_id": "{CLAUDE_SESSION}"}}"#))
        .output()
        .expect("run af statusline");
    let line = String::from_utf8_lossy(&statusline.stdout);
    let gwp: f64 = line
        .split_whitespace()
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("unparseable statusline: {line:?}"));
    assert!(gwp > 0.0, "statusline shows no impact: {line:?}");
}

/// Runs a registered hook command the way Claude Code does: through the
/// shell, the hook JSON on stdin, the settings `env` block in the
/// environment.
fn run_hook(machine: &Machine, env: &BTreeMap<String, String>, command: &str, payload: &Value) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env_clear()
        .env("HOME", &machine.home)
        .env("PATH", &machine.path)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn hook {command}: {error}"));
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write hook stdin");
    let output = child.wait_with_output().expect("wait for hook");
    assert!(output.status.success(), "hook {command} failed");
    let errors = machine.state.join("tmp/hook-errors.log");
    if let Ok(log) = std::fs::read_to_string(&errors) {
        assert!(log.trim().is_empty(), "hook logged errors:\n{log}");
    }
}

/// POSTs a file as `application/json` (both agents' configured protocol)
/// and requires a 2xx, as an OTLP/HTTP exporter would.
fn post_json(url: &str, body: &Path) {
    let rest = url.strip_prefix("http://").expect("http endpoint");
    let (authority, path) = rest.split_at(rest.find('/').expect("endpoint path"));
    let body = std::fs::read(body).expect("read body");
    let mut stream = TcpStream::connect(authority).expect("connect receiver");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set timeout");
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("write request head");
    stream.write_all(&body).expect("write request body");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response.split_whitespace().nth(1).unwrap_or("");
    assert!(
        status.starts_with('2'),
        "POST {url} answered {status:?}: {response}"
    );
}

fn session_entry<'a>(report: &'a Value, session: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .expect("report sessions")
        .iter()
        .find(|entry| entry["session_id"] == session)
        .unwrap_or_else(|| panic!("session {session} missing from report: {report}"))
}

fn session_join<'a>(report: &'a Value, session: &str) -> &'a Value {
    session_entry(report, session)["joins"]
        .as_array()
        .expect("joins")
        .iter()
        .map(|entry| &entry["join"])
        .find(|join| join["unit"]["level"] == "session")
        .unwrap_or_else(|| panic!("session {session} has no session-level join: {report}"))
}
