//! Unit-level coverage of `build_joins`' honesty branches — the cases the
//! Task 5 spool fixture cannot reach (`crates/af-cli/tests/report_join.rs`
//! covers the happy path end-to-end through the real CLI).
//!
//! The theme throughout: a number that could not be computed is *absent*,
//! never zero, and the reason is always somewhere in the record.

use std::collections::BTreeMap;

use af_core::{apportion, build_joins, correlate, ImpactJoin, Zone, ZoneFactors};
use af_events::{
    fixtures, ActionSpan, Attribution, EnergySample, Envelope, ExecutionLocus, Payload,
    ProcessSample,
};
use serde_json::{json, Value};

const SESSION: &str = "sess-join";
const T0: &str = "2026-07-25T12:00:00Z";
const T1: &str = "2026-07-25T12:00:10Z";

/// The pid every span in this suite owns, and the one the process samples
/// report cpu time for — so the L2 rung is actually reachable.
const PID: i64 = 4242;

/// Joules the fully-measured fixture reports: exactly `0.001 kWh`, which
/// keeps the expected values in the assertions readable.
const SESSION_J: f64 = 3600.0;

fn envelope(event_id: &str, ts: &str, payload: Payload) -> Envelope {
    fixtures::envelope(event_id, SESSION, ts, payload)
}

fn span(
    event_id: &str,
    span_id: &str,
    t_start: &str,
    t_end: &str,
    locus: ExecutionLocus,
) -> Envelope {
    envelope(
        event_id,
        t_end,
        Payload::ActionSpan(ActionSpan {
            execution_locus: locus,
            pids: Some(vec![PID]),
            ..fixtures::action_span(span_id, t_start, t_end)
        }),
    )
}

fn energy(event_id: &str, t_start: &str, t_end: &str, joules: f64) -> Envelope {
    envelope(
        event_id,
        t_end,
        Payload::EnergySample(fixtures::energy_sample(t_start, t_end, joules)),
    )
}

fn processes(event_id: &str, t_start: &str, t_end: &str, cpu_ms: u64) -> Envelope {
    envelope(
        event_id,
        t_end,
        Payload::ProcessSample(fixtures::process_sample(
            t_start,
            t_end,
            &[(PID, cpu_ms, None)],
        )),
    )
}

fn llm_call(event_id: &str, attribution: Option<Attribution>) -> Envelope {
    let mut evt = envelope(event_id, T0, Payload::LlmCall(fixtures::llm_call()));
    evt.attribution = attribution;
    evt
}

/// The session almost every case here starts from: one local span
/// (`span-a`) covering `[T0, T1)`, one energy sample of [`SESSION_J`] over
/// the same window, and process data covering it too — so the join reaches
/// L2 and pays the span the whole measurement.
///
/// Written once because eight of these tests differ from each other only
/// in the `llm_call`s and stored estimates they add on top; rebuilding the
/// measured half in each of them made the *difference* — which is what
/// each test is actually about — the hardest thing to see.
fn one_span_session() -> Vec<Envelope> {
    vec![
        span("e1", "span-a", T0, T1, ExecutionLocus::Local),
        energy("e2", T0, T1, SESSION_J),
        processes("e3", T0, T1, 10_000),
    ]
}

/// [`one_span_session`] plus one unattributed `llm_call` per id.
fn one_span_session_with_calls(event_ids: &[&str]) -> Vec<Envelope> {
    let mut events = one_span_session();
    events.extend(event_ids.iter().map(|id| llm_call(id, None)));
    events
}

/// [`one_span_session`] with `span-a` carrying `attribution` — the input
/// the `task` and `tool_call` units are derived from.
fn one_span_session_attributed(attribution: Attribution) -> Vec<Envelope> {
    let mut events = one_span_session();
    events[0].attribution = Some(attribution);
    events
}

/// An [`Attribution`] naming only a `tool_call_id`.
fn tool_call(id: &str) -> Attribution {
    Attribution {
        tool_call_id: Some(id.to_string()),
        ..Default::default()
    }
}

/// An [`Attribution`] naming only a `task_id`.
fn task(id: &str) -> Attribution {
    Attribution {
        task_id: Some(id.to_string()),
        ..Default::default()
    }
}

