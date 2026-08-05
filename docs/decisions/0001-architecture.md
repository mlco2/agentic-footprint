# RFC 0001 — agentic-footprint: architecture & contracts

!!! note "Historical decision record"
    This RFC captures the initial architecture. Current user and contributor
    documentation takes precedence where the implementation has evolved.

- **Status:** Draft for team review
- **Date:** 2026-07-25
- **Audience:** internal (ecologits / codecarbon contributors); structured so it can graduate later
- **Project name:** `agentic-footprint` is a placeholder — see [Open questions](#open-questions)

## 1. Purpose

Instrument agents — coding agents first (Claude Code, then Codex CLI, Gemini CLI, opencode…),
domain agentic apps second — so they report **energy consumption and
environmental impacts** (energy, GHG, water, resources) in a standard way, with all the
nuance the underlying methodologies can express: uncertainty ranges, measured vs modeled
values, life-cycle phases, and explicit gaps ("we did not measure this").

**Why not token-only accounting:** token totals capture remote inference but hide the
end-to-end compute of agentic work — local tool execution, retries, verification
cycles, orphaned processes. The token-only view is what the current statusline offers;
this architecture exists to measure the complete workflow: remote inference estimates
*and* measured local energy, correlated per task.

Built on the two reference projects of the team:

- **[EcoLogits](https://github.com/genai-impact/ecologits)** — impact estimation of remote
  LLM API inference (methodology + model/electricity-mix data).
- **[CodeCarbon](https://github.com/mlco2/codecarbon)** — measurement of local hardware
  energy (RAPL, NVML, powermetrics, TDP fallbacks).

## 2. Goals and non-goals

**Goals (iteration 1, PoC on Claude Code):**

- A standard **event schema** (Contract #1) that any collector can emit and the litellm
  workstream can build against independently.
- A standard **local transport** (JSONL spool) between collectors and the control plane.
- A **Rust control plane** that ingests raw facts, owns all estimation methodology,
  aggregates at session / task / tool granularity, and exposes results locally.
- A Claude Code collector and an adapted `ecologits-statusline` as the first presentation
  consumer.

**Non-goals (deferred, but must not be precluded):**

- Persistence layer / organisation dashboards (a future remote consumer of the read model).
- L3 cgroup-based attribution (see §8).
- Estimation factors for remote tool execution (see §8).
- Multi-runtime native bindings (pyo3/napi) — binary-first distribution comes first (§10).
- Normative read model — Contract #2 stays informative in v1 (§5).

## 3. Architecture

Three decoupled layers, two contracts:

```
COLLECTORS (per-agent, any language, N processes — emit RAW FACTS only)
  • cc-collector: Claude Code (hooks; parsing informed by ccusage adapters)
  • codecarbon sampler: local hardware energy (Python, lifecycle-managed
    by the control plane, logically still a collector)
  • framework collectors for domain agentic apps: litellm middleware
    first (parallel workstream, another dev, autonomous), pydantic-ai
    and others to follow — each conforms to Contract #1, no other
    coupling; the design depends on none of them
  • future: generic ACP-proxy collector (see Annex A)
        │
        │  CONTRACT #1 (normative): event schema + JSONL spool transport
        ▼
CONTROL PLANE (standard core, one Rust binary)
  • ingests & validates spool events; quarantines, never silently drops
  • owns ALL estimation methodology (ecologits sidecar; grid factors on joules)
  • provisions & supervises a managed Python env (uv-based, §7)
  • correlates session/task/tool; joins spans × energy (attribution policy, §8)
  • stores raw facts + derived estimates separately (SQLite) → replayable
        │
        │  CONTRACT #2 (informative in v1): read model (JSON)
        ▼
PRESENTATION (per-surface, custom)
  • ecologits-statusline (queries control plane instead of EcoLogits API)
  • CLI reports · future org dashboards via persistence layer
```

### Load-bearing principles

1. **Facts / estimates separation.** Collectors never compute impacts. All methodology
   lives in the control plane. Consequences: methodology updates never touch collectors;
   every estimate is reproducible; stored raw facts can be **re-estimated** when factors
   improve.
2. **Offline-first methodology.** The control plane works without network; estimates
   record `methodology_version` + source.
3. **Failure honesty.** The control plane never invents a number. Missing sampler →
   "not measured", not zero. Unknown model → `estimation_status: unknown_model`, surfaced.
   Remote spans → counted and labeled unmeasured. Expressing what we *don't* know is part
   of the standard.
4. **Custom edges, standard core.** Collectors and presentation are per-agent custom;
   the control plane is the portable standard core.

## 4. Contract #1 — event schema

Full JSON Schemas: [`schemas/v0.1/events.schema.json`](../../schemas/v0.1/events.schema.json).
Summary below. Raw facts only; **no impact numbers in collector events.**

### Envelope (all events)

| Field | Req | Notes |
|---|---|---|
| `schema_version` | ✓ | semver of this standard (`0.1.0`) |
| `event_id` | ✓ | ULID/UUID, collector-generated |
| `type` | ✓ | one of the five payload types |
| `ts` | ✓ | RFC 3339; interval events also carry `t_start`/`t_end` in payload |
| `collector` | ✓ | `{name, version}` |
| `session_id` | ✓ | the only mandatory correlation key |
| `attribution` | — | `{agent_id?, subagent_id?, task_id?, tool_call_id?}` — deepens correlation to task/tool level when observable |

### Payload types

- **`llm_call`** — remote inference facts: `provider`, `model_id_requested`,
  `model_id_served?`, token usage with ACP-aligned names (`input_tokens`, `output_tokens`,
  `thought_tokens`, `cached_read_tokens`, `cached_write_tokens`), `duration_ms?`,
  `status`, and `usage_source` (`api_response` | `agent_telemetry` | `transcript` |
  `estimated`). **Source reliability is ranked** (Annex B): collectors MUST prefer the
  most authoritative source available — API responses, then agent-native telemetry
  (OTel export), then transcripts (known corruption/undercount issues), then estimates.
  Impact figures derived from transcript-only usage are flagged in the read model:
  carbon estimates on corrupted token counts are mathematically invalid.
- **`energy_sample`** — local measured energy over `[t_start, t_end]`: per-component
  joules (`cpu_j`, `dram_j`, `gpu_j`, `total_j`) with a per-component `method`
  (`rapl` | `powermetrics` | `nvml` | `tdp_model`) so measured vs modeled stays
  distinguishable.
- **`action_span`** — an agent action: `span_id`, `tool_name`, `tool_kind`
  (`bash` | `mcp` | `file_op` | `subagent` | `other`), `execution_locus`
  (`local` | `remote` | `hybrid` | `unknown`), `t_start`/`t_end`, observed `pids[]`,
  optional `cgroup`. Overlapping spans are legal — concurrency is data.
- **`process_sample`** — per-pid-tree CPU-time / memory / IO deltas over an interval:
  the L2 apportionment weighting signal.
- **`session_meta`** — agent app `{name, version}`, host hardware profile (CPU/GPU model
  for TDP fallback), OS, **user-configured** geo zone for grid intensity (never
  auto-geolocated), power source (battery/AC) if known.

### Transport (normative)

- Spool directory under an XDG-style state path (e.g.
  `~/.local/state/agentic-footprint/spool/`).
- One append-only JSONL file per collector+session: `<collector>.<session_id>.jsonl`.
- One event per line; atomic appends (single `write()` of a full line).
- Collectors only append; the control plane ingests (incremental byte offsets), archives,
  and owns retention. Malformed lines go to `rejected/` with a reason.
- Works from a shell one-liner; buffers for free when the control plane isn't running.

## 5. Contract #2 — read model (informative in v1)

Derived outputs of the control plane (`schemas/v0.1/derived.schema.json`):

- **`impact_estimate`** — per `llm_call`: the **full EcoLogits criteria set** — energy,
  GWP (CO₂eq), ADPe, PE, water — each as `{min, max}` ranges, split usage / embodied
  phases where the methodology provides them; stamped `methodology_version` +
  `methodology_source`.
- **`impact_join`** — per attribution unit (session / task / tool call) and interval:
  `local_measured` component (from energy samples via the attribution policy) +
  `remote_estimated` component (from impact estimates), a combined total **explicitly
  labeled as crossing measurement paradigms** (measured Wh + modeled Wh with ranges
  preserved), the `attribution_policy` id, and `unmeasured_remote_spans` count.
- Aggregates by session / task / tool / model, exposed via `report --format json|text`.

Contract #2 becomes normative in a later RFC, aligned with the ongoing GSF SCI → OTel
semantic-conventions normalization (a team member sits in that expert group; validated
semantics will be introduced as they land). The future persistence layer is just another
consumer of this read model.

## 6. Control plane

One Rust binary, two modes over the same core:

- **`report`** (on-demand): ingest spool → update state → emit results → exit. What the
  statusline calls. No daemon needed for the PoC.
- **`watch`** (resident): fs-watch the spool, ingest continuously, supervise Python
  sidecars (live hardware sampling needs a resident parent).

Pipeline (identical in both modes):

1. **Ingest** — read spool files incrementally (per-file byte offsets in state), validate
   against the schema version, quarantine malformed lines to `rejected/` with reasons.
2. **Correlate** — build the session/task/tool attribution tree; join `action_span` ×
   `energy_sample` × `process_sample` under the declared attribution policy (§8).
3. **Estimate** — batch `llm_call` facts to the ecologits estimator sidecar (warm
   process, JSON over stdio); apply grid-intensity factors to measured joules.
4. **Aggregate & store** — single SQLite file under the state dir; raw event archive and
   derived estimates stored separately, so a methodology update is a pure replay.
5. **Expose** — `report --format json|text` (Contract #2).

### Ingestion adapters & the OTLP stance

Where an agent natively exports OpenTelemetry (Claude Code, Gemini CLI, Codex), that
export is the **most reliable usage signal** and is consumed as-is: the control plane
embeds a **minimal local OTLP receiver** (`localhost` endpoint the agent's exporter
points at) as an *ingestion adapter* that normalizes incoming telemetry into Contract #1
events. Direction: **OTLP-in where native, spool inside, OTLP-out later.**

A full "OTLP gateway architecture" (local OTel agents forwarding to a central gateway
for fusion) was considered and rejected for v1:

- a central gateway *is* the persistence layer — out of scope, and contrary to the
  local-first constraint (nothing leaves the machine in v1);
- fusing machine energy with action spans requires the raw local facts — centralizing
  that is a privacy regression, and doing it locally is precisely the control plane;
- mandating OTLP as the universal internal transport raises the collector floor (a
  shell hook can append a JSONL line, not speak OTLP) and excludes sources with no
  OTLP story (codecarbon, agents without OTel) — the fragility would move, not vanish.

OTLP-out (exporting the read model as OTel metrics, aligned with the GSF SCI semantic
conventions as they land) is the planned bridge to org observability, alongside the
future persistence layer.

### Watchdog & debugging interface

The sampler's process watching doubles as a **watchdog**: watched pid trees that
outlive their `action_span` are flagged `orphaned`, and their continued energy is
reported as orphaned local compute — waste that is invisible to token-only monitoring.
For the development phase, the control plane exposes a **debug interface**
(`debug` command / `watch --debug`): a live view of incoming events, open spans,
watched pids, attribution decisions and orphan flags — the primary tool for validating
collectors while building them.

### Estimation & methodology data

- Estimation executes in the **ecologits estimator worker** — a managed Python sidecar
  (JSON over stdio, warm process), so methodology fidelity is guaranteed (no ported
  formulas, zero drift).
- Methodology **data** (model params, electricity mixes, hardware factors) is a
  **versioned artifact**. Distribution rides the team's ongoing periodic
  data-versioning + publication routine for codecarbon data, extended to include
  ecologits data. Resolution order: bundled snapshot (offline baseline) → local dataset
  file → public or **self-hosted** EcoLogits API refresh. Every estimate records which
  version and source it used.
- Control plane releases pin the `ecologits`/`codecarbon` versions they pair with
  (release manifest), keeping binary + methodology reproducible. Crate/binary releases
  are cut manually when needed.

## 7. Managed Python runtime

The control plane provisions and supervises a real Python environment (decision:
**no Monty** — it targets sandboxing of untrusted LLM-generated code, cannot import C
extensions (`psutil`, `pydantic-core`), blocks filesystem/subprocess access that
codecarbon requires, and is experimental; our Python code is trusted and ours).

- **`agentic-footprint python setup`** — drives a pinned `uv` (vendored or fetched once):
  `uv python install` (python-build-standalone) → isolated venv under the state dir →
  hash-locked `ecologits` + `codecarbon` installs pinned by the release manifest. No
  system Python touched.
- **`agentic-footprint python doctor`** — diagnoses (missing env, drift, no network,
  no cache) with actionable fixes.
- **Graceful degradation:** without the env, token facts still collect; estimation is
  `pending` and backfilled after setup (replay).
- **Constrained environments:** mirror env vars (`UV_PYTHON_INSTALL_MIRROR`, private
  indexes), documented offline seed bundle, and `python.env_path` escape hatch (bring
  your own env). First-run provisioning otherwise requires network — stated explicitly.
- **Sidecar contract:** Python components speak JSON over stdio and stay dumb. The
  codecarbon sampler receives a **watch-list** (pids/cgroups) as spans open/close; the
  join logic stays in Rust.
- Rejected alternative: PyO3/libpython embedding (cross-platform ABI coupling in a
  distributed binary).

## 8. Per-action energy attribution

Ambition: measure and segregate agent actions (e.g. one tool run). Physics constraint:
hardware counters measure the **machine**; per-action energy is always machine energy ×
an **apportionment policy** over time and processes. The raw-facts architecture makes
that policy a control-plane concern — versioned, recorded in every derived result, and
re-appliable to history.

Accuracy ladder (each level a declared `attribution_policy`):

| Level | Policy | Status |
|---|---|---|
| L1 | wall-clock slicing: energy in `[t_start, t_end]` → span | fallback; wrong under concurrency |
| **L2** | CPU-time weighting: overlapping spans share each sample ∝ their process-tree CPU delta; **active vs idle is explicit** — the unattributed remainder is reported as `baseline/idle`, never spread over actions | **v1 target** |
| L3 | cgroup isolation per tool (Linux) — measured shares | deferred; schema already carries `cgroup`; test L2 first, decide later |

- **Async/overlapping tools:** overlap is data. Spans overlap freely; apportionment is
  the control plane's job; derived records always carry the policy id.
- **Remote tools** (remote MCP servers, web fetches, cloud sandboxes): locally
  unobservable. `execution_locus: remote` spans are **excluded from the local energy
  join** (no silent double counting) and reported as unmeasured remote activity
  (duration + counts). Future: per-service estimation factors; further out, the standard
  could define a footprint self-declaration convention for remote services.
- **CodeCarbon implication:** requires a **multi-process observer** capability (sample
  machine energy + watched pid trees) that codecarbon lacks today
  (`tracking_mode="process"` only apportions its own process; `start_task`/`stop_task`
  are sequential machine intervals) — a natural upstream contribution by the team. The
  RFC treats it as the sampler capability contract, including **orphan detection**:
  watched pid trees outliving their span are flagged and their energy reported as
  orphaned compute (see §6, watchdog).

## 9. Existing components: reuse map

| Component | Role here | Notes |
|---|---|---|
| **ccusage (native Rust rewrite, local clone)** | token-facts parsing backbone for collectors | MIT; per-agent adapters already cover 16 agents (claude, codex, gemini, copilot, opencode, goose, amp, droid, pi, qwen, kimi…), each owning log discovery/parsing/token+model mapping. Currently binary-only (no `lib` target, v0.0.0, unpublished): either **vendor the adapter module** or (preferred) **upstream a PR splitting adapters into a library crate**. |
| **ecologits** | estimation methodology + data | runs as managed sidecar; data joins the publication routine |
| **codecarbon** | local energy sampler | lifecycle-managed collector; needs multi-process observer upstream (§8) |
| **ecologits-statusline** | first presentation consumer | switches from EcoLogits API to control-plane `report` |
| **framework collectors (domain apps)** | collectors for agentic frameworks | framework-agnostic by design: any middleware/callback layer that emits Contract #1 events is a conformant collector. litellm is simply the first (parallel autonomous workstream; Contract #1 is the interface); pydantic-ai is the next candidate, others as opportunities arise. |
| **ACP** | future generic collector + schema alignment | see Annex A |

## 10. Distribution

**Binary-first, à la uv/ruff:** one static binary (CLI + `watch`), shipped via cargo,
Homebrew, and thin npm/PyPI wrapper packages vendoring the binary. Integrations shell out
or read its output. Native bindings (pyo3, napi) only
when a consumer genuinely needs in-process embedding (Python framework
collectors like litellm or pydantic-ai are plausible first cases) — not before. Spec-first multiple
implementations rejected: methodology consistency is the point of centralizing.

## 11. Iteration 1 scope

**Agent scope is restricted to native-OTel agents.** The PoC targets Claude Code;
Gemini CLI is the named second target (native OTel *and* native ACP). Rationale: their
first-party telemetry bypasses the local-transcript data-loss bugs entirely (Annex B) —
no fragile workarounds in the critical usage path. Transcript parsing is demoted to
cross-check/fallback (`usage_source: transcript`, flagged).

**In:**

- Schema v0.1 (5 event types + envelope) as JSON Schema; spool transport spec.
- Control plane `report` mode: ingestion (spool + local OTLP receiver adapter),
  correlation, SQLite state, ecologits sidecar, `python setup`/`doctor`, L2 attribution.
- Claude Code collector: **OTel-primary for usage**, hooks for action spans and session
  lifecycle.
- **Watchdog + debug interface** (§6): orphan detection and the live `watch --debug`
  view — the primary development-phase tool for validating collectors.
- Adapted statusline.

**PoC acceptance scenario — measure a full workflow:** the environmental cost of one
complete end-to-end task (e.g. resolving a real GitHub issue), reported as an
`impact_join`: remote inference estimates + measured local tool energy + retries and
verification cycles + any orphaned compute — demonstrating what token-only accounting
cannot see.

**Out:** persistence/dashboards, L3, remote-tool estimation, `watch` polish beyond the
debug view, other agents' collectors, bindings.

**Prerequisite:** verify the load-bearing findings of the research report
([`research/`](../../research/)) against primary sources — especially the reliability
ranking of Claude Code usage sources and hook payload details.

## Open questions

1. **Project name.** `agentic-footprint` is a placeholder; team poll ongoing. Naming
   axes explored: metrology (agentmeter), physics units (joule/erg), footprint/trace,
   `open-*` standards naming, ledger/accounting, sober nature names (bio-indicator
   organisms). Key decision: same name for spec and tool (OpenTelemetry model) vs split
   (descriptive spec name + short rtk-like binary). Constraints: short binary, package
   availability, enterprise-serious, no language/vendor reference, EN/FR pronounceable.
2. **Methodology data channel.** Exact artifact format & hosting once the codecarbon
   data publication routine design lands (HF dataset vs package vs API remains open).
3. **ccusage integration.** Upstream lib-split PR vs vendoring; verify per-adapter data
   quality against Annex B issues.
4. **L3 timing.** Revisit after L2 field results.
5. **Contract #2 normativity.** Tied to SCI/OTel semconv progress.
6. **Windows spool semantics.** Atomic append + path conventions to validate.

## Annex A — ACP alignment

The Agent Client Protocol's "Session Usage and Context Status" RFD defines
`usage_update` notifications and `PromptResponse.usage` (`input_tokens`, `output_tokens`,
`thought_tokens`, `cached_read_tokens`, `cached_write_tokens`) — our `llm_call` token
vocabulary is aligned with it by construction. Still `unstable_session_usage`; tokens
only (no local energy, not always model routing). Claude Code, Codex (codex-acp) and
Gemini CLI speak ACP natively or via adapters → **one ACP-proxy collector could cover
any ACP-speaking agent generically**. Mapping table to be maintained in the schema repo;
ACP is an upstream *source* for collectors, not a replacement for Contract #1 transport.

## Annex B — reliability of local usage-data sources

**Principle: local disk transcripts are not trustworthy for usage accounting.** They
are written during streaming for UI responsiveness, not audit integrity. A footprint
pipeline built on parsing them produces mathematically invalid estimates. Evidence:

- **Claude Code transcripts** (`~/.claude/projects/<project>/<session>.jsonl`): input
  tokens recorded as placeholders (anthropics/claude-code#28197), output tokens frozen
  at early streaming values (#22686), thinking tokens omitted — undercounts up to ~two
  orders of magnitude. Community tools parsing them (ccusage et al.) inherit the flaws.
- **Codex CLI**: SQLite state (`state_5.sqlite`) desynchronizes from rollout JSONL
  files; final buffers are dropped on exit (openai/codex#16897, #22452).
- **Race conditions**: Claude Code's SubagentStop hook reportedly fires before the
  subagent transcript is flushed (#25121) — synchronous reads get stale data.

**Resulting source ranking** (encoded in `usage_source`, collectors MUST prefer the
highest available):

1. `api_response` — usage returned in-band by the provider API (litellm-style
   middleware sees this directly).
2. `agent_telemetry` — agent-native OTel export (e.g. `claude_code.token.usage`),
   captured from the agent's memory space after stream completion.
3. `transcript` — local disk logs; cross-check/fallback only, flagged downstream.
4. `estimated` — reconstructed counts; last resort, flagged.

Findings sourced from the research report in [`research/`](../../research/); load-bearing
claims to be re-verified against primary sources before implementation (§11).
