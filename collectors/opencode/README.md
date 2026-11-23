# OpenCode collector

OpenCode collection is experimental: it exists only in a build with
`--features experimental-opencode`, and there is no user guide for it yet.
The live test procedure is in
[`docs/contributing/testing-live.md`](../../docs/contributing/testing-live.md).

There are two collectors:

- **The plugin** (`af-plugin.ts`, temporary), which `af setup` installs as
  `agentic-footprint.ts` in OpenCode's global plugin directory
  (`$XDG_CONFIG_HOME/opencode/plugin/`, default `~/.config/opencode/plugin/`).
  It turns OpenCode's bus events into facts: `session.created` →
  `session_meta`, each `step-finish` part → one `llm_call` (provider and
  model from its assistant message), each completed or failed tool part →
  one `action_span`. It reads no prompts, text, tool inputs, outputs or
  error messages. Checked live on OpenCode 1.19.5 (`opencode run`). A model
  call that fails before its step finishes is not counted. Tests:
  `node --test collectors/opencode/af-plugin.test.ts`.
- **`af collect opencode`** (below), which follows OpenCode's durable
  per-session event stream. Released OpenCode (1.19.5) only writes that
  stream for sessions created through `/api/session`, not for the TUI's or
  `opencode run`'s sessions, so the plugin is what collects in practice.

`af collect opencode` subscribes to OpenCode's durable per-session SSE route
and appends raw Contract #1 facts to
`$AF_STATE_DIR/spool/opencode.<session-id>.jsonl`.

The collector treats these boundaries as authoritative:

- `session.next.step.started` plus `step.ended|failed` → one `llm_call`;
- `tool.called` plus `tool.success|failed` → one `action_span`;
- shell start/end → one local Bash `action_span`.

## Live collection

```sh
af collect opencode \
  --url http://127.0.0.1:4096 \
  --session-id ses_... \
  --directory "$PWD" \
  --pid "$(cat /tmp/af-opencode-server.pid)"
```

The native Rust collector persists an atomic cursor under
`$AF_STATE_DIR/cursors/opencode/`, keyed by the canonical server URL and
session ID. The cursor contains both the exclusive durable sequence and any
unmatched step/tool/shell starts needed to settle events after a restart.
Live mode reconnects indefinitely with bounded exponential backoff and jitter.

The cursor never names a sequence whose facts are not on disk. It is
checkpointed at most once a second or every 64 durable events, and when the
stream ends; each checkpoint fsyncs the spool first, then replaces the cursor
atomically. A replayed or regressed sequence is logged and skipped; a
sequence gap is logged before processing continues. Corrupt or mismatched
cursor state fails closed with a clear error. `--after` explicitly overrides
the saved cursor and starts with empty pending lifecycle state.

A process crash between checkpoints replays the events since the last one.
Stable OpenCode event IDs keep that at-least-once boundary deduplicable (the
store ignores an event ID it already holds); advancing before append would
instead risk permanent fact loss.

## Offline fixtures

```sh
AF_STATE_DIR="$(mktemp -d)" af collect opencode \
  --session-id ses_fixture \
  --input collectors/opencode/test-data/session.sse \
  --pid 4242 \
  --opencode-version test
```

Offline `--input` mode is deterministic and finite. It does not read or write
the live cursor and prints the latest sequence when the fixture ends.

Run the fixture validation with:

```sh
collectors/opencode/test_collector.sh
```

Collector degradation and control-plane failures follow the shared
[`error-handling policy`](../../docs/contributing/error-handling.md).
