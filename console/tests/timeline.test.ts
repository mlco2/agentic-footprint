// @vitest-environment happy-dom
//
// Tests for console/src/lib/selectors/timeline.ts (Task 5 brief). The whole
// file runs under happy-dom (rather than node, vitest's default) because the
// last describe block mounts a real Svelte component tree (`svelte`'s
// `mount`/`unmount`) to exercise hidden-tab discipline; every other test in
// this file is plain logic and is unaffected by which environment runs it.
//
// Isolation note (matches tests/stream.test.ts): timeline.ts imports the
// `eventStore` SINGLETON directly, and its `memo1`-wrapped selectors live in
// module-level closures. Every test gets a fresh module graph via
// `vi.resetModules()` + a dynamic re-import, so no test's ingested data or
// memo cache leaks into another's.
import { describe, expect, it, vi } from "vitest";
import type { FactEvent } from "../src/lib/types/contract1";
import type { DecisionFrame, GapFrame, WatchdogEntry } from "../src/lib/types/debug";
import { fmtMs, fmtTokens } from "../src/lib/format";

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

function llmCall(atMs: number, durationMs: number, payload: Record<string, unknown> = {}): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(atMs),
    collector: COLLECTOR,
    session_id: "ses_test",
    type: "llm_call",
    payload: {
      provider: "anthropic",
      model_id_requested: "claude-x",
      usage: { input_tokens: 100, output_tokens: 40 },
      usage_source: "api_response",
      duration_ms: durationMs,
      status: "ok",
      ...payload,
    },
  } as FactEvent;
}

function actionSpan(tStartMs: number, tEndMs: number, payload: Record<string, unknown> = {}): FactEvent {
  idCounter += 1;
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs),
    collector: COLLECTOR,
    session_id: "ses_test",
    type: "action_span",
    payload: {
      span_id: `spn_${idCounter}`,
      tool_name: "Bash(cargo test)",
      tool_kind: "bash",
      execution_locus: "local",
      t_start: iso(tStartMs),
      t_end: iso(tEndMs),
      pids: [4242],
      status: "ok",
      ...payload,
    },
  } as FactEvent;
}

function energySample(tStartMs: number, tEndMs: number, components: Record<string, unknown>[] = [{ kind: "cpu", energy_j: 10, method: "rapl" }]): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs),
    collector: SAMPLER,
    session_id: "ses_test",
    type: "energy_sample",
    payload: { t_start: iso(tStartMs), t_end: iso(tEndMs), components },
  } as FactEvent;
}

function processSample(tStartMs: number, tEndMs: number, processes: { pid: number; cpu_time_delta_ms: number }[]): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs),
    collector: SAMPLER,
    session_id: "ses_test",
    type: "process_sample",
    payload: { t_start: iso(tStartMs), t_end: iso(tEndMs), processes },
  } as FactEvent;
}

function gapFrame(tStartMs: number, tEndMs: number, reason = "sampler restarted", collector = "codecarbon-sampler"): GapFrame {
  return { t_start: iso(tStartMs), t_end: iso(tEndMs), reason, collector };
}

function watchdog(entry: Partial<WatchdogEntry> & Pick<WatchdogEntry, "pid" | "span_id" | "cmd" | "state">): WatchdogEntry {
  return { cpu_pct: 10, rss_bytes: 1_000_000, ...entry };
}

function decision(kind: DecisionFrame["kind"], atMs: number, text: string, ref?: string): DecisionFrame {
  return { kind, ts: iso(atMs), text, ref };
}

/** Fresh module graph per call: a brand-new `eventStore` singleton AND
 * brand-new `memo1` closures inside `timeline.ts` — no test's ingested data
 * or memo cache can leak into another's. */
async function freshEnv() {
  vi.resetModules();
  const eventStoreMod = await import("../src/lib/stores/eventStore.svelte");
  const timeline = await import("../src/lib/selectors/timeline");
  return { eventStore: eventStoreMod.eventStore, ...timeline };
}

const NOW = 200_000; // comfortably inside a 180s trailing window from most fixture timestamps

