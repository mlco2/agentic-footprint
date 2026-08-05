use std::fs;

use af_events::RejectReason;
use af_spool::quarantine;

mod common;
use common::spool_file;

/// Two rejects for the same source arriving within the same millisecond
/// must not collide: the second write should land in a distinct
/// `-1`-suffixed file rather than truncating the first.
#[test]
fn quarantine_twice_in_the_same_millisecond_produces_two_distinct_files() {
    let dir = tempfile::tempdir().unwrap();
    let rejected_dir = dir.path().join("rejected");
    let source = spool_file(dir.path(), "claude-code", "sess-1");

    let reason = RejectReason::Json("unexpected end of input".to_string());

    // Tight loop to maximize the chance both calls land in the same
    // millisecond (and to exercise the collision path deterministically
    // even if they don't: any pre-existing name is still "taken").
    quarantine(&rejected_dir, &source, "line-one", &reason);
    quarantine(&rejected_dir, &source, "line-two", &reason);

    let entries: Vec<_> = fs::read_dir(&rejected_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        entries.len(),
        2,
        "expected two distinct quarantine files, got {entries:?}"
    );

    // Parse each file name as `claude-code.sess-1.<millis>[-<n>].txt` and
    // pull out the millis part and optional collision suffix, so the
    // assertions below hold whether or not the two writes actually landed
    // in the same millisecond.
    let prefix = "claude-code.sess-1.";
    let parsed: Vec<(String, Option<u32>)> = entries
        .iter()
        .map(|name| {
            let middle = name
                .strip_prefix(prefix)
                .and_then(|s| s.strip_suffix(".txt"))
                .unwrap_or_else(|| panic!("unexpected file name: {name}"));
            match middle.split_once('-') {
                Some((millis, suffix)) => (
                    millis.to_string(),
                    Some(suffix.parse().expect("collision suffix must be numeric")),
                ),
                None => (middle.to_string(), None),
            }
        })
        .collect();

    if parsed[0].0 == parsed[1].0 {
        // Same millisecond: exactly one of the two names must carry the
        // `-1` collision suffix, and the other must be unsuffixed.
        let suffixes: Vec<Option<u32>> = parsed.iter().map(|(_, s)| *s).collect();
        assert!(
            suffixes.contains(&None) && suffixes.contains(&Some(1)),
            "same-millisecond writes must be distinguished by a -1 suffix, got {entries:?}"
        );
    } else {
        // Different milliseconds: no collision suffix was needed.
        assert!(
            parsed.iter().all(|(_, s)| s.is_none()),
            "distinct-millisecond writes should not carry a collision suffix, got {entries:?}"
        );
    }

    // Regardless of naming, both writes must be present with their own
    // content (i.e. neither was truncated/overwritten by the other).
    let contents: Vec<String> = entries
        .iter()
        .map(|name| fs::read_to_string(rejected_dir.join(name)).unwrap())
        .collect();

    let expected_one = "reason: Json(\"unexpected end of input\")\nline-one";
    let expected_two = "reason: Json(\"unexpected end of input\")\nline-two";
    assert!(
        contents.contains(&expected_one.to_string()),
        "missing content for first write: {contents:?}"
    );
    assert!(
        contents.contains(&expected_two.to_string()),
        "missing content for second write: {contents:?}"
    );
}
