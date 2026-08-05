// Task 4b — real-server tolerance contract tests (docs/design-log.md, "af
// watch resident mode, sampler lifecycle, and the `/debug` console
// surface"). The real `af watch --debug` server deviates from the mock's
// rich fixtures in specific, deliberate ways: several fields the mock
// always fills in are `null`/omitted on the real server instead. These are
// hand-built, null-bearing fixtures pushed straight through the
// health/session/report/alloc-consuming code paths (the stores, plus the
// format.ts renderers those values are meant for) — never by nulling out
// dev/scenario.ts's fixtures, which stay rich on purpose (brief: "Keep the
// mock contract-faithful and rich; tolerance is proven with hand-built
// null-bearing unit fixtures, not by nulling the mock").
//
// Every test here asserts two things: no NaN/exception reaching the
// surface, and an honest rendering string (never a fabricated number,
// never a silent 0) for the fields the real server is documented to send
// as null/absent.
import { describe, expect, it, vi } from "vitest";
import { SessionStore } from "../src/lib/stores/sessionStore.svelte";
import { HealthStore } from "../src/lib/stores/healthStore.svelte";
import { ReportStore } from "../src/lib/stores/reportStore.svelte";
import { AllocStore } from "../src/lib/stores/allocStore.svelte";
import { fmtEventsPerS, fmtGridIntensity } from "../src/lib/format";
import type { AllocationTrace, CollectorHealth, DebugReport, HealthPayload, SessionInfo } from "../src/lib/types/debug";

// --- SessionStore / SessionInfo -------------------------------------------

describe("SessionInfo tolerance — real server's pre-estimate placeholders", () => {
  // Hand-built per docs/design-log.md's exact real-server values: grid
  // factor null (no estimator sidecar), methodology version literally
  // "unknown until the first estimate", ecologits_version/codecarbon_version
  // omitted rather than guessed.
  const REAL_SERVER_SESSION: SessionInfo = {
    session_id: "ses_real",
    session_meta: { agent_app: { name: "claude-code" } },
    t_start: "2026-07-25T09:40:12.004Z",
    attribution_policy: "l2_cpu_time",
    methodology: { version: "unknown until the first estimate", source: "bundled" },
    grid: { zone: "FRA", g_co2e_per_kwh: null, source: "unavailable — no estimator sidecar or unknown zone" },
    state_dir: "~/.local/state/agentic-footprint",
    schema_version: "0.1.0",
    mode: "watch --debug",
  };

  it("SessionStore.set() accepts a null grid factor and omitted methodology versions without throwing", () => {
    const store = new SessionStore();
    expect(() => store.set(REAL_SERVER_SESSION)).not.toThrow();
    expect(store.data).toEqual(REAL_SERVER_SESSION);
    expect(store.data?.methodology.ecologits_version).toBeUndefined();
    expect(store.data?.methodology.codecarbon_version).toBeUndefined();
  });

  it("fmtGridIntensity renders the honest 'n/a · <source>' string for this exact fixture, never 0", () => {
    const rendered = fmtGridIntensity(REAL_SERVER_SESSION.grid.g_co2e_per_kwh, REAL_SERVER_SESSION.grid.source);
    expect(rendered).toBe("n/a · unavailable — no estimator sidecar or unknown zone");
    expect(rendered).not.toContain("0 gCO2e");
  });
});

// --- HealthStore / CollectorHealth -----------------------------------------

describe("HealthPayload tolerance — real server's null events_per_s and absent conformance", () => {
  const nullRateCollector: CollectorHealth = {
    name: "claude-code",
    version: "0.1.2",
    transport: "jsonl spool",
    spool_file: "claude-code.01K9Y7QZ.jsonl",
    byte_offset: 2517428,
    events: 30,
    events_per_s: null,
    rejected: 1,
    last_seen: "2026-07-25T09:41:42.881Z",
    emits: ["session_meta", "llm_call", "action_span"],
  };

  const REAL_SERVER_HEALTH: HealthPayload = {
    collectors: [nullRateCollector],
    otlp_receiver: { endpoint: "127.0.0.1:4318", protocol: "http/json", logs_accepted: 0, metrics_discarded: 0 },
    // `conformance` intentionally absent — gap #9 was declined.
    rejected: [],
    python: [{ key: "venv", value: "healthy", status: "ok" }],
  };

  it("HealthStore.set() accepts a null events_per_s and a missing conformance key without throwing", () => {
    const store = new HealthStore();
    expect(() => store.set(REAL_SERVER_HEALTH)).not.toThrow();
    expect(store.data).toEqual(REAL_SERVER_HEALTH);
    expect("conformance" in (store.data as HealthPayload)).toBe(false);
  });

  it("fmtEventsPerS renders '—' for the null rate, never '0/s' or NaN", () => {
    const rendered = fmtEventsPerS(nullRateCollector.events_per_s);
    expect(rendered).toBe("—");
    expect(rendered).not.toContain("NaN");
  });

  // Found during this task's own e2e run (not in the brief's verified-facts
  // list, but a real, reachable state, not a corner case unique to the test
  // rig): `af watch --debug --no-otlp` — a normal, user-facing flag
  // combination (`--otlp-addr`/`--no-otlp` are a real choice, docs/design-log.md)
  // — serves `otlp_receiver.endpoint: null` plus a `note` explaining why.
  it("HealthStore.set() accepts a null otlp_receiver.endpoint (--no-otlp) with its explanatory note, without throwing", () => {
    const noOtlpHealth: HealthPayload = {
      collectors: [],
      otlp_receiver: { endpoint: null, protocol: "http/json", logs_accepted: 0, metrics_discarded: 0, note: "no OTLP receiver is running in this af watch process" },
      rejected: [],
      python: [],
    };
    const store = new HealthStore();
    expect(() => store.set(noOtlpHealth)).not.toThrow();
    expect(store.data?.otlp_receiver.endpoint).toBeNull();
    expect(store.data?.otlp_receiver.note).toContain("no OTLP receiver");
  });
});

