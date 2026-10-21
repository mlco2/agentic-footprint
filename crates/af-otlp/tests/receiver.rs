//! Tests against the captured (real, sanitized) OTLP fixture:
//! - `normalize_logs` in isolation, asserting exact token/model/session
//!   mapping and that the output round-trips through `af_events::parse_line`.
//! - the full HTTP path via `af_otlp::serve` into a channel, including the
//!   malformed-body, reject and `/v1/metrics` cases.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use af_events::{ExecutionLocus, Payload, Status, ToolKind, UsageSource};
use af_otlp::Inbound;
use std::sync::mpsc::{self, Receiver};

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
fn post_codex_logs_routes_to_the_codex_collector() {
    let server = TestServer::start();
    let body = fixture("logs-codex.json");
    let (status, _) = post(
        server.addr(),
        "/v1/logs",
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(status, 200);
    let (facts, _) = server.drain(Duration::from_millis(300));
    assert_eq!(facts.len(), 3);
    assert!(facts
        .iter()
        .all(|event| event.collector.name == "otlp-codex"
            && event.session_id == "019f9eb8-515f-7be1-9bab-834da2e2e4ab"));
}

/// `af setup` and `af service status` decide "our receiver is up" from
/// this header: a bare `200` is what any OTLP collector on the port says.
#[test]
fn every_response_identifies_the_af_receiver_and_its_version() {
    let server = TestServer::start();
    for path in ["/v1/logs", "/not-a-route"] {
        let mut stream = TcpStream::connect(server.addr()).expect("connect");
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        )
        .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        let expected = format!(
            "{}: {}",
            af_otlp::RECEIVER_HEADER.to_ascii_lowercase(),
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            response
                .lines()
                .any(|line| line.trim().to_ascii_lowercase() == expected),
            "{path}: no `{expected}` header in:\n{response}"
        );
    }
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

/// A running receiver plus the channel it forwards to. The handle's own
/// `Drop` shuts the server down whether the test passes or fails.
struct TestServer {
    handle: af_otlp::ServerHandle,
    inbox: Receiver<Inbound>,
}

impl TestServer {
    fn start() -> TestServer {
        Self::with_capacity(64)
    }

    /// A rendezvous channel nobody reads: every `try_send` finds it full.
    fn with_full_inbox() -> TestServer {
        Self::with_capacity(0)
    }

    fn with_capacity(capacity: usize) -> TestServer {
        let (sink, inbox) = mpsc::sync_channel(capacity);
        let handle = af_otlp::serve("127.0.0.1:0".parse().unwrap(), sink).expect("server starts");
        TestServer { handle, inbox }
    }

    fn addr(&self) -> SocketAddr {
        self.handle.addr()
    }

    /// Everything forwarded within `wait`, split into facts and rejects.
    fn drain(&self, wait: Duration) -> (Vec<af_events::Envelope>, Vec<af_otlp::Reject>) {
        let deadline = Instant::now() + wait;
        let (mut facts, mut rejects) = (Vec::new(), Vec::new());
        while let Ok(item) = self
            .inbox
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            match item {
                Inbound::Facts(events) => facts.extend(events),
                Inbound::Reject(reject) => rejects.push(reject),
            }
        }
        (facts, rejects)
    }

    fn no_facts_within(&self, wait: Duration) -> bool {
        self.drain(wait).0.is_empty()
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
    post_with_headers(
        addr,
        path,
        body,
        &[("Host", host), ("Content-Type", "application/json")],
    )
}

/// POSTs with exactly `headers` (plus Content-Length/Connection).
fn post_with_headers(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    let mut head = format!("POST {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    stream
        .write_all(head.as_bytes())
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
            b"POST /v1/logs HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n{",
        )
        .expect("write incomplete request");
    stream
}

#[test]
fn post_logs_forwards_the_normalized_event() {
    let server = TestServer::start();
    let (status, response_body) = post(server.addr(), "/v1/logs", &api_request_body());
    assert_eq!(status, 200);
    assert!(response_body.contains("partialSuccess"));
    let (facts, rejects) = server.drain(Duration::from_millis(300));
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].session_id, "a238442e-16d9-4c00-8a66-635495f32251");
    af_events::parse_line(&serde_json::to_string(&facts[0]).unwrap())
        .expect("a forwarded event is a valid Contract #1 line");
    assert!(rejects.is_empty());
}

