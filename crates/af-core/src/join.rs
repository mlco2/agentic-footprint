//! Contract #2 `impact_join` assembly: locally *measured* energy (from
//! [`crate::attribution`]) joined with remotely *estimated* LLM impacts
//! (from `impact_estimates`) for one attribution unit.
//!
//! Records built here validate against `schemas/v0.1/derived.schema.json`'s
//! `$defs/impact_join`. That schema constrains no `additionalProperties`,
//! which this module uses deliberately: the honesty counters, the local
//! energy split and the zone provenance have nowhere else to live, and
//! dropping them would leave a reader unable to tell a measured zero from
//! an unmeasured one.
//!
//! # Units
//!
//! * **session** — one per session. Its `local_measured.energy` is the
//!   *whole machine* measurement over the session: attributed + orphaned +
//!   baseline. The session is the interval, and the machine ran the whole
//!   interval; charging the session only its attributed slice would make
//!   the measured joules disappear. The split is exposed verbatim in
//!   `local_measured.breakdown_j`.
//! * **tool_call** — one per attributable span, keyed by the collector's
//!   `span_id`. Its `local_measured.energy` is only what the attribution
//!   policy handed that span. `unit.tool_call_id` is set only when the
//!   collector actually attributed one; a span id is not silently promoted
//!   into one.
//! * **task** — one per distinct `attribution.task_id` across the session's
//!   attributable spans. Rare in this PoC (no collector emits `task_id`
//!   yet), which is why it is derived rather than special-cased.
//!
//! Remote spans and zero-length spans get no unit of their own: neither can
//! receive local energy, and a unit reporting `0 J` is indistinguishable
//! from one that was measured and found idle. They are counted instead —
//! `unmeasured_remote_spans` (a schema field) and `counters.degenerate_spans`.
//!
//! # Combined totals
//!
//! `combined_total` crosses measurement paradigms, so a criterion is
//! emitted only when *both* halves are actually available: the local side
//! (energy needs at least one *parsed* energy sample; gwp additionally
//! needs the zone's emission factor from the estimator sidecar) **and** the
//! remote side (every `llm_call` in the unit estimated `ok`, *and* every
//! one of those `ok` estimates having reported that criterion in one
//! consistent unit; a unit with no `llm_call` at all is vacuously complete
//! and contributes zero). Anything else and the criterion is omitted — a
//! "combined" total silently missing one of its halves is a wrong number,
//! not a partial one.
//!
//! # Summing remote criteria
//!
//! Two guards sit on the remote sum, because a sum is only meaningful over
//! like quantities measured the same way:
//!
//! * **Unit mismatch.** If two `ok` estimates report the same criterion in
//!   different units (`kWh` and `Wh`), the criterion is dropped from
//!   `remote_estimated.impacts` *entirely* — a wrong-unit sum must never
//!   render, not even as a partial. The conflict is counted instead, in
//!   `remote_estimated.unit_mismatches`.
//! * **Partial coverage.** An `ok` estimate that simply omits a criterion
//!   leaves the running sum a *subtotal*. The subtotal is still reported in
//!   `impacts` (it is a real lower bound over the estimates that had it),
//!   but the criterion is withheld from `combined_total`, exactly as an
//!   `unknown_model` call withholds everything.
//!
//! # Remote-region staleness
//!
//! Every stored estimate is stamped with its remote-region policy (see
//! [`crate::estimate_pending`]). An explicit override different from a stored
//! region does **not** silently re-label the estimate: the compatibility
//! counter `remote_estimated.stale_zone_estimates` is retained and
//! [`crate::rebuild_derived`] names both remote regions on stderr. The local
//! grid zone is independent and never creates remote staleness.

use std::collections::{BTreeMap, BTreeSet};

use af_events::{Envelope, Payload};
use af_sidecar::Sidecar;
use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::attribution::{Apportionment, Policy};
use crate::correlate::{parse_ts, rfc3339_ms, SessionTree, Span};

/// Joules in one kilowatt-hour.
const J_PER_KWH: f64 = 3.6e6;

