# Checkpoint — coding-agent integration optimization review — 2026-07-26

- **Branch:** `poc-iteration-1`
- **HEAD when checkpointed:** `1185399ff9deaaf469bc30c7211fcb10abca0882`
- **Scope:** follow-up performance and maintainability work for the completed
  OpenCode and Codex integrations
- **Implementation status:** OpenCode and Codex functional integrations are
  complete; Pi remains deferred
- **Important:** the worktree contains intentional concurrent changes. Do not
  reset, stash, clean, or broadly rewrite files before inspecting `git status`.

This is the focused restart document for the next implementation session. The
integration queue marks OpenCode and Codex `done`; the tasks below are a new
hardening/optimization batch, not a reason to reopen or discard the validated
functional work.

## 1. Read first

1. `docs/checkpoint-agent-integrations-optimization-2026-07-26.md`
2. `docs/agent-integrations/opencode.md`
3. `docs/agent-integrations/codex.md`
4. `docs/testing-live.md`
5. `docs/research-coding-agent-protocol-affinity-2026-07-26.md`
6. `git status --short`

The broader project checkpoint remains `docs/checkpoint-2026-07-26.md`.

## 2. Current implementation

### OpenCode

- Native Rust command:
  `af collect opencode`, implemented in
  `crates/af-cli/src/cmd/opencode.rs`.
- Source protocol: durable per-session SSE
  `/api/session/:sessionID/event?after=<exclusive-sequence>`.
- Default cursor is explicit `after=0`.
- Multiline SSE `data:` fields, comments, malformed frames, and malformed
  event shapes are handled without killing the stream.
- `step.started + step.ended|failed` emits one `llm_call`.
- `tool.called + tool.success|failed` and shell start/end emit
  `action_span`s.
- Stable OpenCode event IDs are reused as Contract #1 event IDs.
- Local PID attribution is optional through `--pid`.
- Deterministic fixture runner:
  `collectors/opencode/test_collector.sh`.
- Manual live test:
  `crates/af-cli/tests/live_opencode.rs`.
- Atomic per-server/per-session cursors persist the exclusive durable sequence
  and pending lifecycle starts; live streams reconnect with bounded backoff.

### Codex

- Default topology is native Codex OTLP logs into the existing receiver; no
  hook installation or app-server proxy is required.
- Normalizer: `crates/af-otlp/src/normalize/codex.rs`.
- `conversation_starts` emits `session_meta` and populates a bounded
  conversation-to-provider cache.
- Token-bearing `sse_event/response.completed` emits `llm_call`.
- `tool_result` emits `action_span`.
- The normalizer accepts native token counts encoded as integer, numeric
  string, or integral double and falls back from `timeUnixNano=0` to
  `event.timestamp`.
- Stateful normalizers are currently held by a process-global registry in
  `crates/af-otlp/src/normalize.rs` so provider correlation survives batches.
- OTLP envelopes are routed to spool files by envelope collector and session
  in `crates/af-otlp/src/server.rs`.
- Manual live test: `crates/af-cli/tests/live_codex.rs`.
- Live model is explicit and configurable through `AF_LIVE_CODEX_MODEL`;
  default: `gpt-5.4-mini`.

## 3. Current validation state

The following passed against the shared worktree immediately before this
checkpoint:

```text
collectors/opencode/test_collector.sh
  passed

cargo test -p af-otlp codex -- --nocapture
  8 focused Codex/receiver tests passed

cargo test -p af-cli --test live_opencode --no-run
  compiled

cargo test -p af-cli --test live_codex --no-run
  compiled

git diff --check
  passed
```

Earlier manual live runs also passed for both agents. Authentication may block
re-running Codex live tests in a restricted workspace; deterministic tests and
`--no-run` compilation do not require authentication.

## 4. Ordered optimization backlog

### AI-OPT-1 — OpenCode reconnect and durable cursor persistence