/// An `ok` estimate carrying one energy and one gwp criterion.
fn ok_estimate(kwh: f64, gwp: f64) -> Value {
    json!({
        "status": "ok",
        "impacts": {
            "energy": {"unit": "kWh", "total": {"min": kwh, "max": kwh}},
            "gwp": {"unit": "kgCO2eq", "total": {"min": gwp, "max": gwp}},
        }
    })
}

fn zone_with_factors() -> Zone {
    Zone {
        id: "FRA".to_string(),
        source: "flag".to_string(),
        factors: Some(ZoneFactors {
            gwp_min: 0.05,
            gwp_max: 0.05,
        }),
    }
}

fn build(events: &[Envelope], estimates: &BTreeMap<String, Value>, zone: &Zone) -> Vec<ImpactJoin> {
    let tree = correlate(events);
    let samples: Vec<EnergySample> = events
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::EnergySample(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let procs: Vec<ProcessSample> = events
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::ProcessSample(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let apportionment = apportion(&samples, &procs, &tree);
    build_joins(
        SESSION,
        T0,
        T1,
        events,
        &tree,
        &apportionment,
        estimates,
        zone,
        Some(&zone.id),
    )
}

fn record<'a>(joins: &'a [ImpactJoin], unit_key: &str) -> &'a Value {
    &joins
        .iter()
        .find(|j| j.unit_key == unit_key)
        .unwrap_or_else(|| {
            panic!(
                "no join {unit_key}; got {:?}",
                joins.iter().map(|j| &j.unit_key).collect::<Vec<_>>()
            )
        })
        .record
}

#[test]
fn a_session_with_no_energy_samples_reports_coverage_zero_and_no_energy() {
    let events = vec![
        span("e1", "span-a", T0, T1, ExecutionLocus::Local),
        llm_call("e2", None),
    ];
    let mut estimates = BTreeMap::new();
    estimates.insert("e2".to_string(), ok_estimate(0.001, 0.0005));

    let joins = build(&events, &estimates, &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));

    // Nothing measured the machine: `energy: 0 kWh` would be a fabrication,
    // `coverage: 0` is the fact.
    assert!(session["local_measured"].get("energy").is_none());
    assert!(session["local_measured"].get("gwp").is_none());
    assert!(session["local_measured"].get("breakdown_j").is_none());
    assert_eq!(session["local_measured"]["coverage"], json!(0.0));
    assert_eq!(
        session["local_measured"]["baseline_share_excluded"],
        json!(true)
    );

    // The remote half is fine, but a *combined* total needs both halves.
    assert_eq!(session["remote_estimated"]["llm_calls"], 1);
    assert_eq!(session["combined_total"], json!({}));

    // No sample means no rung of the ladder was applied.
    assert_eq!(session["attribution_policy"], "l1_wall_clock");
    assert_eq!(session["attribution_policy_id"], "l1_wall_clock/v1");

    // The span still gets its own unit, also with nothing measured.
    let unit = record(&joins, &format!("tool_call:{SESSION}:span-a"));
    assert!(unit["local_measured"].get("energy").is_none());
    assert_eq!(unit["combined_total"], json!({}));
}

#[test]
fn remote_and_zero_length_spans_are_counted_but_get_no_unit_of_their_own() {
    let events = vec![
        span("e1", "span-local", T0, T1, ExecutionLocus::Local),
        span(
            "e2",
            "span-remote",
            "2026-07-25T12:00:01Z",
            "2026-07-25T12:00:05Z",
            ExecutionLocus::Remote,
        ),
        // Zero-length local span: a real observation no rung can pay.
        span(
            "e3",
            "span-instant",
            "2026-07-25T12:00:03Z",
            "2026-07-25T12:00:03Z",
            ExecutionLocus::Local,
        ),
        energy("e4", T0, T1, 100.0),
        processes("e5", T0, T1, 5000),
    ];

    let joins = build(&events, &BTreeMap::new(), &zone_with_factors());
    let keys: Vec<&str> = joins.iter().map(|j| j.unit_key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            format!("session:{SESSION}"),
            format!("tool_call:{SESSION}:span-local"),
        ],
        "a unit reporting 0 J is indistinguishable from one measured idle, \
         so unattributable spans are counted rather than given a record"
    );

    let session = record(&joins, &format!("session:{SESSION}"));
    assert_eq!(session["unmeasured_remote_spans"], 1);
    assert_eq!(session["counters"]["degenerate_spans"], 1);
    assert_eq!(session["counters"]["samples_l2"], 1);
    assert_eq!(session["counters"]["samples_l1"], 0);

    // Conservation: the session's local energy is the whole measurement.
    assert_eq!(
        session["local_measured"]["breakdown_j"]["total"]
            .as_f64()
            .unwrap(),
        100.0
    );
    assert_eq!(session["local_measured"]["coverage"], json!(1.0));
}

