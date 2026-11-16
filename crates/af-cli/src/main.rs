mod cmd;
mod paths;
mod statusline;

use clap::Subcommand;

use cmd::report::{Format, Mode};

#[derive(clap::Parser)]
#[command(name = "af")]
#[command(about = "agentic-footprint: environmental impact of coding agents")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Detect and configure supported coding agents
    Setup(cmd::setup::Args),
    /// Manage the resident per-user `af watch` receiver
    Service(cmd::service::Args),
    /// Open the debug console of the collector that is already running
    Console(cmd::console::Args),
    /// Diagnose why an agent's data is not arriving (read-only)
    Doctor(cmd::doctor::Args),
    /// Update af itself (and its managed Python runtime)
    #[command(name = "self")]
    SelfCmd(cmd::self_update::Args),
    #[cfg(feature = "experimental-opencode")]
    /// Run an experimental native coding-agent collector
    Collect {
        #[command(subcommand)]
        collector: CollectAction,
    },
    /// On-demand ingestion and result emission
    Report {
        /// Output format for the per-session facts summary.
        #[arg(long, value_enum, default_value = "json")]
        format: Format,
        /// Local machine grid zone (e.g. FRA, WOR). Defaults to
        /// `$AF_LOCAL_GRID_ZONE`, then `$AF_ZONE`,
        /// then the zone the sessions declare in `session_meta.geo_zone`,
        /// then WOR. This affects only locally measured energy.
        #[arg(long = "local-grid-zone", visible_alias = "zone")]
        local_grid_zone: Option<String>,
        /// Optional remote inference region override. When absent, the
        /// estimator detects or defaults the remote region itself.
        #[arg(long)]
        remote_region: Option<String>,
    },
    /// Wipe the derived records and recompute them from the raw events
    Replay {
        /// Output format for the per-session facts summary.
        #[arg(long, value_enum, default_value = "json")]
        format: Format,
        /// Local machine grid zone to recompute under (see `af report`).
        #[arg(long = "local-grid-zone", visible_alias = "zone")]
        local_grid_zone: Option<String>,
        /// Optional remote inference region override for recomputed
        /// estimates. When absent, the estimator detects it.
        #[arg(long)]
        remote_region: Option<String>,
        /// Wipe and recompute even when no estimator is available. The
        /// stored estimates are deleted and cannot be rebuilt, so every
        /// llm_call comes back `pending` until `af python setup` and
        /// another `af replay`.
        #[arg(long)]
        force: bool,
    },
    /// Resident mode with fs-watch and Python sidecar supervision
    Watch {
        /// Print one line per ingest/attribution decision to stderr AND
        /// serve the debug console API on `--debug-addr`. Both surfaces
        /// render the same four decision kinds (`[ingest]`, `[span open]`,
        /// `[attr]`, `[orphan]`).
        #[arg(long)]
        debug: bool,
        /// Do not spawn the Python sidecars (codecarbon sampler, ecologits
        /// estimator). Ingestion, correlation and attribution still run;
        /// local energy is simply never measured and remote impacts stay
        /// `pending`.
        #[arg(long)]
        no_sidecars: bool,
        /// Address for the local OTLP http/json receiver (`POST /v1/logs`,
        /// `POST /v1/metrics`). Loopback only (127.0.0.1, ::1). A port that
        /// is already in use is reported and the watch continues without a
        /// receiver.
        #[arg(long, default_value = cmd::watch::DEFAULT_OTLP_ADDR, conflicts_with = "no_otlp")]
        otlp_addr: String,
        /// Do not start the OTLP receiver at all. Mutually exclusive with
        /// `--otlp-addr`: asking for an address and asking for no server is
        /// a contradiction worth an error rather than a silent winner.
        #[arg(long)]
        no_otlp: bool,
        /// Exit instead of continuing when the OTLP receiver cannot bind
        /// (the resident service passes this, so its manager retries).
        #[arg(long, conflicts_with = "no_otlp")]
        require_receiver: bool,
        /// Address for the `/debug` HTTP+SSE console API. Only bound with
        /// `--debug`. Loopback only (127.0.0.1, ::1).
        #[arg(long, default_value = cmd::watch::DEFAULT_DEBUG_ADDR)]
        debug_addr: String,
        /// Sampler window length in seconds, passed to the codecarbon
        /// sidecar.
        #[arg(long, default_value_t = 5.0)]
        interval: f64,
        /// Local machine grid zone (see `af report --local-grid-zone`).
        #[arg(long = "local-grid-zone", visible_alias = "zone")]
        local_grid_zone: Option<String>,
        /// Optional remote inference region override. When absent, the
        /// estimator detects it.
        #[arg(long)]
        remote_region: Option<String>,
        /// Milliseconds between flushes: received telemetry, estimates and
        /// derived-view rebuilds are written together, in one commit, this
        /// often (tests pass 0). A new session is still written at once.
        #[arg(long, default_value_t = cmd::watch::DEFAULT_BATCH_INTERVAL_MS, hide = true)]
        batch_interval_ms: u64,
        /// How far back, in milliseconds, a console is told about what it
        /// was not there to see. 0: everything (tests and fixture replays).
        #[arg(long, default_value_t = cmd::debug_server::SNAPSHOT_WINDOW_MS as u64, hide = true)]
        console_window_ms: u64,
    },
    /// Python runtime setup and diagnostics. Hidden: the installer and
    /// `af self update` drive it, and `af doctor` reports its health.
    #[command(hide = true)]
    Python {
        #[command(subcommand)]
        action: PythonAction,
    },
    /// Read Claude Code's status JSON on stdin and print one line of
    /// session impacts: `<gwp_kg> <water_L> <energy_kWh> <adpe_kg> <pe_MJ>`
    /// (range means). Read-only and never fails: no ingest, no estimation,
    /// no writes, zeros when the session has no stored join yet.
    Statusline,
    /// Claude Code hooks collector: read one hook event's JSON on stdin
    /// and append Contract #1 events to the spool. Registered by
    /// `af setup` for every hook event; dispatches on the payload's
    /// `hook_event_name`. Best-effort and always exits 0 — it must never
    /// be the reason a Claude Code turn fails.
    Hook,
    /// Validate one Contract #1 event line from stdin (hidden test helper)
    #[command(hide = true)]
    ValidateLine,
}

