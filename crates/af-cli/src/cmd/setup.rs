use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use super::DEFAULT_LOGS_ENDPOINT;

const CLAUDE_HOOK: &str = include_str!("../../../../collectors/claude-code/af-hook.sh");
/// The Pi collector extension, installed into Pi's global extensions
/// directory, where Pi loads every `*.ts` file at startup.
#[cfg(feature = "experimental-pi")]
pub(crate) const PI_EXTENSION: &str = include_str!("../../../../collectors/pi/af-collector.ts");
#[cfg(feature = "experimental-pi")]
const PI_EXTENSION_FILE: &str = "agentic-footprint.ts";
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SessionEnd",
];

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    #[cfg_attr(
        feature = "experimental-opencode",
        arg(long, help = "Comma-separated agents: codex,claude-code,opencode")
    )]
    #[cfg_attr(
        not(feature = "experimental-opencode"),
        arg(long, help = "Comma-separated agents: codex,claude-code")
    )]
    agents: Option<String>,
    /// Project directory whose project-local agent settings should be configured.
    #[arg(long, default_value = ".")]
    project: PathBuf,
    /// Configure Claude Code in ~/.claude/settings.json for all projects.
    #[arg(long)]
    global: bool,
    /// OTLP HTTP/JSON logs endpoint used by Codex and Claude Code.
    #[arg(long, default_value = DEFAULT_LOGS_ENDPOINT)]
    endpoint: String,
    /// Inspect configuration without writing files.
    #[arg(long)]
    check: bool,
    /// Print the planned changes without writing files.
    #[arg(long)]
    dry_run: bool,
    /// Apply detected changes without interactive confirmation.
    #[arg(long, visible_alias = "non-interactive")]
    yes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Agent {
    Codex,
    ClaudeCode,
    #[cfg(feature = "experimental-opencode")]
    OpenCode,
    #[cfg(feature = "experimental-pi")]
    Pi,
}

impl Agent {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            #[cfg(feature = "experimental-opencode")]
            Self::OpenCode => "OpenCode",
            #[cfg(feature = "experimental-pi")]
            Self::Pi => "Pi",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            #[cfg(feature = "experimental-opencode")]
            Self::OpenCode => "opencode",
            #[cfg(feature = "experimental-pi")]
            Self::Pi => "pi",
        }
    }
}

#[derive(Debug)]
enum Plan {
    Ready {
        agent: Agent,
        detail: String,
    },
    Change(Change),
    Conflict {
        agent: Agent,
        detail: String,
    },
    #[cfg(feature = "experimental-opencode")]
    Guidance {
        agent: Agent,
        detail: String,
    },
    Missing {
        agent: Agent,
    },
}

#[derive(Debug)]
enum Change {
    Codex {
        path: PathBuf,
        contents: String,
    },
    ClaudeHook {
        path: PathBuf,
    },
    ClaudeSettings {
        path: PathBuf,
        contents: String,
    },
    #[cfg(feature = "experimental-pi")]
    PiExtension {
        path: PathBuf,
    },
}

impl Change {
    fn agent(&self) -> Agent {
        match self {
            Self::Codex { .. } => Agent::Codex,
            Self::ClaudeHook { .. } | Self::ClaudeSettings { .. } => Agent::ClaudeCode,
            #[cfg(feature = "experimental-pi")]
            Self::PiExtension { .. } => Agent::Pi,
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Codex { path, .. } => format!("configure native OTLP logs in {}", path.display()),
            Self::ClaudeHook { path } => {
                format!("install the Claude Code hook at {}", path.display())
            }
            Self::ClaudeSettings { path, .. } => {
                format!("merge hooks and OTLP environment into {}", path.display())
            }
            #[cfg(feature = "experimental-pi")]
            Self::PiExtension { path } => {
                format!("install the collector extension at {}", path.display())
            }
        }
    }
}

pub fn run(state_dir: &Path, args: Args) -> Result<()> {
    validate_endpoint(&args.endpoint)?;
    print_receiver_header(state_dir, &args.endpoint);
    if !prepare_receiver(state_dir, &args)? {
        return Ok(());
    }

    let project = args
        .project
        .canonicalize()
        .with_context(|| format!("resolve project directory {}", args.project.display()))?;
    let selected = selected_agents(args.agents.as_deref())?;
    let plans = inspect(state_dir, &project, args.global, &args.endpoint, &selected)?;

    print_agent_header(&project, args.global);
    print_plans(&plans);

    let conflicts = plans
        .iter()
        .filter(|plan| matches!(plan, Plan::Conflict { .. }))
        .count();
    let changes: Vec<&Change> = plans
        .iter()
        .filter_map(|plan| match plan {
            Plan::Change(change) => Some(change),
            _ => None,
        })
        .collect();

    if args.check {
        if conflicts > 0 || !changes.is_empty() {
            bail!("configuration needs attention");
        }
        println!("\nAll selected installed agents are configured.");
        return Ok(());
    }
    if args.dry_run || changes.is_empty() {
        if changes.is_empty() && conflicts == 0 {
            println!("\nNo changes needed.");
        }
        return if conflicts > 0 {
            Err(anyhow!("resolve the conflicts above before applying setup"))
        } else {
            Ok(())
        };
    }
    if conflicts > 0 {
        bail!("resolve the conflicts above before applying setup");
    }

    if !args.yes {
        if !io::stdin().is_terminal() {
            bail!("interactive setup requires a terminal; rerun with --yes");
        }
        print!("\nApply {} change(s)? [Y/n] ", changes.len());
        io::stdout().flush().context("flush setup prompt")?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .context("read setup answer")?;
        let answer = answer.trim().to_ascii_lowercase();
        if !answer.is_empty() && answer != "y" && answer != "yes" {
            println!("Setup cancelled.");
            return Ok(());
        }
    }

    for change in changes {
        apply_change(change)?;
        println!("  applied: {}", change.description());
    }
    println!("\nSetup complete. The resident receiver is running; restart agent processes.");
    Ok(())
}

