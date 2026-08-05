//! Pure mapping from control-plane state to the `/debug` wire shapes
//! defined in `docs/contracts/debug-console/DATA-CONTRACT.md` §2.
//!
//! Everything here is a *rendering* of a value some other module already
//! computed. Nothing in this file may compute an impact, an allocation or a
//! share: the console computes nothing (DATA-CONTRACT §0) and neither does
//! the layer that feeds it — that would fork the methodology into a second
//! implementation whose numbers could disagree with `af report`'s. The
//! allocation traces come from `af_core::apportion_traced`, produced inside
//! the very loop that apportions, and are only reshaped here.
//!
//! The one vocabulary this module *owns* is the decision line: [`Decision`]
//! is rendered both as the `--debug` stderr line
//! and as the debug console contract's §2.3
//! `decision` SSE frame, from one value, so the terminal and the console
//! can never drift apart.

use serde_json::{json, Map, Value};

use af_core::{claims_pid, Policy, SampleTrace, SessionTree, Span, POLICY_NONE};
use af_events::{EnergySample, Envelope, Payload, ProcessSample};

/// The four decision kinds, mapped 1:1
/// onto DATA-CONTRACT §2.3's `decision.kind` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    Ingest,
    SpanOpen,
    Attr,
    Orphan,
}

impl DecisionKind {
    /// The `[…]` prefix of the stderr line.
    pub fn prefix(self) -> &'static str {
        match self {
            DecisionKind::Ingest => "ingest",
            DecisionKind::SpanOpen => "span open",
            DecisionKind::Attr => "attr",
            DecisionKind::Orphan => "orphan",
        }
    }

    /// The `decision.kind` value on the wire.
    pub fn wire(self) -> &'static str {
        match self {
            DecisionKind::Ingest => "ingest",
            DecisionKind::SpanOpen => "span_open",
            DecisionKind::Attr => "attr",
            DecisionKind::Orphan => "orphan",
        }
    }
}

/// One decision, in the single form both surfaces are rendered from.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub kind: DecisionKind,
    pub ts: String,
    pub text: String,
    /// The `event_id`/`span_id` the line is about, so the console's log is
    /// clickable. `None` only when the decision is about no single record.
    pub reference: Option<String>,
}

impl Decision {
    /// `[prefix] timestamp: description`.
    pub fn stderr_line(&self) -> String {
        format!("[{}] {}: {}", self.kind.prefix(), self.ts, self.text)
    }

    /// DATA-CONTRACT §2.3 `decision` frame.
    pub fn frame(&self) -> Value {
        let mut map = Map::new();
        map.insert("kind".into(), json!(self.kind.wire()));
        map.insert("ts".into(), json!(self.ts));
        map.insert("text".into(), json!(self.text));
        if let Some(reference) = &self.reference {
            map.insert("ref".into(), json!(reference));
        }
        Value::Object(map)
    }
}

/// The `[ingest]` decision for one freshly-read event.
///
/// One line per event, summarising the fields that identify it — enough to
/// answer "is the spool receiving the events I expect, well-formed?", which
/// is the question this stream exists for.
pub fn ingest_decision(event: &Envelope) -> Decision {
    let text = match &event.payload {
        Payload::LlmCall(call) => format!(
            "llm_call {} in={} out={} src={} session={}",
            call.model_id_requested,
            call.usage
                .input_tokens
                .map(|n| n.to_string())
                .as_deref()
                .unwrap_or("?"),
            call.usage
                .output_tokens
                .map(|n| n.to_string())
                .as_deref()
                .unwrap_or("?"),
            enum_str(serde_json::to_value(call.usage_source), ""),
            event.session_id,
        ),
        Payload::ActionSpan(span) => format!(
            "action_span {} ({}) {}→{} session={}",
            span.tool_name,
            enum_str(serde_json::to_value(span.tool_kind), "other"),
            span.t_start,
            span.t_end,
            event.session_id,
        ),
        Payload::EnergySample(sample) => format!(
            "energy_sample {:.3}J over {}→{} session={}",
            af_core::sample_energy_j(sample),
            sample.t_start,
            sample.t_end,
            event.session_id,
        ),
        Payload::ProcessSample(sample) => format!(
            "process_sample {} tree(s) over {}→{} session={}",
            sample.processes.len(),
            sample.t_start,
            sample.t_end,
            event.session_id,
        ),
        Payload::SessionMeta(meta) => format!(
            "session_meta {} {} session={}",
            meta.agent_app.name,
            meta.os.as_deref().unwrap_or("os=?"),
            event.session_id,
        ),
    };
    Decision {
        kind: DecisionKind::Ingest,
        ts: event.ts.clone(),
        text,
        reference: Some(event.event_id.clone()),
    }
}

