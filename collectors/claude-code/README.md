# Claude Code hooks collector (`cc-hooks`)

For an end-to-end installation and usage walkthrough—including building
`af`, provisioning Python, configuring OTLP, running `af watch`, and adding
the optional statusline—see
[`docs/claude-code-user-guide.md`](../../docs/claude-code-user-guide.md).

`af-hook.sh` is a POSIX `sh` shim that turns Claude Code's six lifecycle
hooks into Contract #1 events, appended to
`$AF_STATE_DIR/spool/cc-hooks.<session_id>.jsonl` (default `AF_STATE_DIR`:
`~/.local/state/agentic-footprint`). It never reads transcripts — only the
hook JSON Claude Code writes to its stdin.

### Dependencies and failure behavior

**`jq` is a required dependency**, and the *only* external one. `af-hook.sh`
checks `command -v jq` before doing anything else; if `jq` isn't on `PATH`,
the shim **silently no-ops and exits 0** — no error, no log line, nothing
written to the spool. Collection is best-effort; the running Claude Code
session is not something this shim is ever allowed to disturb.

That "never disturb the session" contract extends past the `jq` check: the
shim **always exits 0**, regardless of any internal failure (malformed
stdin JSON, an unwritable state dir, a corrupted open-span file, ...).
Internal errors are best-effort appended to
`$AF_STATE_DIR/tmp/hook-errors.log` (never to stdout/stderr in a way Claude
Code would see as a hook failure) and otherwise swallowed. `session_id` and
`tool_use_id` from the hook payload are also sanitized (every character
outside `[A-Za-z0-9._-]` stripped, leading `.` guarded) before either is
used to build a file path, so a malicious or malformed id can't write
outside `$AF_STATE_DIR/spool/` or `$AF_STATE_DIR/tmp/openspans/<session_id>/`.
The same rule is implemented in `crates/af-otlp` and `python/af_sampler`,
and all three are pinned to the shared conformance vectors in
`tests/fixtures/sanitize-vectors.json` — two collectors that disagree
about what a session id may contain produce two filenames for one session.

This collector observes local tool-call *spans* (when a tool started, when
it finished, what kind it was). It does **not** capture LLM token usage —
that's the OTLP receiver's job (`crates/af-otlp`, Task 8); the two
collectors are complementary and write to separate spool files
(`cc-hooks.*` vs `otlp-cc.*`) for the same session.

## `.claude/settings.json` hooks snippet

Register the **same script**, unmodified, for all six events — the shim
dispatches on the hook payload's own `hook_event_name` field.

