//! `af hook`: the Claude Code hooks collector ("cc-hooks") as a built-in
//! subcommand — the native-Windows counterpart of
//! `collectors/claude-code/af-hook.sh`, and a behavioral port of it: same
//! collector name/version, same spool lines, same open-span lifecycle, so
//! the two implementations are interchangeable per platform.
//!
//! One subcommand handles all six hook events (SessionStart, PreToolUse,
//! PostToolUse, PostToolUseFailure, Stop, SessionEnd) — the same command is
//! registered for every event in `.claude/settings.json`, and dispatch
//! happens on the hook payload's own `hook_event_name` field.
//!
//! Contract (shared with `af statusline`): read the hook JSON from stdin,
//! **always exit 0**, degrade silently. Claude Code blocks on this command
//! for every tool call inside a live session; collection is best-effort and
//! the running session is sacred — this command must never be the reason a
//! turn fails. Errors are best-effort appended to
//! `<state_dir>/tmp/hook-errors.log` and otherwise swallowed. It never
//! reads transcripts — only the hook JSON on stdin.
//!
//! Registered as a bare executable command (`"<af> hook"`, no shell
//! wrapper) deliberately: Claude Code then spawns it as a direct child, so
//! this process's parent PID *is* the Claude Code process, and that PID —
//! carried on the SessionStart bootstrap span's `pids` — is what makes
//! process-tree energy attribution work.
//!
//! `session_id` and `tool_use_id` come straight from Claude Code's hook
//! JSON and are used to build file paths (spool filename, open-span
//! filename); both go through [`af_otlp::sanitize_id`] — the same rule the
//! sh hook and `af_sampler` implement, pinned for all of them by
//! `tests/fixtures/sanitize-vectors.json`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::Value;

use af_events::{
    ActionSpan, AgentApp, Attribution, Collector, Envelope, ExecutionLocus, Payload, SessionMeta,
    Status, ToolKind,
};
use af_otlp::sanitize_id;

const SCHEMA_VERSION: &str = "0.1.0";
const COLLECTOR_NAME: &str = "cc-hooks";
const COLLECTOR_VERSION: &str = "0.1.0";

/// Entry point behind `af hook`. Always returns 0 — see the module doc.
pub fn run() -> i32 {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    let Some(state_dir) = crate::paths::state_dir_checked() else {
        return 0;
    };
    let result = handle(&state_dir, &input, super::now_ms());
    if let Err(error) = result {
        log_line(&state_dir, &format!("{error:#}"));
    }
    0
}