/// Electricity-mix factors for one geographic zone, as returned by the
/// `zone_factors` op of the `af_estimator` sidecar. Only the gwp factor is
/// used by the join today; the others exist on the wire already and are
/// deliberately not re-derived here (methodology stays in one place).
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneFactors {
    /// `kgCO2eq` per kWh, as a range (ecologits reports a point value,
    /// which arrives as `min == max`).
    pub gwp_min: f64,
    pub gwp_max: f64,
}

/// The electricity-mix zone one join pass ran under, and where it came from.
///
/// The provenance is recorded on every record because a zone that was
/// *defaulted* and one the user *declared* produce numerically identical
/// gwp figures with entirely different trustworthiness.
#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    /// Zone identifier passed to the estimator, e.g. `FRA`, `WOR`.
    pub id: String,
    /// How the id was resolved: `flag`, `env`, `session_meta` or `default`.
    pub source: String,
    /// `None` when the estimator sidecar was unavailable or did not know
    /// the zone — the join then reports no local gwp at all rather than
    /// inventing a factor.
    pub factors: Option<ZoneFactors>,
}

impl Zone {
    /// A zone with no usable emission factor — the degraded path.
    pub fn unresolved(id: impl Into<String>, source: impl Into<String>) -> Self {
        Zone {
            id: id.into(),
            source: source.into(),
            factors: None,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "source": self.source,
            "factors_available": self.factors.is_some(),
        })
    }
}

/// One assembled join: the store key it lives under plus the Contract #2
/// record itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactJoin {
    /// `session:<id>`, `task:<id>:<task_id>` or `tool_call:<id>:<span_id>`.
    pub unit_key: String,
    /// Schema-valid `impact_join` record.
    pub record: Value,
}

/// Asks the estimator sidecar for `zone`'s electricity-mix factors.
///
/// Returns `Ok(None)` when the sidecar answers anything other than
/// `status: "ok"` (e.g. `missing_zone`) — an unknown zone is a fact to
/// report, not an error to abort the whole report over. A transport failure
/// (dead sidecar, timeout) *is* returned as `Err`, since it says nothing
/// about the zone.
pub fn fetch_zone_factors(sidecar: &mut Sidecar, zone: &str) -> Result<Option<ZoneFactors>> {
    let response = sidecar
        .request(&json!({"op": "zone_factors", "zone": zone}))
        .with_context(|| format!("zone_factors request for zone {zone}"))?;

    if response.get("status").and_then(Value::as_str) != Some("ok") {
        return Ok(None);
    }
    let (Some(min), Some(max)) = (
        response
            .pointer("/gwp_kg_per_kwh/min")
            .and_then(Value::as_f64),
        response
            .pointer("/gwp_kg_per_kwh/max")
            .and_then(Value::as_f64),
    ) else {
        return Ok(None);
    };
    Ok(Some(ZoneFactors {
        gwp_min: min,
        gwp_max: max,
    }))
}

