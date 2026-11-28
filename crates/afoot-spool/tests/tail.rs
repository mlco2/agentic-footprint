use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use afoot_events::RejectReason;
use afoot_spool::{scan, tail, tail_at_most};

mod common;
use common::{event_id, spool_file, valid_line};

#[test]
fn tail_full_lines_from_zero_returns_all_events() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");

    let mut f = fs::File::create(&file.path).unwrap();
    for i in 0..3 {
        writeln!(f, "{}", valid_line(&format!("evt-{i}"))).unwrap();
    }
    drop(f);

    let result = tail(&file, 0).unwrap();

    assert_eq!(result.events.len(), 3);
    assert!(result.rejected.is_empty());
    assert!(!result.truncated);
    assert_eq!(result.new_offset, fs::metadata(&file.path).unwrap().len());
}

#[test]
fn tail_does_not_consume_a_trailing_partial_line() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");

    // Write a half-finished line with no trailing newline.
    let partial = &valid_line("evt-partial")[..20];
    fs::write(&file.path, partial).unwrap();

    let result = tail(&file, 0).unwrap();
    assert_eq!(result.events.len(), 0);
    assert!(result.rejected.is_empty());
    assert_eq!(
        result.new_offset, 0,
        "offset must stay at the line boundary"
    );

    // Complete the line and tail again from the unchanged offset.
    let mut f = OpenOptions::new().append(true).open(&file.path).unwrap();
    let rest = &valid_line("evt-partial")[20..];
    writeln!(f, "{rest}").unwrap();
    drop(f);

    let result2 = tail(&file, result.new_offset).unwrap();
    assert_eq!(result2.events.len(), 1);
    assert_eq!(result2.events[0].event_id, event_id("evt-partial"));
    assert!(!result2.truncated);
    assert_eq!(result2.new_offset, fs::metadata(&file.path).unwrap().len());
}

#[test]
fn tail_routes_malformed_line_to_rejected_and_advances_offset() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");

    let mut f = fs::File::create(&file.path).unwrap();
    writeln!(f, "not valid json at all").unwrap();
    writeln!(f, "{}", valid_line("evt-ok")).unwrap();
    drop(f);

    let result = tail(&file, 0).unwrap();

    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0].event_id, event_id("evt-ok"));
    assert_eq!(result.rejected.len(), 1);
    assert_eq!(result.rejected[0].line, "not valid json at all");
    assert!(matches!(result.rejected[0].reason, RejectReason::Json(_)));
    assert_eq!(result.rejected[0].byte_offset, 0);
    assert_eq!(result.rejected[0].line_number, 1);
    assert_eq!(result.new_offset, fs::metadata(&file.path).unwrap().len());
}

#[test]
fn tail_preserves_unknown_event_types_without_rejecting_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "future", "sess-opaque");
    let line = r#"{"schema_version":"0.1.0","event_id":"opaque-event-0001","ts":"2026-07-25T00:00:00Z","collector":{"name":"future","version":"1.0.0"},"session_id":"sess-opaque","type":"future_fact","payload":{"new_field":42}}"#;
    fs::write(&file.path, format!("{line}\n")).unwrap();

    let result = tail(&file, 0).unwrap();
    assert!(result.events.is_empty());
    assert!(result.rejected.is_empty());
    assert_eq!(result.opaque_events.len(), 1);
    assert_eq!(result.opaque_events[0].type_tag, "future_fact");
    assert_eq!(result.new_offset, fs::metadata(&file.path).unwrap().len());
}

