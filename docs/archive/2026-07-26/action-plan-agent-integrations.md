# Action plan: autonomous coding-agent integrations

- **Status:** ready for autonomous execution
- **Current order:** OpenCode → Codex; Pi deferred pending adoption/traction
- **Rule:** if one integration is blocked, checkpoint it and continue to the
  next integration
- **Shared live-test infrastructure:**
  `crates/af-cli/tests/common/live.rs`
- **Machine-readable queue:** `docs/agent-integrations/tasks.json`
- **Protocol research:**
  `docs/research-coding-agent-protocol-affinity-2026-07-26.md`

This is the ordered implementation queue for adding coding-agent collectors.
It is intentionally operational: each work package has inputs, outputs,
validation, decision checkpoints, and an explicit point at which an autonomous
implementation should stop and move to the next agent.

## 1. Execution protocol

At the start of every autonomous session:

1. read this document;
2. read `docs/checkpoint-2026-07-26.md` and inspect `git status`;
3. select the first integration whose status is `ready` or `in_progress`;
   `docs/agent-integrations/tasks.json` is the selection source of truth;
4. read that integration's decision file in `docs/agent-integrations/`;
5. preserve unrelated concurrent worktree edits;
6. complete one work package at a time and update both this task list and the
   integration decision file before moving on;
7. run deterministic tests before live tests;
8. never run a live test unless the agent CLI is installed, authenticated, and
   its preflight can fail with a clear skip/action message;
9. if blocked, record the evidence and next decision, mark the package
   `blocked`, then continue to the next agent.

### Status vocabulary

Use exactly these values so the list remains searchable and scriptable:

- `ready`: no known prerequisite blocks implementation;
- `in_progress`: the current active package;
- `done`: implementation and its required validation completed;
- `blocked`: cannot progress without a user/vendor/upstream decision;
- `deferred`: deliberately out of the current autonomous loop.

Only one package should be `in_progress` at a time.

### Blocker threshold

Do not mark an integration blocked merely because a detail is uncertain.
Implement the smallest honest capability and publish explicit gaps. Block only
when one of these is true:

- no supported source can provide stable session or tool identity;
- usage semantics would require fabricating or double-counting `llm_call`;
- integration requires changing the agent's execution semantics without an
  upstream-supported hook;
- a required manual live test repeatedly cannot run because of auth, install,
  network, or vendor failure;
- Contract #1 cannot represent a required fact without a design decision.

When blocked, the decision file must contain:

```text
Date / package
Observed evidence
Why the smallest honest implementation cannot proceed
Options considered
Recommended decision
Exact next command or file to resume from
```

## 2. Shared implementation rules

### Collector boundary

Agent integrations emit raw Contract #1 facts only. They must not contain:

- EcoLogits or CodeCarbon methodology;
- derived joins;
- direct writes to the control-plane SQLite database;
- presentation-specific logic.

### Source precedence

For every emitted fact, record which source is authoritative. When two sources
describe the same operation, one must be designated `authoritative` and the
other `cross_check`; totals are never added blindly.

### Capability descriptor

Every collector or normalizer must declare:

- source protocol and version;
- emitted Contract #1 types;
- live lifecycle fidelity;
- usage completeness;
- process/PID fidelity;
- known unsupported cases.

### Fixtures before live sessions

For each integration, add sanitized fixtures that cover at minimum:

- session start and completion;
- successful tool call;
- failed or cancelled tool call;
- completed LLM usage;
- unknown/new source event preservation or ignore behavior;
- deterministic event IDs and replay/deduplication.

### Live-test conventions

Reuse and extend `crates/af-cli/tests/common/live.rs` rather than introducing a
separate process/watch/HTTP harness. Refactor agent-specific Claude behavior out
of the common module only when a second agent needs the same primitive.

Each agent gets one ignored integration-test binary:

```text
crates/af-cli/tests/live_pi.rs
crates/af-cli/tests/live_opencode.rs
crates/af-cli/tests/live_codex.rs
```

Each binary must:

- preflight the CLI, version, auth, and required configuration;
- use an isolated temporary `AF_STATE_DIR` and temporary project;
- start a real `LiveWatch` on ephemeral ports;
- perform a cheap, bounded prompt with deterministic local tool work;
- wait on observable conditions, never fixed sleeps;
- assert raw events, store state, debug health, and at least one joined unit;
- print actionable stdout/stderr on timeout;
- remain `#[ignore]` and manual-only.

Update `scripts/test-live.sh` to accept an agent selector while retaining a way
to run all available live suites serially:

```sh
scripts/test-live.sh pi
scripts/test-live.sh opencode
scripts/test-live.sh codex
scripts/test-live.sh all
```

Do not add a default CI job that spends provider tokens.

## 3. Ordered backlog

### Deferred integration — Pi