/// The testable core: everything [`run`] does apart from reading stdin,
/// sampling the clock, and swallowing the outcome.
///
/// One `now` per hook invocation, shared by every emitted event's envelope
/// `ts` and `t_end` — a hook invocation *is* one instant as far as this
/// collector can honestly claim to know. Formatted by
/// [`af_core::rfc3339_ms`], the same RFC 3339 millisecond shape the sh
/// hook's jq derives and the control plane stamps derived records with.
pub fn handle(state_dir: &Path, input: &str, now_ms: i64) -> Result<()> {
    let hook: Value = serde_json::from_str(input).context("hook stdin is not valid JSON")?;
    let ts = af_core::rfc3339_ms(now_ms).context("formatting the invocation timestamp")?;

    let session_id = sanitize_id(&json_string(hook.get("session_id"), "unknown"));
    let event = json_string(hook.get("hook_event_name"), "");
    let tool_use_id_raw = json_string(hook.get("tool_use_id"), "");
    let tool_name = json_string(hook.get("tool_name"), "");
    let is_interrupt = match hook.get("is_interrupt") {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(s)) => s == "true",
        _ => false,
    };

    // Open-span scratch files are partitioned per session:
    // <state>/tmp/openspans/<SESSION_ID>/<tool_use_id>. The per-session
    // directory is what makes the Stop/SessionEnd sweep safe: a concurrent
    // session's in-flight spans are not visible here and cannot be swept.
    // Safe as a path component because SESSION_ID has been through
    // sanitize_id: it can hold no separator.
    let openspan_dir = state_dir.join("tmp").join("openspans").join(&session_id);

    match event.as_str() {
        "SessionStart" => {
            // session_meta is schema-frozen (no room for a process-id
            // field), so the parent PID — the Claude Code process — travels
            // as `pids` on a zero-length bootstrap action_span instead.
            // Looked up here, not in `run`: the lookup walks a process
            // snapshot on Windows, and every other (per-tool-call, session-
            // blocking) event has no use for it.
            let ppid = parent_pid();
            emit(
                state_dir,
                &session_id,
                &envelope(
                    &ts,
                    now_ms,
                    &session_id,
                    None,
                    Payload::ActionSpan(ActionSpan {
                        span_id: format!("session-boot-{session_id}"),
                        tool_name: "__session__".to_string(),
                        tool_kind: ToolKind::Other,
                        execution_locus: ExecutionLocus::Local,
                        t_start: ts.clone(),
                        t_end: ts.clone(),
                        pids: ppid.map(|pid| vec![i64::from(pid)]),
                        cgroup: None,
                        status: Some(Status::Ok),
                    }),
                ),
            )?;
            // version omitted: not present in any hook payload field;
            // geo_zone omitted: user-configured, not auto-detected here.
            emit(
                state_dir,
                &session_id,
                &envelope(
                    &ts,
                    now_ms,
                    &session_id,
                    None,
                    Payload::SessionMeta(SessionMeta {
                        agent_app: AgentApp {
                            name: "claude-code".to_string(),
                            version: None,
                        },
                        os: Some(std::env::consts::OS.to_string()),
                        hardware: None,
                        geo_zone: None,
                        power_source: None,
                    }),
                ),
            )?;
        }

        "PreToolUse" => {
            // No spool write here — PreToolUse only opens the span. Without
            // a tool_use_id there's no key to open it under, so there's
            // nothing this hook can usefully record.
            if !tool_use_id_raw.is_empty() {
                let tool_use_id = sanitize_id(&tool_use_id_raw);
                std::fs::create_dir_all(&openspan_dir)
                    .with_context(|| format!("creating {}", openspan_dir.display()))?;
                let record = serde_json::json!({"t_start": ts, "tool_name": tool_name});
                std::fs::write(openspan_dir.join(&tool_use_id), record.to_string())
                    .context("writing the open-span record")?;
            }
        }

        // PostToolUseFailure is the *other* half of PostToolUse, not an
        // extra: Claude Code fires exactly one of the two per tool call,
        // and a failing tool call gets ONLY the failure event. Both share
        // the closing logic; only the `status` of an observed span differs.
        "PostToolUse" | "PostToolUseFailure" => {
            if tool_use_id_raw.is_empty() {
                return Ok(());
            }
            let tool_use_id = sanitize_id(&tool_use_id_raw);
            // `is_interrupt: true` is the user/agent cancelling a running
            // tool, which the schema names `cancelled`; every other failure
            // is `error`.
            let status = if event == "PostToolUseFailure" {
                if is_interrupt {
                    Status::Cancelled
                } else {
                    Status::Error
                }
            } else {
                Status::Ok
            };
            let span_file = openspan_dir.join(&tool_use_id);
            let record = read_open_span(&span_file);
            let span = close_span(record.as_ref(), &tool_use_id, &tool_name, status, &ts);
            let event = envelope(
                &ts,
                now_ms,
                &session_id,
                Some(tool_use_id.clone()),
                Payload::ActionSpan(span),
            );
            emit(state_dir, &session_id, &event)?;
            // Removed either way, so a stale file can't be mis-parsed again
            // by a later Stop/SessionEnd sweep.
            let _ = std::fs::remove_file(&span_file);
        }

        "Stop" | "SessionEnd" => {
            // Close any open-spans still on disk **for this session only**:
            // a PreToolUse that never got a matching PostToolUse (the turn
            // was interrupted, Claude Code crashed mid-tool-call, ...).
            let Ok(entries) = std::fs::read_dir(&openspan_dir) else {
                return Ok(());
            };
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect();
            files.sort();
            let mut corrupt_count = 0_u32;
            for span_file in files {
                let span_id = span_file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // Only files whose content actually parses as JSON are
                // treated as open-spans and emitted; anything else is a
                // corrupt/stray file — counted and removed, nothing emitted
                // for it, never crashes the collector.
                match read_open_span(&span_file) {
                    Some(record) => {
                        let span = close_span(Some(&record), &span_id, "", Status::Unknown, &ts);
                        let event = envelope(
                            &ts,
                            now_ms,
                            &session_id,
                            Some(span_id),
                            Payload::ActionSpan(span),
                        );
                        emit(state_dir, &session_id, &event)?;
                    }
                    None => corrupt_count += 1,
                }
                let _ = std::fs::remove_file(&span_file);
            }
            // The sweep emptied this session's directory; remove it so a
            // long-lived state dir doesn't accumulate one empty directory
            // per session ever run. Best-effort and non-recursive: a
            // concurrent PreToolUse's fresh file makes the removal refuse,
            // which is exactly the safety wanted.
            let _ = std::fs::remove_dir(&openspan_dir);
            if corrupt_count > 0 {
                log_line(
                    state_dir,
                    &format!(
                        "warn: Stop/SessionEnd skipped {corrupt_count} corrupt open-span file(s) under {}",
                        openspan_dir.display()
                    ),
                );
            }
        }

        // Unregistered/unknown hook event: nothing to do. Not an error —
        // keeps the collector forward-compatible with hook events it
        // doesn't model yet, rather than failing a Claude Code turn.
        _ => {}
    }
    Ok(())
}

