//! `afoot doctor`: read-only diagnosis of the failures that make an agent
//! look connected while its data never arrives.
//!
//! * the receiver on the endpoint is not an afoot receiver (or not this one);
//! * the managed CodeCarbon/EcoLogits runtime is missing or broken;
//! * an agent process predates the configuration it reads at startup —
//!   Claude Code reloads hooks while running but not its OTLP exporter
//!   settings, so such a process records tool spans and no token usage;
//! * the store already shows that symptom: recent sessions with tool
//!   activity and no `llm_call`.
//!
//! Exits 1 when anything needs attention, so scripts and the installer can
//! act on it.

use std::path::Path;

use anyhow::Result;

use super::agents::{running_agents, stale_processes, startup_configs};
use super::DEFAULT_LOGS_ENDPOINT;

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    /// OTLP HTTP/JSON logs endpoint the agents were configured with.
    #[arg(long, default_value = DEFAULT_LOGS_ENDPOINT)]
    endpoint: String,
    /// How far back to look for sessions without token telemetry.
    #[arg(long, default_value_t = 7)]
    days: u32,
}

pub fn run(state_dir: &Path, args: Args) -> Result<i32> {
    let mut problems = 0;
    println!("afoot doctor");

    match super::service::receiver_reachable(&args.endpoint) {
        Ok(()) => println!("  ✓ receiver   healthy at {}", args.endpoint),
        Err(error) => {
            problems += 1;
            println!("  ! receiver   {error:#}");
            println!("               run `afoot service install` (or `afoot watch` in a terminal)");
        }
    }

    // The managed CodeCarbon/EcoLogits runtime, reported as one line: the
    // user's remedy is the same whatever broke inside it.
    match afoot_sidecar::runtime_status(state_dir) {
        Ok(versions) => println!(
            "  ✓ runtime    ecologits {} / codecarbon {}",
            versions.ecologits, versions.codecarbon
        ),
        Err(finding) => {
            problems += 1;
            println!("  ! runtime    {}", finding.message);
            println!("               run `afoot self update` (or rerun the installer)");
        }
    }

    let home = crate::paths::home_dir();
    let configs = home.as_deref().map(startup_configs).unwrap_or_default();
    match running_agents() {
        Some(processes) => {
            let stale = stale_processes(&processes, &configs);
            if stale.is_empty() {
                println!("  ✓ processes  no running agent predates its telemetry configuration");
            }
            for (process, config) in &stale {
                problems += 1;
                println!(
                    "  ! {:<10} PID {} started before {} last changed; restart it so its telemetry is exported",
                    config.agent,
                    process.pid,
                    config.path.display()
                );
            }
        }
        None => println!("  – processes  not checked on this platform"),
    }

    let db = state_dir.join("state.db");
    if db.is_file() {
        let store = afoot_store::Store::open(&db)?;
        let since = since_timestamp(args.days);
        let gaps = store.sessions_without_llm_calls(&since)?;
        if gaps.is_empty() {
            println!(
                "  ✓ telemetry  every session with tool activity in the last {} day(s) has token telemetry",
                args.days
            );
        } else {
            problems += 1;
            println!(
                "  ! telemetry  {} session(s) in the last {} day(s) recorded tool activity but no token telemetry:",
                gaps.len(),
                args.days
            );
            for gap in &gaps {
                println!(
                    "               {}  {} tool span(s), last active {}",
                    gap.session_id, gap.tool_spans, gap.last_ts
                );
            }
            println!(
                "               restart the agent process that ran them (it started before telemetry was configured),"
            );
            println!("               or check `afoot setup --check` from that project");
        }
    } else {
        println!("  – telemetry  no data received yet");
    }

    println!(
        "\nCodex sessions launched with a per-project CODEX_HOME read <project>/.codex/config.toml;\n\
         run `afoot setup --check` from each such project."
    );
    if problems == 0 {
        println!("\nNo problems found.");
        Ok(0)
    } else {
        println!("\n{problems} problem(s) found.");
        Ok(1)
    }
}

/// `days` ago, in the fixed-width millisecond UTC form the store's `ts`
/// column holds, so the two compare as text.
fn since_timestamp(days: u32) -> String {
    afoot_core::rfc3339_ms(super::now_ms() - i64::from(days) * 86_400_000).unwrap_or_default()
}