#[test]
fn a_tool_call_unit_claims_only_the_llm_calls_attributed_to_it() {
    let mut events = one_span_session_attributed(tool_call("toolu_123"));
    events.push(llm_call("e4", Some(tool_call("toolu_123"))));
    events.push(llm_call("e5", None));

    let mut estimates = BTreeMap::new();
    estimates.insert("e4".to_string(), ok_estimate(0.002, 0.001));
    estimates.insert("e5".to_string(), ok_estimate(0.004, 0.002));

    let joins = build(&events, &estimates, &zone_with_factors());
    let unit = record(&joins, &format!("tool_call:{SESSION}:span-a"));

    assert_eq!(
        unit["unit"],
        json!({
            "level": "tool_call",
            "session_id": SESSION,
            "span_id": "span-a",
            "tool_call_id": "toolu_123",
        })
    );
    assert_eq!(unit["remote_estimated"]["llm_calls"], 1);
    assert_eq!(
        unit["remote_estimated"]["impacts"]["energy"]["total"]["min"],
        json!(0.002),
        "only the call attributed to this tool_call_id counts toward it"
    );

    // The session sees both calls.
    let session = record(&joins, &format!("session:{SESSION}"));
    assert_eq!(session["remote_estimated"]["llm_calls"], 2);
    assert_eq!(
        session["remote_estimated"]["impacts"]["energy"]["total"]["min"],
        json!(0.006)
    );
}

#[test]
fn a_task_unit_is_derived_from_span_attribution() {
    let mut events = one_span_session_attributed(task("task-7"));
    events.push(llm_call("e4", Some(task("task-7"))));

    let mut estimates = BTreeMap::new();
    estimates.insert("e4".to_string(), ok_estimate(0.002, 0.001));

    let joins = build(&events, &estimates, &zone_with_factors());
    let unit = record(&joins, &format!("task:{SESSION}:task-7"));

    assert_eq!(
        unit["unit"],
        json!({"level": "task", "session_id": SESSION, "task_id": "task-7"})
    );
    assert_eq!(unit["remote_estimated"]["llm_calls"], 1);
    // 3600 J over one span = 0.001 kWh local, plus 0.002 kWh remote.
    assert_eq!(
        unit["local_measured"]["energy"]["total"]["min"],
        json!(0.001)
    );
    assert_eq!(
        unit["combined_total"]["energy"]["total"]["min"],
        json!(0.003)
    );

    // Sorted by unit_key: session, then task, then tool_call.
    assert_eq!(
        joins
            .iter()
            .map(|j| j.unit_key.as_str())
            .collect::<Vec<_>>(),
        vec![
            format!("session:{SESSION}"),
            format!("task:{SESSION}:task-7"),
            format!("tool_call:{SESSION}:span-a"),
        ]
    );
}

#[test]
fn an_unestimable_call_blocks_the_combined_total_without_hiding_the_measurement() {
    let events = one_span_session_with_calls(&["e4", "e5"]);
    let mut estimates = BTreeMap::new();
    estimates.insert("e4".to_string(), ok_estimate(0.002, 0.001));
    estimates.insert("e5".to_string(), json!({"status": "unknown_model"}));

    let joins = build(&events, &estimates, &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));

    assert_eq!(
        session["remote_estimated"]["estimate_status_counts"],
        json!({"ok": 1, "unknown_model": 1}),
        "each status is counted, never collapsed into a total"
    );
    // The one estimable call's impacts are still reported...
    assert_eq!(
        session["remote_estimated"]["impacts"]["energy"]["total"]["min"],
        json!(0.002)
    );
    // ...and the local measurement is untouched...
    assert_eq!(
        session["local_measured"]["energy"]["total"]["min"],
        json!(0.001)
    );
    // ...but adding them would understate the total by an unknown amount.
    assert_eq!(session["combined_total"], json!({}));
}

