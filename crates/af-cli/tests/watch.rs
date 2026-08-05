//! End-to-end test of `af watch` as a resident process: it must ingest
//! what lands in the spool while it runs, serve the debug console's
//! `/debug` contract from that data, and stop cleanly on `SIGTERM`.
//!
//! Everything here drives the real binary against a tempdir `AF_STATE_DIR`
//! with `--no-sidecars` and `--no-otlp`: no Python, no venv, no network,
//! nothing bound but the ephemeral debug port this test picks itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

fn fixture_line(index: usize) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/spool/basic-session/cc-hooks.sess-basic.jsonl");
    let content = std::fs::read_to_string(path).expect("read fixture");
    content
        .lines()
        .nth(index)
        .unwrap_or_else(|| panic!("fixture has no line {index}"))
        .to_string()
}

fn fixture_lines() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/spool/basic-session/cc-hooks.sess-basic.jsonl");
    std::fs::read_to_string(path)
        .expect("read fixture")
        .lines()
        .map(str::to_string)
        .collect()
}

/// A fresh tempdir with an empty `spool/`, which is the state dir every
/// test here starts from.
fn state_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("spool")).expect("create spool dir");
    dir
}

/// Picks an address by binding `:0` and letting it go.
///
/// Inherently racy: the port is free when we look and may not be when the
/// child gets there, and these tests run in parallel with each other. The
/// alternative (a fixed port) makes two concurrent `cargo test` runs fight
/// every time rather than occasionally, so the race is handled where it
/// shows up instead — see [`Watch::start_with`], which retries on a bind
/// failure.
fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr")
}

/// A running `af watch`, its stderr accumulated in the background.
struct Watch {
    child: Child,
    stderr: Arc<Mutex<String>>,
    addr: SocketAddr,
}

impl Watch {
    /// The common case: no sidecars, no OTLP, an ephemeral debug port.
    ///
    /// Retries on a lost port race. `free_port` can only report that a port
    /// was free a moment ago; another test in this same parallel run may
    /// take it before the child binds, and the child then exits with
    /// "Address already in use" — which surfaced as an unrelated-looking
    /// timeout in whichever test drew the short straw.
    fn start(state_dir: &Path, extra: &[&str]) -> Watch {
        let mut args = vec!["--no-sidecars", "--no-otlp"];
        args.extend_from_slice(extra);
        Watch::start_with(state_dir, &args)
    }

    /// The same retry, with every flag under the caller's control — for the
    /// tests that need the sampler path to really run, so: no
    /// `--no-sidecars`. They used to pick their own port inline and skip
    /// the retry, which made them the ones that lost the race.
    fn start_with(state_dir: &Path, args: &[&str]) -> Watch {
        Watch::start_with_env(state_dir, args, &[])
    }

    fn start_with_env(state_dir: &Path, args: &[&str], env: &[(&str, String)]) -> Watch {
        for attempt in 0..5 {
            let watch = Watch::start_at(state_dir, free_addr(), args, env);
            match watch.wait_until_bound() {
                Ok(()) => return watch,
                Err(stderr) => {
                    assert!(
                        stderr.contains("Address already in use"),
                        "af watch failed to start on attempt {attempt}; stderr:\n{stderr}"
                    );
                }
            }
        }
        panic!("af watch lost the port race five times running");
    }

    /// `Ok(())` once the debug port accepts a connection, `Err(stderr)` if
    /// the child instead reported a bind failure or died.
    fn wait_until_bound(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(&self.addr, Duration::from_millis(200)).is_ok() {
                return Ok(());
            }
            let stderr = self.stderr();
            if stderr.contains("failed to bind debug server") {
                return Err(stderr);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!(
            "af watch never bound {}; stderr:\n{}",
            self.addr,
            self.stderr()
        ))
    }

    /// Spawns one `af watch --debug` on `addr`, whatever happens next.
    fn start_at(
        state_dir: &Path,
        addr: SocketAddr,
        extra: &[&str],
        env: &[(&str, String)],
    ) -> Watch {
        let bin = assert_cmd::cargo::cargo_bin("af");
        let mut command = Command::new(bin);
        command
            .env("AF_STATE_DIR", state_dir)
            .args(["watch", "--debug"])
            .args(["--debug-addr", &addr.to_string()])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }

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

