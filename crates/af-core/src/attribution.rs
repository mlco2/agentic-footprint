//! Energy attribution: machine energy samples × the correlated
//! [`SessionTree`] → joules per span, per RFC §8's accuracy ladder.
//!
//! The policy implemented here is `l2_cpu_time` v1. In short, for each energy
//! sample's window `[t0, t1)`:
//!
//! 1. `execution_locus: remote` spans are excluded from the join entirely
//!    and each distinct remote span overlapping *any* sample is counted
//!    once in [`Apportionment::unmeasured_remote_spans`]. `hybrid` and
//!    `unknown` loci are attributed like `local`.
//! 2. **L2** (`process_sample` data overlaps the window): each overlapping
//!    span is weighted by the cpu-time of the pid trees it owns, scaled by
//!    how much of the process window overlaps the energy window. The
//!    fraction of the sample handed to spans is `min(1, W / C)` where `W`
//!    is the total weight in cpu-milliseconds and `C` the window's wall
//!    length in milliseconds — cpu-seconds normalized against *one*
//!    core-second, capped. The remainder is `baseline_idle_j`.
//! 3. **L1** (no process data for that window): spans get
//!    `sample_j × overlap / window`, scaled down if the overlaps sum past
//!    the whole sample; the remainder is `baseline_idle_j`.
//!
//! A span that lists no pids of its own inherits [`SessionTree::root_pids`]
//! — the session's own process tree, hoisted from the collector's bootstrap
//! span — as its claimant set, so an L2 join is reachable with collectors
//! that can only observe the agent's own pid (the Claude Code hook shim is
//! exactly that). Concurrent pid-less spans then share the root tree under
//! the ordinary shared-pid equal-split rule.
//!
//! Conservation is the invariant that matters: for every sample that
//! entered the join, `Σ per_span_j + orphaned_j + baseline_idle_j` equals
//! the sample's total joules. Nothing is invented and nothing evaporates.

use std::collections::{BTreeMap, HashMap, HashSet};

use af_events::{EnergyKind, EnergySample, ProcessDelta, ProcessSample};

use crate::correlate::{parse_ts, SessionTree, Span, SpanId, BOOTSTRAP_TOOL_NAME};

/// Policy id recorded on derived records that used cpu-time weighting.
pub const POLICY_L2_CPU_TIME: &str = "l2_cpu_time/v1";
/// Policy id recorded on derived records that fell back to wall-clock.
pub const POLICY_L1_WALL_CLOCK: &str = "l1_wall_clock/v1";
/// Policy id for a sample **no** policy was applied to: its joules went
/// entirely to baseline without any span being weighed. Not a rung of the
/// ladder — the honest statement that no apportionment happened.
///
/// This exists because labelling such a sample `l1_wall_clock/v1` claims a
/// wall-clock division that never ran, and a console showing "L1" for a
/// window where nothing was attributed is reporting a decision the join
/// did not make.
pub const POLICY_NONE: &str = "none";

/// Which rung of the RFC §8 accuracy ladder an [`Apportionment`] reached.
///
/// This is the *best* rung any one sample used: [`Policy::L2CpuTime`] as
/// soon as a single sample was weighted by process data, since the
/// per-sample breakdown ([`Apportionment::samples_l1`] /
/// [`Apportionment::samples_l2`]) carries the nuance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Policy {
    /// CPU-time weighting from `process_sample` deltas.
    L2CpuTime,
    /// Wall-clock overlap slicing — the honest fallback when no process
    /// data covers a window. Also the value for a join that attributed
    /// nothing at all.
    #[default]
    L1WallClock,
}

impl Policy {
    /// Stable identifier for derived records / the read model.
    pub fn id(self) -> &'static str {
        match self {
            Policy::L2CpuTime => POLICY_L2_CPU_TIME,
            Policy::L1WallClock => POLICY_L1_WALL_CLOCK,
        }
    }

    /// The schema's enum value for this policy: [`Policy::id`] minus its
    /// `/vN` suffix.
    ///
    /// `schemas/v0.1/derived.schema.json` enumerates *unversioned* names
    /// (`l1_wall_clock`, `l2_cpu_time`, `l3_cgroup`) while derived records
    /// also carry the full versioned id, so a record stays re-computable
    /// when the policy mints a v2. The two are carried side by side rather
    /// than one being bent into the other, and this is the single place
    /// that derives one from the other.
    pub fn schema_name(self) -> &'static str {
        self.id().split('/').next().unwrap_or(self.id())
    }
}