fn prepare_receiver(state_dir: &Path, args: &Args) -> Result<bool> {
    if super::service::receiver_reachable(&args.endpoint).is_ok() {
        println!("  receiver: healthy at {}", args.endpoint);
        return Ok(true);
    }

    if args.check {
        return super::service::check_for_setup(state_dir, &args.endpoint)
            .context("resident receiver needs attention")
            .map(|()| true);
    }

    if args.dry_run {
        println!("  receiver: unavailable at {}", args.endpoint);
        print_receiver_plan(state_dir, &args.endpoint);
        println!("\nAgent configuration is not inspected until the receiver is healthy.");
        return Ok(false);
    }

    if !super::service::automatic_manager_available() {
        println!("  receiver: unavailable at {}", args.endpoint);
        println!(
            "\nAutomatic receiver installation is unavailable on this platform.\n{}",
            super::service::foreground_setup_instructions(state_dir, &args.endpoint)
        );
        return Ok(false);
    }

    if args.yes {
        super::service::ensure_for_setup(state_dir, &args.endpoint)
            .context("cannot configure agents until the resident receiver is running")?;
        return Ok(true);
    }

    if !io::stdin().is_terminal() {
        bail!("interactive setup requires a terminal; rerun with --yes");
    }

    println!("  receiver: unavailable at {}", args.endpoint);
    println!("\nAgentic Footprint needs a local receiver for telemetry and energy sampling.");
    println!("The recommended mode starts it automatically for your user account.");
    print!("\nInstall and run the background receiver? [Y/n] ");
    io::stdout().flush().context("flush receiver prompt")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("read receiver answer")?;
    if !is_affirmative(&answer) {
        println!("\nReceiver setup skipped; no agent configuration was inspected or changed.");
        println!(
            "{}",
            super::service::foreground_setup_instructions(state_dir, &args.endpoint)
        );
        return Ok(false);
    }

    super::service::ensure_for_setup(state_dir, &args.endpoint)
        .context("cannot configure agents until the resident receiver is running")?;
    Ok(true)
}

fn print_receiver_plan(state_dir: &Path, endpoint: &str) {
    if super::service::automatic_manager_available() {
        println!("  planned: install, start, and verify the user background receiver");
    } else {
        println!(
            "  required: {}",
            super::service::foreground_setup_instructions(state_dir, endpoint)
        );
    }
}

fn is_affirmative(answer: &str) -> bool {
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    )
}

fn print_receiver_header(state_dir: &Path, endpoint: &str) {
    println!("Agentic Footprint setup");
    println!("  state:    {}", state_dir.display());
    println!("  receiver: checking {endpoint}");
}

fn print_agent_header(project: &Path, global: bool) {
    println!(
        "  Claude:   {}",
        if global {
            "global user settings".to_string()
        } else {
            format!("project settings in {}", project.display())
        }
    );
    println!("\nDetected agents:");
}

fn print_plans(plans: &[Plan]) {
    for plan in plans {
        match plan {
            Plan::Ready { agent, detail } => println!("  ✓ {:<12} {detail}", agent.name()),
            Plan::Change(change) => {
                println!("  • {:<12} {}", change.agent().name(), change.description())
            }
            Plan::Conflict { agent, detail } => println!("  ! {:<12} {detail}", agent.name()),
            #[cfg(feature = "experimental-opencode")]
            Plan::Guidance { agent, detail } => println!("  → {:<12} {detail}", agent.name()),
            Plan::Missing { agent } => println!("  – {:<12} not installed", agent.name()),
        }
    }
}

fn selected_agents(raw: Option<&str>) -> Result<BTreeSet<Agent>> {
    let all = vec![Agent::Codex, Agent::ClaudeCode];
    #[cfg(feature = "experimental-opencode")]
    let all = {
        let mut all = all;
        all.push(Agent::OpenCode);
        all
    };
    #[cfg(feature = "experimental-pi")]
    let all = {
        let mut all = all;
        all.push(Agent::Pi);
        all
    };
    let Some(raw) = raw else {
        return Ok(all.into_iter().collect());
    };
    let mut selected = BTreeSet::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let agent = match value {
            "codex" => Agent::Codex,
            "claude" | "claude-code" => Agent::ClaudeCode,
            #[cfg(feature = "experimental-opencode")]
            "opencode" => Agent::OpenCode,
            #[cfg(feature = "experimental-pi")]
            "pi" => Agent::Pi,
            _ => bail!("unknown agent {value:?}; expected codex or claude-code"),
        };
        selected.insert(agent);
    }
    if selected.is_empty() {
        bail!("--agents must select at least one agent");
    }
    Ok(selected)
}

fn inspect(
    state_dir: &Path,
    project: &Path,
    global: bool,
    endpoint: &str,
    selected: &BTreeSet<Agent>,
) -> Result<Vec<Plan>> {
    let home = crate::paths::home_dir().context("HOME (or USERPROFILE) is not set")?;
    let mut plans = Vec::new();
    for agent in selected {
        let installed = command_exists(agent.command());
        if !installed {
            plans.push(Plan::Missing { agent: *agent });
            continue;
        }
        match agent {
            Agent::Codex => {
                for path in codex_config_paths(&home, project) {
                    plans.push(inspect_codex(&path, endpoint)?);
                }
            }
            Agent::ClaudeCode => {
                plans.extend(inspect_claude(state_dir, &home, project, global, endpoint)?)
            }
            #[cfg(feature = "experimental-opencode")]
            Agent::OpenCode => plans.push(Plan::Guidance {
                agent: Agent::OpenCode,
                detail: "detected; start its server and use `af collect opencode` per session"
                    .to_string(),
            }),
            #[cfg(feature = "experimental-pi")]
            Agent::Pi => plans.push(inspect_pi(&pi_extension_path(&home))),
        }
    }
    Ok(plans)
}

