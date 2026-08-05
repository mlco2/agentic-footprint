// Tests for console/src/lib/selectors/attribution.ts (Task 7 brief).
//
// Isolation note: mirrors inspector.test.ts's own convention — `attribution.ts`
// imports the `eventStore`/`allocStore`/`sessionStore` SINGLETONS directly, and
// its `memo1`-wrapped functions live in module-level closures. Each test gets a
// fully fresh module graph via `vi.resetModules()` + a dynamic re-import, so no
// test's ingested data or memo cache leaks into another's.
import { describe, expect, it, vi } from "vitest";
import type { FactEvent } from "../src/lib/types/contract1";
import type { AllocationRow, AllocationTrace } from "../src/lib/types/debug";
import type { SessionInfo } from "../src/lib/types/debug";
import { fmtJoules, fmtMs, fmtMsCount, fmtPct, fmtWatts } from "../src/lib/format";

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
    ts: iso(tEndMs),
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
    t_start: iso(0),
    t_end: iso(2000),
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

function sessionFixture(): SessionInfo {
  return {
    session_id: "ses_test",
    session_meta: { agent_app: { name: "claude-code" } } as unknown as SessionInfo["session_meta"],
    t_start: iso(0),
    attribution_policy: "l2_cpu_time",
    methodology: { version: "v2026.06.1", source: "bundled" },
    grid: { zone: "US-CAL-CISO", g_co2e_per_kwh: 210, source: "electricitymaps" },
    state_dir: "/tmp/af",
    schema_version: "0.1.0",
    mode: "watch",
  };
}

/** Fresh module graph per call: brand-new `eventStore`/`allocStore`/
 * `sessionStore` singletons AND brand-new `memo1` closures inside
 * `attribution.ts`. */
async function freshEnv() {
  vi.resetModules();
  const eventStoreMod = await import("../src/lib/stores/eventStore.svelte");
  const allocStoreMod = await import("../src/lib/stores/allocStore.svelte");
  const sessionStoreMod = await import("../src/lib/stores/sessionStore.svelte");
  const attribution = await import("../src/lib/selectors/attribution");
  return {
    eventStore: eventStoreMod.eventStore,
    allocStore: allocStoreMod.allocStore,
    sessionStore: sessionStoreMod.sessionStore,
    ...attribution,
  };
}

// ---------------------------------------------------------------------------
// selectSampleList
// ---------------------------------------------------------------------------