/// Builds every `impact_join` record for one session, sorted by `unit_key`.
///
/// `events` is the session's full raw event list, `tree` the
/// [`crate::correlate`] of *those same events*, `apportionment` the result
/// of [`crate::apportion`] over both, `estimates` the stored
/// `impact_estimates` blobs keyed by `llm_call` event id (an absent key is
/// an un-estimated call, reported as `pending`), and `t_start`/`t_end` the
/// session's first/last event timestamps, carried through verbatim rather
/// than reformatted.
///
/// The tree is a **parameter rather than something recomputed here**: every
/// caller already has one (apportionment cannot be produced without it), so
/// correlating a second time was pure duplicated work — and, worse, a
/// second chance for the join's view of the spans to differ from the view
/// the energy was actually divided against. Passing a tree built from other
/// events is the one way to misuse this function, and it is the caller's
/// obligation not to.
///
/// Deterministic by construction: every iteration order is over a
/// `BTree*` or an explicitly sorted vector, so identical inputs produce
/// byte-identical output (float summation order included).
// Eight arguments, and each one is a distinct input the record depends on:
// bundling them into a struct would move the same list one line up and buy
// nothing but a second name for it.
#[allow(clippy::too_many_arguments)]
pub fn build_joins(
    session_id: &str,
    t_start: &str,
    t_end: &str,
    events: &[Envelope],
    tree: &SessionTree,
    apportionment: &Apportionment,
    estimates: &BTreeMap<String, Value>,
    zone: &Zone,
    remote_region_id: Option<&str>,
) -> Vec<ImpactJoin> {
    let coverage_windows = energy_coverage_windows(events);
    let energy_measured = any_energy_sample_parsed(events);
    let calls = llm_calls(events);

    let attributable: Vec<&Span> = tree.spans.iter().filter(|s| s.is_attributable()).collect();
    let mut out = Vec::new();

    // Everything a per-unit join needs that does not vary from unit to
    // unit, gathered once. What is left as an argument to
    // `span_group_join` is exactly what actually differs per unit.
    let ctx = JoinCtx {
        apportionment,
        coverage_windows: &coverage_windows,
        energy_measured,
        calls: &calls,
        estimates,
        zone,
        remote_region_id,
    };

    // ---- session unit ----------------------------------------------------
    let span_ids: BTreeSet<&str> = attributable.iter().map(|s| s.span_id.as_str()).collect();
    let attributed_j: f64 = span_ids.iter().map(|id| apportionment.span_j(id)).sum();
    let session_j = attributed_j + apportionment.orphaned_j + apportionment.baseline_idle_j;

    let session_local = if !energy_measured {
        // Nothing measured the machine during this session. `coverage: 0`
        // is the whole story; an `energy: 0 kWh` would be a fabrication.
        local_measured(None, 0.0, zone, None)
    } else {
        let coverage = match (parse_ts(t_start), parse_ts(t_end)) {
            (Some(t0), Some(t1)) => coverage_fraction(&coverage_windows, t0, t1),
            _ => 0.0,
        };
        local_measured(
            Some(session_j),
            coverage,
            zone,
            Some(json!({
                "attributed": attributed_j,
                "baseline_idle": apportionment.baseline_idle_j,
                "orphaned": apportionment.orphaned_j,
                "total": session_j,
            })),
        )
    };

    let session_remote = remote_estimated(&calls, estimates, remote_region_id, |_| true);
    let mut record = record_base(
        json!({"level": "session", "session_id": session_id}),
        t_start,
        t_end,
        apportionment.policy(),
        zone,
    );
    record.insert(
        "unmeasured_remote_spans".into(),
        json!(apportionment.unmeasured_remote_spans),
    );
    record.insert(
        "counters".into(),
        json!({
            "degenerate_spans": apportionment.degenerate_spans,
            "degenerate_samples": apportionment.degenerate_samples,
            "samples_l1": apportionment.samples_l1,
            "samples_l2": apportionment.samples_l2,
            "skipped_events": apportionment.skipped_events + tree.skipped_events,
            // Windows the sampler covered but enumerated nothing in. They
            // are counted in `samples_l2` above and paid every overlapping
            // span zero, so without this the report reads as "those spans
            // used no energy" rather than "nothing was observed".
            "unobserved_process_windows": apportionment.unobserved_process_windows,
        }),
    );
    finish(&mut record, session_local, session_remote, !energy_measured);
    out.push(ImpactJoin {
        unit_key: format!("session:{session_id}"),
        record: Value::Object(record),
    });

    // ---- tool_call units, one per attributable span ----------------------
    let mut by_span: BTreeMap<&str, Vec<&Span>> = BTreeMap::new();
    for span in &attributable {
        by_span.entry(span.span_id.as_str()).or_default().push(span);
    }
    for (span_id, spans) in &by_span {
        let tool_call_id = spans
            .iter()
            .find_map(|s| s.attribution.as_ref()?.tool_call_id.clone());
        let mut unit = Map::new();
        unit.insert("level".into(), json!("tool_call"));
        unit.insert("session_id".into(), json!(session_id));
        unit.insert("span_id".into(), json!(span_id));
        if let Some(id) = &tool_call_id {
            unit.insert("tool_call_id".into(), json!(id));
        }
        let matcher = |call: &LlmCallRef| match (&tool_call_id, &call.tool_call_id) {
            (Some(unit_id), Some(call_id)) => unit_id == call_id,
            _ => false,
        };
        if let Some(join) = span_group_join(
            &ctx,
            &format!("tool_call:{session_id}:{span_id}"),
            Value::Object(unit),
            spans,
            matcher,
        ) {
            out.push(join);
        }
    }

    // ---- task units, one per distinct attribution.task_id ----------------
    let mut by_task: BTreeMap<String, Vec<&Span>> = BTreeMap::new();
    for span in &attributable {
        if let Some(task_id) = span.attribution.as_ref().and_then(|a| a.task_id.clone()) {
            by_task.entry(task_id).or_default().push(span);
        }
    }
    for (task_id, spans) in &by_task {
        let unit = json!({"level": "task", "session_id": session_id, "task_id": task_id});
        let matcher = |call: &LlmCallRef| call.task_id.as_deref() == Some(task_id.as_str());
        if let Some(join) = span_group_join(
            &ctx,
            &format!("task:{session_id}:{task_id}"),
            unit,
            spans,
            matcher,
        ) {
            out.push(join);
        }
    }

    out.sort_by(|a, b| a.unit_key.cmp(&b.unit_key));
    out
}

