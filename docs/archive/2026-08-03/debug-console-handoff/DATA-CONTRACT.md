# Data contract — wiring the console to real `af` data

**This is the main implementation document.** The prototype fakes every number. This file
names the real source of each one, the endpoints that must exist, the client components to
build, and — most importantly — **what does not exist in the control plane yet**.

Read §1 first. Several panels are designed against capabilities that are internal to the
control plane today and need serialising before any UI work can start.

---

## 0. The load-bearing rule

> **The console computes nothing.** No impacts, no attribution, no apportionment, no
> unit conversions that carry methodology.

RFC §3, principle 1: collectors never compute impacts; all methodology lives in the control
plane. A presentation layer is under the same constraint. Every number the console shows
must arrive pre-computed, stamped with `attribution_policy` and `methodology_version`.

The prototype **violates this** — it computes L2 shares, grid conversions and range
arithmetic in the browser (`allocFor()`, `totals()` in its logic class). That was the only
way to make it move without a backend. **Deleting that client-side maths and replacing it
with fetched values is the single largest task in this port.** If a reviewer can point at a
joule the browser calculated, the port is not done.

---

## 1. Gap list — control-plane work required first

Ordered by how much of the UI they unblock. "In scope" = already named in RFC §11
iteration-1 scope or `docs/design-log.md`; "New" = not currently planned.