/// One closing `action_span`, for both the PostToolUse/PostToolUseFailure
/// path and the Stop/SessionEnd sweep.
///
/// Never fabricate a start time we didn't observe: with no usable
/// open-span record (hooks enabled mid-session, the file lost, a truncated
/// write) the span collapses to a point at `t_end` and is marked
/// `unknown`, which wins over the known outcome on purpose — the span
/// itself was never observed.
///
/// `tool_name` comes from the hook payload when it has one and from the
/// open-span `record` otherwise, which is what lets the sweep — whose Stop
/// payload names no tool — classify the spans it closes. The record is the
/// caller's one [`read_open_span`] result (`None` for a missing or
/// unparseable file), so each span file is read exactly once.
fn close_span(
    record: Option<&Value>,
    span_id: &str,
    tool_name: &str,
    status: Status,
    t_end: &str,
) -> ActionSpan {
    let observed_start = record
        .and_then(|record| record.get("t_start"))
        .and_then(Value::as_str)
        .filter(|start| !start.is_empty());
    let name = if tool_name.is_empty() {
        record
            .map(|record| json_string(record.get("tool_name"), ""))
            .unwrap_or_default()
    } else {
        tool_name.to_string()
    };
    let (kind, locus) = classify_tool(&name);
    ActionSpan {
        span_id: span_id.to_string(),
        tool_name: name,
        tool_kind: kind,
        execution_locus: locus,
        t_start: observed_start.unwrap_or(t_end).to_string(),
        t_end: t_end.to_string(),
        pids: None,
        cgroup: None,
        status: Some(if observed_start.is_none() {
            Status::Unknown
        } else {
            status
        }),
    }
}

/// The open-span record, if the file exists and parses as JSON.
fn read_open_span(span_file: &Path) -> Option<Value> {
    let contents = std::fs::read_to_string(span_file).ok()?;
    serde_json::from_str(&contents).ok()
}

/// tool_name -> (kind, locus). One definition, used by every emitted
/// `action_span`: the closing PostToolUse knows the name from the hook
/// payload, the Stop/SessionEnd sweep reads it back out of the open-span
/// file, and both must classify it identically.
///
/// `mcp__*` -> locus `unknown`: the tool name alone doesn't reveal whether
/// the MCP server is a local stdio process or a remote HTTP server, and
/// this collector never invents a locus it can't observe.
/// WebFetch/WebSearch are the only tools it can be sure are remote-network
/// by name alone.
fn classify_tool(name: &str) -> (ToolKind, ExecutionLocus) {
    match name {
        "Bash" => (ToolKind::Bash, ExecutionLocus::Local),
        _ if name.starts_with("mcp__") => (ToolKind::Mcp, ExecutionLocus::Unknown),
        "Edit" | "Write" | "Read" | "NotebookEdit" | "Glob" | "Grep" => {
            (ToolKind::FileOp, ExecutionLocus::Local)
        }
        "Task" | "Agent" => (ToolKind::Subagent, ExecutionLocus::Local),
        "WebFetch" | "WebSearch" => (ToolKind::Web, ExecutionLocus::Remote),
        _ => (ToolKind::Other, ExecutionLocus::Unknown),
    }
}

fn envelope(
    ts: &str,
    now_ms: i64,
    session_id: &str,
    tool_call_id: Option<String>,
    payload: Payload,
) -> Envelope {
    Envelope {
        schema_version: SCHEMA_VERSION.to_string(),
        event_id: new_event_id(now_ms),
        ts: ts.to_string(),
        collector: Collector {
            name: COLLECTOR_NAME.to_string(),
            version: COLLECTOR_VERSION.to_string(),
        },
        session_id: session_id.to_string(),
        attribution: tool_call_id.map(|id| Attribution {
            tool_call_id: Some(id),
            ..Attribution::default()
        }),
        payload,
    }
}

