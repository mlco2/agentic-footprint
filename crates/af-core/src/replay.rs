//! Rebuilding the derived tables from raw events — the single pipeline
//! behind both `af report` and `af replay`.
//!
//! The two commands differ in exactly one flag: `af replay` wipes
//! `impact_estimates` and `impact_joins` first, so a methodology change (a
//! new ecologits pin, a new attribution policy version) can be re-applied
//! to history. Everything else — zone factors, estimation of the pending
//! backlog, correlation, apportionment, join assembly, upsert — is the same
//! code path, which is what makes "replay reproduces the report" a property
//! rather than a coincidence.
//!
//! **A pass that only computes is never fatal.** A missing or broken
//! estimator sidecar degrades it instead of failing it: local measurement
//! doesn't depend on Python, so the joins are still built, the un-estimated
//! `llm_call`s are reported as `pending`, and the reason lands in
//! [`RebuildOutcome::notes`] for the caller to surface. `af report` on a
//! machine with no venv is meant to be useful, not to be an error.
//!
//! **A pass that *destroys* first is different.** `wipe` deletes stored
//! estimates that only the estimator can rebuild, so it refuses to run at
//! all when no estimator is available — degrading would mean trading a
//! complete history for a permanently `pending` one. `force` overrides
//! that, with the consequence stated in the notes.

use std::collections::{BTreeMap, BTreeSet};

use af_events::{EnergySample, Envelope, Payload, ProcessSample};
use af_sidecar::Sidecar;
use af_store::Store;
use anyhow::{bail, Result};

use crate::attribution::{apportion, Apportionment};
use crate::correlate::{correlate, SessionTree};
use crate::estimate::{estimate_pending, EstimationOutcome, EstimationRegion};
use crate::join::{build_joins, fetch_zone_factors, stale_zone_estimates, Zone};

/// What one [`rebuild_derived`] pass did, for the caller to report.
#[derive(Debug, Clone, PartialEq)]
pub struct RebuildOutcome {
    /// The zone the pass ran under, including whether its emission factors
    /// were actually obtained.
    pub zone: Zone,
    /// `None` when no estimator sidecar was available.
    pub estimation: Option<EstimationOutcome>,
    /// `llm_call` events still without an estimate when the pass finished.
    /// Non-zero after a degraded pass; that is the honest headline number.
    pub pending_llm_calls: usize,
    /// Derived rows deleted before recomputing (`af replay`).
    pub wiped: bool,
    /// Sessions visited.
    pub sessions: usize,
    /// `impact_join` records written.
    pub joins: usize,
    /// Human-readable degradation notes, in the order they occurred.
    pub notes: Vec<String>,
}

/// Recomputes every derived record from `store`'s raw events.
///
/// `sidecar`, when present, is asked for `zone`'s electricity-mix factors
/// **before** any estimate request — the order matters for the golden
/// transcript tests (a replayed conversation has a fixed request order) and
/// is the natural one anyway, since the zone is a property of the pass and
/// the estimates are a property of the backlog.
///
/// `wipe` selects `af replay` semantics. It is checked against `sidecar`
/// **before** anything is deleted: wiping `impact_estimates` with no
/// estimator to recompute them destroys data that cannot be rebuilt from
/// the raw events alone, so the pass errors out (naming `af python setup`)
/// instead. `force` proceeds anyway — the caller has said they would rather
/// have every `llm_call` back to `pending` than keep the old estimates —
/// and the consequence is recorded in [`RebuildOutcome::notes`]. Both flags
/// are ignored when `wipe` is false; `af report` deletes nothing.
///
/// `sessions` restricts the rebuild to the named sessions; `None` rebuilds
/// every session in the store, which is what `af report` and `af replay`
/// pass and what makes their output a function of the whole database. It
/// exists for `af watch`, which runs this pass every couple of seconds and
/// whose *inputs* only ever change for the sessions that just produced
/// events — recomputing a day's worth of idle sessions to write back
/// byte-identical rows is work whose only effect is heat.
///
/// Two things are scoped along with it, deliberately:
///
/// * [`RebuildOutcome::sessions`] and [`RebuildOutcome::joins`] count what
///   the pass *visited*, so a filtered pass reports the filtered numbers
///   rather than implying it looked at the whole store.
/// * The stale-zone notes are raised only for the sessions visited. A
///   filtered caller therefore hears about staleness in the sessions it is
///   currently touching, not in every session that ever existed — which is
///   also the only place it could act on. `af report`/`af replay`, passing
///   `None`, still see all of them.
///
/// [`RebuildOutcome::pending_llm_calls`] stays store-wide: it is a
/// property of the backlog, not of this pass's scope.
#[allow(clippy::too_many_arguments)]
pub fn rebuild_derived(
    store: &mut Store,
    mut sidecar: Option<&mut Sidecar>,
    zone_id: &str,
    zone_source: &str,
    remote_region: &EstimationRegion,
    wipe: bool,
    force: bool,
    sessions: Option<&BTreeSet<String>>,
) -> Result<RebuildOutcome> {
    let mut notes = Vec::new();

    if wipe {
        if sidecar.is_none() {
            if !force {
                bail!(
                    "refusing to wipe the derived records: no estimator is available, so the \
                     stored impact estimates could not be recomputed and every llm_call would \
                     be left pending. Run `af python setup` first, or pass --force to wipe \
                     anyway."
                );
            }
            notes.push(
                "forced wipe with no estimator available: every llm_call is now pending until \
                 an estimator exists and `af replay` runs again"
                    .to_string(),
            );
        }
        store.wipe_derived()?;
    }

    let mut zone = Zone::unresolved(zone_id, zone_source);
    let mut estimation = None;

    if let Some(sidecar) = sidecar.as_mut() {
        match fetch_zone_factors(sidecar, zone_id) {
            Ok(Some(factors)) => zone.factors = Some(factors),
            Ok(None) => notes.push(format!(
                "estimator does not know zone {zone_id}: local gwp omitted"
            )),
            Err(err) => notes.push(format!("zone_factors failed ({err:#}): local gwp omitted")),
        }
    }

    if let Some(sidecar) = sidecar.as_mut() {
        match estimate_pending(store, sidecar, remote_region) {
            Ok(outcome) => estimation = Some(outcome),
            Err(err) => notes.push(format!(
                "estimation failed ({err:#}): remaining llm_calls stay pending"
            )),
        }
    }

    rebuild_joins(
        store,
        zone,
        remote_region,
        estimation,
        wipe,
        sessions,
        notes,
    )
}

