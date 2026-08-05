// Tests for console/src/lib/selectors/stream.ts (Task 4 brief).
//
// Isolation note: `stream.ts` imports the `eventStore` SINGLETON directly
// (DATA-CONTRACT §3.5's pattern — the store isn't a selector parameter), and
// its `memo1`-wrapped functions live in module-level closures. To keep every
// test's store state and memo cache independent, each test gets a fully
// fresh module graph via `vi.resetModules()` + a dynamic re-import, rather
// than sharing one singleton across the file.
import { describe, expect, it, vi } from "vitest";
import type { FactEvent } from "../src/lib/types/contract1";

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

let idCounter = 0;
function nextId(): string {
  idCounter += 1;
  return `evt_${String(idCounter).padStart(6, "0")}`;
}

const COLLECTOR = { name: "claude-code", version: "0.1.2" };

function llmCall(tsMs: number, payload: Record<string, unknown> = {}, envelope: Record<string, unknown> = {}): FactEvent {
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
      usage: { input_tokens: 18420, output_tokens: 642 },
      usage_source: "api_response",
      ...payload,
    },
    ...envelope,
  } as FactEvent;
}

function actionSpan(tStartMs: number, tEndMs: number, payload: Record<string, unknown> = {}, envelope: Record<string, unknown> = {}): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs), // action_span facts are stamped at t_end (SCREENS.md)
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
      status: "ok",
      ...payload,
    },
    ...envelope,
  } as FactEvent;
}

function energySample(tStartMs: number, tEndMs: number, payload: Record<string, unknown> = {}): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs),
    collector: { name: "codecarbon-sampler", version: "3.0.4" },
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

function processSample(tStartMs: number, tEndMs: number, processCount = 2): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: nextId(),
    ts: iso(tEndMs),
    collector: { name: "codecarbon-sampler", version: "3.0.4" },
    session_id: "ses_test",
    type: "process_sample",
    payload: {
      t_start: iso(tStartMs),
      t_end: iso(tEndMs),
      processes: Array.from({ length: processCount }, (_, i) => ({ pid: 1000 + i, cpu_time_delta_ms: 100 * (i + 1) })),
    },
  } as FactEvent;
}

/** Fresh module graph per call: a brand-new `eventStore` singleton AND
 * brand-new `memo1` closures inside `stream.ts` — so no test's ingested data
 * or memo cache can leak into another's. */
async function freshEnv() {
  vi.resetModules();
  const eventStoreMod = await import("../src/lib/stores/eventStore.svelte");
  const stream = await import("../src/lib/selectors/stream");
  return { eventStore: eventStoreMod.eventStore, ...stream };
}

