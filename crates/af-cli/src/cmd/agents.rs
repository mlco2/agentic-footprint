//! What this machine can say about each supported coding agent before and
//! after it sends anything: is its command installed, does its user-level
//! configuration export to this receiver, is a running process older than
//! that configuration.
//!
//! Shared by `af doctor` (which prints the stale-process finding), and by
//! `af watch`, which turns the same facts into the `agents` rows of
//! `GET /debug/status`. Only **user-scope** files are read here
//! (`~/.claude/settings.json`, the user Codex `config.toml`): a resident
//! watcher has no project directory to inspect, so a project-only setup is
//! reported as not configured until its first event arrives.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::setup::{self, CodexOtel};
use super::DEFAULT_LOGS_ENDPOINT;

/// A running agent process, as far as `ps` can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentProcess {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) started: SystemTime,
}

/// A configuration file an agent reads once at process start.
pub(crate) struct StartupConfig {
    pub(crate) agent: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) modified: SystemTime,
}

/// The per-user files whose telemetry settings each agent reads once, at
/// start. Only files that carry OTEL configuration count, judged the way
/// `af setup` reads them: an unrelated edit to a file setup never touched
/// says nothing about telemetry.
pub(crate) fn startup_configs(home: &Path) -> Vec<StartupConfig> {
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

pub(crate) fn stale_processes<'a>(
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
pub(crate) fn running_agents() -> Option<Vec<AgentProcess>> {
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

/// What could be learned about one running agent process without its
/// cooperation. Every field is `None` when it could not be read: a missing
/// fact is never turned into a verdict.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Inspection {
    /// The process's working directory: the project it was started in.
    pub(crate) cwd: Option<PathBuf>,
    /// The Codex home the process really uses, whatever alias or variable
    /// chose it: the directory of the state files it holds open.
    pub(crate) codex_home: Option<PathBuf>,
    /// The Codex session the process is writing, from its open rollout file.
    pub(crate) codex_session: Option<String>,
}

/// `lsof -F pfn` output: a `p<pid>` line opens a process, then pairs of
/// `f<descriptor>` and `n<name>` lines list its open files.
fn parse_lsof(output: &str) -> BTreeMap<u32, Vec<(String, PathBuf)>> {
    let mut by_pid: BTreeMap<u32, Vec<(String, PathBuf)>> = BTreeMap::new();
    let mut pid = None;
    let mut descriptor = "";
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
            if let Some(pid) = pid {
                by_pid.entry(pid).or_default();
            }
        } else if let Some(value) = line.strip_prefix('f') {
            descriptor = value;
        } else if let (Some(name), Some(pid)) = (line.strip_prefix('n'), pid) {
            by_pid
                .entry(pid)
                .or_default()
                .push((descriptor.to_string(), PathBuf::from(name)));
        }
    }
    by_pid
}

/// Reads an [`Inspection`] out of a process's open files (descriptor,
/// path). The working directory is the `cwd` descriptor. A Codex home is
/// recognised by what Codex keeps open inside it: its rollout file under
/// `sessions/`, or its `state_*.sqlite` database.
fn inspection_from(files: &[(String, PathBuf)]) -> Inspection {
    let mut inspection = Inspection {
        cwd: files
            .iter()
            .find(|(descriptor, _)| descriptor == "cwd")
            .map(|(_, path)| path.clone()),
        ..Inspection::default()
    };
    for (_, path) in files {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(stem) = name
            .strip_prefix("rollout-")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
        {
            let home = path
                .ancestors()
                .find(|dir| dir.file_name().is_some_and(|name| name == "sessions"))
                .and_then(Path::parent);
            if let Some(home) = home {
                // The rollout names the session being written: it wins
                // over a database that only names the home.
                inspection.codex_home = Some(home.to_path_buf());
                inspection.codex_session = trailing_uuid(stem);
            }
        } else if name.starts_with("state_")
            && name.ends_with(".sqlite")
            && inspection.codex_home.is_none()
        {
            inspection.codex_home = path.parent().map(Path::to_path_buf);
        }
    }
    inspection
}

/// The UUID a rollout file name ends with (`…T12-29-52-<uuid>`).
fn trailing_uuid(stem: &str) -> Option<String> {
    let start = stem.len().checked_sub(36)?;
    let candidate = stem.get(start..)?;
    let shaped = candidate.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    });
    shaped.then(|| candidate.to_string())
}

/// Inspects running processes of this user. Best effort by design: a tool
/// that is missing, a process that is gone or one that may not be looked
/// at simply yields no facts for it.
pub(crate) fn inspect_processes(pids: &[u32]) -> BTreeMap<u32, Inspection> {
    if pids.is_empty() {
        return BTreeMap::new();
    }
    inspect_platform(pids)
}