#[test]
fn a_full_inbox_is_retryable_and_does_not_increment_accepted() {
    let server = TestServer::with_full_inbox();
    let counters = server.handle.counters();
    let started = Instant::now();
    let (status, response_body) = post(server.addr(), "/v1/logs", &api_request_body());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a full inbox must not block"
    );
    assert_eq!(status, 503);
    let diagnostic: serde_json::Value = serde_json::from_str(&response_body).unwrap();
    assert_eq!(diagnostic["error"]["code"], "receiver_overloaded");
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
    let (sink, _inbox) = mpsc::sync_channel(1);
    let handle = af_otlp::serve("127.0.0.1:0".parse().unwrap(), sink).expect("server starts");
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
fn a_malformed_body_is_rejected_by_size_and_never_stored() {
    let server = TestServer::start();
    let (status, _) = post(server.addr(), "/v1/logs", b"this is not json {{{");
    assert_eq!(status, 200, "the agent's exporter must never see a non-2xx");
    let (facts, rejects) = server.drain(Duration::from_millis(300));
    assert!(facts.is_empty());
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].bytes, 20);
    assert!(rejects[0].attribute_keys.is_empty());
    assert!(
        !format!("{:?}", rejects[0]).contains("not json"),
        "no payload in a reject"
    );
}

