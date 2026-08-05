//! Correlation + energy-attribution tests.
//!
//! Every case builds real Contract #1 payloads and runs them through the
//! public `correlate` → `apportion` path, so the tests exercise timestamp
//! parsing and the tree build as well as the apportionment policy itself.
//! Tests for attribution policy `l2_cpu_time` v1.

use std::collections::HashMap;

use af_core::{apportion, correlate, sample_energy_j, Apportionment, Policy, SessionTree};
use af_events::{
    fixtures, ActionSpan, EnergyComponent, EnergyKind, EnergyMethod, EnergySample, Envelope,
    ExecutionLocus, Payload, ProcessSample, ToolKind,
};
use proptest::prelude::*;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// 2026-07-25T12:00:00Z in epoch millis; tests express times as offsets.
const BASE_MS: i64 = 1_785_326_400_000;

/// RFC 3339 timestamp `offset_ms` after [`BASE_MS`].
fn ts(offset_ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos((BASE_MS + offset_ms) as i128 * 1_000_000)
        .expect("representable timestamp")
        .format(&Rfc3339)
        .expect("formattable timestamp")
}

fn envelope(payload: Payload) -> Envelope {
    fixtures::envelope(&fixtures::event_id("evt"), "sess-1", &ts(0), payload)
}

/// An `action_span` envelope with raw (possibly unparseable) timestamps.
fn raw_span(span_id: &str, locus: ExecutionLocus, t_start: &str, t_end: &str) -> Envelope {
    envelope(Payload::ActionSpan(ActionSpan {
        execution_locus: locus,
        ..fixtures::action_span(span_id, t_start, t_end)
    }))
}

fn span_env(
    span_id: &str,
    locus: ExecutionLocus,
    start_ms: i64,
    end_ms: i64,
    pids: Vec<i64>,
) -> Envelope {
    let mut env = raw_span(span_id, locus, &ts(start_ms), &ts(end_ms));
    if let Payload::ActionSpan(action) = &mut env.payload {
        action.pids = Some(pids);
    }
    env
}

/// A `SessionTree` from `(span_id, locus, start_ms, end_ms, pids)` rows.
fn tree(rows: &[(&str, ExecutionLocus, i64, i64, &[i64])]) -> SessionTree {
    let events: Vec<Envelope> = rows
        .iter()
        .map(|(id, locus, s, e, pids)| span_env(id, *locus, *s, *e, pids.to_vec()))
        .collect();
    correlate(&events)
}

/// A one-component (`total`) energy sample, with its window expressed as
/// offsets from [`BASE_MS`].
fn sample(start_ms: i64, end_ms: i64, joules: f64) -> EnergySample {
    fixtures::energy_sample(&ts(start_ms), &ts(end_ms), joules)
}

/// A process sample from `(pid, cpu_time_delta_ms, orphan_of)` rows.
fn procs(start_ms: i64, end_ms: i64, rows: &[(i64, u64, Option<&str>)]) -> ProcessSample {
    fixtures::process_sample(&ts(start_ms), &ts(end_ms), rows)
}

#[track_caller]
fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{what}: expected {expected}, got {actual}"
    );
}

#[track_caller]
fn assert_span_j(out: &Apportionment, span_id: &str, expected: f64) {
    let actual = out.per_span_j.get(span_id).copied().unwrap_or(0.0);
    assert_close(actual, expected, &format!("span {span_id}"));
}

#[track_caller]
fn assert_conserved(out: &Apportionment, total_j: f64) {
    assert_close(out.total_j(), total_j, "conservation");
}

// ---------------------------------------------------------------------------
// (a) mandated: one span covering one sample fully, 50% cpu share
// ---------------------------------------------------------------------------

#[test]
fn single_span_with_half_a_core_second_gets_half_the_joules() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 50.0);
    assert_close(out.baseline_idle_j, 50.0, "baseline");
    assert_close(out.orphaned_j, 0.0, "orphaned");
    assert_eq!(out.policy(), Policy::L2CpuTime);
    assert_eq!((out.samples_l2, out.samples_l1), (1, 0));
    assert_conserved(&out, 100.0);
}

// ---------------------------------------------------------------------------
// (b) mandated: two overlapping spans, 3:1 cpu ratio
// ---------------------------------------------------------------------------

