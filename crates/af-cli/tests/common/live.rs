//! Harness for the **live** end-to-end suites: the tests that spawn a real
//! coding-agent session against a real `af watch` and watch the whole
//! pipeline light up through the current debug contract.
//!
//! Live suites are `#[ignore]`d by default and never run under a plain
//! `cargo test`: they cost tokens, need the agent CLI installed and logged
//! in, and take minutes. Run them explicitly with `scripts/test-live.sh`
//! (or `cargo test -p af-cli --test live_<agent> -- --ignored`). CI keeps
//! excluding them for free until a job opts in with `-- --ignored`.
//!
//! Isolation contract — a live run must be indistinguishable from a user's
//! machine *except* where it points:
//!
//! * `AF_STATE_DIR` is a tempdir, so the spool, store and offsets of the
//!   developer's real sessions are never read or written.
//! * The agent runs in a temp project dir, so this repo's
//!   `.claude/settings.json` (which targets the real state dir and the
//!   default ports) never applies. The hooks + OTEL env come from a
//!   generated settings file passed via `--settings`.
//! * Ports are ephemeral, taken with the same bind-`:0`-and-retry dance as
//!   `watch.rs`, so a developer's resident `af watch` can keep running.
//!
//! One deliberate non-isolation: the agent CLI's own auth/config (`$HOME`)
//! is inherited — a live test is *supposed* to exercise the real agent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Default wall-clock budget for one agent session. A `-p` turn with two
/// tool calls is usually well under a minute; the budget is generous
/// because a cold agent start (model routing, auth refresh) is not a
/// failure. Override with `AF_LIVE_TIMEOUT_SECS`.
const SESSION_TIMEOUT: Duration = Duration::from_secs(300);

/// Default model for live sessions — an alias the agent CLI resolves, kept
/// cheap on purpose. Override with `AF_LIVE_MODEL` (the estimator only
/// knows registered models, so pick one EcoLogits recognises when the
/// assertions care about estimates).
const DEFAULT_MODEL: &str = "haiku";

/// The repository root, from this crate's manifest directory.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A fresh tempdir with an empty `spool/` — the state dir every live test
/// starts from.
pub fn state_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("spool")).expect("create spool dir");
    dir
}

/// Picks an address by binding `:0` and letting it go. Racy by nature; the
/// race is handled in [`LiveWatch::start`]'s retry, same as `watch.rs`.
fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr")
}