/// The session-wide inputs every per-unit join shares, borrowed for the
/// length of one [`build_joins`] pass.
///
/// They are loop-invariant by construction — the apportionment, the
/// coverage windows, the session's `llm_call`s and the zone are properties
/// of the *session*, not of the unit being assembled — so passing them
/// individually to every call made the signature's arity say nothing about
/// what actually varies.
struct JoinCtx<'a> {
    apportionment: &'a Apportionment,
    coverage_windows: &'a [(i64, i64)],
    energy_measured: bool,
    calls: &'a [LlmCallRef],
    estimates: &'a BTreeMap<String, Value>,
    zone: &'a Zone,
    remote_region_id: Option<&'a str>,
}

/// Assembles the join for a unit backed by a group of spans (`tool_call`
/// and `task` alike).
///
/// Returns `None` when the group's bounds cannot be formatted back to
/// RFC 3339 — unreachable in practice (they were parsed *from* RFC 3339),
/// but a record with a fabricated interval would be worse than no record.
fn span_group_join(
    ctx: &JoinCtx<'_>,
    unit_key: &str,
    unit: Value,
    spans: &[&Span],
    matcher: impl Fn(&LlmCallRef) -> bool,
) -> Option<ImpactJoin> {
    let t0 = spans.iter().map(|s| s.t_start).min()?;
    let t1 = spans.iter().map(|s| s.t_end).max()?;

    // Distinct span ids only: two events sharing a span_id were merged into
    // one apportionment entry, so summing per event would double-count.
    let ids: BTreeSet<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();
    let joules: f64 = ids.iter().map(|id| ctx.apportionment.span_j(id)).sum();

    let local = if !ctx.energy_measured {
        local_measured(None, 0.0, ctx.zone, None)
    } else {
        local_measured(
            Some(joules),
            coverage_fraction(ctx.coverage_windows, t0, t1),
            ctx.zone,
            None,
        )
    };

    let mut record = record_base(
        unit,
        &rfc3339_ms(t0)?,
        &rfc3339_ms(t1)?,
        ctx.apportionment.policy(),
        ctx.zone,
    );
    finish(
        &mut record,
        local,
        remote_estimated(ctx.calls, ctx.estimates, ctx.remote_region_id, matcher),
        !ctx.energy_measured,
    );
    Some(ImpactJoin {
        unit_key: unit_key.to_string(),
        record: Value::Object(record),
    })
}

/// The fields every record carries regardless of unit level.
///
/// `attribution_policy` holds the schema's enum value (`l2_cpu_time`),
/// which has no version suffix; the full versioned policy id from
/// [`Policy::id`] (`l2_cpu_time/v1`) rides alongside in
/// `attribution_policy_id` so a record stays re-computable when the policy
/// mints a v2.
fn record_base(
    unit: Value,
    t_start: &str,
    t_end: &str,
    policy: Policy,
    zone: &Zone,
) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert("unit".into(), unit);
    record.insert("t_start".into(), json!(t_start));
    record.insert("t_end".into(), json!(t_end));
    record.insert("attribution_policy".into(), json!(policy.schema_name()));
    record.insert("attribution_policy_id".into(), json!(policy.id()));
    record.insert("zone".into(), zone.to_json());
    record
}

/// Attaches `local_measured`/`remote_estimated`/`combined_total`.
fn finish(
    record: &mut Map<String, Value>,
    local: LocalMeasured,
    remote: RemoteEstimated,
    no_local_energy: bool,
) {
    let combined = combined_total(&local, &remote, no_local_energy);
    record.insert("local_measured".into(), local.json);
    record.insert("remote_estimated".into(), remote.json);
    record.insert("combined_total".into(), combined);
}

