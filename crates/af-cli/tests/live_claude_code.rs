//! Live end-to-end suite for the Claude Code integration: a real
//! `claude -p` session, hooks + OTLP collectors wired to a real
//! `af watch --debug`, assertions against the `/debug` contract — the
//! live Claude Code coverage through the current debug contract.
//!
//! Every test is `#[ignore]`d: they spend real tokens, need `claude`
//! installed and logged in, and take minutes. Run them with
//! `scripts/test-live.sh`, or directly:
//!
//! ```sh
//! cargo test -p af-cli --test live_claude_code -- --ignored --nocapture
//! ```
//!
//! Knobs: `AF_LIVE_MODEL` (default `haiku`), `AF_LIVE_TIMEOUT_SECS`
//! (default 300).

mod common;

use std::time::Duration;

use common::live::{spooled_session_id, state_dir, wait_until, ClaudeCode, LiveWatch};
use serde_json::Value;

/// What the agent is asked to do — two tool calls, one `Bash` and one
/// `Read`, which is the least work that exercises span collection.
const PROMPT: &str = "run: echo hello, then read README.md";

/// How long after the agent exits the collectors get to land everything:
/// hook spool writes are immediate, but the OTLP exporter flush can trail
/// the process exit.
const SETTLE: Duration = Duration::from_secs(90);

/// Does `events` hold an event of `type_` for `sid` satisfying `pred`?
fn has_event(events: &[Value], sid: &str, type_: &str, pred: impl Fn(&Value) -> bool) -> bool {
    events.iter().any(|event| {
        event["session_id"] == sid && event["type"] == type_ && pred(&event["payload"])
    })
}

/// The named collector's health row, if present.
fn collector<'a>(health: &'a Value, name: &str) -> Option<&'a Value> {
    health["collectors"]
        .as_array()?
        .iter()
        .find(|row| row["name"] == name)
}

/// Token + span flow with no Python at all (`--no-sidecars`): the fresh
/// session's hooks and OTEL exports must both reach the debug console, and
/// nothing may be rejected. Estimates staying pending without the estimator
/// is the documented honest degradation, so the report is only asserted to
/// exist.
#[test]
#[ignore = "live: spawns a real Claude Code session (tokens, network) — run scripts/test-live.sh"]
fn smoke_fresh_session_reaches_debug_console() {
    let agent = ClaudeCode::preflight();
    let dir = state_dir();
    let watch = LiveWatch::start(dir.path(), &["--no-sidecars"]);

    let output = agent.run_session(dir.path(), watch.otlp_addr, PROMPT);
    assert!(!output.trim().is_empty(), "claude -p returned no output");

    // The hook shim names its spool file after the session id — that id is
    // what every following assertion keys on.
    let sid = wait_until(SETTLE, "cc-hooks spool file", || {
        spooled_session_id(dir.path(), "cc-hooks")
    });
    wait_until(SETTLE, "otlp-cc spool file", || {
        spooled_session_id(dir.path(), "otlp-cc")
    });

    // Both collectors ingested, nothing rejected.
    let health = watch.poll_json("/debug/health", SETTLE, |health| {
        ["cc-hooks", "otlp-cc"].iter().all(|name| {
            collector(health, name)
                .map(|row| row["events"].as_u64().unwrap_or(0) > 0)
                .unwrap_or(false)
        })
    });
    for name in ["cc-hooks", "otlp-cc"] {
        let row = collector(&health, name).expect("collector row");
        assert_eq!(
            row["rejected"], 0,
            "{name} rejected events; health: {health}"
        );
    }

    // The session's tool spans and token usage are queryable.
    watch.poll_json("/debug/snapshot?window=600s", SETTLE, |snapshot| {
        let Some(events) = snapshot["events"].as_array() else {
            return false;
        };
        has_event(events, &sid, "action_span", |payload| {
            payload["tool_name"] == "Bash"
        }) && has_event(events, &sid, "llm_call", |payload| {
            payload["usage"]["output_tokens"].as_u64().unwrap_or(0) > 0
        })
    });

    // Bootstrap surfaces the session, and the session-level join exists —
    // with whatever estimation statuses honesty requires.
    watch.poll_json("/debug/session", SETTLE, |session| {
        session["session_id"] == sid.as_str()
    });
    watch.poll_json("/debug/report?level=session", SETTLE, |report| {
        report["impact_join"].is_object()
    });

    let stderr = watch.terminate();
    assert!(
        stderr.contains("[ingest]"),
        "watch stderr never logged an ingest decision:\n{stderr}"
    );
}

/// The energy path: with the managed venv present, a live session must
/// produce `energy_sample`s and allocation traces. Preconditions (venv,
/// `af python setup`) are asserted with the remedy in the message, not
/// skipped — a live run that silently skips its point is worse than one
/// that says what to install.
#[test]
#[ignore = "live: spawns a real Claude Code session and needs `af python setup` — run scripts/test-live.sh"]
fn energy_sampling_attributes_local_compute() {
    let agent = ClaudeCode::preflight();
    let dir = state_dir();

    // The sampler runs from `state_dir/venv`; the harness state dir is a
    // tempdir, so link the developer's real venv in. Its absence is a
    // stated precondition, same spirit as the missing-CLI panic.
    let home = std::env::var("HOME").expect("HOME");
    let venv = std::path::PathBuf::from(&home).join(".local/state/agentic-footprint/venv");
    assert!(
        venv.is_dir(),
        "no managed venv at {} — run `af python setup` first",
        venv.display()
    );
    std::os::unix::fs::symlink(&venv, dir.path().join("venv")).expect("symlink venv");

    let watch = LiveWatch::start(dir.path(), &[]);
    agent.run_session(dir.path(), watch.otlp_addr, PROMPT);

    let sid = wait_until(SETTLE, "cc-hooks spool file", || {
        spooled_session_id(dir.path(), "cc-hooks")
    });

    // The sampler writes on a ~5s cadence for as long as the session tree
    // lives; one sample and one allocation trace are enough to prove the
    // path.
    watch.poll_json("/debug/snapshot?window=600s", SETTLE, |snapshot| {
        let Some(events) = snapshot["events"].as_array() else {
            return false;
        };
        let sampled = has_event(events, &sid, "energy_sample", |payload| {
            payload["components"]
                .as_array()
                .map(|components| {
                    components.iter().any(|component| {
                        component["kind"] == "total"
                            && component["energy_j"].as_f64().unwrap_or(0.0) > 0.0
                    })
                })
                .unwrap_or(false)
        });
        let allocated = snapshot["allocations"]
            .as_array()
            .map(|allocs| !allocs.is_empty())
            .unwrap_or(false);
        sampled && allocated
    });

    watch.terminate();
}
