# Codex integration decisions

- **Status:** done
- **Current package:** closeout complete
- **Source version:** Codex CLI `0.142.0`
- **Task list:** `docs/action-plan-agent-integrations.md`

This file is the append-only design and evidence checkpoint for the Codex
integration. Update it after every completed or blocked work package.

## Baseline decisions

- Treat app-server v2 as the highest-fidelity lifecycle source currently
  identified.
- Treat native OTLP as the preferred inference telemetry source when it can be
  correlated without double counting.
- Use generated schemas pinned to an exact Codex CLI version.
- Treat cumulative thread token totals as cross-checks; emit only proven
  per-operation deltas.
- Keep `exec --json` as a separate noninteractive adapter.
- Do not parse Codex SQLite/rollout state as the primary collector.
- Do not silently replace ordinary TUI operation with an `af`-managed control
  protocol without recording that product decision.

## Open decisions

1. Can ordinary TUI sessions be observed with hooks plus OTLP alone?
2. Are app-server token updates per provider call, per turn, or cumulative?
3. Which IDs correlate app-server items and OTLP inference records?
4. Does app-server expose child PIDs or only logical command items?
5. Is managed app-server launch acceptable, or is a transparent proxy needed?
6. Which app-server protocol changes are backward compatible?
7. What minimum Codex version should the collector support?

## Evidence log

### 2026-07-26 — CX-1 through CX-6 complete

Official Codex documentation and installed CLI `0.142.0` agree that:

- native OTel log export is supported for ordinary CLI/TUI runs and is
  disabled by default;
- representative native events include `codex.conversation_starts`,
  `codex.sse_event`, and `codex.tool_result`;
- app-server v2 is the deeper JSON-RPC lifecycle protocol, but remains
  experimental and primarily intended for embedded clients.

Generated app-server schemas were captured from the installed version. They
confirm stable thread/turn/item identities and command/file item kinds, but no
app-server proxy is required for the selected implementation.

Real captures established:

- `codex exec --json` emits exact command item starts/completions and final
  turn usage, but covers only noninteractive exec mode;
- native OTel works with ordinary Codex execution and emits one token-bearing
  `codex.sse_event` with `event.kind=response.completed` per provider response;
- Codex also emits a sibling duration-only `response.completed` record. It is
  ignored to prevent double counting;
- `codex.tool_result` contains `call_id`, `tool_name`, `duration_ms`, and
  success, sufficient for a completed `action_span` without parsing output;
- some exporter builds encode token counts as `stringValue` or
  `doubleValue`, and set `timeUnixNano=0` while supplying `event.timestamp`;
  the decoder now accepts those exact native variants;
- native OTel does not expose child PIDs. Action spans therefore carry no PID
  rather than attributing the Codex root process incorrectly.

### Runtime decision

Use native OTel as the default integration. This is lower risk than the
original app-server plan because it preserves normal Codex CLI/TUI operation,
requires only user opt-in configuration, and reuses the existing OTLP
receiver. Keep app-server v2 as an optional future affinity upgrade for
products that already embed Codex; do not proxy or replace ordinary sessions.

### Contract mapping

- `codex.conversation_starts` → `session_meta`;
- token-bearing `codex.sse_event/response.completed` → `llm_call`;
- `codex.tool_result` → `action_span`;
- duration-only response records → ignored;
- `exec --json` → researched reference surface, not a second active collector.

### Implementation

- `crates/af-otlp/src/normalize/codex.rs`
- `crates/af-otlp/src/normalize/record.rs`
- `crates/af-otlp/src/server.rs` now routes envelopes by collector and session
  instead of forcing every OTLP normalizer into an `otlp-cc` file;
- `crates/af-otlp/tests/fixtures/otlp/logs-codex.json`
- `crates/af-cli/tests/live_codex.rs`
- `collectors/codex/README.md`
- `scripts/test-live.sh codex`

### Validation

```text
cargo test -p af-otlp -- --nocapture
  34 passed

cargo test -p af-cli --test live_codex \
  -- --ignored --nocapture --test-threads=1
  1 passed
```

Known gaps: OTel tool results do not expose child PIDs, and no active
app-server collector is shipped. This is an accepted fidelity/coupling tradeoff,
not a blocker.

## Blocker record template

### YYYY-MM-DD — CX-N

- **Status:** blocked
- **Evidence:**
- **Why the smallest honest implementation cannot proceed:**
- **Options considered:**
- **Recommendation:**
- **Resume from:**

## Closeout checklist

- [x] Generated schemas/fixtures committed
- [x] App-server and OTLP correlation documented
- [x] Runtime topology decided
- [x] Contract #1 mapping documented
- [x] Deterministic tests pass
- [x] Live test implemented
- [x] Manual live result recorded
- [x] Capability gaps published
- [x] User guide linked