/// The `[span open]` decision for an `action_span`.
///
/// The Claude Code hook collector emits a span only once it has closed, so
/// in this PoC "open" means "the watchdog learned about this span's pid
/// tree", not "the tool started" — the honest reading of a close-only
/// collector. `pids` is what the span itself declared; a span that declared
/// none inherits the session's root tree at attribution time and says so.
pub fn span_open_decision(event: &Envelope, span: &af_events::ActionSpan) -> Decision {
    let pids = match &span.pids {
        Some(pids) if !pids.is_empty() => pids
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(","),
        _ => "root-tree (span declared none)".to_string(),
    };
    Decision {
        kind: DecisionKind::SpanOpen,
        ts: event.ts.clone(),
        text: format!("{} pid={} span={}", span.tool_name, pids, span.span_id),
        reference: Some(span.span_id.clone()),
    }
}

/// The `[attr]` decision for one apportioned energy sample:
/// `sample 5.1J → span x:3.2 idle:1.9`.
pub fn attr_decision(trace: &SampleTrace) -> Decision {
    let mut parts: Vec<String> = trace
        .rows
        .iter()
        .filter(|row| !row.excluded)
        .map(|row| format!("span {}:{:.1}", row.span_id, row.allocated_j))
        .collect();
    if trace.orphaned_j > 0.0 {
        parts.push(format!("orphan:{:.1}", trace.orphaned_j));
    }
    parts.push(format!("idle:{:.1}", trace.baseline_j));
    Decision {
        kind: DecisionKind::Attr,
        ts: trace.t_end.clone(),
        text: format!(
            "sample {:.1}J → {} [{}]",
            trace.total_j,
            parts.join(" "),
            trace.policy_id(),
        ),
        reference: None,
    }
}

/// The `[orphan]` decision: cpu-time observed for a pid tree that belongs
/// to no live span — either explicitly tagged by the sampler's orphan tail
/// or claimed by nobody in the window.
pub fn orphan_decision(trace: &SampleTrace) -> Decision {
    Decision {
        kind: DecisionKind::Orphan,
        ts: trace.t_end.clone(),
        text: format!(
            "{:.3}J of unclaimed compute in {}→{} (tree outlived its span, or no span claimed it)",
            trace.orphaned_j, trace.t_start, trace.t_end,
        ),
        reference: None,
    }
}

