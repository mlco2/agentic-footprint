//! Round-trip tests: construct one sample of each payload type, serialize it,
//! validate the resulting JSON against the authoritative JSON Schema, then
//! deserialize and assert equality with the original value.

use af_events::fixtures;
use af_events::validate::events_validator;
use af_events::{
    ActionSpan, AgentApp, Attribution, EnergyComponent, EnergyKind, EnergyMethod, EnergySample,
    Envelope, ExecutionLocus, Hardware, LlmCall, Payload, PowerSource, ProcessDelta, SessionMeta,
    Status, ToolKind, Usage,
};

#[test]
fn unknown_type_with_valid_base_envelope_is_preserved_opaquely() {
    let line = r#"{"schema_version":"0.1.0","event_id":"opaque-event-0001","ts":"2026-07-25T00:00:00Z","collector":{"name":"future","version":"1.0.0"},"session_id":"sess-opaque","type":"future_fact","payload":{"new_field":42}}"#;
    let parsed = af_events::parse_line_preserving_unknown(line).expect("base envelope valid");
    let af_events::ParsedLine::Opaque(event) = parsed else {
        panic!("unknown type must be opaque");
    };
    assert_eq!(event.type_tag, "future_fact");
    assert_eq!(event.session_id, "sess-opaque");
    assert_eq!(event.json["payload"]["new_field"], 42);
    assert!(
        af_events::parse_line(line).is_err(),
        "typed API stays strict"
    );
}
use serde_json::Value;

const SESSION: &str = "session-123";
const TS: &str = "2026-07-25T12:00:00Z";

/// An envelope carrying `payload`, with attribution set — the maximal
/// envelope, so the optional block is exercised by default and the one
/// test that cares about its absence clears it explicitly.
fn base_envelope(payload: Payload) -> Envelope {
    let mut envelope = fixtures::envelope(
        &fixtures::event_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        SESSION,
        TS,
        payload,
    );
    envelope.attribution = Some(Attribution {
        agent_id: Some("main".to_string()),
        subagent_id: None,
        task_id: Some("task-1".to_string()),
        tool_call_id: None,
    });
    envelope
}

/// Serialize, validate against the real schema, deserialize, and assert
/// round-trip equality.
///
/// Validation goes through [`events_validator`] — the *same* compiled
/// validator `parse_line` uses — rather than a locally compiled one. A
/// test-local `jsonschema::validator_for(...)` leaves
/// `should_validate_formats` at its draft 2020-12 default of *off*, so it
/// silently ignores every `format: date-time` in the contract: it was a
/// strictly weaker gate than production, which is the one thing a
/// contract test must never be.
fn check_roundtrip(envelope: Envelope) {
    let value = serde_json::to_value(&envelope).expect("envelope serializes");
    if let Err(err) = events_validator().validate(&value) {
        panic!("schema validation failed: {err}\ninstance: {value:#}");
    }

    let line = serde_json::to_string(&envelope).expect("envelope serializes to string");
    let parsed = af_events::parse_line(&line).expect("parse_line succeeds on well-formed line");
    assert_eq!(parsed, envelope);

    // The tag the store indexes on must be the tag that was serialized;
    // `type_tag` reads it off the enum instead, so the two can drift.
    assert_eq!(
        value["type"],
        Value::String(envelope.type_tag().to_string())
    );
}

#[test]
fn llm_call_roundtrips_and_validates() {
    let payload = Payload::LlmCall(LlmCall {
        model_id_served: Some("claude-sonnet-5-20260115".to_string()),
        endpoint: Some("https://api.anthropic.com".to_string()),
        usage: Usage {
            input_tokens: Some(1000),
            output_tokens: Some(250),
            thought_tokens: Some(40),
            cached_read_tokens: Some(500),
            cached_write_tokens: Some(0),
        },
        duration_ms: Some(1234),
        status: Some(Status::Ok),
        streaming: Some(false),
        ..fixtures::llm_call()
    });
    check_roundtrip(base_envelope(payload));
}

#[test]
fn energy_sample_roundtrips_and_validates() {
    // Spelled out rather than built from `fixtures::energy_sample`, which
    // is deliberately a single measured `total`: the case worth covering
    // here is the *other* shape — several subsystem components, no total,
    // and a modeled one alongside a measured one.
    let payload = Payload::EnergySample(EnergySample {
        t_start: "2026-07-25T12:00:00Z".to_string(),
        t_end: "2026-07-25T12:00:10Z".to_string(),
        components: vec![
            EnergyComponent {
                kind: EnergyKind::Cpu,
                label: Some("Apple M-series package".to_string()),
                energy_j: 12.5,
                method: EnergyMethod::Powermetrics,
            },
            EnergyComponent {
                kind: EnergyKind::Gpu,
                label: None,
                energy_j: 3.25,
                method: EnergyMethod::TdpModel,
            },
        ],
        host_id: Some("host-hash-abc123".to_string()),
    });
    check_roundtrip(base_envelope(payload));
}

