//! `af summary`: aggregated tokens and estimated impacts over a period,
//! as versioned JSON for other programs (CodeCarbon Desktop first).
//!
//! Read-only, like `af statusline`: the store is opened read-only, nothing
//! is ingested, estimated or written, and a missing database is an empty
//! summary. What it reports is what the collector had stored when it ran.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use af_events::{Envelope, Payload};
use af_store::Store;
use anyhow::Result;
use serde_json::{json, Map, Value};

use super::agents::agent_for_collector;

/// Bumped only when a consumer written against the previous shape would
/// misread this one. Adding a field does not bump it.
pub const PROTOCOL_VERSION: u32 = 1;

const DAY_MS: i64 = 86_400_000;

/// How far back the summary reaches, in local calendar days ending today.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Today,
    #[value(name = "7d")]
    Week,
    #[value(name = "30d")]
    Month,
}

impl Period {
    fn days(self) -> i64 {
        match self {
            Period::Today => 1,
            Period::Week => 7,
            Period::Month => 30,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Period::Today => "today",
            Period::Week => "7d",
            Period::Month => "30d",
        }
    }
}

/// `af summary` arguments.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// Local calendar days covered, ending today.
    #[arg(long, value_enum, default_value = "today")]
    pub period: Period,
    /// Minutes east of UTC that "a day" is cut in. Defaults to this
    /// machine's current offset, or UTC when that cannot be read.
    #[arg(long, allow_hyphen_values = true)]
    pub utc_offset_minutes: Option<i32>,
}

/// Prints the summary. Always exits 0 with a JSON document unless the
/// store exists and cannot be read.
pub fn run(state_dir: &Path, args: Args) -> Result<i32> {
    let offset = args.utc_offset_minutes.unwrap_or_else(local_offset_minutes);
    let db = state_dir.join("state.db");
    let store = db
        .exists()
        .then(|| Store::open_read_only(&db))
        .transpose()?;
    let mut summary = build(store.as_ref(), args.period, super::now_ms(), offset)?;
    summary["collector"]["running"] = collector_running(state_dir);
    println!("{}", serde_json::to_string(&summary)?);
    Ok(0)
}

fn local_offset_minutes() -> i32 {
    time::UtcOffset::current_local_offset()
        .map(|offset| i32::from(offset.whole_minutes()))
        .unwrap_or(0)
}

/// Whether a collector answers on its console socket; `null` where there
/// is no such socket to ask.
fn collector_running(state_dir: &Path) -> Value {
    if cfg!(unix) {
        json!(super::debug_server::socket_is_served(
            &state_dir.join(super::debug_server::SOCKET_NAME)
        ))
    } else {
        Value::Null
    }
}

/// One group's running totals.
#[derive(Default)]
struct Figures {
    calls: u64,
    sessions: BTreeSet<String>,
    input: u64,
    output: u64,
    thought: u64,
    cache_read: u64,
    cache_write: u64,
    /// Stored estimates that can be summed (`status: ok`, with impacts).
    estimates: Vec<Value>,
    /// Calls with no summable estimate, by why: the estimator's status, or
    /// `pending` when it has not stored one yet.
    unestimated: BTreeMap<String, u64>,
}

impl Figures {
    fn add(&mut self, event: &Envelope, estimate: Option<&Value>) {
        let Payload::LlmCall(call) = &event.payload else {
            return;
        };
        self.calls += 1;
        self.sessions.insert(event.session_id.clone());
        self.input += call.usage.input_tokens.unwrap_or(0);
        self.output += call.usage.output_tokens.unwrap_or(0);
        self.thought += call.usage.thought_tokens.unwrap_or(0);
        self.cache_read += call.usage.cached_read_tokens.unwrap_or(0);
        self.cache_write += call.usage.cached_write_tokens.unwrap_or(0);
        let status = estimate
            .and_then(|e| e.get("status"))
            .and_then(Value::as_str);
        match (estimate, status) {
            (Some(estimate), Some("ok")) if estimate.get("impacts").is_some() => {
                self.estimates.push(estimate.clone())
            }
            (_, status) => {
                *self
                    .unestimated
                    .entry(status.unwrap_or("pending").to_string())
                    .or_default() += 1
            }
        }
    }

    fn total_tokens(&self) -> u64 {
        self.input + self.output + self.thought + self.cache_read + self.cache_write
    }