/// The local half of a join, plus the two numbers `combined_total` needs.
struct LocalMeasured {
    json: Value,
    kwh: Option<f64>,
    gwp: Option<(f64, f64)>,
}

/// Builds `local_measured` from `joules` (`None` = nothing was measured).
fn local_measured(
    joules: Option<f64>,
    coverage: f64,
    zone: &Zone,
    breakdown: Option<Value>,
) -> LocalMeasured {
    let mut obj = Map::new();
    let mut kwh = None;
    let mut gwp = None;

    if let Some(joules) = joules {
        let value = joules / J_PER_KWH;
        kwh = Some(value);
        // A measurement is a point value; the schema's own convention is
        // `min == max` for exactly this.
        obj.insert("energy".into(), criterion("kWh", value, value));
        if let Some(factors) = &zone.factors {
            let range = (value * factors.gwp_min, value * factors.gwp_max);
            gwp = Some(range);
            obj.insert("gwp".into(), criterion("kgCO2eq", range.0, range.1));
        }
        if let Some(breakdown) = breakdown {
            obj.insert("breakdown_j".into(), breakdown);
        }
    }

    // Always stated, both of them: idle is separated out by the policy
    // whether or not any energy was measured, and a coverage of 0 is the
    // fact that distinguishes "no local data" from "no local consumption".
    obj.insert("baseline_share_excluded".into(), json!(true));
    obj.insert("coverage".into(), json!(coverage.clamp(0.0, 1.0)));

    LocalMeasured {
        json: Value::Object(obj),
        kwh,
        gwp,
    }
}

/// The remote half of a join, plus what `combined_total` needs to decide
/// whether the two halves may be added at all.
struct RemoteEstimated {
    json: Value,
    /// Summed `total` ranges by criterion name — **only** the criteria that
    /// every `ok` estimate in the unit reported, all in one unit. A
    /// criterion that is missing from this map is one `combined_total`
    /// must refuse, whatever `impacts` shows.
    totals: BTreeMap<String, (String, f64, f64)>,
    /// Every `llm_call` in this unit was estimated `ok` (vacuously true for
    /// a unit with no calls).
    complete: bool,
    calls: usize,
}

/// One criterion's running sum across a set of `ok` estimates, with the two
/// facts that decide whether the sum may be published and whether it may be
/// combined with a local half.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionSum {
    /// The unit of the *first* estimate that reported this criterion.
    pub unit: String,
    pub min: f64,
    pub max: f64,
    /// Estimates that contributed to `min`/`max`. Compare against the
    /// number of estimates summed: fewer contributors means the sum is a
    /// *subtotal*, real as a lower bound but not a total.
    pub contributors: usize,
    /// Estimates that reported this criterion in a *different* unit and
    /// were therefore never added. Non-zero means the sum must not be
    /// published at all — adding kWh to Wh produces a number that is wrong
    /// in a way no consumer could detect.
    pub mismatches: usize,
}

/// The criteria summed across a unit's estimates, in schema order.
const CRITERIA: [&str; 5] = ["adpe", "energy", "gwp", "pe", "water"];

