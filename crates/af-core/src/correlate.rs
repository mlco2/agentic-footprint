//! Correlation: Contract #1 `action_span` events → a [`SessionTree`] of
//! spans with parsed epoch-millisecond bounds, ready for the energy join in
//! [`crate::attribution`].
//!
//! The tree is deliberately flat for v1 — spans overlap freely (the RFC
//! calls overlap "data", not an anomaly), so a strict parent/child tree
//! would have to invent a nesting the collectors never observed. What the
//! tree adds over the raw events is: parsed timestamps, the pid list
//! hoisted out of its `Option`, the session's root pids (from the
//! collector's zero-length bootstrap span), and a count of events that had
//! to be dropped.

use af_events::{Attribution, Envelope, ExecutionLocus, Payload, ToolKind};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// A span's collector-assigned identifier.
pub type SpanId = String;

/// `tool_name` of the zero-length bootstrap span the Claude Code hook
/// collector emits at `SessionStart`. It carries
/// the agent process' pid but represents no work, so it is kept in the tree
/// and excluded from attribution.
pub const BOOTSTRAP_TOOL_NAME: &str = "__session__";

/// One agent action with timestamps resolved to epoch milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub span_id: SpanId,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    /// Where the action ran. Only [`ExecutionLocus::Remote`] is excluded
    /// from the local energy join; `hybrid`/`unknown` are attributed like
    /// `local` (their local half is real, and dropping them would silently
    /// inflate `baseline_idle_j`).
    pub locus: ExecutionLocus,
    /// Inclusive start, epoch milliseconds.
    pub t_start: i64,
    /// Exclusive end, epoch milliseconds.
    pub t_end: i64,
    /// Root pids of the process trees this span was observed to own. Empty
    /// when the collector could not determine them.
    pub pids: Vec<i64>,
    /// The envelope's optional task/tool-level attribution, carried through
    /// verbatim for downstream grouping.
    pub attribution: Option<Attribution>,
}

impl Span {
    /// Whether this span can receive local energy: it must have positive
    /// duration (which excludes the bootstrap span and any span whose
    /// `t_end <= t_start`) and must not be remote.
    pub fn is_attributable(&self) -> bool {
        self.t_end > self.t_start && self.locus != ExecutionLocus::Remote
    }

    /// Half-open interval overlap test against `[t0, t1)`.
    pub fn overlaps(&self, t0: i64, t1: i64) -> bool {
        self.t_start < t1 && self.t_end > t0
    }
}

/// All spans of one session, plus what correlation could not use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionTree {
    /// Every `action_span` whose timestamps parsed, in event order —
    /// including remote and zero-length spans, which downstream code
    /// filters with [`Span::is_attributable`].
    pub spans: Vec<Span>,
    /// Pids reported by the bootstrap span(s) (the agent process itself),
    /// sorted and deduplicated.
    pub root_pids: Vec<i64>,
    /// `action_span` events dropped because a timestamp was not RFC 3339.
    pub skipped_events: u32,
}

/// Builds a [`SessionTree`] from a session's events. Non-`action_span`
/// events are ignored; callers filter by `session_id` beforehand if the
/// slice spans several sessions.
///
/// An `action_span` whose `t_start` or `t_end` is not parseable RFC 3339 is
/// dropped and counted in [`SessionTree::skipped_events`] — a span with an
/// invented timestamp would silently misattribute real joules.
pub fn correlate(events: &[Envelope]) -> SessionTree {
    let mut tree = SessionTree::default();

    for envelope in events {
        let Payload::ActionSpan(action) = &envelope.payload else {
            continue;
        };
        let (Some(t_start), Some(t_end)) = (parse_ts(&action.t_start), parse_ts(&action.t_end))
        else {
            tree.skipped_events += 1;
            continue;
        };

        let pids = action.pids.clone().unwrap_or_default();
        if action.tool_name == BOOTSTRAP_TOOL_NAME {
            tree.root_pids.extend(pids.iter().copied());
        }

        tree.spans.push(Span {
            span_id: action.span_id.clone(),
            tool_name: action.tool_name.clone(),
            tool_kind: action.tool_kind,
            locus: action.execution_locus,
            t_start,
            t_end,
            pids,
            attribution: envelope.attribution.clone(),
        });
    }

    tree.root_pids.sort_unstable();
    tree.root_pids.dedup();
    tree
}