/// Pi's agent directory: `$PI_CODING_AGENT_DIR` (with a leading `~`
/// expanded, as Pi does), else `~/.pi/agent`. Mirrors `getAgentDir()` in
/// Pi's `config.js`. Takes the variable's value so tests need not touch the
/// process environment.
#[cfg(feature = "experimental-pi")]
fn pi_agent_dir(home: &Path, env_dir: Option<std::ffi::OsString>) -> PathBuf {
    match env_dir.filter(|value| !value.is_empty()) {
        Some(value) => {
            let value = PathBuf::from(value);
            match value.strip_prefix("~") {
                Ok(rest) => home.join(rest),
                Err(_) => value,
            }
        }
        None => home.join(".pi").join("agent"),
    }
}

/// Where `af setup` installs the Pi collector: Pi's global extensions
/// directory, so every Pi session in every project loads it.
#[cfg(feature = "experimental-pi")]
pub(crate) fn pi_extension_path(home: &Path) -> PathBuf {
    pi_agent_dir(home, env::var_os("PI_CODING_AGENT_DIR"))
        .join("extensions")
        .join(PI_EXTENSION_FILE)
}

/// Whether the installed extension is this build's collector. An older or
/// edited copy is replaced (after a backup) rather than reported as a
/// conflict: the file is af's own, under an af-owned name.
#[cfg(feature = "experimental-pi")]
fn inspect_pi(path: &Path) -> Plan {
    if fs::read_to_string(path).ok().as_deref() == Some(PI_EXTENSION) {
        Plan::Ready {
            agent: Agent::Pi,
            detail: format!("collector extension installed at {}", path.display()),
        }
    } else {
        Plan::Change(Change::PiExtension {
            path: path.to_path_buf(),
        })
    }
}

fn inspect_codex(path: &Path, endpoint: &str) -> Result<Plan> {
    let path = path.to_path_buf();
    let existing = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    match codex_otel_state(&existing, endpoint) {
        CodexOtel::Matches => Ok(Plan::Ready {
            agent: Agent::Codex,
            detail: format!("native OTLP logs configured in {}", path.display()),
        }),
        CodexOtel::Other => Ok(Plan::Conflict {
            agent: Agent::Codex,
            detail: format!(
                "{} already configures OTEL; merge the af endpoint manually",
                path.display()
            ),
        }),
        // Appending to a file Codex cannot parse would only hide the real
        // problem behind a second one.
        CodexOtel::Invalid(message) => Ok(Plan::Conflict {
            agent: Agent::Codex,
            detail: format!(
                "{} is not valid TOML ({message}); fix it before running setup",
                path.display()
            ),
        }),
        CodexOtel::Absent => {
            let contents = append_codex_otel(&existing, endpoint);
            Ok(Plan::Change(Change::Codex { path, contents }))
        }
    }
}

fn inspect_claude(
    state_dir: &Path,
    home: &Path,
    project: &Path,
    global: bool,
    endpoint: &str,
) -> Result<Vec<Plan>> {
    let collector = hook_collector();
    // jq is a dependency of the sh shim only; the built-in `af hook`
    // collector needs nothing on PATH.
    if collector == HookCollector::ShShim && !command_exists("jq") {
        return Ok(vec![Plan::Conflict {
            agent: Agent::ClaudeCode,
            detail: "jq is required by the Claude Code hook but was not found on PATH".to_string(),
        }]);
    }
    let [user_settings, project_settings, _] = claude_scopes(home, project);
    let settings = if global {
        user_settings
    } else {
        project_settings
    };
    let existing = match fs::read_to_string(&settings) {
        Ok(contents) => serde_json::from_str(&contents)
            .with_context(|| format!("parse Claude Code settings {}", settings.display()))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error).with_context(|| format!("read {}", settings.display())),
    };
    if let Some(current) = existing
        .get("env")
        .and_then(Value::as_object)
        .and_then(|env| env.get("OTEL_EXPORTER_OTLP_ENDPOINT"))
        .and_then(Value::as_str)
    {
        let desired = logs_base_endpoint(endpoint)?;
        if current != desired {
            return Ok(vec![Plan::Conflict {
                agent: Agent::ClaudeCode,
                detail: format!(
                    "{} exports OTLP to {current}; merge the af endpoint manually",
                    settings.display()
                ),
            }]);
        }
    }
    if let Some(detail) = claude_scope_override(home, project, global, endpoint)? {
        return Ok(vec![Plan::Conflict {
            agent: Agent::ClaudeCode,
            detail,
        }]);
    }
    let command = claude_hook_command(state_dir, collector)?;
    let desired = merge_claude_settings(existing.clone(), &command, endpoint)?;
    let mut plans = Vec::new();
    match collector {
        // The sh shim is a file installed from the embedded copy.
        HookCollector::ShShim => {
            let hook = sh_hook_path(state_dir);
            if fs::read_to_string(&hook).ok().as_deref() == Some(CLAUDE_HOOK) {
                plans.push(Plan::Ready {
                    agent: Agent::ClaudeCode,
                    detail: format!("collector hook installed at {}", hook.display()),
                });
            } else {
                plans.push(Plan::Change(Change::ClaudeHook { path: hook }));
            }
        }
        // The built-in subcommand ships inside af itself; nothing to install.
        HookCollector::BuiltIn => {}
    }
    if desired == existing {
        plans.push(Plan::Ready {
            agent: Agent::ClaudeCode,
            detail: format!(
                "project hooks and OTLP configured in {}",
                settings.display()
            ),
        });
    } else {
        plans.push(Plan::Change(Change::ClaudeSettings {
            path: settings,
            contents: serde_json::to_string_pretty(&desired)? + "\n",
        }));
    }
    Ok(plans)
}

