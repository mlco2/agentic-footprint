//! What the receiver hands to its owner instead of writing files.

use afoot_events::Envelope;

/// One unit of receiver output, sent over the owner's bounded channel.
#[derive(Debug)]
pub enum Inbound {
    /// Normalized, allowlisted events from one request.
    Facts(Vec<Envelope>),
    /// Something that could not become a fact. Metadata only.
    Reject(Reject),
}

/// Why a request (or part of one) was not turned into facts. Carries the
/// attribute *names* and the body size — never a value, never the body.
#[derive(Debug, Clone, PartialEq)]
pub struct Reject {
    pub ts_ms: i64,
    pub source: String,
    pub reason: String,
    pub attribute_keys: Vec<String>,
    pub bytes: u64,
}
