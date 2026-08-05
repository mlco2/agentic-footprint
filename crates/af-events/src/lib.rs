//! Contract #1 event types for agentic-footprint collectors.
//!
//! These types are the serde model for `schemas/v0.1/events.schema.json`: the
//! wire format collectors append (one JSON object per line) to the local
//! JSONL spool. Collectors emit raw facts only — no impact estimates.
//!
//! The envelope's `type` and `payload` fields are siblings in the schema
//! (`{ "type": "llm_call", "payload": { ... } }`), with the shape of
//! `payload` selected by `type` via `allOf`/`if`/`then`. [`Payload`] models
//! this with serde's adjacently-tagged-enum representation
//! (`#[serde(tag = "type", content = "payload")]`) and [`Envelope`] flattens
//! it, so the two fields land as siblings of the envelope's own fields
//! rather than nested under a `payload` object.

pub mod validate;

/// Sample Contract #1 values for tests in this crate and downstream ones.
///
/// Behind a feature so the builders never ship in a release binary, and so
/// a crate that wants them must say so in its `dev-dependencies` — a test
/// helper reachable from production code is a test helper that ends up
/// called from production code.
#[cfg(feature = "test-support")]
pub mod fixtures;

pub use validate::RejectReason;

use serde::{Deserialize, Serialize};

/// Top-level event envelope written to the spool, one per JSONL line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema_version: String,
    pub event_id: String,
    pub ts: String,
    pub collector: Collector,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub attribution: Option<Attribution>,
    #[serde(flatten)]
    pub payload: Payload,
}

/// A syntactically valid, base-envelope-valid event whose `type` is not known
/// to this control-plane version. Preserved verbatim for audit/replay but
/// deliberately excluded from typed derivation.
#[derive(Debug, Clone, PartialEq)]
pub struct OpaqueEvent {
    pub schema_version: String,
    pub event_id: String,
    pub ts: String,
    pub collector: Collector,
    pub session_id: String,
    pub type_tag: String,
    pub json: serde_json::Value,
}

#[allow(clippy::large_enum_variant)]
pub enum ParsedLine {
    Known(Envelope),
    Opaque(OpaqueEvent),
}

impl Envelope {
    /// This event's Contract #1 `type` string, from the payload
    /// discriminant — `llm_call`, `energy_sample`, `action_span`,
    /// `process_sample`, `session_meta`.
    ///
    /// Reading the tag off the enum rather than off a serialized
    /// [`serde_json::Value`] means a consumer that needs only the type
    /// (the store's indexed `type` column, a router, a counter) pays
    /// nothing to serialize the whole event, and cannot be wrong about it:
    /// the strings here and serde's `rename_all = "snake_case"` tags are
    /// checked against each other by this crate's tests.
    pub fn type_tag(&self) -> &'static str {
        self.payload.type_tag()
    }
}

/// Identifies the collector that emitted the event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collector {
    pub name: String,
    pub version: String,
}

/// Optional deepening of correlation to task/tool level. The schema forbids
/// additional properties here, so unrecognized fields are a schema error.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribution {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subagent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

/// The five Contract #1 payload kinds. Serializes/deserializes as sibling
/// `type`/`payload` fields once flattened into [`Envelope`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Payload {
    LlmCall(LlmCall),
    EnergySample(EnergySample),
    ActionSpan(ActionSpan),
    ProcessSample(ProcessSample),
    SessionMeta(SessionMeta),
}

impl Payload {
    /// The `type` string this variant serializes as. Must stay in step with
    /// the `rename_all = "snake_case"` tags above; `tests/roundtrip.rs`
    /// asserts it against the actual serialization of each variant.
    pub fn type_tag(&self) -> &'static str {
        match self {
            Payload::LlmCall(_) => "llm_call",
            Payload::EnergySample(_) => "energy_sample",
            Payload::ActionSpan(_) => "action_span",
            Payload::ProcessSample(_) => "process_sample",
            Payload::SessionMeta(_) => "session_meta",
        }
    }
}

/// One remote LLM inference request. Raw facts only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCall {
    pub provider: String,
    pub model_id_requested: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_id_served: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub endpoint: Option<String>,
    pub usage: Usage,
    pub usage_source: UsageSource,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub streaming: Option<bool>,
}

/// Token usage for one LLM call. The schema forbids additional properties.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thought_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cached_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cached_write_tokens: Option<u64>,
}

/// Provenance of usage numbers, in decreasing reliability order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    ApiResponse,
    AgentTelemetry,
    Transcript,
    Estimated,
}

/// Shared status enum for `llm_call` and `action_span` payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Error,
    Cancelled,
    Unknown,
}

/// Locally measured (or hardware-modeled) energy over an interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnergySample {
    pub t_start: String,
    pub t_end: String,
    pub components: Vec<EnergyComponent>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub host_id: Option<String>,
}

/// One measured/modeled energy component within an [`EnergySample`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnergyComponent {
    pub kind: EnergyKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
    pub energy_j: f64,
    pub method: EnergyMethod,
}

/// Which subsystem an [`EnergyComponent`] measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnergyKind {
    Cpu,
    Dram,
    Gpu,
    Total,
    Other,
}

/// How an [`EnergyComponent`]'s `energy_j` was obtained. Measured
/// (`Rapl`/`Powermetrics`/`Nvml`) vs. modeled (`TdpModel`) must stay
/// distinguishable per the schema's description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnergyMethod {
    Rapl,
    Powermetrics,
    Nvml,
    TdpModel,
    Other,
}