/// Rebuilds joins using already-stored estimates and already-resolved zone
/// factors. Used by resident watch so Python I/O can live exclusively in its
/// background estimator worker.
pub fn rebuild_derived_stored(
    store: &mut Store,
    zone: Zone,
    remote_region: &EstimationRegion,
    sessions: Option<&BTreeSet<String>>,
) -> Result<RebuildOutcome> {
    rebuild_joins(
        store,
        zone,
        remote_region,
        None,
        false,
        sessions,
        Vec::new(),
    )
}

pub struct PreparedSession<'a> {
    pub events: &'a [Envelope],
    pub tree: &'a SessionTree,
    pub apportionment: &'a Apportionment,
}

pub fn rebuild_prepared_stored(
    store: &mut Store,
    zone: Zone,
    remote_region: &EstimationRegion,
    sessions: &BTreeMap<String, PreparedSession<'_>>,
) -> Result<RebuildOutcome> {
    let pending_llm_calls = store.count_llm_calls_without_estimate()? as usize;
    let mut joins = 0usize;
    let mut notes = Vec::new();

    for (session_id, prepared) in sessions {
        let first_ts = prepared
            .events
            .first()
            .map(|event| event.ts.as_str())
            .unwrap_or("");
        let last_ts = prepared
            .events
            .last()
            .map(|event| event.ts.as_str())
            .unwrap_or("");
        let llm_event_ids = prepared
            .events
            .iter()
            .filter(|event| matches!(event.payload, Payload::LlmCall(_)))
            .map(|event| event.event_id.clone())
            .collect::<Vec<_>>();
        let estimates = store.estimates_for_events(&llm_event_ids)?;
        if let Some(region_id) = &remote_region.id {
            for (stale, count) in stale_zone_estimates(&estimates, region_id) {
                notes.push(format!(
                    "session {session_id}: {count} stored estimate(s) used remote region {stale}, not the explicit override {region_id}; their remote impacts are reported unchanged — run `af replay` to recompute them"
                ));
            }
        }
        for join in build_joins(
            session_id,
            first_ts,
            last_ts,
            prepared.events,
            prepared.tree,
            prepared.apportionment,
            &estimates,
            &zone,
            remote_region.id.as_deref(),
        ) {
            store.upsert_join(&join.unit_key, &join.record)?;
            joins += 1;
        }
    }

    Ok(RebuildOutcome {
        zone,
        estimation: None,
        pending_llm_calls,
        wiped: false,
        sessions: sessions.len(),
        joins,
        notes,
    })
}

fn rebuild_joins(
    store: &mut Store,
    zone: Zone,
    remote_region: &EstimationRegion,
    estimation: Option<EstimationOutcome>,
    wiped: bool,
    sessions: Option<&BTreeSet<String>>,
    mut notes: Vec<String>,
) -> Result<RebuildOutcome> {
    let pending_llm_calls = store.count_llm_calls_without_estimate()? as usize;

    let mut visited = 0usize;
    let mut joins = 0usize;
    let mut summaries = store.session_summaries()?;
    summaries.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    if let Some(only) = sessions {
        summaries.retain(|summary| only.contains(&summary.session_id));
    }

    for summary in &summaries {
        visited += 1;
        let events = store.events_for_session(&summary.session_id)?;

        let mut samples: Vec<EnergySample> = Vec::new();
        let mut procs: Vec<ProcessSample> = Vec::new();
        let mut llm_event_ids: Vec<String> = Vec::new();
        for event in &events {
            match &event.payload {
                Payload::EnergySample(sample) => samples.push(sample.clone()),
                Payload::ProcessSample(sample) => procs.push(sample.clone()),
                Payload::LlmCall(_) => llm_event_ids.push(event.event_id.clone()),
                _ => {}
            }
        }

        let tree = correlate(&events);
        let apportionment = apportion(&samples, &procs, &tree);
        let estimates = store.estimates_for_events(&llm_event_ids)?;

        if let Some(region_id) = &remote_region.id {
            for (stale, count) in stale_zone_estimates(&estimates, region_id) {
                notes.push(format!(
                    "session {}: {count} stored estimate(s) used remote region {stale}, not \
                     the explicit override {region_id}; their remote impacts are reported \
                     unchanged — run `af replay` to recompute them",
                    summary.session_id
                ));
            }
        }

        for join in build_joins(
            &summary.session_id,
            &summary.first_ts,
            &summary.last_ts,
            &events,
            &tree,
            &apportionment,
            &estimates,
            &zone,
            remote_region.id.as_deref(),
        ) {
            store.upsert_join(&join.unit_key, &join.record)?;
            joins += 1;
        }
    }

    Ok(RebuildOutcome {
        zone,
        estimation,
        pending_llm_calls,
        wiped,
        sessions: visited,
        joins,
        notes,
    })
}
