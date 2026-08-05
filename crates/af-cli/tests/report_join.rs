//! Golden end-to-end coverage for the Task 12 join pipeline: the Task 5
//! spool fixture (`tests/fixtures/spool/basic-session/`) driven through
//! `af report --format json` with the estimator sidecar replaced by
//! `tests/fixtures/fake_sidecar.py --replay
//! tests/fixtures/sidecar/report-join.jsonl` — no ecologits, no venv, no
//! network.
//!
//! Three properties are asserted here:
//!
//! 1. **Exact arithmetic.** Every number in the emitted `impact_join`
//!    records is hand-computed from the fixture in the comments below, so a
//!    change to the attribution policy or the join rules shows up as a test
//!    failure with a concrete expected value rather than a vague diff.
//! 2. **Schema validity.** Each join validates against
//!    `schemas/v0.1/derived.schema.json`'s `$defs/impact_join`.
//! 3. **Determinism.** Running the whole pipeline twice against the same
//!    inputs produces byte-identical stdout — the replay guarantee.
//!
//! Plus a degradation test: with no estimator available at all, joins are
//! still built (local measurement is independent of the sidecar) and the
//! un-estimated `llm_call`s are surfaced as `pending`, never as zero.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

mod common;
use common::{close, repo_root, run_af, seed_state_dir, FRA_GWP, J_PER_KWH, SESSION_J};

// ---------------------------------------------------------------------------
// Fixture arithmetic, computed by hand from
// `tests/fixtures/spool/basic-session/cc-hooks.sess-basic.jsonl`.
//
// Spans (both local, both with explicit pids):
//   span-basic-01  Bash  [14:00:02, 14:00:09)  pids [51234]
//   span-basic-02  Edit  [14:00:05, 14:00:11)  pids [51290]
// Process windows:
//   P1 [02, 09)  pid 51234  4200 cpu-ms
//   P2 [05, 11)  pid 51290  1800 cpu-ms
// Energy samples (no `total` component, so the components are summed):
//   E1 [00, 04)   9.8 J
//   E2 [04, 08)  11.2 J
//   E3 [08, 12)   9.5 J  (cpu 7.4 + gpu 2.1)
//
// Per `l2_cpu_time/v1` (weights in cpu-ms scaled by the process window's
// overlap with the energy window, active share = min(1, W/C)):
//   E1: P1 covers 2000/7000 -> w(span01) = 1200; W/C = 1200/4000 = 0.3
//       span01 += 9.8 * 0.3 * (1200/1200)                    =  2.94
//       baseline += 6.86
//   E2: P1 4000/7000 -> 2400; P2 3000/6000 -> 900; W = 3300; W/C = 0.825
//       span01 += 11.2 * 0.825 * (2400/3300)                 =  6.72
//       span02 += 11.2 * 0.825 * ( 900/3300)                 =  2.52
//       baseline += 1.96
//   E3: P1 1000/7000 -> 600; P2 3000/6000 -> 900; W = 1500; W/C = 0.375
//       span01 += 9.5 * 0.375 * (600/1500)                   =  1.425
//       span02 += 9.5 * 0.375 * (900/1500)                   =  2.1375
//       baseline += 5.9375
// ---------------------------------------------------------------------------

/// Joules attributed to `span-basic-01`: 2.94 + 6.72 + 1.425.
const SPAN01_J: f64 = 11.085;
/// Joules attributed to `span-basic-02`: 0 + 2.52 + 2.1375.
const SPAN02_J: f64 = 4.6575;
/// Joules that landed in neither span: 6.86 + 1.96 + 5.9375.
const BASELINE_J: f64 = 14.7575;

/// Session wall time: `first_ts` 14:00:00 .. `last_ts` 14:00:14.
const SESSION_WALL_MS: f64 = 14_000.0;
/// Union of the three energy windows: [14:00:00, 14:00:12).
const SESSION_COVERED_MS: f64 = 12_000.0;