/// One agent action (tool run, subagent, file operation). Overlapping spans
/// are legal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionSpan {
    pub span_id: String,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub execution_locus: ExecutionLocus,
    pub t_start: String,
    pub t_end: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pids: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cgroup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<Status>,
}

/// The kind of tool an [`ActionSpan`] ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Bash,
    Mcp,
    FileOp,
    Subagent,
    Web,
    Other,
}

/// Where an [`ActionSpan`] executed. Remote spans are excluded from the
/// local energy join and reported as unmeasured remote activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLocus {
    Local,
    Remote,
    Hybrid,
    Unknown,
}

/// Per-process-tree resource deltas over an interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessSample {
    pub t_start: String,
    pub t_end: String,
    pub processes: Vec<ProcessDelta>,
}

/// Resource deltas for one watched process tree, keyed by its root pid.
///
/// The same `pid` may legitimately appear more than once in one
/// [`ProcessSample`] (two spans watching the same tree, or a span plus the
/// orphan tail of an earlier one). Entries are
/// therefore per-watch, not per-machine-pid, and must never be naively
/// summed to obtain machine CPU time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDelta {
    pub pid: i64,
    pub cpu_time_delta_ms: u64,
    /// Set by the codecarbon sampler while a watched tree is in its 60s
    /// orphan tail: the tree outlived the span named here. Schema extension
    /// (the schema's `process_sample` items don't forbid additional
    /// properties); such entries belong to no span and are attributed to
    /// the orphaned-compute bucket.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub orphan_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_rss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub io_read_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub io_write_bytes: Option<u64>,
}

/// Session context. Geo zone is user-configured, never auto-geolocated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub agent_app: AgentApp,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hardware: Option<Hardware>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub geo_zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub power_source: Option<PowerSource>,
}

/// The agent application hosting the session, e.g. `claude-code`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentApp {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
}

/// Host hardware info, used for TDP fallback modeling when counters are
/// unavailable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hardware {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gpu_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ram_gb: Option<f64>,
}

/// Power source at time of sampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

/// Parse one JSONL spool line into a validated [`Envelope`].
///
/// Distinguishes three failure modes so callers (the spool writer) can
/// route rejects appropriately:
/// - [`RejectReason::Json`]: the line is not syntactically valid JSON.
/// - [`RejectReason::UnknownVersion`]: `schema_version` doesn't match the
///   supported `0.1.x` pattern.
/// - [`RejectReason::Schema`]: the JSON is well-formed but doesn't satisfy
///   the event schema (missing required fields, wrong types, invalid enum
///   values, out-of-range numbers, malformed timestamps, etc.).
///
/// Schema conformance is checked twice, deliberately. The committed
/// `schemas/v0.1/events.schema.json` runs first against the raw
/// [`serde_json::Value`] — it is the contract, and it expresses constraints
/// the Rust types cannot (`energy_j` ≥ 0, `event_id` ≥ 16 chars, RFC 3339
/// timestamps, non-empty `components`). Deserialization into [`Envelope`]
/// then enforces the discriminated `type`/`payload` union, which the
/// schema's `if`/`then` form only constrains when the payload key is
/// present. The compiled validator lives in a `OnceLock`, so the per-line
/// cost is validation only, never compilation.
pub fn parse_line(line: &str) -> Result<Envelope, RejectReason> {
    match parse_line_preserving_unknown(line)? {
        ParsedLine::Known(envelope) => Ok(envelope),
        ParsedLine::Opaque(event) => Err(RejectReason::Schema(format!(
            "/type: unknown event type {:?}",
            event.type_tag
        ))),
    }
}

pub fn parse_line_preserving_unknown(line: &str) -> Result<ParsedLine, RejectReason> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| RejectReason::Json(e.to_string()))?;
    validate::check_schema_version(&value)?;
    let type_tag = value.get("type").and_then(serde_json::Value::as_str);
    if type_tag.is_some_and(|type_tag| {
        matches!(
            type_tag,
            "llm_call" | "energy_sample" | "action_span" | "process_sample" | "session_meta"
        )
    }) {
        validate::check_event(&value)?;
        return serde_json::from_value(value)
            .map(ParsedLine::Known)
            .map_err(|e| RejectReason::Schema(e.to_string()));
    }
    parse_opaque(value).map(ParsedLine::Opaque)
}

fn parse_opaque(value: serde_json::Value) -> Result<OpaqueEvent, RejectReason> {
    let required_string = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| RejectReason::Schema(format!("/{key}: required non-empty string")))
    };
    let schema_version = required_string("schema_version")?;
    let event_id = required_string("event_id")?;
    if event_id.len() < 16 {
        return Err(RejectReason::Schema(
            "/event_id: must contain at least 16 characters".to_string(),
        ));
    }
    let ts = required_string("ts")?;
    time::OffsetDateTime::parse(&ts, &time::format_description::well_known::Rfc3339)
        .map_err(|error| RejectReason::Schema(format!("/ts: {error}")))?;
    let collector: Collector = serde_json::from_value(
        value
            .get("collector")
            .cloned()
            .ok_or_else(|| RejectReason::Schema("/collector: required object".to_string()))?,
    )
    .map_err(|error| RejectReason::Schema(format!("/collector: {error}")))?;
    let session_id = required_string("session_id")?;
    let type_tag = required_string("type")?;
    if value.get("payload").is_none() {
        return Err(RejectReason::Schema("/payload: required".to_string()));
    }
    Ok(OpaqueEvent {
        schema_version,
        event_id,
        ts,
        collector,
        session_id,
        type_tag,
        json: value,
    })
}