describe("selectStreamRows", () => {
  it("sorts by ts DESC, not arrival order — end-stamped spans vs sampler cadence", async () => {
    const { eventStore, selectStreamRows } = await freshEnv();

    // Arrival order: span (ts=5000, stamped at its end) first, then a sample
    // that arrived later in wall-clock ingest order but carries an EARLIER
    // ts, then a call with the latest ts. Insertion order != chronological
    // order, exactly the case SCREENS.md calls out.
    const span = actionSpan(1000, 5000);
    eventStore.ingestFact(span);
    const sample = energySample(1000, 3000);
    eventStore.ingestFact(sample);
    const call = llmCall(8000);
    eventStore.ingestFact(call);
    eventStore.flush();

    const { rows } = selectStreamRows(eventStore.rev, new Set());
    expect(rows.map((r) => r.id)).toEqual([call.event_id, span.event_id, sample.event_id]);
  });

  it("filter toggling recomputes rows and counts", async () => {
    const { eventStore, selectStreamRows } = await freshEnv();
    eventStore.ingestFact(llmCall(1000));
    eventStore.ingestFact(actionSpan(1000, 2000));
    eventStore.ingestFact(energySample(2000, 3000));
    eventStore.flush();

    const unfiltered = selectStreamRows(eventStore.rev, new Set());
    expect(unfiltered.total).toBe(3);
    expect(unfiltered.shown).toBe(3);

    const filtered = selectStreamRows(eventStore.rev, new Set(["llm_call"]));
    expect(filtered.total).toBe(2);
    expect(filtered.rows.some((r) => r.type === "llm_call")).toBe(false);
  });

  it("memoises on (rev, filters): identical filter contents (even a new Set instance) hit the cache", async () => {
    const { eventStore, selectStreamRows } = await freshEnv();
    eventStore.ingestFact(llmCall(1000));
    eventStore.ingestFact(actionSpan(1000, 2000));
    eventStore.flush();

    const first = selectStreamRows(eventStore.rev, new Set(["session_meta"]));
    const second = selectStreamRows(eventStore.rev, new Set(["session_meta"])); // different Set object, same contents
    expect(second).toBe(first);
    expect(second.rows).toBe(first.rows);

    const third = selectStreamRows(eventStore.rev, new Set(["session_meta", "llm_call"])); // different filter
    expect(third).not.toBe(first);

    eventStore.ingestFact(energySample(3000, 4000));
    eventStore.flush(); // bumps rev
    const fourth = selectStreamRows(eventStore.rev, new Set(["session_meta", "llm_call"]));
    expect(fourth).not.toBe(third); // rev changed -> recompute even though filter args match
  });

  it("caps rendered rows and reports shown/total separately", async () => {
    const { eventStore, selectStreamRows } = await freshEnv();
    for (let i = 0; i < 10; i += 1) {
      eventStore.ingestFact(llmCall(1000 + i));
    }
    eventStore.flush();

    const { rows, shown, total } = selectStreamRows(eventStore.rev, new Set(), 5);
    expect(total).toBe(10);
    expect(shown).toBe(5);
    expect(rows.length).toBe(5);
  });

  it("formats collector as name@version and ts as hh:mm:ss.SSS", async () => {
    const { eventStore, selectStreamRows } = await freshEnv();
    const tsMs = new Date(2026, 0, 1, 9, 40, 12, 4).getTime();
    eventStore.ingestFact(llmCall(tsMs));
    eventStore.flush();

    const { rows } = selectStreamRows(eventStore.rev, new Set());
    expect(rows[0].collector).toBe("claude-code@0.1.2");
    expect(rows[0].ts).toBe("09:40:12.004");
  });

  it("attribution renders '—' when no attribution is present, else a short form of the deepest id", async () => {
    const { eventStore, selectStreamRows } = await freshEnv();
    eventStore.ingestFact(llmCall(1000));
    eventStore.ingestFact(llmCall(2000, {}, { attribution: { task_id: "tsk_deadbeef01" } }));
    eventStore.flush();

    const { rows } = selectStreamRows(eventStore.rev, new Set());
    const withoutAttr = rows.find((r) => r.attribution === "—");
    const withAttr = rows.find((r) => r.attribution !== "—");
    expect(withoutAttr).toBeDefined();
    expect(withAttr?.attribution).toContain("deadbeef01".slice(-8));
  });

  it("badges llm_call by usage_source provenance rank; transcript/estimated get the alarm class", async () => {
    const { eventStore, selectStreamRows, usageSourceBadgeClass } = await freshEnv();
    eventStore.ingestFact(llmCall(1000, { usage_source: "api_response" }));
    eventStore.ingestFact(llmCall(2000, { usage_source: "agent_telemetry" }));
    eventStore.ingestFact(llmCall(3000, { usage_source: "transcript" }));
    eventStore.ingestFact(llmCall(4000, { usage_source: "estimated" }));
    eventStore.flush();

    const { rows } = selectStreamRows(eventStore.rev, new Set());
    const byId = new Map(rows.map((r) => [r.sourceMethod, r]));
    expect(byId.get("api_response")?.sourceMethodClass).toBe(usageSourceBadgeClass("api_response"));
    expect(byId.get("transcript")?.sourceMethodClass).toMatch(/prov-2|prov-3/);
    expect(byId.get("estimated")?.sourceMethodClass).toMatch(/prov-2|prov-3/);
    // The two alarm ranks must differ from the two non-alarm ranks.
    expect(byId.get("api_response")?.sourceMethodClass).not.toBe(byId.get("transcript")?.sourceMethodClass);
  });

  it("status column reflects the payload's own status, alarm-classed only for error", async () => {
    const { eventStore, selectStreamRows } = await freshEnv();
    eventStore.ingestFact(actionSpan(1000, 2000, { status: "error" }));
    eventStore.ingestFact(actionSpan(3000, 4000, { status: "ok" }));
    eventStore.flush();

    const { rows } = selectStreamRows(eventStore.rev, new Set());
    const errorRow = rows.find((r) => r.status === "error");
    const okRow = rows.find((r) => r.status === "ok");
    expect(errorRow?.statusClass).toBe("status-alarm");
    expect(okRow?.statusClass).toBe("status-neutral");
  });

  describe("facts column: display formatting only, no derived quantities", () => {
    it("energy_sample: uses total_j directly when present, never a Σcomponents", async () => {
      const { eventStore, selectStreamRows } = await freshEnv();
      // total_j deliberately does NOT equal the sum of components — if the
      // facts string used a client-computed sum instead of the server's
      // total_j, this assertion would catch it. Values kept below 10 J so
      // fmtJoules's 2-decimal tier makes the exact source unambiguous (at
      // >=10J whole-joule rounding could coincidentally match either).
      eventStore.ingestFact(energySample(0, 2000, { total_j: 1.23, components: [{ kind: "cpu", energy_j: 0.5, method: "rapl" }] }));
      eventStore.flush();

      const { rows } = selectStreamRows(eventStore.rev, new Set());
      expect(rows[0].facts).toContain("1.23 J"); // the server-supplied total_j, formatted
      expect(rows[0].facts).toContain("0.50 J"); // the one component's own value, formatted individually
      expect(rows[0].facts).not.toMatch(/\bsum\b/i);
    });

    it("energy_sample: renders '…' for the total when the payload carries no total_j — never fabricates one", async () => {
      const { eventStore, selectStreamRows } = await freshEnv();
      eventStore.ingestFact(energySample(0, 2000, { components: [{ kind: "cpu", energy_j: 8.5, method: "rapl" }, { kind: "dram", energy_j: 1.2, method: "rapl" }] }));
      eventStore.flush();

      const { rows } = selectStreamRows(eventStore.rev, new Set());
      expect(rows[0].facts).toContain("total …");
      // Each component's own value is still shown; their sum (9.7) never
      // appears anywhere as a labelled total.
      expect(rows[0].facts).toContain("8.50 J");
      expect(rows[0].facts).toContain("1.20 J");
      expect(rows[0].facts).not.toContain("9.7");
    });

    it("llm_call: shows token counts individually, never a computed total", async () => {
      const { eventStore, selectStreamRows } = await freshEnv();
      eventStore.ingestFact(llmCall(0, { usage: { input_tokens: 850, output_tokens: 200 } }));
      eventStore.flush();

      const { rows } = selectStreamRows(eventStore.rev, new Set());
      expect(rows[0].facts).toContain("in 850");
      expect(rows[0].facts).toContain("out 200");
      expect(rows[0].facts).not.toContain("1,050"); // the (never computed) sum
      expect(rows[0].facts).not.toContain("1.05k");
    });

    it("action_span: tool + duration + locus", async () => {
      const { eventStore, selectStreamRows } = await freshEnv();
      eventStore.ingestFact(actionSpan(0, 9200, { tool_name: "Bash(cargo test)", tool_kind: "bash", execution_locus: "local" }));
      eventStore.flush();

      const { rows } = selectStreamRows(eventStore.rev, new Set());
      expect(rows[0].facts).toBe("Bash(cargo test) · 9.20s · bash/local");
    });

    it("process_sample: a plain process count, not a summed cpu delta", async () => {
      const { eventStore, selectStreamRows } = await freshEnv();
      eventStore.ingestFact(processSample(0, 2000, 3));
      eventStore.flush();

      const { rows } = selectStreamRows(eventStore.rev, new Set());
      expect(rows[0].facts).toBe("3 processes");
    });

    it("session_meta: agent app name/version", async () => {
      const { eventStore, selectStreamRows } = await freshEnv();
      eventStore.ingestFact({
        schema_version: "0.1.0",
        event_id: nextId(),
        ts: iso(0),
        collector: COLLECTOR,
        session_id: "ses_test",
        type: "session_meta",
        payload: { agent_app: { name: "claude-code", version: "0.1.2" } },
      } as FactEvent);
      eventStore.flush();

      const { rows } = selectStreamRows(eventStore.rev, new Set());
      expect(rows[0].facts).toBe("claude-code 0.1.2");
    });
  });
});