/// Claude Code's settings scopes, lowest precedence first. A key set in a
/// later scope wins over the same key in an earlier one.
fn claude_scopes(home: &Path, project: &Path) -> [PathBuf; 3] {
    [
        claude_user_settings(home),
        project.join(".claude").join("settings.json"),
        project.join(".claude").join("settings.local.json"),
    ]
}

/// Claude Code's user-scope settings, read by every project.
pub(crate) fn claude_user_settings(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// Whether Claude Code settings set any OTEL exporter variable in their
/// `env`, the block [`merge_claude_settings`] writes.
pub(crate) fn claude_settings_set_otel(settings: &Value) -> bool {
    settings
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|env| env.keys().any(|key| key.starts_with("OTEL_")))
}

/// Finds a setting that would stop the configuration setup writes from
/// taking effect once Claude Code merges its scopes: telemetry disabled,
/// hooks disabled, or the logs exporter redirected — either from a scope
/// with higher precedence than the file setup writes, or by a
/// signal-specific `OTEL_EXPORTER_OTLP_LOGS_*` key (which overrides the
/// base keys setup writes) in that file itself.
fn claude_scope_override(
    home: &Path,
    project: &Path,
    global: bool,
    endpoint: &str,
) -> Result<Option<String>> {
    let base = logs_base_endpoint(endpoint)?;
    // (key, value setup needs, set by setup itself?)
    let expected: [(&str, &str, bool); 6] = [
        ("CLAUDE_CODE_ENABLE_TELEMETRY", "1", true),
        ("OTEL_LOGS_EXPORTER", "otlp", true),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json", true),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", base, true),
        ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json", false),
        ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", endpoint, false),
    ];
    let written = usize::from(!global);
    for (index, path) in claude_scopes(home, project)
        .iter()
        .enumerate()
        .skip(written)
    {
        let settings: Value = match fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(value) => value,
                Err(error) => {
                    return Ok(Some(format!(
                        "{} is not valid JSON ({error}); Claude Code ignores it",
                        path.display()
                    )))
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        if settings.get("disableAllHooks").and_then(Value::as_bool) == Some(true) {
            return Ok(Some(format!(
                "{} sets disableAllHooks; remove it so the af hook can run",
                path.display()
            )));
        }
        let Some(env) = settings.get("env").and_then(Value::as_object) else {
            continue;
        };
        for (key, wanted, written_by_setup) in expected {
            // In the file setup writes, its own keys are overwritten anyway.
            if index == written && written_by_setup {
                continue;
            }
            let Some(current) = env.get(key).and_then(Value::as_str) else {
                continue;
            };
            let satisfied = if key == "OTEL_LOGS_EXPORTER" {
                current.split(',').any(|exporter| exporter.trim() == wanted)
            } else {
                current == wanted
            };
            if !satisfied {
                return Ok(Some(format!(
                    "{} sets {key}={current}, which overrides the af configuration; \
                     remove it or set it to {wanted}",
                    path.display()
                )));
            }
        }
    }
    Ok(None)
}

/// Which Claude Code collector implementation this setup registers. The
/// two are behavioral equals (`af hook` is a port of the sh shim, pinned
/// by shared tests); this one decision carries everything that differs —
/// the jq prerequisite, whether a hook file is installed, and the command
/// string — so switching a platform (or one day defaulting everyone to
/// the built-in) is a change here, not a hunt through `inspect_claude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookCollector {
    /// The embedded `collectors/claude-code/af-hook.sh`, installed under
    /// the state dir. Needs `jq` on PATH.
    ShShim,
    /// The `af hook` subcommand built into this binary. No prerequisites,
    /// no file to install.
    BuiltIn,
}

fn hook_collector() -> HookCollector {
    if cfg!(unix) {
        HookCollector::ShShim
    } else {
        HookCollector::BuiltIn
    }
}

/// Where the sh shim is installed from the embedded copy.
fn sh_hook_path(state_dir: &Path) -> PathBuf {
    state_dir
        .join("integrations")
        .join("claude-code")
        .join("af-hook.sh")
}

/// The command string registered for every Claude Code hook event.
///
/// Both variants are bare executable paths, never `sh -c`/`cmd /c`
/// wrappers: Claude Code then spawns the hook as a direct child, so the
/// hook's parent PID is the Claude Code process itself — the property the
/// SessionStart bootstrap span's `pids` (and with it process-tree energy
/// attribution) depends on.
fn claude_hook_command(state_dir: &Path, collector: HookCollector) -> Result<String> {
    match collector {
        HookCollector::ShShim => Ok(sh_hook_path(state_dir).to_string_lossy().to_string()),
        HookCollector::BuiltIn => {
            let exe = env::current_exe().context("resolve the af executable path")?;
            // Always quoted: deterministic (the idempotence comparison in
            // merge_claude_settings depends on that), and safe for
            // installs under spaced paths like `C:\Program Files`.
            Ok(format!("\"{}\" hook", exe.to_string_lossy()))
        }
    }
}

