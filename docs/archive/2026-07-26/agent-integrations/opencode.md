# OpenCode integration decisions

- **Status:** done
- **Current package:** closeout complete
- **Source revision:** `7534d23551f665e65080809975b4ca5c7d63807b`
- **Task list:** `docs/action-plan-agent-integrations.md`

This file is the append-only design and evidence checkpoint for the OpenCode
integration. Update it after every completed or blocked work package.

## Baseline decisions

- Prefer an external Rust adapter subscribing to OpenCode's typed SSE event
  stream.
- Pin and record the OpenCode version because the native event route is marked
  experimental.
- Use durable event IDs and aggregate sequence numbers for replay and
  deduplication.
- Treat `Step.Ended` usage as a candidate, not an assumption, until real traces
  prove its retry and billing semantics.
- Enrich matching shell/tool events rather than emitting duplicate actions.
- Start process attribution with OpenCode root-process-tree observation; prefer
  an upstream optional PID field over a private fork.

## Open decisions

1. How does a subscriber request replay from a durable aggregate sequence?
2. Does `Step.Ended` represent one provider response under retries and routing?
3. How are provider-executed tools distinguished from local tools in real
   traces?
4. Do shell and generic tool events share a call ID in every relevant path?
5. Is an external server already present for normal TUI use, or must `af`
   launch one?
6. What version/capability handshake should reject incompatible schemas?
7. Is root-tree process attribution sufficient under concurrent tools?

## Evidence log

### 2026-07-26 — OC-1 through OC-7 complete

- `GET /api/event` begins with a typed `server.connected` event and
  heartbeats.
- The authoritative integration surface is
  `GET /api/session/:sessionID/event?after=<seq>` because it replays durable
  Session events and then continues live.
- The cursor is exclusive: `after=3` replayed sequence `4`.
- Durable events carry stable event IDs and aggregate sequence/version data.
- A real provider-rate-limit path produced
  `step.started → step.failed`; the collector emits one errored `llm_call`
  with empty usage rather than inventing tokens.
- The successful `step.ended` schema, including input/output/reasoning/cache
  usage, is covered by a pinned deterministic fixture.
- `tool.called → tool.success|failed` is one action boundary. A
  provider-executed tool is remote; local Bash/file/subagent tools are local.
- Process attribution is opt-in via `--pid` and is attached only to local
  action spans because the public protocol does not expose child PIDs.
- OpenCode event IDs are reused as Contract #1 IDs, so replay is idempotent.

Implementation:

- `crates/af-cli/src/cmd/opencode.rs`
- `collectors/opencode/test-data/session.sse`
- `collectors/opencode/test_collector.sh`
- `collectors/opencode/README.md`
- `crates/af-cli/tests/live_opencode.rs`

Validation:

```text
collectors/opencode/test_collector.sh
  ok - OpenCode collector fixtures validate

AF_LIVE_OPENCODE_REPO=/Users/aminesaboni/oss/opencode \
  cargo test -p af-cli --test live_opencode \
  -- --ignored --nocapture --test-threads=1
  1 passed
```

Known gaps: retry, cancellation, subagent, compaction, and a successful
provider-executed tool were schema-inspected but not all forced in one live
session.

### 2026-07-26 — native Rust collector and durable reconnect complete

- Replaced the evidence-phase Python adapter with the native
  `af collect opencode` command in `crates/af-cli/src/cmd/opencode.rs`.
- The live collector persists an atomic per-server/per-session cursor after
  successful spool append. Cursor state includes unmatched step/tool/shell
  starts so settlements still pair after restart.
- EOF and transport failures reconnect with bounded exponential backoff and
  jitter from the latest committed exclusive sequence.
- Replayed/regressed sequences are observable and skipped; gaps are observable
  before processing continues. Corrupt or mismatched cursor files fail closed.
- Offline `--input` mode remains finite, deterministic, cursor-independent,
  and prints the latest sequence.
- The fixture script and manual live test now exercise the Rust binary rather
  than a system Python interpreter.

Validation:

```text
cargo test -p af-cli opencode -- --nocapture
  6 focused tests passed

collectors/opencode/test_collector.sh
  ok - OpenCode collector fixtures validate
```

Durability boundary: a crash after spool append but before cursor replacement
can replay the final stable event ID (at-least-once), while cursor advancement
before append is forbidden because it could permanently lose a raw fact.

## Blocker record template

### YYYY-MM-DD — OC-N

- **Status:** blocked
- **Evidence:**
- **Why the smallest honest implementation cannot proceed:**
- **Options considered:**
- **Recommendation:**
- **Resume from:**

## Closeout checklist

- [x] Raw SSE fixtures committed
- [x] Replay semantics documented
- [x] Contract #1 mapping documented
- [x] Deterministic tests pass
- [x] Live test implemented
- [x] Manual live result recorded
- [x] Capability gaps published
- [x] User guide linked
