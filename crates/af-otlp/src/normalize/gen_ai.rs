use af_events::{Collector, Envelope, LlmCall, Payload, Status, Usage, UsageSource};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::record::LogRecord;
use super::{stable_id, LogNormalizer, NormalizerDescriptor, RecordOutcome};

const DETAILS_EVENT: &str = "gen_ai.client.inference.operation.details";

pub(crate) struct GenAiNormalizer;

impl LogNormalizer for GenAiNormalizer {
    fn descriptor(&self) -> NormalizerDescriptor {
        NormalizerDescriptor {
            id: "otel.gen_ai.logs",
            signal: "logs",
            emits: &["llm_call"],
            lifecycle: "completed_operations",
        }
    }

    fn normalize(&self, record: &LogRecord, index: usize) -> RecordOutcome {
        let standard_scope = record
            .scope_name
            .as_deref()
            .is_some_and(|name| name.starts_with("opentelemetry.instrumentation.genai"));
        let applicable = record.body.as_deref() == Some(DETAILS_EVENT)
            || standard_scope
                && record.attributes.contains_key("gen_ai.provider.name")
                && (record.attributes.contains_key("gen_ai.request.model")
                    || record.attributes.contains_key("gen_ai.response.model"));
        if !applicable {
            return RecordOutcome::NotApplicable;
        }
        match map(record, index) {
            Some(envelope) => RecordOutcome::Envelope(Box::new(envelope)),
            None => RecordOutcome::Dropped,
        }
    }
}

fn map(record: &LogRecord, index: usize) -> Option<Envelope> {
    let provider = record.string("gen_ai.provider.name")?;
    let requested = record
        .string("gen_ai.request.model")
        .or_else(|| record.string("gen_ai.response.model"))?;
    let served = record.string("gen_ai.response.model");
    let input_tokens = record.u64("gen_ai.usage.input_tokens");
    let output_tokens = record.u64("gen_ai.usage.output_tokens");
    let session_id = record
        .string("session.id")
        .or_else(|| record.string("gen_ai.conversation.id"))
        .map(|value| crate::sanitize_id(&value))
        .unwrap_or_else(|| "unknown".to_string());
    let time_unix_nano = record.time_unix_nano?;
    let ts = OffsetDateTime::from_unix_timestamp_nanos(time_unix_nano)
        .ok()?
        .format(&Rfc3339)
        .ok()?;
    let response_id = record.string("gen_ai.response.id");

    let event_id = if let Some(response_id) = response_id {
        stable_id("otlp-genai", &response_id)
    } else {
        stable_id(
            "otlp-genai",
            &(
                session_id.clone(),
                time_unix_nano,
                &provider,
                &requested,
                index,
            ),
        )
    };

    Some(Envelope {
        schema_version: "0.1.0".to_string(),
        event_id,
        ts,
        collector: Collector {
            name: "otlp-genai".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        session_id,
        attribution: None,
        payload: Payload::LlmCall(LlmCall {
            provider,
            model_id_requested: requested,
            model_id_served: served,
            endpoint: record.string("server.address"),
            usage: Usage {
                input_tokens,
                output_tokens,
                thought_tokens: None,
                cached_read_tokens: record.u64("gen_ai.usage.cache_read.input_tokens"),
                cached_write_tokens: record.u64("gen_ai.usage.cache_creation.input_tokens"),
            },
            usage_source: UsageSource::AgentTelemetry,
            duration_ms: None,
            status: record.string("error.type").map(|_| Status::Error),
            streaming: record.bool("gen_ai.request.stream"),
        }),
    })
}
