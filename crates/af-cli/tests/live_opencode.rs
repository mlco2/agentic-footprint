mod common;

use common::live::{spooled_session_id, state_dir, wait_until, LiveWatch};
use serde_json::Value;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const SETTLE: Duration = Duration::from_secs(60);

#[test]
#[ignore = "live: starts OpenCode and may use a real provider — run scripts/test-live.sh opencode"]
fn durable_sse_reaches_debug_console() {
    let dir = state_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    std::fs::write(
        project.path().join("README.md"),
        "# OpenCode live fixture\n",
    )
    .expect("write fixture");
    let watch = LiveWatch::start(dir.path(), &["--no-sidecars"]);
    let addr = free_addr();
    let mut server = start_server(project.path(), addr);

    wait_until(SETTLE, "OpenCode server", || {
        TcpStream::connect(addr).ok().map(|_| ())
    });
    let session = post_json(addr, project.path(), "/api/session", "{}");
    let sid = session["data"]["id"]
        .as_str()
        .expect("OpenCode session id")
        .to_string();

    let mut capture = Command::new(env!("CARGO_BIN_EXE_af"))
        .env("AF_STATE_DIR", dir.path())
        .args(["collect", "opencode"])
        .args(["--url", &format!("http://{addr}"), "--session-id", &sid])
        .args(["--directory", &project.path().to_string_lossy()])
        // OpenCode's cursor is exclusive. Starting after sequence 0 replays
        // every post-create durable event, so a fast step start cannot race
        // the collector process startup.
        .args(["--after", "0"])
        .args(["--pid", &server.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn OpenCode collector");

    post_json(
        addr,
        project.path(),
        &format!("/api/session/{sid}/prompt"),
        r#"{"prompt":{"text":"Reply with exactly: ok"}}"#,
    );

    wait_until(SETTLE, "OpenCode llm_call spool event", || {
        let path = dir
            .path()
            .join("spool")
            .join(format!("opencode.{sid}.jsonl"));
        let text = std::fs::read_to_string(path).ok()?;
        text.lines()
            .any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .is_some_and(|event| event["type"] == "llm_call")
            })
            .then_some(())
    });
    assert_eq!(
        spooled_session_id(dir.path(), "opencode").as_deref(),
        Some(sid.as_str())
    );

    watch.poll_json("/debug/snapshot?window=600s", SETTLE, |snapshot| {
        snapshot["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|event| event["session_id"] == sid && event["type"] == "llm_call")
        })
    });

    let _ = capture.kill();
    let _ = capture.wait();
    let _ = server.kill();
    let _ = server.wait();
    watch.terminate();
}

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr")
}

fn start_server(project: &Path, addr: SocketAddr) -> Child {
    let source_repo = std::env::var("AF_LIVE_OPENCODE_REPO").ok();
    let mut command =
        if let Some(repo) = &source_repo {
            let mut command = Command::new("bun");
            command.current_dir(repo).args([
                "run",
                "--conditions=browser",
                "packages/opencode/src/index.ts",
            ]);
            command
        } else {
            let status = Command::new("opencode").arg("--version").output().unwrap_or_else(|error| {
            panic!("OpenCode is not installed ({error}); install it or set AF_LIVE_OPENCODE_REPO")
        });
            assert!(status.status.success(), "opencode --version failed");
            Command::new("opencode")
        };
    if source_repo.is_none() {
        command.current_dir(project);
    }
    command
        .args([
            "serve",
            "--pure",
            "--hostname",
            "127.0.0.1",
            "--port",
            &addr.port().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn OpenCode server")
}

fn post_json(addr: SocketAddr, directory: &Path, path: &str, body: &str) -> Value {
    let output = Command::new("/usr/bin/curl")
        .args([
            "--fail-with-body",
            "--silent",
            "--show-error",
            "--max-time",
            "30",
        ])
        .args(["-X", "POST", "-H", "content-type: application/json"])
        .args([
            "-H",
            &format!("x-opencode-directory: {}", directory.display()),
        ])
        .args(["--data", body, &format!("http://{addr}{path}")])
        .output()
        .expect("run curl");
    assert!(
        output.status.success(),
        "OpenCode request failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("OpenCode JSON response")
}
