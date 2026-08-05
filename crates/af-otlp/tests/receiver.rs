//! Tests against the captured (real, sanitized) OTLP fixture:
//! - `normalize_logs` in isolation, asserting exact token/model/session
//!   mapping and that the output round-trips through `af_events::parse_line`.
//! - the full HTTP path via `af_otlp::serve`, including the malformed-body
//!   and `/v1/metrics` cases.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use af_events::{ExecutionLocus, Payload, Status, ToolKind, UsageSource};

fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/otlp")
        .join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture at {path:?}: {e}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("fixture {path:?} is not valid JSON: {e}"))
}

// ---------------------------------------------------------------------
// normalize_logs
// ---------------------------------------------------------------------

#[test]
fn normalize_logs_extracts_only_the_api_request_record() {
    let body = fixture("logs-api-request.json");
    let outcome = af_otlp::normalize_logs(&body);

    // The captured batch has 5 logRecords for this request/response turn
    // (api_request, assistant_response, hook_execution_start,
    // hook_execution_complete, mcp_server_connection) — only the
    // api_request one carries per-call token usage and should normalize.
    assert_eq!(
        outcome.events.len(),
        1,
        "expected exactly one llm_call envelope, got {:#?}",
        outcome.events
    );
    assert_eq!(outcome.dropped, 0);
}