- **Priority:** P1
- **Status:** complete — implemented as the native Rust collector
- **Primary driver:** reliability and restart performance
- **Files:**
  - `crates/af-cli/src/cmd/opencode.rs`
  - `collectors/opencode/README.md`
  - `collectors/opencode/test_collector.sh`
  - optionally a new fixture/helper under `collectors/opencode/test-data/`

Current problem:

- the collector opens one SSE request and exits on EOF/network error;
- the last sequence is printed only when the stream ends;
- default restart from sequence zero reparses and respools the whole session;
- stable IDs prevent SQLite duplicates, but spool growth and replay cost are
  still O(total session history).

Target design:

1. persist the exclusive durable sequence only after all Contract #1 facts
   derived from that event have been appended successfully;
2. reconnect with bounded exponential backoff and jitter;
3. resume from the persisted sequence;
4. make cursor state atomic and per `(server/session)`;
5. surface sequence regressions or gaps rather than silently advancing;
6. keep offline `--input` mode deterministic and finite.

Required tests:

- disconnect/reconnect does not duplicate Contract #1 facts;
- cursor advances after spool append, not before;
- restart resumes from the saved cursor;
- a corrupt cursor file fails clearly or falls back according to a documented
  policy;
- a sequence gap is observable;
- offline fixture mode still exits and prints the latest sequence.

Completion record:

- Replaced the Python evidence adapter with `af collect opencode` rather than
  hardening a second runtime implementation.
- Cursor files live under `$AF_STATE_DIR/cursors/opencode/`, are keyed by
  canonical server URL plus session ID, and are replaced atomically only after
  complete spool append.
- Pending step/tool/shell starts are stored with the cursor so restart does not
  discard pairing context.
- Reconnect replay, append-before-cursor ordering, restart state, corrupt
  cursor failure, sequence gaps, and finite offline behavior have deterministic
  Rust coverage.
- `collectors/opencode/test_collector.sh` and
  `crates/af-cli/tests/live_opencode.rs` invoke the native binary.

Validation after completion:

```text
cargo test -p af-cli
  59 unit tests passed, 1 performance test ignored
  32 integration tests passed, 4 manual live tests ignored

collectors/opencode/test_collector.sh
  ok - OpenCode collector fixtures validate

cargo test -p af-cli --test live_opencode --no-run
  compiled

rustfmt --edition 2021 --check \
  crates/af-cli/src/cmd/opencode.rs crates/af-cli/tests/cli.rs
  passed

git diff --check
  passed
```

The workspace-wide `cargo fmt --all -- --check` still reports formatting in
concurrent pre-existing edits under `crates/af-cli/src/cmd/debug_server.rs` and
`crates/af-otlp/src/normalize/codex.rs`; this package did not rewrite those
files.

User documentation follow-up:

- Added `docs/codex-opencode-user-guide.md` with complete persistent and
  one-shot Codex OTLP setup, OpenCode server/session/collector setup,
  verification, privacy defaults, shutdown order, and troubleshooting.
- Linked the guide from the root README, both collector READMEs, and the live
  testing guide.
- Verified that Codex CLI 0.142.0 loads the documented `[otel]` table with
  `codex doctor --summary`, and that `codex exec --strict-config` rejects an
  unknown configuration field before starting a turn.
- Made explicit OpenCode `--after` recovery bypass a corrupt saved cursor, so
  the documented intentional replay procedure works as described.

Unified onboarding follow-up:

- Added the native `af setup` inspect/plan/apply wizard for Codex, Claude Code,
  and OpenCode detection.
- Codex setup appends a privacy-safe OTLP table only when no conflicting
  exporter exists; Claude setup installs an embedded hook and merges project
  settings with timestamped backups.
- OpenCode is detected and documented, but automatic server/session discovery
  remains deferred; the wizard reports the current parallel collector command.
- Added the single root `install.sh`, which installs/builds `af` and invokes
  the native wizard. It supports local checkout builds plus explicit binary or
  source URLs for the later public curl distribution.
