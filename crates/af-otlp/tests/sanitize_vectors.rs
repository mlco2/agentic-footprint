//! The cross-language conformance vectors for `sanitize_id`.
//!
//! The rule (`[A-Za-z0-9._-]` kept, everything else stripped, then the
//! empty/leading-dot guard) is implemented three times over, in three
//! languages, because the three collectors that build spool filenames are
//! written in three languages:
//!
//!   * `crates/af-otlp/src/sanitize.rs` (this crate — the OTLP receiver)
//!   * `collectors/claude-code/af-hook.sh` (`tr -cd` + a `case` guard)
//!   * `python/af_sampler/__main__.py` (`re.sub` + the same guard)
//!
//! Duplication is the dangerous kind here: two implementations that
//! disagree about what a session id may contain produce two filenames for
//! one session, and the join silently sees two sessions. So the *vectors*
//! are shared even though the code cannot be —
//! `tests/fixtures/sanitize-vectors.json` is read by this test, by a case
//! in `collectors/claude-code/test_hooks.sh` (through the shim's real
//! PreToolUse/SessionStart path) and by one in `python/tests/test_sampler.py`.
//! A change to the rule that isn't made in all three fails in all three.

use std::path::PathBuf;

use af_otlp::sanitize_id;

struct Vector {
    raw: String,
    sanitized: String,
    note: String,
}

fn vectors() -> Vec<Vector> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/sanitize-vectors.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("vectors are valid JSON");
    let field = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|f| f.as_str())
            .unwrap_or_else(|| panic!("every vector needs a string {key:?}: {v}"))
            .to_string()
    };
    parsed
        .as_array()
        .expect("the vector file is an array of {raw, sanitized, note}")
        .iter()
        .map(|v| Vector {
            raw: field(v, "raw"),
            sanitized: field(v, "sanitized"),
            note: field(v, "note"),
        })
        .collect()
}

#[test]
fn every_shared_vector_sanitizes_the_same_way_here() {
    let vectors = vectors();
    assert!(vectors.len() >= 10, "the vector file lost its contents");
    for v in vectors {
        assert_eq!(
            sanitize_id(&v.raw),
            v.sanitized,
            "vector {:?} ({}) — this crate disagrees with the shared vectors",
            v.raw,
            v.note
        );
    }
}

/// Applying the rule to its own output is a no-op, so a filename can be
/// re-derived at any point in the pipeline without drifting.
#[test]
fn every_shared_vector_is_a_fixed_point_of_the_rule() {
    for v in vectors() {
        assert_eq!(
            sanitize_id(&v.sanitized),
            v.sanitized,
            "vector {:?} is not idempotent",
            v.raw
        );
    }
}