/// What makes a PATH entry an executable for [`command_exists`]: the bare
/// name on unix, the launcher extensions Windows actually resolves on
/// Windows (a fixed list rather than full `PATHEXT` parsing — the agents
/// this setup detects all ship as one of these three).
#[cfg(unix)]
const EXECUTABLE_SUFFIXES: &[&str] = &[""];
#[cfg(windows)]
const EXECUTABLE_SUFFIXES: &[&str] = &[".exe", ".cmd", ".bat"];

pub(crate) fn command_exists(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    find_in_path_dirs(command, env::split_paths(&path), EXECUTABLE_SUFFIXES)
}

/// Pure core of [`command_exists`], parameterized so both platforms'
/// suffix rules are testable on any OS. Lazy over `dirs` so the probe
/// stops at the first hit without materializing the whole PATH.
fn find_in_path_dirs(
    command: &str,
    dirs: impl IntoIterator<Item = PathBuf>,
    suffixes: &[&str],
) -> bool {
    let names: Vec<String> = suffixes
        .iter()
        .map(|suffix| format!("{command}{suffix}"))
        .collect();
    dirs.into_iter()
        .any(|directory| names.iter().any(|name| directory.join(name).is_file()))
}

pub(crate) fn codex_config_path(home: &Path) -> PathBuf {
    if let Some(codex_home) = env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(codex_home).join("config.toml");
    }
    home.join(".codex").join("config.toml")
}

/// Every Codex config file sessions started from `project` may load: the
/// primary home (`$CODEX_HOME`, else `~/.codex`) and, when the project has
/// one, `<project>/.codex`. The latter is Codex's project config layer and
/// also what a per-project `CODEX_HOME` alias points at — a home `af setup`
/// cannot see from its own environment, and which otherwise silently never
/// exports anything.
fn codex_config_paths(home: &Path, project: &Path) -> Vec<PathBuf> {
    let primary = codex_config_path(home);
    let mut paths = vec![primary.clone()];
    let project_home = project.join(".codex");
    if project_home.is_dir() && !same_directory(primary.parent(), &project_home) {
        paths.push(project_home.join("config.toml"));
    }
    paths
}

fn same_directory(left: Option<&Path>, right: &Path) -> bool {
    let Some(left) = left else {
        return false;
    };
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// What a Codex `config.toml` says about OTEL, read the way Codex reads it:
/// parsed as TOML, so a header with a trailing comment, nested-table
/// spelling, or an endpoint that only appears in a comment are all judged
/// by their meaning rather than their text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodexOtel {
    Absent,
    Matches,
    Other,
    Invalid(String),
}

pub(crate) fn codex_otel_state(contents: &str, endpoint: &str) -> CodexOtel {
    let table = match contents.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => {
            let message = error.to_string();
            let first = message.lines().next().unwrap_or("parse error").trim();
            return CodexOtel::Invalid(first.to_string());
        }
    };
    let Some(otel) = table.get("otel") else {
        return CodexOtel::Absent;
    };
    let str_at = |path: &[&str]| -> Option<&str> {
        path.iter()
            .try_fold(otel, |value, key| value.get(key))
            .and_then(toml::Value::as_str)
    };
    let matches = otel.get("log_user_prompt").and_then(toml::Value::as_bool) == Some(false)
        && str_at(&["metrics_exporter"]) == Some("none")
        && str_at(&["trace_exporter"]) == Some("none")
        && str_at(&["exporter", "otlp-http", "endpoint"]) == Some(endpoint)
        && str_at(&["exporter", "otlp-http", "protocol"]) == Some("json");
    if matches {
        CodexOtel::Matches
    } else {
        CodexOtel::Other
    }
}

pub(crate) fn append_codex_otel(existing: &str, endpoint: &str) -> String {
    let mut contents = existing.to_string();
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    if !contents.is_empty() {
        contents.push('\n');
    }
    contents.push_str("# agentic-footprint: native Codex OTLP logs.\n");
    contents.push_str("[otel]\n");
    contents.push_str("environment = \"dev\"\n");
    contents.push_str("log_user_prompt = false\n");
    contents.push_str("metrics_exporter = \"none\"\n");
    contents.push_str("trace_exporter = \"none\"\n");
    contents.push_str(&format!(
        "exporter = {{ otlp-http = {{ endpoint = \"{endpoint}\", protocol = \"json\" }} }}\n"
    ));
    contents
}

