//! What this machine can say about each supported coding agent before and
//! after it sends anything: is its command installed, does its user-level
//! configuration export to this receiver, is a running process older than
//! that configuration.
//!
//! Shared by `af doctor` (which prints the stale-process finding), and by
//! `af watch`, which turns the same facts into the `agents` rows of
//! `GET /debug/status`. With no process to look at, only **user-scope**
//! files are read (`~/.claude/settings.json`, the user Codex
//! `config.toml`). A running process is judged by what it actually loads:
//! its working directory's project scopes for Claude Code, the home it
//! holds open for Codex — see [`Inspection`].

use std::collections::BTreeMap;
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
    /// The Codex sessions the process is writing, from its open rollout
    /// files: one process can run several threads at once.
    pub(crate) codex_sessions: Vec<String>,
    /// `CLAUDE_CONFIG_DIR` from the process's environment, where that can
    /// be read (Linux): the directory that replaces `~/.claude`.
    pub(crate) claude_config_dir: Option<PathBuf>,
}

/// `lsof -F pfn` output: a `p<pid>` line opens a process, then pairs of
/// `f<descriptor>` and `n<name>` lines list its open files.
// Used on macOS only (Linux reads /proc); the tests run everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
                // The rollout names a session being written: it wins over
                // a database that only names the home.
                inspection.codex_home = Some(home.to_path_buf());
                if let Some(session) = trailing_uuid(stem) {
                    if !inspection.codex_sessions.contains(&session) {
                        inspection.codex_sessions.push(session);
                    }
                }
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

/// The longest an inspection tool may take. Detection runs on the watch
/// loop, and nothing it learns is worth stalling ingestion for.
// Used on macOS only (Linux reads /proc); the tests run everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const INSPECT_DEADLINE: Duration = Duration::from_secs(2);

/// Runs `command` and returns its standard output, or `None` when it cannot
/// be started or has not finished within `deadline` (it is then killed).
/// Its exit status is the caller's to ignore: only what it printed counts.
// Used on macOS only (Linux reads /proc); the tests run everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn output_within(mut command: Command, deadline: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read on a thread: a child that fills the pipe would otherwise never
    // exit, and the wait below would always run to the deadline.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let give_up = std::time::Instant::now() + deadline;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return reader.join().ok(),
            Ok(None) if std::time::Instant::now() < give_up => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
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
    // `-n -P -b`: no name lookups and no blocking kernel calls, so a dead
    // network mount or a slow resolver cannot hold the listing up.
    let mut lsof = Command::new("lsof");
    lsof.args(["-n", "-P", "-b", "-a", "-p", &list, "-F", "pfn"]);
    let Some(stdout) = output_within(lsof, INSPECT_DEADLINE) else {
        return BTreeMap::new();
    };
    parse_lsof(&String::from_utf8_lossy(&stdout))
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
            // Linux lets a user read their own processes' environment.
            let environ = std::fs::read(root.join("environ")).unwrap_or_default();
            let variable = |name: &str| {
                let prefix = format!("{name}=");
                environ
                    .split(|byte| *byte == 0)
                    .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
                    .filter(|value| !value.is_empty())
                    .map(|value| PathBuf::from(String::from_utf8_lossy(value).into_owned()))
            };
            if inspection.codex_home.is_none() {
                inspection.codex_home = variable("CODEX_HOME");
            }
            inspection.claude_config_dir = variable("CLAUDE_CONFIG_DIR");
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
    Pi,
}

/// Collector name of the Pi extension (`collectors/pi/af-collector.ts`) and
/// the prefix of its spool files. The collector is TypeScript running inside
/// Pi, so there is no Rust module to own this constant.
pub(crate) const PI_COLLECTOR_NAME: &str = "pi";

impl AgentId {
    pub(crate) fn all() -> Vec<AgentId> {
        vec![
            AgentId::ClaudeCode,
            AgentId::Codex,
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode,
            AgentId::Pi,
        ]
    }

    /// Stable identifier used on the wire.
    pub(crate) fn id(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "claude-code",
            AgentId::Codex => "codex",
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode => "opencode",
            AgentId::Pi => "pi",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "Claude Code",
            AgentId::Codex => "Codex",
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode => "OpenCode",
            AgentId::Pi => "Pi",
        }
    }

    /// The executable name, as on `PATH` and in `ps`.
    pub(crate) fn command(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "claude",
            AgentId::Codex => "codex",
            #[cfg(feature = "experimental-opencode")]
            AgentId::OpenCode => "opencode",
            AgentId::Pi => "pi",
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
        PI_COLLECTOR_NAME => Some(AgentId::Pi),
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
        AgentId::ClaudeCode => claude_config_for(home, None, logs_endpoint).0,
        AgentId::Codex => inspect_codex_file(&setup::codex_config_path(home), logs_endpoint),
        // No config file and no OTLP path: collected per session.
        #[cfg(feature = "experimental-opencode")]
        AgentId::OpenCode => UserConfig::Absent,
        // The collector is an extension `af setup` installs into Pi's global
        // extensions directory; its presence is the configuration.
        AgentId::Pi => inspect_pi_extension(&setup::pi_extension_path(home)),
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

#[cfg(test)]
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
    claude_config_in(None, home, project, logs_endpoint)
}