#[test]
fn normalize_logs_maps_fields_from_the_real_capture() {
    let body = fixture("logs-api-request.json");
    let outcome = af_otlp::normalize_logs(&body);
    let envelope = &outcome.events[0];

    assert_eq!(envelope.schema_version, "0.1.0");
    assert_eq!(envelope.collector.name, "otlp-cc");
    assert_eq!(envelope.collector.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(envelope.session_id, "a238442e-16d9-4c00-8a66-635495f32251");
    assert_eq!(envelope.ts, "2026-07-25T17:44:55.85Z");
    assert!(envelope.attribution.is_none());

    let Payload::LlmCall(llm_call) = &envelope.payload else {
        panic!("expected LlmCall payload, got {:?}", envelope.payload);
    };
    assert_eq!(llm_call.provider, "anthropic");
    assert_eq!(llm_call.model_id_requested, "claude-haiku-4-5-20251001");
    assert_eq!(llm_call.model_id_served, None);
    assert_eq!(llm_call.usage.input_tokens, Some(10));
    assert_eq!(llm_call.usage.output_tokens, Some(246));
    assert_eq!(llm_call.usage.cached_read_tokens, Some(17536));
    assert_eq!(llm_call.usage.cached_write_tokens, Some(8740));
    assert_eq!(llm_call.usage.thought_tokens, None);
    assert_eq!(llm_call.usage_source, UsageSource::AgentTelemetry);
    assert_eq!(llm_call.duration_ms, Some(4038));

    // event_id: the fixture record carries a real `request_id` attribute
    // ("req_011CdPC5cXahJLuMWLHsz3mW"), so event_id should be derived
    // directly from it rather than the hash fallback, and stay
    // deterministic across re-parses of the same fixture.
    assert_eq!(envelope.event_id, "otlp-req_011CdPC5cXahJLuMWLHsz3mW");
    let outcome_again = af_otlp::normalize_logs(&body);
    assert_eq!(outcome_again.events[0].event_id, envelope.event_id);
}

#[test]
fn codex_native_otel_maps_usage_and_tool_result() {
    let body = fixture("logs-codex.json");
    let outcome = af_otlp::normalize_logs(&body);
    assert_eq!(outcome.events.len(), 3, "{:#?}", outcome.events);
    assert_eq!(outcome.dropped, 0);

    let session = outcome
        .events
        .iter()
        .find(|event| matches!(event.payload, Payload::SessionMeta(_)))
        .expect("Codex session_meta");
    let Payload::SessionMeta(meta) = &session.payload else {
        unreachable!()
    };
    assert_eq!(meta.agent_app.name, "codex");
    assert_eq!(meta.agent_app.version.as_deref(), Some("0.142.0"));

    let llm = outcome
        .events
        .iter()
        .find(|event| matches!(event.payload, Payload::LlmCall(_)))
        .expect("Codex llm_call");
    assert_eq!(llm.collector.name, "otlp-codex");
    assert_eq!(llm.session_id, "019f9eb8-515f-7be1-9bab-834da2e2e4ab");
    let Payload::LlmCall(call) = &llm.payload else {
        unreachable!()
    };
    assert_eq!(call.provider, "openai");
    assert_eq!(call.model_id_requested, "gpt-5.6-sol");
    assert_eq!(call.usage.input_tokens, Some(26_345));
    assert_eq!(call.usage.output_tokens, Some(92));
    assert_eq!(call.usage.cached_read_tokens, Some(1_024));
    assert_eq!(call.usage.thought_tokens, Some(19));
    assert_eq!(call.usage_source, UsageSource::AgentTelemetry);
    assert_eq!(call.status, Some(Status::Ok));

    let action = outcome
        .events
        .iter()
        .find(|event| matches!(event.payload, Payload::ActionSpan(_)))
        .expect("Codex action_span");
    let Payload::ActionSpan(span) = &action.payload else {
        unreachable!()
    };
    assert_eq!(span.span_id, "call_fixture_codex");
    assert_eq!(span.tool_name, "exec_command");
    assert_eq!(span.tool_kind, ToolKind::Bash);
    assert_eq!(span.execution_locus, ExecutionLocus::Local);
    assert_eq!(span.status, Some(Status::Ok));
    assert_eq!(span.pids, None);
    assert!(span.t_start < span.t_end);
}

#[test]
fn codex_duration_only_response_completed_is_not_double_counted() {
    let body = serde_json::json!({
        "resourceLogs": [{"scopeLogs": [{"logRecords": [{
            "timeUnixNano": "1785074310574000000",
            "attributes": [
                {"key": "event.name", "value": {"stringValue": "codex.sse_event"}},
                {"key": "event.kind", "value": {"stringValue": "response.completed"}},
                {"key": "duration_ms", "value": {"intValue": "38"}},
                {"key": "conversation.id", "value": {"stringValue": "session"}},
                {"key": "model", "value": {"stringValue": "gpt-test"}}
            ]
        }]}]}]
    });
    let outcome = af_otlp::normalize_logs(&body);
    assert!(outcome.events.is_empty());
    assert_eq!(outcome.dropped, 0);
}

#[test]
fn codex_accepts_numeric_strings_and_event_timestamp_fallback() {
    let body = serde_json::json!({
        "resourceLogs": [{"scopeLogs": [{"logRecords": [{
            "timeUnixNano": "0",
            "attributes": [
                {"key": "event.name", "value": {"stringValue": "codex.sse_event"}},
                {"key": "event.kind", "value": {"stringValue": "response.completed"}},
                {"key": "input_token_count", "value": {"stringValue": "23531"}},
                {"key": "output_token_count", "value": {"stringValue": "153"}},
                {"key": "cached_token_count", "value": {"intValue": "4480"}},
                {"key": "reasoning_token_count", "value": {"doubleValue": 74.0}},
                {"key": "event.timestamp", "value": {"stringValue": "2026-07-26T14:25:25.457Z"}},
                {"key": "conversation.id", "value": {"stringValue": "session"}},
                {"key": "model", "value": {"stringValue": "gpt-5.5"}}
            ]
        }]}]}]
    });
    let outcome = af_otlp::normalize_logs(&body);
    assert_eq!(outcome.events.len(), 1);
    assert_eq!(outcome.events[0].ts, "2026-07-26T14:25:25.457Z");
    let Payload::LlmCall(call) = &outcome.events[0].payload else {
        panic!("expected llm_call");
    };
    assert_eq!(call.usage.input_tokens, Some(23_531));
    assert_eq!(call.usage.output_tokens, Some(153));
    assert_eq!(call.usage.thought_tokens, Some(74));
    // No `conversation_starts` was ever seen for this conversation, so the
    // provider is honestly unknown rather than a hardcoded guess.
    assert_eq!(call.provider, "unknown");
}

#[test]
fn post_codex_logs_routes_to_codex_collector_spool() {
    let server = TestServer::start();
    let body = fixture("logs-codex.json");
    let (status, _) = post(
        server.addr(),
        "/v1/logs",
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(status, 200);
    let lines = spool_lines(
        server.spool_dir(),
        "otlp-codex.019f9eb8-515f-7be1-9bab-834da2e2e4ab.jsonl",
    );
    assert_eq!(lines.len(), 3);
    assert!(lines
        .iter()
        .all(|line| line.contains("\"name\":\"otlp-codex\"")));
}

#[test]
fn standard_gen_ai_record_normalizes_without_claude_specific_attributes() {
    let body = serde_json::json!({
        "resourceLogs": [{
            "resource": {"attributes": [
                {"key": "gen_ai.provider.name", "value": {"stringValue": "anthropic"}},
                {"key": "session.id", "value": {"stringValue": "standard-session"}}
            ]},
            "scopeLogs": [{
                "scope": {"name": "opentelemetry.instrumentation.genai"},
                "logRecords": [{
                    "timeUnixNano": "1785001495850000000",
                    "body": {"stringValue": "gen_ai.client.inference.operation.details"},
                    "attributes": [
                        {"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}},
                        {"key": "gen_ai.request.model", "value": {"stringValue": "claude-sonnet-4"}},
                        {"key": "gen_ai.response.model", "value": {"stringValue": "claude-sonnet-4-20250514"}},
                        {"key": "gen_ai.response.id", "value": {"stringValue": "msg_standard_1"}},
                        {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "120"}},
                        {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "45"}},
                        {"key": "gen_ai.request.stream", "value": {"boolValue": true}},
                        {"key": "server.address", "value": {"stringValue": "api.anthropic.com"}}
                    ]
                }]
            }]
        }]
    });

    let outcome = af_otlp::normalize_logs(&body);
    assert_eq!(outcome.dropped, 0);
    assert_eq!(outcome.unclaimed, 0);
    assert_eq!(outcome.events.len(), 1);
    let envelope = &outcome.events[0];
    assert_eq!(envelope.collector.name, "otlp-genai");
    assert_eq!(envelope.session_id, "standard-session");
    let Payload::LlmCall(call) = &envelope.payload else {
        panic!("expected llm_call");
    };
    assert_eq!(call.provider, "anthropic");
    assert_eq!(call.model_id_requested, "claude-sonnet-4");
    assert_eq!(
        call.model_id_served.as_deref(),
        Some("claude-sonnet-4-20250514")
    );
    assert_eq!(call.usage.input_tokens, Some(120));
    assert_eq!(call.usage.output_tokens, Some(45));
    assert_eq!(call.streaming, Some(true));
    assert_eq!(call.endpoint.as_deref(), Some("api.anthropic.com"));
}

#[test]
fn unrelated_otlp_records_are_unclaimed_not_dropped() {
    let body = logs_body(vec![serde_json::json!({
        "timeUnixNano": "1785001495850000000",
        "body": {"stringValue": "application.unrelated"}
    })]);
    let outcome = af_otlp::normalize_logs(&body);
    assert!(outcome.events.is_empty());
    assert_eq!(outcome.dropped, 0);
    assert_eq!(outcome.unclaimed, 1);
}

#[test]
fn installed_normalizers_declare_their_actual_lifecycle_fidelity() {
    let descriptors = af_otlp::installed_normalizers();
    assert_eq!(descriptors.len(), 3);
    assert!(descriptors.iter().all(|descriptor| {
        descriptor.signal == "logs" && descriptor.lifecycle == "completed_operations"
    }));
    assert!(descriptors.iter().any(|descriptor| {
        descriptor.id == "claude_code.api_request" && descriptor.emits == ["llm_call"]
    }));
    assert!(descriptors.iter().any(|descriptor| {
        descriptor.id == "otel.gen_ai.logs" && descriptor.emits == ["llm_call"]
    }));
    assert!(descriptors.iter().any(|descriptor| {
        descriptor.id == "codex.native_otel"
            && descriptor.emits == ["session_meta", "llm_call", "action_span"]
    }));
}

#[test]
fn normalize_logs_output_round_trips_through_parse_line() {
    let body = fixture("logs-api-request.json");
    let outcome = af_otlp::normalize_logs(&body);
    assert!(!outcome.events.is_empty());

    for envelope in &outcome.events {
        let line = serde_json::to_string(envelope).expect("envelope serializes");
        let parsed = af_events::parse_line(&line)
            .unwrap_or_else(|e| panic!("normalized envelope failed parse_line: {e}\n{line}"));
        assert_eq!(&parsed, envelope);
    }
}

#[test]
fn normalize_logs_on_unrelated_json_yields_nothing() {
    let body = serde_json::json!({"resourceLogs": []});
    let outcome = af_otlp::normalize_logs(&body);
    assert!(outcome.events.is_empty());
    assert_eq!(outcome.dropped, 0);

    let body = serde_json::json!({"not": "otlp shaped at all"});
    let outcome = af_otlp::normalize_logs(&body);
    assert!(outcome.events.is_empty());
    assert_eq!(outcome.dropped, 0);
}

// ---------------------------------------------------------------------
// event_id collision-proofing (review finding #1)
// ---------------------------------------------------------------------

/// Builds a synthetic OTLP `/v1/logs` body with the given `logRecords`, all
/// under one `resourceLogs[0].scopeLogs[0]` (matching the shape of a real
/// batch) so tests can control exactly which fields are present.
fn logs_body(records: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "resourceLogs": [{
            "scopeLogs": [{
                "logRecords": records
            }]
        }]
    })
}