#[test]
fn without_zone_factors_gwp_disappears_from_both_local_and_combined() {
    let events = one_span_session_with_calls(&["e4"]);
    let mut estimates = BTreeMap::new();
    estimates.insert("e4".to_string(), ok_estimate(0.002, 0.001));

    let joins = build(&events, &estimates, &Zone::unresolved("FRA", "default"));
    let session = record(&joins, &format!("session:{SESSION}"));

    assert_eq!(
        session["zone"],
        json!({"factors_available": false, "id": "FRA", "source": "default"})
    );
    assert!(session["local_measured"].get("gwp").is_none());
    assert!(
        session["combined_total"].get("gwp").is_none(),
        "half a combined total is a wrong number, not a partial one"
    );
    // Energy still combines — it needs no emission factor.
    assert_eq!(
        session["combined_total"]["energy"]["total"]["min"],
        json!(0.003)
    );
    // The remote gwp estimate is untouched: it never needed the local zone.
    assert_eq!(
        session["remote_estimated"]["impacts"]["gwp"]["total"]["min"],
        json!(0.001)
    );
}

#[test]
fn a_criterion_reported_in_two_units_is_dropped_entirely_and_counted() {
    let events = one_span_session_with_calls(&["e4", "e5"]);
    let mut estimates = BTreeMap::new();
    estimates.insert("e4".to_string(), ok_estimate(0.002, 0.001));
    // Same criterion, different unit: 0.002 kWh + 5 Wh is not 5.002 of
    // anything, and no consumer could detect the error from the output.
    estimates.insert(
        "e5".to_string(),
        json!({
            "status": "ok",
            "impacts": {
                "energy": {"unit": "Wh", "total": {"min": 5.0, "max": 5.0}},
                "gwp": {"unit": "kgCO2eq", "total": {"min": 0.003, "max": 0.003}},
            }
        }),
    );

    let joins = build(&events, &estimates, &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));
    let remote = &session["remote_estimated"];

    assert_eq!(
        remote["estimate_status_counts"],
        json!({"ok": 2}),
        "both calls were estimated fine; the disagreement is about units"
    );
    assert!(
        remote["impacts"].get("energy").is_none(),
        "a wrong-unit sum must never render, not even as a partial: {remote:#}"
    );
    assert_eq!(
        remote["unit_mismatches"],
        json!({"energy": 1}),
        "the dropped criterion is counted, not silently absent"
    );
    assert!(
        session["combined_total"].get("energy").is_none(),
        "and it can certainly not be added to the local measurement"
    );

    // gwp agreed on kgCO2eq across both estimates, so it is unaffected.
    assert_eq!(
        remote["impacts"]["gwp"]["total"]["min"].as_f64().unwrap(),
        0.004
    );
    assert!(session["combined_total"].get("gwp").is_some());
}

#[test]
fn an_ok_estimate_omitting_a_criterion_withholds_it_from_the_combined_total() {
    let events = one_span_session_with_calls(&["e4", "e5"]);
    let mut estimates = BTreeMap::new();
    estimates.insert("e4".to_string(), ok_estimate(0.002, 0.001));
    // An `ok` estimate that reports gwp but no energy at all.
    estimates.insert(
        "e5".to_string(),
        json!({
            "status": "ok",
            "impacts": {"gwp": {"unit": "kgCO2eq", "total": {"min": 0.003, "max": 0.003}}}
        }),
    );

    let joins = build(&events, &estimates, &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));

    assert_eq!(
        session["remote_estimated"]["estimate_status_counts"],
        json!({"ok": 2})
    );
    // The subtotal is a real lower bound over the estimates that had it, so
    // it is still reported...
    assert_eq!(
        session["remote_estimated"]["impacts"]["energy"]["total"]["min"],
        json!(0.002)
    );
    // ...but it is not the remote *total*, so it may not be combined: the
    // sum would silently understate by whatever e5's energy was.
    assert!(
        session["combined_total"].get("energy").is_none(),
        "a subtotal dressed as a combined total is a wrong number: {session:#}"
    );
    // Both estimates reported gwp, so gwp still combines.
    assert_eq!(
        session["remote_estimated"]["impacts"]["gwp"]["total"]["min"]
            .as_f64()
            .unwrap(),
        0.004
    );
    // local 0.001 kWh * 0.05 = 5e-5, plus 0.004.
    assert_eq!(
        session["combined_total"]["gwp"]["total"]["min"]
            .as_f64()
            .unwrap(),
        0.00405
    );
}