/// Runs one `af` subcommand and requires it to *fail*, returning stderr.
fn run_af_expecting_failure(state_dir: &Path, args: &[&str], with_estimator: bool) -> String {
    let output = common::af_command(state_dir, args, with_estimator)
        .output()
        .expect("run af");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "af {args:?} unexpectedly succeeded: stderr={stderr}"
    );
    stderr
}

/// The session unit's `estimate_status_counts`, which doubles as a probe
/// for whether the stored estimates survived: `{"ok": 2}` means the rows
/// are still there, `{"pending": 2}` means they were wiped.
fn status_counts(state_dir: &Path) -> Value {
    let (stdout, _) = run_af(state_dir, &["report", "--format", "json"], false);
    let report = parse(&stdout);
    join_by_key(&report, "sess-basic", "session:sess-basic")["join"]["remote_estimated"]
        ["estimate_status_counts"]
        .clone()
}

fn parse(stdout: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not valid JSON: {e}\n{}",
            String::from_utf8_lossy(stdout)
        )
    })
}

/// A compiled validator for `$defs/impact_join`, built by pointing the
/// schema document's root `$ref` at that definition.
fn impact_join_validator() -> jsonschema::Validator {
    let path = repo_root().join("schemas/v0.1/derived.schema.json");
    let text = fs::read_to_string(&path).expect("read derived schema");
    let mut schema: Value = serde_json::from_str(&text).expect("derived schema is valid JSON");
    schema["$ref"] = json!("#/$defs/impact_join");
    jsonschema::validator_for(&schema).expect("derived schema compiles")
}

fn session<'a>(report: &'a Value, session_id: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .expect("sessions is an array")
        .iter()
        .find(|s| s["session_id"] == session_id)
        .unwrap_or_else(|| panic!("session {session_id} missing from {report:#}"))
}

fn joins<'a>(report: &'a Value, session_id: &str) -> &'a Vec<Value> {
    session(report, session_id)["joins"]
        .as_array()
        .expect("joins is an array")
}

fn join_by_key<'a>(report: &'a Value, session_id: &str, unit_key: &str) -> &'a Value {
    joins(report, session_id)
        .iter()
        .find(|j| j["unit_key"] == unit_key)
        .unwrap_or_else(|| panic!("no join with unit_key {unit_key}"))
}

fn f(value: &Value, pointer: &str) -> f64 {
    value
        .pointer(pointer)
        .unwrap_or_else(|| panic!("missing {pointer} in {value:#}"))
        .as_f64()
        .unwrap_or_else(|| panic!("{pointer} is not a number in {value:#}"))
}