/// DATA-CONTRACT §2.4 allocation trace for one energy sample.
///
/// Two fields deserve their doc comments read before the numbers are trusted:
///
/// * `denominator_cpu_ms` is the window's wall length, i.e. one core-second
///   per second (`l2_cpu_time/v1`'s single-core normalization). It is *not*
///   Σ watched cpu — dividing by that would make attributed + orphaned ≡
///   100% and erase the baseline/idle remainder by construction, which is
///   the failure mode §2.4 explicitly warns about.
/// * `agent_process.allocated_j` carries the **orphan bucket**: joules of
///   observed compute that no span claimed. `l2_cpu_time/v1` has no
///   separate agent-process bucket — under root-tree inheritance the agent
///   tree's cpu is claimed by pid-less spans while they run and falls to
///   the orphan bucket when none does — so this is the honest occupant of
///   the "neither a span nor idle" slot, and it keeps the trace's
///   arithmetic closed: Σ rows + agent_process + baseline == total_j.
pub fn alloc_trace(
    sample_event_id: &str,
    session_id: &str,
    sample: &EnergySample,
    trace: &SampleTrace,
    root_pid: Option<i64>,
) -> Value {
    let rows: Vec<Value> = trace
        .rows
        .iter()
        .map(|row| {
            json!({
                "span_id": row.span_id,
                "tool_name": row.tool_name,
                "execution_locus": serde_json::to_value(row.locus).unwrap_or(Value::Null),
                "overlap_ms": row.overlap_ms,
                "cpu_delta_ms": row.cpu_delta_ms,
                "share": share_of(row.allocated_j, trace.total_j),
                "allocated_j": row.allocated_j,
                "l1_allocated_j": row.l1_allocated_j,
                "excluded": row.excluded,
                "excluded_reason": row.excluded_reason,
            })
        })
        .collect();

    let components: Vec<Value> = sample
        .components
        .iter()
        .map(|component| {
            json!({
                "kind": serde_json::to_value(component.kind).unwrap_or(Value::Null),
                "label": component.label,
                "energy_j": component.energy_j,
                "method": serde_json::to_value(component.method).unwrap_or(Value::Null),
            })
        })
        .collect();

    json!({
        "sample_event_id": sample_event_id,
        "session_id": session_id,
        "t_start": trace.t_start,
        "t_end": trace.t_end,
        "total_j": trace.total_j,
        "components": components,
        "attribution_policy": policy_schema_name(trace.policy),
        "attribution_policy_id": trace.policy_id(),
        "denominator_cpu_ms": trace.denominator_cpu_ms,
        "denominator_note": "wall-clock ms of the window: l2_cpu_time/v1 normalizes cpu-time \
                             against one core-second per second, never against the sum of the \
                             watched trees",
        "rows": rows,
        "agent_process": {
            "pid": root_pid.unwrap_or(0),
            "cpu_delta_ms": trace.root_cpu_delta_ms,
            "allocated_j": trace.orphaned_j,
            "note": "orphaned/unclaimed compute: l2_cpu_time/v1 has no separate agent-process \
                     bucket, so this is observed cpu that no span claimed (including the agent's \
                     own tree while no span was running, and any orphan tail)",
        },
        "baseline": {
            "allocated_j": trace.baseline_j,
            "share": share_of(trace.baseline_j, trace.total_j),
            "label": "baseline/idle",
        },
        "l1_shadow_sum_share": trace.l1_shadow_sum_share,
    })
}

/// `attribution_policy` carries the *unversioned* schema enum value while
/// `attribution_policy_id` carries the full `…/vN` id — the same pairing
/// `impact_join` records use.
///
/// The stripping rule itself lives in [`Policy::schema_name`], next to the
/// ids it strips, rather than being re-derived from the printed id here:
/// two places that split on `/` are two places that can disagree about what
/// the schema enum is called.
pub fn policy_schema_name(policy: Option<Policy>) -> &'static str {
    match policy {
        Some(policy) => policy.schema_name(),
        None => POLICY_NONE,
    }
}

fn share_of(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        part / whole
    } else {
        0.0
    }
}

/// A serde-serialized enum's wire spelling, with the fallback for the
/// (unreachable) case where it did not serialize to a string.
///
/// `fallback` is a **per-site** argument on purpose: the callers disagree
/// about what an unrenderable enum should read as — an unstated usage
/// source is empty, an unrecognised tool kind is `other`, which is a real
/// member of that schema's enum — and collapsing them onto one default
/// would change what goes on the wire at one of the two sites.
fn enum_str(serialized: serde_json::Result<Value>, fallback: &str) -> String {
    serialized
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| fallback.to_string())
}