#[test]
fn action_span_roundtrips_and_validates() {
    let payload = Payload::ActionSpan(ActionSpan {
        tool_kind: ToolKind::Bash,
        execution_locus: ExecutionLocus::Local,
        pids: Some(vec![1234, 1235]),
        cgroup: Some("/user.slice/af.slice".to_string()),
        status: Some(Status::Ok),
        ..fixtures::action_span(
            "span-abc123",
            "2026-07-25T12:00:00Z",
            "2026-07-25T12:00:02Z",
        )
    });
    check_roundtrip(base_envelope(payload));
}

#[test]
fn process_sample_roundtrips_and_validates() {
    // `fixtures::process_sample` builds the rows; the extra per-row fields
    // (`memory_rss_bytes`, the io counters) are set here because this test
    // exists to prove they survive the round trip.
    let mut payload = fixtures::process_sample(
        "2026-07-25T12:00:00Z",
        "2026-07-25T12:00:05Z",
        &[
            (4242, 800, None),
            // The sampler's orphan-tail extension: same tree, still burning
            // CPU after its span closed.
            (4243, 120, Some("span-abc123")),
        ],
    );
    payload.processes[0] = ProcessDelta {
        memory_rss_bytes: Some(104_857_600),
        io_read_bytes: Some(4096),
        io_write_bytes: Some(0),
        ..payload.processes[0].clone()
    };
    let payload = Payload::ProcessSample(payload);
    check_roundtrip(base_envelope(payload));
}

#[test]
fn session_meta_roundtrips_and_validates() {
    let payload = Payload::SessionMeta(SessionMeta {
        agent_app: AgentApp {
            name: "claude-code".to_string(),
            version: Some("1.2.3".to_string()),
        },
        os: Some("darwin-25.3.0".to_string()),
        hardware: Some(Hardware {
            cpu_model: Some("Apple M4 Max".to_string()),
            gpu_models: Some(vec!["Apple M4 Max GPU".to_string()]),
            ram_gb: Some(64.0),
        }),
        geo_zone: Some("FRA".to_string()),
        power_source: Some(PowerSource::Ac),
    });
    check_roundtrip(base_envelope(payload));
}

#[test]
fn minimal_envelope_without_attribution_roundtrips() {
    let mut envelope = base_envelope(Payload::SessionMeta(SessionMeta {
        agent_app: AgentApp {
            name: "codex-cli".to_string(),
            version: None,
        },
        ..fixtures::session_meta()
    }));
    envelope.attribution = None;
    check_roundtrip(envelope);
}

// --- Adversarial tests: parse_line's three reject paths -------------------

fn valid_session_meta_line() -> String {
    let envelope = base_envelope(Payload::SessionMeta(fixtures::session_meta()));
    serde_json::to_string(&envelope).expect("serializes")
}

#[test]
fn missing_required_field_is_rejected_as_schema() {
    let mut value: Value = serde_json::from_str(&valid_session_meta_line()).unwrap();
    value
        .as_object_mut()
        .expect("envelope is an object")
        .remove("session_id");
    let line = serde_json::to_string(&value).unwrap();

    match af_events::parse_line(&line) {
        Err(af_events::RejectReason::Schema(_)) => {}
        other => panic!("expected RejectReason::Schema, got {other:?}"),
    }
}

#[test]
fn malformed_json_is_rejected_as_json() {
    let line = "{not valid json";

    match af_events::parse_line(line) {
        Err(af_events::RejectReason::Json(_)) => {}
        other => panic!("expected RejectReason::Json, got {other:?}"),
    }
}

#[test]
fn unsupported_schema_version_is_rejected_as_unknown_version() {
    let mut value: Value = serde_json::from_str(&valid_session_meta_line()).unwrap();
    value["schema_version"] = Value::String("9.9.9".to_string());
    let line = serde_json::to_string(&value).unwrap();

    match af_events::parse_line(&line) {
        Err(af_events::RejectReason::UnknownVersion(v)) => assert_eq!(v, "9.9.9"),
        other => panic!("expected RejectReason::UnknownVersion, got {other:?}"),
    }
}

/// A `date-time` field that is not RFC 3339 must be rejected. This is the
/// constraint the test-local validator used to miss entirely: serde
/// deserializes `ts` as a plain `String`, so *only* the schema's `format`
/// check stands between a malformed timestamp and the store.
#[test]
fn a_non_rfc3339_timestamp_is_rejected_by_the_format_check() {
    let mut value: Value = serde_json::from_str(&valid_session_meta_line()).unwrap();
    value["ts"] = Value::String("last tuesday".to_string());
    let line = serde_json::to_string(&value).unwrap();

    match af_events::parse_line(&line) {
        Err(af_events::RejectReason::Schema(_)) => {}
        other => panic!("expected RejectReason::Schema for a bad ts, got {other:?}"),
    }
}