        Watch {
            child,
            stderr,
            addr,
        }
    }

    fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr sink").clone()
    }

    /// Blocks until `predicate` holds over a `GET path` body, or panics
    /// with the accumulated stderr — which is where a watch that failed to
    /// start says why.
    fn poll_json(&self, path: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = Value::Null;
        while Instant::now() < deadline {
            if let Some((status, body)) = http_get(self.addr, path) {
                if status == 200 {
                    if let Ok(value) = serde_json::from_str::<Value>(&body) {
                        if predicate(&value) {
                            return value;
                        }
                        last = value;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "timed out waiting on GET {path}\nlast body: {last}\nstderr:\n{}",
            self.stderr()
        );
    }

    /// Sends `SIGTERM` and asserts the process exits 0 within the grace
    /// period. `kill(1)` rather than a libc dependency: this is exactly the
    /// signal an operator or a supervisor would send.
    fn terminate(mut self) -> String {
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

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Minimal HTTP/1.1 GET over a raw socket — the crate has no HTTP client
/// dependency and this test needs exactly one verb against localhost.
/// `Connection: close` makes the response end at EOF.
///
/// Chunked bodies are decoded: tiny_http streams anything past a few KiB
/// with `Transfer-Encoding: chunked`, and a snapshot of a real session is
/// well past that. Reading bytes rather than a `String` because a chunk
/// boundary may land in the middle of a UTF-8 character.
fn http_get(addr: SocketAddr, path: &str) -> Option<(u16, String)> {
    let (status, _head, body) = http_get_raw(addr, path, &addr.to_string(), &[])?;
    Some((status, body))
}

/// As [`http_get`], but with the `Host` and any extra headers under the
/// caller's control, and the response head returned — the loopback guards
/// are decided from request headers and observed in response headers.
fn http_get_raw(
    addr: SocketAddr,
    path: &str,
    host: &str,
    extra: &[(&str, &str)],
) -> Option<(u16, String, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (field, value) in extra {
        request.push_str(&format!("{field}: {value}\r\n"));
    }
    request.push_str("\r\n");
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
    Some((status, head, String::from_utf8_lossy(&body).into_owned()))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// HTTP/1.1 chunked transfer decoding, enough for a well-formed response.
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
        if size == 0 {
            return Some(out);
        }
        let start = eol + 2;
        out.extend_from_slice(body.get(start..start + size)?);
        body = body.get(start + size + 2..)?;
    }
}

/// `count` schema-valid `llm_call` lines, cloned from the fixture with a
/// fresh `event_id` each. Used to build a frame log — and a
/// `/debug/snapshot` body — far larger than any single socket buffer or
/// connection queue, which is the only way to exercise back-pressure for
/// real.
fn bulk_lines(count: usize) -> Vec<String> {
    let template = fixture_line(1);
    assert!(
        template.contains("\"type\":\"llm_call\""),
        "fixture line 1 is the llm_call this helper clones: {template}"
    );
    (0..count)
        .map(|i| {
            template.replace(
                "\"event_id\":\"sess-basic-evt-02\"",
                // The schema's `event_id` minLength is 16; a narrower
                // counter would make every bulk line a schema reject.
                &format!("\"event_id\":\"bulk-event-{i:06}\""),
            )
        })
        .collect()
}

fn append_lines(spool_dir: &Path, file: &str, lines: &[String]) {
    std::fs::create_dir_all(spool_dir).expect("create spool dir");
    let path = spool_dir.join(file);
    let mut handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open spool file");
    for line in lines {
        writeln!(handle, "{line}").expect("append spool line");
    }
    handle.flush().expect("flush spool line");
}

#[test]
fn slow_estimation_does_not_block_live_ingest() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");
    let estimate_started = dir.path().join("estimate-started");
    let fake =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake_sidecar.py");
    let watch = Watch::start_with_env(
        dir.path(),
        &["--no-otlp"],
        &[
            ("AF_ESTIMATOR_SCRIPT", fake.display().to_string()),
            ("AF_ESTIMATOR_PYTHON", "python3".to_string()),
            ("AF_FAKE_ESTIMATE_DELAY", "3".to_string()),
            (
                "AF_FAKE_ESTIMATE_STARTED_FILE",
                estimate_started.display().to_string(),
            ),
        ],
    );

    append_lines(
        &spool_dir,
        "cc-hooks.sess-basic.jsonl",
        &[fixture_line(0), fixture_line(1)],
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !estimate_started.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        estimate_started.exists(),
        "fake estimator never entered estimate; stderr:\n{}",
        watch.stderr()
    );

    let second = fixture_line(0)
        .replace("sess-basic-evt-01", "second-session-evt-01")
        .replace("sess-basic", "second-session");
    let started = Instant::now();
    append_lines(&spool_dir, "cc-hooks.second-session.jsonl", &[second]);
    // Per-session route: bare `/debug/session` now answers with the
    // *latest-active* session (by `t_last`), which stays `sess-basic` here —
    // the second session's payload existing at all is what proves its pass
    // ran without waiting on the worker.
    watch.poll_json("/debug/session?session_id=second-session", |session| {
        session["session_id"] == serde_json::json!("second-session")
    });
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the watch loop waited for the worker's three-second estimate"
    );

    watch.terminate();
}

/// Two agents' sessions must be served side by side: the second session's
/// pass must not erase the first from `/debug/health`, both must be
/// individually addressable, and `/debug/sessions` must list them for the
/// console's picker. This is the multi-agent contract — before it, every
/// one of these payloads was last-writer-wins.
#[test]
fn two_sessions_are_served_side_by_side() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");
    let watch = Watch::start(dir.path(), &[]);

    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &fixture_lines());
    watch.poll_json("/debug/session?session_id=sess-basic", |session| {
        session["session_id"] == serde_json::json!("sess-basic")
    });

    let second: Vec<String> = fixture_lines()
        .iter()
        .map(|line| line.replace("sess-basic", "second-session"))
        .collect();
    append_lines(&spool_dir, "cc-hooks.second-session.jsonl", &second);
    watch.poll_json("/debug/session?session_id=second-session", |session| {
        session["session_id"] == serde_json::json!("second-session")
    });

    // The picker's list names both, each with its agent identity.
    let sessions = watch.poll_json("/debug/sessions", |list| {
        list.as_array().map(|rows| rows.len()).unwrap_or(0) == 2
    });
    for row in sessions.as_array().expect("sessions array") {
        assert_eq!(row["agent_app"]["name"], "claude-code", "{row}");
        assert!(row["t_last"].is_string(), "{row}");
    }

    // Reports are per session, stamped with the id the client keys on.
    let report = watch.poll_json("/debug/report?session_id=sess-basic", |report| {
        report["session_id"] == serde_json::json!("sess-basic")
    });
    assert_eq!(report["level"], "session");

    // Health accumulates across passes: the second session's pass loaded
    // only its own events, and the first session's collector row must
    // still be there.
    let health = watch.poll_json("/debug/health", |health| {
        health["collectors"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["session_id"] == "second-session"))
    });
    assert!(
        health["collectors"]
            .as_array()
            .expect("collectors array")
            .iter()
            .any(|r| r["session_id"] == "sess-basic"),
        "first session's collector row was erased: {health}"
    );

    watch.terminate();
}

