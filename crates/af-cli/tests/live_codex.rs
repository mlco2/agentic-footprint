mod common;

use common::live::{spooled_session_id, state_dir, wait_until, LiveWatch};
use std::process::{Command, Stdio};
use std::time::Duration;

const SETTLE: Duration = Duration::from_secs(60);
const DEFAULT_CODEX_MODEL: &str = "gpt-5.4-mini";

#[test]
#[ignore = "live: spawns a real Codex session (tokens, network) — run scripts/test-live.sh codex"]
fn native_otel_reaches_debug_console() {
    let version = Command::new("codex")
        .arg("--version")
        .output()
        .unwrap_or_else(|error| {
            panic!("Codex CLI is not installed ({error})");
        });
    assert!(version.status.success(), "codex --version failed");

    let dir = state_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    std::fs::write(project.path().join("README.md"), "# Codex live fixture\n")
        .expect("write fixture");
    let watch = LiveWatch::start(dir.path(), &["--no-sidecars"]);
    let endpoint = format!("http://{}/v1/logs", watch.otlp_addr);
    let exporter =
        format!("otel.exporter={{otlp-http={{endpoint=\"{endpoint}\",protocol=\"json\"}}}}");
    let model =
        std::env::var("AF_LIVE_CODEX_MODEL").unwrap_or_else(|_| DEFAULT_CODEX_MODEL.to_string());

    let output = Command::new("codex")
        .args([
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
        ])
        .args(["-c", "approval_policy=\"never\""])
        .args(["-c", &exporter])
        .args(["-c", "otel.metrics_exporter=\"none\""])
        .args(["-c", "otel.trace_exporter=\"none\""])
        .args(["-m", &model])
        .args(["-s", "read-only", "-C", &project.path().to_string_lossy()])
        .arg("Read README.md with a shell command, then reply with exactly: ok")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codex exec");
    assert!(
        output.status.success(),
        "codex exec failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let sid = wait_until(SETTLE, "otlp-codex spool file", || {
        spooled_session_id(dir.path(), "otlp-codex")
    });
    watch.poll_json("/debug/snapshot?window=600s", SETTLE, |snapshot| {
        let Some(events) = snapshot["events"].as_array() else {
            return false;
        };
        let has_session = events
            .iter()
            .any(|event| event["session_id"] == sid && event["type"] == "session_meta");
        let has_usage = events.iter().any(|event| {
            event["session_id"] == sid
                && event["type"] == "llm_call"
                && event["payload"]["usage"]["output_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
        });
        let has_command = events.iter().any(|event| {
            event["session_id"] == sid
                && event["type"] == "action_span"
                && event["payload"]["tool_name"] == "exec_command"
        });
        has_session && has_usage && has_command
    });

    watch.terminate();
}
