//! `af status`: what is installed and running here, as versioned JSON for
//! other programs (CodeCarbon Desktop first).
//!
//! Read-only. It asks the same questions `af service status`, `af doctor`
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
    // program on the port, an older af, nothing at all.
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
        "af_version": env!("CARGO_PKG_VERSION"),
        "binary": std::env::current_exe().ok(),
        "state_dir": state_dir,
        // Whether `af self update` manages this binary (a release install).
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
        "agents": agent_rows,
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
    let bound = af_core::rfc3339_ms(from_ms - RECEIVING_WINDOW_MS).unwrap_or_default();
    let rows = db
        .is_file()
        .then(|| af_store::Store::open_read_only(&db).ok())
        .flatten()
        .and_then(|store| store.sessions_with_events_since(&bound).ok())
        .unwrap_or_default();
    for (collector, session, ts) in rows {
        let (Some(agent), Some(at)) = (
            agents::agent_for_collector(&collector),
            af_core::parse_ts(&ts),
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
    let Err(finding) = af_sidecar::runtime_status(state_dir) else {
        return json!({"ready": true, "problem": Value::Null, "fix": Value::Null,
            "provisioner": Value::Null, "uv_install_url": af_sidecar::UV_INSTALL_URL});
    };
    // What `af python setup` would use here; with neither, installing uv
    // is the one step that provides everything.
    let provisioner = af_sidecar::provisioner();
    let fix = match provisioner {
        Some(_) => finding.fix_hint,
        None => format!(
            "install uv ({}), which also provides Python, then run `af python setup`",
            af_sidecar::UV_INSTALL_URL
        ),
    };
    json!({
        "ready": false,
        "problem": finding.message,
        "fix": fix,
        "provisioner": provisioner,
        "uv_install_url": af_sidecar::UV_INSTALL_URL,
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
        assert_eq!(status["af_version"], env!("CARGO_PKG_VERSION"));
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
            .contains("af python setup"));
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

    fn event(id: &str, session: &str, at_ms: i64, collector: &str) -> af_events::Envelope {
        use af_events::{fixtures, Collector, Payload};
        let mut event = fixtures::envelope(
            id,
            session,
            &af_core::rfc3339_ms(at_ms).expect("ts"),
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
        let mut store = af_store::Store::open(&dir.path().join("state.db")).expect("open");
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
            af_core::rfc3339_ms(NOON - hour).unwrap()
        );
        let codex = agent("codex");
        assert_ne!(codex["state"], "receiving");
        assert_eq!(codex["sessions"], 0);
        assert!(codex["last_event_at"].is_null());
    }
}