#[test]
fn watch_ingests_live_appends_serves_the_debug_contract_and_stops_cleanly() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");

    let watch = Watch::start(dir.path(), &[]);
    let expected_console_url = format!("debug console on http://{}/", watch.addr);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !watch.stderr().contains(&expected_console_url) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let startup_log = watch.stderr();
    assert!(
        startup_log.contains(&expected_console_url),
        "startup log did not advertise the console root {expected_console_url:?}:\n{startup_log}"
    );
    assert!(
        !startup_log.contains(&format!("http://{}/debug\n", watch.addr)),
        "startup log advertised the API prefix as a browser URL:\n{startup_log}"
    );
    // Appended *after* the watch is resident: this is the live-ingest path,
    // not the "spool already had content at startup" one.
    let lines = fixture_lines();
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &lines);

    // --- §2.1 /debug/session -------------------------------------------
    let session = watch.poll_json("/debug/session", |value| {
        value.get("session_id").and_then(Value::as_str) == Some("sess-basic")
    });
    assert_eq!(session["schema_version"], serde_json::json!("0.1.0"));
    assert_eq!(session["mode"], serde_json::json!("watch --debug"));
    assert_eq!(
        session["session_meta"]["agent_app"]["name"],
        serde_json::json!("claude-code")
    );
    // The fixture declares FRA; the zone must come from the session, not a
    // default silently substituted for it.
    assert_eq!(session["grid"]["zone"], serde_json::json!("FRA"));
    // No estimator sidecar ran, so there is no electricity-mix factor. It
    // must be null, never a plausible-looking number.
    assert!(session["grid"]["g_co2e_per_kwh"].is_null());
    assert_eq!(
        session["state_dir"],
        serde_json::json!(dir.path().display().to_string())
    );

    // --- §2.2 /debug/snapshot ------------------------------------------
    let snapshot = watch.poll_json("/debug/snapshot?window=180s", |value| {
        !value["allocations"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true)
    });
    let events = snapshot["events"].as_array().expect("events array");
    assert_eq!(
        events.len(),
        lines.len(),
        "every fixture event is a fact frame"
    );
    assert!(events
        .iter()
        .any(|event| event["type"] == serde_json::json!("energy_sample")));
    // Contract #1 envelopes, verbatim — not a reshaped projection.
    assert!(events
        .iter()
        .all(|event| event["schema_version"] == serde_json::json!("0.1.0")));
    assert!(snapshot["as_of_seq"].as_u64().is_some());
    // The only span collector in this PoC emits spans on close, so there is
    // never an open one to report. Empty, not fabricated.
    assert_eq!(snapshot["open_spans"], serde_json::json!([]));
    assert!(snapshot["watchdog"].is_array());
    assert!(snapshot["coverage_gaps"].is_array());

    // --- §2.4 allocation traces ----------------------------------------
    let allocations = snapshot["allocations"].as_array().expect("allocations");
    let trace = &allocations[0];
    let sample_id = trace["sample_event_id"].as_str().expect("sample_event_id");
    assert_eq!(
        trace["attribution_policy"],
        serde_json::json!("l2_cpu_time")
    );
    let rows_j: f64 = trace["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["allocated_j"].as_f64().expect("allocated_j"))
        .sum();
    let total = rows_j
        + trace["agent_process"]["allocated_j"]
            .as_f64()
            .expect("agent")
        + trace["baseline"]["allocated_j"].as_f64().expect("baseline");
    let declared = trace["total_j"].as_f64().expect("total_j");
    assert!(
        (total - declared).abs() < 0.01,
        "trace arithmetic: Σrows + agent + baseline = {total}, total_j = {declared}"
    );

    let (status, body) =
        http_get(watch.addr, &format!("/debug/alloc/{sample_id}")).expect("GET alloc");
    assert_eq!(status, 200);
    let fetched: Value = serde_json::from_str(&body).expect("alloc json");
    assert_eq!(fetched["sample_event_id"], serde_json::json!(sample_id));

    let (status, _) =
        http_get(watch.addr, "/debug/alloc/no-such-sample").expect("GET missing alloc");
    assert_eq!(
        status, 404,
        "an unknown sample is a 404, not an empty trace"
    );

    // --- §2.6 /debug/report --------------------------------------------
    let report = watch.poll_json("/debug/report", |value| value.get("impact_join").is_some());
    assert_eq!(report["level"], serde_json::json!("session"));
    assert_eq!(
        report["impact_join"]["unit"]["level"],
        serde_json::json!("session")
    );
    assert_eq!(
        report["impact_join"]["unit"]["session_id"],
        serde_json::json!("sess-basic")
    );
    let histogram = report["estimation_status_histogram"]
        .as_object()
        .expect("histogram");
    for status in ["ok", "unknown_model", "missing_zone", "pending", "error"] {
        assert!(
            histogram.contains_key(status),
            "histogram is zero-filled: {status}"
        );
    }
    // No estimator ran, so both fixture llm_calls are honestly pending.
    assert_eq!(histogram["pending"], serde_json::json!(2));
    assert_eq!(report["by_model"].as_array().expect("by_model").len(), 1);

    // --- §2.7 /debug/health --------------------------------------------
    let health = watch.poll_json("/debug/health", |value| {
        !value["collectors"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true)
    });
    let collector = &health["collectors"][0];
    assert_eq!(collector["name"], serde_json::json!("cc-hooks"));
    assert_eq!(collector["transport"], serde_json::json!("jsonl spool"));
    assert_eq!(
        collector["spool_file"],
        serde_json::json!("cc-hooks.sess-basic.jsonl")
    );
    assert!(collector["byte_offset"].as_u64().expect("byte_offset") > 0);
    assert_eq!(collector["events"], serde_json::json!(lines.len()));
    // The receiver was disabled for this run and says so rather than
    // reporting a port it never bound.
    assert!(health["otlp_receiver"]["endpoint"].is_null());
    assert_eq!(
        health["otlp_receiver"]["protocol"],
        serde_json::json!("http/json")
    );
    assert!(health["python"].is_array());
    // gap #9: conformance counters were never agreed, and their ABSENCE is
    // the honest signal — an empty array would claim they were counted.
    assert!(health.get("conformance").is_none());

    // --- stderr decision stream ----------------------------------------
    let stderr = watch.terminate();
    assert!(stderr.contains("[ingest] "), "stderr:\n{stderr}");
    assert!(stderr.contains("llm_call "), "stderr:\n{stderr}");
    assert!(stderr.contains("[span open] "), "stderr:\n{stderr}");
    assert!(stderr.contains("[attr] "), "stderr:\n{stderr}");
    assert!(
        stderr.contains("af watch: shutting down"),
        "SIGTERM must run the graceful path; stderr:\n{stderr}"
    );
}