#[test]
fn report_builds_schema_valid_joins_with_exact_fixture_arithmetic() {
    let dir = seed_state_dir();

    let (stdout, stderr) = run_af(dir.path(), &["report", "--format", "json"], true);
    assert!(
        stderr.contains("estimated 2"),
        "estimation counters must reach stderr: {stderr}"
    );

    let report = parse(&stdout);
    let session_joins = joins(&report, "sess-basic");

    // One session unit + one tool_call unit per attributable span.
    assert_eq!(
        session_joins
            .iter()
            .map(|j| j["unit_key"].as_str().expect("unit_key is a string"))
            .collect::<Vec<_>>(),
        vec![
            "session:sess-basic",
            "tool_call:sess-basic:span-basic-01",
            "tool_call:sess-basic:span-basic-02",
        ],
        "joins are emitted sorted by unit_key"
    );

    let validator = impact_join_validator();
    for entry in session_joins {
        let record = &entry["join"];
        if let Err(err) = validator.validate(record) {
            panic!("join {} is not schema-valid: {err}", entry["unit_key"]);
        }
    }

    // ---- session unit -----------------------------------------------------
    let s = &join_by_key(&report, "sess-basic", "session:sess-basic")["join"];

    assert_eq!(
        s["unit"],
        json!({"level": "session", "session_id": "sess-basic"})
    );
    assert_eq!(s["t_start"], "2026-07-25T14:00:00Z");
    assert_eq!(s["t_end"], "2026-07-25T14:00:14Z");
    // Schema enum value, plus the versioned policy id the design log fixes.
    assert_eq!(s["attribution_policy"], "l2_cpu_time");
    assert_eq!(s["attribution_policy_id"], "l2_cpu_time/v1");
    assert_eq!(s["unmeasured_remote_spans"], 0);
    assert_eq!(
        s["zone"],
        json!({"factors_available": true, "id": "FRA", "source": "session_meta"})
    );

    // The session's local energy is the whole machine measurement over the
    // session: attributed + orphaned + baseline, with the split exposed.
    let session_kwh = SESSION_J / J_PER_KWH;
    assert_eq!(s["local_measured"]["energy"]["unit"], "kWh");
    close(
        f(s, "/local_measured/energy/total/min"),
        session_kwh,
        "session local energy min",
    );
    close(
        f(s, "/local_measured/energy/total/max"),
        session_kwh,
        "session local energy max (measured point value)",
    );
    close(
        f(s, "/local_measured/breakdown_j/attributed"),
        SPAN01_J + SPAN02_J,
        "attributed joules",
    );
    close(
        f(s, "/local_measured/breakdown_j/baseline_idle"),
        BASELINE_J,
        "baseline joules",
    );
    close(
        f(s, "/local_measured/breakdown_j/orphaned"),
        0.0,
        "orphaned joules",
    );
    close(
        f(s, "/local_measured/breakdown_j/total"),
        SESSION_J,
        "total joules (conservation)",
    );
    assert_eq!(s["local_measured"]["baseline_share_excluded"], true);
    close(
        f(s, "/local_measured/coverage"),
        SESSION_COVERED_MS / SESSION_WALL_MS,
        "session coverage",
    );
    assert_eq!(s["local_measured"]["gwp"]["unit"], "kgCO2eq");
    close(
        f(s, "/local_measured/gwp/total/min"),
        session_kwh * FRA_GWP,
        "session local gwp min",
    );

    // ---- remote side: both llm_calls estimated ok -------------------------
    assert_eq!(s["remote_estimated"]["llm_calls"], 2);
    assert_eq!(
        s["remote_estimated"]["estimate_status_counts"],
        json!({"ok": 2})
    );
    for (criterion, unit, min, max) in [
        ("energy", "kWh", 0.00025, 0.0005),
        ("gwp", "kgCO2eq", 0.000125, 0.00025),
        ("adpe", "kgSbeq", 1.25e-9, 2.5e-9),
        ("pe", "MJ", 0.0025, 0.005),
        ("water", "L", 2.5e-5, 5e-5),
    ] {
        assert_eq!(
            s["remote_estimated"]["impacts"][criterion]["unit"], unit,
            "remote {criterion} unit"
        );
        close(
            f(
                s,
                &format!("/remote_estimated/impacts/{criterion}/total/min"),
            ),
            min,
            &format!("remote {criterion} min"),
        );
        close(
            f(
                s,
                &format!("/remote_estimated/impacts/{criterion}/total/max"),
            ),
            max,
            &format!("remote {criterion} max"),
        );
    }

    // ---- combined: local point value adds to both ends of the range -------
    close(
        f(s, "/combined_total/energy/total/min"),
        session_kwh + 0.00025,
        "combined energy min",
    );
    close(
        f(s, "/combined_total/energy/total/max"),
        session_kwh + 0.0005,
        "combined energy max",
    );
    close(
        f(s, "/combined_total/gwp/total/min"),
        session_kwh * FRA_GWP + 0.000125,
        "combined gwp min",
    );
    close(
        f(s, "/combined_total/gwp/total/max"),
        session_kwh * FRA_GWP + 0.00025,
        "combined gwp max",
    );

    // ---- tool_call units --------------------------------------------------
    let one = &join_by_key(&report, "sess-basic", "tool_call:sess-basic:span-basic-01")["join"];
    assert_eq!(
        one["unit"],
        json!({"level": "tool_call", "session_id": "sess-basic", "span_id": "span-basic-01"}),
        "the fixture's spans carry no attribution.tool_call_id, so the \
         collector's span_id identifies the unit and no tool_call_id is invented"
    );
    // Millisecond precision, always three digits: a span's bounds are
    // re-formatted from parsed epoch millis (unlike the session unit, which
    // carries the collector's own strings through verbatim), so they state
    // the precision the join actually works in rather than rounding to the
    // second whenever the fraction happens to be zero.
    assert_eq!(one["t_start"], "2026-07-25T14:00:02.000Z");
    assert_eq!(one["t_end"], "2026-07-25T14:00:09.000Z");
    close(
        f(one, "/local_measured/energy/total/min"),
        SPAN01_J / J_PER_KWH,
        "span-basic-01 energy",
    );
    close(
        f(one, "/local_measured/gwp/total/max"),
        SPAN01_J / J_PER_KWH * FRA_GWP,
        "span-basic-01 gwp",
    );
    close(
        f(one, "/local_measured/coverage"),
        1.0,
        "span-basic-01 coverage",
    );
    // No llm_call in the fixture carries a tool-level attribution, so the
    // remote side of a tool_call unit is honestly empty, not zeroed impacts.
    assert_eq!(one["remote_estimated"], json!({"llm_calls": 0}));
    close(
        f(one, "/combined_total/energy/total/min"),
        SPAN01_J / J_PER_KWH,
        "span-basic-01 combined energy",
    );

    let two = &join_by_key(&report, "sess-basic", "tool_call:sess-basic:span-basic-02")["join"];
    close(
        f(two, "/local_measured/energy/total/min"),
        SPAN02_J / J_PER_KWH,
        "span-basic-02 energy",
    );

    // ---- determinism: the whole pipeline again, byte-identical ------------
    //
    // The comparison is over the raw `Vec<u8>`, never `from_utf8_lossy`:
    // lossy conversion maps every invalid byte to U+FFFD, so two runs
    // differing only in invalid UTF-8 would compare *equal* and the
    // guarantee would silently stop being tested. The diagnostic is lossy;
    // the assertion is not.
    let (again, _) = run_af(dir.path(), &["report", "--format", "json"], true);
    assert_eq!(
        stdout,
        again,
        "report output must be byte-identical across runs\nrun 1: {}\nrun 2: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&again),
    );

    // ---- replay: wipe derived, recompute, same bytes ----------------------
    let (replayed, replay_stderr) = run_af(dir.path(), &["replay", "--format", "json"], true);
    assert_eq!(
        stdout,
        replayed,
        "replay must reproduce the report byte-for-byte\nreport: {}\nreplay: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&replayed),
    );
    assert!(
        replay_stderr.contains("wiped derived"),
        "replay must say what it wiped: {replay_stderr}"
    );
}