    /// The group's fields, merged into `head` (its identifying keys).
    fn json(&self, head: Value) -> Value {
        let estimated = self.estimates.len() as u64;
        // A criterion some estimate reported in another unit is left out
        // rather than summed across units.
        let remote: Map<String, Value> =
            af_core::sum_criteria(&self.estimates.iter().collect::<Vec<_>>())
                .into_iter()
                .filter(|(_, sum)| sum.mismatches == 0)
                .map(|(criterion, sum)| {
                    (
                        criterion,
                        json!({"unit": sum.unit, "total": {"min": sum.min, "max": sum.max}}),
                    )
                })
                .collect();
        let mut out = match head {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        out.insert("calls".into(), json!(self.calls));
        out.insert("sessions".into(), json!(self.sessions.len()));
        out.insert(
            "tokens".into(),
            json!({
                "input": self.input,
                "output": self.output,
                "thought": self.thought,
                "cache_read": self.cache_read,
                "cache_write": self.cache_write,
                "total": self.total_tokens(),
            }),
        );
        out.insert("estimated_calls".into(), json!(estimated));
        out.insert("unestimated_calls".into(), json!(self.calls - estimated));
        out.insert("unestimated_by_reason".into(), json!(self.unestimated));
        // Modelled, as ranges: what the estimator said about the remote
        // inference. Nothing measured locally is added here.
        out.insert("remote".into(), Value::Object(remote));
        Value::Object(out)
    }
}

/// The local calendar day `ms` falls in, as days since the epoch.
fn local_day(ms: i64, offset_minutes: i32) -> i64 {
    (ms + i64::from(offset_minutes) * 60_000).div_euclid(DAY_MS)
}

/// `YYYY-MM-DD` of a local day index.
fn date_of(day: i64) -> String {
    af_core::rfc3339_ms(day * DAY_MS)
        .map(|ts| ts[..10].to_string())
        .unwrap_or_default()
}

/// The summary document, without `collector.running` (which is about the
/// machine, not the store).
pub fn build(
    store: Option<&Store>,
    period: Period,
    now_ms: i64,
    offset_minutes: i32,
) -> Result<Value> {
    let today = local_day(now_ms, offset_minutes);
    let first_day = today - (period.days() - 1);
    let from_ms = first_day * DAY_MS - i64::from(offset_minutes) * 60_000;

    let mut totals = Figures::default();
    let mut by_agent: BTreeMap<(String, String), Figures> = BTreeMap::new();
    let mut by_model: BTreeMap<(String, String), Figures> = BTreeMap::new();
    let mut by_day: BTreeMap<i64, Figures> = (first_day..=today)
        .map(|day| (day, Figures::default()))
        .collect();
    let mut last_event_at = None;

    if let Some(store) = store {
        last_event_at = store.last_event_ts()?;
        // A day's margin as text, then the instants compared parsed: see
        // `Store::llm_calls_with_ts_between`.
        let bound = |ms: i64| af_core::rfc3339_ms(ms).unwrap_or_default();
        let mut calls =
            store.llm_calls_with_ts_between(&bound(from_ms - DAY_MS), &bound(now_ms + DAY_MS))?;
        calls.retain(|event| {
            af_core::parse_ts(&event.ts).is_some_and(|at| (from_ms..=now_ms).contains(&at))
        });
        let ids: Vec<String> = calls.iter().map(|event| event.event_id.clone()).collect();
        let estimates = store.estimates_for_events(&ids)?;
        for event in &calls {
            let Payload::LlmCall(call) = &event.payload else {
                continue;
            };
            let Some(at) = af_core::parse_ts(&event.ts) else {
                continue;
            };
            let estimate = estimates.get(&event.event_id);
            let (agent, name) = match agent_for_collector(&event.collector.name) {
                Some(agent) => (agent.id().to_string(), agent.name().to_string()),
                None => ("other".to_string(), "Other".to_string()),
            };
            let model = call
                .model_id_served
                .clone()
                .unwrap_or_else(|| call.model_id_requested.clone());
            totals.add(event, estimate);
            by_agent
                .entry((agent.clone(), name))
                .or_default()
                .add(event, estimate);
            by_model
                .entry((agent, model))
                .or_default()
                .add(event, estimate);
            if let Some(day) = by_day.get_mut(&local_day(at, offset_minutes)) {
                day.add(event, estimate);
            }
        }
    }

    // Largest first: what a reader looks for.
    let ranked = |mut rows: Vec<(u64, Value)>| -> Vec<Value> {
        rows.sort_by_key(|row| std::cmp::Reverse(row.0));
        rows.into_iter().map(|(_, row)| row).collect()
    };
    Ok(json!({
        "protocol_version": PROTOCOL_VERSION,
        "af_version": env!("CARGO_PKG_VERSION"),
        "generated_at": af_core::rfc3339_ms(now_ms),
        "period": {
            "name": period.name(),
            "from": af_core::rfc3339_ms(from_ms),
            "to": af_core::rfc3339_ms(now_ms),
            "utc_offset_minutes": offset_minutes,
        },
        "collector": {"running": Value::Null, "last_event_at": last_event_at},
        "totals": totals.json(json!({})),
        "by_agent": ranked(
            by_agent
                .iter()
                .map(|((agent, name), figures)| {
                    (figures.total_tokens(), figures.json(json!({"agent": agent, "name": name})))
                })
                .collect()
        ),
        "by_model": ranked(
            by_model
                .iter()
                .map(|((agent, model), figures)| {
                    (figures.total_tokens(), figures.json(json!({"agent": agent, "model": model})))
                })
                .collect()
        ),
        "by_day": by_day
            .iter()
            .map(|(day, figures)| figures.json(json!({"date": date_of(*day)})))
            .collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_events::{fixtures, Collector, Usage};
    use af_store::ImpactEstimate;

    /// 2026-11-16T12:00:00Z.
    const NOON: i64 = 1_794_830_400_000;

    fn call(
        id: &str,
        session: &str,
        at_ms: i64,
        collector: &str,
        model: &str,
        usage: Usage,
    ) -> Envelope {
        let mut payload = fixtures::llm_call();
        payload.model_id_requested = model.to_string();
        payload.usage = usage;
        let mut event = fixtures::envelope(
            id,
            session,
            &af_core::rfc3339_ms(at_ms).expect("ts"),
            Payload::LlmCall(payload),
        );
        event.collector = Collector {
            name: collector.to_string(),
            version: "1".to_string(),
        };
        event
    }

    fn tokens(input: u64, output: u64, cache_read: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_read_tokens: Some(cache_read),
            ..Default::default()
        }
    }

    fn estimate(event_id: &str, gwp: (f64, f64)) -> ImpactEstimate {
        ImpactEstimate {
            event_id: event_id.to_string(),
            methodology_version: "test".to_string(),
            // The shape the estimator stores (copied from a real row).
            json: json!({
                "id": 1,
                "status": "ok",
                "impacts": {"gwp": {
                    "unit": "kgCO2eq",
                    "total": {"min": gwp.0, "max": gwp.1},
                    "usage": {"min": gwp.0, "max": gwp.1},
                }},
                "methodology": {"ecologits_version": "0.11.3", "source": "bundled"},
            }),
        }
    }

    fn store_with(events: &[Envelope], estimates: &[ImpactEstimate]) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        store.insert_events(events).expect("insert");
        store.upsert_estimates(estimates).expect("estimates");
        (dir, store)
    }

    /// The published contract: what CodeCarbon Desktop is written against.
    fn assert_matches_the_schema(summary: &Value) {
        let schema: Value =
            serde_json::from_str(include_str!("../../../../schemas/summary/v1.schema.json"))
                .expect("schema is JSON");
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        let errors: Vec<String> = validator
            .iter_errors(summary)
            .map(|error| format!("{}: {error}", error.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{errors:#?}\n{summary:#}");
    }

    #[test]
    fn no_store_is_an_empty_summary_with_every_day_of_the_period() {
        let summary = build(None, Period::Week, NOON, 0).expect("build");
        assert_matches_the_schema(&summary);
        assert_eq!(summary["protocol_version"], 1);
        assert_eq!(summary["period"]["name"], "7d");
        assert_eq!(summary["totals"]["calls"], 0);
        assert_eq!(summary["totals"]["tokens"]["total"], 0);
        assert_eq!(summary["by_agent"], json!([]));
        let days: Vec<&str> = summary["by_day"]
            .as_array()
            .unwrap()
            .iter()
            .map(|day| day["date"].as_str().unwrap())
            .collect();
        assert_eq!(days.len(), 7);
        assert_eq!((days[0], days[6]), ("2026-11-10", "2026-11-16"));
        assert_eq!(summary["collector"]["last_event_at"], Value::Null);
    }

    #[test]
    fn calls_are_summed_by_agent_model_and_day_and_older_ones_left_out() {
        let hour = 3_600_000;
        let events = [
            call(
                "c1",
                "s1",
                NOON - hour,
                "otlp-cc",
                "claude-opus-5-5",
                tokens(1000, 100, 5000),
            ),
            call(
                "c2",
                "s1",
                NOON - 2 * hour,
                "otlp-cc",
                "claude-haiku-5-5",
                tokens(10, 1, 0),
            ),
            call(
                "c3",
                "s2",
                NOON - 26 * hour,
                "codex",
                "gpt-x",
                tokens(200, 20, 0),
            ),
            // Eight days ago: outside a 7-day period.
            call(
                "c4",
                "s3",
                NOON - 8 * 24 * hour,
                "otlp-cc",
                "claude-opus-5-5",
                tokens(9, 9, 9),
            ),
        ];
        let unknown = ImpactEstimate {
            event_id: "c2".to_string(),
            methodology_version: "test".to_string(),
            json: json!({"id": 2, "status": "unknown_model"}),
        };
        let (_dir, store) = store_with(&events, &[estimate("c1", (0.001, 0.003)), unknown]);
        let summary = build(Some(&store), Period::Week, NOON, 0).expect("build");
        assert_matches_the_schema(&summary);

        let totals = &summary["totals"];
        assert_eq!(totals["calls"], 3);
        assert_eq!(totals["sessions"], 2);
        assert_eq!(totals["tokens"]["input"], 1210);
        assert_eq!(totals["tokens"]["output"], 121);
        assert_eq!(totals["tokens"]["cache_read"], 5000);
        assert_eq!(totals["tokens"]["total"], 6331);
        assert_eq!(
            (&totals["estimated_calls"], &totals["unestimated_calls"]),
            (&json!(1), &json!(2))
        );
        assert_eq!(
            totals["unestimated_by_reason"],
            json!({"pending": 1, "unknown_model": 1}),
            "why a call has no figure: not estimated yet, or a model the estimator does not know"
        );
        assert_eq!(totals["remote"]["gwp"]["unit"], "kgCO2eq");
        assert_eq!(totals["remote"]["gwp"]["total"]["max"], 0.003);

        let agents = summary["by_agent"].as_array().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(
            (&agents[0]["agent"], &agents[0]["name"]),
            (&json!("claude-code"), &json!("Claude Code"))
        );
        assert_eq!(agents[0]["tokens"]["total"], 6111);
        assert_eq!(agents[1]["agent"], "codex");
        assert_eq!(agents[1]["remote"], json!({}), "nothing estimated for it");

        let models = summary["by_model"].as_array().unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0]["model"], "claude-opus-5-5", "largest first");
        assert_eq!(
            models[0]["calls"], 1,
            "the call outside the period is not counted"
        );

        let days = summary["by_day"].as_array().unwrap();
        assert_eq!(days[6]["date"], "2026-11-16");
        assert_eq!(days[6]["calls"], 2);
        assert_eq!(days[5]["calls"], 1);
        assert_eq!(days[0]["calls"], 0);
        assert!(summary["collector"]["last_event_at"].is_string());
    }

    #[test]
    fn a_day_is_the_local_one() {
        // 23:30 UTC on the 15th is 00:30 on the 16th at UTC+1.
        let late = NOON - 12 * 3_600_000 - 30 * 60_000;
        let events = [call("c1", "s1", late, "codex", "m", tokens(1, 1, 0))];
        let (_dir, store) = store_with(&events, &[]);

        let utc = build(Some(&store), Period::Today, NOON, 0).expect("build");
        assert_eq!(utc["totals"]["calls"], 0, "yesterday, in UTC");
        let paris = build(Some(&store), Period::Today, NOON, 60).expect("build");
        assert_eq!(paris["totals"]["calls"], 1, "today, at UTC+1");
        assert_eq!(paris["by_day"][0]["date"], "2026-11-16");
        assert_eq!(paris["period"]["from"], "2026-11-15T23:00:00.000Z");
    }

    #[test]
    fn timestamps_without_milliseconds_are_placed_like_any_other() {
        // Collectors do not agree on precision, and "…:00Z" sorts after
        // "…:00.500Z" as text: the period is cut on parsed instants.
        let mut edge = call("c1", "s1", 0, "codex", "m", tokens(1, 0, 0));
        edge.ts = "2026-11-16T00:00:00Z".to_string();
        let mut before = call("c2", "s1", 0, "codex", "m", tokens(1, 0, 0));
        before.ts = "2026-11-15T23:59:59.900Z".to_string();
        let (_dir, store) = store_with(&[edge, before], &[]);
        let summary = build(Some(&store), Period::Today, NOON, 0).expect("build");
        assert_eq!(
            summary["totals"]["calls"], 1,
            "midnight is today, 100 ms earlier is not"
        );
    }

    #[test]
    fn a_collector_that_is_no_known_agent_is_other() {
        let events = [call(
            "c1",
            "s1",
            NOON - 1000,
            "some-sdk",
            "m",
            tokens(5, 5, 0),
        )];
        let (_dir, store) = store_with(&events, &[]);
        let summary = build(Some(&store), Period::Today, NOON, 0).expect("build");
        assert_eq!(summary["by_agent"][0]["agent"], "other");
    }
}
