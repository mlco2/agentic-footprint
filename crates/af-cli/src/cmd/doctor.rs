//! `af doctor`: read-only diagnosis of the failures that make an agent
//! look connected while its data never arrives.
//!
//! * the receiver on the endpoint is not an af receiver (or not this one);
//! * the managed CodeCarbon/EcoLogits runtime is missing or broken;
//! * an agent process predates the configuration it reads at startup —
//!   Claude Code reloads hooks while running but not its OTLP exporter
//!   settings, so such a process records tool spans and no token usage;
//! * the store already shows that symptom: recent sessions with tool
//!   activity and no `llm_call`.
//!
//! Exits 1 when anything needs attention, so scripts and the installer can
//! act on it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::Result;

use super::setup::{self, CodexOtel};
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

/// A running agent process, as far as `ps` can say.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentProcess {
    pid: u32,
    name: String,
    started: SystemTime,
}

/// A configuration file an agent reads once at process start.
struct StartupConfig {
    agent: &'static str,
    path: PathBuf,
    modified: SystemTime,
}

pub fn run(state_dir: &Path, args: Args) -> Result<i32> {
    let mut problems = 0;
    println!("af doctor");

    match super::service::receiver_reachable(&args.endpoint) {
        Ok(()) => println!("  ✓ receiver   healthy at {}", args.endpoint),
        Err(error) => {
            problems += 1;
            println!("  ! receiver   {error:#}");
            println!("               run `af service install` (or `af watch` in a terminal)");
        }
    }

    // The managed CodeCarbon/EcoLogits runtime, reported as one line: the
    // user's remedy is the same whatever broke inside it.
    match af_sidecar::runtime_status(state_dir) {
        Ok(versions) => println!(
            "  ✓ runtime    ecologits {} / codecarbon {}",
            versions.ecologits, versions.codecarbon
        ),
        Err(finding) => {
            problems += 1;
            println!("  ! runtime    {}", finding.message);
            println!("               run `af self update` (or rerun the installer)");
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
        let store = af_store::Store::open(&db)?;
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
            println!("               or check `af setup --check` from that project");
        }
    } else {
        println!("  – telemetry  no data received yet");
    }

    println!(
        "\nCodex sessions launched with a per-project CODEX_HOME read <project>/.codex/config.toml;\n\
         run `af setup --check` from each such project."
    );
    if problems == 0 {
        println!("\nNo problems found.");
        Ok(0)
    } else {
        println!("\n{problems} problem(s) found.");
        Ok(1)
    }
}

/// The per-user files whose telemetry settings each agent reads once, at
/// start. Only files that carry OTEL configuration count, judged the way
/// `af setup` reads them: an unrelated edit to a file setup never touched
/// says nothing about telemetry.
fn startup_configs(home: &Path) -> Vec<StartupConfig> {
    let candidates: [(&str, PathBuf, SetsOtel); 2] = [
        (
            "claude",
            setup::claude_user_settings(home),
            claude_sets_otel,
        ),
        ("codex", setup::codex_config_path(home), codex_sets_otel),
    ];
    candidates
        .into_iter()
        .filter_map(|(agent, path, has_otel)| {
            let contents = std::fs::read_to_string(&path).ok()?;
            if !has_otel(&contents) {
                return None;
            }
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some(StartupConfig {
                agent,
                path,
                modified,
            })
        })
        .collect()
}

/// Whether a config file's contents carry OTEL configuration.
type SetsOtel = fn(&str) -> bool;

fn claude_sets_otel(contents: &str) -> bool {
    serde_json::from_str(contents).is_ok_and(|settings| setup::claude_settings_set_otel(&settings))
}

fn codex_sets_otel(contents: &str) -> bool {
    setup::codex_otel_state(contents, DEFAULT_LOGS_ENDPOINT) != CodexOtel::Absent
}

fn stale_processes<'a>(
    processes: &'a [AgentProcess],
    configs: &'a [StartupConfig],
) -> Vec<(&'a AgentProcess, &'a StartupConfig)> {
    processes
        .iter()
        .filter_map(|process| {
            let config = configs.iter().find(|config| config.agent == process.name)?;
            (process.started < config.modified).then_some((process, config))
        })
        .collect()
}

/// Running `claude`/`codex` processes, or `None` where `ps` is unavailable.
fn running_agents() -> Option<Vec<AgentProcess>> {
    if !cfg!(unix) {
        return None;
    }
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,etime=,comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let now = SystemTime::now();
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| parse_ps_line(line, now))
            .filter(|process| process.name == "claude" || process.name == "codex")
            .collect(),
    )
}

fn parse_ps_line(line: &str, now: SystemTime) -> Option<AgentProcess> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let elapsed = parse_etime(fields.next()?)?;
    let command = fields.collect::<Vec<_>>().join(" ");
    let name = command.rsplit('/').next()?.to_string();
    Some(AgentProcess {
        pid,
        name,
        started: now.checked_sub(elapsed)?,
    })
}

/// `ps -o etime` elapsed time: `[[dd-]hh:]mm:ss`.
fn parse_etime(value: &str) -> Option<Duration> {
    let (days, clock) = match value.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, value),
    };
    let parts: Vec<u64> = clock
        .split(':')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    Some(Duration::from_secs(
        ((days * 24 + hours) * 60 + minutes) * 60 + seconds,
    ))
}

/// `days` ago, in the fixed-width millisecond UTC form the store's `ts`
/// column holds, so the two compare as text.
fn since_timestamp(days: u32) -> String {
    af_core::rfc3339_ms(super::now_ms() - i64::from(days) * 86_400_000).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parses_every_ps_shape() {
        assert_eq!(parse_etime("05:07"), Some(Duration::from_secs(307)));
        assert_eq!(parse_etime("02:04:45"), Some(Duration::from_secs(7485)));
        assert_eq!(
            parse_etime("24-00:23:22"),
            Some(Duration::from_secs(24 * 86_400 + 23 * 60 + 22))
        );
        assert_eq!(parse_etime("garbage"), None);
        assert_eq!(parse_etime("1:2:3:4"), None);
    }

    #[test]
    fn ps_lines_yield_basename_and_start_time() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let process = parse_ps_line(" 45737 24-00:23:22 /usr/local/bin/claude", now).unwrap();
        assert_eq!(process.pid, 45737);
        assert_eq!(process.name, "claude");
        assert_eq!(
            process.started,
            now - Duration::from_secs(24 * 86_400 + 23 * 60 + 22)
        );
    }

    #[test]
    fn only_processes_started_before_their_config_changed_are_stale() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let processes = [
            AgentProcess {
                pid: 1,
                name: "claude".into(),
                started: at(100),
            },
            AgentProcess {
                pid: 2,
                name: "claude".into(),
                started: at(300),
            },
            AgentProcess {
                pid: 3,
                name: "codex".into(),
                started: at(100),
            },
        ];
        let configs = [StartupConfig {
            agent: "claude",
            path: PathBuf::from("/home/.claude/settings.json"),
            modified: at(200),
        }];
        let stale: Vec<u32> = stale_processes(&processes, &configs)
            .into_iter()
            .map(|(process, _)| process.pid)
            .collect();
        // codex has no configuration on record, so nothing to compare.
        assert_eq!(stale, [1]);
    }
}
