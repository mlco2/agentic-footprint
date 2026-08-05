// Tests for console/src/lib/selectors/health.ts (Task 9 brief).
//
// Isolation note: `health.ts` imports the `eventStore` SINGLETON directly
// (its `selectHealthAside` reads `eventStore.watchdog`/`eventStore.rev`, the
// same pattern selectors/timeline.ts's `selectRail` uses), and its
// `memo1`-wrapped functions live in module-level closures. Every test gets a
// fully fresh module graph via `vi.resetModules()` + a dynamic re-import
// (same technique as timeline.test.ts/stream.test.ts/attribution.test.ts),
// so no test's ingested watchdog state or memo cache leaks into another's.
import { describe, expect, it, vi } from "vitest";
import type { CollectorHealth, ConformanceRow, HealthPayload, PythonDoctorRow, RejectFrame, SessionInfo, WatchdogEntry } from "../src/lib/types/debug";
import { fmtBytes, fmtClock, fmtCount, fmtCpuPct, fmtEventsPerS, fmtPct } from "../src/lib/format";

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

/** Fresh module graph per call: a brand-new `eventStore` singleton AND
 * brand-new `memo1` closures inside `selectors/health.ts`. */
async function freshEnv() {
  vi.resetModules();
  const eventStoreMod = await import("../src/lib/stores/eventStore.svelte");
  const health = await import("../src/lib/selectors/health");
  return { eventStore: eventStoreMod.eventStore, ...health };
}

// ---------------------------------------------------------------------------
// Fixtures — mirrors dev/fixtures/health.json / reject.json's exact field
// values where convenient, per this file's own provenance test below.
// ---------------------------------------------------------------------------

function collectorFixture(overrides: Partial<CollectorHealth> = {}): CollectorHealth {
  return {
    name: "claude-code",
    version: "0.1.2",
    transport: "jsonl spool",
    spool_file: "claude-code.01K9Y7QZ.jsonl",
    byte_offset: 2_517_428,
    events: 30,
    events_per_s: 0.14,
    rejected: 1,
    last_seen: "2026-07-25T09:41:42.881Z",
    emits: ["session_meta", "llm_call", "action_span"],
    ...overrides,
  };
}

function rejectFixture(overrides: Partial<RejectFrame> = {}): RejectFrame {
  return {
    ts: "2026-07-25T09:40:58.826Z",
    reason: "malformed JSON: unexpected end of input at byte offset 41822",
    origin: "claude-code.01K9Y7QZ.jsonl",
    line: 846,
    byte_offset: 41822,
    raw: '{"schema_version":"0.1.0","event_id":"01K9Y7QZ3F7Q5V8","type":"llm_call","ts":"2026-07-25T09:40:58',
    ...overrides,
  };
}

function healthFixture(overrides: Partial<HealthPayload> = {}): HealthPayload {
  return {
    collectors: [collectorFixture()],
    otlp_receiver: { endpoint: "127.0.0.1:4318", protocol: "http/json", logs_accepted: 11, metrics_discarded: 42 },
    // `conformance` intentionally omitted by default — the mock's/real
    // server's actual case (docs/design-log.md: "deliberately absent").
    rejected: [rejectFixture()],
    python: [
      { key: "ecologits", value: "0.7.1 · hash-locked", status: "ok" },
      { key: "codecarbon", value: "3.0.4", status: "ok" },
      { key: "venv", value: "~/.local/state/agentic-footprint/venv", status: "ok" },
    ],
    ...overrides,
  };
}

function sessionFixture(overrides: Partial<SessionInfo> = {}): SessionInfo {
  return {
    session_id: "ses_test",
    session_meta: { agent_app: { name: "claude-code" } } as unknown as SessionInfo["session_meta"],
    t_start: "2026-07-25T09:40:12.004Z",
    attribution_policy: "l2_cpu_time",
    methodology: { version: "v2026.06.1", source: "bundled" },
    grid: { zone: "FRA", g_co2e_per_kwh: 56, source: "codecarbon data v2026.06" },
    state_dir: "~/.local/state/agentic-footprint",
    schema_version: "0.1.0",
    mode: "watch --debug",
    ...overrides,
  };
}

