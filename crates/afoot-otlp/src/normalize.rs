//! OTLP log normalization registry.
//!
//! The OTLP tree is flattened once into generic records. Ordered normalizers
//! then independently decide whether they claim a record, keeping transport
//! and protobuf-JSON traversal separate from agent/provider mappings.

mod claude_code;
mod codex;
mod gen_ai;
mod record;

use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use afoot_events::Envelope;
use serde_json::Value;

use claude_code::ClaudeCodeNormalizer;
use codex::CodexNormalizer;
use gen_ai::GenAiNormalizer;
use record::LogRecord;

pub use record::{MAX_ATTRIBUTE_BYTES, MAX_ATTRIBUTE_KEY_BYTES, MAX_LOG_RECORDS};

/// At most this many attribute names are kept in one reject.
pub const MAX_REJECT_KEYS: usize = 64;

pub struct NormalizeOutcome {
    pub events: Vec<Envelope>,
    pub dropped: usize,
    pub unclaimed: usize,
    /// Attribute names seen on dropped records (names only — never values),
    /// so a reject can say what an unmappable record looked like.
    pub dropped_keys: Vec<String>,
    /// The body exceeded [`MAX_LOG_RECORDS`] and was not decoded at all.
    pub over_capacity: bool,
}

pub(crate) enum RecordOutcome {
    Envelope(Box<Envelope>),
    NotApplicable,
    Dropped,
}

pub(crate) trait LogNormalizer {
    fn descriptor(&self) -> NormalizerDescriptor;
    fn normalize(&self, record: &LogRecord, index: usize) -> RecordOutcome;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizerDescriptor {
    pub id: &'static str,
    pub signal: &'static str,
    pub emits: &'static [&'static str],
    pub lifecycle: &'static str,
}

/// The registry lives for the process, not per batch: the Codex normalizer
/// carries per-conversation state (`provider_name` arrives on
/// `conversation_starts`, tokens arrive on later `response.completed`
/// records, usually in different OTLP batches), and per-call construction
/// would silently forget it between batches.
fn registry() -> [&'static dyn LogNormalizer; 3] {
    static CLAUDE_CODE: ClaudeCodeNormalizer = ClaudeCodeNormalizer;
    static CODEX: LazyLock<CodexNormalizer> = LazyLock::new(CodexNormalizer::default);
    static GEN_AI: GenAiNormalizer = GenAiNormalizer;
    [&CLAUDE_CODE, &*CODEX, &GEN_AI]
}

/// One id derivation for every normalizer, so the format (`{prefix}-` +
/// 16 hex digits) and the hasher can never drift apart per agent.
///
/// `DefaultHasher::new()` is deterministic within a Rust release but not
/// guaranteed across releases — a toolchain upgrade may change the ids
/// derived for identical records. The consequence is bounded (a record
/// re-delivered across an upgrade dedups as new), accepted for the PoC,
/// and confined to this one function when it is worth fixing properly.
pub(crate) fn stable_id(prefix: &str, value: &impl Hash) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{prefix}-{:016x}", hasher.finish())
}

pub fn installed_normalizers() -> Vec<NormalizerDescriptor> {
    registry()
        .into_iter()
        .map(LogNormalizer::descriptor)
        .collect()
}

pub fn normalize_logs(body: &Value) -> NormalizeOutcome {
    let normalizers = registry();
    let decoded = record::decode_logs(body);
    let mut events = Vec::new();
    let mut dropped = 0usize;
    let mut unclaimed = 0usize;
    let mut dropped_keys = std::collections::BTreeSet::new();

    for (index, record) in decoded.records.iter().enumerate() {
        let mut claimed = false;
        for normalizer in normalizers {
            match normalizer.normalize(record, index) {
                RecordOutcome::Envelope(envelope) => {
                    events.push(*envelope);
                    claimed = true;
                    break;
                }
                RecordOutcome::Dropped => {
                    dropped += 1;
                    for key in record.keys() {
                        if dropped_keys.len() >= MAX_REJECT_KEYS {
                            break;
                        }
                        dropped_keys.insert(key.to_string());
                    }
                    claimed = true;
                    break;
                }
                RecordOutcome::NotApplicable => {}
            }
        }
        if !claimed {
            unclaimed += 1;
        }
    }

    NormalizeOutcome {
        events,
        dropped,
        unclaimed,
        dropped_keys: dropped_keys.into_iter().collect(),
        over_capacity: decoded.over_capacity,
    }
}