/// Sums each criterion's `total` range across `estimates`, keyed by
/// criterion name in schema order.
///
/// `estimates` are the stored estimate blobs already known to be usable —
/// `status: "ok"` *and* carrying an `impacts` object. Filtering is the
/// caller's job because "this call was not estimated" is a fact about the
/// call, not about the arithmetic, and the caller has to tally those
/// statuses anyway.
///
/// Only each criterion's `total` range is summed. The usage/embodied
/// life-cycle split is a property of one estimate's methodology; adding
/// splits across estimates that may not all provide them would produce a
/// subtotal that doesn't reconcile with its own total.
///
/// Deterministic: `estimates` is walked in the given order and criteria in
/// [`CRITERIA`] order, so float summation happens identically on every run.
/// The two guards a caller must honour before publishing a sum are
/// reported, not applied, in [`CriterionSum::mismatches`] and
/// [`CriterionSum::contributors`].
pub fn sum_criteria(estimates: &[&Value]) -> BTreeMap<String, CriterionSum> {
    let mut sums: BTreeMap<String, CriterionSum> = BTreeMap::new();

    for estimate in estimates {
        let Some(impacts) = estimate.get("impacts") else {
            continue;
        };
        for name in CRITERIA {
            let Some(criterion) = impacts.get(name) else {
                // An `ok` estimate that simply doesn't report this criterion
                // leaves the sum a subtotal; `contributors` records that.
                continue;
            };
            let (Some(unit), Some(min), Some(max)) = (
                criterion.get("unit").and_then(Value::as_str),
                criterion.pointer("/total/min").and_then(Value::as_f64),
                criterion.pointer("/total/max").and_then(Value::as_f64),
            ) else {
                continue;
            };
            match sums.get_mut(name) {
                None => {
                    sums.insert(
                        name.to_string(),
                        CriterionSum {
                            unit: unit.to_string(),
                            min,
                            max,
                            contributors: 1,
                            mismatches: 0,
                        },
                    );
                }
                // Adding kWh to Wh produces a number that is wrong in a way
                // no consumer could detect. Refuse the addend outright.
                Some(sum) if sum.unit != unit => sum.mismatches += 1,
                Some(sum) => {
                    sum.min += min;
                    sum.max += max;
                    sum.contributors += 1;
                }
            }
        }
    }

    sums
}

/// Builds `remote_estimated` over the calls `matcher` accepts: the status
/// tally, the [`sum_criteria`] arithmetic, and the two guards that decide
/// what may be published and what may be combined.
///
/// `zone_id` is the zone *this pass* runs under, used only to count
/// estimates that were computed under a different one (see the module
/// docs); their numbers are reported unchanged, since silently re-labelling
/// them would be worse than saying they are stale.
fn remote_estimated(
    calls: &[LlmCallRef],
    estimates: &BTreeMap<String, Value>,
    remote_region_id: Option<&str>,
    matcher: impl Fn(&LlmCallRef) -> bool,
) -> RemoteEstimated {
    let mut statuses: BTreeMap<String, usize> = BTreeMap::new();
    // The estimates whose numbers may be summed, in call order — status
    // `ok` *and* carrying an `impacts` object.
    let mut summable: Vec<&Value> = Vec::new();
    let mut count = 0usize;
    let mut stale_zone = 0usize;
    let mut complete = true;

    for call in calls.iter().filter(|c| matcher(c)) {
        count += 1;
        let estimate = estimates.get(&call.event_id);
        let status = match estimate {
            // An absent row is an un-estimated call, which the schema's
            // `estimation_status` enum already has a name for.
            None => "pending".to_string(),
            Some(value) => value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_string(),
        };
        *statuses.entry(status.clone()).or_default() += 1;

        if let Some(expected) = remote_region_id {
            if estimate
                .and_then(estimate_zone)
                .is_some_and(|region| region != expected)
            {
                stale_zone += 1;
            }
        }

        match estimate {
            Some(value) if status == "ok" && value.get("impacts").is_some() => summable.push(value),
            _ => complete = false,
        }
    }

    let ok_estimates = summable.len();
    let sums = sum_criteria(&summable);

    let mut impacts = Map::new();
    let mut mismatches = Map::new();
    let mut totals: BTreeMap<String, (String, f64, f64)> = BTreeMap::new();
    for (name, sum) in &sums {
        if sum.mismatches > 0 {
            // A wrong-unit sum must never render — not even as a partial.
            mismatches.insert(name.clone(), json!(sum.mismatches));
            continue;
        }
        impacts.insert(name.clone(), criterion(&sum.unit, sum.min, sum.max));
        // Fewer contributors than `ok` estimates means this is a subtotal:
        // still a real lower bound to report in `impacts`, but never a
        // total, so it is withheld from `combined_total`.
        if sum.contributors == ok_estimates {
            totals.insert(name.clone(), (sum.unit.clone(), sum.min, sum.max));
        }
    }

    let mut obj = Map::new();
    obj.insert("llm_calls".into(), json!(count));
    if !statuses.is_empty() {
        obj.insert(
            "estimate_status_counts".into(),
            Value::Object(
                statuses
                    .into_iter()
                    .map(|(k, v)| (k, json!(v)))
                    .collect::<Map<String, Value>>(),
            ),
        );
    }
    if !impacts.is_empty() {
        obj.insert("impacts".into(), Value::Object(impacts));
    }
    if !mismatches.is_empty() {
        obj.insert("unit_mismatches".into(), Value::Object(mismatches));
    }
    if stale_zone > 0 {
        obj.insert("stale_zone_estimates".into(), json!(stale_zone));
    }

    RemoteEstimated {
        json: Value::Object(obj),
        totals,
        complete,
        calls: count,
    }
}

