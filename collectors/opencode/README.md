# OpenCode collector

For the complete server, session discovery, collector, verification, and
troubleshooting procedure, see
[`docs/codex-opencode-user-guide.md`](../../docs/codex-opencode-user-guide.md).

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

The cursor advances only after all Contract #1 facts derived from an event have
been fully appended. A replayed or regressed sequence is logged and skipped; a
sequence gap is logged before processing continues. Corrupt or mismatched
cursor state fails closed with a clear error. `--after` explicitly overrides
the saved cursor and starts with empty pending lifecycle state.

A process crash between a successful spool append and the following atomic
cursor replace can replay the last fact. Stable OpenCode event IDs keep that
at-least-once boundary deduplicable downstream; advancing before append would
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
[`error-handling policy`](../../docs/error-handling.md).
