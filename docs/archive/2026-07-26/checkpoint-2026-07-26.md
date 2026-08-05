# Project checkpoint — 2026-07-26

- **Branch:** `poc-iteration-1`
- **Code HEAD at checkpoint:** `1392de63d1c0b859b440c6aec8b39b70bd74ac86`
- **HEAD summary:** `fix(console): usability & consistency cleanup (Phase 2 Package A)`
- **Purpose:** compact handoff after the architecture/performance review and
  the parallel reuse/efficiency/console batches
- **Expected uncommitted files at checkpoint:** this document, the active
  performance plan, the CodeCarbon evidence note, their README links, the
  dirty-path ingest implementation, and unrelated parallel console edits

This is the restart document for the next review or implementation session.
It distinguishes completed work, current scope, deferred work, and the
remaining priorities that must not be lost after context compaction.

## 1. Completed foundation

### Architecture and implementation

- Contract #1 event envelope and validation.
- Append-only JSONL collector transport with incremental byte offsets.
- SQLite raw/derived separation and deterministic replay.
- Remote EcoLogits estimation with explicit failure states.
- Local energy/process sampling and Rust attribution policies.
- Session/task/tool impact joins.
- Claude Code hooks and native OTLP ingestion.
- CLI report/replay/watch/statusline surfaces.
- Debug HTTP/SSE API and embedded console.

### Review batch fixes already landed

- shared quarantine, filename, timestamp, criterion, zone, PID, policy, Host,
  and sidecar-resolution mechanisms;
- removed dead code and parallel per-session state maps;
- touched-session watch rebuilds with zone-change full-rebuild guard;
- one SQLite connection and one touched-session event load per watch pass;
- schema-v2 indexes;
- cached Python health checks;
- bounded resident dedup/debug structures;
- Claude hook subprocess reduction from roughly 13 to 4 `jq` invocations per
  tool call;
- shared Rust/CLI fixtures and harnesses;
- cross-language sanitizer conformance vectors;
- console usability/consistency cleanup through Phase 2 Package A.

Relevant landed commits:

```text
7b8f078 refactor(core,store,spool,otlp,sidecar): shared primitives
a63a6af test: shared Contract #1 fixtures and production validator
d250987 perf(cli,core): resident-watch efficiency
20c3968 test(cli): shared integration harness
3d5fd95 refactor: shared collector rules and hook fork reduction
be7c103 docs: iteration-2 structural backlog
1392de6 fix(console): usability & consistency cleanup (Phase 2 Package A)
```

## 2. Current active scope

### Implemented in the current worktree

- internal ingest metrics for discovery, tailing, validation, insertion,
  offsets, bytes, lines, deduplication, and total duration;
- deterministic full-scan and targeted scaling fixtures;
- dirty-path targeted ingestion after notification debounce;
- no-I/O two-second sampler-supervision ticks;
- 30-second full-directory reconciliation plus immediate reconciliation after
  watcher errors or failed ingest passes;
- skipped empty insert transactions and unchanged offset writes;
- cached spool file/offset state for debug health publication.

The initial benchmark reduced a one-file append with 1,000 historical spool
files from 1,000 file opens / 1,000 offset reads / 28.184 ms to one file open /
one offset read / 0.171 ms on the development machine. Treat this as scaling
evidence, not a stable performance claim; details and limitations are in the
action-plan document.

The release-mode deterministic matrix now covers all five planned cases with
100 samples and p50/p95/p99 reporting. On the development machine, the
1,000-file single-append case measured p99 39.070 ms for full scan versus
0.256 ms for targeted ingest; the 100-file burst measured p99 22.933 ms versus
19.932 ms because every file was dirty. Full results and limitations are in
the action-plan document.

The remaining evidence scope is deliberately narrow:

1. measure cold-cache behavior where the environment permits;
2. add partial-line and forced-reconciliation variants;
3. exercise realistic day-scale, concurrent-session, OTLP, and restart
   traces;
4. measure append-to-raw and append-to-join latency;
5. collect real spool growth, duplication, and compression evidence;
6. confront lifecycle alternatives only with that evidence.