describe("selectTimelineLanes: geometry", () => {
  it("packs two overlapping action_spans onto different tracks", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    eventStore.ingestFact(actionSpan(60_000, 90_000));
    eventStore.ingestFact(actionSpan(65_000, 85_000)); // fully inside the first — must overlap
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const spanBars = lanes.bars.filter((b) => b.kind === "action_span");
    expect(spanBars.length).toBe(2);
    expect(spanBars[0].topPx).not.toBe(spanBars[1].topPx);
  });

  it("droppedSpans is 0 for 7 or fewer concurrent overlapping action_spans (every one gets a track)", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    for (let i = 0; i < 7; i += 1) {
      eventStore.ingestFact(actionSpan(100_000, 150_000, { span_id: `spn_track_${i}` }));
    }
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    expect(lanes.bars.filter((b) => b.kind === "action_span").length).toBe(7);
    expect(lanes.droppedSpans).toBe(0);
  });

  it("droppedSpans counts overlapping action_spans beyond the 7-track cap — the '+N spans not shown' cue's count", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    // 9 fully-overlapping spans compete for the same 7 tracks: 2 must be dropped.
    for (let i = 0; i < 9; i += 1) {
      eventStore.ingestFact(actionSpan(100_000, 150_000, { span_id: `spn_over_${i}` }));
    }
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    expect(lanes.bars.filter((b) => b.kind === "action_span").length).toBe(7);
    expect(lanes.droppedSpans).toBe(2);
  });

  it("a span outside the trailing window produces no bar", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    // Window is [NOW-180000, NOW) = [20000, 200000). This span ends at 5000 — well before it.
    eventStore.ingestFact(actionSpan(1_000, 5_000));
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    expect(lanes.bars.some((b) => b.kind === "action_span")).toBe(false);
    expect(lanes.spanCount).toBe(0);
  });

  it("a gap band appears iff a gap record exists — never inferred from missing samples", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    // Two samples with a real hole between them, but NO gap record.
    eventStore.ingestFact(energySample(100_000, 102_000));
    eventStore.ingestFact(energySample(140_000, 142_000));
    eventStore.flush();

    const withoutGapRecord = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    expect(withoutGapRecord.bars.some((b) => b.kind === "gap")).toBe(false);

    eventStore.ingestGap(gapFrame(102_000, 140_000));
    eventStore.flush();

    const withGapRecord = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const gapBars = withGapRecord.bars.filter((b) => b.kind === "gap");
    expect(gapBars.length).toBe(1);
    expect(gapBars[0].hatch).toBe("alarm");
    // Full plot height, per the brief.
    expect(gapBars[0].heightPx).toBe(withGapRecord.plotHeightPx);
  });

  it("an orphan tail appears only for a watchdog entry in state 'orphaned'", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    const orphanedSpan = actionSpan(50_000, 60_000, { span_id: "spn_orphan" });
    const openSpan = actionSpan(70_000, 80_000, { span_id: "spn_open" });
    eventStore.ingestFact(orphanedSpan);
    eventStore.ingestFact(openSpan);
    eventStore.replaceWatchdog([
      watchdog({ pid: 111, span_id: "spn_orphan", cmd: "leaked", state: "orphaned", orphaned_since: iso(60_000), outlived_span_by_ms: 40_000 }),
      watchdog({ pid: 222, span_id: "spn_open", cmd: "still running", state: "open" }),
    ]);
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const orphanBars = lanes.bars.filter((b) => b.kind === "orphan");
    expect(orphanBars.length).toBe(1);
    expect(orphanBars[0].id).toBe("spn_orphan");
    expect(orphanBars[0].hatch).toBe("alarm");
    expect(orphanBars[0].title).toContain("spn_orphan");
    expect(orphanBars[0].title).toContain("pid 111");

    // Regression: the orphaned span's own action_span bar and its orphan-tail
    // bar legitimately SHARE an `id` (same span_id) — LaneChart.svelte's
    // `{#each}` key must combine `kind` with `id` to stay unique, or Svelte
    // throws `each_key_duplicate` at runtime (caught live via `npm run dev`
    // once the mock's scenario orphan actually fired). Every (kind, id) pair
    // across the whole flat bars array must be unique.
    const keys = lanes.bars.map((b) => `${b.kind}:${b.id}`);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("llm_call ticks are hatched iff usage_source is transcript/estimated", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    const apiResponse = llmCall(100_000, 500, { usage_source: "api_response" });
    const telemetry = llmCall(101_000, 500, { usage_source: "agent_telemetry" });
    const transcript = llmCall(102_000, 500, { usage_source: "transcript" });
    const estimated = llmCall(103_000, 500, { usage_source: "estimated" });
    for (const e of [apiResponse, telemetry, transcript, estimated]) eventStore.ingestFact(e);
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const byId = new Map(lanes.bars.filter((b) => b.kind === "llm_call").map((b) => [b.id, b]));
    expect(byId.get(apiResponse.event_id)?.hatch).toBe("none");
    expect(byId.get(telemetry.event_id)?.hatch).toBe("none");
    expect(byId.get(transcript.event_id)?.hatch).toBe("alarm");
    expect(byId.get(estimated.event_id)?.hatch).toBe("alarm");
  });

  it("a remote action_span is hatched (neutral); a local one is not", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    const local = actionSpan(100_000, 105_000, { span_id: "spn_local", execution_locus: "local" });
    const remote = actionSpan(100_000, 105_000, { span_id: "spn_remote", execution_locus: "remote", pids: [] });
    eventStore.ingestFact(local);
    eventStore.ingestFact(remote);
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const byId = new Map(lanes.bars.filter((b) => b.kind === "action_span").map((b) => [b.id, b]));
    expect(byId.get("spn_local")?.hatch).toBe("none");
    expect(byId.get("spn_remote")?.hatch).toBe("neutral");
    expect(byId.get("spn_remote")?.fillVar).toBe("transparent");
    expect(byId.get("spn_remote")?.title).toContain("excluded from local energy join");
  });

  it("covers all six bar sources in ONE flat array, not per-lane sub-arrays", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    eventStore.ingestFact(llmCall(100_000, 500));
    eventStore.ingestFact(actionSpan(100_000, 110_000, { span_id: "spn_a" }));
    eventStore.ingestFact(energySample(100_000, 102_000));
    eventStore.ingestFact(processSample(100_000, 102_000, [{ pid: 1, cpu_time_delta_ms: 50 }]));
    eventStore.ingestGap(gapFrame(60_000, 62_000));
    eventStore.replaceWatchdog([watchdog({ pid: 9, span_id: "spn_a", cmd: "leaked", state: "orphaned", orphaned_since: iso(110_000), outlived_span_by_ms: 5000 })]);
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const kinds = new Set(lanes.bars.map((b) => b.kind));
    expect(kinds).toEqual(new Set(["llm_call", "action_span", "energy_sample", "process_sample", "gap", "orphan"]));
    expect(Array.isArray(lanes.bars)).toBe(true); // one flat array, confirmed by the mixed-kind membership above
  });

  it("energy-sample bar height is proportional to watts relative to the window max, bottom-anchored", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    // 2s interval: 10J -> 5W; 20J -> 10W (2x). Heights should reflect that ratio.
    eventStore.ingestFact(energySample(100_000, 102_000, [{ kind: "cpu", energy_j: 10, method: "rapl" }]));
    eventStore.ingestFact(energySample(102_000, 104_000, [{ kind: "cpu", energy_j: 20, method: "rapl" }]));
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const energyBars = lanes.bars.filter((b) => b.kind === "energy_sample").sort((a, b) => a.leftPct - b.leftPct);
    expect(energyBars.length).toBe(2);
    expect(energyBars[1].heightPx).toBeGreaterThan(energyBars[0].heightPx);
    // Both bottom-anchored to the same lane bottom.
    expect(energyBars[0].topPx + energyBars[0].heightPx).toBe(energyBars[1].topPx + energyBars[1].heightPx);
    expect(energyBars[0].title).toContain("cpu");
    expect(energyBars[0].title).toContain("rapl");
  });

  it("llm_call hover title carries model, in/out tokens, duration, and reads alarm-hatched for transcript usage", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    const call = llmCall(100_000, 2350, {
      model_id_requested: "claude-sonnet-4-5-20250929",
      usage: { input_tokens: 18420, output_tokens: 642 },
      usage_source: "transcript",
    });
    eventStore.ingestFact(call);
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const bar = lanes.bars.find((b) => b.kind === "llm_call" && b.id === call.event_id)!;
    expect(bar.title).toContain("claude-sonnet-4-5-20250929");
    expect(bar.title).toContain(fmtTokens(18420));
    expect(bar.title).toContain(fmtTokens(642));
    expect(bar.title).toContain(fmtMs(2350));
    expect(bar.title).toContain("transcript");
    expect(bar.hatch).toBe("alarm"); // usage_source: transcript is one of the alarm-rank sources
  });

  it("process_sample hover title carries each watched tree's own pid + cpu delta, not just the lane's aggregate", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    const sample = processSample(100_000, 102_000, [
      { pid: 21044, cpu_time_delta_ms: 360 },
      { pid: 4412, cpu_time_delta_ms: 40 },
    ]);
    eventStore.ingestFact(sample);
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const bar = lanes.bars.find((b) => b.kind === "process_sample" && b.id === sample.event_id)!;
    expect(bar.title).toContain("pid 21044");
    expect(bar.title).toContain(fmtMs(360));
    expect(bar.title).toContain("pid 4412");
    expect(bar.title).toContain(fmtMs(40));
    // Still carries the aggregate too (cpuMs = 360 + 40 = 400, across 2 trees).
    expect(bar.title).toContain(fmtMs(400));
    expect(bar.title).toContain("2 watched trees");
  });

  it("gap band hover title carries the server's own reason and collector", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    eventStore.ingestGap(gapFrame(100_000, 105_000, "sampler restarted", "codecarbon-sampler"));
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    const bar = lanes.bars.find((b) => b.kind === "gap")!;
    expect(bar.title).toContain("sampler restarted");
    expect(bar.title).toContain("codecarbon-sampler");
    expect(bar.title).toContain(fmtMs(5000));
  });

  it("hiding a type removes its whole lane (label + bars), not just its bars", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    eventStore.ingestFact(llmCall(100_000, 500));
    eventStore.ingestFact(actionSpan(100_000, 110_000));
    eventStore.flush();

    const shown = selectTimelineLanes(eventStore.rev, NOW, new Set(), null);
    expect(shown.laneLabels.some((l) => l.label === "llm_call")).toBe(true);
    expect(shown.bars.some((b) => b.kind === "llm_call")).toBe(true);

    const hidden = selectTimelineLanes(eventStore.rev, NOW, new Set(["llm_call"]), null);
    expect(hidden.laneLabels.some((l) => l.label === "llm_call")).toBe(false);
    expect(hidden.bars.some((b) => b.kind === "llm_call")).toBe(false);
    // action_span lane shifts up to reclaim the space (topPx of its label is smaller).
    const shownSpanLabel = shown.laneLabels.find((l) => l.label === "action_span");
    const hiddenSpanLabel = hidden.laneLabels.find((l) => l.label === "action_span");
    expect(hiddenSpanLabel?.topPx).toBeLessThan(shownSpanLabel?.topPx ?? Infinity);
  });

  it("marks the selected bar", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    const span = actionSpan(100_000, 110_000, { span_id: "spn_sel" });
    eventStore.ingestFact(span);
    eventStore.flush();

    const lanes = selectTimelineLanes(eventStore.rev, NOW, new Set(), "spn_sel");
    const bar = lanes.bars.find((b) => b.id === "spn_sel");
    expect(bar?.selected).toBe(true);
  });
});