| # | Capability | Status | Blocks |
|---|---|---|---|
| 1 | HTTP server on `127.0.0.1` inside `af watch --debug` | **Confirmed by team** (small dev-phase server) | Everything |
| 2 | SSE endpoint streaming normalised events + decisions | New — RFC §6 only specifies a **stderr line format** | Live behaviour of all 5 tabs |
| 3 | **Snapshot / backfill** endpoint | New | The timeline window. SSE alone means an empty chart for the first 3 minutes after connecting — unusable |
| 4 | **Per-sample allocation trace** serialised (which span got which joules, cpu deltas, remainder) | New — the control plane computes this internally (RFC §8 L2) but nothing exposes it | Attribution tab entirely; the `[attr]` decision log; per-span energy in the inspector |
| 5 | **L1 shadow allocation** computed alongside L2 | New | The over-attribution warning ("L1 would sum to 143%") — the clearest demonstration that L2 is necessary |
| 6 | Watchdog state as a queryable list (watched pid trees, cpu/rss, orphan flags) | Partly in scope (orphan detection is in RFC §11) | Watchdog panel, orphan bars |
| 7 | Quarantined lines with reason + origin file + line/byte offset + raw text | In scope (RFC §4 `rejected/`) — needs exposing, not inventing | Health tab |
| 8 | Coverage gaps as **explicit intervals**, not inferred from missing samples | New | The magenta gap band. Critical: a gap must be *reported*, never derived from absence, or a crashed sampler looks like idle |
| 9 | **Schema conformance counters** — per-field presence rates during ingest | New. **This is a design proposal, not an RFC feature.** Confirm the team wants it | Health tab conformance bars |
| 10 | `impact_join` / `impact_estimate` over HTTP (Contract #2 as JSON) | In scope — `af report --format json` exists; needs an HTTP route | Impact tab |
| 11 | `af python doctor` result as structured JSON | In scope as a CLI; needs machine-readable output | Health sidebar |

**Item 9 is the one to challenge first.** The conformance bars ("77.8% of `action_span`s
carry `pids[]`") were invented for this design because the stated top priority was
*"is the spool receiving the events I expect, well-formed?"*. Counting field presence during
ingest is cheap, but it is new surface area. If the team does not want it, that panel is
replaced by a plain validation-error list and the design still holds.

---

## 2. Endpoints

All on `http://127.0.0.1:<port>`, all `GET`, all localhost-only, no auth (local dev tool).
Suggested port 9414 — must be configurable; the statusline already occupies the machine.

### 2.1 `GET /debug/session` — bootstrap
Called once on connect. Everything the header, footer and methodology panel need.

```jsonc
{
  "session_id": "ses_01K9…",
  "session_meta": { /* Contract #1 session_meta payload, verbatim */ },
  "t_start": "2026-07-25T09:40:12.004Z",
  "attribution_policy": "l2_cpu_time",
  "methodology": { "version": "v2026.06.1", "source": "bundled",
                   "ecologits_version": "0.7.1", "codecarbon_version": "3.0.4" },
  "grid": { "zone": "FRA", "g_co2e_per_kwh": 56, "source": "codecarbon data v2026.06" },
  "state_dir": "~/.local/state/agentic-footprint",
  "schema_version": "0.1.0",
  "mode": "watch --debug"
}
```

### 2.2 `GET /debug/snapshot?window=180s` — backfill (**gap #3**)
The last N seconds, so the chart is populated the instant the page opens.
Same frame shapes as the stream, batched:

```jsonc
{
  "events": [ /* Contract #1 events as ingested, ascending by ts */ ],
  "allocations": [ /* see §2.4, for every energy_sample in the window */ ],
  "coverage_gaps": [ { "t_start": "…", "t_end": "…",
                       "reason": "sampler restarted", "collector": "codecarbon-sampler" } ],
  "open_spans": [ /* action_spans with no t_end yet */ ],
  "watchdog": [ /* see §2.5 */ ]
}
```

`coverage_gaps` must come from the control plane (**gap #8**). Do not let the client infer
gaps from missing samples — that conflates "sampler died" with "nothing happened", which is
exactly the dishonesty RFC §3 forbids.

### 2.3 `GET /debug/stream` — SSE (**gap #2**)
`text/event-stream`. Named events so the client can route without sniffing:

| `event:` | `data:` | Feeds |
|---|---|---|
| `fact` | one Contract #1 event, exactly as ingested and normalised | Stream table, timeline, type counts |
| `decision` | `{ kind, ts, text, ref? }` where `kind` ∈ `ingest` | `span_open` | `attr` | `orphan` | Decision log |
| `alloc` | an allocation trace (§2.4) | Attribution tab, per-span energy |
| `reject` | `{ ts, reason, origin, line, byte_offset, raw }` | Health tab |
| `gap` | `{ t_start, t_end, reason, collector }` | Coverage bands |
| `watchdog` | `{ pids: [...] }` — full replacement, ~1 Hz | Watchdog panel |
| `report` | `impact_join` for the session (§2.6), ~1 Hz or on change | Impact tab, statusline preview |
| `health` | collector table + byte offsets + conformance counters (§2.7) | Health tab |

`decision.kind` maps 1:1 onto the four stderr prefixes already fixed in
`docs/design-log.md` (`[ingest]`, `[span open]`, `[attr]`, `[orphan]`). **Keep them
aligned** — the console renders the same vocabulary the team already reads in the terminal,
which is the point. `ref` should carry the `event_id` / `span_id` the line is about so the
log is clickable.

Reconnect: `Last-Event-ID` with the ingest sequence number; on reconnect the server replays
from there, or the client re-snapshots if the id is too old.

### 2.4 Allocation trace (**gaps #4, #5**) — the core payload
Streamed as `alloc`, and fetchable as `GET /debug/alloc/{energy_sample_event_id}`.

```jsonc
{
  "sample_event_id": "01K00447ZQK4NX",
  "t_start": "…", "t_end": "…",
  "total_j": 84.64,
  "components": [ { "kind": "cpu", "label": "AMD Ryzen 9 7950X",
                    "energy_j": 43.21, "method": "rapl" } ],
  "attribution_policy": "l2_cpu_time",
  "denominator_cpu_ms": 32000,          // machine cpu-time in the interval, NOT Σ watched
  "rows": [
    { "span_id": "spn_0046", "tool_name": "Task(schema-verifier)",
      "execution_locus": "local", "overlap_ms": 960, "cpu_delta_ms": 716,
      "share": 0.0224, "allocated_j": 1.90,
      "l1_allocated_j": 41.0,           // shadow policy, gap #5
      "excluded": false, "excluded_reason": null }
  ],
  "agent_process": { "pid": 4412, "cpu_delta_ms": 237, "allocated_j": 0.63 },
  "baseline": { "allocated_j": 82.0, "share": 0.969, "label": "baseline/idle" },
  "l1_shadow_sum_share": 0.62           // >1.0 ⇒ over-attribution warning
}
```

Two things the prototype got wrong at first and that the real implementation must get right:

- **`denominator_cpu_ms` is the machine's cpu-time over the interval, not the sum of
  watched trees.** If you divide by Σ watched, then attributed + agent ≡ 100% and the
  baseline/idle remainder is zero *by construction* — which silently destroys the
  "active vs idle is explicit" guarantee that RFC §8 makes L2's selling point.
- **`excluded: true` with a reason** for `execution_locus: remote` rows. They must appear
  in the trace (so the developer sees they overlapped) while contributing zero joules.

### 2.5 Watchdog (**gap #6**)
```jsonc
[ { "pid": 30887, "span_id": "spn_0006", "cmd": "Bash(uv pip install) → cargo/rustc",
    "cpu_pct": 38.0, "rss_bytes": 1932735283,
    "state": "orphaned",                       // open | orphaned | agent
    "orphaned_since": "…", "outlived_span_by_ms": 47000 } ]
```

### 2.6 `GET /debug/report?level=session|task|tool` (**gap #10**)
Contract #2 verbatim — `impact_join` per `schemas/v0.1/derived.schema.json`, plus
`impact_estimate` grouped by model for the per-model table, plus the `estimation_status`
histogram (`ok` / `pending` / `unknown_model` / `missing_zone`).

Ranges must arrive as `{min, max}` per criterion and **must not** be pre-averaged. The
console renders the range; only the `af statusline` preview shows a range mean, because
that surface is specified that way in `docs/design-log.md`.

Also needed for the "what token-only misses" panel, and none of it is derivable client-side:
`local_measured.energy`, `local_measured.coverage`, `baseline_share_excluded`, the
orphaned-compute total, the agent process's own share, and `unmeasured_remote_spans`.

### 2.7 Health (**gaps #7, #9, #11**)
```jsonc
{
  "collectors": [
    { "name": "claude-code", "version": "0.1.2", "transport": "jsonl spool",
      "spool_file": "claude-code.01K9Y7QZ.jsonl", "byte_offset": 2517428,
      "events": 30, "events_per_s": 0.14, "rejected": 1,
      "last_seen": "…", "emits": ["session_meta","llm_call","action_span"] },
    { "name": "otlp-cc", "version": "0.1.0", "transport": "POST /v1/logs", … }
  ],
  "otlp_receiver": { "endpoint": "127.0.0.1:4318", "protocol": "http/json",
                     "logs_accepted": 11, "metrics_discarded": 42 },
  "conformance": [ { "field": "action_span.pids[]", "present": 21, "total": 27,
                     "note": "…" } ],
  "rejected": [ { "ts": "…", "reason": "malformed JSON: unexpected end of input at byte offset 41822",
                  "origin": "claude-code.01K9Y7QZ.jsonl", "line": 846, "raw": "{\"schema_version\"…" } ],
  "python": [ { "key": "ecologits", "value": "0.7.1 · hash-locked", "status": "ok" } ]
}
```

Note the OTLP receiver is **http/json only**, endpoints `POST /v1/logs` (normalised to
`llm_call`) and `POST /v1/metrics` (200, body discarded) — fixed in `docs/design-log.md`.
The prototype's header says `4317`; that is a gRPC port and is **wrong**. Use the http
port the receiver actually binds.

---

## 3. Client components to build

### 3.1 `AfClient` — transport
Owns `fetch` + `EventSource`. Responsibilities: bootstrap (`/debug/session`), backfill
(`/debug/snapshot`) **then** subscribe (in that order, or the chart flickers); reconnect with
backoff and `Last-Event-ID`; expose `status: connecting | live | reconnecting | offline`
for the header dot. Buffers frames while the UI is paused — pausing must never drop data.

### 3.2 `EventStore` — bounded history + span index
- Ring buffer of facts. The prototype caps at ~2600 events / 560 spans / 200 samples for a
  3-minute window; size for the real event rate, and **make the cap explicit in the UI**
  ("showing N of M") so nobody mistakes truncation for reality.
- **Time-bucketed span index.** Keyed by `floor((t - t0) / 2000)` → spans overlapping that
  2-second slot, with a span inserted into every slot it covers. This is what makes
  "which spans overlap this sample?" O(1) instead of a full scan, and it is the difference
  between a 1 Hz refresh being free and being a stall. Remove entries from the index when
  the ring buffer evicts, or the index leaks.
- **Span ids must be treated as opaque and stable.** (The prototype originally derived them
  from array length and recycled them after eviction, corrupting the overlap join. The
  server supplies real `span_id`s — never synthesise one.)
- Maintains type counts and per-collector counters incrementally, not by re-scanning.

### 3.3 `AllocStore` — memoised traces
`Map<sample_event_id, AllocationTrace>`. Traces are immutable once the sample is closed, so
cache freely. Never recompute; never patch a trace client-side.

### 3.4 `SessionStore`, `ReportStore`, `HealthStore`
Thin holders for §2.1, §2.6, §2.7. `ReportStore` and `HealthStore` are replace-on-arrival.

### 3.5 View selectors (pure functions, gated by active tab)
One per panel: `selectTimelineLanes`, `selectDecisionLog`, `selectStreamRows`,
`selectAllocationTable`, `selectImpactRows`, `selectHealthRows`, `selectInspector`.

Two hard rules, both learned the hard way in the prototype:
- **Only the visible tab's selectors run.** Computing all five per tick is what makes this
  feel slow.
- **Selectors are pure and memoised on `(storeRevision, tab, layout, selectedId, filters)`.**
  No randomness, no `Date.now()` inside a selector — anything non-deterministic makes
  renders unstable and defeats memoisation.

### 3.6 Presentational components
`Masthead` · `TabBar` · `LaneChart` (flat absolutely-positioned bar list + lane labels +
axis — **not** nested per-lane loops) · `DecisionLog` · `EventTable` · `Inspector` ·
`ShareBar` · `AllocationTable` · `ImpactCards` · `CriteriaTable` · `ConformanceBars` ·
`RejectedList` · `WatchdogList` · `StatuslinePreview` · `Footer`.

`LaneChart` is the only non-obvious one: compute all bars into one flat array with
`{left%, width%, topPx, heightPx, fill, hatch, border}` and render a single loop. Lane
geometry is data, not markup. This keeps the three timeline layouts as three geometry
functions over one component.

---

## 4. Field-by-field provenance

Every value in the prototype and where it must come from. `✕` = fabricated, no real source
yet (all covered by §1).

| UI element | Real source |
|---|---|
| Masthead session id, agent app + version, elapsed | `/debug/session` → `session_meta.agent_app`, `t_start` |
| SSE status dot | `AfClient.status` |
| Tab counts | `EventStore` counters |
| Timeline `llm_call` ticks | `llm_call` facts; bar span = `ts - duration_ms` → `ts`; hatched when `usage_source` ∈ {`transcript`,`estimated`} |
| Timeline span bars | `action_span` facts; fill by `tool_kind`, hatch when `execution_locus: remote` |
| Orphan tail bar | `watchdog[].orphaned_since` → now (**✕ gap #6**) |
| Machine-power bars | `energy_sample.components`; watts = Σ`energy_j` / interval seconds |
| Coverage-gap band | `coverage_gaps[]` (**✕ gap #8**) |
| `process_sample` lane | `process_sample.processes[].cpu_time_delta_ms` |
| Axis "now" | client clock; server `ts` is authoritative for data |
| Decision log lines | `decision` frames (**✕ gap #2**) |
| Stream table `facts` column | formatted from each payload — display only, no derived quantities |
| `source / method` badge | `llm_call.usage_source`; `energy_sample.components[].method` |
| Inspector "energy · l2_cpu_time" for a span | Σ `rows[].allocated_j` across overlapping traces (**✕ gap #4**) |
| Inspector share bars | `alloc.rows`, `agent_process`, `baseline` (**✕ gap #4**) |
| Attribution stats, allocation table, L1 column | `alloc` (**✕ gaps #4, #5**) |
| Attribution formula panel | `alloc.denominator_cpu_ms`, `total_j`, `baseline.allocated_j` |
| Impact cards, criteria table, per-model table | `/debug/report` (**✕ gap #10**) |
| "What token-only misses" | `impact_join.local_measured`, orphan total, agent share, `unmeasured_remote_spans` (**✕ gap #10**) |
| `af statusline` preview | `af statusline` format from `docs/design-log.md`: `gwp wcf energy adpe pe`, range means, `nan` when unmeasured |
| Collector table | `/debug/health` → `collectors` (**✕ gap #7**) |
| Conformance bars | `/debug/health` → `conformance` (**✕ gap #9, and confirm it is wanted**) |
| Quarantined lines | `spool/rejected/` (**✕ gap #7**) |
| Python doctor rows | `af python doctor --json` (**✕ gap #11**) |
| Footer byte offsets, spool paths | `/debug/health`, `/debug/session` |

---

## 5. Acceptance

The console is done when it can show the RFC §11 acceptance scenario end to end — one
complete task (e.g. resolving a real GitHub issue) as an `impact_join`: remote inference
estimates **plus** measured local tool energy **plus** retries and verification cycles
**plus** any orphaned compute — with every gap visibly labelled rather than zeroed.

Concretely, on real data:

1. A `cargo test` span shows measured local joules, and its share is visibly less than the
   machine total, with the remainder labelled `baseline/idle`.
2. Two overlapping spans show an L1 shadow sum above 100% while L2 stays at or below it.
3. Killing the codecarbon sampler mid-session produces a magenta coverage band — **not** a
   flat line at zero.
4. A pid tree outliving its span appears as an orphan in the watchdog and in the
   "token-only misses" total.
5. An `llm_call` with an unrecognised model renders `unknown_model` and is excluded from
   totals rather than silently counted as zero.
6. A malformed spool line lands in the Health tab with its reason, origin file and byte
   offset, and the ingest counter does not advance past it.