/// A reject found by a *resumed* tail must still be located against the
/// whole file: an offset and a line number relative to where this read
/// happened to start would point at the wrong line in the editor the
/// developer opens next.
#[test]
fn tail_locates_rejects_against_the_whole_file_not_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");

    let mut f = fs::File::create(&file.path).unwrap();
    writeln!(f, "{}", valid_line("evt-1")).unwrap();
    writeln!(f, "{}", valid_line("evt-2")).unwrap();
    drop(f);

    let first = tail(&file, 0).unwrap();
    assert!(first.rejected.is_empty());

    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&file.path)
        .unwrap();
    writeln!(f, "{}", valid_line("evt-3")).unwrap();
    writeln!(f, "definitely not json").unwrap();
    drop(f);

    let second = tail(&file, first.new_offset).unwrap();

    assert_eq!(second.rejected.len(), 1);
    assert_eq!(second.rejected[0].line, "definitely not json");
    assert_eq!(second.rejected[0].line_number, 4);
    // Offset of the bad line's first byte within the file: everything
    // before it, including the newly appended third line.
    let expected_offset =
        fs::metadata(&file.path).unwrap().len() - "definitely not json\n".len() as u64;
    assert_eq!(second.rejected[0].byte_offset, expected_offset);
}

#[test]
fn tail_skips_empty_lines_without_rejecting() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");

    let mut f = fs::File::create(&file.path).unwrap();
    writeln!(f).unwrap(); // empty line
    writeln!(f, "{}", valid_line("evt-1")).unwrap();
    drop(f);

    let result = tail(&file, 0).unwrap();

    assert_eq!(result.events.len(), 1);
    assert!(result.rejected.is_empty());
    assert_eq!(result.new_offset, fs::metadata(&file.path).unwrap().len());
}

#[test]
fn tail_from_offset_beyond_len_restarts_at_zero_and_flags_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");

    let mut f = fs::File::create(&file.path).unwrap();
    writeln!(f, "{}", valid_line("evt-1")).unwrap();
    drop(f);

    let len = fs::metadata(&file.path).unwrap().len();
    let result = tail(&file, len + 1000).unwrap();

    assert!(result.truncated);
    assert_eq!(result.events.len(), 1);
    assert_eq!(result.new_offset, len);
}

#[test]
fn scan_parses_collector_and_session_id_from_filename() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("claude-code.sess-abc.jsonl"), "").unwrap();
    // session id containing a dot
    fs::write(dir.path().join("otlp-cc.sess.with.dots.jsonl"), "").unwrap();
    // ignored: wrong extension
    fs::write(dir.path().join("claude-code.sess-abc.txt"), "").unwrap();
    // ignored: no session id
    fs::write(dir.path().join("noseparator.jsonl"), "").unwrap();

    let mut files = scan(dir.path());
    files.sort_by(|a, b| a.session_id.cmp(&b.session_id));

    assert_eq!(files.len(), 2);
    assert_eq!(files[0].collector, "claude-code");
    assert_eq!(files[0].session_id, "sess-abc");
    assert_eq!(files[1].collector, "otlp-cc");
    assert_eq!(files[1].session_id, "sess.with.dots");
}

/// The two halves of the filename grammar must be each other's inverse:
/// [`scan`] only ever sees names some writer produced, so a writer and a
/// reader that disagreed would silently lose a whole collector's spool.
#[test]
fn spool_file_name_and_parse_spool_filename_round_trip() {
    for (collector, session_id) in [
        ("claude-code", "sess-abc"),
        // Session ids may contain dots; collector names may not, which is
        // what makes splitting on the *first* dot unambiguous.
        ("otlp-cc", "sess.with.dots"),
    ] {
        let name = afoot_spool::spool_file_name(collector, session_id);
        let parsed = afoot_spool::parse_spool_filename(&name, PathBuf::from(&name))
            .unwrap_or_else(|| panic!("{name} must parse back"));
        assert_eq!(parsed.collector, collector);
        assert_eq!(parsed.session_id, session_id);
    }

    // Names outside the grammar are filtered, not errors.
    assert!(afoot_spool::parse_spool_filename("noseparator.jsonl", PathBuf::new()).is_none());
    assert!(afoot_spool::parse_spool_filename("a.b.txt", PathBuf::new()).is_none());
}