**`PostToolUseFailure` is not optional.** Claude Code fires exactly one of
`PostToolUse` / `PostToolUseFailure` per tool call, and a tool call that
fails — nonzero exit, denied permission, interrupt — gets *only* the
failure event. Omit it from the registration and every failed tool call's
span stays open until the `Stop` sweep closes it with a fabricated end
time (`status: "unknown"`, duration inflated to "until the turn ended").
For a debugging agent that is the wrong half of the session to lose: the
failing test run is the tool call the whole task is about. This behavior is
covered by the collector fixtures.

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "/absolute/path/to/collectors/claude-code/af-hook.sh" } ] }
    ],
    "PreToolUse": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "/absolute/path/to/collectors/claude-code/af-hook.sh" } ] }
    ],
    "PostToolUse": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "/absolute/path/to/collectors/claude-code/af-hook.sh" } ] }
    ],
    "PostToolUseFailure": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "/absolute/path/to/collectors/claude-code/af-hook.sh" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "/absolute/path/to/collectors/claude-code/af-hook.sh" } ] }
    ],
    "SessionEnd": [
      { "hooks": [ { "type": "command", "command": "/absolute/path/to/collectors/claude-code/af-hook.sh" } ] }
    ]
  }
}
```

Use an absolute path; `af-hook.sh` must be executable (`chmod +x`).

### The `$PPID` trick — do not wrap the command in `sh -c '...'`

`command` above is the **direct path to the executable script**, not a
`sh -c '...'` string. This matters: Claude Code spawns the `command` value
as a *direct child process*. Verified empirically during this task's spike
— a hook script that dumped `$$`/`$PPID`/`ps -ef` showed the shim's
`$PPID` was exactly the pid of the running `claude` process itself, one
level up, no intermediate shell in between. If the command were instead
`"sh -c '/path/to/af-hook.sh'"`, the shim's parent would be that
transient `sh -c` wrapper, not Claude Code — the trick would silently
break. This is why `SessionStart`'s bootstrap `action_span` can report
`pids: [$PPID]` as an (imperfect, but real) proxy for "the Claude Code
process running this session," without any other way to discover that pid
from the hook payload.

## OTel env block (Task 8 receiver, run alongside this collector)

Set these before launching `claude` to also capture LLM call telemetry via
the local OTLP receiver (`crates/af-otlp::serve`, `POST /v1/logs`):

```sh
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL=http/json
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
```

The hooks collector and the OTLP receiver are independent and can run with
either one enabled alone.

## Timestamp precision

`af-hook.sh` timestamps every event with **millisecond precision**:
`2026-07-25T12:00:00.123Z`, RFC 3339 UTC. `date` can't produce it portably
(sub-second output needs GNU's non-POSIX `+%N`, absent from macOS's BSD
`date`), so the shim derives it from `jq`'s float `now` instead — jq is
already its only hard dependency, and jq's `strftime` is gmtime-based, so
the `Z` is honest. The timestamp comes from a *single* `now` sample, so
the seconds and the millisecond remainder can never straddle a second
boundary. That sample is taken once per hook invocation and shared by
every event the invocation emits (SessionStart's bootstrap span and
`session_meta` carry the same `ts`, and so does every span a Stop sweep
closes): one invocation is one instant as far as this collector can
honestly claim to know, and re-sampling would only add fork latency to a
hook the session blocks on.

`t_start == t_end` is therefore no longer the common case for a fast tool
call, but it is still possible — and always the case for a `PostToolUse`
with no matching `PreToolUse` record, where the span is honestly collapsed
to a point with `status: "unknown"`. Downstream consumers should still not
assume a nonzero span duration.

## Behavior summary

| Hook event | Emits |
|---|---|
| `SessionStart` | A zero-length bootstrap `action_span` (`tool_name: "__session__"`, `pids: [$PPID]`) + one `session_meta` (`agent_app.name: "claude-code"`, `os`) |
| `PreToolUse` | Nothing to the spool — opens `$AF_STATE_DIR/tmp/openspans/<session_id>/<tool_use_id>` (`{t_start, tool_name}`) |
| `PostToolUse` | One `action_span` closing the open-span with `status: "ok"` (or `status: "unknown"`, `t_start == t_end` if none was found, or the file exists but doesn't parse — same fallback, and the stale file is still deleted) |
| `PostToolUseFailure` | The same closure, with `status: "cancelled"` when the payload's `is_interrupt` is `true` and `status: "error"` otherwise. A span whose start was never observed still closes `unknown` with `t_start == t_end`: the payload's own `duration_ms` is Claude Code's measurement, not this collector's, and `t_start` is never fabricated from it |
| `Stop` / `SessionEnd` | Closes every remaining open-span with `status: "unknown"` and deletes its file; a stray file that doesn't parse as JSON is counted, deleted, and skipped (nothing emitted for it) rather than crashing the sweep |

Collector degradation and control-plane failures follow the shared
[`error-handling policy`](../../docs/error-handling.md).

## Test fixtures (`test-data/`)

- `*.real.json` — sanitized captures from a real headless Claude Code
  v2.1.220 session (`claude -p "run: echo hi" --model haiku`) run during
  this task's spike, with a `.claude/settings.json` dumping every hook's
  raw stdin to a file. Only `cwd`/`transcript_path` were replaced with
  placeholder paths (no other field was altered) — `session_id`,
  `hook_event_name`, `tool_name`, `tool_input`, `tool_use_id`, and
  `tool_response` are the real values Claude Code emitted.
  `posttoolusefailure_bash.real.json` comes from a second v2.1.220 capture
  (Task 15, `claude -p "Run exactly this bash command and then stop:
  exit 3"`), sanitized the same way and additionally re-keyed to the first
  capture's `session_id`/`tool_use_id` so it pairs with
  `pretooluse_bash.real.json` in the test harness.
- `*.synthetic.json` — handwritten from Claude Code's documented hook
  payload fields, for tool kinds (`Edit`, `mcp__*`, `Task`, `WebFetch`)
  the captured session didn't happen to exercise (it only ran `Bash`).

## Running the tests

```sh
collectors/claude-code/test_hooks.sh
```

Builds `af-cli` once, then pipes each fixture through the shim into a
fresh `AF_STATE_DIR` per case and validates every resulting spool line via
`af validate-line` (a hidden `af-cli` subcommand built for exactly this).
Also runs in CI (`.github/workflows/ci.yml`, after `cargo test --workspace`).