/// DATA-CONTRACT §2.5 watchdog list, derived from the most recent
/// `process_sample` of the session.
///
/// Every field is an observation, not an inference: `cpu_pct` is the
/// reported cpu delta over the window the sampler itself declared,
/// `rss_bytes` is what psutil read, and `state` comes from the sampler's
/// own `orphan_of` tag and the session's root pid set. `cmd` is the *span's
/// tool name*, not a process command line — no collector in this PoC
/// reports argv, and inventing one would be a fabricated observation.
pub fn watchdog_entries(sample: &ProcessSample, tree: &SessionTree) -> Vec<Value> {
    let window_ms = match (
        af_core::parse_ts(&sample.t_start),
        af_core::parse_ts(&sample.t_end),
    ) {
        (Some(t0), Some(t1)) if t1 > t0 => (t1 - t0) as f64,
        _ => 0.0,
    };
    let window_end = af_core::parse_ts(&sample.t_end);

    sample
        .processes
        .iter()
        .map(|process| {
            // One question asked once. `orphan_of` used to be re-tested at
            // four separate points — for the owner lookup, the state, the
            // span id and the `orphaned_since` pair — which is four places
            // that had to keep agreeing about what an orphan is.
            let (owner, state): (Option<&Span>, &str) = match &process.orphan_of {
                Some(span_id) => (
                    tree.spans.iter().find(|s| &s.span_id == span_id),
                    "orphaned",
                ),
                None => {
                    let owner = tree
                        .spans
                        .iter()
                        .find(|s| claims_pid(s, process.pid, &tree.root_pids));
                    let state = if tree.root_pids.contains(&process.pid) && owner.is_none() {
                        "agent"
                    } else {
                        "open"
                    };
                    (owner, state)
                }
            };
            let orphaned = process.orphan_of.is_some();

            let mut entry = Map::new();
            entry.insert("pid".into(), json!(process.pid));
            entry.insert(
                "span_id".into(),
                json!(process
                    .orphan_of
                    .clone()
                    .or_else(|| owner.map(|s| s.span_id.clone()))
                    .unwrap_or_default()),
            );
            entry.insert(
                "cmd".into(),
                json!(owner
                    .map(|s| s.tool_name.clone())
                    .unwrap_or_else(|| "unknown (no command line observed)".to_string())),
            );
            entry.insert(
                "cpu_pct".into(),
                json!(if window_ms > 0.0 {
                    process.cpu_time_delta_ms as f64 / window_ms * 100.0
                } else {
                    0.0
                }),
            );
            entry.insert(
                "rss_bytes".into(),
                json!(process.memory_rss_bytes.unwrap_or(0)),
            );
            entry.insert("state".into(), json!(state));

            if let (true, Some(span), Some(end)) = (orphaned, owner, window_end) {
                entry.insert(
                    "orphaned_since".into(),
                    json!(af_core::rfc3339_ms(span.t_end).unwrap_or_default()),
                );
                entry.insert(
                    "outlived_span_by_ms".into(),
                    json!((end - span.t_end).max(0)),
                );
            }
            Value::Object(entry)
        })
        .collect()
}

/// Bytes of a rejected line carried in its frame.
///
/// A reject frame exists so a developer can see *what* failed to parse; the
/// full text is on disk in `rejected/` for anyone who needs it. Without a
/// cap the frame is as large as the line, and a rejected line has no size
/// limit by definition — a multi-megabyte transcript blob appended by a
/// misconfigured collector would be copied into the ring, held for the ring
/// window, and fanned out to every SSE subscriber. 512 bytes is enough to
/// recognise the shape of a line and identify its collector.
const RAW_LINE_LIMIT: usize = 512;

/// DATA-CONTRACT §2.3 `reject` frame.
///
/// `raw` is capped at [`RAW_LINE_LIMIT`] and `raw_truncated` states whether
/// it was, so the console never presents a clipped line as a whole one.
pub fn reject_frame(record: &super::ingest::RejectRecord) -> Value {
    let (raw, truncated) = truncate_raw(&record.raw);
    json!({
        "ts": record.ts,
        "reason": record.reason,
        "origin": record.origin,
        "line": record.line,
        "byte_offset": record.byte_offset,
        "raw": raw,
        "raw_truncated": truncated,
    })
}