#[test]
fn an_unmappable_record_is_rejected_with_its_attribute_names_only() {
    let server = TestServer::start();
    let mut record = api_request_record(Some("req_EEEEEEEEEEEEEEEEEEEEEEEE"));
    record.as_object_mut().unwrap().remove("timeUnixNano");
    let body = logs_body(vec![record]);
    let (status, response_body) = post(
        server.addr(),
        "/v1/logs",
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(status, 200, "the agent's exporter must never see a non-2xx");
    assert!(response_body.contains("partialSuccess"));
    let (facts, rejects) = server.drain(Duration::from_millis(300));
    assert!(facts.is_empty());
    assert_eq!(rejects.len(), 1);
    assert!(rejects[0].attribute_keys.iter().any(|k| k == "request_id"));
    assert!(
        !format!("{:?}", rejects[0]).contains("req_E"),
        "names only, never values"
    );
}

#[test]
fn a_body_over_the_record_cap_is_refused_and_rejected() {
    let server = TestServer::start();
    let records: Vec<_> = (0..=af_otlp::MAX_LOG_RECORDS)
        .map(|_| serde_json::json!({}))
        .collect();
    let body = serde_json::json!({"resourceLogs": [{"scopeLogs": [{"logRecords": records}]}]});
    let (status, _) = post(
        server.addr(),
        "/v1/logs",
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(status, 413);
    let (facts, rejects) = server.drain(Duration::from_millis(300));
    assert!(facts.is_empty());
    assert_eq!(rejects.len(), 1);
}

#[test]
fn post_metrics_returns_200_and_writes_nothing() {
    let server = TestServer::start();

    let metrics_body = fixture("metrics-sample.json");
    let metrics_bytes = serde_json::to_vec(&metrics_body).unwrap();
    let (status, _) = post(server.addr(), "/v1/metrics", &metrics_bytes);
    assert_eq!(status, 200);

    let (facts, rejects) = server.drain(Duration::from_millis(200));
    assert!(
        facts.is_empty() && rejects.is_empty(),
        "/v1/metrics forwards nothing"
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
        server.no_facts_within(Duration::from_millis(200)),
        "a refused request must not reach the store"
    );
}

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

/// `session.id` comes off the wire and is used by the store and, for the
/// collectors that still spool, in a filename: it must be reduced to one
/// safe path component before it leaves the receiver.
#[test]
fn a_traversal_attempt_in_session_id_is_sanitized_before_forwarding() {
    let server = TestServer::start();
    let attack = "../../../../pwned";
    let body = logs_body(vec![api_request_record_for_session(attack)]);
    let (status, _) = post(
        server.addr(),
        "/v1/logs",
        &serde_json::to_vec(&body).unwrap(),
    );
    assert_eq!(status, 200);
    let (facts, _) = server.drain(Duration::from_millis(300));
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].session_id, af_otlp::sanitize_id(attack));
    assert!(!facts[0].session_id.contains('/'));
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

#[test]
fn dropped_records_report_their_attribute_names_only() {
    // Same mutation the unmappable-record HTTP test uses: an api_request
    // record without timeUnixNano is claimed but cannot be mapped.
    let mut record = api_request_record(Some("req_EEEEEEEEEEEEEEEEEEEEEEEE"));
    record.as_object_mut().unwrap().remove("timeUnixNano");
    let outcome = af_otlp::normalize_logs(&logs_body(vec![record]));
    assert_eq!(outcome.dropped, 1);
    assert_eq!(
        outcome.dropped_keys,
        vec![
            "input_tokens",
            "model",
            "output_tokens",
            "request_id",
            "session.id"
        ],
        "the dropped record's attribute names, sorted"
    );
    assert!(
        outcome.dropped_keys.iter().all(|k| !k.contains("req_")),
        "names only, no values"
    );
}

fn api_request_body() -> Vec<u8> {
    serde_json::to_vec(&fixture("logs-api-request.json")).unwrap()
}

#[test]
fn a_request_from_a_web_page_is_refused() {
    let server = TestServer::start();
    let json = ("Content-Type", "application/json");
    let host = ("Host", "127.0.0.1");
    for headers in [
        vec![host, json, ("Origin", "https://evil.example")],
        vec![host, json, ("Origin", "null")],
        vec![host, json, ("Sec-Fetch-Site", "cross-site")],
        vec![host, json, ("Sec-Fetch-Site", "same-site")],
    ] {
        let (status, _) =
            post_with_headers(server.addr(), "/v1/logs", &api_request_body(), &headers);
        assert_eq!(status, 403, "{headers:?}");
    }
    assert!(server.no_facts_within(Duration::from_millis(200)));
}

#[test]
fn a_non_json_content_type_is_refused() {
    let server = TestServer::start();
    for content_type in [None, Some("text/plain"), Some("application/x-protobuf")] {
        let mut headers = vec![("Host", "127.0.0.1")];
        if let Some(value) = content_type {
            headers.push(("Content-Type", value));
        }
        let (status, _) =
            post_with_headers(server.addr(), "/v1/logs", &api_request_body(), &headers);
        assert_eq!(status, 415, "{content_type:?}");
    }
    assert!(server.no_facts_within(Duration::from_millis(200)));
}

#[test]
fn json_content_types_real_exporters_send_are_accepted() {
    let server = TestServer::start();
    for content_type in [
        "application/json",
        "application/json; charset=utf-8",
        "Application/JSON",
    ] {
        let headers = [
            ("Host", "127.0.0.1"),
            ("Content-Type", content_type),
            ("Sec-Fetch-Site", "none"),
        ];
        let (status, _) =
            post_with_headers(server.addr(), "/v1/logs", &api_request_body(), &headers);
        assert_eq!(status, 200, "{content_type}");
    }
}

#[test]
fn reject_metadata_stays_small_whatever_the_attribute_names() {
    let mut record = api_request_record(Some("req_FFFFFFFFFFFFFFFFFFFFFFFF"));
    record.as_object_mut().unwrap().remove("timeUnixNano");
    let attributes = record["attributes"].as_array_mut().unwrap();
    for i in 0..500 {
        attributes.push(serde_json::json!({
            "key": format!("{i:04}{}", "k".repeat(10_000)),
            "value": {"stringValue": "x"}
        }));
    }
    for i in 0..500 {
        attributes
            .push(serde_json::json!({"key": format!("short.{i}"), "value": {"stringValue": "x"}}));
    }
    let outcome = af_otlp::normalize_logs(&logs_body(vec![record]));
    assert_eq!(outcome.dropped, 1);
    assert!(outcome.dropped_keys.len() <= af_otlp::MAX_REJECT_KEYS);
    assert!(outcome
        .dropped_keys
        .iter()
        .all(|key| key.len() <= af_otlp::MAX_ATTRIBUTE_KEY_BYTES));
}

#[test]
fn the_receiver_refuses_to_listen_beyond_loopback() {
    let (tx, _rx) = mpsc::sync_channel(1);
    for addr in ["0.0.0.0:0", "[::]:0"] {
        let error = af_otlp::serve(addr.parse().unwrap(), tx.clone())
            .err()
            .unwrap_or_else(|| panic!("{addr} was accepted"));
        assert!(error.to_string().contains("loopback"), "{error:#}");
    }
    let handle = af_otlp::serve("127.0.0.1:0".parse().unwrap(), tx).expect("loopback is fine");
    handle.shutdown();
}