#[test]
fn report_degrades_honestly_without_an_estimator() {
    let dir = seed_state_dir();

    let (stdout, stderr) = run_af(dir.path(), &["report", "--format", "json"], false);
    assert!(
        stderr.contains("no estimator"),
        "the missing sidecar must be reported: {stderr}"
    );

    let report = parse(&stdout);
    let s = &join_by_key(&report, "sess-basic", "session:sess-basic")["join"];

    // Local measurement is independent of the sidecar and still lands.
    close(
        f(s, "/local_measured/energy/total/min"),
        SESSION_J / J_PER_KWH,
        "session local energy without an estimator",
    );
    // No zone factors -> no local gwp, and therefore no combined gwp: a
    // "combined" total missing one of its two halves would be a lie.
    assert!(s["local_measured"].get("gwp").is_none());
    assert!(s["combined_total"].get("gwp").is_none());
    assert_eq!(s["zone"]["factors_available"], false);

    // The two llm_calls are surfaced as pending, never as zero impact, and
    // no remote impacts are claimed.
    assert_eq!(s["remote_estimated"]["llm_calls"], 2);
    assert_eq!(
        s["remote_estimated"]["estimate_status_counts"],
        json!({"pending": 2})
    );
    assert!(s["remote_estimated"].get("impacts").is_none());
    // Every llm_call is un-estimated, so an energy "combined total" would
    // silently omit the remote half.
    assert!(s["combined_total"].get("energy").is_none());

    // Still schema-valid, still deterministic.
    let validator = impact_join_validator();
    validator
        .validate(s)
        .expect("degraded join is schema-valid");
    let (again, _) = run_af(dir.path(), &["report", "--format", "json"], false);
    assert_eq!(
        stdout,
        again,
        "degraded report must also be byte-identical across runs\nrun 1: {}\nrun 2: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&again),
    );
}