#[test]
fn the_sse_stream_delivers_named_frames_for_events_ingested_after_it_subscribed() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");

    let watch = Watch::start(dir.path(), &[]);
    // Seed one event so the watch is demonstrably alive before subscribing.
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &[fixture_line(0)]);
    watch.poll_json("/debug/session", |value| !value.is_null());

    let mut stream =
        TcpStream::connect_timeout(&watch.addr, Duration::from_secs(2)).expect("connect SSE");
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("read timeout");
    write!(
        stream,
        "GET /debug/stream HTTP/1.1\r\nHost: {}\r\nOrigin: http://localhost:5173\r\nAccept: text/event-stream\r\n\r\n",
        watch.addr
    )
    .expect("send SSE request");

    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut header = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read header line");
        assert!(read > 0, "server closed before finishing headers");
        header.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    assert!(
        header.contains("text/event-stream"),
        "SSE content type missing:\n{header}"
    );
    assert!(
        header.contains("Access-Control-Allow-Origin: http://localhost:5173"),
        "the console is served from another loopback origin in dev, so that \
         origin must be reflected back:\n{header}"
    );

    // Now append the rest of the fixture. These events are ingested while
    // the subscription is live, so they must arrive as frames.
    let rest: Vec<String> = fixture_lines().into_iter().skip(1).collect();
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &rest);

    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => seen.push_str(&line),
            Err(_) => break,
        }
        if seen.contains("event: fact") && seen.contains("event: decision") {
            break;
        }
    }

    assert!(
        seen.contains("event: fact"),
        "no fact frame streamed:\n{seen}"
    );
    assert!(
        seen.contains("event: decision"),
        "no decision frame streamed:\n{seen}"
    );
    assert!(
        seen.contains("\nid: "),
        "frames must carry a seq id:\n{seen}"
    );
    assert!(
        seen.contains("\"kind\":\"ingest\""),
        "decision frames use the design-log vocabulary:\n{seen}"
    );

    drop(reader);
    drop(stream);
    watch.terminate();
}

