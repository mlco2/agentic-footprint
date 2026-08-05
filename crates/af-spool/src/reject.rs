use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use af_events::RejectReason;

use crate::SpoolFile;

/// Archives a line that [`af_events::parse_line`] rejected.
///
/// Writes `rejected_dir/<collector>.<session_id>.<unix_millis>.txt`
/// (creating `rejected_dir` if it doesn't exist yet), whose content is the
/// reason on its own first line (`reason: <Debug of RejectReason>`)
/// followed by the raw, unmodified line as the collector wrote it. If that
/// name is already taken (two rejects landing in the same millisecond), a
/// `-1`, `-2`, ... suffix is inserted before `.txt` until a free name is
/// found, so concurrent rejects never silently overwrite one another.
///
/// Quarantine is best-effort bookkeeping, not part of the event contract:
/// failures (e.g. an unwritable `rejected_dir`) are logged to stderr
/// rather than propagated, so a full disk can't also take down ingestion
/// of well-formed events.
pub fn quarantine(rejected_dir: &Path, source: &SpoolFile, line: &str, reason: &RejectReason) {
    let base_name = format!("{}.{}", source.collector, source.session_id);
    let content = format!("reason: {reason:?}\n{line}");

    if let Err(err) = quarantine_bytes(rejected_dir, &base_name, content.as_bytes()) {
        eprintln!(
            "af-spool: failed to quarantine line from {}.{}: {err}",
            source.collector, source.session_id
        );
    }
}

/// Writes `bytes` to `rejected_dir/<base_name>.<unix_millis>[-N].txt`,
/// creating `rejected_dir` if needed.
///
/// The collision suffix is what makes quarantine safe to call twice in the
/// same millisecond: `create_new` fails rather than truncating, so the loop
/// walks `-1`, `-2`, ... until it finds a free name and no reject can
/// silently overwrite another. `base_name` is everything before the
/// timestamp — `<collector>.<session_id>` for spool rejects, a fixed prefix
/// for whole bodies quarantined by a receiver — so both callers keep their
/// own filename grammar while sharing the one implementation of *when a
/// name is taken*.
///
/// Unlike [`quarantine`] this **propagates** the io error: it is the shared
/// mechanism, and each caller owns the wording of its own best-effort
/// stderr report (which names the source the reader needs to find).
pub fn quarantine_bytes(rejected_dir: &Path, base_name: &str, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(rejected_dir)?;

    let unix_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let stem = format!("{base_name}.{unix_millis}");

    let mut attempt = 0u32;
    loop {
        let file_name = if attempt == 0 {
            format!("{stem}.txt")
        } else {
            format!("{stem}-{attempt}.txt")
        };

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(rejected_dir.join(file_name))
        {
            Ok(mut file) => return file.write_all(bytes),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                attempt += 1;
                continue;
            }
            Err(err) => return Err(err),
        }
    }
}