describe("selectTimelineLanes: track stability across ticks", () => {
  it("keeps a span's track when an earlier, overlapping span expires from the window (no reshuffle)", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    // spn_A: [0, 10_000) — will age out of the window at the second tick.
    // spn_B: [5_000, 300_000) — overlaps spn_A, and stays in the window at
    // both ticks (its own real end is far beyond either nowMs used below).
    eventStore.ingestFact(actionSpan(0, 10_000, { span_id: "spn_A" }));
    eventStore.ingestFact(actionSpan(5_000, 300_000, { span_id: "spn_B" }));
    eventStore.flush();

    // Tick 1: window = [-165000, 15000) — both spans overlap.
    const tick1 = selectTimelineLanes(eventStore.rev, 15_000, new Set(), null);
    const aBar1 = tick1.bars.find((b) => b.kind === "action_span" && b.id === "spn_A");
    const bBar1 = tick1.bars.find((b) => b.kind === "action_span" && b.id === "spn_B");
    expect(aBar1).toBeDefined();
    expect(bBar1).toBeDefined();
    expect(aBar1?.topPx).not.toBe(bBar1?.topPx); // distinct tracks (they overlap)

    // Tick 2: window = [15000, 195000) — spn_A's real end (10_000) is now
    // before the window start, so it drops out entirely; spn_B is still
    // well within the window (its own end, 300_000, is still ahead).
    const tick2 = selectTimelineLanes(eventStore.rev, 195_000, new Set(), null);
    const aBar2 = tick2.bars.find((b) => b.kind === "action_span" && b.id === "spn_A");
    const bBar2 = tick2.bars.find((b) => b.kind === "action_span" && b.id === "spn_B");
    expect(aBar2).toBeUndefined(); // spn_A has genuinely left the window
    expect(bBar2).toBeDefined();
    // The bug this guards against: a from-scratch refit (sorted by
    // tStartMs, first-fit from track 0) would see only spn_B left, hand it
    // track 0, and its row would jump — even though spn_B itself never
    // moved or changed. It must keep the SAME row it had in tick 1.
    expect(bBar2?.topPx).toBe(bBar1?.topPx);
  });

  it("a late-arriving, overlapping span with an earlier tStartMs does not move an already-assigned span", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    eventStore.ingestFact(actionSpan(50_000, 150_000, { span_id: "spn_B" }));
    eventStore.flush();

    const before = selectTimelineLanes(eventStore.rev, 160_000, new Set(), null);
    const bBarBefore = before.bars.find((b) => b.kind === "action_span" && b.id === "spn_B");
    expect(bBarBefore).toBeDefined();

    // Arrives LATER in ingestion order, but its own tStartMs is EARLIER than
    // spn_B's, and it overlaps spn_B — a from-scratch refit sorted by
    // tStartMs would process this one first, hand it track 0, and bump
    // spn_B to a different track.
    eventStore.ingestFact(actionSpan(45_000, 55_000, { span_id: "spn_A" }));
    eventStore.flush();

    const after = selectTimelineLanes(eventStore.rev, 160_000, new Set(), null);
    const bBarAfter = after.bars.find((b) => b.kind === "action_span" && b.id === "spn_B");
    const aBarAfter = after.bars.find((b) => b.kind === "action_span" && b.id === "spn_A");
    expect(bBarAfter?.topPx).toBe(bBarBefore?.topPx); // spn_B's row is unchanged
    expect(aBarAfter).toBeDefined();
    expect(aBarAfter?.topPx).not.toBe(bBarAfter?.topPx); // spn_A gets its own, different row
  });
});