/// How many events the back-pressure tests ingest. Chosen so the frame log
/// (one `fact` + one `decision` per event) stays inside the 8192-frame ring
/// while the replay — and the snapshot body — is several times any socket
/// buffer or the 1024-frame connection queue.
const BULK_EVENTS: usize = 3000;

/// Reads SSE `id:` lines off a raw chunked stream. Chunk-size lines are
/// hex digits on their own line and never match, so no chunk parsing is
/// needed to count frames.
fn read_frame_ids(reader: &mut BufReader<TcpStream>, want: usize, deadline: Instant) -> Vec<i64> {
    let mut ids = Vec::new();
    while ids.len() < want && Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(id) = line.trim().strip_prefix("id: ") {
                    if let Ok(id) = id.parse::<i64>() {
                        ids.push(id);
                    }
                }
                if line.trim() == "event: reset" {
                    ids.push(-1);
                    break;
                }
            }
        }
    }
    ids
}

/// DATA-CONTRACT §2.3: a resuming client gets a **complete** replay or an
/// explicit `reset` — never a partial one, which it has no way to detect.
///
/// The replay used to be `try_send` into the same bounded queue the live
/// fan-out uses, so a client resuming across more than 1024 frames was
/// silently handed the first 1024 and then the live tail, with an
/// undetectable hole where its history should have been.
#[test]
fn a_replay_far_longer_than_the_connection_queue_arrives_without_a_hole() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");
    append_lines(
        &spool_dir,
        "cc-hooks.sess-basic.jsonl",
        &bulk_lines(BULK_EVENTS),
    );

    let watch = Watch::start(dir.path(), &[]);
    watch.poll_json("/debug/snapshot?window=180s", |value| {
        value["events"].as_array().map(Vec::len).unwrap_or(0) >= BULK_EVENTS
    });

    let mut stream =
        TcpStream::connect_timeout(&watch.addr, Duration::from_secs(2)).expect("connect SSE");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    // `from=-1` is the empty-ring cursor: "I have seen nothing, replay
    // everything you still hold", including frame 0.
    write!(
        stream,
        "GET /debug/stream?from=-1 HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n",
        watch.addr
    )
    .expect("send SSE request");

    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let ids = read_frame_ids(
        &mut reader,
        BULK_EVENTS,
        Instant::now() + Duration::from_secs(30),
    );

    if ids.first() == Some(&-1) {
        // A `reset` is the other legal answer — the ring having outrun the
        // cursor. It must be the *only* thing sent, not a truncated replay
        // with a reset stapled to it.
        assert_eq!(
            ids.len(),
            1,
            "a reset replaces a replay, it never joins one"
        );
    } else {
        assert!(
            ids.len() >= BULK_EVENTS,
            "replay stopped after {} frames — the connection queue is 1024, and this is what \
             truncation looks like",
            ids.len()
        );
        assert_eq!(ids[0], 0, "frame 0 is replayed, never skipped");
        assert!(
            ids.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "the replay must be contiguous; first discontinuity at {:?}",
            ids.windows(2).find(|pair| pair[1] != pair[0] + 1)
        );
    }

    drop(reader);
    drop(stream);
    watch.terminate();
}