- Removed the superseded one-off `scripts/setup-codex.sh`.

Validation:

```text
cargo test -p af-cli
  66 unit tests passed, 1 performance test ignored
  33 integration tests passed, 4 manual live tests ignored

scripts/test-install.sh
  ok - install.sh installs af and completes wizard setup

sh -n install.sh
sh -n scripts/test-install.sh
rustfmt --edition 2021 --check \
  crates/af-cli/src/cmd/setup.rs crates/af-cli/src/cmd/opencode.rs \
  crates/af-cli/tests/cli.rs
git diff --check
  passed
```

The loopback-binding tests in the full `af-cli` suite require sandbox network
permission; the final full run passed with that permission enabled.

### AI-OPT-2 — Bound OpenCode unmatched lifecycle state

- **Priority:** P1
- **Primary driver:** long-running memory boundedness
- **Files:** `crates/af-cli/src/cmd/opencode.rs` and fixture tests

Current problem:

- `steps`, `tools`, and `shells` keep starts until settlements arrive;
- crashes, cancellation, dropped events, or protocol drift can retain entries
  for the process lifetime.

Target design:

1. store durable sequence/timestamp with each open item;
2. cap entries globally and/or by category;
3. expire old unmatched entries with an explicit counter/log;
4. clear session state on a demonstrated terminal/session event if available;
5. never synthesize a successful action or inference from incomplete state.

Required tests:

- repeated unmatched starts remain under the configured cap;
- settlements still pair after normal interleaving;
- eviction is deterministic and observable;
- malformed events cannot leave partially inserted state.

### AI-OPT-3 — Stable canonical fallback event IDs

- **Priority:** P1
- **Primary driver:** replay correctness and maintainability
- **Files:**
  - `crates/af-otlp/src/normalize.rs`
  - `crates/af-otlp/src/normalize/codex.rs`
  - possibly Claude/GenAI normalizers if shared intentionally

Current problem:

- fallback IDs use `DefaultHasher`, which is not guaranteed stable across Rust
  releases;
- some IDs include OTLP batch-local record index, so the same record retried in
  a differently partitioned batch may deduplicate as new.

Target design:

1. prefer upstream stable IDs (`call_id`, response/request ID) whenever present;
2. otherwise hash a documented canonical tuple or canonical JSON projection;
3. use a specified stable algorithm, preferably SHA-256 truncated to the
   Contract #1 ID size budget;
4. exclude batch position unless it is genuinely part of source identity;
5. preserve collision resistance for otherwise identical same-timestamp calls.

Required tests:

- identical logical records in different batch positions get the same ID;
- distinct calls with the same model/timestamp/usage remain distinct when a
  native ID is available;
- generated IDs are stable across repeated processes;
- short native IDs remain schema-valid and non-colliding.

### AI-OPT-4 — Receiver-owned normalizer registry

- **Priority:** P2
- **Primary driver:** deterministic replay and maintainability
- **Files:**
  - `crates/af-otlp/src/normalize.rs`
  - `crates/af-otlp/src/server.rs`
  - related tests

Current problem:

- Codex provider correlation requires state across OTLP batches;
- that state currently lives in a process-global `LazyLock` registry;
- `normalize_logs` behavior can depend on records normalized earlier in the
  same process, including test/replay order.

Target design:

1. introduce an explicit `NormalizerRegistry`/`NormalizerSet` value;
2. the live receiver owns one registry for its lifetime;
3. deterministic/offline normalization creates a fresh registry;
4. installed-descriptor discovery remains cheap and state-independent;
5. avoid a new trait/object abstraction unless it clearly simplifies the
   existing three-normalizer registry.

Required tests:

- provider correlation survives separate live receiver requests;
- two fresh registries do not share state;
- replay output does not depend on prior unrelated tests;
- provider cache remains bounded.

### AI-OPT-5 — Reduce spool-write syscall overhead

