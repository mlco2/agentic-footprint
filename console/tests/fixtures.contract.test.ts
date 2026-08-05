// Contract test for the fixture scenario (DATA-CONTRACT.md §2, §5). This is
// the guardrail that keeps dev/scenario.ts honest: every UI task after this
// one develops against the mock server, so a fixture that quietly drifts
// from the contract (wrong frame name, unbalanced allocation arithmetic,
// a `conformance` key that shouldn't be there) would be a silent lie fed to
// every later task.
import { describe, expect, it } from "vitest";
import { buildScenario } from "../dev/scenario";
import { toSnapshotWatchdog } from "../dev/mock-plugin";
import type { FactEvent } from "../src/lib/types/contract1";
import type { AllocationTrace, SseEventName, SseFrame, WatchdogFrame } from "../src/lib/types/debug";

const SSE_EVENT_NAMES: SseEventName[] = ["fact", "decision", "alloc", "reject", "gap", "watchdog", "report", "health", "reset"];

const scenario = buildScenario();

/** Σ rows.allocated_j + agent_process.allocated_j + baseline.allocated_j ≈ total_j. */
function allocArithmeticError(trace: AllocationTrace): number {
  const rowsTotal = trace.rows.reduce((acc, r) => acc + r.allocated_j, 0);
  const reconstructed = rowsTotal + trace.agent_process.allocated_j + trace.baseline.allocated_j;
  return Math.abs(reconstructed - trace.total_j);
}

describe("buildScenario — compile-time contract shape", () => {
  it("every fact frame's payload is a valid Contract #1 Envelope (FactEvent)", () => {
    // This assertion is load-bearing at the type level, not the runtime one:
    // if scenario.ts ever pushed a `fact` frame whose payload didn't satisfy
    // FactEvent's discriminated union, this file would fail to typecheck
    // (`npm run check`) before a single test ran.
    const facts: FactEvent[] = scenario.frames.filter((f) => f.event === "fact").map((f) => f.data);
    expect(facts.length).toBeGreaterThan(0);
  });
});