/// Stable identifier for an *optional* policy: [`POLICY_NONE`] when no
/// policy was applied.
pub fn policy_id(policy: Option<Policy>) -> &'static str {
    match policy {
        Some(policy) => policy.id(),
        None => POLICY_NONE,
    }
}

/// The result of apportioning a session's energy samples over its spans.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Apportionment {
    /// Joules attributed to each span, keyed by `span_id`.
    pub per_span_j: HashMap<SpanId, f64>,
    /// Joules the machine burned that no span claimed: idle, other
    /// processes, and the capped-off remainder. Never spread over actions.
    pub baseline_idle_j: f64,
    /// Joules burned by watched pid trees that belong to no live span —
    /// `orphan_of`-tagged entries (a tree outliving its span) and cpu-time
    /// reported for pids no attributable span owns during that window.
    pub orphaned_j: f64,
    /// Distinct `execution_locus: remote` spans overlapping at least one
    /// energy sample. Counted once each, never per sample. Zero-length
    /// remote spans count too — a remote action observed as an instant is
    /// still unmeasured remote work, and dropping it would under-report the
    /// gap this counter exists to make visible.
    pub unmeasured_remote_spans: u32,
    /// Distinct non-remote spans with `t_end <= t_start` — excluding the
    /// `__session__` bootstrap span, which is zero-length by design —
    /// overlapping at least one energy sample. Counted once each. They are
    /// not attributable (a window with no duration carries no wall-clock
    /// share and cannot be weighted), so this is the honest record of local
    /// work that was observed but could receive nothing.
    pub degenerate_spans: u32,
    /// Samples apportioned by wall-clock overlap (no process data).
    pub samples_l1: u32,
    /// Samples apportioned by cpu-time weights.
    pub samples_l2: u32,
    /// Samples whose window was zero- or negative-length. Their joules go
    /// entirely to [`Apportionment::baseline_idle_j`] — a window with no
    /// duration cannot be divided by.
    pub degenerate_samples: u32,
    /// Energy/process samples dropped before the join: a timestamp that was
    /// not RFC 3339 (either kind), or a `process_sample` whose window was
    /// zero- or negative-length — its overlap fraction would be a division
    /// by zero, so it carries no weight and is not counted as coverage
    /// either. A skipped energy sample's joules never entered the join, so
    /// they are excluded from the conservation identity as well.
    pub skipped_events: u32,
    /// Energy windows covered by `process_sample`s that enumerated **no**
    /// process rows at all.
    ///
    /// This is a distinct failure from "no process data covers this
    /// window": the sampler ran, produced a sample for the interval, and
    /// listed nothing. On such a window every cpu-time weight is zero, so
    /// every overlapping span is paid zero joules and the whole sample
    /// falls to baseline — while [`Apportionment::samples_l2`] still counts
    /// it as cpu-time apportioned. Without this counter that reads as "the
    /// spans genuinely used no energy", which is a claim the data does not
    /// support: the truth is that nothing was observed. Surfaced in the
    /// join counters so a sampler running without the permissions it needs
    /// (or against a pid tree that has already exited) is visible rather
    /// than silently deflating every span's share to zero.
    pub unobserved_process_windows: u32,
}

impl Apportionment {
    /// Every joule this apportionment placed:
    /// `Σ per_span_j + orphaned_j + baseline_idle_j`. Equal (to floating
    /// point) to the total energy of the samples that entered the join.
    pub fn total_j(&self) -> f64 {
        self.per_span_j.values().sum::<f64>() + self.orphaned_j + self.baseline_idle_j
    }

    /// Joules attributed to one span, `0.0` if it received none.
    pub fn span_j(&self, span_id: &str) -> f64 {
        self.per_span_j.get(span_id).copied().unwrap_or(0.0)
    }