// --- ReportStore / DebugReport ---------------------------------------------

describe("DebugReport tolerance — real server ignores ?level= and adds a sixth histogram status", () => {
  const SESSION_LEVEL_PAYLOAD: DebugReport = {
    level: "session", // the real server always answers at session level…
    impact_join: { unit: { level: "session", session_id: "ses_real" }, t_start: "2026-07-25T09:40:12.004Z", t_end: "2026-07-25T09:45:12.004Z", attribution_policy: "l2_cpu_time" },
    by_model: [],
    estimation_status_histogram: { ok: 2, unknown_model: 0, missing_zone: 0, pending: 1, error: 0, missing_usage: 1 },
  };

  it("reportStore.set() (the SSE push path) keys by the payload's own level and tolerates the missing_usage key", () => {
    const store = new ReportStore();
    expect(() => store.set(SESSION_LEVEL_PAYLOAD)).not.toThrow();
    expect(store.get("session")).toEqual(SESSION_LEVEL_PAYLOAD);
    expect(store.get("session")?.estimation_status_histogram.missing_usage).toBe(1);
  });

  it("reportStore.fetchLevel('task') against a server that ignores ?level= caches under the REQUESTED key, not the payload's own (different) level", async () => {
    // …even when a caller asks for "task" — DATA-CONTRACT §2.6 gap:
    // "?level= is ignored (session only)" (docs/design-log.md).
    const fetchImpl = vi.fn(async () => ({ ok: true, json: async () => SESSION_LEVEL_PAYLOAD }) as unknown as Response);
    const store = new ReportStore(fetchImpl as unknown as typeof fetch);

    const result = await store.fetchLevel("task");

    expect(fetchImpl).toHaveBeenCalledWith("/debug/report?level=task");
    // Cached under "task" — the slot the caller actually asked for...
    expect(store.get("task")).toEqual(SESSION_LEVEL_PAYLOAD);
    // ...but the stored object's own `.level` field is left exactly as the
    // server reported it, never rewritten to claim a task-level computation
    // that never happened.
    expect(store.get("task")?.level).toBe("session");
    expect(result.level).toBe("session");
    // And the "session" slot itself was never touched by this call.
    expect(store.get("session")).toBeUndefined();
  });
});

// --- AllocStore / AllocationTrace -------------------------------------------

describe("AllocationTrace tolerance — real server's extra denominator_note / agent_process.note", () => {
  const REAL_SERVER_TRACE: AllocationTrace = {
    sample_event_id: "evt_real_sample",
    t_start: "2026-07-25T09:40:12.004Z",
    t_end: "2026-07-25T09:40:17.004Z",
    total_j: 42,
    components: [{ kind: "cpu", energy_j: 42, method: "rapl" }],
    attribution_policy: "l2_cpu_time",
    denominator_cpu_ms: 5000,
    denominator_note:
      "wall-clock ms of the window: l2_cpu_time/v1 normalizes cpu-time against one core-second per second, never against the sum of the watched trees",
    rows: [],
    agent_process: {
      pid: 4242,
      cpu_delta_ms: 120,
      allocated_j: 1.2,
      note: "orphaned/unclaimed compute: l2_cpu_time/v1 has no separate agent-process bucket, so this is observed cpu that no span claimed (including the agent's own tree while no span was running, and any orphan tail)",
    },
    baseline: { allocated_j: 40.8, share: 0.971, label: "baseline/idle" },
    l1_shadow_sum_share: 0,
  };

  it("AllocStore.ingest() accepts the extra note fields without throwing, and preserves them verbatim", () => {
    const store = new AllocStore();
    expect(() => store.ingest(REAL_SERVER_TRACE)).not.toThrow();
    const entry = store.get("evt_real_sample");
    expect(entry).toEqual({ status: "ready", trace: REAL_SERVER_TRACE });
    // The note is exactly what a later Attribution-tab row would render as
    // its secondary line — never re-derived or reworded client-side
    // (global-constraints.md #1: the client computes nothing).
    expect(entry?.status).toBe("ready");
    if (entry?.status === "ready") {
      expect(entry.trace.agent_process.note).toContain("no separate agent-process bucket");
      expect(entry.trace.denominator_note).toContain("normalizes cpu-time");
    }
  });

  it("a mock-shaped trace with NO note fields (dev/scenario.ts's rich fixtures never set them) still ingests cleanly — both shapes are valid", () => {
    const store = new AllocStore();
    const mockShaped: AllocationTrace = { ...REAL_SERVER_TRACE, denominator_note: undefined, agent_process: { pid: 1, cpu_delta_ms: 10, allocated_j: 0.1 } };
    expect(() => store.ingest(mockShaped)).not.toThrow();
    expect(store.get("evt_real_sample")).toEqual({ status: "ready", trace: mockShaped });
  });
});
