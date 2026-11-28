//! `afoot status`: what is installed and running here, as versioned JSON for
//! other programs (CodeCarbon Desktop first).
//!
//! Read-only. It asks the same questions `afoot service status`, `afoot doctor`
//! and the console's status do, and answers them as data: nothing is
//! started, installed or written.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use super::{agents, debug_server, self_update, service};

/// Bumped only when a consumer written against the previous shape would
/// misread this one.
pub const PROTOCOL_VERSION: u32 = 1;

/// Prints the status document.
pub fn run(state_dir: &Path) -> Result<i32> {
    let status = build(state_dir, crate::paths::home_dir().as_deref());
    println!("{}", serde_json::to_string(&status)?);
    Ok(0)
}

/// The status document. `home` is where agents keep their user
/// configuration; without one, agents are reported from events alone.
pub fn build(state_dir: &Path, home: Option<&Path>) -> Value {
    build_at(state_dir, home, super::now_ms())
}

fn build_at(state_dir: &Path, home: Option<&Path>, now_ms: i64) -> Value {
    let endpoint =
        service::installed_endpoint().unwrap_or_else(|| super::DEFAULT_LOGS_ENDPOINT.to_string());
    // Why the receiver does not answer is for a person to read: another
    // program on the port, an older afoot, nothing at all.
    let reachable = service::receiver_reachable(&endpoint);
    let heard = recent_activity(state_dir, now_ms);
    let agent_rows: Vec<Value> = match home {
        Some(home) => agents::detect_live(home, &endpoint, &mut BTreeMap::new(), &BTreeMap::new())
            .iter()
            .map(|facts| agents::agent_row_in(facts, heard.get(&facts.agent), home))
            .collect(),
        None => Vec::new(),
    };
    json!({
        "protocol_version": PROTOCOL_VERSION,
        "afoot_version": env!("CARGO_PKG_VERSION"),
        "binary": std::env::current_exe().ok(),
        "state_dir": state_dir,
        // Whether `afoot self update` manages this binary (a release install).
        "self_update": {"managed": self_update::manages_this_binary(state_dir)},
        "service": {
            "supported": service::automatic_manager_available(),
            "installed": service::installed(),
        },
        "receiver": {
            "endpoint": endpoint,
            "reachable": reachable.is_ok(),
            "note": reachable.err().map(|err| format!("{err:#}")),
        },
        "collector": {
            "running": cfg!(unix).then(|| {
                debug_server::socket_is_served(&state_dir.join(debug_server::SOCKET_NAME))
            }),
        },
        "python": python_runtime(state_dir),
        "storage": storage(state_dir),
        "agents": agent_rows,
    })
}

/// What the recorded history takes on disk. `afoot` keeps all of it (there is
/// no retention yet), so the sizes are what a person decides from. The
/// Python runtime is not counted: it is not history, and measuring it means
/// walking thousands of files.
fn storage(state_dir: &Path) -> Value {
    // The files of one directory, not its subdirectories: (bytes, count).
    let files_in = |dir: &Path, wanted: &dyn Fn(&str) -> bool| -> (u64, u64) {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| wanted(&entry.file_name().to_string_lossy()))
            .filter_map(|entry| entry.metadata().ok())
            .filter(|meta| meta.is_file())
            .fold((0, 0), |(bytes, count), meta| {
                (bytes + meta.len(), count + 1)
            })
    };
    // The database and its write-ahead files.
    let (database, _) = files_in(state_dir, &|name| name.starts_with("state.db"));
    let (spool, spool_files) = files_in(&state_dir.join("spool"), &|_| true);
    let (logs, _) = files_in(&state_dir.join("logs"), &|_| true);
    json!({
        "database_bytes": database,
        "spool_bytes": spool,
        "spool_files": spool_files,
        "logs_bytes": logs,
    })
}

/// An agent heard from within this long is `receiving`.
const RECEIVING_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// What the store holds from each agent over the last
/// [`RECEIVING_WINDOW_MS`]: its sessions and the time of its last event.
/// The collector has this in memory; a status asked of the store cannot
/// know when the collector started, hence the window. Read-only; a missing
/// or unreadable store is no activity.
fn recent_activity(state_dir: &Path, now_ms: i64) -> BTreeMap<agents::AgentId, agents::Activity> {
    let mut heard: BTreeMap<agents::AgentId, agents::Activity> = BTreeMap::new();
    let db = state_dir.join("state.db");
    let from_ms = now_ms - RECEIVING_WINDOW_MS;
    // A day's margin as text, then the instants compared parsed.
    let bound = afoot_core::rfc3339_ms(from_ms - RECEIVING_WINDOW_MS).unwrap_or_default();
    let rows = db
        .is_file()
        .then(|| afoot_store::Store::open_read_only(&db).ok())
        .flatten()
        .and_then(|store| store.sessions_with_events_since(&bound).ok())
        .unwrap_or_default();
    for (collector, session, ts) in rows {
        let (Some(agent), Some(at)) = (
            agents::agent_for_collector(&collector),
            afoot_core::parse_ts(&ts),
        ) else {
            continue;
        };
        if !(from_ms..=now_ms).contains(&at) {
            continue;
        }
        let activity = heard.entry(agent).or_default();
        let last = activity.sessions.entry(session).or_insert(at);
        *last = (*last).max(at);
        activity.last_event_at_ms = activity.last_event_at_ms.max(Some(at));
    }
    heard
}