/// Polls `f` every 500 ms until it returns `Some`, or panics after
/// `deadline` with `what` — condition-based waiting, never a bare sleep.
pub fn wait_until<T>(deadline: Duration, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + deadline;
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(
            Instant::now() < end,
            "timed out after {}s waiting for {what}",
            deadline.as_secs()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The session id encoded in the first `<collector>.<sid>.jsonl` spool
/// filename, once the collector has written one.
pub fn spooled_session_id(state_dir: &Path, collector: &str) -> Option<String> {
    let prefix = format!("{collector}.");
    let entries = std::fs::read_dir(state_dir.join("spool")).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix(&prefix) {
            if let Some(sid) = rest.strip_suffix(".jsonl") {
                return Some(sid.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// LiveWatch — a real `af watch --debug` on ephemeral ports
// ---------------------------------------------------------------------------

/// A running `af watch --debug`, stderr accumulated in the background.
///
/// Unlike `watch.rs`'s `Watch`, the OTLP receiver is on by default (a live
/// agent session is exactly the case it exists for) and also on an
/// ephemeral port.
pub struct LiveWatch {
    child: Child,
    stderr: Arc<Mutex<String>>,
    pub debug_addr: SocketAddr,
    pub otlp_addr: SocketAddr,
}

impl LiveWatch {
    /// Spawns `af watch --debug` against `state_dir` with ephemeral debug
    /// and OTLP ports, retrying lost port races.
    pub fn start(state_dir: &Path, extra: &[&str]) -> LiveWatch {
        for attempt in 0..5 {
            let watch = LiveWatch::start_at(state_dir, free_addr(), free_addr(), extra);
            match watch.wait_until_bound() {
                Ok(()) => return watch,
                Err(stderr) => {
                    assert!(
                        stderr.contains("Address already in use")
                            || stderr.contains("failed to bind"),
                        "af watch failed to start on attempt {attempt}; stderr:\n{stderr}"
                    );
                }
            }
        }
        panic!("af watch lost the port race five times running");
    }

    fn start_at(
        state_dir: &Path,
        debug_addr: SocketAddr,
        otlp_addr: SocketAddr,
        extra: &[&str],
    ) -> LiveWatch {
        let bin = assert_cmd::cargo::cargo_bin("af");
        let mut command = Command::new(bin);
        command
            .env("AF_STATE_DIR", state_dir)
            .args(["watch", "--debug"])
            .args(["--debug-addr", &debug_addr.to_string()])
            .args(["--otlp-addr", &otlp_addr.to_string()])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().expect("spawn af watch");
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let handle = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            let reader = BufReader::new(handle);
            for line in reader.lines().map_while(Result::ok) {
                let mut sink = sink.lock().expect("stderr sink");
                sink.push_str(&line);
                sink.push('\n');
            }
        });

        LiveWatch {
            child,
            stderr,
            debug_addr,
            otlp_addr,
        }
    }

    fn wait_until_bound(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(&self.debug_addr, Duration::from_millis(200)).is_ok() {
                return Ok(());
            }
            let stderr = self.stderr();
            if stderr.contains("failed to bind") {
                return Err(stderr);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!(
            "af watch never bound {}; stderr:\n{}",
            self.debug_addr,
            self.stderr()
        ))
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr sink").clone()
    }

    /// Blocks until `predicate` holds over a `GET path` body, or panics
    /// with the accumulated stderr. Live deadlines are the caller's: spool
    /// ingest is seconds, but an OTLP exporter flush can trail the agent's
    /// exit by tens of seconds.
    pub fn poll_json(
        &self,
        path: &str,
        deadline: Duration,
        predicate: impl Fn(&Value) -> bool,
    ) -> Value {
        let end = Instant::now() + deadline;
        let mut last = Value::Null;
        while Instant::now() < end {
            if let Some((status, body)) = http_get(self.debug_addr, path) {
                if status == 200 {
                    if let Ok(value) = serde_json::from_str::<Value>(&body) {
                        if predicate(&value) {
                            return value;
                        }
                        last = value;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!(
            "timed out after {}s waiting on GET {path}\nlast body: {last}\nstderr:\n{}",
            deadline.as_secs(),
            self.stderr()
        );
    }

    /// Sends `SIGTERM` and asserts a clean exit, returning the stderr.
    pub fn terminate(mut self) -> String {
        let pid = self.child.id();
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .expect("send SIGTERM");

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "af watch exited {status} after SIGTERM; stderr:\n{}",
                        self.stderr()
                    );
                    return self.stderr();
                }
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    panic!(
                        "af watch did not exit within 15s of SIGTERM; stderr:\n{}",
                        self.stderr()
                    );
                }
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}

impl Drop for LiveWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

// ---------------------------------------------------------------------------
// Agent drivers — one per integrated coding agent
// ---------------------------------------------------------------------------

/// Drives one real Claude Code `-p` session, fully instrumented, isolated
/// from the developer's own configuration (see the module docs).
///
/// Future agent integrations add a sibling driver here (same shape: a
/// `preflight()` that names its missing prerequisite, a `run_session` that
/// points the agent's collector at the harness's state dir and ports) and a
/// `live_<agent>.rs` suite next to `live_claude_code.rs`.
pub struct ClaudeCode {
    pub model: String,
    pub timeout: Duration,
}

impl ClaudeCode {
    /// Checks the `claude` CLI is reachable and reads the env knobs. Panics
    /// with the remedy, not just the fact: live suites are run by a human
    /// who wants to know what to install.
    pub fn preflight() -> ClaudeCode {
        let found = Command::new("claude")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(
            found,
            "live test needs the `claude` CLI on PATH (and logged in) — install Claude Code first"
        );
        let model = std::env::var("AF_LIVE_MODEL")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let timeout = std::env::var("AF_LIVE_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(SESSION_TIMEOUT);
        ClaudeCode { model, timeout }
    }

    /// The settings file a live session runs under — the same hooks + OTEL
    /// env as `.claude/settings.json`, retargeted at the harness's state
    /// dir and OTLP port.
    fn settings(&self, state_dir: &Path, otlp_addr: SocketAddr) -> Value {
        let hook = repo_root().join("collectors/claude-code/af-hook.sh");
        assert!(hook.is_file(), "hook script missing: {}", hook.display());
        let hook_entry =
            json!([{ "hooks": [{ "type": "command", "command": hook.to_string_lossy() }] }]);
        json!({
            "env": {
                "AF_STATE_DIR": state_dir.to_string_lossy(),
                "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
                "OTEL_LOGS_EXPORTER": "otlp",
                "OTEL_METRICS_EXPORTER": "otlp",
                "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json",
                "OTEL_EXPORTER_OTLP_ENDPOINT": format!("http://{otlp_addr}"),
                // Keep export lag short; harmless where unsupported. The
                // exporter also flushes on session exit, which is what the
                // suites actually wait on.
                "OTEL_METRIC_EXPORT_INTERVAL": "5000",
                "OTEL_LOGS_EXPORT_INTERVAL": "2500",
            },
            "hooks": {
                "SessionStart": hook_entry.clone(),
                "PreToolUse": hook_entry.clone(),
                "PostToolUse": hook_entry.clone(),
                "Stop": hook_entry.clone(),
                "SessionEnd": hook_entry,
            },
        })
    }

    /// Runs one `-p` session in a throwaway project dir and returns its
    /// stdout. Panics on non-zero exit or timeout, with both output
    /// streams.
    ///
    /// `--allowedTools "Bash Read"` keeps the headless session able to do
    /// the smoke prompt's work without inheriting whatever permission state
    /// the developer's real config carries.
    pub fn run_session(&self, state_dir: &Path, otlp_addr: SocketAddr, prompt: &str) -> String {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(
            project.path().join("README.md"),
            "# live e2e fixture\n\nA throwaway project for agentic-footprint's live tests.\n",
        )
        .expect("write fixture README");
        let settings_path = project.path().join("af-live-settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&self.settings(state_dir, otlp_addr))
                .expect("serialise settings"),
        )
        .expect("write settings");

        let mut child = Command::new("claude")
            .current_dir(project.path())
            // Belt and braces with the settings `env` block: the hook shim
            // reads `$AF_STATE_DIR` from whatever environment it inherits.
            .env("AF_STATE_DIR", state_dir)
            // `--allowedTools` is variadic and would swallow a trailing
            // positional prompt — keep a fixed-arity flag between them.
            .args(["--print", "--allowedTools", "Bash Read"])
            .args(["--model", &self.model])
            .args(["--settings", &settings_path.to_string_lossy()])
            .arg(prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn claude");

        let stdout = drain(child.stdout.take().expect("piped stdout"));
        let stderr = drain(child.stderr.take().expect("piped stderr"));

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait().expect("try_wait claude") {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    panic!(
                        "claude -p did not finish within {}s\nstdout:\n{}\nstderr:\n{}",
                        self.timeout.as_secs(),
                        take(&stdout),
                        take(&stderr)
                    );
                }
                None => std::thread::sleep(Duration::from_millis(200)),
            }
        };
        let (out, err) = (take(&stdout), take(&stderr));
        assert!(
            status.success(),
            "claude -p exited {status}\nstdout:\n{out}\nstderr:\n{err}"
        );
        out
    }
}

/// Accumulates a child stream in the background — a `-p` session's output
/// is small, but reading it concurrently is what keeps a chatty child from
/// ever deadlocking on a full pipe.
fn drain(handle: impl Read + Send + 'static) -> Arc<Mutex<String>> {
    let sink = Arc::new(Mutex::new(String::new()));
    let writer = Arc::clone(&sink);
    std::thread::spawn(move || {
        let reader = BufReader::new(handle);
        for line in reader.lines().map_while(Result::ok) {
            let mut writer = writer.lock().expect("drain sink");
            writer.push_str(&line);
            writer.push('\n');
        }
    });
    sink
}

fn take(sink: &Arc<Mutex<String>>) -> String {
    sink.lock().expect("drain sink").clone()
}

// ---------------------------------------------------------------------------
// Minimal HTTP GET (same shape as watch.rs's — a live suite is a separate
// test binary, so it compiles its own copy via `mod common`)
// ---------------------------------------------------------------------------

fn http_get(addr: SocketAddr, path: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    write!(stream, "{request}").ok()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let split = find(&raw, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = &raw[split + 4..];
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;

    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    Some((status, String::from_utf8_lossy(&body).into_owned()))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(body.len());
    loop {
        let eol = find(body, b"\r\n")?;
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&body[..eol])
                .split(';')
                .next()?
                .trim(),
            16,
        )
        .ok()?;
        body = &body[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = &body[size + 2..];
    }
}