function watchdogEntry(overrides: Partial<WatchdogEntry> = {}): WatchdogEntry {
  return { pid: 4242, span_id: "spn_1", cmd: "cargo test", cpu_pct: 12.4, rss_bytes: 40_000_000, state: "open", ...overrides };
}

// ---------------------------------------------------------------------------
// selectCollectorTable — idle clamp + dot thresholds
// ---------------------------------------------------------------------------

describe("selectCollectorTable", () => {
  it("clamps idle at 0 when last_seen is ahead of nowMs (end-stamped health snapshot vs. client clock)", async () => {
    const { selectCollectorTable } = await freshEnv();
    const nowMs = 1_000_000;
    // 100s "in the future" relative to the client clock — a naive
    // `Math.abs(nowMs - lastSeenMs)` would misread this as 100s idle
    // (magenta); the spec requires it read as 0 idle (cyan) instead.
    const health = healthFixture({ collectors: [collectorFixture({ last_seen: iso(nowMs + 100_000) })] });
    const [row] = selectCollectorTable(health, nowMs);
    expect(row.idleMs).toBe(0);
    expect(row.dotClass).toBe("dot-accent");
  });

  it.each([
    [11_900, "dot-accent"],
    [12_100, "dot-neutral"],
    [44_900, "dot-neutral"],
    [45_100, "dot-alarm"],
  ] as const)("dot threshold at idle=%ims resolves to %s", async (idleMs, expected) => {
    const { selectCollectorTable } = await freshEnv();
    const nowMs = 1_000_000;
    const health = healthFixture({ collectors: [collectorFixture({ last_seen: iso(nowMs - idleMs) })] });
    const [row] = selectCollectorTable(health, nowMs);
    expect(row.dotClass).toBe(expected);
    expect(row.idleMs).toBe(idleMs);
  });

  it("returns [] before the first health payload arrives", async () => {
    const { selectCollectorTable } = await freshEnv();
    expect(selectCollectorTable(undefined, 1_000_000)).toEqual([]);
  });

  it("carries version/transport/emits/rate/rejected verbatim from the payload, through format.ts where numeric", async () => {
    const { selectCollectorTable } = await freshEnv();
    const nowMs = 1_000_000;
    const health = healthFixture();
    const [row] = selectCollectorTable(health, nowMs);
    expect(row.version).toBe("0.1.2");
    expect(row.transport).toBe("jsonl spool");
    expect(row.emitsLabel).toBe("session_meta, llm_call, action_span");
    expect(row.rateLabel).toBe(fmtEventsPerS(0.14));
    expect(row.rejected).toBe(1);
    expect(row.lastSeenLabel).toBe(fmtClock(Date.parse("2026-07-25T09:41:42.881Z")));
  });

  it("renders '—' for the real server's null events_per_s, never a fabricated rate (tolerance)", async () => {
    const { selectCollectorTable } = await freshEnv();
    const health = healthFixture({ collectors: [collectorFixture({ events_per_s: null })] });
    const [row] = selectCollectorTable(health, 1_000_000);
    expect(row.rateLabel).toBe("—");
  });

  it("is memoisation-stable across identical (health, nowMs) references", async () => {
    const { selectCollectorTable } = await freshEnv();
    const health = healthFixture();
    const a = selectCollectorTable(health, 1_000_000);
    const b = selectCollectorTable(health, 1_000_000);
    expect(b).toBe(a);
    const c = selectCollectorTable(health, 1_000_001);
    expect(c).not.toBe(a);
  });
});

// ---------------------------------------------------------------------------
// selectConformance — gap #9, DEFERRED BY DECISION: both branches
// ---------------------------------------------------------------------------

