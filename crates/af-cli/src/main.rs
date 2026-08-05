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
        /// `POST /v1/metrics`). A port that is already in use is reported
        /// and the watch continues without a receiver.
        #[arg(long, default_value = cmd::watch::DEFAULT_OTLP_ADDR, conflicts_with = "no_otlp")]
        otlp_addr: String,
        /// Do not start the OTLP receiver at all. Mutually exclusive with
        /// `--otlp-addr`: asking for an address and asking for no server is
        /// a contradiction worth an error rather than a silent winner.
        #[arg(long)]
        no_otlp: bool,
        /// Address for the `/debug` HTTP+SSE console API. Only bound with
        /// `--debug`.
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
    },
    /// Python runtime setup and diagnostics
    Python {
        #[command(subcommand)]
        action: PythonAction,
    },
    /// Read Claude Code's status JSON on stdin and print one line of
    /// session impacts: `<gwp_kg> <water_L> <energy_kWh> <adpe_kg> <pe_MJ>`
    /// (range means). Read-only and never fails: no ingest, no estimation,
    /// no writes, zeros when the session has no stored join yet.
    Statusline,
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
            if let Err(err) = cmd::setup::run(&state_dir, args) {
                eprintln!("af setup: {err:#}");
                std::process::exit(1);
            }
        }
        Commands::Service(args) => {
            let state_dir = paths::state_dir();
            if let Err(err) = cmd::service::run(&state_dir, args) {
                eprintln!("af service: {err:#}");
                std::process::exit(1);
            }
        }
        #[cfg(feature = "experimental-opencode")]
        Commands::Collect { collector } => {
            let state_dir = paths::state_dir();
            let result = match collector {
                CollectAction::Opencode(args) => cmd::opencode::run(&state_dir, args),
            };
            if let Err(err) = result {
                eprintln!("af collect opencode: {err:#}");
                std::process::exit(1);
            }
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
            debug_addr,
            interval,
            local_grid_zone,
            remote_region,
        } => {
            let state_dir = paths::state_dir();
            let args = cmd::watch::WatchArgs {
                debug,
                no_sidecars,
                no_otlp,
                otlp_addr,
                debug_addr,
                interval,
                local_grid_zone,
                remote_region,
            };
            if let Err(err) = cmd::watch::run(&state_dir, args) {
                eprintln!("af watch: {err:#}");
                std::process::exit(1);
            }
        }
        Commands::Python { action } => {
            let state_dir = paths::state_dir();
            match action {
                PythonAction::Setup => {
                    if let Err(err) = cmd::python::run_setup(&state_dir) {
                        eprintln!("af python setup: {err:#}");
                        std::process::exit(1);
                    }
                }
                PythonAction::Doctor => {
                    let code = cmd::python::run_doctor(&state_dir);
                    std::process::exit(code);
                }
            }
        }
        Commands::Statusline => {
            std::process::exit(statusline::run());
        }
        Commands::ValidateLine => {
            std::process::exit(cmd::validate_line::run());
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
