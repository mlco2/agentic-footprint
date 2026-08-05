// Tests for console/src/lib/selectors/inspector.ts (Task 6 brief).
//
// Isolation note: mirrors stream.test.ts's own convention — `inspector.ts`
// imports the `eventStore`/`allocStore` SINGLETONS directly (DATA-CONTRACT
// §3.5's pattern — stores aren't selector parameters), and its `memo1`-
// wrapped functions live in module-level closures. Each test gets a fully
// fresh module graph via `vi.resetModules()` + a dynamic re-import, so no
// test's ingested data or memo cache leaks into another's.
import { describe, expect, it, vi } from "vitest";
import type { FactEvent } from "../src/lib/types/contract1";
import type { AllocationRow, AllocationTrace, OpenActionSpanEvent } from "../src/lib/types/debug";
import { fmtJoules, fmtPct } from "../src/lib/format";

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

let idCounter = 0;
function nextId(): string {
  idCounter += 1;
  return `evt_${String(idCounter).padStart(6, "0")}`;
}

const COLLECTOR = { name: "claude-code", version: "0.1.2" };
const SAMPLER = { name: "codecarbon-sampler", version: "3.0.4" };

function actionSpan(spanId: string, tStartMs: number, tEndMs: number, payload: Record<string, unknown> = {}): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs), // action_span facts are stamped at t_end (SCREENS.md)
    collector: COLLECTOR,
    session_id: "ses_test",
    type: "action_span",
    payload: {
      span_id: spanId,
      tool_name: `Tool(${spanId})`,
      tool_kind: "bash",
      execution_locus: "local",
      t_start: iso(tStartMs),
      t_end: iso(tEndMs),
      status: "ok",
      ...payload,
    },
  } as FactEvent;
}

function energySample(tStartMs: number, tEndMs: number, payload: Record<string, unknown> = {}): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs),
    collector: SAMPLER,
    session_id: "ses_test",
    type: "energy_sample",
    payload: {
      t_start: iso(tStartMs),
      t_end: iso(tEndMs),
      components: [{ kind: "cpu", label: "AMD Ryzen 9 7950X", energy_j: 43.21, method: "rapl" }],
      ...payload,
    },
  } as FactEvent;
}

function openActionSpan(spanId: string, tStartMs: number, payload: Record<string, unknown> = {}): OpenActionSpanEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tStartMs),
    collector: COLLECTOR,
    session_id: "ses_test",
    type: "action_span",
    payload: {
      span_id: spanId,
      tool_name: `Tool(${spanId})`,
      tool_kind: "bash",
      execution_locus: "local",
      t_start: iso(tStartMs),
      status: "ok",
      ...payload,
    },
  } as OpenActionSpanEvent;
}

function llmCall(tsMs: number): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tsMs),
    collector: COLLECTOR,
    session_id: "ses_test",
    type: "llm_call",
    payload: {
      provider: "anthropic",
      model_id_requested: "claude-x",
      usage: { input_tokens: 100, output_tokens: 50 },
      usage_source: "api_response",
    },
  } as FactEvent;
}

function makeRow(spanId: string, overrides: Partial<AllocationRow> = {}): AllocationRow {
  return {
    span_id: spanId,
    tool_name: `Tool(${spanId})`,
    execution_locus: "local",
    overlap_ms: 960,
    cpu_delta_ms: 716,
    share: 0.0224,
    allocated_j: 1.9,
    l1_allocated_j: 41.0,
    excluded: false,
    excluded_reason: null,
    ...overrides,
  };
}

function makeTrace(sampleEventId: string, overrides: Partial<AllocationTrace> = {}): AllocationTrace {
  return {
    sample_event_id: sampleEventId,
    t_start: "2026-07-25T00:00:00.000Z",
    t_end: "2026-07-25T00:00:02.000Z",
    total_j: 84.64,
    components: [{ kind: "cpu", energy_j: 84.64, method: "rapl" }],
    attribution_policy: "l2_cpu_time",
    denominator_cpu_ms: 16000,
    rows: [],
    agent_process: { pid: 4412, cpu_delta_ms: 237, allocated_j: 0.63 },
    baseline: { allocated_j: 82.0, share: 0.969, label: "baseline/idle" },
    l1_shadow_sum_share: 0.62,
    ...overrides,
  };
}

/** Fresh module graph per call: brand-new `eventStore`/`allocStore`
 * singletons AND brand-new `memo1` closures inside `inspector.ts`. */