fn merge_claude_settings(mut value: Value, command: &str, endpoint: &str) -> Result<Value> {
    let root = value
        .as_object_mut()
        .context("Claude Code settings root must be a JSON object")?;
    let env = object_entry(root, "env")?;
    env.insert(
        "CLAUDE_CODE_ENABLE_TELEMETRY".into(),
        Value::String("1".into()),
    );
    env.insert("OTEL_LOGS_EXPORTER".into(), Value::String("otlp".into()));
    env.insert("OTEL_METRICS_EXPORTER".into(), Value::String("none".into()));
    env.insert(
        "OTEL_EXPORTER_OTLP_PROTOCOL".into(),
        Value::String("http/json".into()),
    );
    env.insert(
        "OTEL_EXPORTER_OTLP_ENDPOINT".into(),
        Value::String(logs_base_endpoint(endpoint)?.to_string()),
    );
    env.insert(
        "OTEL_LOGS_EXPORT_INTERVAL".into(),
        Value::String("2000".into()),
    );

    let hooks = object_entry(root, "hooks")?;
    for event in CLAUDE_EVENTS {
        let entries = hooks
            .entry((*event).to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .with_context(|| format!("Claude Code hooks.{event} must be an array"))?;
        entries.retain(|entry| {
            hook_entry_command(entry).is_none_or(|configured| !is_managed_af_hook(configured))
        });
        let exists = entries
            .iter()
            .any(|entry| hook_entry_command(entry) == Some(command));
        if !exists {
            entries.push(json!({
                "hooks": [{
                    "type": "command",
                    "command": command
                }]
            }));
        }
    }
    Ok(value)
}

/// Whether a registered hook command is one of ours — any variant, any
/// platform — so re-running setup replaces it instead of accumulating
/// duplicates: the sh shim under the state dir or a checkout (forward or
/// backslash paths), or the built-in `af hook` registration.
pub(crate) fn is_managed_af_hook(command: &str) -> bool {
    let normalized = command.replace('\\', "/");
    normalized.ends_with("/integrations/claude-code/af-hook.sh")
        || normalized.ends_with("/collectors/claude-code/af-hook.sh")
        || is_af_hook_command(&normalized)
}

/// Matches `<path to af or af.exe> hook`, with or without quotes around
/// the executable path.
fn is_af_hook_command(normalized: &str) -> bool {
    let Some(executable) = normalized.trim().strip_suffix(" hook") else {
        return false;
    };
    let executable = executable.trim().trim_matches('"');
    let basename = executable.rsplit('/').next().unwrap_or(executable);
    basename == "af" || basename.eq_ignore_ascii_case("af.exe")
}

fn object_entry<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    root.entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("Claude Code settings {key} must be a JSON object"))
}

pub(crate) fn hook_entry_command(value: &Value) -> Option<&str> {
    value
        .get("hooks")?
        .as_array()?
        .iter()
        .find_map(|hook| hook.get("command")?.as_str())
}

pub(crate) fn logs_base_endpoint(endpoint: &str) -> Result<&str> {
    endpoint
        .strip_suffix("/v1/logs")
        .context("OTLP logs endpoint must end with /v1/logs")
}

fn validate_endpoint(endpoint: &str) -> Result<()> {
    if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
        bail!("OTLP logs endpoint must use http:// or https://");
    }
    if endpoint.contains(['\"', '\n', '\r']) {
        bail!("OTLP logs endpoint contains characters that cannot be written safely");
    }
    logs_base_endpoint(endpoint)?;
    Ok(())
}

fn apply_change(change: &Change) -> Result<()> {
    match change {
        Change::Codex { path, contents } | Change::ClaudeSettings { path, contents } => {
            write_config(path, contents.as_bytes())
        }
        Change::ClaudeHook { path } => {
            write_config(path, CLAUDE_HOOK.as_bytes())?;
            make_executable(path)
        }
        #[cfg(feature = "experimental-pi")]
        Change::PiExtension { path } => write_config(path, PI_EXTENSION.as_bytes()),
    }
}

fn write_config(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        let backup = backup_path(path);
        fs::copy(path, &backup)
            .with_context(|| format!("back up {} to {}", path.display(), backup.display()))?;
        println!("  backup: {}", backup.display());
    }
    super::write_atomic(path, bytes)
}