/// The managed Python runtime the impact estimates need. Without it tokens
/// are still recorded; there are no impact figures.
fn python_runtime(state_dir: &Path) -> Value {
    let Err(finding) = afoot_sidecar::runtime_status(state_dir) else {
        return json!({"ready": true, "problem": Value::Null, "fix": Value::Null,
            "provisioner": Value::Null, "uv_install_url": afoot_sidecar::UV_INSTALL_URL});
    };
    // What `afoot python setup` would use here; with neither, installing uv
    // is the one step that provides everything.
    let provisioner = afoot_sidecar::provisioner();
    let fix = match provisioner {
        Some(_) => finding.fix_hint,
        None => format!(
            "install uv ({}), which also provides Python, then run `afoot python setup`",
            afoot_sidecar::UV_INSTALL_URL
        ),
    };
    json!({
        // A warning (a dependency to upgrade) leaves the runtime usable.
        "ready": finding.severity != afoot_sidecar::Severity::Error,
        "problem": finding.message,
        "fix": fix,
        "provisioner": provisioner,
        "uv_install_url": afoot_sidecar::UV_INSTALL_URL,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_every_agent_and_answers_each_question_as_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = build(dir.path(), Some(&dir.path().join("home")));
        assert_eq!(status["protocol_version"], 1);
        assert_eq!(status["afoot_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["self_update"]["managed"], false, "no receipt here");
        assert!(status["service"]["installed"].is_boolean());
        assert!(status["receiver"]["reachable"].is_boolean());
        assert!(status["receiver"]["endpoint"]
            .as_str()
            .unwrap()
            .starts_with("http://"));
        if cfg!(unix) {
            assert_eq!(
                status["collector"]["running"], false,
                "no socket in an empty dir"
            );
        }
        // No runtime in an empty state directory: said, with what to do.
        assert_eq!(status["python"]["ready"], false);
        assert!(status["python"]["problem"]
            .as_str()
            .unwrap()
            .contains("venv"));
        assert!(status["python"]["fix"]
            .as_str()
            .unwrap()
            .contains("afoot python setup"));
        assert!(status["python"]["uv_install_url"]
            .as_str()
            .unwrap()
            .starts_with("https://docs.astral.sh/uv"));
        let ids: Vec<&str> = status["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&"claude-code") && ids.contains(&"codex"),
            "{ids:?}"
        );
        for row in status["agents"].as_array().unwrap() {
            assert!(row["state"].is_string(), "{row}");
        }
    }

    #[test]
    fn without_a_home_directory_no_agent_is_guessed() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(build(dir.path(), None)["agents"], json!([]));
    }
    /// 2026-11-16T12:00:00Z.
    const NOON: i64 = 1_794_830_400_000;

    fn event(id: &str, session: &str, at_ms: i64, collector: &str) -> afoot_events::Envelope {
        use afoot_events::{fixtures, Collector, Payload};
        let mut event = fixtures::envelope(
            id,
            session,
            &afoot_core::rfc3339_ms(at_ms).expect("ts"),
            Payload::LlmCall(fixtures::llm_call()),
        );
        event.collector = Collector {
            name: collector.to_string(),
            version: "1".to_string(),
        };
        event
    }

    #[test]
    fn an_agent_heard_from_in_the_last_day_is_receiving() {
        let hour = 3_600_000;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = afoot_store::Store::open(&dir.path().join("state.db")).expect("open");
        store
            .insert_events(&[
                event("e1", "s1", NOON - 3 * hour, "otlp-cc"),
                event("e2", "s1", NOON - hour, "otlp-cc"),
                event("e3", "s2", NOON - 2 * hour, "cc-hooks"),
                // More than a day ago: codex is not receiving now.
                event("e4", "s3", NOON - 25 * hour, "codex"),
            ])
            .expect("insert");
        drop(store);

        let status = build_at(dir.path(), Some(&dir.path().join("home")), NOON);
        let agent = |id: &str| {
            status["agents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|agent| agent["id"] == id)
                .unwrap_or_else(|| panic!("no {id}"))
                .clone()
        };
        let claude = agent("claude-code");
        assert_eq!(claude["state"], "receiving");
        assert_eq!(claude["sessions"], 2, "both of its collectors count");
        assert_eq!(
            claude["last_event_at"],
            afoot_core::rfc3339_ms(NOON - hour).unwrap()
        );
        let codex = agent("codex");
        assert_ne!(codex["state"], "receiving");
        assert_eq!(codex["sessions"], 0);
        assert!(codex["last_event_at"].is_null());
    }
    #[test]
    fn storage_reports_what_the_database_the_spool_and_the_logs_take() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = build(dir.path(), None);
        assert_eq!(
            empty["storage"],
            json!({"database_bytes": 0, "spool_bytes": 0, "spool_files": 0, "logs_bytes": 0})
        );

        std::fs::write(dir.path().join("state.db"), [0u8; 100]).unwrap();
        std::fs::write(dir.path().join("state.db-wal"), [0u8; 20]).unwrap();
        std::fs::create_dir_all(dir.path().join("spool")).unwrap();
        std::fs::write(dir.path().join("spool").join("a.s1.jsonl"), [0u8; 7]).unwrap();
        std::fs::write(dir.path().join("spool").join("a.s2.jsonl"), [0u8; 5]).unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        std::fs::write(dir.path().join("logs").join("watch.log"), [0u8; 3]).unwrap();
        // Not the database: only its own files count.
        std::fs::write(dir.path().join("install-receipt.json"), [0u8; 50]).unwrap();

        assert_eq!(
            storage(dir.path()),
            json!({"database_bytes": 120, "spool_bytes": 12, "spool_files": 2, "logs_bytes": 3})
        );
    }
}