describe("selectSampleList", () => {
  it("lists every energy_sample newest-first, with interval/total/meta/selected", async () => {
    const { eventStore, allocStore, selectSampleList } = await freshEnv();
    const s1 = energySample(0, 2000);
    eventStore.ingestFact(s1);
    const s2 = energySample(2000, 4000);
    eventStore.ingestFact(s2);
    eventStore.flush();

    allocStore.ingest(makeTrace(s1.event_id, { total_j: 80, rows: [makeRow("spn_a")], baseline: { allocated_j: 78, share: 0.9, label: "baseline/idle" } }));
    allocStore.ingest(makeTrace(s2.event_id, { total_j: 90, rows: [], baseline: { allocated_j: 90, share: 1, label: "baseline/idle" } }));
    allocStore.flush();

    const rows = selectSampleList(eventStore.rev, allocStore.rev, s2.event_id, null);
    expect(rows.map((r) => r.sampleEventId)).toEqual([s2.event_id, s1.event_id]); // newest first
    expect(rows[0].selected).toBe(true);
    expect(rows[1].selected).toBe(false);
    expect(rows[0].status).toBe("ready");
    expect(rows[0].totalLabel).toBe(fmtJoules(90));
    expect(rows[1].metaLabel).toBe(`1 span · idle ${fmtPct(0.9)}`);
  });

  it("a sample with no trace yet renders 'trace pending', never a fabricated total", async () => {
    const { eventStore, allocStore, selectSampleList } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    const rows = selectSampleList(eventStore.rev, allocStore.rev, null, null);
    expect(rows).toEqual([{ sampleEventId: sample.event_id, intervalLabel: expect.any(String), selected: false, status: "pending", pendingLabel: "trace pending" }]);
  });

  it("a 404'd sample renders 'trace unavailable (outside window)'", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ status: 404, ok: false, json: async () => ({}) } as unknown as Response));
    const { eventStore, allocStore, selectSampleList } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();
    await allocStore.fetch(sample.event_id);
    allocStore.flush();
    vi.unstubAllGlobals();

    const rows = selectSampleList(eventStore.rev, allocStore.rev, null, null);
    expect(rows).toEqual([{ sampleEventId: sample.event_id, intervalLabel: expect.any(String), selected: false, status: "unavailable", pendingLabel: "trace unavailable (outside window)" }]);
  });

  it("magenta L1 flag appears ONLY when l1_shadow_sum_share > 1, with the alarm class", async () => {
    const { eventStore, allocStore, selectSampleList } = await freshEnv();
    const over = energySample(0, 2000);
    eventStore.ingestFact(over);
    const under = energySample(2000, 4000);
    eventStore.ingestFact(under);
    eventStore.flush();

    allocStore.ingest(makeTrace(over.event_id, { l1_shadow_sum_share: 1.43 }));
    allocStore.ingest(makeTrace(under.event_id, { l1_shadow_sum_share: 0.62 }));
    allocStore.flush();

    const rows = selectSampleList(eventStore.rev, allocStore.rev, null, null);
    const overRow = rows.find((r) => r.sampleEventId === over.event_id)!;
    const underRow = rows.find((r) => r.sampleEventId === under.event_id)!;
    expect(overRow.l1FlagLabel).toBe(`L1 ${fmtPct(1.43)}`);
    expect(overRow.l1FlagClass).toBe("status-alarm");
    expect(underRow.l1FlagLabel).toBeUndefined();
    expect(underRow.l1FlagClass).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// selectAllocationDetail
// ---------------------------------------------------------------------------

describe("selectAllocationDetail", () => {
  it("returns null when nothing resolvable as an energy_sample is selected", async () => {
    const { eventStore, allocStore, selectAllocationDetail } = await freshEnv();
    const span = actionSpan("spn_x", 0, 1000);
    eventStore.ingestFact(span);
    eventStore.flush();
    expect(selectAllocationDetail(eventStore.rev, allocStore.rev, (span as Extract<FactEvent, { type: "action_span" }>).event_id)).toBeNull();
    expect(selectAllocationDetail(eventStore.rev, allocStore.rev, null)).toBeNull();
  });

  it("pending/unavailable statuses are honest, not fabricated", async () => {
    const { eventStore, allocStore, selectAllocationDetail } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    const pending = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    expect(pending).toEqual({ sampleEventId: sample.event_id, intervalLabel: expect.any(String), status: "pending" });
  });

  it("a 404'd sample's trace renders 'unavailable' status, never a fabricated detail", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ status: 404, ok: false, json: async () => ({}) } as unknown as Response));
    const { eventStore, allocStore, selectAllocationDetail, selectPolicyAside } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();
    await allocStore.fetch(sample.event_id);
    allocStore.flush();
    vi.unstubAllGlobals();

    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    expect(detail).toEqual({ sampleEventId: sample.event_id, intervalLabel: expect.any(String), status: "unavailable" });
    expect(detail?.stats).toBeUndefined();

    const aside = selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id);
    expect(aside).toEqual({ sampleEventId: sample.event_id, status: "unavailable" });
  });

  it("a remote (excluded) row renders a 0-width L2 share segment, the correct joule text, and its excluded_reason note", async () => {
    const { eventStore, allocStore, selectAllocationDetail } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    const remoteRow = makeRow("spn_remote", {
      execution_locus: "remote",
      allocated_j: 0,
      share: 0,
      l1_allocated_j: 0,
      excluded: true,
      excluded_reason: "execution_locus: remote — no local energy attributable",
    });
    allocStore.ingest(makeTrace(sample.event_id, { rows: [remoteRow] }));
    allocStore.flush();

    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    expect(detail?.status).toBe("ready");
    const row = detail!.allocationRows!.find((r) => r.key === "spn_remote")!;
    expect(row.excluded).toBe(true);
    expect(row.shareSegments[0].fraction).toBe(0);
    expect(row.l2JoulesLabel).toBe(fmtJoules(0));
    expect(row.noteLabel).toBe(remoteRow.excluded_reason);
  });

  it("the baseline row is present and non-zero whenever the trace's baseline is non-zero (never rendered as 0)", async () => {
    const { eventStore, allocStore, selectAllocationDetail } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    allocStore.ingest(
      makeTrace(sample.event_id, {
        total_j: 100,
        rows: [makeRow("spn_a", { allocated_j: 3, share: 0.03 })],
        agent_process: { pid: 4412, cpu_delta_ms: 100, allocated_j: 2 },
        baseline: { allocated_j: 95, share: 0.95, label: "baseline/idle" },
      }),
    );
    allocStore.flush();

    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    const baselineRow = detail!.allocationRows!.find((r) => r.kind === "baseline")!;
    expect(baselineRow.l2JoulesLabel).toBe(fmtJoules(95));
    expect(baselineRow.l2JoulesLabel).not.toBe(fmtJoules(0));
    expect(baselineRow.shareSegments[0].fraction).toBe(0.95);
  });

  it("the agent-process row renders its real note when the trace carries one (real-server orphan-bucket nuance)", async () => {
    const { eventStore, allocStore, selectAllocationDetail } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    allocStore.ingest(
      makeTrace(sample.event_id, {
        agent_process: { pid: 4412, cpu_delta_ms: 237, allocated_j: 0.63, note: "l2_cpu_time/v1 has no separate agent-process bucket; this carries the orphan bucket." },
      }),
    );
    allocStore.flush();

    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    const agentRow = detail!.allocationRows!.find((r) => r.kind === "agent")!;
    expect(agentRow.noteLabel).toBe("l2_cpu_time/v1 has no separate agent-process bucket; this carries the orphan bucket.");
    expect(agentRow.shareLabel).toBe("—"); // no `.share` field on agent_process — never fabricated
    expect(agentRow.shareSegments).toEqual([]);
  });

  it("components are solid (measured) for rapl/powermetrics/nvml, hatched for tdp_model", async () => {
    const { eventStore, allocStore, selectAllocationDetail } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    allocStore.ingest(
      makeTrace(sample.event_id, {
        components: [
          { kind: "cpu", energy_j: 60, method: "rapl" },
          { kind: "gpu", energy_j: 20, method: "tdp_model" },
        ],
      }),
    );
    allocStore.flush();

    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    expect(detail!.components!.find((c) => c.method === "rapl")!.hatched).toBe(false);
    expect(detail!.components!.find((c) => c.method === "tdp_model")!.hatched).toBe(true);
  });

  it("the interval strip gives each overlapping span its OWN 19px row (stacked by topPx), not one shared track where they'd occlude each other", async () => {
    const { eventStore, allocStore, selectAllocationDetail, STRIP_ROW_HEIGHT_PX } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    // Two spans that fully overlap each other in wall-clock time — the
    // occlusion case SCREENS.md's "19px rows" (plural) exists to avoid.
    eventStore.ingestFact(actionSpan("spn_a", 0, 2000));
    eventStore.ingestFact(actionSpan("spn_b", 0, 2000));
    eventStore.flush();

    allocStore.ingest(makeTrace(sample.event_id, { rows: [makeRow("spn_a"), makeRow("spn_b")] }));
    allocStore.flush();

    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    const strip = detail!.intervalStrip!;
    expect(strip).toHaveLength(2);
    // Distinct rows (never both at topPx 0), in `trace.rows` order.
    const tops = strip.map((s) => s.topPx);
    expect(new Set(tops).size).toBe(2);
    expect(tops).toEqual([0, STRIP_ROW_HEIGHT_PX]);
  });

  it("notes include the over-attribution warning only when l1_shadow_sum_share > 1, and the remote-exclusion note only when a row is excluded", async () => {
    const { eventStore, allocStore, selectAllocationDetail } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    allocStore.ingest(
      makeTrace(sample.event_id, {
        l1_shadow_sum_share: 1.43,
        rows: [makeRow("spn_remote", { execution_locus: "remote", excluded: true, excluded_reason: "execution_locus: remote", allocated_j: 0, share: 0 })],
      }),
    );
    allocStore.flush();

    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    expect(detail!.notes!.some((n) => n.tone === "alarm" && n.text.includes(fmtPct(1.43)))).toBe(true);
    expect(detail!.notes!.some((n) => n.text.includes("excluded"))).toBe(true);
    expect(detail!.notes!.some((n) => n.text.includes("baseline/idle"))).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// selectPolicyAside
// ---------------------------------------------------------------------------

describe("selectPolicyAside", () => {
  it("substitutes THIS sample's real per-row numbers verbatim, includes denominator_note when present, and the grid block", async () => {
    const { eventStore, allocStore, sessionStore, selectPolicyAside } = await freshEnv();
    sessionStore.set(sessionFixture());
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    const row = makeRow("spn_a", { cpu_delta_ms: 716, share: 0.0224, allocated_j: 1.9 });
    allocStore.ingest(
      makeTrace(sample.event_id, {
        rows: [row],
        denominator_cpu_ms: 16000,
        denominator_note: "machine cpu-time over the interval, not the sum of watched trees",
      }),
    );
    allocStore.flush();

    const aside = selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id);
    expect(aside?.status).toBe("ready");
    expect(aside?.denominatorNote).toBe("machine cpu-time over the interval, not the sum of watched trees");
    expect(aside?.denominatorLabel).toBe(fmtMs(16000));
    const spanFormulaRow = aside!.formulaRows!.find((r) => r.key === "spn_a")!;
    expect(spanFormulaRow.cpuDeltaLabel).toBe(fmtMs(716));
    expect(spanFormulaRow.shareLabel).toBe(fmtPct(0.0224));
    expect(spanFormulaRow.allocatedLabel).toBe(fmtJoules(1.9));
    expect(aside?.gridZone).toBe("US-CAL-CISO");
    expect(aside?.geoNoteLabel).toBe("auto-geolocated: never");
  });

  it("formulaSubstitution types the LARGEST-share row's real numbers into the formula shape — never a recomputed division", async () => {
    const { eventStore, allocStore, selectPolicyAside } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();

    // spn_big has the larger `.share` — its numbers must be the ones
    // substituted, not spn_small's (which has a bigger allocated_j from a
    // different total_j the test isn't distracted by) and not a recomputed
    // cpu_delta_ms/denominator_cpu_ms division of the test's own devising.
    const small = makeRow("spn_small", { cpu_delta_ms: 100, share: 0.01, allocated_j: 0.5 });
    const big = makeRow("spn_big", { cpu_delta_ms: 716, share: 0.0224, allocated_j: 1.9 });
    allocStore.ingest(makeTrace(sample.event_id, { total_j: 84.64, rows: [small, big], denominator_cpu_ms: 32000 }));
    allocStore.flush();

    const aside = selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id);
    const sub = aside!.formulaSubstitution!;
    expect(sub.label).toContain("spn_big");
    expect(sub.label).not.toContain("spn_small");
    expect(sub.shareLine).toBe(`share = ${fmtMsCount(716)} / ${fmtMsCount(32000)} = ${fmtPct(0.0224)}`);
    expect(sub.allocLine).toBe(`alloc_j = ${fmtPct(0.0224)} × ${fmtJoules(84.64)} = ${fmtJoules(1.9)}`);
  });

  it("formulaSubstitution is absent (not fabricated) when the trace has no rows at all", async () => {
    const { eventStore, allocStore, selectPolicyAside } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();
    allocStore.ingest(makeTrace(sample.event_id, { rows: [] }));
    allocStore.flush();

    const aside = selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id);
    expect(aside!.formulaSubstitution).toBeUndefined();
  });

  it("pending/unavailable mirror the detail selector's honesty", async () => {
    const { eventStore, allocStore, selectPolicyAside } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();
    expect(selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id)).toEqual({ sampleEventId: sample.event_id, status: "pending" });
    expect(selectPolicyAside(eventStore.rev, allocStore.rev, null)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Memoisation
// ---------------------------------------------------------------------------

describe("memoisation", () => {
  it("all three selectors are reference-stable across identical args, and recompute when allocRev changes", async () => {
    const { eventStore, allocStore, selectSampleList, selectAllocationDetail, selectPolicyAside } = await freshEnv();
    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    eventStore.flush();
    allocStore.ingest(makeTrace(sample.event_id));
    allocStore.flush();

    const list1 = selectSampleList(eventStore.rev, allocStore.rev, sample.event_id, null);
    const list2 = selectSampleList(eventStore.rev, allocStore.rev, sample.event_id, null);
    expect(list2).toBe(list1);

    const detail1 = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    const detail2 = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    expect(detail2).toBe(detail1);

    const aside1 = selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id);
    const aside2 = selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id);
    expect(aside2).toBe(aside1);

    allocStore.ingest(makeTrace(sample.event_id, { total_j: 999 }));
    allocStore.flush();
    const detail3 = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    expect(detail3).not.toBe(detail1);
  });
});

// ---------------------------------------------------------------------------
// Provenance discipline — every J/kJ/% token rendered by this file's
// selectors must trace to `format.ts` applied to a real trace field, or to
// EXACTLY the two sanctioned display aggregations (avg power, attributed-J
// sum) — same technique as inspector.test.ts's own provenance sweep.
// ---------------------------------------------------------------------------

describe("provenance discipline", () => {
  // Widened from the original J/kJ/% sweep to also catch ms- and
  // W-denominated tokens (fmtMs/fmtMsCount/fmtWatts, format.ts) — this
  // file's rendered strings carry plenty of both (interval-strip titles,
  // the formula substitution's cpu_delta_ms/denominator_cpu_ms line, avg
  // power) and the earlier regex silently skipped every one of them,
  // exempting those fields from the provenance check entirely rather than
  // proving them honest. `\b` after the letter-suffixed units (ms/s/kJ/J/kW/W)
  // stops a bare trailing digit inside ordinary prose — e.g. metaLabel's "1
  // span" — from being misread as a fabricated "1 s" token; `%` needs no
  // such guard since it isn't a word character itself.
  const NUMERIC_TOKEN_RE = /-?\d[\d,]*(?:\.\d+)?\s?(?:kJ\b|J\b|kW\b|W\b|ms\b|s\b|%)/g;

  function extractTokens(strings: readonly string[]): string[] {
    const out: string[] = [];
    for (const s of strings) {
      const m = s.match(NUMERIC_TOKEN_RE);
      if (m) out.push(...m);
    }
    return out;
  }

  it("selectSampleList + selectAllocationDetail + selectPolicyAside render no J/kJ/% token that isn't format.ts applied to a real trace field (or the two named aggregations)", async () => {
    const { eventStore, allocStore, sessionStore, selectSampleList, selectAllocationDetail, selectPolicyAside } = await freshEnv();
    sessionStore.set(sessionFixture());

    const sample = energySample(0, 2000);
    eventStore.ingestFact(sample);
    // Real SpanRecords for the interval strip's geometry lookup.
    eventStore.ingestFact(actionSpan("spn_prov", 200, 1800));
    eventStore.ingestFact(actionSpan("spn_remote", 0, 2000, { execution_locus: "remote", tool_kind: "web" }));
    eventStore.flush();

    // Sentinel decimals, distinguishable from one another.
    const row = makeRow("spn_prov", { overlap_ms: 960, cpu_delta_ms: 716, allocated_j: 1.23, share: 0.0224, l1_allocated_j: 41.17 });
    const remoteRow = makeRow("spn_remote", {
      execution_locus: "remote",
      overlap_ms: 2000,
      cpu_delta_ms: 0,
      allocated_j: 0,
      share: 0,
      l1_allocated_j: 0,
      excluded: true,
      excluded_reason: "execution_locus: remote",
    });
    const trace = makeTrace(sample.event_id, {
      total_j: 84.64,
      components: [
        { kind: "cpu", energy_j: 60.11, method: "rapl" },
        { kind: "gpu", energy_j: 24.53, method: "tdp_model" },
      ],
      denominator_cpu_ms: 16000,
      denominator_note: "machine cpu-time, not sum of watched trees",
      rows: [row, remoteRow],
      agent_process: { pid: 4412, cpu_delta_ms: 237, allocated_j: 0.63, note: "carries the orphan bucket" },
      baseline: { allocated_j: 82.85, share: 0.979, label: "baseline/idle" },
      l1_shadow_sum_share: 1.43, // over 100% — exercises the alarm note/flag path too
    });
    allocStore.ingest(trace);
    allocStore.flush();

    const listRows = selectSampleList(eventStore.rev, allocStore.rev, sample.event_id, null);
    const detail = selectAllocationDetail(eventStore.rev, allocStore.rev, sample.event_id);
    const aside = selectPolicyAside(eventStore.rev, allocStore.rev, sample.event_id);
    expect(detail?.status).toBe("ready");
    expect(aside?.status).toBe("ready");

    // The two sanctioned aggregations, computed independently here and
    // asserted to equal EXACTLY these derivations (brief: "assert these
    // equal formatting of those two specific derivations and nothing else").
    const intervalS = 2; // trace.t_start -> t_end is exactly 2s in makeTrace's default/override
    const avgPowerLabel = fmtWatts(trace.total_j / intervalS);
    const attributedJ = row.allocated_j + remoteRow.allocated_j;
    const attributedLabel = fmtJoules(attributedJ);
    expect(detail!.stats!.find((s) => s.label === "avg power")!.value).toBe(avgPowerLabel);
    expect(detail!.stats!.find((s) => s.label === "attributed")!.value).toBe(attributedLabel);

    const allowedTokens = new Set<string>([
      fmtJoules(trace.total_j),
      fmtJoules(row.allocated_j),
      fmtJoules(remoteRow.allocated_j),
      fmtJoules(trace.agent_process.allocated_j),
      fmtJoules(trace.baseline.allocated_j),
      fmtJoules(trace.components[0].energy_j),
      fmtJoules(trace.components[1].energy_j),
      fmtJoules(row.l1_allocated_j),
      fmtJoules(remoteRow.l1_allocated_j),
      fmtPct(row.share),
      fmtPct(remoteRow.share),
      fmtPct(trace.baseline.share),
      fmtPct(trace.l1_shadow_sum_share),
      // the two sanctioned aggregations' tokens — attributedJ (a J value) and
      // avg power (a W value, now caught by the widened regex too — see this
      // describe block's own header comment — instead of silently skipped).
      attributedLabel,
      avgPowerLabel,
      // ms-denominated tokens the widened regex now also sweeps: the
      // interval strip's own overlap_ms (fmtMs), and the formula
      // substitution's cpu_delta_ms/denominator_cpu_ms line (fmtMsCount) —
      // every one of them a real trace field, formatted through format.ts.
      fmtMs(row.overlap_ms),
      fmtMs(remoteRow.overlap_ms),
      fmtMsCount(row.cpu_delta_ms),
      fmtMsCount(trace.denominator_cpu_ms),
    ]);

    const renderedStrings: string[] = [
      ...listRows.flatMap((r) => [r.totalLabel, r.metaLabel, r.l1FlagLabel, r.pendingLabel]),
      ...(detail!.stats ?? []).map((s) => s.value),
      ...(detail!.intervalStrip ?? []).map((s) => s.title),
      ...(detail!.components ?? []).map((c) => c.jouleLabel),
      ...(detail!.allocationRows ?? []).flatMap((r) => [r.shareLabel, r.l2JoulesLabel, r.l1JoulesLabel, r.noteLabel, ...r.shareSegments.flatMap((seg) => [seg.title, `${seg.value_j}`])]),
      ...(detail!.notes ?? []).map((n) => n.text),
      ...(aside!.formulaRows ?? []).flatMap((r) => [r.shareLabel, r.allocatedLabel]),
      aside!.totalJLabel,
      aside!.formulaSubstitution?.label,
      aside!.formulaSubstitution?.shareLine,
      aside!.formulaSubstitution?.allocLine,
    ].filter((v): v is string => typeof v === "string");

    const tokens = extractTokens(renderedStrings);
    expect(tokens.length).toBeGreaterThan(0); // sanity: the test isn't vacuously true
    for (const token of tokens) {
      expect(allowedTokens.has(token)).toBe(true);
    }
  });
});
