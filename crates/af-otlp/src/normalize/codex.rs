use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use af_events::{
    ActionSpan, AgentApp, Attribution, Collector, Envelope, ExecutionLocus, LlmCall, Payload,
    SessionMeta, Status, ToolKind, Usage, UsageSource,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use super::record::LogRecord;
use super::{stable_id, LogNormalizer, NormalizerDescriptor, RecordOutcome};

mod attrs {
    pub const EVENT_NAME: &str = "event.name";
    pub const EVENT_KIND: &str = "event.kind";
    pub const EVENT_TIMESTAMP: &str = "event.timestamp";
    pub const CONVERSATION_ID: &str = "conversation.id";
    pub const MODEL: &str = "model";
    pub const APP_VERSION: &str = "app.version";
    pub const PROVIDER_NAME: &str = "provider_name";
    pub const INPUT_TOKENS: &str = "input_token_count";
    pub const OUTPUT_TOKENS: &str = "output_token_count";
    pub const CACHED_TOKENS: &str = "cached_token_count";
    pub const CACHE_WRITE_TOKENS: &str = "cache_write_token_count";
    pub const REASONING_TOKENS: &str = "reasoning_token_count";
    pub const TOOL_NAME: &str = "tool_name";
    pub const CALL_ID: &str = "call_id";
    pub const DURATION_MS: &str = "duration_ms";
    pub const SUCCESS: &str = "success";
    pub const MCP_SERVER: &str = "mcp_server";
}

/// How many conversation → provider mappings are remembered. A resident
/// `af watch` sees a handful of concurrent Codex sessions, not hundreds;
/// past the cap the oldest mapping is evicted and a very old session's
/// later calls fall back to `unknown` — stated, never guessed.
const PROVIDER_CACHE_CAP: usize = 512;

/// `provider_name` arrives once, on `codex.conversation_starts`;
/// `response.completed` carries usage but no provider. This cache carries
/// the one to the other. A `response.completed` for a conversation this
/// cache has never seen means the receiver missed the session's start (it
/// was not running, or the mapping was evicted) — the honest provider for
/// that case is `unknown`, not a hardcoded guess: Codex is provider-
/// configurable (`model_providers`), including OSS models whose names look
/// deceptively like OpenAI's.
#[derive(Default)]
struct ProviderCache {
    by_conversation: HashMap<String, String>,
    insertion_order: VecDeque<String>,
}

impl ProviderCache {
    fn insert(&mut self, conversation: String, provider: String) {
        if self
            .by_conversation
            .insert(conversation.clone(), provider)
            .is_none()
        {
            self.insertion_order.push_back(conversation);
            if self.insertion_order.len() > PROVIDER_CACHE_CAP {
                if let Some(evicted) = self.insertion_order.pop_front() {
                    self.by_conversation.remove(&evicted);
                }
            }
        }
    }

    fn get(&self, conversation: &str) -> Option<String> {
        self.by_conversation.get(conversation).cloned()
    }
}

#[derive(Default)]
pub(crate) struct CodexNormalizer {
    providers: Mutex<ProviderCache>,
}

impl LogNormalizer for CodexNormalizer {
    fn descriptor(&self) -> NormalizerDescriptor {
        NormalizerDescriptor {
            id: "codex.native_otel",
            signal: "logs",
            emits: &["session_meta", "llm_call", "action_span"],
            lifecycle: "completed_operations",
        }
    }

    fn normalize(&self, record: &LogRecord, index: usize) -> RecordOutcome {
        match record.string(attrs::EVENT_NAME).as_deref() {
            Some("codex.conversation_starts") => {
                self.remember_provider(record);
                match map_session(record, index) {
                    Some(envelope) => RecordOutcome::Envelope(Box::new(envelope)),
                    None => RecordOutcome::Dropped,
                }
            }
            Some("codex.sse_event")
                if record.string(attrs::EVENT_KIND).as_deref() == Some("response.completed") =>
            {
                match map_llm_call(record, index, &self.provider_for(record)) {
                    Some(envelope) => RecordOutcome::Envelope(Box::new(envelope)),
                    None if has_any_usage(record) => RecordOutcome::Dropped,
                    None => RecordOutcome::NotApplicable,
                }
            }
            Some("codex.tool_result") => match map_action(record, index) {
                Some(envelope) => RecordOutcome::Envelope(Box::new(envelope)),
                None => RecordOutcome::Dropped,
            },
            _ => RecordOutcome::NotApplicable,
        }
    }
}

impl CodexNormalizer {
    fn remember_provider(&self, record: &LogRecord) {
        let (Some(conversation), Some(provider)) = (
            record.string(attrs::CONVERSATION_ID),
            record.string(attrs::PROVIDER_NAME),
        ) else {
            return;
        };
        if let Ok(mut cache) = self.providers.lock() {
            cache.insert(crate::sanitize_id(&conversation), provider);
        }
    }

    fn provider_for(&self, record: &LogRecord) -> String {
        record
            .string(attrs::CONVERSATION_ID)
            .and_then(|conversation| {
                self.providers
                    .lock()
                    .ok()?
                    .get(&crate::sanitize_id(&conversation))
            })
            .unwrap_or_else(|| "unknown".to_string())
    }
}

fn map_session(record: &LogRecord, index: usize) -> Option<Envelope> {
    let session_id = crate::sanitize_id(&record.string(attrs::CONVERSATION_ID)?);
    let (time_unix_nano, ts) = record_time(record)?;
    Some(Envelope {
        schema_version: "0.1.0".to_string(),
        event_id: stable_id("otlp-codex-session", &(&session_id, time_unix_nano, index)),
        ts,
        collector: Collector {
            name: "otlp-codex".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        session_id,
        attribution: None,
        payload: Payload::SessionMeta(SessionMeta {
            agent_app: AgentApp {
                name: "codex".to_string(),
                version: record.string(attrs::APP_VERSION),
            },
            os: None,
            hardware: None,
            geo_zone: None,
            power_source: None,
        }),
    })
}

fn map_llm_call(record: &LogRecord, index: usize, provider: &str) -> Option<Envelope> {
    let session_id = crate::sanitize_id(&record.string(attrs::CONVERSATION_ID)?);
    let model = record.string(attrs::MODEL)?;
    let input_tokens = record.u64(attrs::INPUT_TOKENS);
    let output_tokens = record.u64(attrs::OUTPUT_TOKENS);
    let cached_read_tokens = record.u64(attrs::CACHED_TOKENS);
    let cached_write_tokens = record.u64(attrs::CACHE_WRITE_TOKENS);
    let thought_tokens = record.u64(attrs::REASONING_TOKENS);
    if [
        input_tokens,
        output_tokens,
        cached_read_tokens,
        cached_write_tokens,
        thought_tokens,
    ]
    .into_iter()
    .all(|value| value.is_none())
    {
        return None;
    }
    let (time_unix_nano, ts) = record_time(record)?;

    Some(Envelope {
        schema_version: "0.1.0".to_string(),
        event_id: stable_id(
            "otlp-codex",
            &(
                &session_id,
                time_unix_nano,
                &model,
                input_tokens,
                output_tokens,
                cached_read_tokens,
                thought_tokens,
                index,
            ),
        ),
        ts,
        collector: Collector {
            name: "otlp-codex".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        session_id,
        attribution: None,
        payload: Payload::LlmCall(LlmCall {
            provider: provider.to_string(),
            model_id_requested: model,
            model_id_served: None,
            endpoint: None,
            usage: Usage {
                input_tokens,
                output_tokens,
                thought_tokens,
                cached_read_tokens,
                cached_write_tokens,
            },
            usage_source: UsageSource::AgentTelemetry,
            duration_ms: None,
            // Native Codex emits token usage only on a successful streamed
            // `response.completed` event. Errors are separate event kinds and
            // never reach this mapper.
            status: Some(Status::Ok),
            streaming: Some(true),
        }),
    })
}

fn map_action(record: &LogRecord, index: usize) -> Option<Envelope> {
    let session_id = crate::sanitize_id(&record.string(attrs::CONVERSATION_ID)?);
    let tool_name = record.string(attrs::TOOL_NAME)?;
    let call_id = record.string(attrs::CALL_ID);
    let duration_ms = record.u64(attrs::DURATION_MS)?;
    let (time_unix_nano, end_ts) = record_time(record)?;
    let end = OffsetDateTime::parse(&end_ts, &Rfc3339).ok()?;
    let start = end.checked_sub(Duration::milliseconds(i64::try_from(duration_ms).ok()?))?;
    let kind = classify_tool(&tool_name);
    let locus = classify_locus(kind, record.string(attrs::MCP_SERVER).as_deref());
    let span_id = call_id.clone().unwrap_or_else(|| {
        stable_id(
            "codex-call",
            &(&session_id, time_unix_nano, &tool_name, duration_ms, index),
        )
    });

    Some(Envelope {
        schema_version: "0.1.0".to_string(),
        event_id: call_id
            .as_ref()
            .map(|value| stable_id("otlp-codex-tool", value))
            .unwrap_or_else(|| stable_id("otlp-codex-tool", &span_id)),
        ts: end.format(&Rfc3339).ok()?,
        collector: Collector {
            name: "otlp-codex".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        session_id,
        attribution: Some(Attribution {
            agent_id: None,
            subagent_id: None,
            task_id: None,
            tool_call_id: Some(span_id.clone()),
        }),
        payload: Payload::ActionSpan(ActionSpan {
            span_id,
            tool_name,
            tool_kind: kind,
            execution_locus: locus,
            t_start: start.format(&Rfc3339).ok()?,
            t_end: end.format(&Rfc3339).ok()?,
            pids: None,
            cgroup: None,
            status: Some(match record.bool(attrs::SUCCESS) {
                Some(true) => Status::Ok,
                Some(false) => Status::Error,
                None => Status::Unknown,
            }),
        }),
    })
}

fn has_any_usage(record: &LogRecord) -> bool {
    [
        attrs::INPUT_TOKENS,
        attrs::OUTPUT_TOKENS,
        attrs::CACHED_TOKENS,
        attrs::CACHE_WRITE_TOKENS,
        attrs::REASONING_TOKENS,
    ]
    .into_iter()
    .any(|key| record.u64(key).is_some())
}

fn classify_tool(tool_name: &str) -> ToolKind {
    match tool_name {
        "exec_command" | "shell" => ToolKind::Bash,
        "apply_patch" | "file_change" => ToolKind::FileOp,
        "web_search" => ToolKind::Web,
        "spawn_agent" | "send_input" | "wait_agent" => ToolKind::Subagent,
        name if name.starts_with("mcp__") => ToolKind::Mcp,
        _ => ToolKind::Other,
    }
}

fn classify_locus(kind: ToolKind, mcp_server: Option<&str>) -> ExecutionLocus {
    match kind {
        ToolKind::Bash | ToolKind::FileOp | ToolKind::Subagent => ExecutionLocus::Local,
        ToolKind::Web => ExecutionLocus::Remote,
        // An MCP server is as likely a local process as a remote service,
        // and nothing in the event says which. `unknown` is the same honest
        // choice the Claude Code hook collector made — claiming `remote` would silently excuse
        // local MCP compute from measurement.
        ToolKind::Mcp => ExecutionLocus::Unknown,
        ToolKind::Other if mcp_server.is_some_and(|value| !value.is_empty()) => {
            ExecutionLocus::Unknown
        }
        ToolKind::Other => ExecutionLocus::Unknown,
    }
}

fn format_time(time_unix_nano: i128) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(time_unix_nano)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

fn record_time(record: &LogRecord) -> Option<(i128, String)> {
    if let Some(time_unix_nano) = record.time_unix_nano.filter(|value| *value > 0) {
        return Some((time_unix_nano, format_time(time_unix_nano)?));
    }
    let timestamp = record.string(attrs::EVENT_TIMESTAMP)?;
    let parsed = OffsetDateTime::parse(&timestamp, &Rfc3339).ok()?;
    Some((parsed.unix_timestamp_nanos(), timestamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_codex_tools() {
        assert_eq!(classify_tool("exec_command"), ToolKind::Bash);
        assert_eq!(classify_tool("apply_patch"), ToolKind::FileOp);
        assert_eq!(classify_tool("mcp__github__search"), ToolKind::Mcp);
        assert_eq!(classify_tool("spawn_agent"), ToolKind::Subagent);
    }

    #[test]
    fn mcp_tools_get_unknown_locus_like_the_other_collectors() {
        assert_eq!(classify_locus(ToolKind::Mcp, None), ExecutionLocus::Unknown);
        assert_eq!(
            classify_locus(ToolKind::Other, Some("github")),
            ExecutionLocus::Unknown
        );
        assert_eq!(classify_locus(ToolKind::Bash, None), ExecutionLocus::Local);
    }

    fn record_with(attrs: &[(&str, &str)]) -> LogRecord {
        LogRecord {
            body: None,
            time_unix_nano: Some(1_785_074_307_423_000_000),
            attributes: attrs
                .iter()
                .map(|(key, value)| (key.to_string(), serde_json::json!({ "stringValue": value })))
                .collect(),
            scope_name: None,
        }
    }

    #[test]
    fn provider_name_is_carried_from_conversation_starts_to_llm_calls() {
        let normalizer = CodexNormalizer::default();
        let starts = record_with(&[
            ("event.name", "codex.conversation_starts"),
            ("conversation.id", "conv-1"),
            ("provider_name", "ollama"),
        ]);
        assert!(matches!(
            normalizer.normalize(&starts, 0),
            RecordOutcome::Envelope(_)
        ));

        let completed = record_with(&[
            ("event.name", "codex.sse_event"),
            ("event.kind", "response.completed"),
            ("conversation.id", "conv-1"),
            ("model", "gpt-oss:120b"),
            ("output_token_count", "42"),
        ]);
        let RecordOutcome::Envelope(envelope) = normalizer.normalize(&completed, 1) else {
            panic!("expected llm_call envelope");
        };
        let Payload::LlmCall(call) = &envelope.payload else {
            panic!("expected llm_call payload");
        };
        assert_eq!(call.provider, "ollama");
    }

    #[test]
    fn llm_call_without_a_seen_conversation_start_says_unknown_provider() {
        let normalizer = CodexNormalizer::default();
        let completed = record_with(&[
            ("event.name", "codex.sse_event"),
            ("event.kind", "response.completed"),
            ("conversation.id", "conv-never-seen"),
            ("model", "gpt-5.6-sol"),
            ("output_token_count", "7"),
        ]);
        let RecordOutcome::Envelope(envelope) = normalizer.normalize(&completed, 0) else {
            panic!("expected llm_call envelope");
        };
        let Payload::LlmCall(call) = &envelope.payload else {
            panic!("expected llm_call payload");
        };
        assert_eq!(call.provider, "unknown");
    }
}