/// A client that opens a socket and never reads it must cost only itself.
///
/// Every `/debug` response is written off the accept thread for this reason:
/// with the response written inline, one stalled reader of a large snapshot
/// blocks the single accept loop, and the server stops answering *every*
/// route — then fails to shut down, because the accept thread is still
/// blocked in that write when `stop()` joins it.
#[test]
fn a_client_that_never_reads_wedges_neither_the_next_request_nor_shutdown() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");
    append_lines(
        &spool_dir,
        "cc-hooks.sess-basic.jsonl",
        &bulk_lines(BULK_EVENTS),
    );

    let watch = Watch::start(dir.path(), &[]);
    watch.poll_json("/debug/snapshot?window=180s", |value| {
        value["events"].as_array().map(Vec::len).unwrap_or(0) >= BULK_EVENTS
    });

    // Four requests for a body of well over a megabyte, none of them ever
    // read. Held open for the rest of the test.
    let stalled: Vec<TcpStream> = (0..4)
        .map(|_| {
            let mut socket = TcpStream::connect_timeout(&watch.addr, Duration::from_secs(2))
                .expect("connect stalled client");
            write!(
                socket,
                "GET /debug/snapshot?window=180s HTTP/1.1\r\nHost: {}\r\n\r\n",
                watch.addr
            )
            .expect("send stalled request");
            socket.flush().expect("flush stalled request");
            socket
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));

    let (status, _) = http_get(watch.addr, "/debug/session")
        .expect("a stalled client must not wedge the accept loop");
    assert_eq!(status, 200);

    // …and shutdown must not wait on them either. `terminate` asserts a
    // clean exit within its own grace period.
    let started = Instant::now();
    let stderr = watch.terminate();
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "SIGTERM took {:?} with stalled clients attached; stderr:\n{stderr}",
        started.elapsed()
    );
    drop(stalled);
}