#[cfg(unix)]
#[test]
fn targeted_path_accepts_an_alias_of_the_same_spool_directory() {
    use std::os::unix::fs::symlink;

    use afoot_spool::spool_file_from_path;

    let dir = tempfile::tempdir().unwrap();
    let spool = dir.path().join("spool");
    fs::create_dir(&spool).unwrap();
    let alias = dir.path().join("spool-alias");
    symlink(&spool, &alias).unwrap();

    let real = spool.join(afoot_spool::spool_file_name("collector", "session"));
    fs::write(&real, "").unwrap();
    let notified = alias.join(real.file_name().unwrap());

    let parsed = spool_file_from_path(&spool, &notified).expect("aliased direct child");
    assert_eq!(parsed.collector, "collector");
    assert_eq!(parsed.session_id, "session");
}

#[test]
fn a_backlog_is_read_in_bounded_pieces_that_together_lose_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");
    let mut f = fs::File::create(&file.path).unwrap();
    for i in 0..5 {
        writeln!(f, "{}", valid_line(&format!("evt-{i}"))).unwrap();
    }
    drop(f);
    let line = valid_line("evt-0").len() as u64 + 1;

    // Room for one line and part of the next.
    let limit = line + line / 2;
    let (mut offset, mut seen, mut calls) = (0, Vec::new(), 0);
    loop {
        let result = tail_at_most(&file, offset, limit).unwrap();
        assert!(
            result.bytes_read <= limit,
            "never more than the limit at once"
        );
        seen.extend(result.events.iter().map(|event| event.event_id.clone()));
        offset = result.new_offset;
        calls += 1;
        if !result.more {
            break;
        }
    }
    assert_eq!(calls, 5);
    assert_eq!(
        seen,
        (0..5)
            .map(|i| event_id(&format!("evt-{i}")))
            .collect::<Vec<_>>()
    );
    assert_eq!(offset, fs::metadata(&file.path).unwrap().len());
}

#[test]
fn a_line_longer_than_the_limit_is_rejected_and_the_next_one_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");
    let mut f = fs::File::create(&file.path).unwrap();
    writeln!(f, "{}", valid_line("evt-before")).unwrap();
    let before = valid_line("evt-before").len() as u64 + 1;
    writeln!(f, "{}", "x".repeat(5000)).unwrap();
    writeln!(f, "{}", valid_line("evt-after")).unwrap();
    drop(f);

    let limit = 1000;
    let first = tail_at_most(&file, 0, limit).unwrap();
    assert_eq!(first.events.len(), 1);
    assert!(first.more);

    let second = tail_at_most(&file, first.new_offset, limit).unwrap();
    assert_eq!(second.rejected.len(), 1);
    assert_eq!(second.rejected[0].byte_offset, before);
    assert_eq!(second.rejected[0].line_number, 2);
    assert!(
        second.rejected[0].line.len() < 1000,
        "the oversized line itself is not kept"
    );
    assert!(second.rejected[0]
        .reason
        .to_string()
        .contains("longer than"));
    assert_eq!(second.new_offset, before + 5001);
    assert!(second.more);

    let third = tail_at_most(&file, second.new_offset, limit).unwrap();
    assert_eq!(third.events[0].event_id, event_id("evt-after"));
    assert!(!third.more);
}

#[test]
fn an_unfinished_line_longer_than_the_limit_is_left_for_later() {
    let dir = tempfile::tempdir().unwrap();
    let file = spool_file(dir.path(), "claude-code", "sess-1");
    fs::write(&file.path, "x".repeat(5000)).unwrap();

    let result = tail_at_most(&file, 0, 1000).unwrap();
    assert_eq!(result.new_offset, 0);
    assert!(result.rejected.is_empty());
    assert!(result.partial_line);
    assert!(!result.more, "nothing a second call could do now");
}
