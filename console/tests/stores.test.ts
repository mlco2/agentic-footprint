// Item 6 (untested paths): dedicated unit coverage for the three "thin
// replace-on-arrival" stores — sessionStore, reportStore, healthStore —
// which previously had no tests of their own (reportStore's requested-vs-
// payload keying and healthStore's null-tolerance were only exercised
// indirectly, via tests/tolerance.test.ts's real-server fixtures, and
// nowhere at all for plain replace-on-arrival semantics). No real network —
// reportStore's fetch path uses a mock `fetch`.
import { describe, expect, it, vi } from "vitest";
import { SessionStore } from "../src/lib/stores/sessionStore.svelte";
import { ReportStore } from "../src/lib/stores/reportStore.svelte";
import { HealthStore } from "../src/lib/stores/healthStore.svelte";
import type { DebugReport, HealthPayload, SessionInfo } from "../src/lib/types/debug";

function sessionInfo(overrides: Partial<SessionInfo> = {}): SessionInfo {
  return {
    session_id: "ses_a",
    session_meta: { agent_app: { name: "claude-code" } },
    t_start: "2026-07-25T00:00:00.000Z",
    attribution_policy: "l2_cpu_time",
    methodology: { version: "v1", source: "bundled" },
    grid: { zone: "FRA", g_co2e_per_kwh: 56, source: "test" },
    state_dir: "~/.local/state/agentic-footprint",
    schema_version: "0.1.0",
    mode: "watch --debug",
    ...overrides,
  };
}

function report(level: DebugReport["level"], overrides: Partial<DebugReport> = {}): DebugReport {
  return {
    level,
    impact_join: { unit: { level: level === "tool" ? "tool_call" : level, session_id: "ses_a" }, t_start: "2026-07-25T00:00:00.000Z", t_end: "2026-07-25T00:01:00.000Z", attribution_policy: "l2_cpu_time" },
    by_model: [],
    estimation_status_histogram: { ok: 0, unknown_model: 0, missing_zone: 0, pending: 0, error: 0 },
    ...overrides,
  };
}

function health(overrides: Partial<HealthPayload> = {}): HealthPayload {
  return {
    collectors: [],
    otlp_receiver: { endpoint: "127.0.0.1:4318", protocol: "http/json", logs_accepted: 0, metrics_discarded: 0 },
    rejected: [],
    python: [],
    ...overrides,
  };
}

function fakeJsonResponse(body: unknown): Response {
  return { ok: true, status: 200, json: async () => body } as unknown as Response;
}

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

describe("SessionStore — replace-on-arrival", () => {
  it("starts null and set() fully replaces, never merges, the previous value", () => {
    const store = new SessionStore();
    expect(store.data).toBeNull();

    const a = sessionInfo({ session_id: "ses_a", mode: "watch --debug" });
    store.set(a);
    expect(store.data).toEqual(a);

    // A second, differently-shaped session (e.g. a real reconnect after a
    // server restart) must wholly replace the first — no field from `a`
    // should leak into the result even though both share most field names.
    const b = sessionInfo({ session_id: "ses_b", mode: "watch --debug --no-otlp", grid: { zone: "USE", g_co2e_per_kwh: null, source: "unavailable" } });
    store.set(b);
    expect(store.data).toEqual(b);
    expect(store.data?.session_id).toBe("ses_b");
    expect(store.data?.grid.g_co2e_per_kwh).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// ReportStore
// ---------------------------------------------------------------------------

describe("ReportStore — replace-on-arrival and requested-vs-payload level keying", () => {
  it("set() (the SSE push path) replaces the report at its own `.level`, leaving other levels untouched", () => {
    const store = new ReportStore();
    const sessionV1 = report("session", { estimation_status_histogram: { ok: 1 } });
    store.set(sessionV1);
    expect(store.get("session")).toEqual(sessionV1);
    expect(store.get("task")).toBeUndefined();

    // A second `report` SSE frame at the SAME level fully replaces the
    // first — not merged field-by-field.
    const sessionV2 = report("session", { estimation_status_histogram: { ok: 5, error: 1 } });
    store.set(sessionV2);
    expect(store.get("session")).toEqual(sessionV2);
    expect(store.get("session")).not.toEqual(sessionV1);

    // Pushing a report at a DIFFERENT level must not disturb the one
    // already cached at "session" — `levels` is keyed per level.
    const taskReport = report("task");
    store.set(taskReport);
    expect(store.get("task")).toEqual(taskReport);
    expect(store.get("session")).toEqual(sessionV2);
  });

  it("fetchLevel() caches under the REQUESTED level, not the payload's own `.level` — even when they differ", async () => {
    // The real `af watch --debug` server ignores `?level=` and always answers
    // at session level (DATA-CONTRACT §2.6 gap, docs/design-log.md) — so a
    // `fetchLevel("task")` caller must still find its result under "task",
    // or it would re-fetch forever, and `get("session")` must stay untouched
    // since nothing ever actually requested "session".
    const sessionShapedPayload = report("session");
    const fetchImpl = vi.fn(async (url: string) => {
      expect(url).toBe("/debug/report?level=task");
      return fakeJsonResponse(sessionShapedPayload);
    });
    const store = new ReportStore(fetchImpl as unknown as typeof fetch);

    const result = await store.fetchLevel("task");

    expect(store.get("task")).toEqual(sessionShapedPayload);
    expect(store.get("task")?.level, "the stored object's own .level is left exactly as the server sent it").toBe("session");
    expect(result.level).toBe("session");
    expect(store.get("session"), "the 'session' slot was never requested and must stay empty").toBeUndefined();
  });

  it("fetchLevel() dedupes concurrent callers for the same level into one fetch", async () => {
    const payload = report("session");
    const fetchImpl = vi.fn(async () => fakeJsonResponse(payload));
    const store = new ReportStore(fetchImpl as unknown as typeof fetch);

    const [a, b] = await Promise.all([store.fetchLevel("session"), store.fetchLevel("session")]);
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(a).toEqual(payload);
    expect(b).toEqual(payload);
  });
});

// ---------------------------------------------------------------------------
// HealthStore
// ---------------------------------------------------------------------------

describe("HealthStore — replace-on-arrival, preserving conformance === undefined", () => {
  it("starts null and set() fully replaces the previous value", () => {
    const store = new HealthStore();
    expect(store.data).toBeNull();

    const a = health({ collectors: [{ name: "claude-code", version: "0.1.2", transport: "jsonl spool", events: 10, events_per_s: 1, rejected: 0, last_seen: "2026-07-25T00:00:00.000Z", emits: ["session_meta"] }] });
    store.set(a);
    expect(store.data).toEqual(a);

    const b = health({ collectors: [] });
    store.set(b);
    expect(store.data).toEqual(b);
    expect(store.data?.collectors).toHaveLength(0);
  });

  it("preserves `conformance === undefined` through a round trip — never defaulted to []", () => {
    const store = new HealthStore();
    const payload = health(); // gap #9 declined: no `conformance` key at all
    store.set(payload);

    expect(store.data?.conformance).toBeUndefined();
    expect("conformance" in (store.data as HealthPayload)).toBe(false);
  });

  it("a later set() WITH conformance, followed by one WITHOUT it, ends at undefined again (full replace, not a sticky merge)", () => {
    const store = new HealthStore();
    store.set(health({ conformance: [{ field: "tool_name", present: 8, total: 10 }] }));
    expect(store.data?.conformance).toHaveLength(1);

    store.set(health()); // next payload declines conformance again
    expect(store.data?.conformance, "must not retain the previous payload's conformance array").toBeUndefined();
  });
});