describe("selectConformance", () => {
  it("renders the pending panel when health is undefined", async () => {
    const { selectConformance } = await freshEnv();
    expect(selectConformance(undefined)).toEqual({ kind: "pending" });
  });

  it("renders the pending panel when health.conformance is absent — never an empty bars table", async () => {
    const { selectConformance } = await freshEnv();
    const health = healthFixture(); // conformance omitted
    expect("conformance" in health).toBe(false);
    expect(selectConformance(health)).toEqual({ kind: "pending" });
  });

  it("renders bars with the correct color class at 91% / 75% / 40% (cyan >90, neutral 60-90, magenta below)", async () => {
    const { selectConformance } = await freshEnv();
    const rows: ConformanceRow[] = [
      { field: "action_span.pids[]", present: 91, total: 100, note: "remote spans never carry pids — expected to be low" },
      { field: "llm_call.usage.thought_tokens", present: 75, total: 100, note: "only reasoning-capable models report this" },
      { field: "energy_sample.components[].label", present: 40, total: 100, note: "best-effort hardware naming" },
    ];
    const health = healthFixture({ conformance: rows });
    const model = selectConformance(health);
    expect(model.kind).toBe("bars");
    if (model.kind !== "bars") throw new Error("unreachable");
    expect(model.rows[0]).toMatchObject({ field: "action_span.pids[]", pctLabel: "91%", fractionLabel: "91/100", colorClass: "dot-accent" });
    expect(model.rows[1]).toMatchObject({ pctLabel: "75%", fractionLabel: "75/100", colorClass: "dot-neutral" });
    expect(model.rows[2]).toMatchObject({ pctLabel: "40%", fractionLabel: "40/100", colorClass: "dot-alarm" });
    // The note is carried through verbatim — SCREENS.md: "several are
    // expected to be low and that must be said inline".
    expect(model.rows[2].note).toBe("best-effort hardware naming");
  });

  it("is memoisation-stable across identical health references", async () => {
    const { selectConformance } = await freshEnv();
    const health = healthFixture({ conformance: [{ field: "x", present: 1, total: 2 }] });
    const a = selectConformance(health);
    const b = selectConformance(health);
    expect(b).toBe(a);
  });

  // Exact-boundary cases at the two cut points SCREENS.md §5 names ("4px bar
  // (cyan > 90%, neutral 60–90%, magenta below)") and `conformanceColorClass`
  // (health.ts) actually implements as `pct > 90` / `pct >= 60`. SCREENS.md's
  // own prose ("neutral 60–90%") reads as if 90% itself could go either way;
  // the IMPLEMENTED semantics win here (brief: "verify against the
  // implemented `>90` cut ... the implemented semantics win, assert them") —
  // 90.0% is strictly NOT `> 90`, so it renders neutral, not accent.
  it("renders exactly 90.0% as neutral, not accent — the implemented cut is `pct > 90`, not `pct >= 90`", async () => {
    const { selectConformance } = await freshEnv();
    const health = healthFixture({ conformance: [{ field: "x", present: 90, total: 100 }] });
    const model = selectConformance(health);
    if (model.kind !== "bars") throw new Error("unreachable");
    expect(model.rows[0].pctLabel).toBe("90%");
    expect(model.rows[0].colorClass).toBe("dot-neutral");
  });

  it("renders exactly 60.0% as neutral (the lower cut is inclusive: `pct >= 60`), not alarm", async () => {
    const { selectConformance } = await freshEnv();
    const health = healthFixture({ conformance: [{ field: "x", present: 60, total: 100 }] });
    const model = selectConformance(health);
    if (model.kind !== "bars") throw new Error("unreachable");
    expect(model.rows[0].pctLabel).toBe("60%");
    expect(model.rows[0].colorClass).toBe("dot-neutral");
  });
});

// ---------------------------------------------------------------------------
// selectRejected — byte_offset + raw verbatim
// ---------------------------------------------------------------------------