- **Integration status:** `deferred`
- **Decision log:** `docs/agent-integrations/pi.md`
- **Source revision researched:**
  `5bc1c2c0a6f07e00e8c240304182f213ab8d311f`

#### PI-1 — Capture extension event ordering

- **Status:** `ready`
- Build a capture-only Pi extension outside production collector code.
- Capture sanitized events for session new/resume/fork, one successful bash
  tool, one tool error, abort, retry, compaction, and shutdown.
- Identify the exactly-once completed assistant event carrying normalized
  usage.
- Record findings in `docs/agent-integrations/pi.md`.

Validation:

```sh
# Pi-repository-specific focused tests or capture command, documented after
# the extension entrypoint is selected.
```

Block and continue to OpenCode if Pi cannot expose stable session/tool IDs or
an exactly-once completed usage record.

#### PI-2 — Implement thin Contract #1 extension

- **Status:** `ready`
- Create the Pi integration in a dedicated collector directory.
- Emit `session_meta`, `action_span`, and `llm_call`.
- Use Pi's stable session and tool-call IDs.
- Keep append work bounded, fail-open, and free of methodology/database logic.
- Sanitize IDs with the shared cross-language vectors.
- Add collector documentation and installation instructions.

Suggested location:

```text
collectors/pi/
```

#### PI-3 — Process attribution

- **Status:** `ready`
- Use Pi's supported bash spawn hook or built-in-operation wrapper.
- Preserve Pi's existing shell semantics, cancellation, buffering, and
  process-tree cleanup.
- Emit observed root PIDs on corresponding bash spans.
- If reliable PID observation requires replacing core execution behavior,
  record the gap and proceed without PIDs rather than patching Pi silently.

#### PI-4 — Deterministic tests

- **Status:** `ready`
- Add sanitized raw Pi event fixtures.
- Test success, failure, abort, retries, compaction helper calls, custom tools,
  duplicate delivery, and partial/invalid records.
- Add Contract #1 schema validation and replay determinism.
- Test collector append failure remains fail-open.

#### PI-5 — Live test

- **Status:** `ready`
- Add `live_pi.rs` using `common/live.rs`.
- Extend the common harness with generic child-output and project-fixture
  helpers only where needed.
- Preflight the Pi executable and authentication/provider configuration.
- Verify one LLM call, one bash action span, session metadata, and debug health.
- Add Pi dispatch to `scripts/test-live.sh`.

Required validation before `done`:

```sh
cargo test -p af-events -p af-spool -p af-store -p af-core -p af-cli --no-fail-fast
cargo fmt --all -- --check
git diff --check
scripts/test-live.sh pi
```

The live command is manual and may be recorded as environment-blocked while
deterministic tests still complete.

#### PI-6 — Closeout

- **Status:** `ready`
- Update the research affinity based on real fixtures.
- Mark known gaps explicitly.
- Add a user guide and README link.
- Record final files, commands, results, and unresolved decisions in
  `docs/agent-integrations/pi.md`.

### Integration 1 — OpenCode

- **Integration status:** `done`
- **Decision log:** `docs/agent-integrations/opencode.md`
- **Source revision researched:**
  `7534d23551f665e65080809975b4ca5c7d63807b`

#### OC-1 — Capture typed SSE and replay

- **Status:** `done`
- Run OpenCode's server/TUI with the typed `/api/event` SSE stream.
- Capture sanitized events for normal inference, retry, error, cancellation,
  shell, file edit, remote/provider-executed tool, subagent, and compaction.
- Disconnect and reconnect to verify durable sequence/replay behavior.
- Confirm `Step.Ended` is one honest completed inference unit.
- Record findings in `docs/agent-integrations/opencode.md`.

Block and continue to Codex if the event route cannot replay reliably, event
version drift cannot be detected, or usage is cumulative/ambiguous.

#### OC-2 — Implement offline normalizer

- **Status:** `done`
- Add Rust source types or narrowly scoped JSON decoding for the pinned event
  schema.
- Normalize durable step events to `llm_call` and tool/shell events to
  `action_span`.
- Enrich rather than double-count matching shell and generic tool events.
- Use durable event IDs and aggregate sequence numbers for deduplication.
- Preserve unknown events or ignore them with explicit counters.

Suggested locations:

```text
crates/af-opencode/        # if transport + protocol justify a crate
collectors/opencode/       # operational docs/config
```

Do not create a new crate until the captured fixtures prove the boundary is
large enough to deserve one; an `af-cli` module is acceptable for the spike.

#### OC-3 — Live SSE adapter

- **Status:** `done`
- Add a reconnecting SSE subscriber with bounded buffering.
- Persist the last durable sequence per session/aggregate only after emitted
  facts are durably spooled.
- Detect unsupported schema/protocol versions and surface a health gap.
- Avoid making the OpenCode agent depend on `af` availability.