fn api_request_record(request_id: Option<&str>) -> serde_json::Value {
    let mut attributes = vec![
        serde_json::json!({"key": "session.id", "value": {"stringValue": "sess-1"}}),
        serde_json::json!({"key": "model", "value": {"stringValue": "claude-haiku-4-5"}}),
        serde_json::json!({"key": "input_tokens", "value": {"intValue": 10}}),
        serde_json::json!({"key": "output_tokens", "value": {"intValue": 20}}),
    ];
    if let Some(request_id) = request_id {
        attributes
            .push(serde_json::json!({"key": "request_id", "value": {"stringValue": request_id}}));
    }

    serde_json::json!({
        "timeUnixNano": "1785001495850000000",
        "body": {"stringValue": "claude_code.api_request"},
        "attributes": attributes,
    })
}

/// An `api_request` record with a caller-chosen `session.id`, for the
/// tests that care what that value does to a filename.
fn api_request_record_for_session(session_id: &str) -> serde_json::Value {
    let mut record = api_request_record(Some("req-1"));
    record["attributes"][0]["value"]["stringValue"] = serde_json::json!(session_id);
    record
}

#[test]
fn two_records_with_different_request_ids_get_distinct_event_ids() {
    // Same timestamp, model, and tokens across both records (as seen in
    // real captures) — only request_id differs.
    let body = logs_body(vec![
        api_request_record(Some("req_AAAAAAAAAAAAAAAAAAAAAAAA")),
        api_request_record(Some("req_BBBBBBBBBBBBBBBBBBBBBBBB")),
    ]);

    let outcome = af_otlp::normalize_logs(&body);
    assert_eq!(outcome.events.len(), 2);
    assert_eq!(outcome.dropped, 0);
    assert_eq!(
        outcome.events[0].event_id,
        "otlp-req_AAAAAAAAAAAAAAAAAAAAAAAA"
    );
    assert_eq!(
        outcome.events[1].event_id,
        "otlp-req_BBBBBBBBBBBBBBBBBBBBBBBB"
    );
    assert_ne!(outcome.events[0].event_id, outcome.events[1].event_id);
}

