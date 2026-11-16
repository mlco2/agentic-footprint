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
    let endpoint =
        service::installed_endpoint().unwrap_or_else(|| super::DEFAULT_LOGS_ENDPOINT.to_string());
    // Why the receiver does not answer is for a person to read: another
    // program on the port, an older af, nothing at all.
    let reachable = service::receiver_reachable(&endpoint);
    let agent_rows: Vec<Value> = match home {
        Some(home) => agents::detect_live(home, &endpoint, &mut BTreeMap::new(), &BTreeMap::new())
            .iter()
            .map(|facts| agents::agent_row_in(facts, None, home))
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
        "agents": agent_rows,
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
}