/// macOS: one `lsof` for every process. Its exit status is not consulted:
/// it is non-zero as soon as one listed pid is gone, with the others still
/// printed.
#[cfg(target_os = "macos")]
fn inspect_platform(pids: &[u32]) -> BTreeMap<u32, Inspection> {
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let Ok(output) = Command::new("lsof")
        .args(["-a", "-p", &list, "-F", "pfn"])
        .output()
    else {
        return BTreeMap::new();
    };
    parse_lsof(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .map(|(pid, files)| (pid, inspection_from(&files)))
        .collect()
}

/// Linux: `/proc/<pid>`, readable for the user's own processes.
#[cfg(target_os = "linux")]
fn inspect_platform(pids: &[u32]) -> BTreeMap<u32, Inspection> {
    pids.iter()
        .map(|&pid| {
            let root = PathBuf::from(format!("/proc/{pid}"));
            let mut files: Vec<(String, PathBuf)> = std::fs::read_link(root.join("cwd"))
                .map(|cwd| ("cwd".to_string(), cwd))
                .into_iter()
                .collect();
            if let Ok(entries) = std::fs::read_dir(root.join("fd")) {
                files.extend(entries.flatten().filter_map(|entry| {
                    let target = std::fs::read_link(entry.path()).ok()?;
                    Some((entry.file_name().to_string_lossy().into_owned(), target))
                }));
            }
            let mut inspection = inspection_from(&files);
            if inspection.codex_home.is_none() {
                // Linux lets a user read their own processes' environment.
                inspection.codex_home =
                    std::fs::read(root.join("environ"))
                        .ok()
                        .and_then(|environ| {
                            environ
                                .split(|byte| *byte == 0)
                                .find_map(|entry| entry.strip_prefix(b"CODEX_HOME="))
                                .filter(|value| !value.is_empty())
                                .map(|value| {
                                    PathBuf::from(String::from_utf8_lossy(value).into_owned())
                                })
                        });
            }
            (pid, inspection)
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn inspect_platform(_pids: &[u32]) -> BTreeMap<u32, Inspection> {
    BTreeMap::new()
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

/// A coding agent `af` knows how to observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgentId {
    ClaudeCode,
    Codex,
    #[cfg(feature = "experimental-opencode")]
    OpenCode,
}

impl AgentId {
    pub(crate) fn all() -> Vec<AgentId> {
        vec![
            AgentId::ClaudeCode,
            AgentId::Codex,
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode,
        ]
    }

    /// Stable identifier used on the wire.
    pub(crate) fn id(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "claude-code",
            AgentId::Codex => "codex",
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode => "opencode",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "Claude Code",
            AgentId::Codex => "Codex",
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode => "OpenCode",
        }
    }

    /// The executable name, as on `PATH` and in `ps`.
    pub(crate) fn command(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "claude",
            AgentId::Codex => "codex",
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode => "opencode",
        }
    }
}

/// Which agent a collector's events belong to. Decided by collector name
/// because that is known the moment an event is received, on both paths
/// (the OTLP normalizer's collector, the spool file's name); the session's
/// `agent_app` only arrives with the hook's `session_meta`, a flush later,
/// and never for an OTLP-only session.
pub(crate) fn agent_for_collector(collector: &str) -> Option<AgentId> {
    match collector {
        // `cc-hooks` is the hook collector's name and its spool file prefix;
        // `otlp-cc` is the OTLP normalizer's.
        "claude-code" | super::hook::COLLECTOR_NAME | "otlp-cc" => Some(AgentId::ClaudeCode),
        "codex" => Some(AgentId::Codex),
        #[cfg(feature = "experimental-opencode")]
        super::opencode::COLLECTOR_NAME => Some(AgentId::OpenCode),
        other if other.starts_with("otlp-codex") => Some(AgentId::Codex),
        _ => None,
    }
}

/// What an agent's user-scope configuration says about exporting to one
/// receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserConfig {
    /// No file, or a file that carries no telemetry configuration at all.
    Absent,
    Configured {
        path: PathBuf,
    },
    /// Telemetry configuration is present but will not reach this receiver.
    Misconfigured {
        path: PathBuf,
        detail: String,
    },
}

/// `logs_endpoint` is the full URL, e.g. `http://127.0.0.1:4318/v1/logs`.
pub(crate) fn inspect_user_config(agent: AgentId, home: &Path, logs_endpoint: &str) -> UserConfig {
    match agent {
        AgentId::ClaudeCode => {
            inspect_claude_file(&setup::claude_user_settings(home), logs_endpoint)
        }
        AgentId::Codex => inspect_codex_file(&setup::codex_config_path(home), logs_endpoint),
        // No config file and no OTLP path: collected per session.
        #[cfg(feature = "experimental-opencode")]
        AgentId::OpenCode => UserConfig::Absent,
    }
}

/// Reads a config file; `Ok(None)` when it does not exist.
fn read_config(path: &Path) -> Result<Option<String>, UserConfig> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(UserConfig::Misconfigured {
            path: path.to_path_buf(),
            detail: format!("cannot be read: {error}"),
        }),
    }
}

fn inspect_claude_file(path: &Path, logs_endpoint: &str) -> UserConfig {
    claude_config_from(&[path.to_path_buf()], logs_endpoint).0
}