async function freshEnv() {
  vi.resetModules();
  const eventStoreMod = await import("../src/lib/stores/eventStore.svelte");
  const allocStoreMod = await import("../src/lib/stores/allocStore.svelte");
  const inspector = await import("../src/lib/selectors/inspector");
  return { eventStore: eventStoreMod.eventStore, allocStore: allocStoreMod.allocStore, ...inspector };
}

// ---------------------------------------------------------------------------
// Selection convergence
// ---------------------------------------------------------------------------

describe("selection convergence", () => {
  it("selecting a closed action_span by its event_id (Stream/correlated/decision-ref route) or its span_id (Timeline-bar/decision-ref route) produces an identical Inspector model", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    const span = actionSpan("spn_conv", 1000, 5000, { tool_kind: "mcp", execution_locus: "local", cgroup: "cg1" }) as Extract<FactEvent, { type: "action_span" }>;
    eventStore.ingestFact(span);
    eventStore.flush();

    const byEventId = selectInspector(eventStore.rev, span.event_id);
    const bySpanId = selectInspector(eventStore.rev, span.payload.span_id);

    expect(byEventId).not.toBeNull();
    expect(bySpanId).not.toBeNull();
    // Not necessarily the SAME reference (different memo cache key — the
    // selectedId argument differs) but the CONTENT must converge exactly.
    expect(bySpanId).toEqual(byEventId);
    expect(bySpanId?.title).toBe("Tool(spn_conv)");
    expect(bySpanId?.rows.find((r) => r.key === "cgroup")?.value).toBe("cg1");
  });

  it("an open span (no closing fact yet) resolves via its span_id to an honest, non-null model", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    eventStore.ingestOpenSpan(openActionSpan("spn_open", 1000, { tool_name: "Bash(cargo build)" }));
    eventStore.flush();

    const model = selectInspector(eventStore.rev, "spn_open");
    expect(model).not.toBeNull();
    expect(model?.kind).toBe("action_span");
    expect(model?.title).toBe("Bash(cargo build)");
    expect(model?.sub).toContain("open");
    // Honest about what isn't known yet — never fabricated.
    expect(model?.rows.find((r) => r.key === "t_end")?.value).toBe("— (open)");
    expect(model?.rows.find((r) => r.key === "cgroup")?.value).not.toBe("cg1");
  });

  it("still returns null for an id that matches nothing at all", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    eventStore.ingestFact(llmCall(0));
    eventStore.flush();
    expect(selectInspector(eventStore.rev, "not_a_real_id")).toBeNull();
    expect(selectInspector(eventStore.rev, null)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// selectSpanEnergy — the span-level sum
// ---------------------------------------------------------------------------

describe("selectSpanEnergy", () => {
  it("sums allocated_j ONLY across rows matching the selected span_id, across every overlapping trace — ignoring other spans' rows in the same traces", async () => {
    const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
    const span = actionSpan("spn_target", 0, 10_000);
    eventStore.ingestFact(span);
    const sample1 = energySample(0, 4000);
    eventStore.ingestFact(sample1);
    const sample2 = energySample(4000, 10_000);
    eventStore.ingestFact(sample2);
    eventStore.flush();

    // Each trace carries a row for spn_target AND a row for an unrelated
    // span — if the sum ever picked up the other span's allocated_j, this
    // test would catch it (the "hand-summed fixture" check the brief asks
    // for is exactly this: 1.23 + 4.56, never anything else).
    allocStore.ingest(
      makeTrace(sample1.event_id, { rows: [makeRow("spn_target", { allocated_j: 1.23, share: 0.01 }), makeRow("spn_other", { allocated_j: 99, share: 0.9 })] }),
    );
    allocStore.ingest(
      makeTrace(sample2.event_id, { rows: [makeRow("spn_target", { allocated_j: 4.56, share: 0.02 }), makeRow("spn_other", { allocated_j: 77, share: 0.7 })] }),
    );
    allocStore.flush();

    const model = selectSpanEnergy(eventStore.rev, allocStore.rev, "spn_target");
    expect(model).not.toBeNull();
    expect(model?.totalJ).toBeCloseTo(1.23 + 4.56, 6);
    expect(model?.totalLabel).toBe(fmtJoules(1.23 + 4.56));
    expect(model?.samples.length).toBe(2);
    expect(model?.samples.every((s) => s.status === "ready")).toBe(true);
  });

  it("sums MULTIPLE rows matching the same span_id within one trace (defensive: 'rows' plural)", async () => {
    const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
    const span = actionSpan("spn_multi", 0, 4000);
    eventStore.ingestFact(span);
    const sample = energySample(0, 4000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    allocStore.ingest(makeTrace(sample.event_id, { rows: [makeRow("spn_multi", { allocated_j: 1.0 }), makeRow("spn_multi", { allocated_j: 2.0 })] }));
    allocStore.flush();

    const model = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
    expect(model?.totalJ).toBeCloseTo(3.0, 6);
  });

  it("returns null when the selection isn't an action_span", async () => {
    const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
    const call = llmCall(0);
    eventStore.ingestFact(call);
    eventStore.flush();
    expect(selectSpanEnergy(eventStore.rev, allocStore.rev, call.event_id)).toBeNull();
    expect(selectSpanEnergy(eventStore.rev, allocStore.rev, null)).toBeNull();
  });

  it("a ready trace with no row at all for the selected span renders an honest neutral note, never a 0-fraction 'this span' segment", async () => {
    const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
    const span = actionSpan("spn_no_row", 0, 4000);
    eventStore.ingestFact(span);
    const sample = energySample(0, 4000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    // Trace is ready (allocStore has it), but carries only an unrelated
    // span's row — spn_no_row itself never got an allocation row.
    allocStore.ingest(makeTrace(sample.event_id, { rows: [makeRow("spn_other", { allocated_j: 5, share: 0.1 })] }));
    allocStore.flush();

    const model = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
    expect(model?.samples.length).toBe(1);
    const row = model?.samples[0];
    expect(row?.status).toBe("ready");
    expect(row?.noRowNote).toBe("no allocation recorded for this span in this sample");
    // The whole point: no "this span" segment (0-fraction or otherwise) —
    // segments is simply absent, not an array containing a fabricated 0.
    expect(row?.segments).toBeUndefined();
    // Never counted toward the span-level total either.
    expect(model?.totalJ).toBe(0);
  });

  it("a matching row that is `excluded: true` still counts as a real match: its server value (0 J) is summed, no 'no allocation recorded' note", async () => {
    const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
    const span = actionSpan("spn_excluded", 0, 4000, { execution_locus: "remote" });
    eventStore.ingestFact(span);
    const sample = energySample(0, 4000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    // The trace's only row for spn_excluded is excluded (remote) — 0 joules,
    // but it IS a real row: matchingRows.length is 1, not 0.
    const excludedRow = makeRow("spn_excluded", { execution_locus: "remote", allocated_j: 0, share: 0, excluded: true, excluded_reason: "execution_locus: remote" });
    allocStore.ingest(makeTrace(sample.event_id, { rows: [excludedRow] }));
    allocStore.flush();

    const model = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
    expect(model?.samples.length).toBe(1);
    const row = model?.samples[0];
    expect(row?.status).toBe("ready");
    // Row count reflected: the excluded row IS a match, so this is NOT the
    // "no allocation recorded" honesty case (that's reserved for a trace
    // carrying literally zero rows for this span_id) — segments are built.
    expect(row?.noRowNote).toBeUndefined();
    expect(row?.segments).toBeDefined();
    expect(row?.segments?.[0]).toMatchObject({ label: "this span", value_j: 0, fraction: 0 });
    // The span-energy sum includes the excluded row's own server value (0),
    // it isn't skipped/filtered out of the Σ — ready, not pending/unavailable.
    expect(model?.totalJ).toBe(0);
    expect(model?.totalLabel).toBe(fmtJoules(0));
  });

  describe("pending/unavailable honesty — never 0", () => {
    it("a sample never fetched renders as 'pending', not 0, and doesn't count toward totalJ", async () => {
      const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
      const span = actionSpan("spn_pending", 0, 4000);
      eventStore.ingestFact(span);
      const sample = energySample(0, 4000);
      eventStore.ingestFact(sample);
      eventStore.flush();
      // Deliberately no allocStore.ingest() — simulates "not fetched yet".

      const model = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
      expect(model?.samples).toEqual([{ sampleEventId: sample.event_id, label: expect.any(String), status: "pending" }]);
      expect(model?.totalJ).toBe(0);
      expect(model?.totalLabel).toBe("trace pending");
      expect(model?.totalLabel).not.toContain("0 J");
    });

    it("a 404'd sample renders as 'unavailable (outside window)', not 0", async () => {
      // Stub the global `fetch` BEFORE the (fresh) module graph is imported:
      // `AllocStore`'s constructor default-binds `fetch.bind(globalThis)` at
      // construction time, so the singleton must be built AFTER the stub is
      // in place for `allocStore.fetch()` below to hit it rather than a real
      // network call.
      vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ status: 404, ok: false, json: async () => ({}) } as unknown as Response));
      const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
      const span = actionSpan("spn_unavail", 0, 4000);
      eventStore.ingestFact(span);
      const sample = energySample(0, 4000);
      eventStore.ingestFact(sample);
      eventStore.flush();

      // Drive the singleton `allocStore` through its own real fetch() path
      // — exactly what the tab container's fetch-triggering effect does.
      // `AllocStore.doFetch()` caches the `unavailable` marker itself; this
      // test never reaches into the store's private fields.
      await allocStore.fetch(sample.event_id);
      allocStore.flush();
      vi.unstubAllGlobals();

      const model = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
      expect(model?.samples[0]).toEqual({ sampleEventId: sample.event_id, label: expect.any(String), status: "unavailable" });
      expect(model?.totalLabel).toBe("trace unavailable (outside window)");
      expect(model?.totalLabel).not.toContain("0 J");
    });

    it("a partial mix (one ready, one pending) sums only the ready trace — an honest partial, not 0", async () => {
      const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
      const span = actionSpan("spn_partial", 0, 8000);
      eventStore.ingestFact(span);
      const readySample = energySample(0, 4000);
      eventStore.ingestFact(readySample);
      const pendingSample = energySample(4000, 8000);
      eventStore.ingestFact(pendingSample);
      eventStore.flush();

      allocStore.ingest(makeTrace(readySample.event_id, { rows: [makeRow("spn_partial", { allocated_j: 2.5 })] }));
      allocStore.flush();

      const model = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
      expect(model?.totalJ).toBeCloseTo(2.5, 6);
      expect(model?.totalLabel).toBe(fmtJoules(2.5));
      const statuses = model?.samples.map((s) => s.status).sort();
      expect(statuses).toEqual(["pending", "ready"]);
    });

    it("no overlapping energy samples at all renders an honest 'no samples' label, not 0", async () => {
      const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
      const span = actionSpan("spn_alone", 0, 1000);
      eventStore.ingestFact(span);
      eventStore.flush();

      const model = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
      expect(model?.samples).toEqual([]);
      expect(model?.totalLabel).toBe("no energy samples overlapping yet");
    });
  });
});

// ---------------------------------------------------------------------------
// selectSampleShare — the full-trace bar
// ---------------------------------------------------------------------------

describe("selectSampleShare", () => {
  it("builds one segment per row + agent_process + baseline, fractions/values matching the trace verbatim", async () => {
    const { eventStore, allocStore, selectSampleShare } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    const trace = makeTrace(sample.event_id, {
      total_j: 100,
      rows: [makeRow("spn_a", { allocated_j: 10, share: 0.1 })],
      agent_process: { pid: 1, cpu_delta_ms: 50, allocated_j: 5 },
      baseline: { allocated_j: 85, share: 0.85, label: "baseline/idle" },
    });
    allocStore.ingest(trace);
    allocStore.flush();

    const model = selectSampleShare(eventStore.rev, allocStore.rev, sample.event_id);
    expect(model?.status).toBe("ready");
    expect(model?.segments).toHaveLength(3);

    const [rowSeg, agentSeg, baselineSeg] = model!.segments!;
    expect(rowSeg).toMatchObject({ label: "spn_a", value_j: 10, fraction: 0.1, fill: "accent" });
    // agent_process carries no `.share` field on the trace — its fraction is
    // the one sanctioned display-scaling exception (allocated_j / total_j).
    expect(agentSeg).toMatchObject({ label: "agent process", value_j: 5, fraction: 5 / 100, fill: "accent300" });
    expect(baselineSeg).toMatchObject({ label: "baseline/idle", value_j: 85, fraction: 0.85, fill: "neutral-hatch" });
  });

  it("an excluded (execution_locus: remote) row gets the neutral-hatch fill, its reason noted in the title, never the alarm axis", async () => {
    const { eventStore, allocStore, selectSampleShare } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    allocStore.ingest(
      makeTrace(sample.event_id, {
        rows: [makeRow("spn_remote", { execution_locus: "remote", allocated_j: 0, share: 0, excluded: true, excluded_reason: "execution_locus: remote" })],
      }),
    );
    allocStore.flush();

    const model = selectSampleShare(eventStore.rev, allocStore.rev, sample.event_id);
    const excludedSeg = model?.segments?.find((s) => s.label === "spn_remote");
    expect(excludedSeg?.fill).toBe("neutral-hatch");
    expect(excludedSeg?.title).toContain("excluded");
  });

  it("a never-fetched sample renders 'pending', with no segments to show", async () => {
    const { eventStore, allocStore, selectSampleShare } = await freshEnv();
    const pendingSample = energySample(0, 2000);
    eventStore.ingestFact(pendingSample);
    eventStore.flush();

    const pendingModel = selectSampleShare(eventStore.rev, allocStore.rev, pendingSample.event_id);
    expect(pendingModel).toEqual({ sampleEventId: pendingSample.event_id, status: "pending" });
  });

  it("a 404'd sample renders 'unavailable', with no segments to show", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ status: 404, ok: false, json: async () => ({}) } as unknown as Response));
    const { eventStore, allocStore, selectSampleShare } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    await allocStore.fetch(sample.event_id);
    allocStore.flush();
    vi.unstubAllGlobals();

    const model = selectSampleShare(eventStore.rev, allocStore.rev, sample.event_id);
    expect(model).toEqual({ sampleEventId: sample.event_id, status: "unavailable" });
  });

  it("returns null when the selection isn't an energy_sample", async () => {
    const { eventStore, allocStore, selectSampleShare } = await freshEnv();
    const span = actionSpan("spn_x", 0, 1000);
    eventStore.ingestFact(span);
    eventStore.flush();
    expect(selectSampleShare(eventStore.rev, allocStore.rev, span.event_id)).toBeNull();
    expect(selectSampleShare(eventStore.rev, allocStore.rev, null)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// selectRelevantSampleIds — what the container should fetch
// ---------------------------------------------------------------------------

describe("selectRelevantSampleIds", () => {
  it("for an energy_sample selection, returns just that sample's own id", async () => {
    const { eventStore, selectRelevantSampleIds } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();
    expect(selectRelevantSampleIds(eventStore.rev, sample.event_id)).toEqual([sample.event_id]);
  });

  it("for an action_span selection, returns every overlapping energy_sample's id, not the non-overlapping ones", async () => {
    const { eventStore, selectRelevantSampleIds } = await freshEnv();
    const span = actionSpan("spn_range", 2000, 6000);
    eventStore.ingestFact(span);
    const overlapping1 = energySample(1000, 3000); // overlaps [2000,6000)
    eventStore.ingestFact(overlapping1);
    const overlapping2 = energySample(5000, 7000); // overlaps
    eventStore.ingestFact(overlapping2);
    const before = energySample(0, 1000); // ends before span starts — no overlap
    eventStore.ingestFact(before);
    const after = energySample(7000, 8000); // starts after span ends — no overlap
    eventStore.ingestFact(after);
    eventStore.flush();

    const ids = selectRelevantSampleIds(eventStore.rev, span.event_id);
    expect(new Set(ids)).toEqual(new Set([overlapping1.event_id, overlapping2.event_id]));
  });

  it("returns [] for a selection with no relevant samples (llm_call, or nothing selected)", async () => {
    const { eventStore, selectRelevantSampleIds } = await freshEnv();
    const call = llmCall(0);
    eventStore.ingestFact(call);
    eventStore.flush();
    expect(selectRelevantSampleIds(eventStore.rev, call.event_id)).toEqual([]);
    expect(selectRelevantSampleIds(eventStore.rev, null)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// Memoisation
// ---------------------------------------------------------------------------

describe("memoisation", () => {
  it("selectSpanEnergy returns the same reference for unchanged (rev, allocRev, selectedId), a new one when allocRev changes", async () => {
    const { eventStore, allocStore, selectSpanEnergy } = await freshEnv();
    const span = actionSpan("spn_memo", 0, 4000);
    eventStore.ingestFact(span);
    const sample = energySample(0, 4000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    const first = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
    const second = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
    expect(second).toBe(first);

    allocStore.ingest(makeTrace(sample.event_id, { rows: [makeRow("spn_memo", { allocated_j: 1 })] }));
    allocStore.flush();
    const third = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
    expect(third).not.toBe(first);
  });

  it("selectSampleShare and selectInspector are likewise reference-stable across identical args", async () => {
    const { eventStore, allocStore, selectSampleShare, selectInspector } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();
    allocStore.ingest(makeTrace(sample.event_id));
    allocStore.flush();

    const shareA = selectSampleShare(eventStore.rev, allocStore.rev, sample.event_id);
    const shareB = selectSampleShare(eventStore.rev, allocStore.rev, sample.event_id);
    expect(shareB).toBe(shareA);

    const inspA = selectInspector(eventStore.rev, sample.event_id);
    const inspB = selectInspector(eventStore.rev, sample.event_id);
    expect(inspB).toBe(inspA);
  });
});

// ---------------------------------------------------------------------------
// Provenance discipline — every rendered joule/percent string traceable to a
// real trace field via format.ts (same technique as Task 7's provenance
// test: build the allowed set by formatting every reachable value, then
// assert every numeric token found in the rendered output is in that set).
// ---------------------------------------------------------------------------

describe("provenance discipline", () => {
  /** Every `fmtJoules`/`fmtPct` numeric token embedded in a string —
   * matches "1.23 J", "82 J", "1.23 kJ", "2.2%", etc. `kJ` is checked before
   * the bare `J` alternative so it isn't split into "k" + "J". */
  const NUMERIC_TOKEN_RE = /-?\d[\d,]*(?:\.\d+)?\s?(?:kJ|J|%)/g;

  function extractTokens(strings: readonly string[]): string[] {
    const out: string[] = [];
    for (const s of strings) {
      const m = s.match(NUMERIC_TOKEN_RE);
      if (m) out.push(...m);
    }
    return out;
  }

  it("selectSpanEnergy + selectSampleShare render no numeric token that isn't `format.ts` applied to a real trace field (or the two named display aggregations)", async () => {
    const { eventStore, allocStore, selectSpanEnergy, selectSampleShare } = await freshEnv();
    const span = actionSpan("spn_prov", 0, 4000);
    eventStore.ingestFact(span);
    const sample = energySample(0, 4000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    // Sentinel decimals distinguishable from each other; span_id/tool_name
    // kept letters-only so they can never accidentally look like a
    // numeric+unit token themselves.
    const row = makeRow("spn_prov", { allocated_j: 1.23, share: 0.0224 });
    const otherRow = makeRow("spn_other", { allocated_j: 6.78, share: 0.0891 });
    const trace = makeTrace(sample.event_id, {
      total_j: 84.64,
      rows: [row, otherRow],
      agent_process: { pid: 4412, cpu_delta_ms: 237, allocated_j: 0.63 },
      baseline: { allocated_j: 82.0, share: 0.969, label: "baseline/idle" },
    });
    allocStore.ingest(trace);
    allocStore.flush();

    const spanEnergy = selectSpanEnergy(eventStore.rev, allocStore.rev, span.event_id);
    const sampleShare = selectSampleShare(eventStore.rev, allocStore.rev, sample.event_id);
    expect(spanEnergy).not.toBeNull();
    expect(sampleShare).not.toBeNull();

    // The allowed set: every trace field this task's brief sanctions,
    // formatted through format.ts — plus the ONE display aggregation
    // (`selectSpanEnergy`'s Σ rows[].allocated_j for spn_prov) and the ONE
    // display-scaling proportion (agent_process.allocated_j / total_j).
    const allowedTokens = new Set<string>([
      fmtJoules(row.allocated_j),
      fmtJoules(otherRow.allocated_j),
      fmtJoules(trace.agent_process.allocated_j),
      fmtJoules(trace.baseline.allocated_j),
      fmtPct(row.share),
      fmtPct(otherRow.share),
      fmtPct(trace.baseline.share),
      fmtPct(trace.agent_process.allocated_j / trace.total_j),
      // selectSpanEnergy's own sanctioned sum (only spn_prov's own row here).
      fmtJoules(row.allocated_j),
    ]);

    const renderedStrings: string[] = [
      spanEnergy!.totalLabel,
      ...spanEnergy!.samples.flatMap((s) => (s.segments ?? []).flatMap((seg) => [seg.title, `${seg.value_j}`])),
      ...(sampleShare!.segments ?? []).flatMap((seg) => [seg.title, `${seg.value_j}`]),
    ].map((v) => String(v));

    const tokens = extractTokens(renderedStrings);
    expect(tokens.length).toBeGreaterThan(0); // sanity: the test isn't vacuously true
    for (const token of tokens) {
      expect(allowedTokens.has(token)).toBe(true);
    }
  });
});