#[test]
fn two_identical_records_without_request_id_still_get_distinct_event_ids_via_batch_index() {
    // Same timestamp, model, and tokens, and now request_id is stripped
    // too — every hashed field but the batch index is identical, so
    // without the index fix these would collide onto one event_id and one
    // of the two api_requests would get deduped away downstream.
    let body = logs_body(vec![api_request_record(None), api_request_record(None)]);

    let outcome = af_otlp::normalize_logs(&body);
    assert_eq!(outcome.events.len(), 2);
    assert_eq!(outcome.dropped, 0);
    assert_ne!(
        outcome.events[0].event_id, outcome.events[1].event_id,
        "identical records without request_id must still get distinct event_ids \
         via the batch-index hash fallback"
    );
}

// ---------------------------------------------------------------------
// record-level timeUnixNano dual tolerance + dropped-record accounting
// (review finding #2)
// ---------------------------------------------------------------------

#[test]
fn normalize_logs_accepts_time_unix_nano_as_a_json_number_not_just_a_string() {
    let mut record = api_request_record(Some("req_CCCCCCCCCCCCCCCCCCCCCCCC"));
    record["timeUnixNano"] = serde_json::json!(1785001495850000000i64);
    let body = logs_body(vec![record]);

    let outcome = af_otlp::normalize_logs(&body);
    assert_eq!(
        outcome.events.len(),
        1,
        "a JSON-number timeUnixNano must still normalize, not be silently dropped"
    );
    assert_eq!(outcome.dropped, 0);
}