    /// The policy actually applied to at least one sample, or `None` when
    /// no sample was apportioned by anything.
    ///
    /// [`Apportionment::policy`] cannot answer this: it falls back to
    /// [`Policy::L1WallClock`], so a join that never divided a single
    /// sample is indistinguishable from one that used wall-clock
    /// throughout. Surfaces exist that label a session by its policy, and
    /// "L1" on a session where nothing was attributed is a claim about a
    /// computation that did not happen.
    pub fn applied_policy(&self) -> Option<Policy> {
        if self.samples_l2 > 0 {
            Some(Policy::L2CpuTime)
        } else if self.samples_l1 > 0 {
            Some(Policy::L1WallClock)
        } else {
            None
        }
    }

    /// The best rung reached across all samples, with
    /// [`Policy::L1WallClock`] standing in for "no sample was apportioned
    /// at all".
    ///
    /// Derived from [`Apportionment::applied_policy`] rather than stored:
    /// the two used to be independent fields that had to agree, which is a
    /// consistency obligation with no upside — a computed answer cannot
    /// drift from the counters it is computed from. Callers that need to
    /// distinguish the fallback from a real L1 division must ask
    /// `applied_policy` instead; that distinction is exactly what this
    /// method throws away, and it is thrown away deliberately for the
    /// schema field (`attribution_policy`) whose enum has no "none".
    pub fn policy(&self) -> Policy {
        self.applied_policy().unwrap_or_default()
    }
}

/// The total joules an [`EnergySample`] reports.
///
/// v1 attributes the sample as a whole: the `total` component when the
/// collector emitted one (it is the machine figure the sampler itself
/// computed), otherwise the sum of the per-subsystem components.
/// Per-component attribution (separate cpu/dram/gpu shares, each with its
/// own weighting signal) is future work — the gpu share in particular
/// wants a gpu-time signal the sampler does not collect yet.
pub fn sample_energy_j(sample: &EnergySample) -> f64 {
    let mut total = 0.0;
    let mut saw_total = false;
    let mut sum_all = 0.0;
    for component in &sample.components {
        sum_all += component.energy_j;
        if component.kind == EnergyKind::Total {
            saw_total = true;
            total += component.energy_j;
        }
    }
    if saw_total {
        total
    } else {
        sum_all
    }
}

/// One span's line in a [`SampleTrace`]: what it overlapped, what cpu-time
/// weight it carried, and what it was paid.
///
/// `l1_allocated_j` is the **shadow** L1 allocation — what the wall-clock
/// policy *would* have paid this span for this sample, computed but never
/// applied. It is deliberately **unscaled** by the concurrency correction
/// the real L1 fallback applies, because its purpose is to expose
/// over-attribution: two spans each overlapping the whole window each show
/// the whole sample, and [`SampleTrace::l1_shadow_sum_share`] reads 2.0.
/// Scaling it would hide exactly the defect it exists to demonstrate.
#[derive(Debug, Clone, PartialEq)]
pub struct AllocRow {
    pub span_id: SpanId,
    pub tool_name: String,
    pub locus: af_events::ExecutionLocus,
    /// Milliseconds of this span that fell inside the energy window.
    pub overlap_ms: f64,
    /// The span's cpu-time weight for this window, in milliseconds, after
    /// per-`process_sample` deduplication, equal splitting between
    /// claimants, and scaling by each process window's overlap. Zero on an
    /// L1 (wall-clock) sample — no process data existed to weigh.
    pub cpu_delta_ms: f64,
    /// Joules this span was paid out of this sample.
    pub allocated_j: f64,
    /// The L1 shadow allocation (see the struct docs).
    pub l1_allocated_j: f64,
    /// `true` for `execution_locus: remote` rows. They appear so the
    /// overlap is visible, and carry zero joules.
    pub excluded: bool,
    pub excluded_reason: Option<String>,
}