describe("buildScenario — runtime invariants", () => {
  it("every frame's event name is one of the §2.3 SSE event names", () => {
    for (const frame of scenario.frames) {
      expect(SSE_EVENT_NAMES).toContain(frame.event);
    }
  });

  it("atMs is monotonically non-decreasing and non-negative across the frame log", () => {
    let prev = -1;
    for (const frame of scenario.frames) {
      expect(frame.atMs).toBeGreaterThanOrEqual(0);
      expect(frame.atMs).toBeGreaterThanOrEqual(prev);
      prev = frame.atMs;
    }
  });

  it("every energy_sample fact has a corresponding alloc trace (item 10)", () => {
    const energySampleIds = scenario.frames
      .filter((f) => f.event === "fact" && (f.data as FactEvent).type === "energy_sample")
      .map((f) => (f.data as FactEvent).event_id);
    expect(energySampleIds.length).toBeGreaterThan(0);
    for (const id of energySampleIds) {
      expect(scenario.allocs.has(id), `missing alloc trace for energy_sample ${id}`).toBe(true);
    }
    // ... and no extras: every alloc trace maps back to a real energy_sample.
    expect(scenario.allocs.size).toBe(energySampleIds.length);
  });

  it("alloc arithmetic is consistent: Σrows + agent + baseline ≈ total_j (±0.01)", () => {
    for (const trace of scenario.allocs.values()) {
      expect(allocArithmeticError(trace), `sample ${trace.sample_event_id} arithmetic off`).toBeLessThanOrEqual(0.01);
    }
  });

  it("a deliberately broken fixture is NOT arithmetically consistent (the check has teeth)", () => {
    const [sample] = scenario.allocs.values();
    const broken: AllocationTrace = { ...sample, baseline: { ...sample.baseline, allocated_j: sample.baseline.allocated_j + 5 } };
    expect(allocArithmeticError(broken)).toBeGreaterThan(0.01);
  });

  it("item 2: at least one sample has l1_shadow_sum_share > 1 while its L2 row shares sum <= 1", () => {
    const overAttributed = [...scenario.allocs.values()].filter((t) => t.l1_shadow_sum_share > 1);
    expect(overAttributed.length).toBeGreaterThan(0);
    for (const trace of overAttributed) {
      const l2ShareSum = trace.rows.reduce((acc, r) => acc + r.share, 0);
      expect(l2ShareSum).toBeLessThanOrEqual(1);
    }
  });

  it("item 1: the long bash span's overlapping samples show baseline dominant (~0.9+ idle share)", () => {
    const bashRowSamples = [...scenario.allocs.values()].filter((t) => t.rows.some((r) => r.span_id === "spn_0001"));
    expect(bashRowSamples.length).toBeGreaterThan(0);
    for (const trace of bashRowSamples) {
      expect(trace.baseline.share).toBeGreaterThanOrEqual(0.85);
    }
  });

  it("item 7: execution_locus remote rows are excluded, with a reason, and contribute 0 joules", () => {
    const remoteRows = [...scenario.allocs.values()].flatMap((t) => t.rows).filter((r) => r.execution_locus === "remote");
    expect(remoteRows.length).toBeGreaterThan(0);
    for (const row of remoteRows) {
      expect(row.excluded).toBe(true);
      expect(row.excluded_reason).toBeTruthy();
      expect(row.allocated_j).toBe(0);
    }
  });

  it("item 3: a gap frame is present with the contract's exact reason/collector", () => {
    const gaps = scenario.frames.filter((f) => f.event === "gap");
    expect(gaps.length).toBeGreaterThan(0);
    const gap = gaps[0].data as { reason: string; collector: string; t_start: string; t_end: string };
    expect(gap.reason).toBe("sampler restarted");
    expect(gap.collector).toBe("codecarbon-sampler");
  });

  it("item 4: the orphan watchdog entry appears only in frames after its span closes", () => {
    const spanCloseAtMs = scenario.frames.find((f) => f.event === "fact" && (f.data as FactEvent).type === "action_span" && (f.data as FactEvent).payload.span_id === "spn_0006")!.atMs;
    const watchdogFrames = scenario.frames.filter((f): f is Extract<SseFrame, { event: "watchdog" }> => f.event === "watchdog");
    expect(watchdogFrames.length).toBeGreaterThan(0);
    for (const frame of watchdogFrames) {
      const hasOrphan = frame.data.pids.some((e) => e.span_id === "spn_0006" && e.state === "orphaned");
      if (hasOrphan) {
        expect(frame.atMs).toBeGreaterThan(spanCloseAtMs);
      }
    }
    const anyOrphanFrame = watchdogFrames.some((f) => f.data.pids.some((e) => e.state === "orphaned"));
    expect(anyOrphanFrame).toBe(true);
  });

  it("watchdog wire shapes follow DATA-CONTRACT's own asymmetry: §2.3's SSE frame is wrapped `{ pids: [...] }`, §2.2's Snapshot.watchdog stays a bare array", () => {
    const watchdogFrames = scenario.frames.filter((f): f is Extract<SseFrame, { event: "watchdog" }> => f.event === "watchdog");
    expect(watchdogFrames.length).toBeGreaterThan(0);

    // §2.3: the stream frame's `data:` is an object wrapping the array, not the array itself.
    const sample: WatchdogFrame = watchdogFrames[0].data;
    expect(sample).toHaveProperty("pids");
    expect(Array.isArray(sample.pids)).toBe(true);
    expect(Array.isArray(sample)).toBe(false);

    // §2.2: the same production unwrap the mock server's /debug/snapshot handler
    // uses (mock-plugin.ts's toSnapshotWatchdog) must strip the wrapper back to
    // a bare WatchdogEntry[], never forwarding the wire wrapper into a snapshot.
    const orphanFrame = watchdogFrames.find((f) => f.data.pids.some((e) => e.state === "orphaned"))!;
    const snapshotShape = toSnapshotWatchdog(orphanFrame.data);
    expect(Array.isArray(snapshotShape)).toBe(true);
    expect(snapshotShape).toEqual(orphanFrame.data.pids);
    expect(snapshotShape.some((e) => e.state === "orphaned")).toBe(true);

    // Missing frame -> empty bare array (never undefined, never the wrapper).
    expect(toSnapshotWatchdog(undefined)).toEqual([]);
  });

  it("report's local_measured energy/gwp are never rounded away to a false 0 (\"not measured\" != 0)", () => {
    const local = scenario.report.impact_join.local_measured!;
    expect(local.energy!.total.min).toBeGreaterThan(0);
    expect(local.gwp!.total.min).toBeGreaterThan(0);
    // adpe is naturally ~1e-6 in kgSbeq — the same rounding trap.
    const acmeOrSonnet = scenario.report.by_model.find((g) => g.model_id === "claude-sonnet-4-5-20250929");
    expect(acmeOrSonnet!.impacts.adpe!.total.min).toBeGreaterThan(0);
  });

  it("item 5: the unknown-model llm_call is counted under unknown_model and excluded from by_model impacts", () => {
    expect(scenario.report.estimation_status_histogram.unknown_model).toBeGreaterThan(0);
    const unknownGroup = scenario.report.by_model.find((g) => g.model_id === "acme-mystery-7b");
    expect(unknownGroup).toBeDefined();
    expect(unknownGroup!.impacts.energy).toBeUndefined();
  });

  it("item 6: a reject frame carries the exact malformed-JSON reason and byte offset", () => {
    const rejects = scenario.frames.filter((f) => f.event === "reject");
    expect(rejects.length).toBe(1);
    const reject = rejects[0].data as { reason: string; byte_offset: number };
    expect(reject.reason).toBe("malformed JSON: unexpected end of input at byte offset 41822");
    expect(reject.byte_offset).toBe(41822);
  });

  it("item 8: an llm_call with usage_source transcript is present", () => {
    const transcriptCalls = scenario.frames.filter(
      (f) => f.event === "fact" && (f.data as FactEvent).type === "llm_call" && (f.data as Extract<FactEvent, { type: "llm_call" }>).payload.usage_source === "transcript",
    );
    expect(transcriptCalls.length).toBeGreaterThan(0);
  });

  it("item 9: health has no conformance key, and the otlp receiver is http/json on 4318 (never 4317)", () => {
    expect("conformance" in scenario.health).toBe(false);
    expect(scenario.health.otlp_receiver.endpoint).toBe("127.0.0.1:4318");
    expect(scenario.health.otlp_receiver.protocol).toBe("http/json");
    expect(scenario.health.otlp_receiver.endpoint).not.toContain("4317");
  });

  it("item 10: at least two agent_telemetry llm_calls carry full token usage", () => {
    const calls = scenario.frames
      .filter((f) => f.event === "fact" && (f.data as FactEvent).type === "llm_call")
      .map((f) => f.data as Extract<FactEvent, { type: "llm_call" }>)
      .filter((f) => f.payload.usage_source === "agent_telemetry");
    expect(calls.length).toBeGreaterThanOrEqual(2);
    for (const call of calls) {
      expect(call.payload.usage.input_tokens).toBeGreaterThan(0);
      expect(call.payload.usage.output_tokens).toBeGreaterThan(0);
    }
  });

  it("item 10: decision frames of all four kinds (ingest, span_open, attr, orphan) are present", () => {
    const kinds = new Set(scenario.frames.filter((f): f is Extract<SseFrame, { event: "decision" }> => f.event === "decision").map((f) => f.data.kind));
    expect(kinds).toEqual(new Set(["ingest", "span_open", "attr", "orphan"]));
  });

  it("item 10: energy_sample components use both rapl and powermetrics methods", () => {
    const methods = new Set([...scenario.allocs.values()].flatMap((t) => t.components.map((c) => c.method)));
    expect(methods.has("rapl")).toBe(true);
    expect(methods.has("powermetrics")).toBe(true);
  });
});
