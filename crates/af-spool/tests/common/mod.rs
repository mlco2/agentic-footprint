//! Helpers shared by `tail.rs` and `reject.rs`.
//!
//! `tests/common/mod.rs` rather than a `tests/common.rs`: cargo compiles
//! every top-level file in `tests/` as its own test binary, and a
//! subdirectory module is the way to share code between them without also
//! running it as an (empty) suite of its own.
//!
//! The module is compiled into *each* test binary independently, so a
//! helper only one of them uses reads as dead code in the other. That is
//! inherent to the sharing mechanism, not a sign of an unused helper.
#![allow(dead_code)]

use af_spool::SpoolFile;
use std::path::Path;

/// A [`SpoolFile`] for `<dir>/<collector>.<session_id>.jsonl`.
///
/// The path is built through [`af_spool::spool_file_name`] — the same
/// function the writers use — so a test cannot accidentally agree with a
/// filename grammar the library no longer implements.
///
/// Note the file itself is **not** created: `tail` tests write their own
/// content, and `quarantine` never touches the source file at all.
pub fn spool_file(dir: &Path, collector: &str, session_id: &str) -> SpoolFile {
    SpoolFile {
        collector: collector.to_string(),
        session_id: session_id.to_string(),
        path: dir.join(af_spool::spool_file_name(collector, session_id)),
    }
}

/// The schema's `event_id` carries `minLength: 16`. Tests want short
/// readable names, so pad them out to a conforming length rather than
/// littering the assertions with ULIDs.
pub fn event_id(name: &str) -> String {
    format!("{name:-<16}")
}

/// A minimal schema-valid `session_meta` line for `sess-1`, identified by
/// `name` (padded by [`event_id`]).
///
/// Written as a literal rather than serialized from an [`af_events::Envelope`]:
/// these tests are about the *tailer's* treatment of bytes on disk, and a
/// line produced by the very serializer under test downstream would not be
/// an independent statement of what a collector actually writes.
pub fn valid_line(name: &str) -> String {
    let event_id = event_id(name);
    format!(
        r#"{{"schema_version":"0.1.0","event_id":"{event_id}","ts":"2026-07-25T00:00:00Z","collector":{{"name":"claude-code","version":"1.0.0"}},"session_id":"sess-1","type":"session_meta","payload":{{"agent_app":{{"name":"claude-code"}}}}}}"#
    )
}