#[test]
fn two_overlapping_spans_split_the_active_share_by_cpu_ratio() {
    let tree = tree(&[
        ("s1", ExecutionLocus::Local, 0, 1000, &[10]),
        ("s2", ExecutionLocus::Local, 0, 1000, &[11]),
    ]);
    let samples = [sample(0, 1000, 100.0)];
    // 300ms vs 100ms of a 1000ms window: 40% active, split 3:1.
    let procs = [procs(0, 1000, &[(10, 300, None), (11, 100, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 30.0);
    assert_span_j(&out, "s2", 10.0);
    assert_close(out.baseline_idle_j, 60.0, "baseline");
    assert_eq!(out.policy(), Policy::L2CpuTime);
    assert_conserved(&out, 100.0);
}

// ---------------------------------------------------------------------------
// (c) mandated: no process samples → L1 wall-clock
// ---------------------------------------------------------------------------

#[test]
fn no_process_samples_falls_back_to_l1_wall_clock() {
    let tree = tree(&[
        ("s1", ExecutionLocus::Local, 0, 500, &[10]),
        ("s2", ExecutionLocus::Local, 750, 1000, &[11]),
    ]);
    let samples = [sample(0, 1000, 100.0)];

    let out = apportion(&samples, &[], &tree);

    assert_span_j(&out, "s1", 50.0); // 500/1000 of the window
    assert_span_j(&out, "s2", 25.0); // 250/1000 of the window
    assert_close(out.baseline_idle_j, 25.0, "baseline");
    assert_eq!(out.policy(), Policy::L1WallClock);
    assert_eq!((out.samples_l2, out.samples_l1), (0, 1));
    assert_conserved(&out, 100.0);
}

#[test]
fn l1_shares_are_capped_when_overlapping_spans_exceed_the_window() {
    // Three spans each covering the whole window would claim 300%.
    let tree = tree(&[
        ("s1", ExecutionLocus::Local, 0, 1000, &[]),
        ("s2", ExecutionLocus::Local, 0, 1000, &[]),
        ("s3", ExecutionLocus::Local, 0, 1000, &[]),
    ]);
    let samples = [sample(0, 1000, 90.0)];

    let out = apportion(&samples, &[], &tree);

    for id in ["s1", "s2", "s3"] {
        assert_span_j(&out, id, 30.0);
    }
    assert_close(out.baseline_idle_j, 0.0, "baseline");
    assert_conserved(&out, 90.0);
}

#[test]
fn a_process_sample_that_does_not_overlap_the_window_still_means_l1() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(5000, 6000, &[(10, 900, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 100.0);
    assert_eq!(out.policy(), Policy::L1WallClock);
    assert_eq!((out.samples_l2, out.samples_l1), (0, 1));
}

#[test]
fn a_degenerate_process_window_carries_no_weight_and_falls_back_to_l1() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(500, 500, &[(10, 900, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 100.0);
    assert_eq!(out.policy(), Policy::L1WallClock);
    // Dropped, but not silently: the window is counted so coverage figures
    // can tell "no process data" from "process data we could not use".
    assert_eq!(out.skipped_events, 1);
    assert_conserved(&out, 100.0);
}

#[test]
fn a_run_of_zero_cpu_deltas_stays_l2_and_never_falls_back_to_wall_clock() {
    // design-log: sub-0.5ms rounds to 0; a 0 delta is an observed near-idle
    // tree, not a missing measurement. It must not resurrect L1.
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 0, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 0.0);
    assert_close(out.baseline_idle_j, 100.0, "baseline");
    assert_eq!(out.policy(), Policy::L2CpuTime);
    assert_eq!((out.samples_l2, out.samples_l1), (1, 0));
}

// ---------------------------------------------------------------------------
// (d) mandated: remote spans
// ---------------------------------------------------------------------------

#[test]
fn remote_spans_are_excluded_and_counted_once_each() {
    let tree = tree(&[
        ("local", ExecutionLocus::Local, 0, 1000, &[10]),
        ("remote", ExecutionLocus::Remote, 0, 1000, &[11]),
    ]);
    // The remote span overlaps three samples; it must still count once.
    let samples = [
        sample(0, 1000, 60.0),
        sample(1000, 2000, 0.0),
        sample(0, 1000, 0.0),
    ];
    let procs = [procs(0, 1000, &[(10, 500, None), (11, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_eq!(out.unmeasured_remote_spans, 1);
    assert_span_j(&out, "remote", 0.0);
    assert!(!out.per_span_j.contains_key("remote"));
    // pid 11's cpu belongs to no attributable span → orphaned, not the
    // remote span and not silently dropped into a local span.
    assert_span_j(&out, "local", 30.0);
    assert_close(out.orphaned_j, 30.0, "orphaned");
    assert_conserved(&out, 60.0);
}

#[test]
fn a_remote_span_overlapping_no_sample_is_not_counted() {
    let tree = tree(&[("remote", ExecutionLocus::Remote, 5000, 6000, &[])]);
    let out = apportion(&[sample(0, 1000, 10.0)], &[], &tree);
    assert_eq!(out.unmeasured_remote_spans, 0);
    assert_close(out.baseline_idle_j, 10.0, "baseline");
}

#[test]
fn a_zero_length_remote_span_inside_a_sample_is_still_counted_once() {
    // A remote call the collector could only observe as an instant is still
    // unmeasured remote work; excluding it would under-report the very gap
    // this counter exists to make visible.
    let tree = tree(&[("remote", ExecutionLocus::Remote, 500, 500, &[])]);
    let samples = [sample(0, 1000, 40.0), sample(0, 1000, 0.0)];

    let out = apportion(&samples, &[], &tree);

    assert_eq!(out.unmeasured_remote_spans, 1);
    assert!(!out.per_span_j.contains_key("remote"));
    assert_eq!(
        out.degenerate_spans, 0,
        "remote spans are not double-counted"
    );
    assert_close(out.baseline_idle_j, 40.0, "baseline");
    assert_conserved(&out, 40.0);
}

#[test]
fn a_zero_length_remote_span_on_a_sample_boundary_is_not_counted() {
    // `overlaps` is half-open: a zero-length span at t1 (or t0) lies
    // outside `[t0, t1)`, so it belongs to neither neighbouring window.
    let tree = tree(&[("remote", ExecutionLocus::Remote, 1000, 1000, &[])]);
    let out = apportion(&[sample(0, 1000, 10.0)], &[], &tree);
    assert_eq!(out.unmeasured_remote_spans, 0);
}

#[test]
fn zero_length_local_spans_overlapping_a_sample_are_counted_as_degenerate() {
    let tree = tree(&[
        ("z1", ExecutionLocus::Local, 500, 500, &[]),
        ("z2", ExecutionLocus::Unknown, 250, 250, &[]),
        ("live", ExecutionLocus::Local, 0, 1000, &[]),
    ]);
    // Two samples: the degenerate spans must count once each, not per sample.
    let samples = [sample(0, 1000, 100.0), sample(0, 1000, 0.0)];

    let out = apportion(&samples, &[], &tree);

    assert_eq!(out.degenerate_spans, 2);
    assert_eq!(out.unmeasured_remote_spans, 0);
    assert_span_j(&out, "z1", 0.0);
    assert_span_j(&out, "z2", 0.0);
    assert_span_j(&out, "live", 100.0);
    assert_conserved(&out, 100.0);
}

#[test]
fn degenerate_span_counting_excludes_bootstrap_and_non_overlapping_spans() {
    let mut boot = span_env("session-boot-1", ExecutionLocus::Local, 500, 500, vec![10]);
    if let Payload::ActionSpan(action) = &mut boot.payload {
        action.tool_name = "__session__".to_string();
        action.tool_kind = ToolKind::Other;
    }
    let tree = correlate(&[
        boot,
        // Zero-length, but nowhere near the sample.
        span_env("elsewhere", ExecutionLocus::Local, 9000, 9000, vec![]),
        // Negative-length spans are degenerate too.
        span_env("backwards", ExecutionLocus::Local, 600, 400, vec![]),
    ]);

    let out = apportion(&[sample(0, 1000, 10.0)], &[], &tree);

    // The bootstrap span is zero-length by design and overlaps the sample;
    // counting it would report a defect in every single session.
    assert_eq!(out.degenerate_spans, 1);
    assert_close(out.baseline_idle_j, 10.0, "baseline");
}

// ---------------------------------------------------------------------------
// root-tree inheritance: pid-less spans claim the session's own process tree
// ---------------------------------------------------------------------------

/// A tree whose bootstrap span carries `root_pids`, plus `rows` of ordinary
/// spans — the shape the Claude Code hook collector actually produces.
fn tree_with_root(
    root_pids: Vec<i64>,
    rows: &[(&str, ExecutionLocus, i64, i64, &[i64])],
) -> SessionTree {
    let mut boot = span_env("session-boot-1", ExecutionLocus::Local, 0, 0, root_pids);
    if let Payload::ActionSpan(action) = &mut boot.payload {
        action.tool_name = "__session__".to_string();
        action.tool_kind = ToolKind::Other;
    }
    let mut events = vec![boot];
    events.extend(
        rows.iter()
            .map(|(id, locus, s, e, pids)| span_env(id, *locus, *s, *e, pids.to_vec())),
    );
    correlate(&events)
}

#[test]
fn a_pid_less_span_inherits_the_root_tree_and_gets_the_active_share() {
    // The hook collector pids only the bootstrap span, so without this rule
    // every tool-call span would be pid-less and the whole session's cpu
    // would land in the orphan bucket — L2 would be unreachable in practice.
    let tree = tree_with_root(vec![10], &[("s1", ExecutionLocus::Local, 0, 1000, &[])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 50.0);
    assert_close(out.orphaned_j, 0.0, "orphaned");
    assert_close(out.baseline_idle_j, 50.0, "baseline");
    assert_eq!(out.policy(), Policy::L2CpuTime);
    assert_eq!((out.samples_l2, out.samples_l1), (1, 0));
    assert_conserved(&out, 100.0);
}

#[test]
fn two_concurrent_pid_less_spans_split_the_inherited_root_tree_equally() {
    let tree = tree_with_root(
        vec![10],
        &[
            ("s1", ExecutionLocus::Local, 0, 1000, &[]),
            ("s2", ExecutionLocus::Local, 0, 1000, &[]),
        ],
    );
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    // One physical tree, two claimants, no cpu isolation → equal split.
    assert_span_j(&out, "s1", 25.0);
    assert_span_j(&out, "s2", 25.0);
    assert_close(out.orphaned_j, 0.0, "orphaned");
    assert_conserved(&out, 100.0);
}

#[test]
fn a_span_with_its_own_pids_does_not_inherit_the_root_tree() {
    // An explicit pid list is a measurement; widening it to the root tree
    // would hand the span cpu it was observed not to own.
    let tree = tree_with_root(vec![10], &[("s1", ExecutionLocus::Local, 0, 1000, &[11])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 0.0);
    assert_close(out.orphaned_j, 50.0, "orphaned");
    assert_conserved(&out, 100.0);
}

#[test]
fn a_pid_less_span_claims_nothing_when_the_session_has_no_root_pids() {
    // No bootstrap span (OTLP-only session, hooks enabled mid-session):
    // there is no tree to inherit, so the cpu is honestly orphaned.
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[])]);
    let out = apportion(
        &[sample(0, 1000, 100.0)],
        &[procs(0, 1000, &[(10, 500, None)])],
        &tree,
    );

    assert_span_j(&out, "s1", 0.0);
    assert_close(out.orphaned_j, 50.0, "orphaned");
    assert_conserved(&out, 100.0);
}

#[test]
fn a_pid_less_span_inherits_every_root_pid() {
    let tree = tree_with_root(vec![10, 11], &[("s1", ExecutionLocus::Local, 0, 1000, &[])]);
    let out = apportion(
        &[sample(0, 1000, 100.0)],
        &[procs(0, 1000, &[(10, 300, None), (11, 200, None)])],
        &tree,
    );

    assert_span_j(&out, "s1", 50.0);
    assert_close(out.orphaned_j, 0.0, "orphaned");
    assert_conserved(&out, 100.0);
}

#[test]
fn hybrid_and_unknown_loci_are_attributed_like_local() {
    for locus in [
        ExecutionLocus::Local,
        ExecutionLocus::Hybrid,
        ExecutionLocus::Unknown,
    ] {
        let tree = tree(&[("s1", locus, 0, 1000, &[10])]);
        let out = apportion(
            &[sample(0, 1000, 100.0)],
            &[procs(0, 1000, &[(10, 500, None)])],
            &tree,
        );
        assert_span_j(&out, "s1", 50.0);
        assert_eq!(
            out.unmeasured_remote_spans, 0,
            "{locus:?} counted as remote"
        );
    }
}

// ---------------------------------------------------------------------------
// shared pids, orphans, overlap fractions, capping
// ---------------------------------------------------------------------------

#[test]
fn a_pid_watched_by_two_spans_is_split_equally_not_double_counted() {
    // design-log: the sampler emits one entry per (span, pid) pair, so the
    // same physical tree appears twice with independent baselines.
    let tree = tree(&[
        ("s1", ExecutionLocus::Local, 0, 1000, &[10]),
        ("s2", ExecutionLocus::Local, 0, 1000, &[10]),
    ]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 400, None), (10, 400, None)])];

    let out = apportion(&samples, &procs, &tree);

    // 400ms of physical cpu (not 800), split 200/200 → 20 J each.
    assert_span_j(&out, "s1", 20.0);
    assert_span_j(&out, "s2", 20.0);
    assert_close(out.baseline_idle_j, 60.0, "baseline");
    assert_conserved(&out, 100.0);
}

#[test]
fn duplicate_entries_for_one_pid_use_the_longest_observed_delta() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    // Two watchers of pid 10 with different baselines; only one span is in
    // the tree, so it claims the whole (deduplicated) 500ms.
    let procs = [procs(0, 1000, &[(10, 200, None), (10, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 50.0);
    assert_conserved(&out, 100.0);
}

#[test]
fn orphan_entries_land_in_the_orphan_bucket() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 300, None), (11, 200, Some("s0"))])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 30.0);
    assert_close(out.orphaned_j, 20.0, "orphaned");
    assert_close(out.baseline_idle_j, 50.0, "baseline");
    assert_conserved(&out, 100.0);
}

#[test]
fn an_orphan_sharing_a_pid_with_a_live_span_splits_that_pid_equally() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 400, None), (10, 400, Some("s0"))])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 20.0);
    assert_close(out.orphaned_j, 20.0, "orphaned");
    assert_conserved(&out, 100.0);
}