The detailed plan is
[`action-plan-performance-evidence.md`](action-plan-performance-evidence.md).

### Explicitly not in the current scope

- integration with the in-progress CodeCarbon Rust core;
- changing the current sampler topology;
- JSONL rotation, deletion, compaction, or retention;
- Parquet archive implementation;
- redesigning Contract #1;
- broad structural refactors from the iteration-2 backlog.

## 3. CodeCarbon cross-project evidence

The one-machine-sampler-per-session finding remains important evidence but is
not an agentic-footprint task for the current scope.

The asynchronous evidence note is
[`evidence-codecarbon-rust-boundary.md`](evidence-codecarbon-rust-boundary.md).
It records:

- the concurrent-session double-measurement risk;
- useful hardware/workload/attribution responsibility boundaries;
- accounting-oriented data requirements;
- realistic agent and ML workload fixtures;
- open questions for the CodeCarbon Rust rewrite;
- a shared timeline in which real traces precede any future integration.

Do not turn that note into an integration plan without an explicit new scope
decision.

## 4. Completed follow-up batch

### P0/P1 — correctness and resident-loop risks

#### Asynchronous estimator worker

`af watch` now delegates estimator and zone-factor Python I/O to one resident
worker with a separate SQLite connection. The database is the durable queue;
capacity-one channels coalesce wakeups and completions, and only sessions whose
estimates changed are rebuilt. A three-second fake estimate no longer delays
ingestion of another session. `af report` and `af replay` remain synchronous.

#### Machine sampling topology

One Python machine-mode sampler per session remains a possible duplicate-energy
correctness problem for concurrent sessions. It is intentionally tracked in
the CodeCarbon evidence note rather than the active agentic-footprint plan.

#### Local grid and remote inference region

The machine-local grid zone and remote inference region are no longer the same
setting. `--local-grid-zone` (with compatibility alias `--zone`) and
`AF_LOCAL_GRID_ZONE`/`AF_ZONE` govern local factors. `--remote-region` and
`AF_REMOTE_REGION` are optional audited overrides; otherwise the estimator
owns remote-region detection and the request omits `electricity_mix_zone`.
Stored estimates carry `remote_region` provenance. Full host/session zone
partitioning is explicitly deferred because the current state directory is
machine-local and stable-location deployments are the target usage.

#### Generic OTLP normalization

`af-otlp` now has the target registry shape:

```text
OTLP transport
  -> generic OTLP record/batch representation
  -> registered normalizer
       - Claude Code
       - OTel GenAI semantic conventions
       - future agents/frameworks
  -> Contract #1 envelopes
```

A generic decoder flattens resource, scope, and record attributes once.
Ordered Claude Code and standards-shaped GenAI log normalizers claim records
independently. Health separates accepted, claimed-but-dropped, and valid but
unclaimed records, and publishes normalizer capability descriptors. Trace
timing is not fabricated from logs; `/v1/traces` remains future work.

#### Unknown-event preservation

Unknown event types with a valid stable envelope are preserved verbatim in the
schema-v3 `opaque_events` table, advance their spool offsets, and are excluded
from typed derivation. `parse_line` remains strict for typed callers;
`parse_line_preserving_unknown` is the ingest boundary. Invalid JSON and invalid
base envelopes still quarantine.

#### Typed and versioned derived contract

`ImpactJoin` remains ad hoc JSON assembled in Rust and consumed by the console.
A typed, versioned contract is needed before additional presentation or remote
consumers make the implicit shape harder to change.

This work should define:

- schema/version identity;
- stable required fields;
- extension policy;
- unknown-field behavior;
- migration/replay expectations;
- Rust and TypeScript generated or shared representations where practical.

#### Session/host-scoped context

Zone, host, sensor, and possibly methodology context should be explicit on the
facts and derived units they govern. Avoid another global pass-level setting
that becomes ambiguous once stores merge data from several machines.

#### Collector capabilities

Installed normalizers now advertise signal, emitted Contract #1 types, and
lifecycle fidelity. Current log normalizers honestly declare completed
operations only. Rich live lifecycle semantics remain iteration-2 work.