- **Priority:** P2
- **Primary driver:** hot-path performance
- **Files:**
  - `crates/af-cli/src/cmd/opencode.rs`
  - `crates/af-otlp/src/server.rs`

Current problem:

- OpenCode opens, appends one line, and closes the spool for each emitted fact;
- OTLP groups by destination but calls `write_all` once per line.

Target design:

- OpenCode: either retain one append descriptor for the collector lifetime or
  use a small bounded writer abstraction. Preserve line completeness and
  crash-visible flush semantics.
- OTLP: concatenate the already bounded lines per destination and perform one
  `write_all` per collector/session/request.

Measure before and after with syscall counts or a deterministic append-heavy
fixture. Do not add asynchronous buffering without evidence; it complicates
durability and shutdown.

### AI-OPT-6 — Generic OTLP health and quarantine labeling

- **Priority:** P3
- **Primary driver:** operations and maintainability
- **Files:**
  - `crates/af-cli/src/cmd/watch.rs`
  - `crates/af-otlp/src/server.rs`
  - tests/docs

Current problem:

- debug health recognizes only `otlp-cc` as `POST /v1/logs`;
- Codex appears as a JSONL collector even though its source is OTLP;
- malformed/dropped batches use `otlp-cc.*` quarantine prefixes regardless of
  which normalizer failed.

Target design:

- identify OTLP collectors through normalizer descriptors or a stable prefix
  convention;
- use generic `otlp.unparsed` / `otlp.dropped` quarantine names, with claimed
  normalizer IDs included in metadata/logs where practical;
- do not duplicate transport knowledge in the debug console.

## 5. Suggested execution order

1. AI-OPT-1 reconnect/cursor persistence.
2. AI-OPT-2 bounded OpenCode state.
3. AI-OPT-3 stable canonical IDs.
4. AI-OPT-4 receiver-owned registry.
5. AI-OPT-5 measured write batching.
6. AI-OPT-6 operational labeling cleanup.

Keep each package reviewable. Update this document after each package with
files changed, decisions, test commands, and measured performance evidence.

## 6. Validation commands

Fast deterministic loop:

```sh
collectors/opencode/test_collector.sh
cargo test -p af-otlp codex -- --nocapture
cargo test -p af-cli --test live_opencode --no-run
cargo test -p af-cli --test live_codex --no-run
cargo fmt --all -- --check
git diff --check
```

The OTLP receiver HTTP tests bind loopback and may require sandbox approval.

Manual live tests, when credentials/network permit:

```sh
AF_LIVE_OPENCODE_REPO=/absolute/path/to/opencode \
  scripts/test-live.sh opencode

AF_LIVE_CODEX_MODEL=gpt-5.4-mini \
  scripts/test-live.sh codex
```

## 7. Guardrails

- Preserve Contract #1 raw-fact boundaries; do not add estimation methodology
  to collectors.
- Do not parse Codex SQLite/rollout state as the primary source.
- Do not replace normal Codex CLI/TUI workflows with app-server coupling.
- Do not infer OpenCode child PIDs from the server PID.
- Do not classify plain MCP work as remote when source events do not prove it.
- Preserve unrelated concurrent worktree edits.
- Do not mark Pi active; it remains deferred until adoption/traction changes.

## 8. Context from the review

Positive properties to preserve:

- OpenCode uses native stable event IDs and explicit exclusive sequence
  semantics.
- OpenCode SSE parsing tolerates multiline data, comments, malformed frames,
  and malformed event payloads without terminating collection.
- Codex native OTLP preserves normal CLI/TUI operation and avoids app-server
  runtime coupling.
- Codex provider identity is carried from `conversation_starts` and falls back
  honestly to `unknown` if the start was missed.
- Provider cache is bounded to 512 conversations.
- Duration-only Codex `response.completed` events are not double-counted.
- Plain MCP execution locus remains `unknown` rather than being guessed remote.
- OTLP spool routing is collector/session aware.

The current focused test suite passes. The backlog above is optimization and
hardening work, not a report that the integrations are unusable.