/// The full allocation decision for one energy sample: every joule of it,
/// placed.
///
/// `Σ rows.allocated_j + orphaned_j + baseline_j == total_j` for every
/// trace — the per-sample statement of the conservation invariant the
/// aggregate [`Apportionment`] makes for the whole session.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleTrace {
    /// Index of the sample in the `samples` slice handed to
    /// [`apportion_traced`], so the caller can recover its `event_id`
    /// without this layer having to know about envelopes. Samples whose
    /// timestamps didn't parse produce no trace at all.
    pub sample_index: usize,
    /// The sample's own window bounds, verbatim.
    pub t_start: String,
    pub t_end: String,
    /// Joules the sample reported ([`sample_energy_j`]).
    pub total_j: f64,
    /// The denominator the policy divided the cpu-time weights by: the
    /// window's wall length in milliseconds, i.e. **one core-second per
    /// second** (`l2_cpu_time/v1`'s single-core normalization). It is not
    /// the sum of the watched trees' cpu — dividing by that would make
    /// attributed + orphaned ≡ 100% and delete the baseline/idle remainder
    /// by construction. Zero for a degenerate window, which is divided by
    /// nothing.
    pub denominator_cpu_ms: f64,
    /// The rung this one sample reached, or `None` when **no** policy was
    /// applied to it — a degenerate window, or a window where nothing
    /// attributable was active. Such a sample's joules go wholly to
    /// baseline; naming a rung for it would claim a division that never
    /// ran. Rendered as [`POLICY_NONE`] by [`SampleTrace::policy_id`].
    pub policy: Option<Policy>,
    /// One row per overlapping span, attributable or remote-excluded.
    pub rows: Vec<AllocRow>,
    /// Joules from this sample that went to the orphan bucket.
    pub orphaned_j: f64,
    /// Joules from this sample left as baseline/idle.
    pub baseline_j: f64,
    /// Σ of the rows' unscaled wall-clock shares. Above 1.0 means the L1
    /// policy would have attributed more than the machine actually burned.
    pub l1_shadow_sum_share: f64,
    /// Total cpu-time weight (ms) the session's root pid tree contributed
    /// to this window, deduplicated per pid. The agent's own process tree:
    /// a real observation, reported whether or not any span claimed it.
    pub root_cpu_delta_ms: f64,
}

impl SampleTrace {
    /// The policy id for this sample, [`POLICY_NONE`] when none was
    /// applied.
    pub fn policy_id(&self) -> &'static str {
        policy_id(self.policy)
    }
}

/// A `process_sample` with parsed, positive-length bounds.
struct ProcWindow<'a> {
    t_start: i64,
    t_end: i64,
    processes: &'a [ProcessDelta],
}

/// CPU-time weights (in milliseconds) for one energy window.
#[derive(Default)]
struct Weights {
    per_span: BTreeMap<SpanId, f64>,
    orphan: f64,
    /// Weight contributed by pids in the session's root tree, whoever
    /// claimed it. Reported (not attributed) so the debug surface can say
    /// how much of the window was the agent's own process tree.
    root: f64,
}

impl Weights {
    fn total(&self) -> f64 {
        self.per_span.values().sum::<f64>() + self.orphan
    }
}

/// Apportions `samples` over `tree`'s spans, weighted by `procs`.
///
/// See the module documentation for the policy. `samples` and `procs` are the raw Contract #1
/// payloads for one session, in any order.
pub fn apportion(
    samples: &[EnergySample],
    procs: &[ProcessSample],
    tree: &SessionTree,
) -> Apportionment {
    run(samples, procs, tree).0
}

/// [`apportion`], plus a per-sample [`SampleTrace`] for every sample that
/// entered the join.
///
/// The traces are produced **inside the same loop** that computes the
/// aggregate, from the same intermediate values — they are a record of what
/// the policy did, never a second implementation of it. That is what makes
/// the debug console's attribution view showable without minting a rival
/// source of truth for the numbers: a trace that disagreed with the
/// `Apportionment` would be a bug in the recording, not a difference of
/// method.
pub fn apportion_traced(
    samples: &[EnergySample],
    procs: &[ProcessSample],
    tree: &SessionTree,
) -> (Apportionment, Vec<SampleTrace>) {
    run(samples, procs, tree)
}

