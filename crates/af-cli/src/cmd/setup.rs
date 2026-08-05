use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

const DEFAULT_LOGS_ENDPOINT: &str = "http://127.0.0.1:4318/v1/logs";
const CLAUDE_HOOK: &str = include_str!("../../../../collectors/claude-code/af-hook.sh");
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
}

impl Agent {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            #[cfg(feature = "experimental-opencode")]
            Self::OpenCode => "OpenCode",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            #[cfg(feature = "experimental-opencode")]
            Self::OpenCode => "opencode",
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
    Codex { path: PathBuf, contents: String },
    ClaudeHook { path: PathBuf },
    ClaudeSettings { path: PathBuf, contents: String },
}

impl Change {
    fn agent(&self) -> Agent {
        match self {
            Self::Codex { .. } => Agent::Codex,
            Self::ClaudeHook { .. } | Self::ClaudeSettings { .. } => Agent::ClaudeCode,
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
        }
    }
}

pub fn run(state_dir: &Path, args: Args) -> Result<()> {
    validate_endpoint(&args.endpoint)?;
    let project = args
        .project
        .canonicalize()
        .with_context(|| format!("resolve project directory {}", args.project.display()))?;
    let selected = selected_agents(args.agents.as_deref())?;
    let plans = inspect(state_dir, &project, args.global, &args.endpoint, &selected)?;

    if args.check {
        super::service::check_for_setup(state_dir, &args.endpoint)
            .context("resident receiver needs attention")?;
    } else if !args.dry_run {
        super::service::ensure_for_setup(state_dir, &args.endpoint)
            .context("cannot configure agents until the resident receiver is running")?;
    }

    print_header(state_dir, &project, args.global, &args.endpoint);
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

fn print_header(state_dir: &Path, project: &Path, global: bool, endpoint: &str) {
    println!("Agentic Footprint setup");
    println!("  state:    {}", state_dir.display());
    println!(
        "  Claude:   {}",
        if global {
            "global user settings".to_string()
        } else {
            format!("project settings in {}", project.display())
        }
    );
    println!("  receiver: {endpoint}");
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
    let mut plans = Vec::new();
    for agent in selected {
        let installed = command_exists(agent.command());
        if !installed {
            plans.push(Plan::Missing { agent: *agent });
            continue;
        }
        match agent {
            Agent::Codex => plans.push(inspect_codex(endpoint)?),
            Agent::ClaudeCode => {
                plans.extend(inspect_claude(state_dir, project, global, endpoint)?)
            }
            #[cfg(feature = "experimental-opencode")]
            Agent::OpenCode => plans.push(Plan::Guidance {
                agent: Agent::OpenCode,
                detail: "detected; start its server and use `af collect opencode` per session"
                    .to_string(),
            }),
        }
    }
    Ok(plans)
}

fn inspect_codex(endpoint: &str) -> Result<Plan> {
    let path = codex_config_path()?;
    let existing = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if has_codex_otel(&existing) {
        if codex_otel_matches(&existing, endpoint) {
            return Ok(Plan::Ready {
                agent: Agent::Codex,
                detail: format!("native OTLP logs configured in {}", path.display()),
            });
        }
        return Ok(Plan::Conflict {
            agent: Agent::Codex,
            detail: format!(
                "{} already configures OTEL; merge the af endpoint manually",
                path.display()
            ),
        });
    }
    let contents = append_codex_otel(&existing, endpoint);
    Ok(Plan::Change(Change::Codex { path, contents }))
}

fn inspect_claude(
    state_dir: &Path,
    project: &Path,
    global: bool,
    endpoint: &str,
) -> Result<Vec<Plan>> {
    if !command_exists("jq") {
        return Ok(vec![Plan::Conflict {
            agent: Agent::ClaudeCode,
            detail: "jq is required by the Claude Code hook but was not found on PATH".to_string(),
        }]);
    }
    let hook = state_dir
        .join("integrations")
        .join("claude-code")
        .join("af-hook.sh");
    let settings = if global {
        let home = env::var_os("HOME").context("HOME is not set")?;
        PathBuf::from(home).join(".claude").join("settings.json")
    } else {
        project.join(".claude").join("settings.json")
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
    let desired = merge_claude_settings(existing.clone(), &hook, endpoint)?;
    let mut plans = Vec::new();
    if fs::read_to_string(&hook).ok().as_deref() == Some(CLAUDE_HOOK) {
        plans.push(Plan::Ready {
            agent: Agent::ClaudeCode,
            detail: format!("collector hook installed at {}", hook.display()),
        });
    } else {
        plans.push(Plan::Change(Change::ClaudeHook { path: hook }));
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

fn command_exists(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|directory| directory.join(command).is_file())
}

fn codex_config_path() -> Result<PathBuf> {
    if let Some(home) = env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join("config.toml"));
    }
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".codex").join("config.toml"))
}