#[derive(Subcommand)]
enum PythonAction {
    /// Provision the managed venv (`uv venv` + `uv pip install`, pins
    /// from `python/manifest.toml`)
    Setup,
    /// Diagnose the managed venv and print actionable findings
    Doctor,
    /// Upgrade ecologits/codecarbon to the newest releases outside the
    /// 7-day cooldown (smoke-tested; the current runtime is kept on failure)
    Upgrade {
        /// Print the resolved versions without installing
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(feature = "experimental-opencode")]
#[derive(Subcommand)]
enum CollectAction {
    /// Collect OpenCode's durable per-session SSE events
    Opencode(cmd::opencode::Args),
}

fn main() {
    let cli = <Cli as clap::Parser>::parse();

    match cli.command {
        Commands::Setup(args) => {
            let state_dir = paths::state_dir();
            exit_with("af setup", cmd::setup::run(&state_dir, args).map(|()| 0));
        }
        Commands::Doctor(args) => {
            exit_with("af doctor", cmd::doctor::run(&paths::state_dir(), args))
        }
        Commands::SelfCmd(args) => {
            exit_with("af self", cmd::self_update::run(&paths::state_dir(), args))
        }
        Commands::Console(args) => {
            exit_with("af console", cmd::console::run(&paths::state_dir(), args))
        }
        Commands::Service(args) => {
            let state_dir = paths::state_dir();
            exit_with(
                "af service",
                cmd::service::run(&state_dir, args).map(|()| 0),
            );
        }
        #[cfg(feature = "experimental-opencode")]
        Commands::Collect { collector } => {
            let state_dir = paths::state_dir();
            let result = match collector {
                CollectAction::Opencode(args) => cmd::opencode::run(&state_dir, args),
            };
            exit_with("af collect opencode", result.map(|()| 0));
        }
        Commands::Report {
            format,
            local_grid_zone,
            remote_region,
        } => run_report(Mode::Report, format, local_grid_zone, remote_region),
        Commands::Replay {
            format,
            local_grid_zone,
            remote_region,
            force,
        } => run_report(
            Mode::Replay { force },
            format,
            local_grid_zone,
            remote_region,
        ),
        Commands::Watch {
            debug,
            no_sidecars,
            otlp_addr,
            no_otlp,
            require_receiver,
            debug_addr,
            interval,
            local_grid_zone,
            remote_region,
            batch_interval_ms,
            console_window_ms,
        } => {
            let state_dir = paths::state_dir();
            let args = cmd::watch::WatchArgs {
                debug,
                no_sidecars,
                no_otlp,
                require_receiver,
                otlp_addr,
                debug_addr,
                interval,
                local_grid_zone,
                remote_region,
                batch_interval: std::time::Duration::from_millis(batch_interval_ms),
                console_window_ms: i64::try_from(console_window_ms).unwrap_or(i64::MAX),
            };
            exit_with("af watch", cmd::watch::run(&state_dir, args).map(|()| 0));
        }
        Commands::Python { action } => {
            let state_dir = paths::state_dir();
            match action {
                PythonAction::Setup => {
                    exit_with(
                        "af python setup",
                        cmd::python::run_setup(&state_dir).map(|()| 0),
                    );
                }
                PythonAction::Upgrade { dry_run } => exit_with(
                    "af python upgrade",
                    cmd::python_upgrade::run(&state_dir, dry_run),
                ),
                PythonAction::Doctor => {
                    let code = cmd::python::run_doctor(&state_dir);
                    std::process::exit(code);
                }
            }
        }
        Commands::Statusline => {
            std::process::exit(statusline::run());
        }
        Commands::Hook => {
            std::process::exit(cmd::hook::run());
        }
        Commands::ValidateLine => {
            std::process::exit(cmd::validate_line::run());
        }
    }
}

/// Exits with `result`'s code, or prints `<command>: <error>` and exits 1.
fn exit_with(command: &str, result: anyhow::Result<i32>) -> ! {
    match result {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("{command}: {err:#}");
            std::process::exit(1);
        }
    }
}

/// `af report` / `af replay`: one pipeline, one failure path, and the
/// command name taken from the mode rather than restated at each call site.
fn run_report(
    mode: Mode,
    format: Format,
    local_grid_zone: Option<String>,
    remote_region: Option<String>,
) {
    let state_dir = paths::state_dir();
    if let Err(err) = cmd::report::run(
        &state_dir,
        mode,
        format,
        local_grid_zone.as_deref(),
        remote_region.as_deref(),
    ) {
        eprintln!("{}: {err:#}", mode.name());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Each flush is one commit, and a commit rewrites whole pages twice
    /// (WAL, then checkpoint): the interval is what the disk wear follows.
    #[test]
    fn watch_flushes_once_a_minute_by_default() {
        let cli = Cli::try_parse_from(["af", "watch"]).expect("parse");
        let Commands::Watch {
            batch_interval_ms, ..
        } = cli.command
        else {
            panic!("not watch");
        };
        assert_eq!(batch_interval_ms, 60_000);
    }
}