#[test]
fn api_request_record_missing_time_unix_nano_is_counted_as_dropped_not_silently_skipped() {
    let mut record = api_request_record(Some("req_DDDDDDDDDDDDDDDDDDDDDDDD"));
    record.as_object_mut().unwrap().remove("timeUnixNano");
    let body = logs_body(vec![record]);

    let outcome = af_otlp::normalize_logs(&body);
    assert!(outcome.events.is_empty());
    assert_eq!(outcome.dropped, 1);
}

// ---------------------------------------------------------------------
// full HTTP path
// ---------------------------------------------------------------------

/// A running receiver plus the tempdir its spool lives in, torn down
/// together when the guard drops.
///
/// The three lines every HTTP test opened with (make a tempdir, join
/// `spool`, `serve` on port 0) and the `handle.shutdown()` every one of
/// them had to remember to end with are the same three lines and the same
/// obligation each time — and an early `panic!` from a failed assertion
/// skipped the shutdown entirely, leaking a bound port and a server thread
/// for the rest of the run. Binding the teardown to a scope instead means
/// [`af_otlp::ServerHandle`]'s own `Drop` runs whether the test passes or
/// fails, which is the case that actually needed covering.
///
/// **Field order is load-bearing**: Rust drops fields in declaration order,
/// so `handle` gets its shutdown grace period before `_tmp` removes the
/// directory. Tests with a deliberately stalled reader keep the client alive
/// only until shutdown returns, then release it before the tempdir is dropped.
struct TestServer {
    handle: af_otlp::ServerHandle,
    /// Held so the temp directory outlives the server that writes into it.
    _tmp: tempfile::TempDir,
    spool_dir: PathBuf,
}

impl TestServer {
    fn start() -> TestServer {
        Self::start_at(|root| root.join("spool"))
    }

    /// As [`TestServer::start`], but with the spool directory chosen by
    /// `layout` — the traversal test needs a *nested* spool so "escaped one
    /// level up" is distinguishable from "stayed put".
    fn start_at(layout: impl FnOnce(&Path) -> PathBuf) -> TestServer {
        let tmp = tempfile::tempdir().unwrap();
        let spool_dir = layout(tmp.path());
        let handle = af_otlp::serve("127.0.0.1:0".parse().unwrap(), spool_dir.clone())
            .expect("server starts");
        TestServer {
            handle,
            _tmp: tmp,
            spool_dir,
        }
    }

