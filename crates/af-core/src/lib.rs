//! `af-core`: control-plane logic that sits between `af-store` (local
//! state) and the Python sidecars (`af-sidecar`) — impact estimation for
//! remote LLM calls, and the correlation + energy-attribution engine for
//! local measurements.
//!
//! The estimator sidecar protocol (`estimate`/`zone_factors` ops and status
//! values) and the normative
//! specification of the `l2_cpu_time` v1 attribution policy implemented in
//! [`attribution`].

mod attribution;
mod correlate;
mod estimate;
mod join;
mod replay;

pub use attribution::{
    apportion, apportion_traced, claims_pid, policy_id, sample_energy_j, AllocRow, Apportionment,
    Policy, SampleTrace, POLICY_L1_WALL_CLOCK, POLICY_L2_CPU_TIME, POLICY_NONE,
};
pub use correlate::{
    correlate, parse_ts, rfc3339_ms, SessionTree, Span, SpanId, BOOTSTRAP_TOOL_NAME,
};
pub use estimate::{
    estimate_events, estimate_pending, estimate_pending_limit, EstimationOutcome, EstimationRegion,
};
pub use join::{
    build_joins, fetch_zone_factors, stale_zone_estimates, sum_criteria, CriterionSum, ImpactJoin,
    Zone, ZoneFactors,
};
pub use replay::{
    rebuild_derived, rebuild_derived_stored, rebuild_prepared_stored, PreparedSession,
    RebuildOutcome,
};