## 5. Remaining priorities

### P2 — recorded iteration-2 structural backlog

Already documented in `docs/design-log.md`:

- first-class bootstrap span instead of `"__session__"`;
- one exhaustive span-classification enum;
- typed/versioned `ImpactJoin`;
- one ingest normalization stage;
- `SampleOutcome` enum for honest sample outcomes;
- sub-quadratic apportionment;
- schema-native estimator criterion names.

Avoid implementing these opportunistically inside unrelated performance or
genericity changes. Group them into explicit review units.

## 6. Remaining order

Unless new evidence changes priorities:

1. review real spool growth and I/O evidence before lifecycle decisions;
2. decide whether `/v1/traces` is needed for live GenAI operations;
3. stop before the recorded iteration-2 structural refactors;
4. revisit CodeCarbon integration only from an explicit cross-project scope.

## 7. Validation checkpoint

Validation completed during the review before this checkpoint:

- Rust unit and non-network integration suites passed.
- `af-cli` watch integration suite passed when permitted to bind loopback
  ports.
- Claude hook suite: **67/67 passed**.
- Statusline shell suite: **24/24 passed**.
- Python tests were not run in that environment because the available Python
  interpreter did not have `pytest`; no dependency installation was performed.

Because the console cleanup commit landed after the earlier Rust review, run
the repository's normal console unit/e2e/lint commands before modifying console
contracts in a future session.

Validation for the dirty-path implementation:

- `cargo test -p af-spool -p af-store -p af-cli --no-fail-fast`: all unit and
  non-network integration tests passed; the first watch run identified the
  macOS `/var` versus `/private/var` notification alias and a regression test
  now covers equivalent parent paths;
- `cargo test -p af-cli --test watch --no-fail-fast` with loopback permission:
  **9/9 passed**;
- deterministic full-scan and targeted ignored benchmark fixtures passed;
- `cargo fmt --all` and `git diff --check` passed;
- parallel console changes visible in the worktree were not modified or
  validated as part of this batch.

Validation for the completed pre-iteration-2 follow-up batch:

- `cargo test -p af-events -p af-spool -p af-store -p af-core -p af-otlp \
  -p af-cli --no-fail-fast` with loopback permission: all tests passed;
- `af-cli` watch integration suite: **10/10 passed**, including the
  three-second slow-estimator non-blocking regression;
- `af-otlp` receiver suite: **22/22 passed**, including real Claude capture,
  standards-shaped GenAI logs, unclaimed records, and capability descriptors;
- `af-store` migration suite passes through schema version 3 and confirms
  opaque events survive separately from typed facts;
- `cargo fmt --all -- --check` and `git diff --check` passed;
- iteration-2 structural topics listed above were not modified.

## 8. Restart procedure

At the start of the next implementation session:

1. read this checkpoint;
2. read `docs/action-plan-performance-evidence.md`;
3. inspect `git status` because parallel agents may have changed the worktree;
4. review the remaining evidence matrix or select the next P1/P2 priority;
5. debrief the proposed next runtime fix before modifying it;
6. preserve unrelated concurrent edits;
7. update this checkpoint or create a dated successor when the active batch is
   completed.

## 9. Documentation map

- Coding-agent integration optimization restart checkpoint:
  `docs/checkpoint-agent-integrations-optimization-2026-07-26.md`

- Claude Code installation and operation:
  `docs/claude-code-user-guide.md`
- Overall architecture: `docs/rfc/0001-architecture.md`
- Rebuild/data flow: `docs/rebuild-architecture.md`
- Active performance plan: `docs/action-plan-performance-evidence.md`
- CodeCarbon async evidence: `docs/evidence-codecarbon-rust-boundary.md`
- Coding-agent protocol comparison:
  `docs/research-coding-agent-protocol-affinity-2026-07-26.md`
- Ordered autonomous integration queue:
  `docs/action-plan-agent-integrations.md`
- Machine-readable integration task state:
  `docs/agent-integrations/tasks.json`
- Historical decisions and structural backlog: `docs/design-log.md`