    fn with_spool_path_blocked() -> TestServer {
        let tmp = tempfile::tempdir().unwrap();
        let spool_dir = tmp.path().join("spool");
        fs::write(&spool_dir, b"not a directory").unwrap();
        let handle = af_otlp::serve("127.0.0.1:0".parse().unwrap(), spool_dir.clone())
            .expect("server starts");
        TestServer {
            handle,
            _tmp: tmp,
            spool_dir,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.handle.addr()
    }

    fn spool_dir(&self) -> &Path {
        &self.spool_dir
    }

    /// The `rejected/` directory the receiver quarantines into: the spool
    /// directory's *sibling*, not a child of it.
    fn rejected_dir(&self) -> PathBuf {
        self.spool_dir
            .parent()
            .expect("spool dir has a parent")
            .join("rejected")
    }
}

/// Minimal blocking HTTP/1.1 client: POSTs `body` to `path` on `addr` and
/// returns `(status_code, response_body)`. Deliberately not pulling in an
/// HTTP client crate for this one loopback request in tests.
fn post(addr: SocketAddr, path: &str, body: &[u8]) -> (u16, String) {
    post_with_host(addr, path, body, "127.0.0.1")
}

/// As [`post`], but with the `Host` header under the caller's control —
/// the receiver's rebinding guard is decided from it.
fn post_with_host(addr: SocketAddr, path: &str, body: &[u8], host: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write request head");
    stream.write_all(body).expect("write request body");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");

    let status_line = response.lines().next().unwrap_or_default();
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("couldn't parse status from: {status_line}"));

    let response_body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();

    (status_code, response_body)
}

fn start_stalled_post(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect stalled client");
    stream
        .write_all(
            b"POST /v1/logs HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n{",
        )
        .expect("write incomplete request");
    stream
}

fn spool_lines(spool_dir: &Path, file_name: &str) -> Vec<String> {
    let path = spool_dir.join(file_name);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read spool file {path:?}: {e}"))
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn post_logs_writes_a_valid_spool_line() {
    let server = TestServer::start();

    let body = fixture("logs-api-request.json");
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let (status, response_body) = post(server.addr(), "/v1/logs", &body_bytes);
    assert_eq!(status, 200);
    assert!(response_body.contains("partialSuccess"));

    let lines = spool_lines(
        server.spool_dir(),
        "otlp-cc.a238442e-16d9-4c00-8a66-635495f32251.jsonl",
    );
    assert_eq!(lines.len(), 1, "expected exactly one spooled line");
    af_events::parse_line(&lines[0]).expect("spooled line parses");
}

#[test]
fn persistence_failure_is_retryable_and_does_not_increment_accepted() {
    let server = TestServer::with_spool_path_blocked();
    let counters = server.handle.counters();
    let body = serde_json::to_vec(&fixture("logs-api-request.json")).unwrap();

    let (status, response_body) = post(server.addr(), "/v1/logs", &body);

    assert_eq!(status, 503);
    let diagnostic: serde_json::Value = serde_json::from_str(&response_body).unwrap();
    assert_eq!(diagnostic["error"]["code"], "persistence_failed");
    assert_eq!(diagnostic["error"]["retryable"], true);
    assert_eq!(counters.logs_requests(), 1);
    assert_eq!(counters.logs_accepted(), 0);
    assert_eq!(counters.logs_persistence_failures(), 1);
    assert_eq!(counters.logs_persistence_failed_events(), 1);
}

#[test]
fn one_stalled_body_does_not_block_other_ingestion() {
    let server = TestServer::start();
    let stalled = start_stalled_post(server.addr());
    thread::sleep(Duration::from_millis(50));

    let body = serde_json::to_vec(&fixture("logs-api-request.json")).unwrap();
    let started = Instant::now();
    let (status, _) = post(server.addr(), "/v1/logs", &body);

    assert_eq!(status, 200);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a stalled client serialized otherwise independent ingestion"
    );
    drop(stalled);
}

#[test]
fn shutdown_is_bounded_when_a_client_stalls_mid_body() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = af_otlp::serve("127.0.0.1:0".parse().unwrap(), tmp.path().join("spool"))
        .expect("server starts");
    let stalled = start_stalled_post(handle.addr());
    thread::sleep(Duration::from_millis(50));

    let started = Instant::now();
    handle.shutdown();

    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutdown waited indefinitely for a stalled body reader"
    );
    drop(stalled);
}