describe("selectInspector", () => {
  it("returns null when nothing is selected", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    expect(selectInspector(eventStore.rev, null)).toBeNull();
  });

  it("returns null for an id not present in the store (e.g. evicted or unknown)", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    eventStore.ingestFact(llmCall(0));
    eventStore.flush();
    expect(selectInspector(eventStore.rev, "not_a_real_id")).toBeNull();
  });

  it("builds eyebrow/title/sub/rows/rawJson for the selected record, and carries `kind` for later share-bar work", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    const call = llmCall(0, { model_id_requested: "claude-sonnet-4-5-20250929" });
    eventStore.ingestFact(call);
    eventStore.flush();

    const model = selectInspector(eventStore.rev, call.event_id);
    expect(model).not.toBeNull();
    expect(model?.kind).toBe("llm_call");
    expect(model?.eyebrow).toBe("llm_call");
    expect(model?.title).toBe("claude-sonnet-4-5-20250929");
    expect(model?.rows.some((r) => r.key === "input_tokens")).toBe(true);
    expect(JSON.parse(model?.rawJson ?? "null")).toMatchObject({ event_id: call.event_id, type: "llm_call" });
  });

  it("flags transcript/estimated usage_source rows as alarm tone", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    const call = llmCall(0, { usage_source: "transcript" });
    eventStore.ingestFact(call);
    eventStore.flush();

    const model = selectInspector(eventStore.rev, call.event_id);
    const row = model?.rows.find((r) => r.key === "usage_source");
    expect(row?.tone).toBe("alarm");
  });

  it("flags a remote action_span's execution_locus as modelled tone, not alarm", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    const span = actionSpan(0, 1000, { execution_locus: "remote" });
    eventStore.ingestFact(span);
    eventStore.flush();

    const model = selectInspector(eventStore.rev, span.event_id);
    const row = model?.rows.find((r) => r.key === "execution_locus");
    expect(row?.tone).toBe("modelled");
  });

  it("memoises on (rev, selectedId): same args yield the same object reference", async () => {
    const { eventStore, selectInspector } = await freshEnv();
    const call = llmCall(0);
    eventStore.ingestFact(call);
    eventStore.flush();

    const first = selectInspector(eventStore.rev, call.event_id);
    const second = selectInspector(eventStore.rev, call.event_id);
    expect(second).toBe(first);

    const third = selectInspector(eventStore.rev, null);
    expect(third).not.toBe(first);
  });
});