#[test]
fn replay_refuses_to_wipe_the_estimates_it_cannot_rebuild() {
    let dir = seed_state_dir();

    // Seed a fully-estimated store.
    let (_, stderr) = run_af(dir.path(), &["report", "--format", "json"], true);
    assert!(stderr.contains("estimated 2"), "seeded estimates: {stderr}");
    assert_eq!(status_counts(dir.path()), json!({"ok": 2}));

    // With no estimator, `af replay` would delete rows nothing could
    // rebuild — a complete history traded for a permanently pending one.
    let stderr = run_af_expecting_failure(dir.path(), &["replay", "--format", "json"], false);
    assert!(
        stderr.contains("refusing to wipe"),
        "the refusal must say what it refused: {stderr}"
    );
    assert!(
        stderr.contains("af python setup"),
        "and must name the command that fixes it: {stderr}"
    );
    assert!(stderr.contains("--force"), "and the escape hatch: {stderr}");

    // Nothing was deleted: the estimates are still `ok`, not `pending`.
    assert_eq!(
        status_counts(dir.path()),
        json!({"ok": 2}),
        "a refused replay must leave the derived records intact"
    );

    // --force is the user saying they'd rather have them gone.
    let (_, forced_stderr) = run_af(
        dir.path(),
        &["replay", "--format", "json", "--force"],
        false,
    );
    assert!(
        forced_stderr.contains("wiped derived"),
        "a forced replay still wipes: {forced_stderr}"
    );
    assert!(
        forced_stderr.contains("forced wipe"),
        "and says what it cost: {forced_stderr}"
    );
    assert_eq!(
        status_counts(dir.path()),
        json!({"pending": 2}),
        "the estimates really are gone, and are reported as pending"
    );
}

#[test]
fn a_stored_estimate_from_another_remote_region_is_reported_stale() {
    let dir = seed_state_dir();

    let (_, stderr) = run_af(
        dir.path(),
        &["report", "--format", "json", "--remote-region", "FRA"],
        true,
    );
    assert!(
        stderr.contains("remote region FRA"),
        "seeded under FRA: {stderr}"
    );

    // Re-report with an explicit WOR remote override. Stored estimates were
    // computed against FRA and cannot be recomputed without `af replay`.
    let (stdout, stderr) = run_af(
        dir.path(),
        &[
            "report",
            "--format",
            "json",
            "--zone",
            "WOR",
            "--remote-region",
            "WOR",
        ],
        false,
    );
    assert!(
        stderr.contains("remote region FRA") && stderr.contains("override WOR"),
        "the warning must name both remote regions: {stderr}"
    );
    assert!(
        stderr.contains("af replay"),
        "and the way to fix it: {stderr}"
    );

    let report = parse(&stdout);
    let s = &join_by_key(&report, "sess-basic", "session:sess-basic")["join"];
    assert_eq!(s["zone"]["id"], "WOR");
    assert_eq!(
        s["remote_estimated"]["stale_zone_estimates"], 2,
        "both stored estimates belong to another zone: {s:#}"
    );
    // Their numbers are reported unchanged — re-labelling them under WOR
    // would be the actual lie.
    assert_eq!(
        s["remote_estimated"]["estimate_status_counts"],
        json!({"ok": 2})
    );
    // Still schema-valid with the extra counter present.
    impact_join_validator()
        .validate(s)
        .expect("join with a staleness counter is schema-valid");
}
