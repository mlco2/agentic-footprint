//! af-spool: incremental JSONL spool reader with offset tracking and
//! quarantine of malformed lines.
//!
//! Collectors append single-line Contract #1 events to files under the
//! spool directory named `<collector>.<session_id>.jsonl`.
//! [`scan`] discovers those files, [`tail`] reads new
//! complete lines from a byte offset using [`af_events::parse_line`], and
//! [`quarantine`] archives lines that fail to parse.

mod reject;
mod tail;

pub use reject::{quarantine, quarantine_bytes};
pub use tail::{tail, RejectedLine, TailResult};

use std::path::{Path, PathBuf};

/// A spool file discovered on disk, parsed from the filename grammar
/// `<collector>.<session_id>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolFile {
    pub collector: String,
    pub session_id: String,
    pub path: PathBuf,
}

/// Scans `spool_dir` (non-recursively) for files matching
/// `<collector>.<session_id>.jsonl`.
///
/// Collector names never contain a dot, but session ids may, so the
/// collector is everything before the *first* `.` and the session id is
/// everything between that and the trailing `.jsonl`. Entries that don't
/// match this grammar (directories, wrong extension, no `.` separator) are
/// silently skipped — that's filtering, not an error. A missing or
/// unreadable `spool_dir` yields an empty result.
pub fn scan(spool_dir: &Path) -> Vec<SpoolFile> {
    let Ok(entries) = std::fs::read_dir(spool_dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let path = entry.path();
            let file_name = path.file_name()?.to_str()?;
            parse_spool_filename(file_name, path.clone())
        })
        .collect()
}

/// The spool filename for `(collector, session_id)`:
/// `<collector>.<session_id>.jsonl`, the inverse of
/// [`parse_spool_filename`].
///
/// Writers use this rather than formatting the name themselves, so the
/// grammar [`scan`] parses is defined in exactly one place. Neither
/// component is sanitized here: this crate has no view of where the two
/// strings came from, and a writer whose session id is attacker-controlled
/// (the OTLP receiver's is) must reduce it to a safe path component at its
/// own trust boundary — the guarantee belongs next to the risk.
pub fn spool_file_name(collector: &str, session_id: &str) -> String {
    format!("{collector}.{session_id}.jsonl")
}

/// Parses `file_name` as `<collector>.<session_id>.jsonl`, pairing the
/// result with `path`. `None` for anything that doesn't match the grammar.
///
/// Collector names never contain a dot, but session ids may, so the split
/// is on the *first* `.` — see [`scan`].
pub fn parse_spool_filename(file_name: &str, path: PathBuf) -> Option<SpoolFile> {
    let rest = file_name.strip_suffix(".jsonl")?;
    let (collector, session_id) = rest.split_once('.')?;
    if collector.is_empty() || session_id.is_empty() {
        return None;
    }
    Some(SpoolFile {
        collector: collector.to_string(),
        session_id: session_id.to_string(),
        path,
    })
}

/// Parses one direct child of `spool_dir` as a spool file.
///
/// Filesystem notifications can contain directory paths, removed paths, and
/// unrelated files. Keeping this boundary in `af-spool` means targeted
/// ingestion uses exactly the same filename grammar as a full [`scan`]
/// without performing another directory walk.
pub fn spool_file_from_path(spool_dir: &Path, path: &Path) -> Option<SpoolFile> {
    let parent = path.parent()?;
    let same_parent = parent == spool_dir
        || parent
            .canonicalize()
            .ok()
            .zip(spool_dir.canonicalize().ok())
            .is_some_and(|(actual, expected)| actual == expected);
    if !same_parent || !path.is_file() {
        return None;
    }
    let file_name = path.file_name()?.to_str()?;
    parse_spool_filename(file_name, path.to_path_buf())
}