/// The one implementation behind [`apportion`] and [`apportion_traced`].
///
/// Traces are recorded unconditionally and simply dropped by [`apportion`].
/// They used to be gated on a `trace: bool` threaded through seven
/// branches, which bought a few `Vec` allocations per sample at the price
/// of a second control-flow shape through the policy — the one shape that
/// is never exercised by the tests that check the numbers. A recording that
/// only runs when someone is watching is a recording nobody can trust.
fn run(
    samples: &[EnergySample],
    procs: &[ProcessSample],
    tree: &SessionTree,
) -> (Apportionment, Vec<SampleTrace>) {
    let mut out = Apportionment::default();
    let mut traces: Vec<SampleTrace> = Vec::new();

    // Parse the process windows once. A degenerate one (`t_end <= t_start`)
    // carries no usable weight — its overlap fraction would be a division
    // by zero — so it is dropped rather than counted as coverage, which
    // would wrongly suppress the L1 fallback for that window. Dropped is
    // not the same as unseen: it is counted in `skipped_events` alongside
    // the unparseable ones, so coverage never silently shrinks.
    let mut windows: Vec<ProcWindow> = Vec::with_capacity(procs.len());
    for proc_sample in procs {
        let (Some(t_start), Some(t_end)) =
            (parse_ts(&proc_sample.t_start), parse_ts(&proc_sample.t_end))
        else {
            out.skipped_events += 1;
            continue;
        };
        if t_end <= t_start {
            out.skipped_events += 1;
            continue;
        }
        windows.push(ProcWindow {
            t_start,
            t_end,
            processes: &proc_sample.processes,
        });
    }

    let attributable: Vec<&Span> = tree.spans.iter().filter(|s| s.is_attributable()).collect();
    // No `t_end > t_start` filter: a zero-length remote span is still a
    // remote action this join cannot measure. `Span::overlaps` is half-open,
    // so a zero-length span only overlaps a window it falls strictly inside
    // — it is counted once, for one sample, never for a touching boundary.
    let remote: Vec<&Span> = tree
        .spans
        .iter()
        .filter(|s| s.locus == af_events::ExecutionLocus::Remote)
        .collect();
    let mut remote_seen: HashSet<&str> = HashSet::new();
    // Local spans with no duration: real observations that no rung of the
    // ladder can pay, tracked so the gap is visible rather than inferred
    // from a total that doesn't add up.
    let degenerate: Vec<&Span> = tree
        .spans
        .iter()
        .filter(|s| {
            s.locus != af_events::ExecutionLocus::Remote
                && s.t_end <= s.t_start
                && s.tool_name != BOOTSTRAP_TOOL_NAME
        })
        .collect();
    let mut degenerate_seen: HashSet<&str> = HashSet::new();

    for (sample_index, sample) in samples.iter().enumerate() {
        let (Some(t0), Some(t1)) = (parse_ts(&sample.t_start), parse_ts(&sample.t_end)) else {
            out.skipped_events += 1;
            continue;
        };
        let energy_j = sample_energy_j(sample);
        // A blank trace for this sample, filled in as the policy decides.
        // Built up-front so every early `continue` below still emits one:
        // a sample that was placed entirely in baseline is a decision the
        // debug view must be able to show, not an absence.
        let mut this = SampleTrace {
            sample_index,
            t_start: sample.t_start.clone(),
            t_end: sample.t_end.clone(),
            total_j: energy_j,
            denominator_cpu_ms: 0.0,
            // `None` until a policy is actually applied below. Every early
            // `continue` in this loop leaves it `None`, which is the point:
            // those samples were apportioned by nothing.
            policy: None,
            rows: Vec::new(),
            orphaned_j: 0.0,
            baseline_j: 0.0,
            l1_shadow_sum_share: 0.0,
            root_cpu_delta_ms: 0.0,
        };

        if t1 <= t0 {
            out.degenerate_samples += 1;
            out.baseline_idle_j += energy_j;
            this.baseline_j = energy_j;
            traces.push(this);
            continue;
        }
        let window_ms = (t1 - t0) as f64;
        this.denominator_cpu_ms = window_ms;

        for span in &remote {
            if span.overlaps(t0, t1) {
                remote_seen.insert(span.span_id.as_str());
                // Remote spans appear in the trace so the developer can see
                // they overlapped, with zero joules and the reason they got
                // none.
                this.rows.push(AllocRow {
                    span_id: span.span_id.clone(),
                    tool_name: span.tool_name.clone(),
                    locus: span.locus,
                    overlap_ms: overlap_ms(span.t_start, span.t_end, t0, t1),
                    cpu_delta_ms: 0.0,
                    allocated_j: 0.0,
                    l1_allocated_j: 0.0,
                    excluded: true,
                    excluded_reason: Some(
                        "execution_locus: remote — not measured on this machine".to_string(),
                    ),
                });
            }
        }
        for span in &degenerate {
            if span.overlaps(t0, t1) {
                degenerate_seen.insert(span.span_id.as_str());
            }
        }

        let active: Vec<&Span> = attributable
            .iter()
            .copied()
            .filter(|s| s.overlaps(t0, t1))
            .collect();
        let covering: Vec<&ProcWindow> = windows
            .iter()
            .filter(|w| overlap_ms(w.t_start, w.t_end, t0, t1) > 0.0)
            .collect();
        // The sampler covered this window and listed nothing. Distinct from
        // "no process data at all" (which falls back to L1), and invisible
        // in the result otherwise: every weight is zero, so every span is
        // paid zero and the sample reads as genuine idleness rather than as
        // an absence of observation.
        if !covering.is_empty() && covering.iter().all(|w| w.processes.is_empty()) {
            out.unobserved_process_windows += 1;
        }

        let mut attributed = 0.0;
        // Joules paid to each active span out of *this* sample, for the
        // trace. The aggregate `per_span_j` accumulates across samples and
        // cannot be differenced back apart afterwards.
        let mut this_sample_j: BTreeMap<String, f64> = BTreeMap::new();
        let mut this_sample_cpu_ms: BTreeMap<String, f64> = BTreeMap::new();
        if !covering.is_empty() {
            let weights = cpu_weights(&covering, &active, &tree.root_pids, t0, t1);
            let total_w = weights.total();
            this.root_cpu_delta_ms = weights.root;
            if active.is_empty() && weights.orphan <= 0.0 {
                // Process data exists but names nothing this join can place
                // — no policy was applied, the sample is pure baseline.
                out.baseline_idle_j += energy_j;
                this.baseline_j = energy_j;
                traces.push(this);
                continue;
            }
            out.samples_l2 += 1;
            this.policy = Some(Policy::L2CpuTime);
            if total_w > 0.0 {
                // Single-core normalization: W cpu-ms against C wall-ms,
                // capped at the whole sample. Conservative for v1;
                // multi-core normalization is a future policy version.
                let active_j = energy_j * (total_w / window_ms).min(1.0);
                for (span_id, weight) in &weights.per_span {
                    let joules = active_j * (weight / total_w);
                    *out.per_span_j.entry(span_id.clone()).or_default() += joules;
                    attributed += joules;
                    *this_sample_j.entry(span_id.clone()).or_default() += joules;
                    *this_sample_cpu_ms.entry(span_id.clone()).or_default() += weight;
                }
                let orphan_j = active_j * (weights.orphan / total_w);
                out.orphaned_j += orphan_j;
                attributed += orphan_j;
                this.orphaned_j = orphan_j;
            }
        } else if !active.is_empty() {
            out.samples_l1 += 1;
            this.policy = Some(Policy::L1WallClock);
            let fractions: Vec<(&str, f64)> = active
                .iter()
                .map(|s| {
                    (
                        s.span_id.as_str(),
                        overlap_ms(s.t_start, s.t_end, t0, t1) / window_ms,
                    )
                })
                .collect();
            let claimed: f64 = fractions.iter().map(|(_, f)| f).sum();
            // Concurrent spans can claim more than 100% of the window;
            // scale down so the sample is never over-attributed.
            let scale = if claimed > 1.0 { 1.0 / claimed } else { 1.0 };
            for (span_id, fraction) in fractions {
                let joules = energy_j * fraction * scale;
                *out.per_span_j.entry(span_id.to_string()).or_default() += joules;
                attributed += joules;
                *this_sample_j.entry(span_id.to_string()).or_default() += joules;
            }
        } else {
            out.baseline_idle_j += energy_j;
            this.baseline_j = energy_j;
            traces.push(this);
            continue;
        }

        out.baseline_idle_j += energy_j - attributed;

        this.baseline_j = energy_j - attributed;
        for span in &active {
            let overlap = overlap_ms(span.t_start, span.t_end, t0, t1);
            let l1_share = overlap / window_ms;
            this.l1_shadow_sum_share += l1_share;
            this.rows.push(AllocRow {
                span_id: span.span_id.clone(),
                tool_name: span.tool_name.clone(),
                locus: span.locus,
                overlap_ms: overlap,
                cpu_delta_ms: this_sample_cpu_ms
                    .get(span.span_id.as_str())
                    .copied()
                    .unwrap_or(0.0),
                allocated_j: this_sample_j
                    .get(span.span_id.as_str())
                    .copied()
                    .unwrap_or(0.0),
                l1_allocated_j: energy_j * l1_share,
                excluded: false,
                excluded_reason: None,
            });
        }
        traces.push(this);
    }

    out.unmeasured_remote_spans = remote_seen.len() as u32;
    out.degenerate_spans = degenerate_seen.len() as u32;
    (out, traces)
}

