use af_events::{Collector, Envelope, LlmCall, Payload, Usage, UsageSource};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::record::LogRecord;
use super::{stable_id, LogNormalizer, NormalizerDescriptor, RecordOutcome};

mod attrs {
    pub const EVENT_BODY: &str = "claude_code.api_request";
    pub const SESSION_ID: &str = "session.id";
    pub const MODEL: &str = "model";
    pub const INPUT_TOKENS: &str = "input_tokens";
    pub const OUTPUT_TOKENS: &str = "output_tokens";
    pub const CACHE_READ_TOKENS: &str = "cache_read_tokens";
    pub const CACHE_CREATION_TOKENS: &str = "cache_creation_tokens";
    pub const DURATION_MS: &str = "duration_ms";
    pub const REQUEST_ID: &str = "request_id";
}

const MIN_EVENT_ID_LEN: usize = 16;

pub(crate) struct ClaudeCodeNormalizer;

impl LogNormalizer for ClaudeCodeNormalizer {
    fn descriptor(&self) -> NormalizerDescriptor {
        NormalizerDescriptor {
            id: "claude_code.api_request",
            signal: "logs",
            emits: &["llm_call"],
            lifecycle: "completed_operations",
        }
    }

    fn normalize(&self, record: &LogRecord, index: usize) -> RecordOutcome {
        if record.body.as_deref() != Some(attrs::EVENT_BODY) {
            return RecordOutcome::NotApplicable;
        }
        match map(record, index) {
            Some(envelope) => RecordOutcome::Envelope(Box::new(envelope)),
            None => RecordOutcome::Dropped,
        }
    }
}

fn map(record: &LogRecord, index: usize) -> Option<Envelope> {
    let session_id = record
        .string(attrs::SESSION_ID)
        .map(|raw| crate::sanitize_id(&raw))
        .unwrap_or_else(|| "unknown".to_string());
    let model = record.string(attrs::MODEL)?;
    let input_tokens = record.u64(attrs::INPUT_TOKENS);
    let output_tokens = record.u64(attrs::OUTPUT_TOKENS);
    let time_unix_nano = record.time_unix_nano?;
    let ts = OffsetDateTime::from_unix_timestamp_nanos(time_unix_nano)
        .ok()?
        .format(&Rfc3339)
        .ok()?;
    let request_id = record.string(attrs::REQUEST_ID);

    Some(Envelope {
        schema_version: "0.1.0".to_string(),
        event_id: derive_event_id(
            request_id.as_deref(),
            index,
            &session_id,
            time_unix_nano,
            &model,
            input_tokens,
            output_tokens,
        ),
        ts,
        collector: Collector {
            name: "otlp-cc".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        session_id,
        attribution: None,
        payload: Payload::LlmCall(LlmCall {
            provider: "anthropic".to_string(),
            model_id_requested: model,
            model_id_served: None,
            endpoint: None,
            usage: Usage {
                input_tokens,
                output_tokens,
                thought_tokens: None,
                cached_read_tokens: record.u64(attrs::CACHE_READ_TOKENS),
                cached_write_tokens: record.u64(attrs::CACHE_CREATION_TOKENS),
            },
            usage_source: UsageSource::AgentTelemetry,
            duration_ms: record.u64(attrs::DURATION_MS),
            status: None,
            streaming: None,
        }),
    })
}

fn derive_event_id(
    request_id: Option<&str>,
    index: usize,
    session_id: &str,
    time_unix_nano: i128,
    model: &str,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> String {
    if let Some(request_id) = request_id {
        let id = format!("otlp-{request_id}");
        if id.len() < MIN_EVENT_ID_LEN {
            return stable_id(&id, &request_id);
        }
        return id;
    }
    // Tuple hashing writes each field in sequence, exactly like the
    // field-at-a-time hashing this replaced — the derived ids are unchanged.
    stable_id(
        "otlp",
        &(
            session_id,
            time_unix_nano,
            model,
            input_tokens,
            output_tokens,
            index,
        ),
    )
}
