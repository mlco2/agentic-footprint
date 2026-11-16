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

use std::collections::BTreeSet;
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
        "claude-code" | "otlp-cc" => Some(AgentId::ClaudeCode),
        "codex" => Some(AgentId::Codex),
        #[cfg(feature = "experimental-opencode")]
        "opencode" => Some(AgentId::OpenCode),
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
    let contents = match read_config(path) {
        Ok(Some(contents)) => contents,
        Ok(None) => return UserConfig::Absent,
        Err(unreadable) => return unreadable,
    };
    let misconfigured = |detail: String| UserConfig::Misconfigured {
        path: path.to_path_buf(),
        detail,
    };
    let settings: Value = match serde_json::from_str(&contents) {
        Ok(settings) => settings,
        Err(error) => return misconfigured(format!("not valid JSON: {error}")),
    };
    if !setup::claude_settings_set_otel(&settings) {
        return UserConfig::Absent;
    }
    // What `af setup` writes: the base endpoint, without `/v1/logs`.
    let expected = logs_endpoint
        .strip_suffix("/v1/logs")
        .unwrap_or(logs_endpoint);
    let exported = settings
        .pointer("/env/OTEL_EXPORTER_OTLP_ENDPOINT")
        .and_then(Value::as_str);
    if exported != Some(expected) {
        return misconfigured(match exported {
            Some(other) => format!("exports telemetry to {other}, not to {expected}"),
            None => format!("sets OTEL variables but no endpoint; expected {expected}"),
        });
    }
    let has_af_hook = settings
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
    if !has_af_hook {
        return misconfigured("exports telemetry but the af hook is not registered".to_string());
    }
    UserConfig::Configured {
        path: path.to_path_buf(),
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

/// What a watcher has received from one agent since it started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Activity {
    pub(crate) sessions: BTreeSet<String>,
    pub(crate) last_event_at_ms: Option<i64>,
}

fn system_time_rfc3339(time: SystemTime) -> Option<String> {
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