#[test]
fn orphan_cpu_is_attributed_even_when_no_span_overlaps_the_window() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 500, &[10])]);
    let samples = [sample(1000, 2000, 100.0)];
    let procs = [procs(1000, 2000, &[(10, 400, Some("s1"))])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 0.0);
    assert_close(out.orphaned_j, 40.0, "orphaned");
    assert_eq!((out.samples_l2, out.samples_l1), (1, 0));
    assert_conserved(&out, 100.0);
}

#[test]
fn process_weight_is_scaled_by_the_overlap_fraction_of_its_window() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 2000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    // The process window [500,1500) overlaps the energy window by half.
    let procs = [procs(500, 1500, &[(10, 800, None)])];

    let out = apportion(&samples, &procs, &tree);

    // 800ms * 0.5 = 400ms against a 1000ms window → 40%.
    assert_span_j(&out, "s1", 40.0);
    assert_conserved(&out, 100.0);
}

#[test]
fn cpu_time_beyond_one_core_second_is_capped_and_leaves_no_baseline() {
    // Multi-core reality: 4 cores busy for the whole window. v1 normalizes
    // against a single core and caps at 100% of the sample.
    let tree = tree(&[
        ("s1", ExecutionLocus::Local, 0, 1000, &[10]),
        ("s2", ExecutionLocus::Local, 0, 1000, &[11]),
    ]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 3000, None), (11, 1000, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 75.0);
    assert_span_j(&out, "s2", 25.0);
    assert_close(out.baseline_idle_j, 0.0, "baseline");
    assert_conserved(&out, 100.0);
}