describe("selectTimelineLanes: memoisation", () => {
  it("same (rev, nowMs, filters, selection) -> same LaneModel reference", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    eventStore.ingestFact(actionSpan(100_000, 110_000));
    eventStore.flush();

    const first = selectTimelineLanes(eventStore.rev, NOW, new Set(["session_meta"]), null);
    const second = selectTimelineLanes(eventStore.rev, NOW, new Set(["session_meta"]), null); // new Set, same contents
    expect(second).toBe(first);

    const differentNow = selectTimelineLanes(eventStore.rev, NOW + 1000, new Set(["session_meta"]), null);
    expect(differentNow).not.toBe(first);

    eventStore.ingestFact(actionSpan(120_000, 130_000));
    eventStore.flush(); // bumps rev
    const afterMutation = selectTimelineLanes(eventStore.rev, NOW, new Set(["session_meta"]), null);
    expect(afterMutation).not.toBe(first);
  });

  it("stays reference-stable for repeated identical calls even once sticky track assignments exist", async () => {
    const { eventStore, selectTimelineLanes } = await freshEnv();
    // Two overlapping spans force the sticky assignment map to actually
    // populate — this guards against a regression where wiring in
    // module-level track state accidentally broke memo1's caching (e.g. by
    // mutating something on every call regardless of a cache hit).
    eventStore.ingestFact(actionSpan(50_000, 60_000, { span_id: "spn_x" }));
    eventStore.ingestFact(actionSpan(55_000, 65_000, { span_id: "spn_y" }));
    eventStore.flush();

    const first = selectTimelineLanes(eventStore.rev, 100_000, new Set(), null);
    const second = selectTimelineLanes(eventStore.rev, 100_000, new Set(), null);
    expect(second).toBe(first);
    expect(second.bars).toBe(first.bars);
  });
});