fn backup_path(path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(
        "{}.bak.{timestamp}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config")
    ))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("make hook executable {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_kind(plan: &Plan) -> &'static str {
        match plan {
            Plan::Ready { .. } => "ready",
            Plan::Change(_) => "change",
            Plan::Conflict { .. } => "conflict",
            #[cfg(feature = "experimental-opencode")]
            Plan::Guidance { .. } => "guidance",
            Plan::Missing { .. } => "missing",
        }
    }

    fn codex_plan_for(contents: &str) -> Plan {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, contents).unwrap();
        inspect_codex(&path, DEFAULT_LOGS_ENDPOINT).unwrap()
    }

    #[test]
    fn codex_otel_table_with_trailing_comment_is_not_appended_twice() {
        // A second `[otel]` table makes Codex refuse to load its config.
        let plan = codex_plan_for("[otel] # mine\nexporter = \"none\"\n");
        assert_eq!(plan_kind(&plan), "conflict");
    }

    #[test]
    fn codex_otel_written_as_nested_tables_is_recognized() {
        let plan = codex_plan_for(
            "[otel]\nenvironment=\"dev\"\nlog_user_prompt=false\nmetrics_exporter=\"none\"\n\
             trace_exporter=\"none\"\n\n[otel.exporter.otlp-http]\n\
             endpoint=\"http://127.0.0.1:4318/v1/logs\"\nprotocol=\"json\"\n",
        );
        assert_eq!(plan_kind(&plan), "ready");
    }

    #[test]
    fn codex_endpoint_only_in_a_comment_does_not_count_as_configured() {
        let plan = codex_plan_for(
            "# exporter = { otlp-http = { endpoint = \"http://127.0.0.1:4318/v1/logs\", protocol = \"json\" } }\n\
             # log_user_prompt = false\n# metrics_exporter = \"none\"\n# trace_exporter = \"none\"\n\
             [otel]\nexporter = \"none\"\n",
        );
        assert_eq!(plan_kind(&plan), "conflict");
    }

    #[test]
    fn invalid_codex_config_is_a_conflict_not_an_append() {
        let plan = codex_plan_for("model = \"unterminated\n");
        assert_eq!(plan_kind(&plan), "conflict");
    }

    #[test]
    fn appended_codex_otel_parses_as_toml_and_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let merged = append_codex_otel("model = \"gpt-test\"\n", DEFAULT_LOGS_ENDPOINT);
        merged.parse::<toml::Table>().unwrap();
        std::fs::write(&path, merged).unwrap();
        let plan = inspect_codex(&path, DEFAULT_LOGS_ENDPOINT).unwrap();
        assert_eq!(plan_kind(&plan), "ready");
    }

    #[test]
    fn project_codex_home_is_configured_alongside_the_user_home() {
        // `alias codex='CODEX_HOME="$PWD/.codex" codex'` (or Codex's own
        // project layer) loads `<project>/.codex/config.toml`.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        let paths = codex_config_paths(&home, &project);
        assert!(
            paths.contains(&project.join(".codex").join("config.toml")),
            "{paths:?}"
        );
    }

    #[test]
    fn project_codex_home_is_listed_once_when_it_is_also_the_primary() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        // `home` == project: `~/.codex` and `<project>/.codex` are one file.
        let paths = codex_config_paths(&project, &project);
        assert_eq!(paths.len(), 1, "{paths:?}");
    }

    struct ClaudeFixture {
        _dir: tempfile::TempDir,
        state: PathBuf,
        home: PathBuf,
        project: PathBuf,
    }

    fn claude_fixture() -> ClaudeFixture {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        ClaudeFixture {
            _dir: dir,
            state,
            home,
            project,
        }
    }

    fn claude_kinds(fixture: &ClaudeFixture, global: bool) -> Vec<&'static str> {
        inspect_claude(
            &fixture.state,
            &fixture.home,
            &fixture.project,
            global,
            DEFAULT_LOGS_ENDPOINT,
        )
        .unwrap()
        .iter()
        .map(plan_kind)
        .collect()
    }

    #[test]
    fn claude_local_settings_disabling_telemetry_is_a_conflict() {
        let fixture = claude_fixture();
        std::fs::write(
            fixture.project.join(".claude/settings.local.json"),
            r#"{"env": {"CLAUDE_CODE_ENABLE_TELEMETRY": "0"}}"#,
        )
        .unwrap();
        assert_eq!(claude_kinds(&fixture, true), ["conflict"]);
    }

    #[test]
    fn claude_project_logs_endpoint_override_is_a_conflict() {
        let fixture = claude_fixture();
        std::fs::write(
            fixture.project.join(".claude/settings.json"),
            r#"{"env": {"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": "http://collector:4318/v1/logs"}}"#,
        )
        .unwrap();
        assert_eq!(claude_kinds(&fixture, true), ["conflict"]);
    }

    #[test]
    fn claude_signal_specific_override_in_the_written_file_is_a_conflict() {
        let fixture = claude_fixture();
        std::fs::write(
            fixture.home.join(".claude/settings.json"),
            r#"{"env": {"OTEL_EXPORTER_OTLP_LOGS_PROTOCOL": "http/protobuf"}}"#,
        )
        .unwrap();
        assert_eq!(claude_kinds(&fixture, true), ["conflict"]);
    }

    #[test]
    fn claude_hooks_disabled_in_a_higher_scope_is_a_conflict() {
        let fixture = claude_fixture();
        std::fs::write(
            fixture.project.join(".claude/settings.local.json"),
            r#"{"disableAllHooks": true}"#,
        )
        .unwrap();
        assert_eq!(claude_kinds(&fixture, true), ["conflict"]);
    }

    #[test]
    fn claude_higher_scope_repeating_the_same_values_is_not_a_conflict() {
        let fixture = claude_fixture();
        std::fs::write(
            fixture.project.join(".claude/settings.json"),
            r#"{"env": {"CLAUDE_CODE_ENABLE_TELEMETRY": "1", "OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:4318"}}"#,
        )
        .unwrap();
        assert!(!claude_kinds(&fixture, true).contains(&"conflict"));
    }

    #[test]
    fn claude_user_scope_is_lower_precedence_than_project_setup() {
        // Project-level setup wins over the user file, so a user-level
        // override cannot hide it.
        let fixture = claude_fixture();
        std::fs::write(
            fixture.home.join(".claude/settings.json"),
            r#"{"env": {"OTEL_EXPORTER_OTLP_ENDPOINT": "http://elsewhere:4318"}}"#,
        )
        .unwrap();
        assert!(!claude_kinds(&fixture, false).contains(&"conflict"));
    }

    #[test]
    fn receiver_prompt_defaults_to_yes() {
        assert!(is_affirmative(""));
        assert!(is_affirmative("   \n"));
    }

    #[test]
    fn receiver_prompt_accepts_yes_variants() {
        assert!(is_affirmative("y"));
        assert!(is_affirmative("Y"));
        assert!(is_affirmative("yes"));
        assert!(is_affirmative(" YES "));
    }

    #[test]
    fn receiver_prompt_rejects_other_answers() {
        assert!(!is_affirmative("n"));
        assert!(!is_affirmative("no"));
        assert!(!is_affirmative("later"));
    }

    #[test]
    fn codex_append_preserves_existing_configuration() {
        let existing = "model = \"gpt-test\"\n";
        let merged = append_codex_otel(existing, DEFAULT_LOGS_ENDPOINT);
        assert!(merged.starts_with(existing));
        assert!(merged.contains("[otel]"));
        assert_eq!(
            codex_otel_state(&merged, DEFAULT_LOGS_ENDPOINT),
            CodexOtel::Matches
        );
    }

    #[test]
    fn existing_other_codex_exporter_is_a_conflict() {
        let existing = "[otel]\nexporter = \"none\"\n";
        assert_eq!(
            codex_otel_state(existing, DEFAULT_LOGS_ENDPOINT),
            CodexOtel::Other
        );
    }

    #[test]
    fn claude_merge_preserves_settings_and_is_idempotent() {
        let hook = "/tmp/af-hook.sh";
        let input = json!({"permissions": {"allow": ["Read"]}});
        let once = merge_claude_settings(input, hook, DEFAULT_LOGS_ENDPOINT).unwrap();
        let twice = merge_claude_settings(once.clone(), hook, DEFAULT_LOGS_ENDPOINT).unwrap();
        assert_eq!(once, twice);
        assert_eq!(once["permissions"]["allow"][0], "Read");
        assert_eq!(once["env"]["OTEL_METRICS_EXPORTER"], "none");
        for event in CLAUDE_EVENTS {
            assert_eq!(once["hooks"][event].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn claude_merge_replaces_an_old_af_hook_path() {
        let input = json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "/checkout/collectors/claude-code/af-hook.sh"
                    }]
                }]
            }
        });
        let merged = merge_claude_settings(
            input,
            "/state/integrations/claude-code/af-hook.sh",
            DEFAULT_LOGS_ENDPOINT,
        )
        .unwrap();
        let entries = merged["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            hook_entry_command(&entries[0]),
            Some("/state/integrations/claude-code/af-hook.sh")
        );
    }

    #[test]
    fn managed_hook_predicate_recognizes_every_variant() {
        // sh shim, forward and backslash paths.
        assert!(is_managed_af_hook(
            "/state/integrations/claude-code/af-hook.sh"
        ));
        assert!(is_managed_af_hook(
            "/checkout/collectors/claude-code/af-hook.sh"
        ));
        assert!(is_managed_af_hook(
            r"C:\state\integrations\claude-code\af-hook.sh"
        ));
        // The built-in `af hook` registration, quoted and not.
        assert!(is_managed_af_hook(r#""C:\Program Files\af\af.exe" hook"#));
        assert!(is_managed_af_hook(r"C:\bin\af.exe hook"));
        assert!(is_managed_af_hook("/usr/local/bin/af hook"));
        // Not ours.
        assert!(!is_managed_af_hook("/usr/local/bin/other-hook.sh"));
        assert!(!is_managed_af_hook("af watch"));
        assert!(!is_managed_af_hook(r"C:\bin\afx.exe hook"));
        assert!(!is_managed_af_hook("daf hook"));
    }

    #[test]
    fn path_probe_honors_platform_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex"), "").unwrap();
        std::fs::write(dir.path().join("claude.cmd"), "").unwrap();
        let dirs = || [dir.path().to_path_buf()];
        // Unix rule: bare name only.
        assert!(find_in_path_dirs("codex", dirs(), &[""]));
        assert!(!find_in_path_dirs("claude", dirs(), &[""]));
        // Windows rule: launcher extensions only.
        assert!(find_in_path_dirs(
            "claude",
            dirs(),
            &[".exe", ".cmd", ".bat"]
        ));
        assert!(!find_in_path_dirs(
            "codex",
            dirs(),
            &[".exe", ".cmd", ".bat"]
        ));
        assert!(!find_in_path_dirs("missing", dirs(), &[""]));
    }

    #[test]
    fn endpoint_requires_logs_route() {
        assert!(validate_endpoint(DEFAULT_LOGS_ENDPOINT).is_ok());
        assert!(validate_endpoint("http://127.0.0.1:4318").is_err());
        assert!(validate_endpoint("http://host/\"bad/v1/logs").is_err());
        assert_eq!(
            logs_base_endpoint(DEFAULT_LOGS_ENDPOINT).unwrap(),
            "http://127.0.0.1:4318"
        );
    }

    #[test]
    fn agent_selection_accepts_aliases() {
        let selected = selected_agents(Some("codex,claude")).unwrap();
        assert!(selected.contains(&Agent::Codex));
        assert!(selected.contains(&Agent::ClaudeCode));
    }

    #[cfg(feature = "experimental-opencode")]
    #[test]
    fn experimental_agent_selection_accepts_opencode() {
        let selected = selected_agents(Some("opencode")).unwrap();
        assert!(selected.contains(&Agent::OpenCode));
    }

    #[cfg(feature = "experimental-pi")]
    #[test]
    fn pi_is_selectable_and_its_agent_dir_follows_pi() {
        assert!(selected_agents(Some("pi")).unwrap().contains(&Agent::Pi));
        assert!(selected_agents(None).unwrap().contains(&Agent::Pi));
        let home = Path::new("/home/u");
        assert_eq!(pi_agent_dir(home, None), home.join(".pi").join("agent"));
        assert_eq!(
            pi_agent_dir(home, Some("".into())),
            home.join(".pi").join("agent")
        );
        assert_eq!(
            pi_agent_dir(home, Some("/opt/pi".into())),
            PathBuf::from("/opt/pi")
        );
        assert_eq!(
            pi_agent_dir(home, Some("~/custom/pi".into())),
            home.join("custom").join("pi")
        );
    }

    #[cfg(feature = "experimental-pi")]
    #[test]
    fn pi_extension_is_installed_then_ready_and_an_old_copy_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extensions").join(PI_EXTENSION_FILE);

        let plan = inspect_pi(&path);
        assert_eq!(plan_kind(&plan), "change");
        let Plan::Change(change) = plan else {
            unreachable!()
        };
        apply_change(&change).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PI_EXTENSION);
        assert_eq!(plan_kind(&inspect_pi(&path)), "ready");

        std::fs::write(&path, "// older collector\n").unwrap();
        let Plan::Change(change) = inspect_pi(&path) else {
            panic!("an old copy must be replaced")
        };
        apply_change(&change).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PI_EXTENSION);
        // The old copy is backed up under a name Pi does not load (*.ts only).
        let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".bak."))
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(!backups[0].ends_with(".ts"));
    }
}