describe("selectCorrelated", () => {
  it("includes only events within ±6s, excludes the selection, sorted by |offset|, with signed offset labels", async () => {
    const { eventStore, selectCorrelated } = await freshEnv();
    const selected = llmCall(10_000);
    eventStore.ingestFact(selected);
    const near = llmCall(10_400); // +0.4s
    eventStore.ingestFact(near);
    const before = actionSpan(6_000, 7_900); // ts = t_end = 7900 -> -2.1s
    eventStore.ingestFact(before);
    const boundary = energySample(3_900, 4_000); // ts = 4000 -> -6.0s, inside the window (<=6000)
    eventStore.ingestFact(boundary);
    const outside = llmCall(16_001); // +6.001s, just outside
    eventStore.ingestFact(outside);
    eventStore.flush();

    const rows = selectCorrelated(eventStore.rev, selected.event_id);
    expect(rows.map((r) => r.id)).toEqual([near.event_id, before.event_id, boundary.event_id]);
    expect(rows.find((r) => r.id === near.event_id)?.offsetLabel).toBe("+0.4s");
    expect(rows.find((r) => r.id === before.event_id)?.offsetLabel).toBe("−2.1s");
    expect(rows.some((r) => r.id === outside.event_id)).toBe(false);
    expect(rows.some((r) => r.id === selected.event_id)).toBe(false);
  });

  it("caps at 20", async () => {
    const { eventStore, selectCorrelated } = await freshEnv();
    const selected = llmCall(100_000);
    eventStore.ingestFact(selected);
    for (let i = 0; i < 25; i += 1) {
      eventStore.ingestFact(llmCall(100_000 + (i + 1) * 100)); // all within 2.5s, well inside ±6s
    }
    eventStore.flush();

    const rows = selectCorrelated(eventStore.rev, selected.event_id);
    expect(rows.length).toBe(20);
    // Sorted by |offset| ascending -> the nearest 20 of the 25 are kept.
    for (let i = 1; i < rows.length; i += 1) {
      expect(Math.abs(rows[i].offsetMs)).toBeGreaterThanOrEqual(Math.abs(rows[i - 1].offsetMs));
    }
  });

  it("returns empty for no selection or an unknown id", async () => {
    const { eventStore, selectCorrelated } = await freshEnv();
    eventStore.ingestFact(llmCall(0));
    eventStore.flush();
    expect(selectCorrelated(eventStore.rev, null)).toEqual([]);
    expect(selectCorrelated(eventStore.rev, "not_a_real_id")).toEqual([]);
  });
});