#[test]
fn post_malformed_logs_body_returns_200_and_quarantines_without_corrupting_the_spool() {
    let server = TestServer::start();

    // First establish one legitimate spool file so we can assert it's
    // untouched by the subsequent bad request.
    let good_body = fixture("logs-api-request.json");
    let good_bytes = serde_json::to_vec(&good_body).unwrap();
    let (status, _) = post(server.addr(), "/v1/logs", &good_bytes);
    assert_eq!(status, 200);
    let file_name = "otlp-cc.a238442e-16d9-4c00-8a66-635495f32251.jsonl";
    let lines_before = spool_lines(server.spool_dir(), file_name);
    assert_eq!(lines_before.len(), 1);

    let (status, _) = post(server.addr(), "/v1/logs", b"this is not json {{{");
    assert_eq!(status, 200, "the agent's exporter must never see a non-2xx");

    // The good spool file must be unchanged (no corruption, no spurious
    // append).
    let lines_after = spool_lines(server.spool_dir(), file_name);
    assert_eq!(lines_after, lines_before);

    // The malformed body must have been quarantined under the spool
    // dir's sibling `rejected/`.
    let rejected_files: Vec<_> = fs::read_dir(server.rejected_dir())
        .unwrap_or_else(|e| panic!("expected rejected dir to exist: {e}"))
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(rejected_files.len(), 1, "expected one quarantined file");
    let quarantined = fs::read_to_string(&rejected_files[0]).unwrap();
    assert_eq!(quarantined, "this is not json {{{");
}

#[test]
fn post_logs_with_unmappable_api_request_record_drops_it_quarantines_and_still_200s() {
    let server = TestServer::start();

    // Well-formed JSON, and the record is identified as claude_code.api_request
    // by its body, but it's missing timeUnixNano — normalize_record can't map
    // it. This must be counted/quarantined as a drop, not vanish silently.
    let mut record = api_request_record(Some("req_EEEEEEEEEEEEEEEEEEEEEEEE"));
    record.as_object_mut().unwrap().remove("timeUnixNano");
    let body = logs_body(vec![record]);
    let body_bytes = serde_json::to_vec(&body).unwrap();

    let (status, response_body) = post(server.addr(), "/v1/logs", &body_bytes);
    assert_eq!(status, 200, "the agent's exporter must never see a non-2xx");
    assert!(response_body.contains("partialSuccess"));

    assert!(
        !server.spool_dir().exists() || fs::read_dir(server.spool_dir()).unwrap().next().is_none(),
        "the unmappable record must not produce a spool line"
    );

    let dropped_files: Vec<_> = fs::read_dir(server.rejected_dir())
        .unwrap_or_else(|e| panic!("expected rejected dir to exist: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("otlp-cc.dropped."))
        })
        .collect();
    assert_eq!(
        dropped_files.len(),
        1,
        "expected one otlp-cc.dropped.* quarantine file"
    );
}

#[test]
fn post_metrics_returns_200_and_writes_nothing() {
    let server = TestServer::start();

    let metrics_body = fixture("metrics-sample.json");
    let metrics_bytes = serde_json::to_vec(&metrics_body).unwrap();
    let (status, _) = post(server.addr(), "/v1/metrics", &metrics_bytes);
    assert_eq!(status, 200);

    assert!(
        !server.spool_dir().exists() || fs::read_dir(server.spool_dir()).unwrap().next().is_none(),
        "expected /v1/metrics to write nothing to the spool"
    );
    assert!(
        !server.rejected_dir().exists(),
        "expected /v1/metrics to never quarantine anything"
    );
}

#[test]
fn unknown_path_404s() {
    let server = TestServer::start();
    let (status, _) = post(server.addr(), "/not-otlp", b"{}");
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------
// hardening: this is an unauthenticated port, and everything it reads is
// chosen by whoever connected to it
// ---------------------------------------------------------------------

/// A page on the open web can point a `fetch` at `127.0.0.1:4318` after
/// resolving its own hostname there. What it cannot do is forge `Host`.
#[test]
fn a_post_with_a_foreign_host_is_refused() {
    let server = TestServer::start();

    let body = serde_json::to_vec(&fixture("logs-api-request.json")).unwrap();
    let (status, _) = post_with_host(server.addr(), "/v1/logs", &body, "evil.example");
    assert_eq!(status, 403);
    assert!(
        !server.spool_dir().exists(),
        "a refused request must not reach the spool"
    );
}

/// The forms a real agent exporter sends must keep working — Claude Code's
/// is `Host: 127.0.0.1:4318`. Breaking this silently stops all `llm_call`
/// telemetry, which is worse than the attack it guards against.
#[test]
fn the_host_forms_a_real_exporter_sends_are_accepted() {
    let server = TestServer::start();

    let port = server.addr().port();
    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        "127.0.0.1".to_string(),
        format!("[::1]:{port}"),
    ] {
        let (status, _) = post_with_host(server.addr(), "/v1/metrics", b"{}", &host);
        assert_eq!(status, 200, "Host: {host} must be accepted");
    }
}