/// The Claude Code configuration a process started in `project` loads: the
/// user scope, then the project's two scopes on top (`None`: a process
/// whose directory is unknown, or no process at all — the user scope
/// alone). Second value: when that configuration last changed, to tell
/// which processes started before it.
pub(crate) fn claude_config_for(
    home: &Path,
    project: Option<&Path>,
    logs_endpoint: &str,
) -> (UserConfig, Option<SystemTime>) {
    let mut scopes = vec![setup::claude_user_settings(home)];
    if let Some(project) = project {
        for name in ["settings.json", "settings.local.json"] {
            let path = project.join(".claude").join(name);
            // A process started in the home directory: one file, not two.
            if !scopes.contains(&path) {
                scopes.push(path);
            }
        }
    }
    claude_config_from(&scopes, logs_endpoint)
}

/// Judges Claude Code settings files layered lowest precedence first: an
/// `env` key set in a later file wins, hooks add up.
fn claude_config_from(scopes: &[PathBuf], logs_endpoint: &str) -> (UserConfig, Option<SystemTime>) {
    let misconfigured = |path: &Path, detail: String| UserConfig::Misconfigured {
        path: path.to_path_buf(),
        detail,
    };
    // Each variable with the file that has the last word on it.
    let mut env: BTreeMap<String, (String, PathBuf)> = BTreeMap::new();
    let mut has_af_hook = false;
    let mut hooks_disabled: Option<PathBuf> = None;
    // Only files that take part in telemetry age the configuration: an
    // edit to a project's permissions does not make its processes stale.
    let mut modified: Option<SystemTime> = None;
    for path in scopes {
        let contents = match read_config(path) {
            Ok(Some(contents)) => contents,
            Ok(None) => continue,
            Err(unreadable) => return (unreadable, modified),
        };
        let settings: Value = match serde_json::from_str(&contents) {
            Ok(settings) => settings,
            Err(error) => {
                return (
                    misconfigured(path, format!("not valid JSON: {error}")),
                    modified,
                )
            }
        };
        let mut takes_part = false;
        if settings.get("disableAllHooks").and_then(Value::as_bool) == Some(true) {
            hooks_disabled = Some(path.clone());
            takes_part = true;
        }
        for (key, value) in settings
            .get("env")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if key.starts_with("OTEL_") || key == "CLAUDE_CODE_ENABLE_TELEMETRY" {
                let value = value.as_str().unwrap_or_default().to_string();
                env.insert(key.clone(), (value, path.clone()));
                takes_part = true;
            }
        }
        let hook_here = settings
            .get("hooks")
            .and_then(Value::as_object)
            .is_some_and(|hooks| {
                hooks
                    .values()
                    .filter_map(Value::as_array)
                    .flatten()
                    .filter_map(setup::hook_entry_command)
                    .any(setup::is_managed_af_hook)
            });
        has_af_hook |= hook_here;
        if takes_part || hook_here {
            let changed = std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok();
            modified = modified.max(changed);
        }
    }

    // The file a reader should open: the last one to set an OTEL variable.
    let Some(otel_path) = scopes
        .iter()
        .rev()
        .find(|path| {
            env.iter()
                .any(|(key, (_, from))| key.starts_with("OTEL_") && from == *path)
        })
        .cloned()
    else {
        return (UserConfig::Absent, modified);
    };
    // What `af setup` writes: the base endpoint, without `/v1/logs`.
    let expected = setup::logs_base_endpoint(logs_endpoint).unwrap_or(logs_endpoint);
    let config = match env.get("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Some((exported, from)) if exported != expected => misconfigured(
            from,
            format!("exports telemetry to {exported}, not to {expected}"),
        ),
        None => misconfigured(
            &otel_path,
            format!("sets OTEL variables but no endpoint; expected {expected}"),
        ),
        Some(_) => match (env.get("CLAUDE_CODE_ENABLE_TELEMETRY"), hooks_disabled) {
            (Some((value, from)), _) if value != "1" => misconfigured(
                from,
                format!(
                    "sets CLAUDE_CODE_ENABLE_TELEMETRY to {value}, which switches telemetry off"
                ),
            ),
            (_, Some(from)) => misconfigured(
                &from,
                "sets disableAllHooks, so the af hook cannot run".to_string(),
            ),
            _ if !has_af_hook => misconfigured(
                &otel_path,
                "exports telemetry but the af hook is not registered".to_string(),
            ),
            _ => UserConfig::Configured { path: otel_path },
        },
    };
    (config, modified)
}

/// The Codex configuration of one Codex home (`<home>/config.toml`), and
/// when it last changed.
pub(crate) fn codex_config_for(
    codex_home: &Path,
    logs_endpoint: &str,
) -> (UserConfig, Option<SystemTime>) {
    let path = codex_home.join("config.toml");
    let modified = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok();
    (inspect_codex_file(&path, logs_endpoint), modified)
}

fn inspect_codex_file(path: &Path, logs_endpoint: &str) -> UserConfig {
    let contents = match read_config(path) {
        Ok(Some(contents)) => contents,
        Ok(None) => return UserConfig::Absent,
        Err(unreadable) => return unreadable,
    };
    let misconfigured = |detail: String| UserConfig::Misconfigured {
        path: path.to_path_buf(),
        detail,
    };
    match setup::codex_otel_state(&contents, logs_endpoint) {
        CodexOtel::Absent => UserConfig::Absent,
        CodexOtel::Matches => UserConfig::Configured {
            path: path.to_path_buf(),
        },
        CodexOtel::Other => misconfigured(format!(
            "its [otel] section does not export JSON logs to {logs_endpoint}"
        )),
        CodexOtel::Invalid(message) => misconfigured(format!("not valid TOML: {message}")),
    }
}