/// A sampler that dies is a **reported** coverage gap and a **bounded**
/// respawn — one gap frame per death (not per pass), on a widening backoff
/// (not once every 2 s forever).
///
/// The fake interpreter exits immediately, which is what a venv missing
/// codecarbon, or a machine with no readable energy counter, looks like from
/// here. It also stands in for the case `try_wait` exists to catch: a
/// sampler that dies *after* its one `watch` op was written, which no
/// subsequent write would ever notice because there is no subsequent write.
#[test]
fn a_sampler_that_dies_is_reported_once_per_death_and_respawned_with_backoff() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &fixture_lines());

    let bin_dir = dir.path().join("venv/bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake venv");
    let python = bin_dir.join("python");
    std::fs::write(
        &python,
        "#!/bin/sh\nprintf '%s\\n' '{\"id\":1,\"ok\":true,\"status\":\"ready\"}'\nexit 7\n",
    )
    .expect("write fake python");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake python");
    }

    // Note: no `--no-sidecars`, so the sampler path really runs.
    let watch = Watch::start_with(dir.path(), &["--no-otlp"]);

    // 2s, then 4s, then 8s: three spawns in this window, where an unbounded
    // respawn would manage one per 2s pass.
    std::thread::sleep(Duration::from_secs(13));

    let snapshot = watch.poll_json("/debug/snapshot?window=180s", |value| {
        !value["coverage_gaps"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true)
    });
    let gaps = snapshot["coverage_gaps"].as_array().expect("gaps").len();
    let stderr = watch.stderr();
    let spawns = stderr.matches("shared codecarbon sampler ready").count();
    let deaths = stderr.matches("shared sampler exited (").count();

    assert!(
        spawns >= 2,
        "the sampler must be respawned; stderr:\n{stderr}"
    );
    assert!(
        spawns <= 4,
        "{spawns} spawns in 13s is a respawn storm, not a backoff; stderr:\n{stderr}"
    );
    assert_eq!(
        gaps, deaths,
        "exactly one coverage gap per death — not one per pass; stderr:\n{stderr}"
    );
    assert!(
        deaths >= spawns - 1,
        "every dead sampler must be observed dead ({deaths} deaths for {spawns} spawns); \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("coverage gap published"),
        "the death is stated on stderr too; stderr:\n{stderr}"
    );

    watch.terminate();
}

/// With no venv at all, the reason for a session having no local energy is
/// stated once — not silently, and not once per pass.
#[test]
fn a_session_with_no_managed_venv_is_told_so_exactly_once() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &fixture_lines());

    let watch = Watch::start_with(dir.path(), &["--no-otlp"]);
    watch.poll_json("/debug/session", |value| !value.is_null());
    // Several passes' worth.
    std::thread::sleep(Duration::from_secs(5));

    let stderr = watch.terminate();
    assert_eq!(
        stderr.matches("no managed venv under").count(),
        1,
        "said once per session, not once per pass; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("sess-basic"),
        "the note names the session it costs; stderr:\n{stderr}"
    );
}