describe("selectRejected", () => {
  it("returns [] when health is undefined", async () => {
    const { selectRejected } = await freshEnv();
    expect(selectRejected(undefined)).toEqual([]);
  });

  it("carries byte_offset and raw verbatim, plus reason/origin/line", async () => {
    const { selectRejected } = await freshEnv();
    const health = healthFixture();
    const [row] = selectRejected(health);
    expect(row.raw).toBe('{"schema_version":"0.1.0","event_id":"01K9Y7QZ3F7Q5V8","type":"llm_call","ts":"2026-07-25T09:40:58');
    expect(row.byteOffsetLabel).toBe(fmtCount(41822));
    expect(row.byteOffsetLabel).toBe("41,822");
    expect(row.lineLabel).toBe("846");
    expect(row.reason).toBe("malformed JSON: unexpected end of input at byte offset 41822");
    expect(row.origin).toBe("claude-code.01K9Y7QZ.jsonl");
  });

  it("is memoisation-stable across identical health references", async () => {
    const { selectRejected } = await freshEnv();
    const health = healthFixture();
    const a = selectRejected(health);
    const b = selectRejected(health);
    expect(b).toBe(a);
    const c = selectRejected(healthFixture({ rejected: [] }));
    expect(c).not.toBe(a);
  });
});

// ---------------------------------------------------------------------------
// selectHealthAside — ingestion KVs (provenance), watchdog reuse, doctor dots
// ---------------------------------------------------------------------------