#[test]
fn a_span_outside_the_window_gets_nothing_even_if_its_pid_is_sampled() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 2000, 3000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(10, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 0.0);
    // Unclaimed cpu is orphaned compute, not span energy and not idle.
    assert_close(out.orphaned_j, 50.0, "orphaned");
    assert_conserved(&out, 100.0);
}

// ---------------------------------------------------------------------------
// energy components
// ---------------------------------------------------------------------------

#[test]
fn component_totals_table() {
    let cpu = EnergyComponent {
        kind: EnergyKind::Cpu,
        label: None,
        energy_j: 10.0,
        method: EnergyMethod::Rapl,
    };
    let dram = EnergyComponent {
        kind: EnergyKind::Dram,
        label: None,
        energy_j: 3.0,
        method: EnergyMethod::TdpModel,
    };
    let gpu = EnergyComponent {
        kind: EnergyKind::Gpu,
        label: None,
        energy_j: 7.0,
        method: EnergyMethod::Nvml,
    };
    let total = EnergyComponent {
        kind: EnergyKind::Total,
        label: None,
        energy_j: 15.0,
        method: EnergyMethod::Rapl,
    };

    let cases: [(&str, Vec<EnergyComponent>, f64); 4] = [
        (
            "total wins over the parts",
            vec![cpu.clone(), dram.clone(), total.clone()],
            15.0,
        ),
        (
            "parts are summed without a total",
            vec![cpu.clone(), dram.clone(), gpu.clone()],
            20.0,
        ),
        ("no components at all", vec![], 0.0),
        ("only a total", vec![total.clone()], 15.0),
    ];

    for (what, components, expected) in cases {
        let s = EnergySample {
            t_start: ts(0),
            t_end: ts(1000),
            components,
            host_id: None,
        };
        assert_close(sample_energy_j(&s), expected, what);
        // and the same number is what apportionment conserves
        let out = apportion(&[s], &[], &SessionTree::default());
        assert_conserved(&out, expected);
    }
}