/// Clips `raw` to [`RAW_LINE_LIMIT`] **bytes**, at a character boundary.
///
/// The limit is a byte budget — that is what bounds the frame — but slicing
/// a `String` mid-character panics, so the cut backs up to the nearest
/// boundary. A rejected line is arbitrary bytes from a collector we do not
/// control, so this path is reachable by anything that writes UTF-8.
fn truncate_raw(raw: &str) -> (&str, bool) {
    if raw.len() <= RAW_LINE_LIMIT {
        return (raw, false);
    }
    let mut end = RAW_LINE_LIMIT;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    (&raw[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_core::{apportion_traced, correlate};
    use af_events::{
        ActionSpan, Collector, EnergyComponent, EnergyKind, EnergyMethod, ExecutionLocus,
        ProcessDelta, ToolKind,
    };

    fn envelope(event_id: &str, ts: &str, payload: Payload) -> Envelope {
        Envelope {
            schema_version: "0.1.0".into(),
            event_id: event_id.into(),
            ts: ts.into(),
            collector: Collector {
                name: "cc-hooks".into(),
                version: "0.3.1".into(),
            },
            session_id: "sess-1".into(),
            attribution: None,
            payload,
        }
    }

    fn span(span_id: &str, t_start: &str, t_end: &str, pids: Option<Vec<i64>>) -> ActionSpan {
        ActionSpan {
            span_id: span_id.into(),
            tool_name: "Bash".into(),
            tool_kind: ToolKind::Bash,
            execution_locus: ExecutionLocus::Local,
            t_start: t_start.into(),
            t_end: t_end.into(),
            pids,
            cgroup: None,
            status: None,
        }
    }

    fn sample(t_start: &str, t_end: &str, joules: f64) -> EnergySample {
        EnergySample {
            t_start: t_start.into(),
            t_end: t_end.into(),
            components: vec![EnergyComponent {
                kind: EnergyKind::Total,
                label: Some("Apple M4 Pro".into()),
                energy_j: joules,
                method: EnergyMethod::TdpModel,
            }],
            host_id: None,
        }
    }

    #[test]
    fn decision_lines_and_frames_share_one_vocabulary() {
        let decision = Decision {
            kind: DecisionKind::SpanOpen,
            ts: "2026-07-25T14:00:01.500Z".into(),
            text: "Bash pid=42 span=spn-1".into(),
            reference: Some("spn-1".into()),
        };
        assert_eq!(
            decision.stderr_line(),
            "[span open] 2026-07-25T14:00:01.500Z: Bash pid=42 span=spn-1"
        );
        let frame = decision.frame();
        assert_eq!(frame["kind"], json!("span_open"));
        assert_eq!(frame["ref"], json!("spn-1"));
        assert_eq!(frame["text"], json!("Bash pid=42 span=spn-1"));
    }

    #[test]
    fn a_decision_without_a_reference_omits_the_key_rather_than_nulling_it() {
        let decision = Decision {
            kind: DecisionKind::Attr,
            ts: "2026-07-25T14:00:05.000Z".into(),
            text: "sample 5.1J → idle:5.1".into(),
            reference: None,
        };
        assert!(decision.frame().get("ref").is_none());
    }

    #[test]
    fn every_payload_kind_renders_an_ingest_line() {
        let events = vec![
            envelope(
                "e1",
                "2026-07-25T14:00:00Z",
                Payload::ActionSpan(span(
                    "spn-1",
                    "2026-07-25T14:00:00Z",
                    "2026-07-25T14:00:02Z",
                    Some(vec![7]),
                )),
            ),
            envelope(
                "e2",
                "2026-07-25T14:00:02Z",
                Payload::EnergySample(sample("2026-07-25T14:00:00Z", "2026-07-25T14:00:02Z", 10.0)),
            ),
            envelope(
                "e3",
                "2026-07-25T14:00:02Z",
                Payload::ProcessSample(ProcessSample {
                    t_start: "2026-07-25T14:00:00Z".into(),
                    t_end: "2026-07-25T14:00:02Z".into(),
                    processes: vec![],
                }),
            ),
        ];
        for event in &events {
            let decision = ingest_decision(event);
            assert_eq!(decision.kind, DecisionKind::Ingest);
            assert_eq!(decision.reference.as_deref(), Some(event.event_id.as_str()));
            assert!(decision.stderr_line().starts_with("[ingest] "));
            assert!(decision.stderr_line().contains("session=sess-1"));
        }
    }

    #[test]
    fn attr_line_matches_the_design_log_format() {
        let tree = correlate(&[envelope(
            "e1",
            "2026-07-25T14:00:02Z",
            Payload::ActionSpan(span(
                "spn-1",
                "2026-07-25T14:00:00Z",
                "2026-07-25T14:00:02Z",
                Some(vec![7]),
            )),
        )]);
        let samples = vec![sample("2026-07-25T14:00:00Z", "2026-07-25T14:00:02Z", 5.1)];
        let procs = vec![ProcessSample {
            t_start: "2026-07-25T14:00:00Z".into(),
            t_end: "2026-07-25T14:00:02Z".into(),
            processes: vec![ProcessDelta {
                pid: 7,
                cpu_time_delta_ms: 1255,
                orphan_of: None,
                memory_rss_bytes: Some(1024),
                io_read_bytes: None,
                io_write_bytes: None,
            }],
        }];
        let (_, traces) = apportion_traced(&samples, &procs, &tree);
        let line = attr_decision(&traces[0]).stderr_line();
        assert!(line.starts_with("[attr] "), "{line}");
        assert!(
            line.contains("sample 5.1J → span spn-1:3.2 idle:1.9"),
            "{line}"
        );
        assert!(line.contains("l2_cpu_time/v1"), "{line}");
    }

    #[test]
    fn an_alloc_trace_accounts_for_every_joule_of_its_sample() {
        let tree = correlate(&[envelope(
            "e1",
            "2026-07-25T14:00:02Z",
            Payload::ActionSpan(span(
                "spn-1",
                "2026-07-25T14:00:00Z",
                "2026-07-25T14:00:02Z",
                Some(vec![7]),
            )),
        )]);
        let samples = vec![sample(
            "2026-07-25T14:00:00Z",
            "2026-07-25T14:00:02Z",
            84.64,
        )];
        let procs = vec![ProcessSample {
            t_start: "2026-07-25T14:00:00Z".into(),
            t_end: "2026-07-25T14:00:02Z".into(),
            processes: vec![ProcessDelta {
                pid: 7,
                cpu_time_delta_ms: 500,
                orphan_of: None,
                memory_rss_bytes: Some(1024),
                io_read_bytes: None,
                io_write_bytes: None,
            }],
        }];
        let (_, traces) = apportion_traced(&samples, &procs, &tree);
        let value = alloc_trace("evt-sample", "sess-1", &samples[0], &traces[0], Some(7));

        assert_eq!(value["sample_event_id"], json!("evt-sample"));
        assert_eq!(value["attribution_policy"], json!("l2_cpu_time"));
        assert_eq!(value["attribution_policy_id"], json!("l2_cpu_time/v1"));
        // §2.4: the denominator is the machine's capacity over the window,
        // never Σ watched cpu (500ms here) — otherwise baseline is zero by
        // construction.
        assert_eq!(value["denominator_cpu_ms"], json!(2000.0));

        let rows_j: f64 = value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["allocated_j"].as_f64().unwrap())
            .sum();
        let total = rows_j
            + value["agent_process"]["allocated_j"].as_f64().unwrap()
            + value["baseline"]["allocated_j"].as_f64().unwrap();
        assert!(
            (total - 84.64).abs() < 1e-9,
            "Σrows + agent + baseline = {total}, expected 84.64"
        );
        assert!(value["baseline"]["allocated_j"].as_f64().unwrap() > 0.0);
        assert_eq!(value["components"][0]["method"], json!("tdp_model"));
    }

    #[test]
    fn a_remote_span_appears_in_the_trace_excluded_and_unpaid() {
        let mut remote = span(
            "spn-remote",
            "2026-07-25T14:00:00Z",
            "2026-07-25T14:00:02Z",
            None,
        );
        remote.execution_locus = ExecutionLocus::Remote;
        remote.tool_name = "WebFetch".into();
        let tree = correlate(&[envelope(
            "e1",
            "2026-07-25T14:00:02Z",
            Payload::ActionSpan(remote),
        )]);
        let samples = vec![sample("2026-07-25T14:00:00Z", "2026-07-25T14:00:02Z", 12.0)];
        let (_, traces) = apportion_traced(&samples, &[], &tree);
        let value = alloc_trace("evt-sample", "sess-1", &samples[0], &traces[0], None);

        let rows = value["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["excluded"], json!(true));
        assert_eq!(rows[0]["allocated_j"], json!(0.0));
        assert!(rows[0]["excluded_reason"]
            .as_str()
            .unwrap()
            .contains("remote"));
        // All of it stayed in baseline; nothing was invented for the remote span.
        assert_eq!(value["baseline"]["allocated_j"], json!(12.0));
    }

    #[test]
    fn concurrent_spans_expose_l1_over_attribution_while_l2_stays_within_the_sample() {
        let events = vec![
            envelope(
                "e1",
                "2026-07-25T14:00:02Z",
                Payload::ActionSpan(span(
                    "spn-a",
                    "2026-07-25T14:00:00Z",
                    "2026-07-25T14:00:02Z",
                    Some(vec![7]),
                )),
            ),
            envelope(
                "e2",
                "2026-07-25T14:00:02Z",
                Payload::ActionSpan(span(
                    "spn-b",
                    "2026-07-25T14:00:00Z",
                    "2026-07-25T14:00:02Z",
                    Some(vec![8]),
                )),
            ),
        ];
        let tree = correlate(&events);
        let samples = vec![sample("2026-07-25T14:00:00Z", "2026-07-25T14:00:02Z", 20.0)];
        let procs = vec![ProcessSample {
            t_start: "2026-07-25T14:00:00Z".into(),
            t_end: "2026-07-25T14:00:02Z".into(),
            processes: vec![
                ProcessDelta {
                    pid: 7,
                    cpu_time_delta_ms: 200,
                    orphan_of: None,
                    memory_rss_bytes: Some(1),
                    io_read_bytes: None,
                    io_write_bytes: None,
                },
                ProcessDelta {
                    pid: 8,
                    cpu_time_delta_ms: 200,
                    orphan_of: None,
                    memory_rss_bytes: Some(1),
                    io_read_bytes: None,
                    io_write_bytes: None,
                },
            ],
        }];
        let (_, traces) = apportion_traced(&samples, &procs, &tree);
        let value = alloc_trace("evt-sample", "sess-1", &samples[0], &traces[0], Some(7));

        assert_eq!(value["l1_shadow_sum_share"], json!(2.0));
        let l2_share_sum: f64 = value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["share"].as_f64().unwrap())
            .sum();
        assert!(l2_share_sum <= 1.0, "L2 shares sum to {l2_share_sum}");
    }

    #[test]
    fn watchdog_entries_report_observed_state_not_guesses() {
        let tree = correlate(&[
            envelope(
                "e1",
                "2026-07-25T14:00:02Z",
                Payload::ActionSpan(span(
                    "spn-1",
                    "2026-07-25T14:00:00Z",
                    "2026-07-25T14:00:02Z",
                    Some(vec![7]),
                )),
            ),
            envelope(
                "e2",
                "2026-07-25T14:00:00Z",
                Payload::ActionSpan(ActionSpan {
                    tool_name: "__session__".into(),
                    tool_kind: ToolKind::Other,
                    ..span(
                        "session-boot",
                        "2026-07-25T14:00:00Z",
                        "2026-07-25T14:00:00Z",
                        Some(vec![99]),
                    )
                }),
            ),
        ]);
        let procs = ProcessSample {
            t_start: "2026-07-25T14:00:00Z".into(),
            t_end: "2026-07-25T14:00:02Z".into(),
            processes: vec![
                ProcessDelta {
                    pid: 7,
                    cpu_time_delta_ms: 1000,
                    orphan_of: None,
                    memory_rss_bytes: Some(2048),
                    io_read_bytes: None,
                    io_write_bytes: None,
                },
                ProcessDelta {
                    pid: 8,
                    cpu_time_delta_ms: 400,
                    orphan_of: Some("spn-1".into()),
                    memory_rss_bytes: Some(64),
                    io_read_bytes: None,
                    io_write_bytes: None,
                },
            ],
        };
        let entries = watchdog_entries(&procs, &tree);

        assert_eq!(entries[0]["pid"], json!(7));
        assert_eq!(entries[0]["state"], json!("open"));
        assert_eq!(entries[0]["span_id"], json!("spn-1"));
        assert_eq!(entries[0]["cpu_pct"], json!(50.0));
        assert_eq!(entries[0]["rss_bytes"], json!(2048));

        assert_eq!(entries[1]["state"], json!("orphaned"));
        assert_eq!(entries[1]["span_id"], json!("spn-1"));
        assert_eq!(
            entries[1]["orphaned_since"],
            json!("2026-07-25T14:00:02.000Z")
        );
        assert_eq!(entries[1]["outlived_span_by_ms"], json!(0));
    }

    fn reject_record(raw: &str) -> super::super::ingest::RejectRecord {
        super::super::ingest::RejectRecord {
            ts: "2026-07-25T14:00:00.000Z".into(),
            reason: "invalid JSON: expected value".into(),
            origin: "cc-hooks.sess-1.jsonl".into(),
            line: 42,
            byte_offset: 1024,
            raw: raw.into(),
        }
    }

    #[test]
    fn a_short_rejected_line_is_carried_whole_and_says_so() {
        let frame = reject_frame(&reject_record("not valid json at all"));
        assert_eq!(frame["raw"], json!("not valid json at all"));
        assert_eq!(frame["raw_truncated"], json!(false));
    }

    /// A rejected line has no size limit — it is whatever a collector
    /// appended. Uncapped, one bad multi-megabyte line is copied into the
    /// frame ring and fanned out to every SSE subscriber.
    #[test]
    fn an_oversized_rejected_line_is_capped_and_flagged() {
        let huge = "x".repeat(RAW_LINE_LIMIT * 40);
        let frame = reject_frame(&reject_record(&huge));

        let raw = frame["raw"].as_str().expect("raw is a string");
        assert_eq!(raw.len(), RAW_LINE_LIMIT);
        assert_eq!(
            frame["raw_truncated"],
            json!(true),
            "the console must never present a clipped line as a whole one"
        );
    }

    /// Truncation is a byte budget, but slicing a `String` mid-character
    /// panics — and the bytes come from a collector we do not control.
    #[test]
    fn truncation_backs_up_to_a_character_boundary() {
        // Every char is 3 bytes, so the limit lands mid-character.
        let multibyte = "€".repeat(RAW_LINE_LIMIT);
        let frame = reject_frame(&reject_record(&multibyte));

        let raw = frame["raw"].as_str().expect("raw is a string");
        assert!(raw.len() <= RAW_LINE_LIMIT);
        assert!(raw.len() > RAW_LINE_LIMIT - 4, "backed up at most one char");
        assert_eq!(frame["raw_truncated"], json!(true));
    }
}
