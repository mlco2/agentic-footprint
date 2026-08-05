//! Reusable Contract #1 sample values for tests, behind the
//! `test-support` feature.
//!
//! Every crate downstream of this one had grown its own private copy of
//! "a plausible `llm_call` envelope" — five of them, differing in the
//! collector name, the token counts, and (more awkwardly) in which
//! optional fields they bothered to set. That is a lot of surface area
//! for something no test is actually asserting about, and it means a
//! change to the contract has to be re-learned in five places.
//!
//! These builders return **plain structs with public fields**, so a test
//! that cares about one field says so with struct-update syntax
//! (`ActionSpan { pids: Some(vec![7]), ..fixtures::action_span(..) }`)
//! and everything it does *not* mention is visibly not the point of the
//! test. Nothing here is randomized or clever: fixtures that surprise you
//! are worse than duplication.
//!
//! The values are schema-valid as-is — `tests/roundtrip.rs` validates
//! each payload kind against `schemas/v0.1/events.schema.json` — so a
//! fixture is a safe starting point for a test that will go on to feed
//! [`crate::parse_line`].

use crate::{
    ActionSpan, AgentApp, Attribution, Collector, EnergyComponent, EnergyKind, EnergyMethod,
    EnergySample, Envelope, ExecutionLocus, LlmCall, Payload, ProcessDelta, ProcessSample,
    SessionMeta, ToolKind, Usage, UsageSource,
};

/// The schema version every fixture declares.
pub const SCHEMA_VERSION: &str = "0.1.0";

/// Pads `name` out to the schema's `event_id` `minLength: 16`, so tests
/// can use short readable ids (`event_id("evt-1")`) without littering the
/// assertions with ULIDs or tripping validation.
pub fn event_id(name: &str) -> String {
    format!("{name:-<16}")
}

/// The collector stamp every fixture carries.
pub fn collector() -> Collector {
    Collector {
        name: "test-collector".to_string(),
        version: "0.1.0".to_string(),
    }
}

/// An envelope around `payload`, with no [`Attribution`].
///
/// `event_id` is used **verbatim**: tests key stored estimates and join
/// lookups off it, so silently rewriting it would break the correspondence
/// they rely on. Pass it through [`event_id`] when the event has to
/// survive schema validation.
pub fn envelope(event_id: &str, session_id: &str, ts: &str, payload: Payload) -> Envelope {
    Envelope {
        schema_version: SCHEMA_VERSION.to_string(),
        event_id: event_id.to_string(),
        ts: ts.to_string(),
        collector: collector(),
        session_id: session_id.to_string(),
        attribution: None,
        payload,
    }
}

/// A fully populated [`Attribution`] — every optional field set, so a test
/// asserting that one of them is carried through can clear the rest.
pub fn attribution() -> Attribution {
    Attribution {
        agent_id: Some("main".to_string()),
        subagent_id: None,
        task_id: Some("task-1".to_string()),
        tool_call_id: None,
    }
}

/// One remote LLM call: anthropic, tokens reported by the API itself.
pub fn llm_call() -> LlmCall {
    LlmCall {
        provider: "anthropic".to_string(),
        model_id_requested: "claude-sonnet-5".to_string(),
        model_id_served: None,
        endpoint: None,
        usage: Usage {
            output_tokens: Some(100),
            ..Default::default()
        },
        usage_source: UsageSource::ApiResponse,
        duration_ms: Some(1000),
        status: None,
        streaming: None,
    }
}

/// A local `Bash` action over `[t_start, t_end)`, owning no pids.
pub fn action_span(span_id: &str, t_start: &str, t_end: &str) -> ActionSpan {
    ActionSpan {
        span_id: span_id.to_string(),
        tool_name: "Bash".to_string(),
        tool_kind: ToolKind::Bash,
        execution_locus: ExecutionLocus::Local,
        t_start: t_start.to_string(),
        t_end: t_end.to_string(),
        pids: None,
        cgroup: None,
        status: None,
    }
}

/// An energy sample reporting `joules` as a single measured `total`
/// component — the shape the attribution policy treats as the machine
/// figure, rather than one it has to sum the subsystems to obtain.
pub fn energy_sample(t_start: &str, t_end: &str, joules: f64) -> EnergySample {
    EnergySample {
        t_start: t_start.to_string(),
        t_end: t_end.to_string(),
        components: vec![EnergyComponent {
            kind: EnergyKind::Total,
            label: None,
            energy_j: joules,
            method: EnergyMethod::Rapl,
        }],
        host_id: None,
    }
}

/// A process sample built from `(pid, cpu_time_delta_ms, orphan_of)` rows.
///
/// Rows rather than a pid list because the two facts that make this
/// payload interesting — the same pid appearing twice, and the
/// `orphan_of` tail — are per-row, and a helper that could not express
/// them would only be usable for the uninteresting case.
pub fn process_sample(
    t_start: &str,
    t_end: &str,
    rows: &[(i64, u64, Option<&str>)],
) -> ProcessSample {
    ProcessSample {
        t_start: t_start.to_string(),
        t_end: t_end.to_string(),
        processes: rows
            .iter()
            .map(|(pid, cpu_time_delta_ms, orphan_of)| ProcessDelta {
                pid: *pid,
                cpu_time_delta_ms: *cpu_time_delta_ms,
                orphan_of: orphan_of.map(str::to_string),
                memory_rss_bytes: None,
                io_read_bytes: None,
                io_write_bytes: None,
            })
            .collect(),
    }
}

/// Session context for a `claude-code` session, with nothing optional set
/// — notably no `geo_zone`, which is user-configured and never invented.
pub fn session_meta() -> SessionMeta {
    SessionMeta {
        agent_app: AgentApp {
            name: "claude-code".to_string(),
            version: None,
        },
        os: None,
        hardware: None,
        geo_zone: None,
        power_source: None,
    }
}