// ---------------------------------------------------------------------------
// robustness: degenerate windows, degenerate spans, unparseable timestamps
// ---------------------------------------------------------------------------

#[test]
fn degenerate_energy_windows_go_entirely_to_baseline() {
    let cases: [(&str, i64, i64); 2] = [("zero length", 500, 500), ("negative length", 900, 400)];

    for (what, start, end) in cases {
        let tree = tree(&[("s1", ExecutionLocus::Local, 0, 2000, &[10])]);
        let out = apportion(
            &[sample(start, end, 42.0)],
            &[procs(0, 2000, &[(10, 1000, None)])],
            &tree,
        );
        assert_close(out.baseline_idle_j, 42.0, what);
        assert_span_j(&out, "s1", 0.0);
        assert_eq!(out.degenerate_samples, 1, "{what}: degenerate counter");
        assert_eq!((out.samples_l2, out.samples_l1), (0, 0), "{what}");
        assert_conserved(&out, 42.0);
    }
}

#[test]
fn degenerate_spans_are_never_attributed() {
    let cases: [(&str, i64, i64); 2] = [("zero length", 500, 500), ("negative length", 900, 400)];

    for (what, start, end) in cases {
        let tree = tree(&[("s1", ExecutionLocus::Local, start, end, &[10])]);
        assert_eq!(tree.spans.len(), 1, "{what}: span kept in the tree");
        let out = apportion(
            &[sample(0, 1000, 100.0)],
            &[procs(0, 1000, &[(10, 500, None)])],
            &tree,
        );
        assert_span_j(&out, "s1", 0.0);
        // Its pid's cpu is claimed by nobody → orphaned compute.
        assert_close(out.orphaned_j, 50.0, what);
        assert_conserved(&out, 100.0);
    }
}