/// The remote region a stored estimate was computed under. New rows use the
/// structured `remote_region` stamp; the legacy `zone` string remains
/// readable so existing databases retain their audit semantics.
fn estimate_zone(estimate: &Value) -> Option<&str> {
    estimate
        .get("remote_region")
        .and_then(|region| region.get("id"))
        .and_then(Value::as_str)
        .or_else(|| estimate.get("zone").and_then(Value::as_str))
}

/// Counts stored estimates whose stamped remote region differs from
/// `region_id`, keyed by the stale id so a warning can name both regions.
///
/// Used by [`crate::rebuild_derived`] to warn once per session; the join
/// records carry the same count per unit in
/// `remote_estimated.stale_zone_estimates`.
pub fn stale_zone_estimates(
    estimates: &BTreeMap<String, Value>,
    region_id: &str,
) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for estimate in estimates.values() {
        if let Some(stamped) = estimate_zone(estimate) {
            if stamped != region_id {
                *out.entry(stamped.to_string()).or_default() += 1;
            }
        }
    }
    out
}

/// `local + remote` for the two criteria the schema allows, emitted only
/// when both halves are genuinely available (see the module docs).
fn combined_total(local: &LocalMeasured, remote: &RemoteEstimated, no_local_energy: bool) -> Value {
    let mut obj = Map::new();
    if no_local_energy || !remote.complete {
        return Value::Object(obj);
    }

    // A unit with no llm_calls contributes an exact zero on the remote side
    // — that is a complete remote half, not a missing one.
    let remote_range = |name: &str, unit: &str| -> Option<(f64, f64)> {
        match remote.totals.get(name) {
            Some((got_unit, min, max)) if got_unit == unit => Some((*min, *max)),
            Some(_) => None, // unit mismatch: refuse to add unlike quantities
            None if remote.calls == 0 => Some((0.0, 0.0)),
            None => None,
        }
    };

    if let (Some(kwh), Some((min, max))) = (local.kwh, remote_range("energy", "kWh")) {
        obj.insert("energy".into(), criterion("kWh", kwh + min, kwh + max));
    }
    if let (Some((lo, hi)), Some((min, max))) = (local.gwp, remote_range("gwp", "kgCO2eq")) {
        obj.insert("gwp".into(), criterion("kgCO2eq", lo + min, hi + max));
    }
    Value::Object(obj)
}

fn criterion(unit: &str, min: f64, max: f64) -> Value {
    json!({"unit": unit, "total": {"min": min, "max": max}})
}

/// One `llm_call` reduced to what the join needs, in a deterministic order.
struct LlmCallRef {
    event_id: String,
    task_id: Option<String>,
    tool_call_id: Option<String>,
}

/// The session's `llm_call` events, sorted by `(ts, event_id)` so that
/// float summation happens in the same order on every run — `ts` alone
/// admits ties, and SQLite makes no promise about how it breaks them.
fn llm_calls(events: &[Envelope]) -> Vec<LlmCallRef> {
    let mut keyed: Vec<(&str, &str, LlmCallRef)> = events
        .iter()
        .filter(|e| matches!(e.payload, Payload::LlmCall(_)))
        .map(|e| {
            (
                e.ts.as_str(),
                e.event_id.as_str(),
                LlmCallRef {
                    event_id: e.event_id.clone(),
                    task_id: e.attribution.as_ref().and_then(|a| a.task_id.clone()),
                    tool_call_id: e.attribution.as_ref().and_then(|a| a.tool_call_id.clone()),
                },
            )
        })
        .collect();
    keyed.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    keyed.into_iter().map(|(_, _, call)| call).collect()
}