describe("selectDecisionLog", () => {
  it("caps [attr] at 8 while retaining other kinds, newest-first", async () => {
    const { eventStore, selectDecisionLog } = await freshEnv();
    for (let i = 0; i < 25; i += 1) {
      eventStore.ingestDecision(decision("attr", 1000 + i, `attr line ${i}`));
    }
    eventStore.ingestDecision(decision("span_open", 2000, "span opened"));
    eventStore.ingestDecision(decision("ingest", 2001, "fact ingested"));
    eventStore.ingestDecision(decision("orphan", 2002, "pid orphaned", "spn_x"));
    eventStore.flush();

    const rows = selectDecisionLog(eventStore.rev);
    const attrRows = rows.filter((r) => r.kind === "attr");
    expect(attrRows.length).toBe(8);
    expect(rows.some((r) => r.kind === "span_open")).toBe(true);
    expect(rows.some((r) => r.kind === "ingest")).toBe(true);
    expect(rows.some((r) => r.kind === "orphan")).toBe(true);
    // Newest-first — `ts` is a fixed-width zero-padded "hh:mm:ss.SSS" string
    // (format.ts's fmtClock), so a plain string comparison is chronological
    // for same-day timestamps like this fixture's.
    for (let i = 1; i < rows.length; i += 1) {
      expect(rows[i].ts <= rows[i - 1].ts).toBe(true);
    }
  });

  it("caps total visible rows at 30 even with no [attr] lines at all", async () => {
    const { eventStore, selectDecisionLog } = await freshEnv();
    for (let i = 0; i < 40; i += 1) {
      eventStore.ingestDecision(decision("span_open", 1000 + i, `span ${i}`));
    }
    eventStore.flush();

    const rows = selectDecisionLog(eventStore.rev);
    expect(rows.length).toBe(30);
  });

  it("flags orphan rows with kind 'orphan' and the [orphan] prefix", async () => {
    const { eventStore, selectDecisionLog } = await freshEnv();
    eventStore.ingestDecision(decision("orphan", 5000, "pid 111 outlived spn_1 by 40.0s", "spn_1"));
    eventStore.flush();

    const rows = selectDecisionLog(eventStore.rev);
    expect(rows[0].kind).toBe("orphan");
    expect(rows[0].prefixLabel).toBe("[orphan]");
    expect(rows[0].ref).toBe("spn_1");
  });

  // Item 5 (DecisionLog stable keys): `key` is `eventStore`'s per-decision
  // `seq`, assigned once at ingest — it must never change for an existing
  // row just because a newer decision arrived and shifted everyone else's
  // array index (rows render newest-first, so every prior row's index
  // changes on every new arrival; only `seq` doesn't).
  it("assigns each decision a stable key that survives newer decisions arriving (newest-first reordering)", async () => {
    const { eventStore, selectDecisionLog } = await freshEnv();
    eventStore.ingestDecision(decision("ingest", 1000, "first"));
    eventStore.ingestDecision(decision("span_open", 1001, "second"));
    eventStore.flush();

    const before = selectDecisionLog(eventStore.rev);
    expect(before.map((r) => r.text)).toEqual(["second", "first"]); // newest-first
    const keyOfFirst = before.find((r) => r.text === "first")!.key;
    const keyOfSecond = before.find((r) => r.text === "second")!.key;
    expect(keyOfFirst).not.toBe(keyOfSecond);

    eventStore.ingestDecision(decision("attr", 1002, "third"));
    eventStore.flush();

    const after = selectDecisionLog(eventStore.rev);
    expect(after.map((r) => r.text)).toEqual(["third", "second", "first"]); // "first"/"second" both shifted position
    expect(after.find((r) => r.text === "first")!.key, "prior row's key must be unchanged despite its index shifting").toBe(keyOfFirst);
    expect(after.find((r) => r.text === "second")!.key, "prior row's key must be unchanged despite its index shifting").toBe(keyOfSecond);
  });
});

