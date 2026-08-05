use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use af_events::{Envelope, OpaqueEvent, ParsedLine, RejectReason};

use crate::SpoolFile;

/// Outcome of one [`tail`] call.
pub struct TailResult {
    /// Successfully parsed events, in file order.
    pub events: Vec<Envelope>,
    /// Base-envelope-valid events whose type this binary does not understand.
    pub opaque_events: Vec<OpaqueEvent>,
    /// Byte offset to pass as `from_offset` on the next call.
    pub new_offset: u64,
    /// Lines that failed [`af_events::parse_line`], with where they were.
    pub rejected: Vec<RejectedLine>,
    /// `true` when `from_offset` was past the file's current length, so
    /// tailing restarted from offset 0.
    ///
    /// Collectors are append-only and never delete (global constraint), so
    /// this should never happen in practice; it's handled explicitly
    /// (rather than erroring or silently under-reading) so a caller whose
    /// on-disk file was replaced or truncated out from under it — e.g. a
    /// collector restart that recreates its session file — finds out
    /// instead of silently missing events. Re-delivered events are
    /// expected to be deduplicated downstream by `event_id` (the store's
    /// primary key), so truncation recovery is idempotent for consumers
    /// using the standard ingest path.
    pub truncated: bool,
    /// Bytes read from the file after seeking to the effective offset.
    pub bytes_read: u64,
    /// Newline-terminated records encountered, including empty and rejected
    /// records. This is the amount of line-framing work performed.
    pub complete_lines: u64,
    /// Whether bytes remain after the last newline. Those bytes are retained
    /// for the next tail rather than treated as a malformed event.
    pub partial_line: bool,
    /// Wall time spent validating complete non-empty records.
    pub validation_duration: Duration,
}

/// One line that failed to parse, located precisely enough to be found
/// again by hand.
///
/// `byte_offset` and `line_number` exist because a quarantined line is only
/// actionable if the reader can point at it in the original file: the debug
/// console's Health tab renders exactly these three facts (reason, origin,
/// offset) next to the raw text. `line_number` is 1-based and counted from
/// the *start of the file*, not from the tail's starting offset, so it means
/// what a text editor means by "line 846".
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedLine {
    /// The raw line, newline stripped.
    pub line: String,
    /// Why [`af_events::parse_line`] refused it.
    pub reason: RejectReason,
    /// Byte offset of the line's first byte within the file.
    pub byte_offset: u64,
    /// 1-based line number within the file.
    pub line_number: u64,
}

/// Reads newly appended, complete JSONL lines from `file` starting at
/// `from_offset`.
///
/// Each complete line (terminated by `\n`) is handed to
/// [`af_events::parse_line`]: successes land in `events`, failures in
/// `rejected` as a [`RejectedLine`]. A trailing partial line — appended
/// but not yet newline-terminated — is left unconsumed: `new_offset` stops
/// just past the last `\n`, so a later call with that offset picks the
/// completed line up. Lines that are empty once trimmed of their newline
/// are skipped silently (the offset still advances past them); they are
/// not rejects.
///
/// If `from_offset` is beyond the file's current length, tailing restarts
/// from 0 and [`TailResult::truncated`] is set — see its docs.
pub fn tail(file: &SpoolFile, from_offset: u64) -> io::Result<TailResult> {
    let mut f = File::open(&file.path)?;
    let len = f.metadata()?.len();

    let (start_offset, truncated) = if from_offset > len {
        (0, true)
    } else {
        (from_offset, false)
    };

    f.seek(SeekFrom::Start(start_offset))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;

    let mut events = Vec::new();
    let mut opaque_events = Vec::new();
    let mut rejected: Vec<RejectedLine> = Vec::new();
    let mut cursor = 0usize;
    let mut consumed = 0usize;
    // Lines fully consumed from `buf` so far, so a reject can be numbered
    // relative to the start of this read; the prefix before `start_offset`
    // is counted once, below, and only when there is a reject to place.
    let mut lines_in_buf = 0u64;
    let mut validation_duration = Duration::ZERO;

    while let Some(pos) = buf[cursor..].iter().position(|&b| b == b'\n') {
        let line_end = cursor + pos;
        let line_bytes = &buf[cursor..line_end];
        let line_start = cursor;
        let line_index = lines_in_buf;
        cursor = line_end + 1;
        consumed = cursor;
        lines_in_buf += 1;

        if line_bytes.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(line_bytes).into_owned();
        let validation_started = Instant::now();
        let parsed = af_events::parse_line_preserving_unknown(&line);
        validation_duration += validation_started.elapsed();
        match parsed {
            Ok(ParsedLine::Known(envelope)) => events.push(envelope),
            Ok(ParsedLine::Opaque(event)) => opaque_events.push(event),
            Err(reason) => rejected.push(RejectedLine {
                line,
                reason,
                byte_offset: start_offset + line_start as u64,
                // Provisional: relative to `start_offset`. Rebased below.
                line_number: line_index + 1,
            }),
        }
    }

    if !rejected.is_empty() && start_offset > 0 {
        let preceding = count_lines_before(&mut f, start_offset)?;
        for entry in &mut rejected {
            entry.line_number += preceding;
        }
    }

    Ok(TailResult {
        events,
        opaque_events,
        new_offset: start_offset + consumed as u64,
        rejected,
        truncated,
        bytes_read: buf.len() as u64,
        complete_lines: lines_in_buf,
        partial_line: consumed < buf.len(),
        validation_duration,
    })
}

/// Counts newline-terminated lines in `[0, upto)` of `f`.
///
/// Only called when a line was actually rejected, so the cost of re-reading
/// the already-consumed prefix is paid once per quarantined line batch —
/// never on the healthy path. Reads in chunks so a large spool file is not
/// buffered whole just to place one bad line.
fn count_lines_before(f: &mut File, upto: u64) -> io::Result<u64> {
    f.seek(SeekFrom::Start(0))?;
    let mut remaining = upto;
    let mut chunk = vec![0u8; 64 * 1024];
    let mut lines = 0u64;
    while remaining > 0 {
        let want = remaining.min(chunk.len() as u64) as usize;
        let read = f.read(&mut chunk[..want])?;
        if read == 0 {
            break;
        }
        lines += chunk[..read].iter().filter(|&&b| b == b'\n').count() as u64;
        remaining -= read as u64;
    }
    Ok(lines)
}