/// Whether the session has at least one `energy_sample` the join could
/// parse — the predicate that decides whether a local half exists at all.
///
/// Deliberately *not* "the union coverage window is non-empty". A session
/// whose only samples are zero-length still measured the machine, and
/// `l2_cpu_time/v1` books their joules as baseline idle; keying the local
/// half on the coverage window would delete those joules from the record
/// while `coverage: 0` claimed nothing had been seen. A sample whose
/// timestamps don't parse is a different case — its joules never entered
/// the apportionment (it is counted in `counters.skipped_events`), so it
/// must not vouch for a local half either.
fn any_energy_sample_parsed(events: &[Envelope]) -> bool {
    events.iter().any(|e| match &e.payload {
        Payload::EnergySample(sample) => {
            parse_ts(&sample.t_start).is_some() && parse_ts(&sample.t_end).is_some()
        }
        _ => false,
    })
}

/// The union of the session's energy-sample windows, as sorted,
/// non-overlapping `[start, end)` intervals in epoch milliseconds.
///
/// Union, not sum: overlapping samples (two collectors, a restarted
/// sampler) would otherwise push coverage past 1 and hide the fact that a
/// stretch of the session was never measured at all. Samples that carry no
/// joules still count as coverage — they measured the machine and found
/// nothing, which is exactly what coverage is meant to capture.
fn energy_coverage_windows(events: &[Envelope]) -> Vec<(i64, i64)> {
    let mut windows: Vec<(i64, i64)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::EnergySample(sample) => {
                // Touch `sample_energy_j` nowhere here: a zero-joule sample
                // is still coverage.
                let t0 = parse_ts(&sample.t_start)?;
                let t1 = parse_ts(&sample.t_end)?;
                (t1 > t0).then_some((t0, t1))
            }
            _ => None,
        })
        .collect();
    windows.sort_unstable();

    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(windows.len());
    for (start, end) in windows {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Fraction of `[t0, t1)` covered by `windows`, clamped to `0..=1`.
/// A non-positive interval has no wall time to cover, so it reports `0`
/// rather than dividing by zero.
fn coverage_fraction(windows: &[(i64, i64)], t0: i64, t1: i64) -> f64 {
    if t1 <= t0 {
        return 0.0;
    }
    let covered: i64 = windows
        .iter()
        .map(|(start, end)| (t1.min(*end) - t0.max(*start)).max(0))
        .sum();
    (covered as f64 / (t1 - t0) as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_never_exceeds_one_and_guards_degenerate_intervals() {
        let windows = [(0, 1000), (500, 2000)];
        let merged: Vec<(i64, i64)> = {
            let mut merged: Vec<(i64, i64)> = Vec::new();
            for (start, end) in windows {
                match merged.last_mut() {
                    Some(last) if start <= last.1 => last.1 = last.1.max(end),
                    _ => merged.push((start, end)),
                }
            }
            merged
        };
        assert_eq!(merged, vec![(0, 2000)]);
        assert_eq!(coverage_fraction(&merged, 0, 2000), 1.0);
        assert_eq!(coverage_fraction(&merged, 0, 4000), 0.5);
        assert_eq!(coverage_fraction(&merged, 3000, 4000), 0.0);
        assert_eq!(coverage_fraction(&merged, 100, 100), 0.0);
        assert_eq!(coverage_fraction(&merged, 200, 100), 0.0);
    }

    #[test]
    fn combined_is_omitted_when_either_half_is_missing() {
        let zone = Zone::unresolved("FRA", "default");
        let local = local_measured(Some(3.6e6), 1.0, &zone, None);
        let remote = RemoteEstimated {
            json: json!({}),
            totals: BTreeMap::new(),
            complete: true,
            calls: 0,
        };
        // No zone factors -> no local gwp -> no combined gwp, but energy
        // still combines (a unit with zero llm_calls is complete).
        let combined = combined_total(&local, &remote, false);
        assert_eq!(combined["energy"]["total"]["min"], json!(1.0));
        assert!(combined.get("gwp").is_none());

        // A pending estimate makes the remote half incomplete: nothing is
        // combined at all.
        let pending = RemoteEstimated {
            json: json!({}),
            totals: BTreeMap::new(),
            complete: false,
            calls: 1,
        };
        assert_eq!(combined_total(&local, &pending, false), json!({}));
    }
}
