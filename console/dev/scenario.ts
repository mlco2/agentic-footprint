// Deterministic fixture scenario for the mock /debug server. Everything here
// is a pure function of a fixed seed and fixed constants — no Date.now(),
// no Math.random() (global-constraints.md #determinism). Wall-clock shifting
// (mapping `atMs` onto real time) happens only in mock-plugin.ts.
//
// console/dev/ is dev-only: nothing here may be imported by console/src/.
import type { ActionSpan, EnergySample, FactEvent, LlmCall, SessionMeta } from "../src/lib/types/contract1";
import type { Criterion, ImpactEstimate, ImpactJoin, Impacts } from "../src/lib/types/contract2";
import type {
  AllocationRow,
  AllocationTrace,
  DebugReport,
  DecisionFrame,
  EstimationStatus,
  GapFrame,
  HealthPayload,
  ModelImpactGroup,
  SessionInfo,
  SseFrame,
  WatchdogEntry,
} from "../src/lib/types/debug";

import sessionFixture from "./fixtures/session.json";
import healthFixture from "./fixtures/health.json";
import rejectFixture from "./fixtures/reject.json";
import watchdogOrphanFixture from "./fixtures/watchdog-orphan.json";

export interface Scenario {
  session: SessionInfo;
  /** Chronological, `atMs` relative to `session.t_start`. Monotonic non-negative. */
  frames: SseFrame[];
  /** Every energy_sample gets exactly one trace, keyed by its event_id. */
  allocs: Map<string, AllocationTrace>;
  report: DebugReport;
  health: HealthPayload;
}

// ---------------------------------------------------------------------------
// Determinism helpers
// ---------------------------------------------------------------------------

/** mulberry32 — small, fast, deterministic PRNG. Used only for cosmetic
 * jitter (cpu%, byte counts); every arithmetic invariant the contract test
 * checks is computed by exact reconciliation (see buildAllocTrace) and is
 * unaffected by the jitter's actual values. */