#[test]
fn the_bootstrap_session_span_is_kept_but_never_attributed() {
    let mut boot = span_env("session-boot-1", ExecutionLocus::Local, 0, 0, vec![10]);
    if let Payload::ActionSpan(action) = &mut boot.payload {
        action.tool_name = "__session__".to_string();
        action.tool_kind = ToolKind::Other;
    }
    let tree = correlate(&[
        boot,
        span_env("s1", ExecutionLocus::Local, 0, 1000, vec![11]),
    ]);

    assert_eq!(tree.spans.len(), 2);
    assert_eq!(tree.root_pids, vec![10]);

    let out = apportion(
        &[sample(0, 1000, 100.0)],
        &[procs(0, 1000, &[(11, 500, None)])],
        &tree,
    );
    assert_span_j(&out, "session-boot-1", 0.0);
    assert_span_j(&out, "s1", 50.0);
}

#[test]
fn unparseable_span_timestamps_are_skipped_and_counted() {
    let events = vec![
        raw_span("bad-start", ExecutionLocus::Local, "not-a-date", &ts(1000)),
        raw_span("bad-end", ExecutionLocus::Local, &ts(0), ""),
        raw_span("ok", ExecutionLocus::Local, &ts(0), &ts(1000)),
    ];
    let tree = correlate(&events);

    assert_eq!(tree.skipped_events, 2);
    assert_eq!(tree.spans.len(), 1);
    assert_eq!(tree.spans[0].span_id, "ok");
}

#[test]
fn unparseable_sample_timestamps_are_skipped_and_counted() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let mut bad_sample = sample(0, 1000, 999.0);
    bad_sample.t_end = "23:59:59".to_string();
    let mut bad_procs = procs(0, 1000, &[(10, 900, None)]);
    bad_procs.t_start = "yesterday".to_string();

    let out = apportion(&[bad_sample, sample(0, 1000, 100.0)], &[bad_procs], &tree);

    assert_eq!(out.skipped_events, 2);
    // The unparseable sample's joules are not conserved into anything —
    // the event never entered the join at all.
    assert_span_j(&out, "s1", 100.0);
    assert_eq!(out.policy(), Policy::L1WallClock);
    assert_conserved(&out, 100.0);
}

#[test]
fn timestamps_with_offsets_and_fractional_seconds_parse() {
    let events = vec![raw_span(
        "s1",
        ExecutionLocus::Local,
        "2026-07-25T14:00:00.500+02:00",
        "2026-07-25T12:00:01.500Z",
    )];
    let tree = correlate(&events);
    assert_eq!(tree.skipped_events, 0);
    assert_eq!(tree.spans[0].t_end - tree.spans[0].t_start, 1000);
}

#[test]
fn correlate_ignores_non_span_events_and_keeps_span_fields() {
    let events = vec![
        envelope(Payload::EnergySample(sample(0, 1000, 1.0))),
        span_env("s1", ExecutionLocus::Hybrid, 0, 1000, vec![7, 8]),
    ];
    let tree = correlate(&events);

    assert_eq!(tree.spans.len(), 1);
    let span = &tree.spans[0];
    assert_eq!(span.span_id, "s1");
    assert_eq!(span.tool_name, "Bash");
    assert_eq!(span.tool_kind, ToolKind::Bash);
    assert_eq!(span.locus, ExecutionLocus::Hybrid);
    assert_eq!(span.pids, vec![7, 8]);
    assert_eq!(span.t_end - span.t_start, 1000);
    assert!(tree.root_pids.is_empty());
}

#[test]
fn multiple_samples_accumulate_per_span() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 2000, &[10])]);
    let samples = [sample(0, 1000, 100.0), sample(1000, 2000, 100.0)];
    let procs = [
        procs(0, 1000, &[(10, 500, None)]),
        procs(1000, 2000, &[(10, 250, None)]),
    ];

    let out = apportion(&samples, &procs, &tree);

    assert_span_j(&out, "s1", 75.0);
    assert_close(out.baseline_idle_j, 125.0, "baseline");
    assert_eq!(out.samples_l2, 2);
    assert_conserved(&out, 200.0);
}

