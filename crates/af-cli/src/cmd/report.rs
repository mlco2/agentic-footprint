//! `af report` and `af replay`: the two entry points into the derived-record
//! pipeline.
//!
//! `af report` runs one ingest pass, rebuilds the derived records
//! (estimates for new `llm_call`s, then `impact_join`s for every session)
//! and prints them. `af replay` skips ingest, wipes `impact_estimates` and
//! `impact_joins` first, and recomputes the same records from the raw
//! events — so a methodology change can be re-applied to history and, given
//! identical inputs, produces byte-identical output.
//!
//! Everything that varies between runs (how many events were ingested, how
//! many estimates this particular pass computed, whether a sidecar was
//! found) goes to **stderr**. Stdout carries only facts derived from the
//! stored state, which is what makes the byte-identical-replay guarantee
//! testable.

use std::path::Path;

use anyhow::Result;
use serde_json::{json, Map, Value};

use af_core::rebuild_derived;
use af_store::{SessionSummary, Store};

use super::estimator;
use super::ingest::ingest;

/// Output format for `af report --format <FORMAT>`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Text,
}

/// Which of the two commands this module's single pipeline is running as.
///
/// The pair used to be a `(wipe: bool, force: bool)` argument that every
/// caller had to assemble correctly — and `force` is meaningless without
/// `wipe`, a constraint two bools cannot express and nothing checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `af report`: ingest the spool, then rebuild the derived records.
    Report,
    /// `af replay`: no ingest; wipe the derived records first and
    /// recompute them from the raw events. `force` proceeds even with no
    /// estimator available (see [`rebuild_derived`]).
    Replay { force: bool },
}

impl Mode {
    /// The command name every diagnostic on stderr is prefixed with.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Report => "af report",
            Mode::Replay { .. } => "af replay",
        }
    }

    fn wipe(self) -> bool {
        matches!(self, Mode::Replay { .. })
    }

    fn force(self) -> bool {
        matches!(self, Mode::Replay { force: true })
    }
}

/// Runs the full pipeline against `state_dir` and prints the result.
///
/// `zone` overrides the electricity-mix zone (see
/// [`crate::cmd::resolve_zone`]).
pub fn run(
    state_dir: &Path,
    mode: Mode,
    format: Format,
    local_grid_zone: Option<&str>,
    remote_region: Option<&str>,
) -> Result<()> {
    let name = mode.name();
    let db_path = state_dir.join("state.db");

    // One connection for the whole command, opened before ingest so that
    // ingest and the rebuild share it. `af report` may be the first thing
    // that ever touches this state dir and so creates it; `af replay`
    // recomputes what is already there, and conjuring an empty database for
    // a directory that does not exist would answer a mistyped path with an
    // empty report instead of an error.
    let mut store = match mode {
        Mode::Report => {
            std::fs::create_dir_all(state_dir)?;
            let mut store = Store::open(&db_path)?;
            let summary = ingest(&mut store, state_dir)?;
            eprintln!(
                "{name}: ingested {} new event(s) from {} spool file(s), {} rejected",
                summary.ingested, summary.files, summary.rejected
            );
            store
        }
        Mode::Replay { .. } => Store::open(&db_path)?,
    };

    let (zone_id, zone_source) = super::resolve_zone(&store, local_grid_zone)?;
    let remote_region = super::resolve_remote_region(remote_region);
    let mut estimator = estimator::spawn(state_dir);
    if let Some(note) = &estimator.note {
        eprintln!("{name}: {note}");
    }

    let outcome = rebuild_derived(
        &mut store,
        estimator.sidecar.as_mut(),
        &zone_id,
        &zone_source,
        &remote_region,
        mode.wipe(),
        mode.force(),
        // Every session: what `af report` and `af replay` print is a
        // function of the whole store, and a filtered rebuild would make it
        // a function of whatever happened to be ingested last.
        None,
    )?;

    if outcome.wiped {
        eprintln!("{name}: wiped derived records before recomputing");
    }
    for note in &outcome.notes {
        eprintln!("{name}: {note}");
    }
    if let Some(estimation) = &outcome.estimation {
        eprintln!(
            "{name}: estimated {}, unknown_model {}, missing_zone {}, missing_usage {}, errors {}",
            estimation.estimated,
            estimation.unknown_model,
            estimation.missing_zone,
            estimation.missing_usage,
            estimation.errors,
        );
    }
    eprintln!(
        "{name}: local grid {} ({}, factors {}), remote region {}, {} join(s) over {} session(s), {} llm_call(s) pending",
        outcome.zone.id,
        outcome.zone.source,
        if outcome.zone.factors.is_some() {
            "available"
        } else {
            "unavailable"
        },
        remote_region.id.as_deref().unwrap_or("estimator-auto"),
        outcome.joins,
        outcome.sessions,
        outcome.pending_llm_calls,
    );

    let mut summaries = store.session_summaries()?;
    summaries.sort_by(|a, b| a.session_id.cmp(&b.session_id));

    match format {
        Format::Json => print_json(&store, &summaries)?,
        Format::Text => print_text(&summaries),
    }

    Ok(())
}

/// Emits the per-session facts summary with each session's stored
/// `impact_join` records attached.
///
/// The joins are read back **from the store** rather than reused from the
/// build step: what is printed is then provably what was persisted, and the
/// SQL `ORDER BY unit_key` fixes the emission order in one place.
fn print_json(store: &Store, summaries: &[SessionSummary]) -> Result<()> {
    let mut sessions: Vec<Value> = Vec::with_capacity(summaries.len());
    for s in summaries {
        let mut event_counts = Map::new();
        for (type_tag, count) in &s.counts {
            event_counts.insert(type_tag.clone(), json!(count));
        }
        let joins: Vec<Value> = store
            .joins_for_session(&s.session_id)?
            .into_iter()
            .map(|(unit_key, record)| json!({"unit_key": unit_key, "join": record}))
            .collect();
        sessions.push(json!({
            "session_id": s.session_id,
            "event_counts": event_counts,
            "first_ts": s.first_ts,
            "last_ts": s.last_ts,
            "joins": joins,
        }));
    }

    println!("{}", json!({ "sessions": sessions }));
    Ok(())
}

fn print_text(summaries: &[SessionSummary]) {
    // Tab-separated rather than fixed-width columns: `event_counts` grows
    // unboundedly with the number of distinct event types in a session, so
    // a fixed column width would silently misalign under real data.
    // Joins are JSON-only — a Contract #2 record has ranges, provenance and
    // honesty counters that don't survive a one-line-per-session table.
    println!("SESSION\tEVENT_COUNTS\tFIRST_TS\tLAST_TS");
    for s in summaries {
        let counts = s
            .counts
            .iter()
            .map(|(type_tag, count)| format!("{type_tag}={count}"))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{}\t{}\t{}\t{}",
            s.session_id, counts, s.first_ts, s.last_ts
        );
    }
}