/// [`claude_config_for`] for a process known to run with `config_dir` as
/// its `CLAUDE_CONFIG_DIR`, which takes the place of `~/.claude`.
fn claude_config_in(
    config_dir: Option<&Path>,
    home: &Path,
    project: Option<&Path>,
    logs_endpoint: &str,
) -> (UserConfig, Option<SystemTime>) {
    let user = match config_dir {
        Some(dir) => dir.join("settings.json"),
        None => setup::claude_user_settings(home),
    };
    let mut scopes = vec![user];
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
        // The last scope to set it has the last word.
        if let Some(disabled) = settings.get("disableAllHooks").and_then(Value::as_bool) {
            hooks_disabled = disabled.then(|| path.clone());
            takes_part = true;
        }
        for (key, value) in settings
            .get("env")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if key.starts_with("OTEL_") || key == "CLAUDE_CODE_ENABLE_TELEMETRY" {
                // Claude Code stringifies whatever JSON value it is given.
                let value = match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
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
    // What `af setup` writes: the base endpoint, without `/v1/logs`. The
    // logs-specific variable, when set, is the one Claude Code obeys.
    let expected = setup::logs_base_endpoint(logs_endpoint).unwrap_or(logs_endpoint);
    let wrong_endpoint = match (
        env.get("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
        env.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
    ) {
        (Some((logs, from)), _) => (logs != logs_endpoint).then(|| {
            misconfigured(
                from,
                format!("exports logs to {logs}, not to {logs_endpoint}"),
            )
        }),
        (None, Some((exported, from))) => (exported != expected).then(|| {
            misconfigured(
                from,
                format!("exports telemetry to {exported}, not to {expected}"),
            )
        }),
        (None, None) => Some(misconfigured(
            &otel_path,
            format!("sets OTEL variables but no endpoint; expected {expected}"),
        )),
    };
    let switched_off = |value: &str| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    };
    // Likewise for the protocol: this receiver takes http/json only, and
    // the logs-specific variable has the last word.
    let wrong_protocol = env
        .get("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL")
        .or_else(|| env.get("OTEL_EXPORTER_OTLP_PROTOCOL"))
        .filter(|(protocol, _)| protocol != "http/json")
        .map(|(protocol, from)| {
            misconfigured(
                from,
                format!("exports logs as {protocol}; this receiver takes http/json"),
            )
        });
    let config = if let Some(wrong) = wrong_endpoint.or(wrong_protocol) {
        wrong
    } else {
        match (
            env.get("CLAUDE_CODE_ENABLE_TELEMETRY"),
            env.get("OTEL_LOGS_EXPORTER"),
            hooks_disabled,
        ) {
            (Some((value, from)), _, _) if switched_off(value) => misconfigured(
                from,
                format!(
                    "sets CLAUDE_CODE_ENABLE_TELEMETRY to \"{value}\", which switches telemetry off"
                ),
            ),
            (_, Some((exporters, from)), _)
                if !exporters
                    .split(',')
                    .any(|exporter| exporter.trim() == "otlp") =>
            {
                misconfigured(
                    from,
                    format!("sets OTEL_LOGS_EXPORTER to \"{exporters}\", so no logs are exported"),
                )
            }
            (_, _, Some(from)) => misconfigured(
                &from,
                "sets disableAllHooks, so the af hook cannot run".to_string(),
            ),
            _ if !has_af_hook => misconfigured(
                &otel_path,
                "exports telemetry but the af hook is not registered".to_string(),
            ),
            _ => UserConfig::Configured { path: otel_path },
        }
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

/// Pi is configured when `af setup`'s extension is in Pi's global
/// extensions directory; a copy from another af version still collects but
/// should be refreshed.
fn inspect_pi_extension(path: &Path) -> UserConfig {
    match read_config(path) {
        Ok(None) => UserConfig::Absent,
        Ok(Some(contents)) if contents == setup::PI_EXTENSION => UserConfig::Configured {
            path: path.to_path_buf(),
        },
        Ok(Some(_)) => UserConfig::Misconfigured {
            path: path.to_path_buf(),
            detail: "the collector extension is from another af version".to_string(),
        },
        Err(unreadable) => unreadable,
    }
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
    /// Where it runs, when it could be inspected.
    pub(crate) cwd: Option<PathBuf>,
    /// The configuration this very process loads. `None`: it could not be
    /// inspected far enough to know, and nothing is claimed about it.
    pub(crate) config: Option<UserConfig>,
    /// The file that configuration lives in — or would live in, for a
    /// Codex home that has none.
    pub(crate) config_path: Option<PathBuf>,
    /// The sessions it is running, when a link exists.
    pub(crate) session_ids: Vec<String>,
}

impl ProcessFact {
    /// A process nothing is known about beyond the process table.
    fn listed(pid: u32, started: SystemTime, stale: bool) -> Self {
        Self {
            pid,
            started,
            stale,
            cwd: None,
            config: None,
            config_path: None,
            session_ids: Vec::new(),
        }
    }
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
            .map(|process| {
                ProcessFact::listed(
                    process.pid,
                    process.started,
                    config_modified.is_some_and(|modified| process.started < modified),
                )
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

/// Detects every supported agent: the user-scope config files, `PATH`,
/// one `ps`, and an inspection of the agent processes it lists. Writes
/// nothing. `cache` holds what earlier calls found, so that a settled
/// process is not looked at twice.
pub(crate) fn detect_live(
    home: &Path,
    logs_endpoint: &str,
    cache: &mut BTreeMap<u32, Inspection>,
    claude_sessions: &BTreeMap<u32, String>,
) -> Vec<AgentFacts> {
    let processes = running_agents();
    if let Some(processes) = &processes {
        let wanted = pids_to_inspect(processes, cache);
        cache.extend(inspect_processes(&wanted));
    }
    detect_from(
        home,
        logs_endpoint,
        processes.as_deref(),
        cache,
        claude_sessions,
    )
}

/// Forgets processes that are gone and names those worth inspecting: any
/// not seen yet, a Claude Code whose directory is still unknown, and every
/// Codex — it can start another session, whose file names the new one.
fn pids_to_inspect(processes: &[AgentProcess], cache: &mut BTreeMap<u32, Inspection>) -> Vec<u32> {
    cache.retain(|pid, _| processes.iter().any(|process| process.pid == *pid));
    processes
        .iter()
        .filter(|process| {
            process.name == AgentId::Codex.command()
                || cache
                    .get(&process.pid)
                    .is_none_or(|seen| seen.cwd.is_none())
        })
        .map(|process| process.pid)
        .collect()
}

/// [`detect_live`] from already-gathered inputs: the process list, what
/// inspecting those processes found (by pid), and the Claude Code sessions
/// the hook tied to a pid.
pub(crate) fn detect_from(
    home: &Path,
    logs_endpoint: &str,
    processes: Option<&[AgentProcess]>,
    inspections: &BTreeMap<u32, Inspection>,
    claude_sessions: &BTreeMap<u32, String>,
) -> Vec<AgentFacts> {
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
            let mut facts = facts_from(
                agent,
                setup::command_exists(agent.command()),
                config,
                modified,
                processes,
            );
            for process in facts.processes.iter_mut().flatten() {
                if let Some(inspection) = inspections.get(&process.pid) {
                    judge_process(
                        agent,
                        process,
                        inspection,
                        home,
                        logs_endpoint,
                        claude_sessions,
                    );
                }
            }
            facts
        })
        .collect()
}

/// Fills in what inspecting one process allows to be said about it: the
/// configuration it loads, and whether it started before that changed.
fn judge_process(
    agent: AgentId,
    process: &mut ProcessFact,
    inspection: &Inspection,
    home: &Path,
    logs_endpoint: &str,
    claude_sessions: &BTreeMap<u32, String>,
) {
    process.cwd = inspection.cwd.clone();
    let judged = match agent {
        AgentId::ClaudeCode => {
            process.session_ids = claude_sessions
                .get(&process.pid)
                .cloned()
                .into_iter()
                .collect();
            // Without its directory the project scopes are unknown, and
            // they can switch everything off.
            inspection.cwd.as_deref().map(|cwd| {
                claude_config_in(
                    inspection.claude_config_dir.as_deref(),
                    home,
                    Some(cwd),
                    logs_endpoint,
                )
            })
        }
        AgentId::Codex => {
            process.session_ids = inspection.codex_sessions.clone();
            inspection.codex_home.as_deref().map(|codex_home| {
                process.config_path = Some(codex_home.join("config.toml"));
                codex_config_for(codex_home, logs_endpoint)
            })
        }
        #[cfg(feature = "experimental-opencode")]
        AgentId::OpenCode => None,
        AgentId::Pi => None,
    };
    let Some((config, modified)) = judged else {
        return;
    };
    if let UserConfig::Configured { path } | UserConfig::Misconfigured { path, .. } = &config {
        process.config_path = Some(path.clone());
    }
    process.stale = modified.is_some_and(|modified| process.started < modified);
    process.config = Some(config);
}

/// `path` as one word of a command meant to be pasted: as it is when
/// nothing in it means anything to a shell, else in single quotes. POSIX
/// shells write a quote inside them as `'\''`; PowerShell (`powershell`
/// true), where `\` and `:` are ordinary, doubles it.
fn shell_path(path: &Path, powershell: bool) -> String {
    let text = path.display().to_string();
    let ordinary = if powershell { "_./-\\:" } else { "_./-" };
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ordinary.contains(c))
    {
        text
    } else if powershell {
        format!("'{}'", text.replace('\'', "''"))
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// The `af setup` invocation that configures what this process loads.
fn setup_command(agent: AgentId, process: &ProcessFact, home: &Path) -> String {
    let base = format!("af setup --agents {}", agent.id());
    let quoted = |path: &Path| shell_path(path, cfg!(windows));
    let codex_home = process.config_path.as_deref().and_then(Path::parent);
    match (agent, &process.cwd, codex_home) {
        // A Codex home that is neither the default nor the project's own:
        // setup finds it through the same variable Codex was given.
        (AgentId::Codex, cwd, Some(codex_home))
            if codex_home != home.join(".codex")
                && cwd.as_deref().map(|cwd| cwd.join(".codex")).as_deref() != Some(codex_home) =>
        {
            if cfg!(windows) {
                format!("$env:CODEX_HOME = {}; {base}", quoted(codex_home))
            } else {
                format!("CODEX_HOME={} {base}", quoted(codex_home))
            }
        }
        (AgentId::Codex, _, Some(codex_home)) if codex_home == home.join(".codex") => base,
        (_, Some(cwd), _) => format!("{base} --project {}", quoted(cwd)),
        _ => base,
    }
}

/// What the agent's events say beyond the sessions tied to a process.
#[derive(Clone, Copy)]
struct Unexplained {
    /// The latest event from a session no listed process accounts for.
    last_ms: Option<i64>,
    /// Whether there are at least as many such sessions as configured
    /// processes with no session link — only then may each of those
    /// processes be credited with one.
    covers_every_unlinked_process: bool,
}

/// `(state, detail, action, last event ms, session)` of one process.
type ProcessState<'a> = (
    &'static str,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<&'a String>,
);

/// One process's state. First rule that matches; an event that arrived
/// outranks every file.
fn process_state<'a>(
    agent: AgentId,
    process: &'a ProcessFact,
    home: &Path,
    activity: Option<&Activity>,
    unexplained: Unexplained,
) -> ProcessState<'a> {
    let heard = process
        .session_ids
        .iter()
        .filter_map(|session| Some((activity?.sessions.get(session).copied()?, session)))
        .max();
    if let Some((at, session)) = heard {
        return ("receiving", None, None, Some(at), Some(session));
    }
    let session = process.session_ids.first();
    let Some(config) = &process.config else {
        return (
            "unknown",
            Some("cannot inspect this process".to_string()),
            None,
            None,
            session,
        );
    };
    match config {
        // No command: `af setup` reports these as conflicts to resolve by
        // hand, in the file named here.
        UserConfig::Misconfigured { path, detail } => (
            "misconfigured",
            Some(format!("{}: {detail}", path.display())),
            None,
            None,
            session,
        ),
        UserConfig::Absent => (
            "not_configured",
            process
                .config_path
                .as_ref()
                .map(|path| format!("{} has no telemetry configuration", path.display())),
            Some(setup_command(agent, process, home)),
            None,
            session,
        ),
        UserConfig::Configured { .. } if process.stale => (
            "needs_restart",
            None,
            Some(format!("restart {}", agent.command())),
            None,
            session,
        ),
        // Configured, tied to no session, while the agent's events come
        // from sessions no process accounts for. With enough of those to
        // go round, they may well be its own, and "nothing received" would
        // be a claim nothing supports. With fewer, crediting every such
        // process would hide a silent one: it stays waiting, and says why.
        UserConfig::Configured { .. } if session.is_none() && unexplained.last_ms.is_some() => {
            if unexplained.covers_every_unlinked_process {
                ("receiving", None, None, unexplained.last_ms, None)
            } else {
                (
                    "waiting",
                    Some(
                        "the agent is receiving, but that cannot be tied to this process"
                            .to_string(),
                    ),
                    None,
                    None,
                    None,
                )
            }
        }
        UserConfig::Configured { .. } => ("waiting", None, None, None, session),
    }
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
    /// Each session heard from, with the time of its last event (ms).
    pub(crate) sessions: BTreeMap<String, i64>,
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
#[cfg(test)]
pub(crate) fn agent_row(facts: &AgentFacts, activity: Option<&Activity>) -> Value {
    agent_row_in(facts, activity, Path::new(""))
}

/// [`agent_row`] knowing the home directory, which only decides how a
/// setup command for a Codex in its default home is spelled.
pub(crate) fn agent_row_in(facts: &AgentFacts, activity: Option<&Activity>, home: &Path) -> Value {
    let last_event_at_ms = activity.and_then(|activity| activity.last_event_at_ms);
    let processes = facts.processes.as_deref().unwrap_or_default();

    // Events from sessions that no listed process accounts for.
    let orphans: Vec<i64> = activity
        .into_iter()
        .flat_map(|activity| &activity.sessions)
        .filter(|(session, _)| {
            !processes
                .iter()
                .any(|process| process.session_ids.contains(session))
        })
        .map(|(_, at)| *at)
        .collect();
    let unlinked = processes
        .iter()
        .filter(|process| {
            process.session_ids.is_empty()
                && !process.stale
                && matches!(process.config, Some(UserConfig::Configured { .. }))
        })
        .count();
    let unexplained = Unexplained {
        last_ms: orphans.iter().copied().max(),
        covers_every_unlinked_process: unlinked <= orphans.len(),
    };
    let states: Vec<_> = processes
        .iter()
        .map(|process| process_state(facts.agent, process, home, activity, unexplained))
        .collect();

    // Without an event, the most actionable thing a process says; with no
    // process that could be inspected, what the user-scope file says.
    let from_processes = [
        "needs_restart",
        "misconfigured",
        "not_configured",
        "waiting",
    ]
    .into_iter()
    .find(|wanted| states.iter().any(|(state, ..)| state == wanted));
    let (state, action): (&str, Option<String>) = if last_event_at_ms.is_some() {
        ("receiving", None)
    } else if let Some(state) = from_processes {
        (state, None)
    } else {
        global_state(facts)
    };
    let global = from_processes.is_none() || last_event_at_ms.is_some();
    let (config_path, detail) = match &facts.config {
        UserConfig::Absent => (None, None),
        UserConfig::Configured { path } => (Some(path), None),
        UserConfig::Misconfigured { path, detail } => (Some(path), Some(detail.as_str())),
    };

    json!({
        "id": facts.agent.id(),
        "name": facts.agent.name(),
        "state": state,
        // The user-scope file's own trouble; silent when the state above
        // was read from a process, whose row carries its reason.
        "detail": detail.filter(|_| global),
        "action": action,
        "config_path": config_path.map(|path| path.display().to_string()),
        "processes": facts.processes.as_ref().map(|processes| {
            processes
                .iter()
                .zip(&states)
                .map(|(process, (state, detail, action, heard, session))| {
                    json!({
                        "pid": process.pid,
                        "started_at": system_time_rfc3339(process.started),
                        "stale": process.stale,
                        "cwd": process.cwd.as_ref().map(|path| path.display().to_string()),
                        "config_path": process
                            .config_path
                            .as_ref()
                            .map(|path| path.display().to_string()),
                        "state": state,
                        "detail": detail,
                        "action": action,
                        "session_id": session,
                        "last_event_at": heard.and_then(af_core::rfc3339_ms),
                    })
                })
                .collect::<Vec<_>>()
        }),
        "sessions": activity.map_or(0, |activity| activity.sessions.len()),
        "last_event_at": last_event_at_ms.and_then(af_core::rfc3339_ms),
    })
}

/// The agent's state from the user-scope file and the process table alone
/// — what there is to say when no process could be inspected.
fn global_state(facts: &AgentFacts) -> (&'static str, Option<String>) {
    let has_stale = facts
        .processes
        .as_ref()
        .is_some_and(|processes| processes.iter().any(|process| process.stale));
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
        (UserConfig::Misconfigured { .. }, _) => ("misconfigured", Some("af setup".to_string())),
        (UserConfig::Absent, true) => ("not_configured", Some("af setup".to_string())),
        (UserConfig::Absent, false) => ("not_installed", None),
    }
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
                codex_sessions: vec!["01a1e41e-e5cc-7150-8e41-13fcbc72f747".to_string()],
                claude_config_dir: None,
            }
        );
    }

    #[test]
    fn a_state_database_alone_names_the_codex_home() {
        let files = [("20".to_string(), PathBuf::from("/h/.codex/state_5.sqlite"))];
        let inspection = inspection_from(&files);
        assert_eq!(inspection.codex_home, Some(PathBuf::from("/h/.codex")));
        assert!(inspection.codex_sessions.is_empty());
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
        assert!(inspection.codex_sessions.is_empty());
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
                        .map(|(pid, started)| ProcessFact::listed(pid, started, false))
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
                ProcessFact::listed(1, at(940), true),
                ProcessFact::listed(2, at(1_060), false),
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

    // --- per-process state ---------------------------------------------------

    #[test]
    fn a_codex_running_several_sessions_is_heard_through_any_of_them() {
        let files = [
            ("9".to_string(), PathBuf::from("/h/.codex/sessions/2026/rollout-2026-11-16T12-00-00-aaaaaaaa-0000-0000-0000-000000000001.jsonl")),
            ("10".to_string(), PathBuf::from("/h/.codex/sessions/2026/rollout-2026-11-16T12-05-00-aaaaaaaa-0000-0000-0000-000000000002.jsonl")),
        ];
        let inspection = inspection_from(&files);
        let (idle, busy) = (
            "aaaaaaaa-0000-0000-0000-000000000001",
            "aaaaaaaa-0000-0000-0000-000000000002",
        );
        assert_eq!(inspection.codex_sessions, [idle, busy]);

        let dir = tempfile::tempdir().unwrap();
        let processes = [codex_process(7, later())];
        let facts = detect_from(
            dir.path(),
            ENDPOINT,
            Some(&processes),
            &BTreeMap::from([(7, inspection)]),
            &BTreeMap::new(),
        );
        let row = row_of(&facts, AgentId::Codex, Some(&sessions(&[(busy, 7_000)])));
        assert_eq!(row["processes"][0]["state"], "receiving");
        assert_eq!(
            row["processes"][0]["session_id"], busy,
            "the one heard from"
        );
    }

    #[test]
    fn one_unexplained_session_does_not_vouch_for_several_silent_processes() {
        let (_dir, home, _project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let processes = [
            claude_process(1, later()),
            claude_process(2, later()),
            claude_process(3, later()),
        ];
        let here = Inspection {
            cwd: Some(home.clone()),
            ..Inspection::default()
        };
        let inspections = BTreeMap::from([(1, here.clone()), (2, here.clone()), (3, here)]);
        let facts = detect_from(
            &home,
            ENDPOINT,
            Some(&processes),
            &inspections,
            &BTreeMap::new(),
        );
        let row = row_of(
            &facts,
            AgentId::ClaudeCode,
            Some(&sessions(&[("orphan", 4_000)])),
        );
        for process in row["processes"].as_array().unwrap() {
            assert_eq!(process["state"], "waiting", "{process}");
            assert!(
                process["detail"]
                    .as_str()
                    .unwrap()
                    .contains("cannot be tied to this process"),
                "it is said that the agent is receiving, without crediting this process: {process}"
            );
        }
    }

    #[test]
    fn a_command_that_outlives_its_deadline_is_abandoned() {
        let started = std::time::Instant::now();
        let mut slow = Command::new("sleep");
        slow.arg("5");
        assert_eq!(output_within(slow, Duration::from_millis(200)), None);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );

        let mut quick = Command::new("echo");
        quick.arg("hi");
        assert_eq!(
            output_within(quick, Duration::from_secs(5)),
            Some(b"hi\n".to_vec())
        );
        assert_eq!(
            output_within(
                Command::new("/nonexistent/af-test-tool"),
                Duration::from_secs(1)
            ),
            None
        );
    }

    #[test]
    fn claude_settings_that_export_in_other_valid_ways_are_configured() {
        // Telemetry enabled as "true" rather than "1".
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let local = project.join(".claude/settings.local.json");
        std::fs::write(&local, r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"true"}}"#).unwrap();
        assert!(
            matches!(
                claude_config_for(&home, Some(&project), ENDPOINT).0,
                UserConfig::Configured { .. }
            ),
            "\"true\" enables telemetry"
        );
        // A higher scope switching hooks back on.
        std::fs::write(home.join(".claude/settings.local-test.json"), "{}").unwrap();
        std::fs::write(
            project.join(".claude/settings.json"),
            r#"{"disableAllHooks":true}"#,
        )
        .unwrap();
        std::fs::write(&local, r#"{"disableAllHooks":false}"#).unwrap();
        assert!(
            matches!(
                claude_config_for(&home, Some(&project), ENDPOINT).0,
                UserConfig::Configured { .. }
            ),
            "the last scope to set disableAllHooks has the last word"
        );
    }

    #[test]
    fn a_logs_only_endpoint_counts_and_a_wrong_one_overrides() {
        let logs_only = json!({
            "env": {
                "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
                "OTEL_LOGS_EXPORTER": "otlp",
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": ENDPOINT,
            },
            "hooks": { "PostToolUse": [{ "hooks": [{ "type": "command", "command": GOOD_HOOK }] }] }
        })
        .to_string();
        let (_dir, home, project) = claude_scopes_fixture(Some(&logs_only));
        assert!(matches!(
            claude_config_for(&home, None, ENDPOINT).0,
            UserConfig::Configured { .. }
        ));
        // The logs-specific variable wins over the general one.
        std::fs::write(
            project.join(".claude/settings.json"),
            r#"{"env":{"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"http://elsewhere:4318/v1/logs"}}"#,
        )
        .unwrap();
        let (config, _) = claude_config_for(&home, Some(&project), ENDPOINT);
        let UserConfig::Misconfigured { detail, .. } = config else {
            panic!("expected misconfigured, got {config:?}");
        };
        assert!(detail.contains("http://elsewhere:4318/v1/logs"), "{detail}");
    }

    #[test]
    fn logs_specific_overrides_of_a_good_base_config_are_misconfigured() {
        // The base settings export here; one logs-specific variable sends
        // the logs — the only signal this receiver takes — somewhere else,
        // or in a form it cannot read.
        for (key, value, expected) in [
            (
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                "http://collector.internal:4318/v1/logs",
                "collector.internal",
            ),
            ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "grpc", "grpc"),
            (
                "OTEL_EXPORTER_OTLP_PROTOCOL",
                "http/protobuf",
                "http/protobuf",
            ),
            ("OTEL_LOGS_EXPORTER", "console", "console"),
        ] {
            let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
                "http://127.0.0.1:4318",
                Some(GOOD_HOOK),
            )));
            let local = project.join(".claude/settings.local.json");
            std::fs::write(&local, json!({ "env": { key: value } }).to_string()).unwrap();
            let (config, _) = claude_config_for(&home, Some(&project), ENDPOINT);
            let UserConfig::Misconfigured { path, detail } = config else {
                panic!("{key}={value}: expected misconfigured, got {config:?}");
            };
            assert_eq!(path, local, "{key}");
            assert!(detail.contains(expected), "{key}: {detail}");
        }
        // The logs-specific protocol has the last word over the general one.
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        std::fs::write(
            project.join(".claude/settings.local.json"),
            r#"{"env":{"OTEL_EXPORTER_OTLP_PROTOCOL":"grpc","OTEL_EXPORTER_OTLP_LOGS_PROTOCOL":"http/json"}}"#,
        )
        .unwrap();
        assert!(matches!(
            claude_config_for(&home, Some(&project), ENDPOINT).0,
            UserConfig::Configured { .. }
        ));
    }

    #[test]
    fn a_claude_config_dir_replaces_the_user_scope() {
        let (dir, home, project) = claude_scopes_fixture(None);
        let other = dir.path().join("claude-work");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("settings.json"),
            claude_settings("http://127.0.0.1:4318", Some(GOOD_HOOK)),
        )
        .unwrap();
        let processes = [claude_process(1, later())];
        let inspect = |config_dir: Option<&Path>| {
            let inspections = BTreeMap::from([(
                1,
                Inspection {
                    cwd: Some(project.clone()),
                    claude_config_dir: config_dir.map(Path::to_path_buf),
                    ..Inspection::default()
                },
            )]);
            let facts = detect_from(
                &home,
                ENDPOINT,
                Some(&processes),
                &inspections,
                &BTreeMap::new(),
            );
            row_of(&facts, AgentId::ClaudeCode, None)["processes"][0]["state"].clone()
        };
        assert_eq!(inspect(None), "not_configured");
        assert_eq!(inspect(Some(&other)), "waiting");
    }

    #[test]
    fn a_setup_command_survives_an_awkward_directory_name() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("it's $HOME (x)");
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        let processes = [codex_process(7, later())];
        let facts = detect_from(
            dir.path(),
            ENDPOINT,
            Some(&processes),
            &BTreeMap::from([(7, codex_in(&project, "s"))]),
            &BTreeMap::new(),
        );
        let row = row_of(&facts, AgentId::Codex, None);
        assert_eq!(
            row["processes"][0]["action"],
            format!(
                "af setup --agents codex --project {}",
                shell_path(&project, cfg!(windows))
            )
        );
        // Each shell's own way of writing a quote inside quotes.
        let awkward = Path::new("it's $HOME (x)");
        assert_eq!(shell_path(awkward, false), r"'it'\''s $HOME (x)'");
        assert_eq!(shell_path(awkward, true), "'it''s $HOME (x)'");
        assert_eq!(
            shell_path(Path::new(r"C:\Users\ann\work"), true),
            r"C:\Users\ann\work"
        );
        assert_eq!(
            shell_path(Path::new("/home/ann/work"), false),
            "/home/ann/work"
        );
    }

    #[test]
    fn a_misconfigured_process_names_the_file_and_offers_no_command_setup_cannot_honour() {
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let local = project.join(".claude").join("settings.local.json");
        std::fs::write(&local, r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#).unwrap();
        let processes = [claude_process(1, later())];
        let inspections = BTreeMap::from([(
            1,
            Inspection {
                cwd: Some(project.clone()),
                ..Inspection::default()
            },
        )]);
        let facts = detect_from(
            &home,
            ENDPOINT,
            Some(&processes),
            &inspections,
            &BTreeMap::new(),
        );
        let process = &row_of(&facts, AgentId::ClaudeCode, None)["processes"][0];
        assert_eq!(process["state"], "misconfigured");
        assert_eq!(process["action"], Value::Null);
        assert!(
            process["detail"]
                .as_str()
                .unwrap()
                .starts_with(&local.display().to_string()),
            "{process}"
        );
    }

    #[test]
    fn only_what_can_have_changed_is_inspected_again() {
        let processes = [
            claude_process(1, at(1)),
            claude_process(2, at(1)),
            codex_process(3, at(1)),
            claude_process(4, at(1)),
        ];
        let known = Inspection {
            cwd: Some(PathBuf::from("/p")),
            ..Inspection::default()
        };
        let mut cache = BTreeMap::from([
            (1, known.clone()),                   // a Claude whose directory is known: settled
            (2, Inspection::default()),           // looked at too early: try again
            (3, codex_in(Path::new("/p"), "s1")), // a Codex may have moved to a new session
            (99, known.clone()),                  // gone
        ]);
        assert_eq!(pids_to_inspect(&processes, &mut cache), [2, 3, 4]);
        assert!(
            !cache.contains_key(&99),
            "a process that is gone is forgotten"
        );
        assert!(cache.contains_key(&1));
    }

    fn codex_process(pid: u32, started: SystemTime) -> AgentProcess {
        AgentProcess {
            pid,
            name: "codex".into(),
            started,
        }
    }

    /// A Codex process running in `project` with `<project>/.codex` as its
    /// home, writing session `session`.
    fn codex_in(project: &Path, session: &str) -> Inspection {
        Inspection {
            cwd: Some(project.to_path_buf()),
            codex_home: Some(project.join(".codex")),
            codex_sessions: vec![session.to_string()],
            claude_config_dir: None,
        }
    }

    fn row_of(facts: &[AgentFacts], agent: AgentId, activity: Option<&Activity>) -> Value {
        let facts = facts.iter().find(|facts| facts.agent == agent).unwrap();
        agent_row(facts, activity)
    }

    fn sessions(entries: &[(&str, i64)]) -> Activity {
        Activity {
            sessions: entries
                .iter()
                .map(|(id, at)| (id.to_string(), *at))
                .collect(),
            last_event_at_ms: entries.iter().map(|(_, at)| *at).max(),
        }
    }

    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(3600)
    }

    #[test]
    fn a_codex_with_an_unconfigured_project_home_is_not_configured_with_its_project_command() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        // The user's default home is configured: irrelevant to this process.
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex/config.toml"),
            setup::append_codex_otel("", ENDPOINT),
        )
        .unwrap();
        let processes = [codex_process(7, later())];
        let inspections = BTreeMap::from([(7, codex_in(&project, "s9"))]);
        let facts = detect_from(
            &home,
            ENDPOINT,
            Some(&processes),
            &inspections,
            &BTreeMap::new(),
        );
        let row = row_of(&facts, AgentId::Codex, None);
        let process = &row["processes"][0];
        assert_eq!(process["state"], "not_configured", "{process}");
        assert_eq!(process["cwd"], project.display().to_string());
        assert_eq!(
            process["config_path"],
            project
                .join(".codex")
                .join("config.toml")
                .display()
                .to_string()
        );
        assert_eq!(
            process["action"],
            format!(
                "af setup --agents codex --project {}",
                shell_path(&project, cfg!(windows))
            )
        );
        assert_eq!(process["session_id"], "s9");
        assert_eq!(
            row["state"], "not_configured",
            "the agent's state is its process's, not the unused global file's"
        );
    }

    #[test]
    fn an_uninspectable_process_is_unknown_never_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let processes = [codex_process(7, later())];
        let facts = detect_from(
            dir.path(),
            ENDPOINT,
            Some(&processes),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let row = row_of(&facts, AgentId::Codex, None);
        let process = &row["processes"][0];
        assert_eq!(process["state"], "unknown");
        assert_eq!(process["action"], Value::Null);
        assert_eq!(process["cwd"], Value::Null);
        assert!(
            ["not_configured", "not_installed"].contains(&row["state"].as_str().unwrap()),
            "with nothing inspected the agent falls back to the global file: {row}"
        );
    }

    #[test]
    fn two_processes_one_exporting_one_not() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let processes = [codex_process(7, later()), codex_process(8, later())];
        let inspections = BTreeMap::from([(7, codex_in(&a, "s-a")), (8, codex_in(&b, "s-b"))]);
        let facts = detect_from(
            dir.path(),
            ENDPOINT,
            Some(&processes),
            &inspections,
            &BTreeMap::new(),
        );
        let row = row_of(&facts, AgentId::Codex, Some(&sessions(&[("s-a", 5_000)])));
        assert_eq!(row["state"], "receiving");
        assert_eq!(row["processes"][0]["state"], "receiving");
        assert_eq!(
            row["processes"][0]["last_event_at"],
            af_core::rfc3339_ms(5_000).unwrap()
        );
        assert_eq!(row["processes"][1]["state"], "not_configured");
        assert_eq!(row["processes"][1]["last_event_at"], Value::Null);
    }

    #[test]
    fn a_claude_process_is_linked_to_its_session_by_the_hooks_pid() {
        let (_dir, home, project) = claude_scopes_fixture(None);
        let processes = [claude_process(11, later())];
        let inspections = BTreeMap::from([(
            11,
            Inspection {
                cwd: Some(project.clone()),
                ..Inspection::default()
            },
        )]);
        let links = BTreeMap::from([(11, "c1".to_string())]);
        let facts = detect_from(&home, ENDPOINT, Some(&processes), &inspections, &links);
        let quiet = row_of(&facts, AgentId::ClaudeCode, None);
        assert_eq!(quiet["processes"][0]["state"], "not_configured");
        assert_eq!(
            quiet["processes"][0]["action"],
            format!(
                "af setup --agents claude-code --project {}",
                shell_path(&project, cfg!(windows))
            )
        );
        let row = row_of(
            &facts,
            AgentId::ClaudeCode,
            Some(&sessions(&[("c1", 9_000)])),
        );
        assert_eq!(row["processes"][0]["state"], "receiving");
        assert_eq!(row["processes"][0]["session_id"], "c1");
    }

    #[test]
    fn agent_state_takes_the_most_actionable_process_state() {
        let (_dir, home, project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let elsewhere = home.join("elsewhere");
        std::fs::write(
            project.join(".claude/settings.local.json"),
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        )
        .unwrap();
        let at_cwd = |cwd: &Path| Inspection {
            cwd: Some(cwd.to_path_buf()),
            ..Inspection::default()
        };
        let inspections = BTreeMap::from([
            (1, at_cwd(&elsewhere)),
            (2, at_cwd(&project)),
            (3, at_cwd(&elsewhere)),
        ]);
        let states = |processes: &[AgentProcess]| {
            let facts = detect_from(
                &home,
                ENDPOINT,
                Some(processes),
                &inspections,
                &BTreeMap::new(),
            );
            let row = row_of(&facts, AgentId::ClaudeCode, None);
            let each: Vec<String> = row["processes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|process| process["state"].as_str().unwrap().to_string())
                .collect();
            (row["state"].as_str().unwrap().to_string(), each)
        };
        assert_eq!(
            states(&[claude_process(1, later()), claude_process(2, later())]),
            (
                "misconfigured".to_string(),
                vec!["waiting".to_string(), "misconfigured".to_string()]
            )
        );
        // Started long before its settings were written: restart outranks.
        assert_eq!(
            states(&[claude_process(2, later()), claude_process(3, at(1))]).0,
            "needs_restart"
        );
    }

    #[test]
    fn a_waiting_process_with_no_session_link_takes_events_nothing_else_explains() {
        let (_dir, home, _project) = claude_scopes_fixture(Some(&claude_settings(
            "http://127.0.0.1:4318",
            Some(GOOD_HOOK),
        )));
        let processes = [claude_process(1, later())];
        let inspections = BTreeMap::from([(
            1,
            Inspection {
                cwd: Some(home.clone()),
                ..Inspection::default()
            },
        )]);
        let facts = detect_from(
            &home,
            ENDPOINT,
            Some(&processes),
            &inspections,
            &BTreeMap::new(),
        );
        let row = row_of(
            &facts,
            AgentId::ClaudeCode,
            Some(&sessions(&[("orphan", 4_000)])),
        );
        assert_eq!(
            row["processes"][0]["state"], "receiving",
            "never 'nothing received' while the agent's events have no other source"
        );
    }

    fn facts(installed: bool, config: UserConfig, stale: Option<bool>) -> AgentFacts {
        AgentFacts {
            agent: AgentId::ClaudeCode,
            installed,
            config,
            processes: stale.map(|stale| vec![ProcessFact::listed(6363, at(10), stale)]),
        }
    }

    fn active() -> Activity {
        Activity {
            sessions: BTreeMap::from([("s1".to_string(), 1), ("s2".to_string(), 2)]),
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
    fn pi_is_recognised_from_its_spool_and_configured_by_its_extension() {
        assert_eq!(agent_for_collector("pi"), Some(AgentId::Pi));
        assert_eq!(AgentId::Pi.name(), "Pi");

        let dir = tempfile::tempdir().unwrap();
        let extension = dir.path().join("extensions").join("agentic-footprint.ts");
        let pi = |installed| AgentFacts {
            agent: AgentId::Pi,
            installed,
            config: inspect_pi_extension(&extension),
            processes: None,
        };

        // Installed, extension missing: af setup installs it.
        let row = agent_row(&pi(true), None);
        assert_eq!(row["state"], "not_configured");
        assert_eq!(row["action"], "af setup");
        assert_eq!(agent_row(&pi(false), None)["state"], "not_installed");

        // This build's extension: waiting for the first Pi session.
        std::fs::create_dir_all(extension.parent().unwrap()).unwrap();
        std::fs::write(&extension, setup::PI_EXTENSION).unwrap();
        assert_eq!(agent_row(&pi(true), None)["state"], "waiting");
        assert_eq!(agent_row(&pi(true), Some(&active()))["state"], "receiving");

        // A copy from another af version: refresh it with af setup.
        std::fs::write(&extension, "// older collector\n").unwrap();
        let row = agent_row(&pi(true), None);
        assert_eq!(row["state"], "misconfigured");
        assert_eq!(row["action"], "af setup");
    }

    #[test]
    fn every_agent_is_listed_once_with_distinct_ids() {
        let ids: std::collections::BTreeSet<&str> =
            AgentId::all().into_iter().map(AgentId::id).collect();
        assert_eq!(ids.len(), AgentId::all().len());
        assert!(ids.contains("claude-code") && ids.contains("codex"));
    }
}
