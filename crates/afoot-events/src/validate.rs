//! Validation of raw spool lines against the authoritative JSON Schema,
//! plus the semantic checks that sit outside it.
//!
//! [`check_event`] runs the committed
//! `schemas/v0.1/events.schema.json` (embedded at compile time, compiled
//! once into a process-wide [`OnceLock`]) over the parsed
//! [`serde_json::Value`] before it is deserialized into
//! [`crate::Envelope`]. That catches the constraints serde's derived
//! `Deserialize` cannot express — `energy_j` minimums, `event_id`
//! `minLength`, `date-time` formats, `minItems` — so the spool never
//! accepts an event the contract forbids.
//!
//! Serde's `Deserialize` remains the second gate: it enforces the parts of
//! the shape the schema leaves open (the `type`/`payload` discriminated
//! union is expressed with `if`/`then`, which does not *require* the
//! matching payload keys), and both failure modes surface as
//! [`RejectReason::Schema`].

use std::fmt;
use std::sync::OnceLock;

/// The authoritative Contract #1 schema, embedded so validation needs no
/// filesystem access at runtime.
const EVENTS_SCHEMA: &str = include_str!("../../../schemas/v0.1/events.schema.json");

/// Process-wide compiled validator. Compiling the schema costs milliseconds
/// and must never happen per line.
static EVENTS_VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();

/// Why [`crate::parse_line`] rejected a spool line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// The line is not syntactically valid JSON.
    Json(String),
    /// The JSON parsed but doesn't satisfy the event schema (missing
    /// required fields, wrong types, invalid enum values, ...).
    Schema(String),
    /// `schema_version` doesn't match the supported `^0\.1\.[0-9]+$`
    /// pattern.
    UnknownVersion(String),
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RejectReason::Json(msg) => write!(f, "invalid JSON: {msg}"),
            RejectReason::Schema(msg) => write!(f, "schema violation: {msg}"),
            RejectReason::UnknownVersion(v) => write!(f, "unsupported schema_version: {v}"),
        }
    }
}

impl std::error::Error for RejectReason {}

/// The compiled Contract #1 event validator, built on first use.
///
/// `should_validate_formats(true)` is required: under draft 2020-12
/// `format` is an annotation by default, and the contract relies on it for
/// the RFC 3339 `ts`/`t_start`/`t_end` fields.
pub fn events_validator() -> &'static jsonschema::Validator {
    EVENTS_VALIDATOR.get_or_init(|| {
        let schema: serde_json::Value =
            serde_json::from_str(EVENTS_SCHEMA).expect("embedded events schema is valid JSON");
        jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .expect("embedded events schema compiles")
    })
}

/// Validates a raw event value against the embedded Contract #1 schema.
///
/// Reports the first violation with its instance path, so a reject frame
/// names the offending field rather than just "schema violation".
pub fn check_event(value: &serde_json::Value) -> Result<(), RejectReason> {
    match events_validator().validate(value) {
        Ok(()) => Ok(()),
        Err(error) => {
            let path = error.instance_path().to_string();
            let location = if path.is_empty() { "/" } else { path.as_str() };
            Err(RejectReason::Schema(format!("{location}: {error}")))
        }
    }
}

/// Checks the `schema_version` field of a raw JSON value against the
/// schema's `^0\.1\.[0-9]+$` pattern, matching
/// `schemas/v0.1/events.schema.json`.
///
/// If the field is absent or not a string, this passes without error and
/// leaves the "missing required field" case to the subsequent typed
/// deserialization (surfaced as [`RejectReason::Schema`]) — this function's
/// only job is distinguishing "unsupported version" from "malformed event".
pub fn check_schema_version(value: &serde_json::Value) -> Result<(), RejectReason> {
    match value.get("schema_version").and_then(|v| v.as_str()) {
        Some(version) if !is_supported_version(version) => {
            Err(RejectReason::UnknownVersion(version.to_string()))
        }
        _ => Ok(()),
    }
}

/// `^0\.1\.[0-9]+$`
fn is_supported_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let major = parts.next();
    let minor = parts.next();
    let patch = parts.next();
    let trailing_none = parts.next().is_none();

    major == Some("0")
        && minor == Some("1")
        && trailing_none
        && matches!(patch, Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_0_1_x() {
        assert!(is_supported_version("0.1.0"));
        assert!(is_supported_version("0.1.42"));
    }

    #[test]
    fn rejects_other_versions() {
        assert!(!is_supported_version("9.9.9"));
        assert!(!is_supported_version("0.2.0"));
        assert!(!is_supported_version("1.1.0"));
        assert!(!is_supported_version("0.1"));
        assert!(!is_supported_version("0.1.0.1"));
        assert!(!is_supported_version("0.1.beta"));
        assert!(!is_supported_version(""));
    }
}