/// One running process of an agent, judged against its configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessFact {
    pub(crate) pid: u32,
    pub(crate) started: SystemTime,
    /// Started before the configuration last changed: it is still running
    /// with whatever the file said before.
    pub(crate) stale: bool,
}

/// Everything detection knows about one agent, before any event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentFacts {
    pub(crate) agent: AgentId,
    pub(crate) installed: bool,
    pub(crate) config: UserConfig,
    /// `None` when processes could not be listed on this platform.
    pub(crate) processes: Option<Vec<ProcessFact>>,
}

/// Assembles one agent's facts from already-gathered inputs. `processes`
/// is every agent process found; this keeps the agent's own.
pub(crate) fn facts_from(
    agent: AgentId,
    installed: bool,
    config: UserConfig,
    config_modified: Option<SystemTime>,
    processes: Option<&[AgentProcess]>,
) -> AgentFacts {
    let processes = processes.map(|all| {
        all.iter()
            .filter(|process| process.name == agent.command())
            .map(|process| ProcessFact {
                pid: process.pid,
                started: process.started,
                stale: config_modified.is_some_and(|modified| process.started < modified),
            })
            .collect()
    });
    AgentFacts {
        agent,
        installed,
        config,
        processes,
    }
}

/// Detects every supported agent. Reads the user-scope config files,
/// `PATH`, and one `ps`; writes nothing.
pub(crate) fn detect(home: &Path, logs_endpoint: &str) -> Vec<AgentFacts> {
    let processes = running_agents();
    AgentId::all()
        .into_iter()
        .map(|agent| {
            let config = inspect_user_config(agent, home, logs_endpoint);
            let modified = match &config {
                UserConfig::Absent => None,
                UserConfig::Configured { path } | UserConfig::Misconfigured { path, .. } => {
                    std::fs::metadata(path)
                        .and_then(|meta| meta.modified())
                        .ok()
                }
            };
            facts_from(
                agent,
                setup::command_exists(agent.command()),
                config,
                modified,
                processes.as_deref(),
            )
        })
        .collect()
}

/// `ps` reports elapsed time in whole seconds, so the start time derived
/// from it moves by up to a second between two listings of the same
/// process. Carries the earlier estimate over, so that detecting twice
/// with nothing changed yields equal facts.
pub(crate) fn keep_start_times(new: &mut [AgentFacts], old: &[AgentFacts]) {
    const JITTER: Duration = Duration::from_secs(2);
    for facts in new {
        let Some(before) = old
            .iter()
            .find(|old| old.agent == facts.agent)
            .and_then(|old| old.processes.as_deref())
        else {
            continue;
        };
        for process in facts.processes.iter_mut().flatten() {
            let same = before.iter().find(|old| {
                old.pid == process.pid
                    && old.stale == process.stale
                    && old
                        .started
                        .duration_since(process.started)
                        .unwrap_or_else(|earlier| earlier.duration())
                        < JITTER
            });
            if let Some(same) = same {
                process.started = same.started;
            }
        }
    }
}

/// What a watcher has received from one agent since it started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Activity {
    pub(crate) sessions: BTreeSet<String>,
    pub(crate) last_event_at_ms: Option<i64>,
}

pub(crate) fn system_time_rfc3339(time: SystemTime) -> Option<String> {
    let ms = time.duration_since(UNIX_EPOCH).ok()?.as_millis();
    af_core::rfc3339_ms(i64::try_from(ms).ok()?)
}

