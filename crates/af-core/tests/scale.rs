//! A working day of one session through attribution and joins.
//!
//! Ignored by default: it is a measurement, not a check. Run it with
//! `cargo test --release -p af-core --test scale -- --ignored --nocapture`
//! to see how long a full rebuild of an eight-hour session takes, and a
//! digest of everything it produced — the digest is how a change to the
//! algorithms is shown to leave every number and trace exactly as it was.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use af_core::{apportion_traced, build_joins, correlate, Zone};
use af_events::{fixtures, ActionSpan, Attribution, Envelope, LlmCall, Payload};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const BASE_MS: i64 = 1_785_326_400_000;
const ROOT_PID: i64 = 4242;

fn ts(offset_ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos((BASE_MS + offset_ms) as i128 * 1_000_000)
        .expect("representable timestamp")
        .format(&Rfc3339)
        .expect("formattable timestamp")
}

fn attributed(
    mut env: Envelope,
    tool_call_id: Option<String>,
    task_id: Option<String>,
) -> Envelope {
    env.attribution = Some(Attribution {
        tool_call_id,
        task_id,
        ..Default::default()
    });
    env
}

/// One session at five-second sampling for `hours`, with a tool span every
/// 40 s (some long, some pid-owning), a task every ten tool calls, and two
/// LLM calls per tool call.
fn session(hours: i64) -> Vec<Envelope> {
    let window_ms = 5_000;
    let windows = hours * 3_600_000 / window_ms;
    let mut events = Vec::new();
    let mut boot = fixtures::action_span("boot", &ts(0), &ts(1));
    boot.tool_name = af_core::BOOTSTRAP_TOOL_NAME.to_string();
    boot.pids = Some(vec![ROOT_PID]);
    events.push(fixtures::envelope(
        "evt-boot-0000000001",
        "s",
        &ts(0),
        Payload::ActionSpan(boot),
    ));

    for i in 0..windows {
        let (t0, t1) = (i * window_ms, (i + 1) * window_ms);
        events.push(fixtures::envelope(
            &format!("evt-energy-{i:010}"),
            "s",
            &ts(t1),
            Payload::EnergySample(fixtures::energy_sample(
                &ts(t0),
                &ts(t1),
                12.5 + (i % 7) as f64,
            )),
        ));
        let rows = [
            (ROOT_PID, (i % 900) as u64, None),
            (
                ROOT_PID + 1 + i % 3,
                (i % 300) as u64,
                if i % 11 == 0 { Some("orphaned") } else { None },
            ),
        ];
        events.push(fixtures::envelope(
            &format!("evt-procs-{i:010}"),
            "s",
            &ts(t1),
            Payload::ProcessSample(fixtures::process_sample(&ts(t0), &ts(t1), &rows)),
        ));
    }

    let tools = hours * 3_600_000 / 40_000;
    for j in 0..tools {
        let start = j * 40_000 + 1_234;
        let len = match j % 20 {
            0 => 600_000,
            1 => 0,
            _ => 3_000 + (j % 13) * 1_000,
        };
        let span_id = format!("span-{j:08}");
        let mut span = ActionSpan {
            ..fixtures::action_span(&span_id, &ts(start), &ts(start + len))
        };
        if j % 5 == 0 {
            span.pids = Some(vec![ROOT_PID + 1 + j % 3]);
        }
        let tool_call_id = format!("toolu_{j:08}");
        let task_id = format!("task-{}", j / 10);
        events.push(attributed(
            fixtures::envelope(
                &format!("evt-span-{j:010}"),
                "s",
                &ts(start + len),
                Payload::ActionSpan(span),
            ),
            Some(tool_call_id.clone()),
            Some(task_id.clone()),
        ));
        for k in 0..2 {
            events.push(attributed(
                fixtures::envelope(
                    &format!("evt-call-{j:08}-{k}"),
                    "s",
                    &ts(start + k * 500),
                    Payload::LlmCall(LlmCall {
                        ..fixtures::llm_call()
                    }),
                ),
                Some(tool_call_id.clone()),
                Some(task_id.clone()),
            ));
        }
    }
    events.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.event_id.cmp(&b.event_id)));
    events
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn a_working_day_rebuilds() {
    let events = session(8);
    let started = Instant::now();
    let tree = correlate(&events);
    let mut samples = Vec::new();
    let mut procs = Vec::new();
    for event in &events {
        match &event.payload {
            Payload::EnergySample(sample) => samples.push(sample.clone()),
            Payload::ProcessSample(sample) => procs.push(sample.clone()),
            _ => {}
        }
    }
    let (apportionment, traces) = apportion_traced(&samples, &procs, &tree);
    let attributed_at = started.elapsed();
    let estimates: BTreeMap<String, serde_json::Value> = events
        .iter()
        .filter(|event| matches!(event.payload, Payload::LlmCall(_)))
        .enumerate()
        .filter(|(i, _)| i % 3 != 0)
        .map(|(i, event)| {
            let status = if i % 7 == 0 { "unknown_model" } else { "ok" };
            (
                event.event_id.clone(),
                serde_json::json!({
                    "status": status,
                    "impacts": {"energy": {"unit": "kWh", "total": {"min": 0.001 * i as f64, "max": 0.002 * i as f64}}}
                }),
            )
        })
        .collect();
    let joins = build_joins(
        "s",
        &events.first().expect("events").ts,
        &events.last().expect("events").ts,
        &events,
        &tree,
        &apportionment,
        &estimates,
        &Zone::unresolved("FRA", "test"),
        None,
    );
    let total = started.elapsed();

    // Line-sorted, so a map's iteration order (`per_span_j` is a hash map)
    // cannot change the digest; every value is still compared exactly.
    let mut dump = format!("{apportionment:#?}\n{traces:#?}\n");
    for join in &joins {
        dump.push_str(&format!("{} {}\n", join.unit_key, join.record));
    }
    let mut lines: Vec<&str> = dump.lines().collect();
    lines.sort_unstable();
    let mut hasher = DefaultHasher::new();
    lines.hash(&mut hasher);
    println!(
        "8 h session: {} events, {} samples, {} spans, {} joins; attribution {:?}, total {:?}; digest {:016x}",
        events.len(),
        samples.len(),
        tree.spans.len(),
        joins.len(),
        attributed_at,
        total,
        hasher.finish()
    );
}