/// Without a cap the receiver buffers whatever a client streams at it.
#[test]
fn an_oversized_body_is_refused_rather_than_buffered() {
    let server = TestServer::start();

    // Just over the 4 MiB cap, and valid JSON, so the only thing that can
    // reject it is the cap.
    let filler = "x".repeat(4 * 1024 * 1024 + 16);
    let body = format!(r#"{{"pad":"{filler}"}}"#);
    let (status, _) = post(server.addr(), "/v1/logs", body.as_bytes());
    assert_eq!(status, 413);

    // …and a normal-sized batch still goes through on the same server.
    let ok_body = serde_json::to_vec(&fixture("logs-api-request.json")).unwrap();
    let (status, _) = post(server.addr(), "/v1/logs", &ok_body);
    assert_eq!(status, 200);
}

/// `session.id` comes off the wire and lands in a filename. Unsanitized,
/// this body appends JSON outside the spool directory entirely.
#[test]
fn a_traversal_attempt_in_session_id_cannot_escape_the_spool_directory() {
    // Nested, so an escape of one or two levels lands somewhere the
    // assertions below can see.
    let mut root = PathBuf::new();
    let server = TestServer::start_at(|tmp| {
        root = tmp.to_path_buf();
        tmp.join("nested").join("spool")
    });

    let attack = "../../../../pwned";
    let body = logs_body(vec![api_request_record_for_session(attack)]);
    let (status, _) = post(
        server.addr(),
        "/v1/logs",
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(status, 200);

    // Nothing escaped: not into the tempdir root, not anywhere but the
    // spool directory itself.
    assert!(
        !root.join("pwned").exists() && !root.join("pwned.jsonl").exists(),
        "the traversal escaped the spool directory"
    );

    let written: Vec<String> = fs::read_dir(server.spool_dir())
        .expect("spool dir exists")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let expected = format!("otlp-cc.{}.jsonl", af_otlp::sanitize_id(attack));
    assert_eq!(
        written,
        vec![expected.clone()],
        "the id must be reduced to one safe path component"
    );

    // The envelope's own session_id must match the file it landed in, or
    // the join would see two sessions where there is one.
    let lines = spool_lines(server.spool_dir(), &expected);
    let envelope = af_events::parse_line(&lines[0]).expect("spooled line parses");
    assert_eq!(envelope.session_id, af_otlp::sanitize_id(attack));
    assert!(!envelope.session_id.contains('/'));
}

/// `request_id` is whatever the exporter sent. Anthropic's are ~30 chars,
/// but a short one used to produce an `event_id` below the schema's
/// `minLength: 16` — which is a reject at ingest, i.e. telemetry silently
/// lost between the receiver and the store.
#[test]
fn a_short_request_id_still_yields_a_schema_valid_event_id() {
    let body = logs_body(vec![api_request_record(Some("r1"))]);
    let outcome = af_otlp::normalize_logs(&body);
    assert_eq!(outcome.dropped, 0);

    let line = serde_json::to_string(&outcome.events[0]).expect("serializes");
    af_events::parse_line(&line).expect("a short request_id must not produce a rejectable event");
}

/// …and the extension must stay injective: two distinct short request ids
/// collapsing onto one `event_id` would have the store dedup one away.
#[test]
fn two_short_request_ids_do_not_collide() {
    let body = logs_body(vec![
        api_request_record(Some("r1")),
        api_request_record(Some("r2")),
    ]);
    let outcome = af_otlp::normalize_logs(&body);

    assert_eq!(outcome.events.len(), 2);
    assert_ne!(outcome.events[0].event_id, outcome.events[1].event_id);
}
