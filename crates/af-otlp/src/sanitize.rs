//! Reducing an untrusted id to one safe path component.
//!
//! The OTLP receiver takes `session.id` from an exporter's log attributes
//! and builds a spool filename out of it. That value is not ours: it is
//! whatever the agent process put on the wire, and anything that can reach
//! `127.0.0.1:4318` can put anything there. Unsanitized,
//! `session.id = "../../../../../.zshrc"` makes the receiver append JSON to
//! a file outside the spool.
//!
//! The rule is the same one `collectors/claude-code/af-hook.sh`'s
//! `sanitize_id()` applies, deliberately: two collectors that disagree
//! about what a session id may contain produce two different filenames for
//! the same session, and the join silently sees two sessions. Keep these in
//! step — the shim is the reference implementation, this is its mirror.

/// Strips every character outside `[A-Za-z0-9._-]`, then guards the empty
/// string and a leading `.`.
///
/// Removing `/` is what makes traversal impossible: a stripped id can never
/// contain a path separator. The leading-dot guard covers both hidden files
/// and the literal `..` that survives the strip intact.
///
/// Idempotent, so applying it twice (at extraction and again at the point a
/// filename is built) costs nothing and keeps the guarantee local to both.
pub fn sanitize_id(raw: &str) -> String {
    let mut clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    if clean.is_empty() || clean.starts_with('.') {
        clean.insert(0, 'x');
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_session_id_is_left_alone() {
        assert_eq!(
            sanitize_id("4848dec5-894b-43c9-806a-b7991cb5b216"),
            "4848dec5-894b-43c9-806a-b7991cb5b216"
        );
        assert_eq!(sanitize_id("sess_1.2-3"), "sess_1.2-3");
    }

    /// The attack this exists for: any client that can reach the receiver
    /// chooses this string.
    #[test]
    fn traversal_attempts_cannot_survive_as_path_separators() {
        for attempt in [
            "../../../../etc/cron.d/pwned",
            "..%2F..%2Fetc",
            "/etc/passwd",
            "sess/../../evil",
            "..",
            "../",
        ] {
            let clean = sanitize_id(attempt);
            assert!(
                !clean.contains('/') && !clean.contains('\\'),
                "{attempt:?} sanitized to {clean:?}, which still has a separator"
            );
            assert!(
                !clean.starts_with('.'),
                "{attempt:?} sanitized to {clean:?}, which is still dot-prefixed"
            );
            assert_ne!(clean, "..");
        }
    }

    #[test]
    fn an_id_that_sanitizes_to_nothing_still_yields_a_filename() {
        assert_eq!(sanitize_id(""), "x");
        assert_eq!(sanitize_id("///"), "x");
        assert_eq!(sanitize_id("日本語"), "x");
    }

    #[test]
    fn sanitizing_is_idempotent() {
        for raw in ["", "..", "../../x", "normal-id", ".hidden", "日本語"] {
            let once = sanitize_id(raw);
            assert_eq!(sanitize_id(&once), once, "not idempotent for {raw:?}");
        }
    }

    /// Kept in step with `collectors/claude-code/af-hook.sh`'s
    /// `sanitize_id()`: `tr -cd 'A-Za-z0-9._-'` then the empty/leading-dot
    /// guard.
    #[test]
    fn the_character_class_matches_the_hook_shims() {
        assert_eq!(sanitize_id("a Z0._-"), "aZ0._-");
        assert_eq!(sanitize_id("a\tb\nc"), "abc");
        assert_eq!(sanitize_id("a:b;c|d*e?f"), "abcdef");
    }
}