#[test]
fn a_session_whose_only_energy_samples_are_degenerate_still_reports_its_joules() {
    let events = vec![
        span("e1", "span-a", T0, T1, ExecutionLocus::Local),
        // Zero-length energy sample: the policy can attribute none of it,
        // and books all 42 J as baseline idle — but the machine *was*
        // measured, so the joules must not vanish from the record.
        energy("e2", T0, T0, 42.0),
        processes("e3", T0, T1, 10_000),
    ];

    let joins = build(&events, &BTreeMap::new(), &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));
    let local = &session["local_measured"];

    assert_eq!(session["counters"]["degenerate_samples"], json!(1));
    assert_eq!(
        local["breakdown_j"]["total"].as_f64().unwrap(),
        42.0,
        "conservation still holds over a degenerate sample: {local:#}"
    );
    assert_eq!(
        local["breakdown_j"]["baseline_idle"].as_f64().unwrap(),
        42.0
    );
    assert_eq!(local["breakdown_j"]["attributed"].as_f64().unwrap(), 0.0);
    assert_eq!(
        local["energy"]["total"]["min"].as_f64().unwrap(),
        42.0 / 3.6e6
    );
    // Nothing covered any wall time, and the empty union window must not be
    // divided by.
    assert_eq!(local["coverage"], json!(0.0));
    // Zero llm_calls is a vacuously complete remote half, so the local
    // measurement still combines.
    assert_eq!(
        session["combined_total"]["energy"]["total"]["min"]
            .as_f64()
            .unwrap(),
        42.0 / 3.6e6
    );
}

#[test]
fn estimates_computed_under_another_zone_are_counted_as_stale() {
    let events = one_span_session_with_calls(&["e4", "e5"]);
    let mut stale = ok_estimate(0.002, 0.001);
    stale["zone"] = json!("WOR");
    let mut current = ok_estimate(0.002, 0.001);
    current["zone"] = json!("FRA");
    let mut estimates = BTreeMap::new();
    estimates.insert("e4".to_string(), stale);
    estimates.insert("e5".to_string(), current);

    // The pass runs under FRA; e4 was estimated under WOR.
    let joins = build(&events, &estimates, &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));

    assert_eq!(
        session["remote_estimated"]["stale_zone_estimates"],
        json!(1),
        "a stored estimate from another zone is named, not silently re-labelled"
    );
    // Its numbers are still reported unchanged — re-labelling them would be
    // the actual lie.
    assert_eq!(
        session["remote_estimated"]["impacts"]["energy"]["total"]["min"],
        json!(0.004)
    );

    // An estimate with no stamp at all (written before stamping existed) is
    // never *assumed* stale.
    let mut unstamped = BTreeMap::new();
    unstamped.insert("e4".to_string(), ok_estimate(0.002, 0.001));
    unstamped.insert("e5".to_string(), ok_estimate(0.002, 0.001));
    let joins = build(&events, &unstamped, &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));
    assert!(session["remote_estimated"]
        .get("stale_zone_estimates")
        .is_none());
}

#[test]
fn coverage_is_clamped_when_samples_run_past_the_session_bounds() {
    let events = vec![
        span("e1", "span-a", T0, T1, ExecutionLocus::Local),
        // Two overlapping samples, together spanning well past [T0, T1).
        energy("e2", "2026-07-25T11:59:00Z", "2026-07-25T12:00:05Z", 10.0),
        energy("e3", "2026-07-25T12:00:04Z", "2026-07-25T12:01:00Z", 10.0),
    ];

    let joins = build(&events, &BTreeMap::new(), &zone_with_factors());
    let session = record(&joins, &format!("session:{SESSION}"));

    assert_eq!(
        session["local_measured"]["coverage"],
        json!(1.0),
        "overlapping windows are unioned, not summed, and the result is clamped"
    );
    let total = session["local_measured"]["breakdown_j"]["total"]
        .as_f64()
        .unwrap();
    assert!(
        (total - 20.0).abs() < 1e-9,
        "conservation over both samples: expected 20 J, got {total}"
    );
}