function mulberry32(seed: number): () => number {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const rng = mulberry32(0xdebc045);

function round2(n: number): number {
  return Math.round(n * 100) / 100;
}

function round4(n: number): number {
  return Math.round(n * 10000) / 10000;
}

/** Higher-precision rounding for criteria naturally this small at session
 * scale (kWh energy, kg gwp, kgSbeq adpe over ~100s of local activity) —
 * round4 would collapse a genuinely non-zero measured value to 0, which is
 * exactly the "not measured rendered as 0" dishonesty global-constraints.md
 * forbids for the console; the fixture must not manufacture that failure
 * mode itself. */
function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

const T0_MS = Date.parse((sessionFixture as SessionInfo).t_start);

function msToIso(atMs: number): string {
  return new Date(T0_MS + atMs).toISOString();
}

let idCounter = 0;
function nextId(): string {
  idCounter += 1;
  return `01K9${String(idCounter).padStart(12, "0")}`;
}

// ---------------------------------------------------------------------------
// Fixed timeline constants
// ---------------------------------------------------------------------------

const SESSION_ID = (sessionFixture as SessionInfo).session_id;
const SAMPLE_MS = 2000;
const TOTAL_MS = 100_000;
const GAP_START_MS = 20_000;
const GAP_END_MS = 24_000;
/** Machine cpu-time available per sample interval — a stand-in for an
 * 8-core box (8 * SAMPLE_MS). Deliberately far larger than any single
 * watched tree's cpu delta, so baseline/idle dominates by construction
 * (DATA-CONTRACT §2.4: denominator_cpu_ms is machine time, not Σ watched). */
const DENOMINATOR_CPU_MS = 8 * SAMPLE_MS;

const COLLECTOR_CLAUDE_CODE = { name: "claude-code", version: "0.1.2" };
const COLLECTOR_CODECARBON = { name: "codecarbon-sampler", version: "3.0.4" };
const COLLECTOR_OTLP = { name: "otlp-cc", version: "0.1.0" };

interface SpanDescriptor {
  span_id: string;
  tool_name: string;
  tool_kind: ActionSpan["tool_kind"];
  execution_locus: ActionSpan["execution_locus"];
  pids: number[];
  t_start_ms: number;
  t_end_ms: number;
  /** Average cpu-ms consumed per second of wall-clock overlap (drives the L2 share). */
  cpu_ms_per_s: number;
  status: NonNullable<ActionSpan["status"]>;
}

// Item 1: long bash span ("cargo test"-like), measured joules, baseline dominant.
// Item 4: item 4's orphan (spn_0006) reuses this list too.
// Item 2: spn_0002/spn_0003 overlap fully inside sample #17 ([34000,36000)),
// producing l1_shadow_sum_share = 2000/2000 + 860/2000 = 1.43 while their
// (tiny, cpu-time-based) L2 shares stay far under 1.0.
// Item 7: spn_0007 is execution_locus: remote — excluded, 0 joules.
const SPANS: SpanDescriptor[] = [
  {
    span_id: "spn_0001",
    tool_name: "Bash(cargo test)",
    tool_kind: "bash",
    execution_locus: "local",
    pids: [21044],
    t_start_ms: 0,
    t_end_ms: 32_000,
    cpu_ms_per_s: 180,
    status: "ok",
  },
  {
    span_id: "spn_0002",
    tool_name: "Edit(src/lib.rs)",
    tool_kind: "file_op",
    execution_locus: "local",
    pids: [21099],
    t_start_ms: 34_000,
    t_end_ms: 36_000,
    cpu_ms_per_s: 200,
    status: "ok",
  },
  {
    span_id: "spn_0003",
    tool_name: "Task(subagent-reviewer)",
    tool_kind: "subagent",
    execution_locus: "local",
    pids: [21100],
    t_start_ms: 35_140,
    t_end_ms: 36_000,
    cpu_ms_per_s: 125,
    status: "ok",
  },
  {
    span_id: "spn_0006",
    tool_name: "Bash(uv pip install) → cargo/rustc",
    tool_kind: "bash",
    execution_locus: "local",
    pids: [30887],
    t_start_ms: 26_000,
    t_end_ms: 30_000,
    cpu_ms_per_s: 210,
    status: "ok",
  },
  {
    span_id: "spn_0007",
    tool_name: "WebFetch(https://api.example.com/data)",
    tool_kind: "web",
    execution_locus: "remote",
    pids: [],
    t_start_ms: 50_000,
    t_end_ms: 52_000,
    cpu_ms_per_s: 0,
    status: "ok",
  },
];

// Item 4: the orphan appears in `watchdog` frames only after spn_0006 closes
// (t_end_ms = 30_000); outlived_span_by_ms = 47_000 in the fixture, so the
// orphan is first reported at 30_000 + 47_000 = 77_000ms.
const ORPHAN_DETECTED_AT_MS = 77_000;
const ORPHAN_ENTRY = watchdogOrphanFixture as WatchdogEntry;

// Item 6: malformed spool line, verbatim byte offset from the fixture. ts -
// T0 gives its atMs; kept in lockstep with dev/fixtures/reject.json by
// computing rather than re-typing the offset.
const REJECT_AT_MS = Date.parse((rejectFixture as { ts: string }).ts) - T0_MS;

// ---------------------------------------------------------------------------
// llm_call fixtures (items 5, 8, 10)
// ---------------------------------------------------------------------------

interface LlmCallFixture {
  atMs: number;
  collector: { name: string; version: string };
  payload: LlmCall;
  /** undefined => estimation_status "unknown_model", no impacts computed. */
  impacts: Impacts | undefined;
  estimationStatus: EstimationStatus;
}

const LLM_CALLS: LlmCallFixture[] = [
  {
    // Item 10: usage_source agent_telemetry, full token usage.
    atMs: 10_000,
    collector: COLLECTOR_OTLP,
    payload: {
      provider: "anthropic",
      model_id_requested: "claude-sonnet-4-5-20250929",
      model_id_served: "claude-sonnet-4-5-20250929",
      usage: {
        input_tokens: 18420,
        output_tokens: 642,
        thought_tokens: 1024,
        cached_read_tokens: 15000,
        cached_write_tokens: 0,
      },
      usage_source: "agent_telemetry",
      duration_ms: 2350,
      status: "ok",
      streaming: true,
    },
    impacts: {
      energy: { unit: "kWh", total: { min: 0.0028, max: 0.0041 }, usage: { min: 0.0025, max: 0.0038 }, embodied: { min: 0.0003, max: 0.0003 } },
      gwp: { unit: "kgCO2eq", total: { min: 0.0016, max: 0.0023 }, usage: { min: 0.0014, max: 0.0021 }, embodied: { min: 0.0002, max: 0.0002 } },
      adpe: { unit: "kgSbeq", total: { min: 0.0000012, max: 0.0000018 } },
      pe: { unit: "MJ", total: { min: 0.031, max: 0.045 } },
      water: { unit: "L", total: { min: 0.0021, max: 0.0029 } },
    },
    estimationStatus: "ok",
  },
  {
    // Item 5: unrecognised model — excluded from totals, counted under unknown_model.
    atMs: 40_000,
    collector: COLLECTOR_CLAUDE_CODE,
    payload: {
      provider: "acme",
      model_id_requested: "acme-mystery-7b",
      usage: { input_tokens: 1200, output_tokens: 340 },
      usage_source: "api_response",
      duration_ms: 1500,
      status: "ok",
      streaming: false,
    },
    impacts: undefined,
    estimationStatus: "unknown_model",
  },
  {
    // Item 8: usage_source transcript (known accuracy issues, still estimated).
    atMs: 54_000,
    collector: COLLECTOR_CLAUDE_CODE,
    payload: {
      provider: "anthropic",
      model_id_requested: "claude-3-5-sonnet-20241022",
      usage: { input_tokens: 9000, output_tokens: 1200 },
      usage_source: "transcript",
      duration_ms: 4200,
      status: "ok",
      streaming: true,
    },
    impacts: {
      energy: { unit: "kWh", total: { min: 0.0019, max: 0.0027 }, usage: { min: 0.0017, max: 0.0025 }, embodied: { min: 0.0002, max: 0.0002 } },
      gwp: { unit: "kgCO2eq", total: { min: 0.0011, max: 0.0015 }, usage: { min: 0.0009, max: 0.0014 }, embodied: { min: 0.0001, max: 0.0001 } },
      adpe: { unit: "kgSbeq", total: { min: 0.0000008, max: 0.0000012 } },
      pe: { unit: "MJ", total: { min: 0.021, max: 0.03 } },
      water: { unit: "L", total: { min: 0.0014, max: 0.0019 } },
    },
    estimationStatus: "ok",
  },
  {
    // Item 10: second agent_telemetry call with full usage.
    atMs: 60_000,
    collector: COLLECTOR_OTLP,
    payload: {
      provider: "anthropic",
      model_id_requested: "claude-haiku-4-5-20251001",
      model_id_served: "claude-haiku-4-5-20251001",
      usage: {
        input_tokens: 5230,
        output_tokens: 210,
        thought_tokens: 0,
        cached_read_tokens: 4800,
        cached_write_tokens: 0,
      },
      usage_source: "agent_telemetry",
      duration_ms: 890,
      status: "ok",
      streaming: false,
    },
    impacts: {
      energy: { unit: "kWh", total: { min: 0.0006, max: 0.0009 }, usage: { min: 0.0005, max: 0.0008 }, embodied: { min: 0.0001, max: 0.0001 } },
      gwp: { unit: "kgCO2eq", total: { min: 0.0003, max: 0.0005 }, usage: { min: 0.0003, max: 0.0004 }, embodied: { min: 0.00004, max: 0.00004 } },
      adpe: { unit: "kgSbeq", total: { min: 0.0000003, max: 0.0000004 } },
      pe: { unit: "MJ", total: { min: 0.0067, max: 0.0098 } },
      water: { unit: "L", total: { min: 0.0005, max: 0.0007 } },
    },
    estimationStatus: "ok",
  },
];

// ---------------------------------------------------------------------------
// Allocation trace construction (arithmetic invariants live here, exactly)
// ---------------------------------------------------------------------------

function sumCriteria(criteria: Array<Criterion | undefined>): Criterion | undefined {
  const present = criteria.filter((c): c is Criterion => c !== undefined);
  if (present.length === 0) return undefined;
  const unit = present[0].unit;
  const sum = (pick: (c: Criterion) => { min: number; max: number } | undefined) => {
    const parts = present.map(pick).filter((r): r is { min: number; max: number } => r !== undefined);
    if (parts.length === 0) return undefined;
    return {
      min: round8(parts.reduce((acc, r) => acc + r.min, 0)),
      max: round8(parts.reduce((acc, r) => acc + r.max, 0)),
    };
  };
  const total = sum((c) => c.total);
  if (!total) return undefined;
  const usage = sum((c) => c.usage);
  const embodied = sum((c) => c.embodied);
  return { unit, total, usage, embodied };
}

function sumImpacts(all: Array<Impacts | undefined>): Impacts {
  return {
    energy: sumCriteria(all.map((i) => i?.energy)),
    gwp: sumCriteria(all.map((i) => i?.gwp)),
    adpe: sumCriteria(all.map((i) => i?.adpe)),
    pe: sumCriteria(all.map((i) => i?.pe)),
    water: sumCriteria(all.map((i) => i?.water)),
  };
}

function overlapMs(aStart: number, aEnd: number, bStart: number, bEnd: number): number {
  return Math.max(0, Math.min(aEnd, bEnd) - Math.max(aStart, bStart));
}

interface SampleAlloc {
  trace: AllocationTrace;
  attributedJ: number; // Σ rows.allocated_j + agent.allocated_j (baseline excluded)
}

function buildAllocTrace(sampleIndex: number, tStartMs: number, tEndMs: number): SampleAlloc {
  const sampleEventId = nextId();
  const method = sampleIndex % 2 === 0 ? "rapl" : "powermetrics";
  const totalJ = round2(80 + rng() * 8);

  const overlapping = SPANS.filter((s) => overlapMs(tStartMs, tEndMs, s.t_start_ms, s.t_end_ms) > 0);

  const rows: AllocationRow[] = overlapping.map((s) => {
    const overlap = overlapMs(tStartMs, tEndMs, s.t_start_ms, s.t_end_ms);
    const remote = s.execution_locus === "remote";
    const cpuDeltaMs = remote ? 0 : Math.round(s.cpu_ms_per_s * (overlap / 1000) * (0.9 + rng() * 0.2));
    const share = round4(cpuDeltaMs / DENOMINATOR_CPU_MS);
    const allocatedJ = remote ? 0 : round2(totalJ * share);
    const l1AllocatedJ = remote ? 0 : round2(totalJ * (overlap / SAMPLE_MS));
    return {
      span_id: s.span_id,
      tool_name: s.tool_name,
      execution_locus: s.execution_locus,
      overlap_ms: overlap,
      cpu_delta_ms: cpuDeltaMs,
      share,
      allocated_j: allocatedJ,
      l1_allocated_j: l1AllocatedJ,
      excluded: remote,
      excluded_reason: remote ? "execution_locus: remote — no local energy attributable" : null,
    };
  });

  const agentCpuDeltaMs = Math.round(40 * (SAMPLE_MS / 1000) * (0.85 + rng() * 0.3));
  const agentShare = round4(agentCpuDeltaMs / DENOMINATOR_CPU_MS);
  const agentAllocatedJ = round2(totalJ * agentShare);

  const rowsTotalJ = rows.reduce((acc, r) => acc + r.allocated_j, 0);
  // Baseline is the exact remainder: guarantees rows + agent + baseline ===
  // total_j to the cent, and is why the denominator being machine-wide (not
  // Σ watched) makes baseline the dominant term by construction.
  const baselineAllocatedJ = round2(totalJ - rowsTotalJ - agentAllocatedJ);
  const baselineShare = round4(baselineAllocatedJ / totalJ);

  const l1ShadowSumShare = round4(
    rows.filter((r) => !r.excluded).reduce((acc, r) => acc + r.overlap_ms / SAMPLE_MS, 0),
  );

  const trace: AllocationTrace = {
    sample_event_id: sampleEventId,
    t_start: msToIso(tStartMs),
    t_end: msToIso(tEndMs),
    total_j: totalJ,
    components: [{ kind: "cpu", label: "AMD Ryzen 9 7950X", energy_j: totalJ, method }],
    attribution_policy: "l2_cpu_time",
    denominator_cpu_ms: DENOMINATOR_CPU_MS,
    rows,
    agent_process: { pid: 4412, cpu_delta_ms: agentCpuDeltaMs, allocated_j: agentAllocatedJ },
    baseline: { allocated_j: baselineAllocatedJ, share: baselineShare, label: "baseline/idle" },
    l1_shadow_sum_share: l1ShadowSumShare,
  };

  return { trace, attributedJ: round2(rowsTotalJ + agentAllocatedJ) };
}

// ---------------------------------------------------------------------------
// Scenario assembly
// ---------------------------------------------------------------------------

export function buildScenario(): Scenario {
  const frames: SseFrame[] = [];
  const allocs = new Map<string, AllocationTrace>();
  let attributedJAllSamples = 0;
  let coveredMs = 0;

  function push<T extends SseFrame["event"]>(atMs: number, event: T, data: Extract<SseFrame, { event: T }>["data"]) {
    frames.push({ atMs, event, data } as SseFrame);
  }

  function decision(atMs: number, kind: DecisionFrame["kind"], text: string, ref?: string) {
    push(atMs, "decision", { kind, ts: msToIso(atMs), text, ref });
  }

  // --- session_meta ---
  const sessionMetaEvent: FactEvent = {
    schema_version: "0.1.0",
    event_id: nextId(),
    type: "session_meta",
    ts: msToIso(0),
    collector: COLLECTOR_CLAUDE_CODE,
    session_id: SESSION_ID,
    payload: (sessionFixture as SessionInfo).session_meta as SessionMeta,
  };
  push(0, "fact", sessionMetaEvent);
  decision(0, "ingest", `session_meta ingested: agent_app=${sessionMetaEvent.payload.agent_app.name}`, sessionMetaEvent.event_id);

  // --- action_span facts + span_open decisions (items 1, 2, 4, 7) ---
  for (const s of SPANS) {
    decision(s.t_start_ms, "span_open", `action_span open: span_id=${s.span_id} tool=${s.tool_name} locus=${s.execution_locus}`, s.span_id);
    const actionSpanEvent: FactEvent = {
      schema_version: "0.1.0",
      event_id: nextId(),
      type: "action_span",
      ts: msToIso(s.t_end_ms),
      collector: COLLECTOR_CLAUDE_CODE,
      session_id: SESSION_ID,
      payload: {
        span_id: s.span_id,
        tool_name: s.tool_name,
        tool_kind: s.tool_kind,
        execution_locus: s.execution_locus,
        t_start: msToIso(s.t_start_ms),
        t_end: msToIso(s.t_end_ms),
        pids: s.pids,
        status: s.status,
      },
    };
    push(s.t_end_ms, "fact", actionSpanEvent);
    decision(s.t_end_ms, "ingest", `action_span ingested: span_id=${s.span_id}`, s.span_id);
  }

  // --- llm_calls (items 5, 8, 10) ---
  const estimates: Array<{ modelId: string; estimate: ImpactEstimate; impacts: Impacts | undefined }> = [];
  // `EstimationStatus` now includes `missing_usage` (schemas/v0.1/derived.schema.json,
  // commit fb0f1f9) — zero-filled here like every other status even though
  // this scenario's fixture LLM_CALLS never actually assign it, so the
  // literal keeps satisfying `Record<EstimationStatus, number>` exhaustively.
  const histogram: Record<EstimationStatus, number> = { ok: 0, unknown_model: 0, missing_zone: 0, missing_usage: 0, pending: 0, error: 0 };
  for (const call of LLM_CALLS) {
    const eventId = nextId();
    const llmCallEvent: FactEvent = {
      schema_version: "0.1.0",
      event_id: eventId,
      type: "llm_call",
      ts: msToIso(call.atMs),
      collector: call.collector,
      session_id: SESSION_ID,
      payload: call.payload,
    };
    push(call.atMs, "fact", llmCallEvent);
    decision(call.atMs, "ingest", `llm_call ingested: model=${call.payload.model_id_requested} usage_source=${call.payload.usage_source}`, eventId);

    const estimate: ImpactEstimate = {
      event_id: eventId,
      estimation_status: call.estimationStatus,
      impacts: call.impacts,
      methodology: { version: "v2026.06.1", source: "bundled", ecologits_version: "0.7.1" },
    };
    histogram[call.estimationStatus] += 1;
    estimates.push({ modelId: call.payload.model_id_requested, estimate, impacts: call.impacts });
  }

  // --- energy_sample / process_sample / alloc, every SAMPLE_MS, skipping the gap (item 3) ---
  const sampleCount = TOTAL_MS / SAMPLE_MS;
  for (let i = 0; i < sampleCount; i += 1) {
    const tStartMs = i * SAMPLE_MS;
    const tEndMs = tStartMs + SAMPLE_MS;
    if (tStartMs >= GAP_START_MS && tStartMs < GAP_END_MS) continue; // sampler down

    const { trace, attributedJ } = buildAllocTrace(i, tStartMs, tEndMs);
    allocs.set(trace.sample_event_id, trace);
    attributedJAllSamples += attributedJ;
    coveredMs += SAMPLE_MS;

    const energySampleEvent: FactEvent = {
      schema_version: "0.1.0",
      event_id: trace.sample_event_id,
      type: "energy_sample",
      ts: msToIso(tEndMs),
      collector: COLLECTOR_CODECARBON,
      session_id: SESSION_ID,
      payload: {
        t_start: trace.t_start,
        t_end: trace.t_end,
        components: [trace.components[0] as EnergySample["components"][number]] as EnergySample["components"],
      },
    };
    push(tEndMs, "fact", energySampleEvent);

    const processSampleEvent: FactEvent = {
      schema_version: "0.1.0",
      event_id: nextId(),
      type: "process_sample",
      ts: msToIso(tEndMs),
      collector: COLLECTOR_CODECARBON,
      session_id: SESSION_ID,
      payload: {
        t_start: trace.t_start,
        t_end: trace.t_end,
        processes: [
          ...trace.rows
            .filter((r) => !r.excluded)
            .map((r) => ({ pid: SPANS.find((s) => s.span_id === r.span_id)?.pids[0] ?? 0, cpu_time_delta_ms: r.cpu_delta_ms })),
          { pid: trace.agent_process.pid, cpu_time_delta_ms: trace.agent_process.cpu_delta_ms },
        ],
      },
    };
    push(tEndMs, "fact", processSampleEvent);
    push(tEndMs, "alloc", trace);
    decision(
      tEndMs,
      "attr",
      `attr: sample=${trace.sample_event_id} total=${trace.total_j}J baseline=${trace.baseline.allocated_j}J (${Math.round(trace.baseline.share * 100)}% idle)`,
      trace.sample_event_id,
    );
  }

  // --- coverage gap (item 3) ---
  const gap: GapFrame = {
    t_start: msToIso(GAP_START_MS),
    t_end: msToIso(GAP_END_MS),
    reason: "sampler restarted",
    collector: "codecarbon-sampler",
  };
  push(GAP_END_MS, "gap", gap);

  // --- reject frame (item 6), verbatim from fixture ---
  push(REJECT_AT_MS, "reject", rejectFixture as { ts: string; reason: string; origin: string; line: number; byte_offset: number; raw: string });

  // --- watchdog frames: every SPANS entry with a local pid shows "open"
  // while its own [t_start_ms, t_end_ms) is active — not only spn_0001
  // (fixture realism: spn_0002/0003/0006 all get their own "open" entries
  // during their own windows; spn_0007 is `execution_locus: remote` with an
  // empty `pids` array, so it naturally contributes nothing here, same as a
  // real remote span would). The orphan (item 4) appears only in frames
  // emitted after its span (spn_0006) closes, from ORPHAN_DETECTED_AT_MS
  // onward — well after spn_0006's own "open" window (26_000-30_000) has
  // already ended, so the two never overlap for the same pid. ---
  for (let i = 0; i < sampleCount; i += 1) {
    const atMs = i * SAMPLE_MS;
    if (atMs >= GAP_START_MS && atMs < GAP_END_MS) continue;
    const entries: WatchdogEntry[] = [];
    for (const s of SPANS) {
      if (atMs < s.t_start_ms || atMs >= s.t_end_ms) continue;
      for (const pid of s.pids) {
        entries.push({
          pid,
          span_id: s.span_id,
          cmd: s.tool_name,
          cpu_pct: round2(8 + rng() * 10),
          rss_bytes: 150_000_000 + Math.round(rng() * 350_000_000),
          state: "open",
        });
      }
    }
    if (atMs >= ORPHAN_DETECTED_AT_MS) {
      entries.push(ORPHAN_ENTRY);
    }
    // DATA-CONTRACT §2.3's table specifies the `watchdog` SSE frame's `data:`
    // as `{ pids: [...] }` — an object wrapping the array — unlike
    // `Snapshot.watchdog` (§2.2) and `WatchdogEntry` (§2.5) itself, which are
    // both bare arrays/objects with no wrapper. Follow the doc literally here.
    push(atMs, "watchdog", { pids: entries });
  }
  decision(ORPHAN_DETECTED_AT_MS, "orphan", `orphan detected: pid=${ORPHAN_ENTRY.pid} span_id=${ORPHAN_ENTRY.span_id} outlived_span_by_ms=${ORPHAN_ENTRY.outlived_span_by_ms}`, ORPHAN_ENTRY.span_id);

  // --- report (session-level impact_join + per-model table + histogram) ---
  const attributedKWh = attributedJAllSamples / 3_600_000;
  const localEnergy: Criterion = { unit: "kWh", total: { min: round8(attributedKWh), max: round8(attributedKWh) } };
  const grid = (sessionFixture as SessionInfo).grid;
  // The mock fixture (dev/fixtures/session.json) always carries a real grid
  // factor — the scenario is deliberately rich, never null. `?? 0` only
  // satisfies the type, which is `number | null` because the real server's
  // g_co2e_per_kwh CAN be null (no estimator sidecar); this fallback is
  // never actually exercised against this fixture.
  const localGwpKg = (attributedKWh * (grid.g_co2e_per_kwh ?? 0)) / 1000;
  const localGwp: Criterion = { unit: "kgCO2eq", total: { min: round8(localGwpKg), max: round8(localGwpKg) } };

  const okImpacts = estimates.filter((e) => e.impacts !== undefined).map((e) => e.impacts);
  const remoteImpacts = sumImpacts(okImpacts);

  const combinedEnergy = sumCriteria([localEnergy, remoteImpacts.energy]);
  const combinedGwp = sumCriteria([localGwp, remoteImpacts.gwp]);

  const byModel: ModelImpactGroup[] = [];
  for (const { modelId, estimate, impacts } of estimates) {
    const existing = byModel.find((g) => g.model_id === modelId);
    if (existing) {
      existing.estimates.push(estimate);
    } else {
      byModel.push({ model_id: modelId, estimates: [estimate], impacts: impacts ?? {} });
    }
  }

  const impactJoin: ImpactJoin = {
    unit: { level: "session", session_id: SESSION_ID },
    t_start: msToIso(0),
    t_end: msToIso(TOTAL_MS),
    attribution_policy: "l2_cpu_time",
    local_measured: {
      energy: localEnergy,
      gwp: localGwp,
      baseline_share_excluded: true,
      coverage: round4(coveredMs / TOTAL_MS),
    },
    remote_estimated: { impacts: remoteImpacts, llm_calls: LLM_CALLS.length },
    combined_total: { energy: combinedEnergy, gwp: combinedGwp },
    unmeasured_remote_spans: SPANS.filter((s) => s.execution_locus === "remote").length,
  };

  const report: DebugReport = {
    level: "session",
    impact_join: impactJoin,
    by_model: byModel,
    estimation_status_histogram: histogram,
  };

  // --- health (item 9): no `conformance` key — its absence is meaningful. ---
  const health: HealthPayload = {
    ...(healthFixture as Omit<HealthPayload, "rejected" | "conformance">),
    rejected: [rejectFixture as HealthPayload["rejected"][number]],
  };

  // Periodic `report`/`health` frames — the mock reuses the final computed
  // objects at each tick rather than re-deriving partial-session values;
  // real report/health payloads evolve over time, but doing so faithfully
  // here would duplicate control-plane logic this console must never own
  // (DATA-CONTRACT §0). Shape fidelity, not incremental accuracy, is the point.
  for (let atMs = 0; atMs <= TOTAL_MS; atMs += 5000) {
    push(atMs, "report", report);
  }
  for (let atMs = 0; atMs <= TOTAL_MS; atMs += 10_000) {
    push(atMs, "health", health);
  }

  frames.sort((a, b) => a.atMs - b.atMs);

  return {
    session: sessionFixture as SessionInfo,
    frames,
    allocs,
    report,
    health,
  };
}