#[test]
fn watch_without_debug_binds_no_debug_port_and_still_ingests() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &fixture_lines());

    // Never bound by this run — `--debug-addr` without `--debug` is exactly
    // what the test is here to prove is inert — so there is no race to lose.
    let addr = free_addr();

    let bin = assert_cmd::cargo::cargo_bin("af");
    let mut child = Command::new(bin)
        .env("AF_STATE_DIR", dir.path())
        .args(["watch", "--no-sidecars", "--no-otlp"])
        .args(["--debug-addr", &addr.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn af watch");

    // Give it time to run its startup pass.
    let db = dir.path().join("state.db");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !db.exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(db.exists(), "watch must ingest without --debug");
    assert!(
        http_get(addr, "/debug/session").is_none(),
        "the debug server is a --debug-only surface"
    );

    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    let status = child.wait().expect("wait");
    assert!(status.success(), "clean exit without --debug too: {status}");
}

/// Binding to loopback is not the same as being loopback-only. A page on
/// the open web can resolve its own hostname to `127.0.0.1` and `fetch`
/// this port; what it cannot forge is the `Host` header, and what the
/// browser will not let it read without a matching
/// `Access-Control-Allow-Origin` is the response body.
#[test]
fn the_debug_server_refuses_foreign_hosts_and_reflects_only_loopback_origins() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");

    let watch = Watch::start(dir.path(), &[]);
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &[fixture_line(0)]);
    watch.poll_json("/debug/session", |value| !value.is_null());

    // A rebinding attacker's request: the socket is loopback, the Host is
    // not.
    let (status, _head, body) =
        http_get_raw(watch.addr, "/debug/snapshot", "evil.example", &[]).expect("request");
    assert_eq!(status, 403, "a non-loopback Host must be refused: {body}");
    assert!(
        body.contains("forbidden_host"),
        "the refusal must say why: {body}"
    );

    // The console's own dev origin is reflected, so its fetches work.
    let (status, head, _body) = http_get_raw(
        watch.addr,
        "/debug/snapshot",
        &watch.addr.to_string(),
        &[("Origin", "http://localhost:5173")],
    )
    .expect("request");
    assert_eq!(status, 200);
    assert!(
        head.contains("Access-Control-Allow-Origin: http://localhost:5173"),
        "the console's dev origin must be reflected:\n{head}"
    );

    // A foreign origin gets no CORS header at all — not `*`, not an echo.
    let (status, head, _body) = http_get_raw(
        watch.addr,
        "/debug/snapshot",
        &watch.addr.to_string(),
        &[("Origin", "http://evil.example")],
    )
    .expect("request");
    assert_eq!(
        status, 200,
        "the request itself is fine; the browser is what must refuse to read it"
    );
    assert!(
        !head.contains("Access-Control-Allow-Origin"),
        "a foreign origin must be granted nothing:\n{head}"
    );

    watch.terminate();
}

/// The health payload reported the process-wide reject total against every
/// collector row, so one collector writing malformed lines made every other
/// collector in the session look equally broken — and a reader trying to
/// find the bad one had nothing to go on.
#[test]
fn reject_counts_are_per_collector_not_the_process_wide_total() {
    let dir = state_dir();
    let spool_dir = dir.path().join("spool");

    let watch = Watch::start(dir.path(), &[]);

    // One healthy collector…
    append_lines(&spool_dir, "cc-hooks.sess-basic.jsonl", &fixture_lines());
    // …and one writing garbage into the same session.
    append_lines(
        &spool_dir,
        "bad-collector.sess-basic.jsonl",
        &[
            "not valid json at all".to_string(),
            "{\"also\": \"not an event\"}".to_string(),
        ],
    );

    let health = watch.poll_json("/debug/health", |value| {
        value["collectors"]
            .as_array()
            .map(|rows| !rows.is_empty())
            .unwrap_or(false)
            && value["rejected_total"].as_u64().unwrap_or(0) >= 2
    });

    let rows = health["collectors"].as_array().expect("collectors");
    let cc = rows
        .iter()
        .find(|row| row["name"] == serde_json::json!("cc-hooks"))
        .expect("the healthy collector has a row");
    assert_eq!(
        cc["rejected"],
        serde_json::json!(0),
        "cc-hooks wrote nothing malformed; another collector's rejects are not its own: {health}"
    );

    // The process-wide total still counts them, so nothing is hidden —
    // it is just no longer attributed to whoever happens to be listed.
    assert!(health["rejected_total"].as_u64().expect("rejected_total") >= 2);
    assert_eq!(health["rejected_spool"], health["rejected_total"]);
    assert_eq!(health["rejected_otlp"], serde_json::json!(0));

    watch.terminate();
}