/// Appends one Contract #1 event line to this session's spool file. A
/// single write keeps each line complete; collectors never delete spool
/// lines once written.
fn emit(state_dir: &Path, session_id: &str, event: &Envelope) -> Result<()> {
    use std::io::Write;

    let spool_dir = state_dir.join("spool");
    std::fs::create_dir_all(&spool_dir)
        .with_context(|| format!("creating spool dir {}", spool_dir.display()))?;
    let line = serde_json::to_string(event).context("serializing the event")?;
    let path = spool_dir.join(af_spool::spool_file_name(COLLECTOR_NAME, session_id));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening spool file {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// A unique event id, comfortably over the schema's `minLength: 16`
/// without a uuid dependency: the invocation's wall-clock millis + pid +
/// a per-process counter (distinct ids within one invocation) + a randomly
/// seeded hash (distinct across concurrent invocations in the same
/// millisecond).
fn new_event_id(now_ms: i64) -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let random = RandomState::new().build_hasher().finish();
    format!(
        "cc-{now_ms:x}-{pid}-{count}-{random:08x}",
        pid = std::process::id()
    )
}

/// The parent process id — the Claude Code process when this command is
/// registered as a bare executable path (see the module doc).
#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    Some(std::os::unix::process::parent_id())
}

/// Windows has no getppid; walk a Toolhelp process snapshot for our own
/// entry's `th32ParentProcessID`.
#[cfg(windows)]
fn parent_pid() -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    let own_pid = std::process::id();
    // SAFETY: a snapshot handle is closed on every path out; PROCESSENTRY32
    // is a plain data struct with dwSize set before use, as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        let mut found = None;
        if Process32First(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == own_pid {
                    found = Some(entry.th32ParentProcessID);
                    break;
                }
                if Process32Next(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

/// `jq`-compatible string coercion for a hook JSON field:
/// missing/null/false fall back to `default` (jq's `//` treats null and
/// false as absent), strings pass through, and anything else renders as
/// its JSON text (jq's `tostring`).
fn json_string(value: Option<&Value>, default: &str) -> String {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => default.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Best-effort line into the hook error log under `<state_dir>/tmp`;
/// failures to log are swallowed — there is nowhere safer to report them.
fn log_line(state_dir: &Path, message: &str) {
    use std::io::Write;

    let tmp = state_dir.join("tmp");
    if std::fs::create_dir_all(&tmp).is_err() {
        return;
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tmp.join("hook-errors.log"))
    {
        let _ = writeln!(file, "af[claude-hook] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_matches_the_sh_hook_table() {
        assert_eq!(
            classify_tool("Bash"),
            (ToolKind::Bash, ExecutionLocus::Local)
        );
        assert_eq!(
            classify_tool("mcp__github__create_issue"),
            (ToolKind::Mcp, ExecutionLocus::Unknown)
        );
        for name in ["Edit", "Write", "Read", "NotebookEdit", "Glob", "Grep"] {
            assert_eq!(
                classify_tool(name),
                (ToolKind::FileOp, ExecutionLocus::Local)
            );
        }
        for name in ["Task", "Agent"] {
            assert_eq!(
                classify_tool(name),
                (ToolKind::Subagent, ExecutionLocus::Local)
            );
        }
        for name in ["WebFetch", "WebSearch"] {
            assert_eq!(classify_tool(name), (ToolKind::Web, ExecutionLocus::Remote));
        }
        for name in ["", "TodoWrite", "bash"] {
            assert_eq!(
                classify_tool(name),
                (ToolKind::Other, ExecutionLocus::Unknown)
            );
        }
    }

    #[test]
    fn event_ids_are_long_enough_and_distinct() {
        let a = new_event_id(1_784_000_000_007);
        let b = new_event_id(1_784_000_000_007);
        assert!(a.len() >= 16, "id too short: {a}");
        assert_ne!(a, b, "same-instant ids must still differ");
    }

    #[test]
    fn json_string_mirrors_jq_alternative_and_tostring() {
        use serde_json::json;
        assert_eq!(json_string(None, "unknown"), "unknown");
        assert_eq!(json_string(Some(&Value::Null), "unknown"), "unknown");
        assert_eq!(json_string(Some(&json!(false)), "d"), "d");
        assert_eq!(json_string(Some(&json!("sess")), "d"), "sess");
        assert_eq!(json_string(Some(&json!(5)), "d"), "5");
        assert_eq!(json_string(Some(&json!(true)), "d"), "true");
    }
}