describe("selectRail", () => {
  it("clamps idle at 0 and buckets dot state at the 12s/45s thresholds", async () => {
    const { eventStore, selectRail } = await freshEnv();
    eventStore.ingestFact(llmCall(NOW - 5_000, 100)); // idle 5s -> accent
    eventStore.ingestFact({ ...(llmCall(NOW - 20_000, 100) as FactEvent), collector: { name: "otlp-cc", version: "0.1.0" } }); // idle 20s -> neutral
    eventStore.ingestFact({ ...(llmCall(NOW - 50_000, 100) as FactEvent), collector: { name: "codecarbon-sampler", version: "3.0.4" } }); // idle 50s -> alarm
    // A fact stamped slightly AHEAD of "now" (client clock vs. server ts) must clamp to 0 idle, not negative.
    eventStore.ingestFact({ ...(llmCall(NOW + 500, 100) as FactEvent), collector: { name: "ahead-of-now", version: "0.0.1" } });
    eventStore.flush();

    const rail = selectRail(eventStore.rev, NOW);
    const byName = new Map(rail.collectors.map((c) => [c.name, c]));
    expect(byName.get("claude-code")?.dotClass).toBe("dot-accent");
    expect(byName.get("otlp-cc")?.dotClass).toBe("dot-neutral");
    expect(byName.get("codecarbon-sampler")?.dotClass).toBe("dot-alarm");
    expect(byName.get("ahead-of-now")?.dotClass).toBe("dot-accent");
  });

  it("reuses M4's per-type counts and never fabricates a per-collector rate", async () => {
    const { eventStore, selectRail } = await freshEnv();
    eventStore.ingestFact(llmCall(NOW - 1000, 100));
    eventStore.ingestFact(actionSpan(NOW - 5000, NOW - 1000));
    eventStore.flush();

    const rail = selectRail(eventStore.rev, NOW);
    const llmType = rail.types.find((t) => t.type === "llm_call");
    const spanType = rail.types.find((t) => t.type === "action_span");
    expect(llmType?.count).toBe(eventStore.perType.get("llm_call"));
    expect(spanType?.count).toBe(eventStore.perType.get("action_span"));
    for (const c of rail.collectors) expect(c.eventsPerSLabel).toBe("—");
  });

  it("produces a magenta orphan summary line iff any watchdog entry is orphaned", async () => {
    const { eventStore, selectRail } = await freshEnv();
    eventStore.replaceWatchdog([watchdog({ pid: 1, span_id: "spn_1", cmd: "still running", state: "open" })]);
    eventStore.flush();
    expect(selectRail(eventStore.rev, NOW).orphanSummary).toBeNull();

    eventStore.replaceWatchdog([watchdog({ pid: 2, span_id: "spn_2", cmd: "leaked", state: "orphaned", orphaned_since: iso(NOW - 10_000), outlived_span_by_ms: 9000 })]);
    eventStore.flush();
    const summary = selectRail(eventStore.rev, NOW).orphanSummary;
    expect(summary).not.toBeNull();
    expect(summary).toContain("pid 2");
    expect(summary).toContain("spn_2");
  });
});