/// CPU-time weights for one energy window `[t0, t1)`.
///
/// Two design-log facts shape this:
///
/// * **A pid may appear several times in one `process_sample`** (two spans
///   watching the same tree, or a span plus an earlier span's orphan tail),
///   with different deltas measured from different baselines. Summing them
///   would double-count physical cpu time, so entries are first
///   deduplicated per pid, keeping the *longest* observed delta (the most
///   complete view of that tree's cpu in the window).
/// * That deduplicated weight is then **split equally** between every
///   claimant of the pid: each attributable span overlapping the window
///   that lists the pid, plus the orphan bucket when any entry for the pid
///   is `orphan_of`-tagged. Equal split is the only defensible division
///   without per-span cpu isolation (that is L3's job).
///
/// A pid no overlapping span claims falls entirely to the orphan bucket —
/// it is real, observed cpu time that belongs to no action, which is
/// exactly what "orphaned compute" means; folding it into idle would hide
/// it and folding it into a span would invent an owner.
///
/// `root_pids` is the session's own process tree. A span that lists no pids
/// of its own claims it instead ([`claims_pid`]) — most collectors can name
/// the agent process but not the per-tool-call subtree, and without this
/// every one of their spans would be pid-less, fail to claim anything, and
/// send the whole session's measured cpu to the orphan bucket.
fn cpu_weights(
    windows: &[&ProcWindow],
    active: &[&Span],
    root_pids: &[i64],
    t0: i64,
    t1: i64,
) -> Weights {
    let mut weights = Weights::default();

    for window in windows {
        let fraction = overlap_ms(window.t_start, window.t_end, t0, t1)
            / (window.t_end - window.t_start) as f64;

        // pid → (longest observed cpu delta, any entry orphan-tagged)
        let mut by_pid: BTreeMap<i64, (u64, bool)> = BTreeMap::new();
        for process in window.processes {
            let entry = by_pid.entry(process.pid).or_insert((0, false));
            entry.0 = entry.0.max(process.cpu_time_delta_ms);
            entry.1 |= process.orphan_of.is_some();
        }

        for (pid, (cpu_ms, orphan_tagged)) in by_pid {
            if root_pids.contains(&pid) {
                weights.root += cpu_ms as f64 * fraction;
            }
            let claimants: Vec<&SpanId> = active
                .iter()
                .filter(|s| claims_pid(s, pid, root_pids))
                .map(|s| &s.span_id)
                .collect();
            let to_orphan = orphan_tagged || claimants.is_empty();
            let share =
                cpu_ms as f64 * fraction / (claimants.len() + usize::from(to_orphan)) as f64;
            for span_id in claimants {
                *weights.per_span.entry(span_id.clone()).or_default() += share;
            }
            if to_orphan {
                weights.orphan += share;
            }
        }
    }

    weights
}

