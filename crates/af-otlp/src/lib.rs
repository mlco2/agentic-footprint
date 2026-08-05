//! af-otlp: local OTLP http/json receiver with pluggable log normalizers.
//!
//! Claude Code, run with `CLAUDE_CODE_ENABLE_TELEMETRY=1` and
//! `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`, exports OTel logs and metrics
//! over HTTP to a configurable `OTEL_EXPORTER_OTLP_ENDPOINT`. This crate is
//! that endpoint: [`serve`] runs a tiny HTTP server accepting `POST
//! /v1/logs` (normalized into Contract #1 `llm_call` events and appended to
//! the spool) and `POST /v1/metrics` (accepted, discarded — no Contract #1
//! shape for raw OTel metrics yet). Anything else 404s.
//!
//! [`normalize_logs`] flattens OTLP resource/scope/record attributes once and
//! dispatches each record to ordered normalizers. The built-ins cover Claude
//! Code's captured `claude_code.api_request` logs and standard `gen_ai.*`
//! inference-detail records. Unclaimed valid records are counted separately
//! from claimed records that failed to map.

mod normalize;
mod sanitize;
mod server;

pub use normalize::{
    installed_normalizers, normalize_logs, NormalizeOutcome, NormalizerDescriptor,
};
pub use sanitize::sanitize_id;
pub use server::{serve, Counters, ServerHandle};