fn has_codex_otel(contents: &str) -> bool {
    contents.lines().map(str::trim).any(|line| {
        line == "[otel]"
            || line.starts_with("[otel.")
            || line.starts_with("otel.")
            || line.starts_with("otel =")
            || line.starts_with("otel=")
    })
}

fn codex_otel_matches(contents: &str, endpoint: &str) -> bool {
    contents.contains(&format!("endpoint = \"{endpoint}\""))
        && contents.contains("protocol = \"json\"")
        && contents.contains("log_user_prompt = false")
        && contents.contains("metrics_exporter = \"none\"")
        && contents.contains("trace_exporter = \"none\"")
}

fn append_codex_otel(existing: &str, endpoint: &str) -> String {
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

fn merge_claude_settings(mut value: Value, hook: &Path, endpoint: &str) -> Result<Value> {
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
    let command = hook.to_string_lossy().to_string();
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
            .any(|entry| hook_entry_command(entry) == Some(command.as_str()));
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

fn is_managed_af_hook(command: &str) -> bool {
    command.ends_with("/integrations/claude-code/af-hook.sh")
        || command.ends_with("/collectors/claude-code/af-hook.sh")
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

fn hook_entry_command(value: &Value) -> Option<&str> {
    value
        .get("hooks")?
        .as_array()?
        .iter()
        .find_map(|hook| hook.get("command")?.as_str())
}

fn logs_base_endpoint(endpoint: &str) -> Result<&str> {
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
    }
}

fn write_config(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create configuration directory {}", parent.display()))?;
    if path.exists() {
        let backup = backup_path(path);
        fs::copy(path, &backup)
            .with_context(|| format!("back up {} to {}", path.display(), backup.display()))?;
        println!("  backup: {}", backup.display());
    }
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config"),
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create temporary file {}", temporary.display()))?;
        secure_permissions(&temporary, path)?;
        file.write_all(bytes)
            .with_context(|| format!("write temporary file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary file {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("replace configuration {}", path.display()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync configuration directory {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn secure_permissions(temporary: &Path, existing: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(existing)
        .map(|metadata| metadata.permissions().mode())
        .unwrap_or(0o600);
    fs::set_permissions(temporary, fs::Permissions::from_mode(mode & 0o777))
        .with_context(|| format!("set configuration permissions {}", temporary.display()))
}

#[cfg(not(unix))]
fn secure_permissions(_temporary: &Path, _existing: &Path) -> Result<()> {
    Ok(())
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

    #[test]
    fn codex_append_preserves_existing_configuration() {
        let existing = "model = \"gpt-test\"\n";
        let merged = append_codex_otel(existing, DEFAULT_LOGS_ENDPOINT);
        assert!(merged.starts_with(existing));
        assert!(merged.contains("[otel]"));
        assert!(codex_otel_matches(&merged, DEFAULT_LOGS_ENDPOINT));
    }

    #[test]
    fn existing_other_codex_exporter_is_a_conflict() {
        let existing = "[otel]\nexporter = \"none\"\n";
        assert!(has_codex_otel(existing));
        assert!(!codex_otel_matches(existing, DEFAULT_LOGS_ENDPOINT));
    }

    #[test]
    fn claude_merge_preserves_settings_and_is_idempotent() {
        let hook = Path::new("/tmp/af-hook.sh");
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
            Path::new("/state/integrations/claude-code/af-hook.sh"),
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
}