/// Parses an RFC 3339 timestamp (`Z` or numeric offset, optional fractional
/// seconds) to epoch milliseconds. Returns `None` for anything unparseable
/// or outside `i64` millisecond range.
pub fn parse_ts(raw: &str) -> Option<i64> {
    let parsed = OffsetDateTime::parse(raw, &Rfc3339).ok()?;
    i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).ok()
}

/// Formats epoch milliseconds back to RFC 3339 UTC, **always** with three
/// fractional digits (`2026-07-25T14:00:02.000Z`). The inverse of
/// [`parse_ts`] at the precision this project actually works in. `None`
/// only when `epoch_ms` is outside the representable calendar range.
///
/// Hand-formatted rather than routed through `time`'s `Rfc3339`, which
/// omits the fractional part when it is zero and would therefore emit
/// second precision for exactly the timestamps that land on a whole second
/// — the same string shape being emitted for two different precisions is
/// what makes a derived record's interval ambiguous. Every timestamp this
/// project mints (collector-side and control-plane) is millisecond-precise
/// and fixed-width, so it sorts lexicographically in the same order it
/// sorts chronologically.
pub fn rfc3339_ms(epoch_ms: i64) -> Option<String> {
    let t = OffsetDateTime::from_unix_timestamp_nanos(epoch_ms as i128 * 1_000_000).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utc_offsets_and_fractions_to_the_same_instant() {
        let z = parse_ts("2026-07-25T12:00:00Z").unwrap();
        assert_eq!(parse_ts("2026-07-25T14:00:00+02:00"), Some(z));
        assert_eq!(parse_ts("2026-07-25T12:00:00.250Z"), Some(z + 250));
    }

    #[test]
    fn rejects_non_rfc3339_input() {
        // NB: `time` accepts RFC 3339 §5.6's optional space separator, so
        // "2026-07-25 12:00:00Z" parses — deliberately not asserted here.
        for raw in [
            "",
            "not-a-date",
            "2026-07-25",
            "12:00:00Z",
            "2026-07-25T12:00:00",
        ] {
            assert_eq!(parse_ts(raw), None, "{raw} should not parse");
        }
    }

    #[test]
    fn rfc3339_ms_always_carries_three_fractional_digits() {
        // A whole second is where `time`'s Rfc3339 would drop the fraction
        // and emit a differently-shaped string for the same precision.
        let whole = parse_ts("2026-07-25T14:00:02Z").unwrap();
        assert_eq!(
            rfc3339_ms(whole).as_deref(),
            Some("2026-07-25T14:00:02.000Z")
        );

        // ...and a sub-second instant round-trips through parse_ts.
        let fractional = parse_ts("2026-07-25T14:00:02.007Z").unwrap();
        assert_eq!(
            rfc3339_ms(fractional).as_deref(),
            Some("2026-07-25T14:00:02.007Z")
        );
        assert_eq!(parse_ts(&rfc3339_ms(fractional).unwrap()), Some(fractional));
    }

    #[test]
    fn span_predicates() {
        let span = Span {
            span_id: "s".into(),
            tool_name: "Bash".into(),
            tool_kind: ToolKind::Bash,
            locus: ExecutionLocus::Local,
            t_start: 100,
            t_end: 200,
            pids: vec![],
            attribution: None,
        };
        assert!(span.is_attributable());
        assert!(span.overlaps(150, 250));
        assert!(
            !span.overlaps(200, 300),
            "half-open: touching is not overlap"
        );
        assert!(!span.overlaps(0, 100));

        let backwards = Span {
            t_start: 200,
            t_end: 100,
            ..span.clone()
        };
        assert!(!backwards.is_attributable());

        let remote = Span {
            locus: ExecutionLocus::Remote,
            ..span
        };
        assert!(!remote.is_attributable());
    }
}