/// Whether `span` claims `pid`'s cpu time.
///
/// **Root-tree inheritance:** a span that lists pids owns exactly those and
/// nothing else — an explicit pid list is a measurement, and widening it to
/// the root tree would hand a span cpu it was observed not to own. A span
/// with an *empty* list has no measurement at all, and inherits the
/// session's `root_pids` (the agent process tree, from the collector's
/// bootstrap span). That is what makes L2 reachable for a collector that
/// can only see the agent's own pid; concurrent pid-less spans then split
/// the root tree equally under the ordinary shared-pid rule, which is the
/// same "no cpu isolation, so divide evenly" honesty. With no root pids the
/// span claims nothing and its window's cpu falls to the orphan bucket.
///
/// Public because it is a *methodology* rule, not an implementation
/// detail: any surface that explains why a span was (or was not) paid for
/// a pid's cpu time has to answer with this exact predicate, and a second
/// copy of it elsewhere would be a rival definition of ownership.
pub fn claims_pid(span: &Span, pid: i64, root_pids: &[i64]) -> bool {
    if span.pids.is_empty() {
        root_pids.contains(&pid)
    } else {
        span.pids.contains(&pid)
    }
}

/// Overlap of `[a0, a1)` and `[b0, b1)` in milliseconds, `0.0` if disjoint.
fn overlap_ms(a0: i64, a1: i64, b0: i64, b1: i64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_events::{EnergyComponent, EnergyMethod};

    fn component(kind: EnergyKind, energy_j: f64) -> EnergyComponent {
        EnergyComponent {
            kind,
            label: None,
            energy_j,
            method: EnergyMethod::Rapl,
        }
    }

    #[test]
    fn overlap_is_half_open_and_never_negative() {
        assert_eq!(overlap_ms(0, 10, 5, 20), 5.0);
        assert_eq!(overlap_ms(0, 10, 10, 20), 0.0);
        assert_eq!(overlap_ms(0, 10, 20, 30), 0.0);
        assert_eq!(overlap_ms(0, 100, 10, 20), 10.0);
    }

    #[test]
    fn total_component_wins_over_the_parts() {
        let sample = EnergySample {
            t_start: "2026-07-25T12:00:00Z".into(),
            t_end: "2026-07-25T12:00:01Z".into(),
            components: vec![
                component(EnergyKind::Cpu, 10.0),
                component(EnergyKind::Dram, 2.0),
                component(EnergyKind::Total, 11.5),
            ],
            host_id: None,
        };
        assert_eq!(sample_energy_j(&sample), 11.5);
    }

    #[test]
    fn only_pid_less_spans_inherit_the_root_tree() {
        let span = Span {
            span_id: "s".into(),
            tool_name: "Bash".into(),
            tool_kind: af_events::ToolKind::Bash,
            locus: af_events::ExecutionLocus::Local,
            t_start: 0,
            t_end: 1000,
            pids: vec![],
            attribution: None,
        };
        assert!(claims_pid(&span, 10, &[10, 11]));
        assert!(!claims_pid(&span, 12, &[10, 11]));
        // No root pids to inherit: a pid-less span claims nothing.
        assert!(!claims_pid(&span, 10, &[]));

        let owns = Span {
            pids: vec![11],
            ..span
        };
        assert!(claims_pid(&owns, 11, &[10]));
        assert!(!claims_pid(&owns, 10, &[10]), "own pids are not widened");
    }

    #[test]
    fn policy_ids_are_stable() {
        assert_eq!(Policy::L2CpuTime.id(), "l2_cpu_time/v1");
        assert_eq!(Policy::L1WallClock.id(), "l1_wall_clock/v1");
        assert_eq!(Policy::default(), Policy::L1WallClock);
    }

    #[test]
    fn schema_name_strips_the_version_suffix() {
        assert_eq!(Policy::L2CpuTime.schema_name(), "l2_cpu_time");
        assert_eq!(Policy::L1WallClock.schema_name(), "l1_wall_clock");
    }

    #[test]
    fn policy_falls_back_to_l1_only_when_nothing_was_apportioned() {
        let nothing = Apportionment::default();
        assert_eq!(nothing.applied_policy(), None);
        assert_eq!(nothing.policy(), Policy::L1WallClock);

        let l1 = Apportionment {
            samples_l1: 1,
            ..Default::default()
        };
        assert_eq!(l1.policy(), Policy::L1WallClock);

        // L2 wins as soon as one sample was weighted by process data.
        let both = Apportionment {
            samples_l1: 3,
            samples_l2: 1,
            ..Default::default()
        };
        assert_eq!(both.policy(), Policy::L2CpuTime);
    }
}