describe("selectHealthAside", () => {
  it("ingestion KVs equal the fixture's own strings verbatim (provenance)", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    const session = sessionFixture({ state_dir: "/tmp/af-state" });
    const health = healthFixture({
      collectors: [collectorFixture({ name: "claude-code", spool_file: "claude-code.01K9Y7QZ.jsonl", byte_offset: 2_517_428 })],
      otlp_receiver: { endpoint: "127.0.0.1:4318", protocol: "http/json", logs_accepted: 11, metrics_discarded: 42 },
    });
    const aside = selectHealthAside(health, eventStore.rev, session);

    const spoolRow = aside.ingestion.find((r) => r.label === "claude-code spool");
    expect(spoolRow?.value).toBe(`/tmp/af-state/spool/claude-code.01K9Y7QZ.jsonl · byte ${fmtCount(2_517_428)}`);

    const endpointRow = aside.ingestion.find((r) => r.label === "otlp endpoint");
    expect(endpointRow?.value).toBe("127.0.0.1:4318 · http/json");

    const acceptedRow = aside.ingestion.find((r) => r.label === "otlp logs accepted");
    expect(acceptedRow?.value).toBe(fmtCount(11));
    const discardedRow = aside.ingestion.find((r) => r.label === "otlp metrics discarded");
    expect(discardedRow?.value).toBe(fmtCount(42));
  });

  it("renders a 'rejected total' row when the real server's rejected_total is present, honestly absent when it isn't (the mock's case)", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    const withoutTotal = selectHealthAside(healthFixture(), eventStore.rev, sessionFixture());
    expect(withoutTotal.ingestion.find((r) => r.label === "rejected total")).toBeUndefined();

    const withTotal = selectHealthAside(healthFixture({ rejected_total: 7 }), eventStore.rev, sessionFixture());
    const row = withTotal.ingestion.find((r) => r.label === "rejected total");
    expect(row?.value).toBe(fmtCount(7));
  });

  it("skips a collector with no spool_file (e.g. an HTTP-fed collector) rather than fabricating a path", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    const health = healthFixture({ collectors: [collectorFixture({ name: "otlp-cc", spool_file: undefined, byte_offset: undefined, transport: "POST /v1/logs" })] });
    const aside = selectHealthAside(health, eventStore.rev, sessionFixture());
    expect(aside.ingestion.find((r) => r.label === "otlp-cc spool")).toBeUndefined();
  });

  it("renders the real server's --no-otlp note when otlp_receiver.endpoint is null (tolerance)", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    const health = healthFixture({
      otlp_receiver: { endpoint: null, protocol: "http/json", logs_accepted: 0, metrics_discarded: 0, note: "no OTLP receiver is running in this af watch process" },
    });
    const aside = selectHealthAside(health, eventStore.rev, sessionFixture());
    const endpointRow = aside.ingestion.find((r) => r.label === "otlp endpoint");
    expect(endpointRow?.value).toBe("no OTLP receiver is running in this af watch process");
  });

  it("reuses the Timeline rail's WatchdogRailRow shape/formatting for eventStore.watchdog", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    eventStore.replaceWatchdog([watchdogEntry({ pid: 777, cmd: "pytest -q", cpu_pct: 8.25, rss_bytes: 12_582_912, state: "open" })]);
    eventStore.flush();
    const aside = selectHealthAside(healthFixture(), eventStore.rev, sessionFixture());
    expect(aside.watchdog).toEqual([{ pid: 777, cmd: "pytest -q", cpuPctLabel: fmtCpuPct(8.25), rssLabel: fmtBytes(12_582_912), state: "open" }]);
  });

  it("builds an orphan summary line iff a watchdog entry is orphaned, matching Timeline's own wording", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    eventStore.replaceWatchdog([watchdogEntry({ pid: 99, span_id: "spn_orphan", state: "orphaned", outlived_span_by_ms: 4200 })]);
    eventStore.flush();
    const aside = selectHealthAside(healthFixture(), eventStore.rev, sessionFixture());
    expect(aside.orphanSummary).toBe("pid 99 outlived spn_orphan by 4.20s");
  });

  it("orphanSummary is null when nothing is orphaned", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    eventStore.replaceWatchdog([watchdogEntry({ state: "open" })]);
    eventStore.flush();
    const aside = selectHealthAside(healthFixture(), eventStore.rev, sessionFixture());
    expect(aside.orphanSummary).toBeNull();
  });

  it.each([
    ["ok", "dot-accent"],
    ["warn", "dot-neutral"],
    ["error", "dot-alarm"],
    ["unknown-status", "dot-neutral"],
  ] as const)("python doctor status %s maps to %s", async (status, expected) => {
    const { selectHealthAside, eventStore } = await freshEnv();
    const row: PythonDoctorRow = { key: "venv", value: "healthy", status };
    const health = healthFixture({ python: [row] });
    const aside = selectHealthAside(health, eventStore.rev, sessionFixture());
    expect(aside.doctor).toEqual([{ key: "venv", value: "healthy", dotClass: expected }]);
  });

  it("returns [] ingestion/doctor rows before the first health payload arrives, without throwing", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    const aside = selectHealthAside(undefined, eventStore.rev, sessionFixture());
    expect(aside.ingestion).toEqual([]);
    expect(aside.doctor).toEqual([]);
  });

  it("is memoisation-stable across identical (health, rev, session) references", async () => {
    const { selectHealthAside, eventStore } = await freshEnv();
    const health = healthFixture();
    const session = sessionFixture();
    const a = selectHealthAside(health, eventStore.rev, session);
    const b = selectHealthAside(health, eventStore.rev, session);
    expect(b).toBe(a);

    eventStore.replaceWatchdog([watchdogEntry()]);
    eventStore.flush();
    const c = selectHealthAside(health, eventStore.rev, session);
    expect(c).not.toBe(a);
  });
});

// ---------------------------------------------------------------------------
// selectConformance's percentage formatter must route through format.ts
// (fmtPct) — sanity check the exact test-fixture math lines up with the
// shared formatter rather than a private re-implementation.
// ---------------------------------------------------------------------------

describe("selectConformance formatting sanity", () => {
  it("pctLabel is exactly fmtPct(present/total)", async () => {
    const { selectConformance } = await freshEnv();
    const health = healthFixture({ conformance: [{ field: "x", present: 21, total: 27 }] });
    const model = selectConformance(health);
    if (model.kind !== "bars") throw new Error("unreachable");
    expect(model.rows[0].pctLabel).toBe(fmtPct(21 / 27));
  });
});