/// One element of the status payload's `agents` array.
///
/// `state` is the first rule that matches, and `receiving` comes first on
/// purpose: an event that actually arrived outranks anything the files or
/// the process table suggest.
pub(crate) fn agent_row(facts: &AgentFacts, activity: Option<&Activity>) -> Value {
    let last_event_at_ms = activity.and_then(|activity| activity.last_event_at_ms);
    let has_stale = facts
        .processes
        .as_ref()
        .is_some_and(|processes| processes.iter().any(|process| process.stale));

    let (state, action): (&str, Option<String>) = if last_event_at_ms.is_some() {
        ("receiving", None)
    } else {
        match (&facts.config, facts.installed) {
            #[cfg(feature = "experimental-opencode")]
            (_, true) if facts.agent == AgentId::OpenCode => {
                ("waiting", Some("af collect opencode".to_string()))
            }
            #[cfg(feature = "experimental-opencode")]
            (_, false) if facts.agent == AgentId::OpenCode => ("not_installed", None),
            (UserConfig::Configured { .. }, _) if has_stale => (
                "needs_restart",
                Some(format!("restart {}", facts.agent.command())),
            ),
            (UserConfig::Configured { .. }, _) => ("waiting", None),
            (UserConfig::Misconfigured { .. }, _) => {
                ("misconfigured", Some("af setup".to_string()))
            }
            (UserConfig::Absent, true) => ("not_configured", Some("af setup".to_string())),
            (UserConfig::Absent, false) => ("not_installed", None),
        }
    };

    let (config_path, detail) = match &facts.config {
        UserConfig::Absent => (None, None),
        UserConfig::Configured { path } => (Some(path), None),
        UserConfig::Misconfigured { path, detail } => (Some(path), Some(detail.as_str())),
    };

    json!({
        "id": facts.agent.id(),
        "name": facts.agent.name(),
        "state": state,
        "detail": detail,
        "action": action,
        "config_path": config_path.map(|path| path.display().to_string()),
        "processes": facts.processes.as_ref().map(|processes| {
            processes
                .iter()
                .map(|process| {
                    json!({
                        "pid": process.pid,
                        "started_at": system_time_rfc3339(process.started),
                        "stale": process.stale,
                    })
                })
                .collect::<Vec<_>>()
        }),
        "sessions": activity.map_or(0, |activity| activity.sessions.len()),
        "last_event_at": last_event_at_ms.and_then(af_core::rfc3339_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSOF: &str = "p6363\nfcwd\nn/Users/u/oss/dcb\nftxt\nn/opt/claude\np74475\nfcwd\nn/p\nf20\nn/p/.codex/state_5.sqlite\nf21\nn/p/.codex/state_5.sqlite-wal\nf60\nn/p/.codex/sessions/2026/11/16/rollout-2026-11-16T12-29-52-01a1e41e-e5cc-7150-8e41-13fcbc72f747.jsonl\n";

    #[test]
    fn parse_lsof_groups_files_by_pid() {
        let parsed = parse_lsof(LSOF);
        assert_eq!(parsed.keys().copied().collect::<Vec<_>>(), [6363, 74475]);
        assert_eq!(
            parsed[&6363],
            [
                ("cwd".to_string(), PathBuf::from("/Users/u/oss/dcb")),
                ("txt".to_string(), PathBuf::from("/opt/claude")),
            ]
        );
        assert_eq!(parsed[&74475].len(), 4);
    }

    #[test]
    fn inspection_finds_cwd_codex_home_and_session() {
        let parsed = parse_lsof(LSOF);
        assert_eq!(
            inspection_from(&parsed[&74475]),
            Inspection {
                cwd: Some(PathBuf::from("/p")),
                codex_home: Some(PathBuf::from("/p/.codex")),
                codex_session: Some("01a1e41e-e5cc-7150-8e41-13fcbc72f747".to_string()),
            }
        );
    }

    #[test]
    fn a_state_database_alone_names_the_codex_home() {
        let files = [("20".to_string(), PathBuf::from("/h/.codex/state_5.sqlite"))];
        let inspection = inspection_from(&files);
        assert_eq!(inspection.codex_home, Some(PathBuf::from("/h/.codex")));
        assert_eq!(inspection.codex_session, None);
        assert_eq!(inspection.cwd, None);
    }

    #[test]
    fn inspection_of_a_process_with_no_codex_files_has_no_home() {
        let parsed = parse_lsof(LSOF);
        assert_eq!(
            inspection_from(&parsed[&6363]),
            Inspection {
                cwd: Some(PathBuf::from("/Users/u/oss/dcb")),
                ..Inspection::default()
            }
        );
    }

    #[test]
    fn a_rollout_name_without_a_uuid_gives_a_home_but_no_session() {
        let files = [(
            "9".to_string(),
            PathBuf::from("/h/.codex/sessions/rollout-odd.jsonl"),
        )];
        let inspection = inspection_from(&files);
        assert_eq!(inspection.codex_home, Some(PathBuf::from("/h/.codex")));
        assert_eq!(inspection.codex_session, None);
    }

    #[test]
    fn inspecting_no_process_or_a_dead_one_yields_nothing_and_does_not_panic() {
        assert!(inspect_processes(&[]).is_empty());
        // No such pid: whatever the platform, no facts are invented.
        let inspected = inspect_processes(&[u32::MAX - 1]);
        assert!(inspected.values().all(|i| *i == Inspection::default()));
    }

    #[test]
    fn keep_start_times_absorbs_ps_jitter_but_not_a_reused_pid() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let facts = |processes: Vec<(u32, SystemTime)>| {
            vec![AgentFacts {
                agent: AgentId::ClaudeCode,
                installed: true,
                config: UserConfig::Absent,
                processes: Some(
                    processes
                        .into_iter()
                        .map(|(pid, started)| ProcessFact {
                            pid,
                            started,
                            stale: false,
                        })
                        .collect(),
                ),
            }]
        };
        let old = facts(vec![(1, t0), (2, t0)]);
        let mut new = facts(vec![
            (1, t0 + Duration::from_millis(900)),
            (2, t0 + Duration::from_secs(3600)),
            (3, t0),
        ]);
        keep_start_times(&mut new, &old);
        assert_eq!(
            new,
            facts(vec![(1, t0), (2, t0 + Duration::from_secs(3600)), (3, t0)]),
            "pid 1 keeps its first estimate; pid 2 is another process under a reused pid"
        );
    }

    const ENDPOINT: &str = "http://127.0.0.1:4318/v1/logs";

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

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

    // --- collector → agent -------------------------------------------------

    #[test]
    fn collectors_map_to_agents() {
        assert_eq!(
            agent_for_collector("claude-code"),
            Some(AgentId::ClaudeCode)
        );
        assert_eq!(agent_for_collector("otlp-cc"), Some(AgentId::ClaudeCode));
        // The hook collector's own name, which is also its spool file prefix.
        assert_eq!(agent_for_collector("cc-hooks"), Some(AgentId::ClaudeCode));
        assert_eq!(agent_for_collector("codex"), Some(AgentId::Codex));
        assert_eq!(agent_for_collector("otlp-codex"), Some(AgentId::Codex));
        assert_eq!(agent_for_collector("otlp-codex-tool"), Some(AgentId::Codex));
        for other in [
            "otlp-http",
            "otlp-genai",
            "codecarbon",
            "codecarbon-sampler",
            "",
        ] {
            assert_eq!(agent_for_collector(other), None, "{other}");
        }
    }

    // --- user-scope config -------------------------------------------------

    fn claude_file(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    fn claude_settings(endpoint: &str, hook: Option<&str>) -> String {
        let mut value = json!({
            "env": {
                "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
                "OTEL_LOGS_EXPORTER": "otlp",
                "OTEL_EXPORTER_OTLP_ENDPOINT": endpoint,
            }
        });
        if let Some(command) = hook {
            value["hooks"] = json!({
                "PostToolUse": [{ "hooks": [{ "type": "command", "command": command }] }]
            });
        }
        value.to_string()
    }

    #[test]
    fn claude_absent_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(inspect_claude_file(&path, ENDPOINT), UserConfig::Absent);
    }

    /// A home with user-scope Claude settings and a project next to it.
    fn claude_scopes_fixture(user: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        if let Some(user) = user {
            std::fs::write(home.join(".claude/settings.json"), user).unwrap();
        }
        (dir, home, project)
    }

    const GOOD_HOOK: &str = "/usr/local/bin/af hook";

    #[test]
    fn project_local_scope_disabling_telemetry_is_misconfigured() {
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        assert!(matches!(
            claude_config_for(&home, None, ENDPOINT).0,
            UserConfig::Configured { .. }
        ));
        let local = project.join(".claude/settings.local.json");
        std::fs::write(&local, r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#).unwrap();
        let (config, _) = claude_config_for(&home, Some(&project), ENDPOINT);
        let UserConfig::Misconfigured { path, detail } = config else {
            panic!("expected misconfigured, got {config:?}");
        };
        assert_eq!(path, local, "the file that switches it off");
        assert!(detail.contains("CLAUDE_CODE_ENABLE_TELEMETRY"), "{detail}");
    }

    #[test]
    fn project_scope_supplies_the_config_the_user_scope_lacks() {
        let (_dir, home, project) = claude_scopes_fixture(None);
        assert_eq!(
            claude_config_for(&home, Some(&project), ENDPOINT).0,
            UserConfig::Absent
        );
        let settings = project.join(".claude/settings.json");
        std::fs::write(
            &settings,
            claude_settings("http://127.0.0.1:4318", Some(GOOD_HOOK)),
        )
        .unwrap();
        let (config, modified) = claude_config_for(&home, Some(&project), ENDPOINT);
        assert_eq!(config, UserConfig::Configured { path: settings });
        assert!(modified.is_some(), "the loaded file's time, for staleness");
        assert_eq!(
            claude_config_for(&home, None, ENDPOINT).0,
            UserConfig::Absent,
            "a process elsewhere does not load this project's settings"
        );
    }

    #[test]
    fn a_later_scope_overrides_the_endpoint_of_an_earlier_one() {
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let settings = project.join(".claude/settings.json");
        std::fs::write(
            &settings,
            r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"http://collector:4318"}}"#,
        )
        .unwrap();
        let (config, _) = claude_config_for(&home, Some(&project), ENDPOINT);
        let UserConfig::Misconfigured { path, detail } = config else {
            panic!("expected misconfigured, got {config:?}");
        };
        assert_eq!(path, settings);
        assert!(detail.contains("http://collector:4318"), "{detail}");
    }

    #[test]
    fn disable_all_hooks_in_any_scope_is_misconfigured() {
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let local = project.join(".claude/settings.local.json");
        std::fs::write(&local, r#"{"disableAllHooks":true}"#).unwrap();
        let (config, _) = claude_config_for(&home, Some(&project), ENDPOINT);
        let UserConfig::Misconfigured { path, detail } = config else {
            panic!("expected misconfigured, got {config:?}");
        };
        assert_eq!(path, local);
        assert!(detail.contains("disableAllHooks"), "{detail}");
    }

    #[test]
    fn an_unrelated_project_file_neither_configures_nor_ages_the_config() {
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let (_, before) = claude_config_for(&home, None, ENDPOINT);
        std::fs::write(
            project.join(".claude/settings.local.json"),
            r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#,
        )
        .unwrap();
        let (config, after) = claude_config_for(&home, Some(&project), ENDPOINT);
        assert_eq!(
            config,
            UserConfig::Configured {
                path: home.join(".claude/settings.json")
            }
        );
        assert_eq!(
            after, before,
            "editing permissions does not make processes stale"
        );
    }

    #[test]
    fn codex_home_without_config_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (config, modified) = codex_config_for(dir.path(), ENDPOINT);
        assert_eq!(config, UserConfig::Absent);
        assert_eq!(modified, None);
        std::fs::write(
            dir.path().join("config.toml"),
            setup::append_codex_otel("", ENDPOINT),
        )
        .unwrap();
        let (config, modified) = codex_config_for(dir.path(), ENDPOINT);
        assert_eq!(
            config,
            UserConfig::Configured {
                path: dir.path().join("config.toml")
            }
        );
        assert!(modified.is_some());
    }

    #[test]
    fn claude_settings_without_otel_are_absent() {
        let (_dir, path) = claude_file(r#"{"env":{"EDITOR":"vi"}}"#);
        assert_eq!(inspect_claude_file(&path, ENDPOINT), UserConfig::Absent);
    }

    #[test]
    fn claude_configured_needs_matching_endpoint_and_hook() {
        let (_dir, path) = claude_file(&claude_settings(
            "http://127.0.0.1:4318",
            Some("/usr/local/bin/af hook"),
        ));
        assert_eq!(
            inspect_claude_file(&path, ENDPOINT),
            UserConfig::Configured { path: path.clone() }
        );
    }

    #[test]
    fn claude_other_endpoint_is_misconfigured_naming_it() {
        let (_dir, path) = claude_file(&claude_settings(
            "http://127.0.0.1:9999",
            Some("/usr/local/bin/af hook"),
        ));
        let UserConfig::Misconfigured { detail, .. } = inspect_claude_file(&path, ENDPOINT) else {
            panic!("expected misconfigured");
        };
        assert!(detail.contains("http://127.0.0.1:9999"), "{detail}");
    }

    #[test]
    fn claude_otel_without_af_hook_is_misconfigured() {
        let (_dir, path) = claude_file(&claude_settings("http://127.0.0.1:4318", None));
        let UserConfig::Misconfigured { detail, .. } = inspect_claude_file(&path, ENDPOINT) else {
            panic!("expected misconfigured");
        };
        assert!(detail.contains("hook"), "{detail}");
    }

    #[test]
    fn claude_invalid_json_is_misconfigured() {
        let (_dir, path) = claude_file("{ not json");
        let UserConfig::Misconfigured { detail, .. } = inspect_claude_file(&path, ENDPOINT) else {
            panic!("expected misconfigured");
        };
        assert!(detail.contains("not valid JSON"), "{detail}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_config_is_misconfigured_not_absent() {
        // A directory where the file should be: reading it is an error that
        // is not "not found".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(
            inspect_claude_file(&path, ENDPOINT),
            UserConfig::Misconfigured { .. }
        ));
        assert!(matches!(
            inspect_codex_file(&path, ENDPOINT),
            UserConfig::Misconfigured { .. }
        ));
    }

    #[test]
    fn codex_states_follow_codex_otel_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(inspect_codex_file(&path, ENDPOINT), UserConfig::Absent);

        std::fs::write(&path, "model = \"x\"\n").unwrap();
        assert_eq!(inspect_codex_file(&path, ENDPOINT), UserConfig::Absent);

        let matching = format!(
            "[otel]\nlog_user_prompt = false\nmetrics_exporter = \"none\"\ntrace_exporter = \"none\"\nexporter = {{ otlp-http = {{ endpoint = \"{ENDPOINT}\", protocol = \"json\" }} }}\n"
        );
        std::fs::write(&path, &matching).unwrap();
        assert_eq!(
            inspect_codex_file(&path, ENDPOINT),
            UserConfig::Configured { path: path.clone() }
        );
        // The same file judged against another watcher's endpoint.
        assert!(matches!(
            inspect_codex_file(&path, "http://127.0.0.1:9999/v1/logs"),
            UserConfig::Misconfigured { .. }
        ));

        std::fs::write(&path, "[otel\n").unwrap();
        let UserConfig::Misconfigured { detail, .. } = inspect_codex_file(&path, ENDPOINT) else {
            panic!("expected misconfigured");
        };
        assert!(detail.contains("not valid TOML"), "{detail}");
    }

    // --- facts and rows ----------------------------------------------------

    fn configured() -> UserConfig {
        UserConfig::Configured {
            path: PathBuf::from("/home/u/.claude/settings.json"),
        }
    }

    fn claude_process(pid: u32, started: SystemTime) -> AgentProcess {
        AgentProcess {
            pid,
            name: "claude".into(),
            started,
        }
    }

    #[test]
    fn stale_process_is_flagged() {
        let processes = [
            claude_process(1, at(940)),
            claude_process(2, at(1_060)),
            AgentProcess {
                pid: 3,
                name: "codex".into(),
                started: at(1),
            },
        ];
        let facts = facts_from(
            AgentId::ClaudeCode,
            true,
            configured(),
            Some(at(1_000)),
            Some(&processes),
        );
        assert_eq!(
            facts.processes,
            Some(vec![
                ProcessFact {
                    pid: 1,
                    started: at(940),
                    stale: true
                },
                ProcessFact {
                    pid: 2,
                    started: at(1_060),
                    stale: false
                },
            ]),
            "only this agent's processes, each judged against the config's mtime"
        );
    }

    #[test]
    fn no_process_listing_gives_none() {
        let facts = facts_from(AgentId::ClaudeCode, true, configured(), Some(at(1)), None);
        assert_eq!(facts.processes, None);
        let row = agent_row(&facts, None);
        assert_eq!(row["processes"], Value::Null);
        assert_eq!(
            row["state"], "waiting",
            "never needs_restart without a listing"
        );
    }

    fn facts(installed: bool, config: UserConfig, stale: Option<bool>) -> AgentFacts {
        AgentFacts {
            agent: AgentId::ClaudeCode,
            installed,
            config,
            processes: stale.map(|stale| {
                vec![ProcessFact {
                    pid: 6363,
                    started: at(10),
                    stale,
                }]
            }),
        }
    }

    fn active() -> Activity {
        Activity {
            sessions: BTreeSet::from(["s1".to_string(), "s2".to_string()]),
            last_event_at_ms: Some(1_794_000_000_123),
        }
    }

    #[test]
    fn rows_carry_the_contract_fields() {
        let row = agent_row(&facts(true, configured(), Some(false)), Some(&active()));
        assert_eq!(row["id"], "claude-code");
        assert_eq!(row["name"], "Claude Code");
        assert_eq!(row["state"], "receiving");
        assert_eq!(row["detail"], Value::Null);
        assert_eq!(row["action"], Value::Null);
        assert_eq!(row["config_path"], "/home/u/.claude/settings.json");
        assert_eq!(row["sessions"], 2);
        assert_eq!(
            row["last_event_at"],
            af_core::rfc3339_ms(1_794_000_000_123).unwrap()
        );
        assert_eq!(row["processes"][0]["pid"], 6363);
        assert_eq!(row["processes"][0]["stale"], false);
        assert_eq!(
            row["processes"][0]["started_at"],
            af_core::rfc3339_ms(10_000).unwrap()
        );
    }

    #[test]
    fn state_follows_the_first_matching_rule() {
        let misconfigured = UserConfig::Misconfigured {
            path: PathBuf::from("/c"),
            detail: "exports elsewhere".into(),
        };
        // (facts, activity, state, action)
        let cases = [
            (
                facts(false, UserConfig::Absent, None),
                true,
                "receiving",
                None,
            ),
            (
                facts(true, configured(), Some(true)),
                false,
                "needs_restart",
                Some("restart claude"),
            ),
            (facts(true, configured(), None), false, "waiting", None),
            (
                facts(true, configured(), Some(false)),
                false,
                "waiting",
                None,
            ),
            (facts(false, configured(), None), false, "waiting", None),
            (
                facts(true, misconfigured.clone(), None),
                false,
                "misconfigured",
                Some("af setup"),
            ),
            (
                facts(true, UserConfig::Absent, None),
                false,
                "not_configured",
                Some("af setup"),
            ),
            (
                facts(false, UserConfig::Absent, None),
                false,
                "not_installed",
                None,
            ),
        ];
        for (facts, is_active, state, action) in cases {
            let activity = is_active.then(active);
            let row = agent_row(&facts, activity.as_ref());
            assert_eq!(row["state"], state, "{facts:?}");
            assert_eq!(
                row["action"],
                action.map(Value::from).unwrap_or(Value::Null),
                "{facts:?}"
            );
        }
        let row = agent_row(&facts(true, misconfigured, None), None);
        assert_eq!(row["detail"], "exports elsewhere");
    }

    #[test]
    fn a_receiving_agent_keeps_its_stale_process_flag() {
        let row = agent_row(&facts(true, configured(), Some(true)), Some(&active()));
        assert_eq!(row["state"], "receiving");
        assert_eq!(row["processes"][0]["stale"], true);
    }

    #[test]
    fn an_agent_without_events_has_no_last_event() {
        let row = agent_row(&facts(true, configured(), None), None);
        assert_eq!(row["sessions"], 0);
        assert_eq!(row["last_event_at"], Value::Null);
        // Seen, but nothing timestamped: still not "receiving".
        let empty = Activity::default();
        assert_eq!(
            agent_row(&facts(true, configured(), None), Some(&empty))["state"],
            "waiting"
        );
    }

    #[cfg(feature = "experimental-opencode")]
    #[test]
    fn opencode_has_three_states_only() {
        let opencode = |installed| AgentFacts {
            agent: AgentId::OpenCode,
            installed,
            config: UserConfig::Absent,
            processes: None,
        };
        assert_eq!(agent_for_collector("opencode"), Some(AgentId::OpenCode));
        let waiting = agent_row(&opencode(true), None);
        assert_eq!(waiting["state"], "waiting");
        assert_eq!(waiting["action"], "af collect opencode");
        assert_eq!(agent_row(&opencode(false), None)["state"], "not_installed");
        assert_eq!(
            agent_row(&opencode(false), Some(&active()))["state"],
            "receiving"
        );
    }

    #[test]
    fn every_agent_is_listed_once_with_distinct_ids() {
        let ids: BTreeSet<&str> = AgentId::all().into_iter().map(AgentId::id).collect();
        assert_eq!(ids.len(), AgentId::all().len());
        assert!(ids.contains("claude-code") && ids.contains("codex"));
    }
}