#[test]
fn policy_is_l2_when_any_sample_used_process_data() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 3000, &[10])]);
    let samples = [
        sample(0, 1000, 10.0),    // process data → L2
        sample(2000, 3000, 10.0), // none → L1
    ];
    let procs = [procs(0, 1000, &[(10, 500, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_eq!(out.policy(), Policy::L2CpuTime);
    assert_eq!((out.samples_l2, out.samples_l1), (1, 1));
    assert_conserved(&out, 20.0);
}

#[test]
fn an_empty_join_is_pure_baseline() {
    let out = apportion(&[sample(0, 1000, 12.0)], &[], &SessionTree::default());
    assert!(out.per_span_j.is_empty());
    assert_close(out.baseline_idle_j, 12.0, "baseline");
    assert_eq!(out.policy(), Policy::L1WallClock);
    assert_eq!((out.samples_l2, out.samples_l1), (0, 0));
}

// ---------------------------------------------------------------------------
// (e) mandated: conservation property
// ---------------------------------------------------------------------------

const LOCI: [ExecutionLocus; 4] = [
    ExecutionLocus::Local,
    ExecutionLocus::Remote,
    ExecutionLocus::Hybrid,
    ExecutionLocus::Unknown,
];

fn arb_span_event() -> impl Strategy<Value = Envelope> {
    (
        0usize..4,
        0usize..LOCI.len(),
        0i64..4000,
        -500i64..4000,
        prop::collection::vec(1i64..5, 0..3),
    )
        .prop_map(|(id, locus, start, duration, pids)| {
            span_env(
                &format!("span-{id}"),
                LOCI[locus],
                start,
                start + duration,
                pids,
            )
        })
}

fn arb_sample() -> impl Strategy<Value = EnergySample> {
    (0i64..4000, -200i64..2000, 0.0f64..1000.0)
        .prop_map(|(start, duration, joules)| sample(start, start + duration, joules))
}

fn arb_process_sample() -> impl Strategy<Value = ProcessSample> {
    (
        0i64..4000,
        -200i64..2000,
        prop::collection::vec((1i64..5, 0u64..4000, prop::option::of(0usize..4)), 0..5),
    )
        .prop_map(|(start, duration, rows)| {
            let owned: Vec<(i64, u64, Option<String>)> = rows
                .into_iter()
                .map(|(pid, cpu, orphan)| (pid, cpu, orphan.map(|i| format!("span-{i}"))))
                .collect();
            let borrowed: Vec<(i64, u64, Option<&str>)> = owned
                .iter()
                .map(|(pid, cpu, orphan)| (*pid, *cpu, orphan.as_deref()))
                .collect();
            procs(start, start + duration, &borrowed)
        })
}

proptest! {
    /// Every measured joule lands in exactly one bucket: a span, the orphan
    /// bucket, or baseline/idle. Nothing is invented, nothing evaporates.
    #[test]
    fn conservation_holds_for_random_joins(
        span_events in prop::collection::vec(arb_span_event(), 0..6),
        samples in prop::collection::vec(arb_sample(), 0..6),
        process_samples in prop::collection::vec(arb_process_sample(), 0..6),
    ) {
        let tree = correlate(&span_events);
        let out = apportion(&samples, &process_samples, &tree);

        let total: f64 = samples.iter().map(sample_energy_j).sum();
        prop_assert!(
            (out.total_j() - total).abs() <= 1e-6 * total.max(1.0),
            "expected {total} J, got {} J ({out:?})",
            out.total_j()
        );
        // No unparseable timestamps are generated, so every skipped event is
        // a degenerate `process_sample` window — at most one per input.
        prop_assert!(out.skipped_events <= process_samples.len() as u32);
        prop_assert!(out.baseline_idle_j >= -1e-9, "negative baseline: {:?}", out);
        prop_assert!(out.orphaned_j >= -1e-9, "negative orphan bucket: {:?}", out);
        for (id, j) in &out.per_span_j {
            prop_assert!(j.is_finite() && *j >= -1e-9, "bad joules for {}: {}", id, j);
        }
        // Remote spans never receive energy.
        let remote: HashMap<&str, ()> = tree
            .spans
            .iter()
            .filter(|s| s.locus == ExecutionLocus::Remote)
            .map(|s| (s.span_id.as_str(), ()))
            .collect();
        let local: HashMap<&str, ()> = tree
            .spans
            .iter()
            .filter(|s| s.locus != ExecutionLocus::Remote)
            .map(|s| (s.span_id.as_str(), ()))
            .collect();
        for id in remote.keys() {
            if !local.contains_key(id) {
                prop_assert!(!out.per_span_j.contains_key(*id), "remote span {} got energy", id);
            }
        }
        prop_assert!(out.samples_l1 + out.samples_l2 + out.degenerate_samples <= samples.len() as u32);
    }
}

// ---------------------------------------------------------------------------
// unobserved process windows: the sampler ran and saw nothing
// ---------------------------------------------------------------------------

/// A `process_sample` covering the window with an empty `processes` array is
/// not the same fact as no process data at all. Every weight is zero, so
/// every overlapping span is paid nothing and the whole sample falls to
/// baseline — which, uncounted, reads as "these spans used no energy"
/// rather than "nothing was observed".
#[test]
fn a_process_window_that_enumerated_nothing_is_counted_not_read_as_idleness() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[])];

    let out = apportion(&samples, &procs, &tree);

    assert_eq!(
        out.unobserved_process_windows, 1,
        "the empty enumeration must be visible in the counters"
    );
    // The span really does get nothing — that part is correct and stays.
    assert_span_j(&out, "s1", 0.0);
    assert_close(out.baseline_idle_j, 100.0, "baseline");
    assert_conserved(&out, 100.0);
}