#### OC-4 — Process attribution decision

- **Status:** `done`
- First implement root-process-tree observation plus overlapping tool spans.
- Measure ambiguity under concurrent shell tools/background processes.
- If insufficient, prepare an upstream proposal for optional PID on
  shell-start events; do not maintain a private OpenCode fork by default.
- Record the accepted fidelity in the capability descriptor.

#### OC-5 — Deterministic tests

- **Status:** `done`
- Add sanitized SSE fixtures with durable sequence metadata.
- Test replay, reconnect, duplicate events, sequence gaps, unknown event types,
  retries, failures, provider-executed tools, and shell/tool enrichment.
- Test exact usage mapping including reasoning/cache tokens.
- Test no double counting after reconnect.

#### OC-6 — Live test

- **Status:** `done`
- Add `live_opencode.rs` using `common/live.rs`.
- Start or connect to an isolated OpenCode server in a temporary project.
- Verify one exact `llm_call`, one action span, session metadata, reconnect
  health, and joined output.
- Add OpenCode dispatch to `scripts/test-live.sh`.

Required validation before `done`:

```sh
cargo test -p af-events -p af-spool -p af-store -p af-core -p af-cli --no-fail-fast
cargo fmt --all -- --check
git diff --check
scripts/test-live.sh opencode
```

#### OC-7 — Closeout

- **Status:** `ready`
- Update protocol-affinity findings from real traces.
- Document server startup, event subscription, version pinning, and gaps.
- Add README/user-guide links.
- Record final evidence in `docs/agent-integrations/opencode.md`.

### Integration 2 — Codex

- **Integration status:** `done`
- **Decision log:** `docs/agent-integrations/codex.md`
- **Source version researched:** installed Codex CLI `0.142.0`

#### CX-1 — Capture app-server and OTLP traces

- **Status:** `done`
- Generate and save the app-server schemas with the exact Codex version.
- Capture a real session containing command execution, file changes, MCP,
  failure, cancellation, delegation, and multiple model turns.
- Capture the same session's OTLP stream.
- Determine correlation keys and whether token notifications represent calls,
  turns, or cumulative totals.
- Record findings in `docs/agent-integrations/codex.md`.

Block the Codex implementation if observation requires replacing ordinary TUI
workflows and no acceptable managed-launch/proxy mode can be selected without a
user decision. Preserve fixtures and finish documentation before stopping.

#### CX-2 — Offline app-server normalizer

- **Status:** `done`
- Decode only the pinned, generated v2 schemas needed for impact accounting.
- Map thread/turn/item lifecycle to session/task/action facts.
- Emit usage only from demonstrated per-operation deltas.
- Treat cumulative token totals as cross-checks.
- Preserve protocol/version provenance.

#### CX-3 — Choose runtime topology

- **Status:** `done`
- Evaluate, in order:
  1. passive hooks plus OTLP for ordinary TUI sessions;
  2. `af`-managed app-server launch;
  3. transparent app-server proxy;
  4. `exec --json` for noninteractive sessions only.
- Select the least-coupled topology that still provides honest lifecycle.
- If the choice materially changes the user workflow, checkpoint and defer the
  decision rather than silently selecting it.

#### CX-4 — Deterministic tests

- **Status:** `done`
- Add generated-schema fixtures and sanitized app-server/OTLP traces.
- Test usage delta handling, cumulative cross-checks, retries, cancellation,
  unknown item kinds, duplicate notifications, and source deduplication.
- Add a separate `exec --json` fixture path if implemented.

#### CX-5 — Live test

- **Status:** `done`
- Add `live_codex.rs` using `common/live.rs`.
- Preflight the Codex version, auth, and selected runtime topology.
- Verify thread/session metadata, one command/file action, one usage fact, and
  debug health.
- Add Codex dispatch to `scripts/test-live.sh`.

Required validation before `done`:

```sh
cargo test -p af-events -p af-spool -p af-store -p af-core -p af-cli --no-fail-fast
cargo fmt --all -- --check
git diff --check
scripts/test-live.sh codex
```

#### CX-6 — Closeout

- **Status:** `done`
- Record the accepted operational coupling and unsupported TUI modes.
- Add installation/user documentation only for the topology actually tested.
- Update the affinity matrix and `docs/agent-integrations/codex.md`.

## 4. Cross-integration completion criteria

The autonomous loop is complete when each integration is either:

- `done`, with deterministic tests, manual live evidence, docs, and a final
  decision log; or
- `blocked`, with reproducible evidence, a recommended decision, and an exact
  resume point.

After all three integrations have been visited:

1. update `docs/checkpoint-2026-07-26.md` or create a dated successor;
2. summarize capability coverage and gaps across collectors;
3. list live tests that could not run and why;
4. stop before unrelated iteration-2 structural refactors.