describe("Timeline hidden-tab discipline", () => {
  it("computes lane geometry only while the Timeline tab is mounted", async () => {
    // Deliberately NOT `vi.resetModules()` here: this suite's other blocks
    // reset modules per-test (via `freshEnv()`) to isolate `eventStore`
    // state, which also (transitively) reloads Svelte's own internal client
    // runtime each time. `mount`/`unmount`/`flushSync` must come from the
    // SAME loaded instance of "svelte" that `TabHarness.svelte` (and the
    // Timeline/Stream components it renders) end up compiled against, or
    // Svelte's internal effect bookkeeping is split across two incompatible
    // runtime instances and mounting corrupts. Fetching every one of these
    // — "svelte" itself included — via `await import(...)` in this same
    // tick, with no intervening reset, keeps them all on one consistent
    // module-registry snapshot regardless of what earlier tests left cached.
    vi.doMock("../src/lib/selectors/timeline", async (importOriginal) => {
      const actual = await importOriginal<typeof import("../src/lib/selectors/timeline")>();
      return { ...actual, selectTimelineLanes: vi.fn(actual.selectTimelineLanes) };
    });

    const { mount, unmount, flushSync } = await import("svelte");
    const { uiStore } = await import("../src/lib/stores/uiStore.svelte");
    const timelineSelectors = await import("../src/lib/selectors/timeline");
    const { default: TabHarness } = await import("./TabHarness.svelte");
    const spy = timelineSelectors.selectTimelineLanes as unknown as ReturnType<typeof vi.fn>;

    uiStore.setTab("stream");
    const target = document.createElement("div");
    document.body.appendChild(target);
    const instance = mount(TabHarness, { target });
    flushSync();

    expect(spy).not.toHaveBeenCalled(); // Stream is showing — Timeline never mounted, selector never runs

    uiStore.setTab("timeline");
    flushSync();
    expect(spy.mock.calls.length).toBeGreaterThan(0); // now mounted — the title row alone reads `lanes`

    const callsWhileVisible = spy.mock.calls.length;
    uiStore.setTab("stream"); // unmounts Timeline again
    flushSync();
    flushSync(); // a second flush would catch any lingering re-render still calling the selector
    expect(spy.mock.calls.length).toBe(callsWhileVisible); // no further calls once hidden again

    unmount(instance);
    document.body.removeChild(target);
    vi.doUnmock("../src/lib/selectors/timeline");
  });
});