/// The counter must not fire for the ordinary "no process data" case, which
/// is a different failure with a different remedy (L1 fallback, not a
/// broken sampler).
#[test]
fn a_window_with_no_process_data_at_all_is_not_counted_as_unobserved() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];

    let out = apportion(&samples, &[], &tree);

    assert_eq!(out.unobserved_process_windows, 0);
    assert_eq!(out.policy(), Policy::L1WallClock);
}

/// …nor when the sampler enumerated something, even if no span claimed it.
#[test]
fn a_window_with_observed_processes_is_never_counted_as_unobserved() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [sample(0, 1000, 100.0)];
    let procs = [procs(0, 1000, &[(999, 400, None)])];

    let out = apportion(&samples, &procs, &tree);

    assert_eq!(out.unobserved_process_windows, 0);
}

/// Each covered window counts separately, and a window covered by a mix of
/// empty and non-empty samples was observed.
#[test]
fn unobserved_windows_are_counted_per_window_and_only_when_wholly_empty() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 3000, &[10])]);
    let samples = [
        sample(0, 1000, 100.0),
        sample(1000, 2000, 100.0),
        sample(2000, 3000, 100.0),
    ];
    let procs = [
        procs(0, 1000, &[]),
        procs(1000, 2000, &[]),
        // The third window has one empty sample and one that saw something.
        procs(2000, 3000, &[]),
        procs(2000, 3000, &[(10, 500, None)]),
    ];

    let out = apportion(&samples, &procs, &tree);

    assert_eq!(out.unobserved_process_windows, 2);
}

// ---------------------------------------------------------------------------
// policy truthfulness: "no policy applied" is not "L1"
// ---------------------------------------------------------------------------

/// `Apportionment::policy` defaults to L1, so it cannot distinguish "used
/// wall-clock" from "divided nothing at all". `applied_policy` can, and the
/// surfaces that label a session by its policy use it.
#[test]
fn a_join_that_apportioned_nothing_reports_no_policy() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);

    // No energy samples: nothing was ever divided.
    let out = apportion(&[], &[], &tree);
    assert_eq!(out.applied_policy(), None);
    assert_eq!(
        af_core::policy_id(out.applied_policy()),
        af_core::POLICY_NONE
    );

    // A sample with no attributable span: also nothing divided.
    let empty_tree = tree_from_nothing();
    let out = apportion(&[sample(0, 1000, 100.0)], &[], &empty_tree);
    assert_eq!(out.applied_policy(), None);
}

#[test]
fn an_applied_policy_is_reported_as_the_rung_that_ran() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);

    let l1 = apportion(&[sample(0, 1000, 100.0)], &[], &tree);
    assert_eq!(l1.applied_policy(), Some(Policy::L1WallClock));
    assert_eq!(
        af_core::policy_id(l1.applied_policy()),
        af_core::POLICY_L1_WALL_CLOCK
    );

    let l2 = apportion(
        &[sample(0, 1000, 100.0)],
        &[procs(0, 1000, &[(10, 500, None)])],
        &tree,
    );
    assert_eq!(l2.applied_policy(), Some(Policy::L2CpuTime));
}

/// The per-sample trace carries `None` for a sample no policy touched.
/// Labelling it `l1_wall_clock/v1` claims a wall-clock division that never
/// ran, and the console renders that label verbatim.
#[test]
fn a_sample_trace_reports_no_policy_when_none_was_applied() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let samples = [
        // Degenerate window: divided by nothing.
        sample(2000, 2000, 10.0),
        // No span active: nothing to divide over.
        sample(5000, 6000, 10.0),
        // Really apportioned.
        sample(0, 1000, 100.0),
    ];
    let procs = [procs(0, 1000, &[(10, 500, None)])];

    let (_out, traces) = af_core::apportion_traced(&samples, &procs, &tree);

    assert_eq!(traces.len(), 3);
    assert_eq!(traces[0].policy, None);
    assert_eq!(traces[0].policy_id(), af_core::POLICY_NONE);
    assert_eq!(traces[1].policy, None);
    assert_eq!(traces[1].policy_id(), af_core::POLICY_NONE);
    assert_eq!(traces[2].policy, Some(Policy::L2CpuTime));
    assert_eq!(traces[2].policy_id(), af_core::POLICY_L2_CPU_TIME);
}

/// An L1-apportioned sample says L1, not "none" — the `None` must be the
/// narrow "nothing happened" case, not a blanket downgrade.
#[test]
fn an_l1_sample_trace_still_reports_l1() {
    let tree = tree(&[("s1", ExecutionLocus::Local, 0, 1000, &[10])]);
    let (_out, traces) = af_core::apportion_traced(&[sample(0, 1000, 100.0)], &[], &tree);

    assert_eq!(traces[0].policy, Some(Policy::L1WallClock));
    assert_eq!(traces[0].policy_id(), af_core::POLICY_L1_WALL_CLOCK);
}

fn tree_from_nothing() -> SessionTree {
    correlate(&[])
}
