// AfClient tests (brief §"Tests"): a mock EventSource + mock fetch, asserting
// the strict bootstrap order, pause buffering semantics, dedup across the
// snapshot/stream boundary, and status transitions including backoff and
// `reset`. No real network, no real timers except where fake timers are
// used explicitly for the backoff assertions.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AfClient, type EventSourceLike, type VisibilityLike } from "../src/lib/client/afClient.svelte";
import { StatusStore } from "../src/lib/stores/statusStore.svelte";
import { EventStore } from "../src/lib/stores/eventStore.svelte";
import { AllocStore } from "../src/lib/stores/allocStore.svelte";
import { SessionStore } from "../src/lib/stores/sessionStore.svelte";
import { ReportStore } from "../src/lib/stores/reportStore.svelte";
import { HealthStore } from "../src/lib/stores/healthStore.svelte";
import { UiStore } from "../src/lib/stores/uiStore.svelte";
import type { FactEvent } from "../src/lib/types/contract1";
import type { AllocationTrace, DebugReport, HealthPayload, SessionInfo, Snapshot, WatcherStatus } from "../src/lib/types/debug";

// --- fake EventSource --------------------------------------------------

class FakeEventSource implements EventSourceLike {
  listeners = new Map<string, Array<(ev: { data: string; lastEventId?: string }) => void>>();
  closed = false;

  constructor(public url: string) {}

  addEventListener(type: string, listener: (ev: { data: string; lastEventId?: string }) => void): void {
    const arr = this.listeners.get(type) ?? [];
    arr.push(listener);
    this.listeners.set(type, arr);
  }

  close(): void {
    this.closed = true;
  }

  emit(type: string, data: unknown, seq?: number): void {
    const ev = { data: JSON.stringify(data), lastEventId: seq !== undefined ? String(seq) : undefined };
    for (const l of this.listeners.get(type) ?? []) l(ev);
  }

  emitError(): void {
    for (const l of this.listeners.get("error") ?? []) l({ data: "" });
  }

  /** The browser's `open` event: response headers arrived, stream is up. */
  emitOpen(): void {
    for (const l of this.listeners.get("open") ?? []) l({ data: "" });
  }
}

/** A controllable stand-in for the page's visibility: `hidden` is what
 * `document.hidden` would say, `wake()` is the tab coming back (or the
 * window regaining focus, or the network coming back). */
class FakeVisibility implements VisibilityLike {
  hidden = false;
  private callbacks: Array<() => void> = [];

  isHidden(): boolean {
    return this.hidden;
  }

  onWake(cb: () => void): void {
    this.callbacks.push(cb);
  }

  wake(): void {
    this.hidden = false;
    for (const cb of this.callbacks) cb();
  }
}

function watcherStatus(bootId: string): WatcherStatus {
  return {
    boot_id: bootId,
    started_at: "2026-07-25T00:00:00.000Z",
    af_version: "0.1.0",
    flush_interval_ms: 60000,
    receiver: { endpoint: "127.0.0.1:4318", note: null },
    agents: [],
  };
}

// --- fixtures ------------------------------------------------------------

const SESSION_INFO: SessionInfo = {
  session_id: "ses_test",
  session_meta: { agent_app: { name: "claude-code" } },
  t_start: "2026-07-25T00:00:00.000Z",
  attribution_policy: "l2_cpu_time",
  methodology: { version: "v1", source: "bundled" },
  grid: { zone: "FRA", g_co2e_per_kwh: 56, source: "test" },
  state_dir: "~/.local/state/agentic-footprint",
  schema_version: "0.1.0",
  mode: "watch --debug",
};

const HEALTH_INFO: HealthPayload = {
  collectors: [],
  otlp_receiver: { endpoint: "x", protocol: "http/json", logs_accepted: 0, metrics_discarded: 0 },
  rejected: [],
  python: [],
};

const REPORT_INFO: DebugReport = {
  level: "session",
  impact_join: { unit: { level: "session", session_id: "ses_test" }, t_start: "2026-07-25T00:00:00.000Z", t_end: "2026-07-25T00:01:00.000Z", attribution_policy: "l2_cpu_time" },
  by_model: [],
  estimation_status_histogram: { ok: 0, unknown_model: 0, missing_zone: 0, pending: 0, error: 0 },
};

function actionSpanFact(spanId: string, eventId: string, tStartMs: number, tEndMs: number): FactEvent {
  return {
    schema_version: "0.1.0",
    event_id: eventId,
    ts: new Date(tEndMs).toISOString(),
    collector: { name: "claude-code", version: "0.0.0" },
    session_id: "ses_test",
    type: "action_span",
    payload: {
      span_id: spanId,
      tool_name: `Tool(${spanId})`,
      tool_kind: "bash",
      execution_locus: "local",
      t_start: new Date(tStartMs).toISOString(),
      t_end: new Date(tEndMs).toISOString(),
      status: "ok",
    },
  } as FactEvent;
}

function allocTrace(sampleId: string): AllocationTrace {
  return {
    sample_event_id: sampleId,
    t_start: "2026-07-25T00:00:00.000Z",
    t_end: "2026-07-25T00:00:02.000Z",
    total_j: 10,
    components: [{ kind: "cpu", energy_j: 10, method: "rapl" }],
    attribution_policy: "l2_cpu_time",
    denominator_cpu_ms: 16000,
    rows: [],
    agent_process: { pid: 1, cpu_delta_ms: 1, allocated_j: 0.1 },
    baseline: { allocated_j: 9.9, share: 0.99, label: "baseline/idle" },
    l1_shadow_sum_share: 0,
  };
}

function emptySnapshot(asOfSeq: number, overrides: Partial<Snapshot> = {}): Snapshot {
  return {
    events: [],
    allocations: [],
    coverage_gaps: [],
    open_spans: [],
    watchdog: [],
    as_of_seq: asOfSeq,
    ...overrides,
  };
}

function fakeJsonResponse(body: unknown): Response {
  return { ok: true, status: 200, json: async () => body } as unknown as Response;
}

// --- test harness ----------------------------------------------------------

interface Harness {
  client: AfClient;
  eventStore: EventStore;
  allocStore: AllocStore;
  sessionStore: SessionStore;
  reportStore: ReportStore;
  healthStore: HealthStore;
  statusStore: StatusStore;
  visibility: FakeVisibility;
  uiStore: UiStore;
  esInstances: FakeEventSource[];
  fetchCalls: string[];
  callOrder: string[];
  setSnapshot: (snap: Snapshot) => void;
  /** When true, the next (and every subsequent, until cleared) call to
   * `/debug/snapshot` rejects instead of resolving — for exercising the
   * bootstrap/re-bootstrap failure paths. */
  setSnapshotFails: (fails: boolean) => void;
  /** Same, for `/debug/session`. */
  setSessionFails: (fails: boolean) => void;
  /** Same, for the post-snapshot `/debug/report?level=session` fetch. */
  setReportFails: (fails: boolean) => void;
  /** Same, for the post-snapshot `/debug/health` fetch. */
  setHealthFails: (fails: boolean) => void;
  /** What `/debug/status` answers: a status with this boot id, a 404 (a
   * watcher that predates the route), or a network failure. */
  setStatus: (mode: { bootId: string } | "404" | "fail") => void;
  /** `/debug/session` answers `null`: the watcher has no session yet. */
  setSessionNull: (isNull: boolean) => void;
}

function makeHarness(options: { eventStoreCapacity?: number } = {}): Harness {
  const eventStore = new EventStore(options.eventStoreCapacity ?? 64);
  const allocStore = new AllocStore();
  const sessionStore = new SessionStore();
  const reportStore = new ReportStore();
  const healthStore = new HealthStore();
  const uiStore = new UiStore();
  const esInstances: FakeEventSource[] = [];
  const fetchCalls: string[] = [];
  const callOrder: string[] = [];

  let currentSnapshot: Snapshot = emptySnapshot(0);
  let snapshotFails = false;
  let sessionFails = false;
  let reportFails = false;
  let healthFails = false;
  let statusMode: { bootId: string } | "404" | "fail" = { bootId: "boot-1" };
  let sessionNull = false;
  const statusStore = new StatusStore();
  const visibility = new FakeVisibility();

  const fetchImpl = (vi.fn(async (url: string) => {
    fetchCalls.push(url);
    if (url === "/debug/status") {
      if (statusMode === "fail") throw new Error("simulated /debug/status network failure");
      if (statusMode === "404") return { ok: false, status: 404, json: async () => ({ error: "not_found" }) } as unknown as Response;
      return fakeJsonResponse(watcherStatus(statusMode.bootId));
    }
    if (url === "/debug/session") {
      if (sessionFails) throw new Error("simulated /debug/session network failure");
      callOrder.push("session");
      return fakeJsonResponse(sessionNull ? null : SESSION_INFO);
    }
    if (url.startsWith("/debug/snapshot")) {
      if (snapshotFails) throw new Error("simulated /debug/snapshot network failure");
      callOrder.push("snapshot");
      return fakeJsonResponse(currentSnapshot);
    }
    if (url.startsWith("/debug/report")) {
      if (reportFails) throw new Error("simulated /debug/report network failure");
      callOrder.push("report");
      return fakeJsonResponse(REPORT_INFO);
    }
    if (url === "/debug/health") {
      if (healthFails) throw new Error("simulated /debug/health network failure");
      callOrder.push("health");
      return fakeJsonResponse(HEALTH_INFO);
    }
    throw new Error(`unexpected fetch ${url}`);
  }) as unknown) as typeof fetch;

  const createEventSource = (url: string): EventSourceLike => {
    callOrder.push("subscribe");
    const es = new FakeEventSource(url);
    esInstances.push(es);
    return es;
  };

  const client = new AfClient({
    eventStore,
    allocStore,
    sessionStore,
    reportStore,
    healthStore,
    statusStore,
    visibility,
    uiStore,
    fetchImpl,
    createEventSource,
  });

  return {
    client,
    eventStore,
    allocStore,
    sessionStore,
    reportStore,
    healthStore,
    statusStore,
    visibility,
    uiStore,
    esInstances,
    fetchCalls,
    callOrder,
    setSnapshot: (snap: Snapshot) => {
      currentSnapshot = snap;
    },
    setSnapshotFails: (fails: boolean) => {
      snapshotFails = fails;
    },
    setSessionFails: (fails: boolean) => {
      sessionFails = fails;
    },
    setReportFails: (fails: boolean) => {
      reportFails = fails;
    },
    setHealthFails: (fails: boolean) => {
      healthFails = fails;
    },
    setStatus: (mode) => {
      statusMode = mode;
    },
    setSessionNull: (isNull: boolean) => {
      sessionNull = isNull;
    },
  };
}

describe("AfClient — bootstrap order", () => {
  it("fetches session, then snapshot, then subscribes — in that strict order (report/health are fire-and-forget afterwards, never ahead of subscribe)", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(42));

    await h.client.start();

    expect(h.callOrder.slice(0, 3)).toEqual(["session", "snapshot", "subscribe"]);
    expect(h.sessionStore.data).toEqual(SESSION_INFO);
    expect(h.esInstances).toHaveLength(1);
    expect(h.esInstances[0].url).toBe("/debug/stream?from=42");
    h.client.dispose();
  });

  it("status is 'connecting' immediately, then 'live' on the first stream frame", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    expect(h.client.status).toBe("connecting");

    const startPromise = h.client.start();
    expect(h.client.status).toBe("connecting");
    await startPromise;
    expect(h.client.status).toBe("connecting"); // no frame yet

    h.esInstances[0].emit("health", { collectors: [], otlp_receiver: { endpoint: "x", protocol: "http/json", logs_accepted: 0, metrics_discarded: 0 }, rejected: [], python: [] }, 1);
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });
});

// The real `af watch --debug` server only pushes `report`/`health` frames
// per ingest pass, not on subscribe (docs/design-log.md) — unlike the mock,
// whose `/debug/stream` promptly resends the latest of each to a new
// connection. So AfClient fetches both once, itself, right after every
// (re)snapshot — otherwise Impact/Health would sit on their empty states
// until the control plane's next pass happened to run.
describe("AfClient — bootstrap fetches report + health once", () => {
  it("populates reportStore and healthStore from a one-shot fetch after the snapshot, without an SSE frame", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));

    await h.client.start();

    await vi.waitFor(() => {
      expect(h.reportStore.get("session")).toEqual(REPORT_INFO);
      expect(h.healthStore.data).toEqual(HEALTH_INFO);
    });
    // Never ahead of the strict session -> snapshot -> subscribe order.
    expect(h.callOrder.indexOf("report")).toBeGreaterThan(h.callOrder.indexOf("subscribe"));
    expect(h.callOrder.indexOf("health")).toBeGreaterThan(h.callOrder.indexOf("subscribe"));
    h.client.dispose();
  });

  it("a failed report/health fetch is non-fatal — bootstrap still reaches `live`, and the stores just stay empty until the next SSE frame", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    h.setReportFails(true);
    h.setHealthFails(true);

    await h.client.start();
    expect(h.esInstances).toHaveLength(1); // subscribe still happened — this failure never touches bootstrap's own retry path

    h.esInstances[0].emit("fact", actionSpanFact("spn_a", "evt_a", 0, 100), 1);
    expect(h.client.status).toBe("live");
    expect(h.reportStore.get("session")).toBeUndefined();
    expect(h.healthStore.data).toBeNull();
    h.client.dispose();
  });

  it("also runs after a post-`reset` re-bootstrap", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(10));
    await h.client.start();
    await vi.waitFor(() => expect(h.reportStore.get("session")).toEqual(REPORT_INFO));

    h.reportStore.levels = {}; // simulate nothing cached, as if this were a fresh page
    h.healthStore.data = null;
    h.setSnapshot(emptySnapshot(20));
    h.esInstances[0].emit("reset", {});

    await vi.waitFor(() => {
      expect(h.reportStore.get("session")).toEqual(REPORT_INFO);
      expect(h.healthStore.data).toEqual(HEALTH_INFO);
    });
    h.client.dispose();
  });
});

describe("AfClient — dedup across snapshot/stream overlap", () => {
  it("a fact present in both the snapshot and the stream is ingested exactly once", async () => {
    const h = makeHarness();
    const overlapping = actionSpanFact("spn_overlap", "evt_overlap", 0, 1000);
    h.setSnapshot(emptySnapshot(5, { events: [overlapping] }));

    await h.client.start();
    expect(h.eventStore.totalSeen).toBe(1);

    h.esInstances[0].emit("fact", overlapping, 5); // same event_id, replayed by the stream
    expect(h.eventStore.totalSeen, "duplicate event_id must not be re-ingested").toBe(1);

    const fresh = actionSpanFact("spn_new", "evt_new", 2000, 3000);
    h.esInstances[0].emit("fact", fresh, 6);
    expect(h.eventStore.totalSeen).toBe(2);
    h.client.dispose();
  });

  it("an alloc trace present in both snapshot and stream is ingested exactly once", async () => {
    const h = makeHarness();
    const trace = allocTrace("sample_1");
    h.setSnapshot(emptySnapshot(1, { allocations: [trace] }));

    await h.client.start();
    expect(h.allocStore.get("sample_1")).toEqual({ status: "ready", trace });

    const mutated = { ...trace, total_j: 999 };
    h.esInstances[0].emit("alloc", mutated, 2);
    // Dedup means the (different-looking, but same-id) replay is dropped —
    // the cached trace stays the snapshot's original, never silently patched.
    expect(h.allocStore.get("sample_1")).toEqual({ status: "ready", trace });
    h.client.dispose();
  });

  it("the dedup set is bounded (8192): once evicted, an id can be seen again without being treated as a duplicate", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    const es = h.esInstances[0];

    // Fill the dedup set past its cap with distinct ids, then replay the
    // very first one — it must have aged out, so this "duplicate" is
    // actually ingested again (a real server wouldn't do this, but it
    // pins down that the Set is genuinely bounded, not unbounded).
    const DEDUP_CAP = 8192;
    for (let i = 0; i < DEDUP_CAP + 1; i += 1) {
      es.emit("fact", actionSpanFact(`fill_${i}`, `fill_evt_${i}`, i, i + 1), i + 1);
    }
    expect(h.eventStore.totalSeen).toBe(DEDUP_CAP + 1);

    es.emit("fact", actionSpanFact("fill_0", "fill_evt_0", 0, 1), DEDUP_CAP + 2);
    expect(h.eventStore.totalSeen, "the oldest id aged out of the bounded dedup set").toBe(DEDUP_CAP + 2);
    h.client.dispose();
  }, 15000);

  // Item 1 (perf(console): ring eviction, O(1) not O(n)): pins the exact
  // wrap-boundary semantics of the fixed-size ring, not just "eventually
  // ages out" — the insert that fills the ring past capacity must evict
  // EXACTLY the one slot it overwrites, leaving every other still-live id
  // (including the very next one written) deduped as normal.
  it("dedup ring wrap boundary: filling past capacity evicts only the exact overwritten slot, not its neighbors", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    const es = h.esInstances[0];
    const DEDUP_CAP = 8192;

    for (let i = 0; i < DEDUP_CAP; i += 1) {
      es.emit("fact", actionSpanFact(`w_${i}`, `w_evt_${i}`, i, i + 1), i + 1);
    }
    expect(h.eventStore.totalSeen, "ring exactly full, write index wrapped back to 0").toBe(DEDUP_CAP);

    // The wrap-boundary insert: evicts exactly the slot at write index 0 (w_evt_0).
    es.emit("fact", actionSpanFact("w_new", "w_evt_new", DEDUP_CAP, DEDUP_CAP + 1), DEDUP_CAP + 1);
    expect(h.eventStore.totalSeen).toBe(DEDUP_CAP + 1);

    // w_evt_1 (the very next slot, NOT the one evicted) must still be deduped.
    es.emit("fact", actionSpanFact("w_1_again", "w_evt_1", 1, 2), DEDUP_CAP + 2);
    expect(h.eventStore.totalSeen, "w_evt_1 must still be deduped immediately after the wrap").toBe(DEDUP_CAP + 1);

    // w_evt_0 (the one actually evicted at the wrap) is now seen fresh.
    es.emit("fact", actionSpanFact("w_0_again", "w_evt_0", 0, 1), DEDUP_CAP + 3);
    expect(h.eventStore.totalSeen, "w_evt_0 aged out exactly at the wrap and is ingested again").toBe(DEDUP_CAP + 2);
    h.client.dispose();
  }, 15000);
});

describe("AfClient — pause buffering", () => {
  it("buffers frames while uiStore.live is false, and applies them in order on resume — visible via rev after a single tick", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    const revBeforePause = h.eventStore.rev;
    h.uiStore.live = false;

    const es = h.esInstances[0];
    es.emit("fact", actionSpanFact("b1", "e1", 0, 100), 1);
    es.emit("fact", actionSpanFact("b2", "e2", 200, 300), 2);
    es.emit("fact", actionSpanFact("b3", "e3", 400, 500), 3);

    expect(h.eventStore.totalSeen, "paused frames must not reach the store yet").toBe(0);
    expect(h.eventStore.rev, "paused frames must not bump rev either").toBe(revBeforePause);
    expect(h.client.pausedBuffered).toBe(3);

    h.uiStore.live = true;
    h.client.tick(); // a *single* 1 Hz tick must both drain the buffer AND flush it to rev

    // Regression guard: drain-then-flush must happen in the SAME tick, not
    // drain-this-tick/flush-next-tick — otherwise resumed data would be in
    // the store (totalSeen already right) but not yet rev-visible, and any
    // reactive UI reading `rev` would still show stale content for another
    // full tick after the counters already look caught up.
    expect(h.eventStore.rev, "a single post-resume tick must make the drain rev-visible, not lag a tick").toBe(revBeforePause + 1);
    expect(h.eventStore.totalSeen).toBe(3);
    expect([...h.eventStore.spans.keys()]).toEqual(["b1", "b2", "b3"]); // order preserved
    expect(h.client.pausedBuffered).toBe(0);
    expect(h.client.pausedDropped).toBe(0);
    h.client.dispose();
  });

  it("caps the buffer at the event store's ring capacity, dropping the oldest and counting drops", async () => {
    const h = makeHarness({ eventStoreCapacity: 3 });
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    const revBeforePause = h.eventStore.rev;
    h.uiStore.live = false;

    const es = h.esInstances[0];
    for (let i = 1; i <= 5; i += 1) {
      es.emit("fact", actionSpanFact(`b${i}`, `e${i}`, i * 100, i * 100 + 50), i);
    }

    expect(h.client.pausedBuffered).toBe(3); // capped at capacity
    expect(h.client.pausedDropped).toBe(2); // b1, b2 dropped

    h.uiStore.live = true;
    h.client.tick();
    expect(h.eventStore.rev, "single-tick resume must be rev-visible here too").toBe(revBeforePause + 1);

    expect(h.eventStore.totalSeen).toBe(3);
    expect([...h.eventStore.spans.keys()]).toEqual(["b3", "b4", "b5"]);
    expect(h.client.pausedDropped, "drop counter resets on resume").toBe(0);
    h.client.dispose();
  });

  it("the EventSource connection stays open while paused — pausing never closes it", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    h.uiStore.live = false;
    expect(h.esInstances[0].closed).toBe(false);
    h.client.dispose();
  });
});

// Item 6 (untested paths): a malformed `data:` frame — JSON.parse fails —
// must be dropped silently: never crash, never corrupt store state, never
// move `status` (receiving ANY frame, even a malformed one, is still real
// evidence the transport works, so `onFrameReceived()` runs first and
// `status` should stay/become `live` exactly as a well-formed frame would;
// what must NOT happen is the malformed payload reaching a store).
describe("AfClient — malformed SSE frame", () => {
  it("drops a frame whose `data:` fails JSON.parse without state corruption or an unexpected status change", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    h.esInstances[0].emit("fact", actionSpanFact("spn_a", "evt_a", 0, 100), 1);
    expect(h.client.status).toBe("live");
    const totalSeenBefore = h.eventStore.totalSeen;
    const revBefore = h.eventStore.rev;
    const spansBefore = [...h.eventStore.spans.keys()];

    // Emit a raw malformed frame directly — `es.emit()` always JSON.stringifies
    // a valid payload, so this bypasses that helper to hand the listener
    // genuinely invalid `data:` text, as a real malformed SSE frame would.
    const listeners = h.esInstances[0].listeners.get("fact") ?? [];
    expect(listeners.length).toBeGreaterThan(0);
    for (const l of listeners) l({ data: "{not valid json", lastEventId: "2" });

    expect(h.client.status, "a malformed frame is still evidence the transport works — status stays live").toBe("live");
    expect(h.eventStore.totalSeen, "the malformed payload must never reach eventStore").toBe(totalSeenBefore);
    expect(h.eventStore.rev, "no ingest happened, so no dirty flag was ever set").toBe(revBefore);
    expect([...h.eventStore.spans.keys()]).toEqual(spansBefore);
    expect(h.client.lastSeq, "lastEventId bookkeeping still updates even though the payload was dropped").toBe(2);

    // The connection keeps working for the next, well-formed frame.
    h.esInstances[0].emit("fact", actionSpanFact("spn_b", "evt_b", 200, 300), 3);
    expect(h.eventStore.totalSeen).toBe(totalSeenBefore + 1);
    h.client.dispose();
  });
});

// Item 6 (untested paths, deliberate behavior change): frames buffered while
// paused belong to the stream generation `reset` is about to tear down — the
// fresh re-snapshot rebootstrap() fetches supersedes them entirely, so they
// must never survive to be replayed on a later resume (previously they
// were: `handleFrame`'s `reset` branch never touched `pauseBuffer` at all).
describe("AfClient — `reset` while paused clears the pause buffer", () => {
  it("drops the dead-generation buffer on reset, counts the drop honestly, and never replays it on resume", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    h.uiStore.live = false;

    const es = h.esInstances[0];
    es.emit("fact", actionSpanFact("b1", "e1", 0, 100), 1);
    es.emit("fact", actionSpanFact("b2", "e2", 200, 300), 2);
    expect(h.client.pausedBuffered).toBe(2);
    expect(h.client.pausedDropped).toBe(0);

    const freshFact = actionSpanFact("spn_fresh", "evt_fresh", 5000, 5100);
    h.setSnapshot(emptySnapshot(30, { events: [freshFact] }));
    es.emit("reset", {});
    await vi.waitFor(() => expect(h.esInstances).toHaveLength(2));

    // Post-reset: the buffer is gone, the drop is surfaced, and state
    // matches the fresh snapshot exactly (not the stale buffered frames).
    expect(h.client.pausedBuffered, "the dead-generation buffer must be cleared by the reset").toBe(0);
    expect(h.client.pausedDropped, "the drop is surfaced honestly through the existing counter").toBe(2);
    expect(h.eventStore.totalSeen, "only the fresh snapshot's event was ingested").toBe(1);
    expect([...h.eventStore.spans.keys()]).toEqual(["spn_fresh"]);

    // Resume: nothing from the cleared buffer reappears, and one live tick
    // closes the books on the whole pause episode — `pausedDropped` must not
    // stay elevated forever just because the buffer it counted against was
    // already empty by the time of this tick (see `drainPauseBufferIfLive`'s
    // doc comment: the reset-drop and the buffer-drain are now the same
    // episode, closed together, not gated on the buffer still being
    // non-empty).
    h.uiStore.live = true;
    h.client.tick();
    expect(h.eventStore.totalSeen, "resume must not replay the cleared buffer").toBe(1);
    expect([...h.eventStore.spans.keys()]).toEqual(["spn_fresh"]);
    expect(h.client.pausedDropped, "one live tick must close out the reset-time drop, not leave it elevated indefinitely").toBe(0);
    expect(h.client.pausedBuffered).toBe(0);
    h.client.dispose();
  });

  it("pausedDropped from a reset-while-paused drop is cleared by the very next live tick, even with nothing left to drain", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    h.uiStore.live = false;

    const es = h.esInstances[0];
    es.emit("fact", actionSpanFact("p1", "pe1", 0, 100), 1);
    es.emit("fact", actionSpanFact("p2", "pe2", 200, 300), 2);
    es.emit("fact", actionSpanFact("p3", "pe3", 400, 500), 3);

    h.setSnapshot(emptySnapshot(40));
    es.emit("reset", {});
    await vi.waitFor(() => expect(h.esInstances).toHaveLength(2));

    // Reset-while-paused counted the drop and emptied the buffer.
    expect(h.client.pausedDropped).toBe(3);
    expect(h.client.pausedBuffered).toBe(0);

    // Resume — a single tick, with the buffer already empty, must still
    // zero the drop counter rather than leaving it stuck at 3 until some
    // unrelated future pause cycle happens to touch it.
    h.uiStore.live = true;
    h.client.tick();
    expect(h.client.pausedDropped).toBe(0);
    expect(h.client.pausedBuffered).toBe(0);
    h.client.dispose();
  });

  it("a reset while LIVE (buffer already empty) is unaffected — same re-bootstrap as always", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    expect(h.uiStore.live).toBe(true);

    const freshFact = actionSpanFact("spn_fresh2", "evt_fresh2", 1000, 1100);
    h.setSnapshot(emptySnapshot(15, { events: [freshFact] }));
    h.esInstances[0].emit("reset", {});
    await vi.waitFor(() => expect(h.esInstances).toHaveLength(2));

    expect(h.client.pausedDropped).toBe(0);
    expect(h.client.pausedBuffered).toBe(0);
    expect(h.eventStore.totalSeen).toBe(1);
    h.client.dispose();
  });
});

describe("AfClient — `reset` triggers a full re-bootstrap", () => {
  it("re-snapshots and resubscribes on `event: reset`, closing the old connection", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(10));
    await h.client.start();
    const firstEs = h.esInstances[0];

    const secondFact = actionSpanFact("spn_after_reset", "evt_after_reset", 5000, 5100);
    h.setSnapshot(emptySnapshot(20, { events: [secondFact] }));

    firstEs.emit("reset", {});
    await vi.waitFor(() => expect(h.esInstances).toHaveLength(2));

    expect(firstEs.closed).toBe(true);
    expect(h.callOrder.filter((c) => c === "snapshot")).toHaveLength(2);
    expect(h.esInstances[1].url).toBe("/debug/stream?from=20");
    expect(h.eventStore.totalSeen).toBe(1); // the post-reset snapshot's event was ingested
    h.client.dispose();
  });

  it("a reset whose re-snapshot fetch fails does NOT leave status at `live` while disconnected, and schedules a retry", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(10));
    await h.client.start();
    const firstEs = h.esInstances[0];
    firstEs.emit("health", { collectors: [], otlp_receiver: { endpoint: "x", protocol: "http/json", logs_accepted: 0, metrics_discarded: 0 }, rejected: [], python: [] }, 1);
    expect(h.client.status).toBe("live");

    // The reset frame itself is real evidence the transport just worked, but
    // rebootstrap() tears the connection down and its own re-snapshot fetch
    // is about to fail — the client is genuinely disconnected at that point
    // and must say so, not keep showing the dot from the reset frame.
    h.setSnapshotFails(true);
    firstEs.emit("reset", {});

    await vi.waitFor(() => expect(h.client.status).not.toBe("live"), { timeout: 2000 });
    expect(h.client.status).toBe("reconnecting");
    expect(firstEs.closed, "the old connection is still torn down even though the re-fetch failed").toBe(true);
    expect(h.esInstances, "must not have resubscribed on a failed re-fetch").toHaveLength(1);

    // Recovery: once the snapshot endpoint is healthy again, the scheduled
    // backoff retry picks it back up on its own.
    h.setSnapshotFails(false);
    await vi.waitFor(() => expect(h.esInstances).toHaveLength(2), { timeout: 2000 });
    expect(h.esInstances[1].url).toBe("/debug/stream?from=10");
    h.esInstances[1].emit("fact", actionSpanFact("spn_recovered", "evt_recovered", 0, 100), 2);
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });
});

describe("AfClient — bootstrap failure handling", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("a failed initial bootstrap never leaves status stuck at `connecting` — it moves to `reconnecting` and retries with backoff", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    h.setSnapshotFails(true); // session succeeds, snapshot fails — bootstrap() must catch this itself

    await h.client.start(); // must resolve even though the bootstrap attempt inside it failed
    expect(h.client.status, "a thrown bootstrap fetch must not leave status at connecting forever").toBe("reconnecting");
    expect(h.esInstances, "must never have reached subscribe() on a failed bootstrap").toHaveLength(0);

    h.setSnapshotFails(false);
    await vi.advanceTimersByTimeAsync(500); // backoff floor -> bootstrap() retries and this time succeeds
    expect(h.esInstances).toHaveLength(1);
    expect(h.client.status, "still not live until an actual frame arrives").toBe("reconnecting");

    h.esInstances[0].emit("fact", actionSpanFact("spn_a", "evt_a", 0, 100), 1);
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });

  it("a session fetch failure during bootstrap is handled the same way as a snapshot failure", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    h.setSessionFails(true);

    await h.client.start();
    expect(h.client.status).toBe("reconnecting");
    expect(h.sessionStore.data, "must not have partially set session data").toBeNull();
    expect(h.esInstances).toHaveLength(0);

    h.setSessionFails(false);
    await vi.advanceTimersByTimeAsync(500);
    await vi.advanceTimersByTimeAsync(0); // the retry is status -> session -> snapshot: one more async hop than before
    expect(h.sessionStore.data).toEqual(SESSION_INFO);
    expect(h.esInstances).toHaveLength(1);
    h.client.dispose();
  });

  it("repeated bootstrap failures reach `offline` at the same N=5 threshold as SSE errors, still retrying at the capped interval", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    h.setSnapshotFails(true);

    // Bootstrap retries are fully automatic (no external trigger needed,
    // unlike an SSE `error` event) — each retry that still fails schedules
    // the next one itself, so advances must match the exact backoff
    // sequence (500, 1000, 2000, 4000, capped 8000) one step at a time, or
    // a single generous advance cascades through several failures at once.
    await h.client.start();
    expect(h.client.status).toBe("reconnecting"); // failure 1, next delay 500

    await vi.advanceTimersByTimeAsync(500); // failure 2, next delay 1000
    expect(h.client.status).toBe("reconnecting");

    await vi.advanceTimersByTimeAsync(1000); // failure 3, next delay 2000
    expect(h.client.status).toBe("reconnecting");

    await vi.advanceTimersByTimeAsync(2000); // failure 4, next delay 4000
    expect(h.client.status).toBe("reconnecting");

    await vi.advanceTimersByTimeAsync(4000); // failure 5 -> offline, next delay capped at 8000
    expect(h.client.status).toBe("offline");
    expect(h.esInstances).toHaveLength(0); // every attempt failed before ever reaching subscribe()

    h.setSnapshotFails(false);
    await vi.advanceTimersByTimeAsync(8000); // still retrying at the capped interval while offline
    expect(h.esInstances).toHaveLength(1);
    h.esInstances[0].emit("fact", actionSpanFact("spn_b", "evt_b", 0, 100), 1);
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });
});

describe("AfClient — status transitions with backoff", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("reconnects with 0.5s->8s exponential backoff and goes offline after 5 consecutive failures, recovering to live on the next frame", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(0));
    await h.client.start();
    h.esInstances[0].emit("health", { collectors: [], otlp_receiver: { endpoint: "x", protocol: "http/json", logs_accepted: 0, metrics_discarded: 0 }, rejected: [], python: [] }, 1);
    expect(h.client.status).toBe("live");

    // First failure: backoff floor is 500ms — not sooner, not much later.
    h.esInstances[0].emitError();
    expect(h.client.status).toBe("reconnecting");
    expect(h.esInstances).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(499);
    expect(h.esInstances, "must not reconnect before the 500ms floor").toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(h.esInstances).toHaveLength(2);

    // Failures 2-4 stay 'reconnecting'; each reconnect attempt is generously
    // advanced past its backoff ceiling (8s) to fire deterministically.
    for (let failure = 2; failure <= 4; failure += 1) {
      h.esInstances.at(-1)!.emitError();
      expect(h.client.status, `failure ${failure}`).toBe("reconnecting");
      await vi.advanceTimersByTimeAsync(8500);
    }
    expect(h.esInstances).toHaveLength(5);

    // 5th consecutive failure -> offline, still retrying at the capped interval.
    h.esInstances.at(-1)!.emitError();
    expect(h.client.status).toBe("offline");
    await vi.advanceTimersByTimeAsync(8500);
    expect(h.esInstances).toHaveLength(6);
    expect(h.client.status, "still offline until a frame actually arrives").toBe("offline");

    // Recovery: any frame on the new connection brings it back to live.
    h.esInstances.at(-1)!.emit("fact", actionSpanFact("spn_recover", "evt_recover", 0, 100), 2);
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });
});

// ---------------------------------------------------------------------------
// Connection state machine: live on open, heartbeat, boot id, quiet retries
// ---------------------------------------------------------------------------

/** The `/debug/*` requests of one bootstrap, in order, ignoring the
 * fire-and-forget report/health/sessions fetches that follow it. */
function bootstrapFetches(calls: string[]): string[] {
  return calls.filter((url) => url === "/debug/status" || url === "/debug/session" || url.startsWith("/debug/snapshot"));
}

describe("AfClient — live on open, and the empty watcher", () => {
  it("is live as soon as the stream opens, before any frame", async () => {
    const h = makeHarness();
    await h.client.start();
    expect(h.client.status).toBe("connecting");
    h.esInstances[0].emitOpen();
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });

  it("probes /debug/status first, then session, then snapshot", async () => {
    const h = makeHarness();
    await h.client.start();
    expect(bootstrapFetches(h.fetchCalls)).toEqual(["/debug/status", "/debug/session", "/debug/snapshot?window=180s"]);
    expect(h.statusStore.data?.boot_id).toBe("boot-1");
    expect(h.client.bootId).toBe("boot-1");
    h.client.dispose();
  });

  it("a null session is not a failure: the watcher simply has no session yet", async () => {
    const h = makeHarness();
    h.setSessionNull(true);
    h.setSnapshot(emptySnapshot(-1));
    await h.client.start();
    expect(h.esInstances, "the stream is still opened").toHaveLength(1);
    expect(h.esInstances[0].url).toBe("/debug/stream?from=-1");
    expect(h.sessionStore.data).toBeNull();
    expect(h.sessionStore.list).toEqual([]);
    h.esInstances[0].emitOpen();
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });

  it("a 404 on /debug/status does not block bootstrap", async () => {
    const h = makeHarness();
    h.setStatus("404");
    await h.client.start();
    expect(h.esInstances).toHaveLength(1);
    expect(h.statusStore.unavailable).toBe(true);
    expect(h.statusStore.data).toBeNull();
    h.esInstances[0].emitOpen();
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });

  it("a status frame with the same boot_id only updates the status store", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    const fetchesBefore = h.fetchCalls.length;
    const next = { ...watcherStatus("boot-1"), flush_interval_ms: 1234 };
    h.esInstances[0].emit("status", next);
    expect(h.statusStore.data?.flush_interval_ms).toBe(1234);
    expect(h.esInstances).toHaveLength(1);
    expect(h.fetchCalls).toHaveLength(fetchesBefore);
    h.client.dispose();
  });

  it("status and ping frames are applied even while paused, never buffered", async () => {
    const h = makeHarness();
    await h.client.start();
    h.uiStore.toggleLive();
    h.esInstances[0].emit("status", { ...watcherStatus("boot-1"), flush_interval_ms: 77 });
    h.esInstances[0].emit("ping", { boot_id: "boot-1" });
    expect(h.statusStore.data?.flush_interval_ms).toBe(77);
    expect(h.client.pausedBuffered).toBe(0);
    h.client.dispose();
  });
});

describe("AfClient — reconnect, boot id and heartbeat", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("reconnect to the SAME watcher process probes its identity, then resumes from the last frame seen — nothing the server can replay is skipped", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(10));
    await h.client.start();
    h.esInstances[0].emitOpen();
    h.esInstances[0].emit("fact", actionSpanFact("spn_a", "evt_a", 0, 100), 11);
    h.esInstances[0].emit("decision", { kind: "attr", text: "seen", ts: "2026-07-25T00:00:00.000Z" }, 12);
    await vi.advanceTimersByTimeAsync(0);

    // The head has moved on to 40 while the connection was down. Jumping
    // there would skip every decision/reject frame in 13..40, which the
    // snapshot does not carry and the server would have replayed.
    h.setSnapshot(emptySnapshot(40));
    h.fetchCalls.length = 0;
    h.esInstances[0].emitError();
    expect(h.esInstances[0].closed, "the failed stream is closed at once, so the browser cannot auto-reconnect it").toBe(true);
    await vi.advanceTimersByTimeAsync(500);

    expect(bootstrapFetches(h.fetchCalls), "the probe alone: same process, no new snapshot").toEqual(["/debug/status"]);
    expect(h.esInstances).toHaveLength(2);
    expect(h.esInstances[1].url).toBe("/debug/stream?from=12");
    h.client.dispose();
  });

  it("a `reset` on that resumed stream falls back to a full re-sync from a fresh snapshot", async () => {
    const h = makeHarness();
    h.setSnapshot(emptySnapshot(10));
    await h.client.start();
    h.esInstances[0].emitOpen();
    h.esInstances[0].emitError();
    await vi.advanceTimersByTimeAsync(500);
    expect(h.esInstances[1].url).toBe("/debug/stream?from=10");

    h.setSnapshot(emptySnapshot(9000));
    h.fetchCalls.length = 0;
    h.esInstances[1].emit("reset", {});
    await vi.advanceTimersByTimeAsync(0);
    expect(bootstrapFetches(h.fetchCalls)).toEqual(["/debug/status", "/debug/session", "/debug/snapshot?window=180s"]);
    expect(h.esInstances[2].url).toBe("/debug/stream?from=9000");
    h.client.dispose();
  });

  it("reconnect to a watcher with no status route cannot know the process, so it re-syncs from a snapshot", async () => {
    const h = makeHarness();
    h.setStatus("404");
    h.setSnapshot(emptySnapshot(10));
    await h.client.start();
    h.esInstances[0].emitOpen();
    h.setSnapshot(emptySnapshot(40));
    h.esInstances[0].emitError();
    await vi.advanceTimersByTimeAsync(500);
    expect(h.esInstances[1].url).toBe("/debug/stream?from=40");
    h.client.dispose();
  });

  it("a watcher with no status route never pings, and is not dropped for staying quiet", async () => {
    const h = makeHarness();
    h.setStatus("404");
    await h.client.start();
    h.esInstances[0].emitOpen();
    // A pre-status watcher is silent between flushes (a minute by default).
    await vi.advanceTimersByTimeAsync(5 * 60_000);
    expect(h.esInstances[0].closed).toBe(false);
    expect(h.esInstances).toHaveLength(1);
    expect(h.client.status).toBe("live");
    h.client.dispose();
  });

  it("a different boot_id clears state and re-syncs", async () => {
    const h = makeHarness();
    const fact = actionSpanFact("spn_a", "evt_a", 0, 100);
    h.setSnapshot(emptySnapshot(5, { events: [fact] }));
    await h.client.start();
    h.esInstances[0].emitOpen();
    h.esInstances[0].emit("decision", { kind: "attr", text: "old process", ts: "2026-07-25T00:00:00.000Z" }, 6);
    h.client.tick();
    expect(h.eventStore.totalSeen).toBe(1);
    expect(h.eventStore.decisions).toHaveLength(1);

    // The watcher restarted: same event in its fresh snapshot, new boot id.
    h.setStatus({ bootId: "boot-2" });
    h.setSnapshot(emptySnapshot(0, { events: [fact] }));
    h.esInstances[0].emit("ping", { boot_id: "boot-2" });
    await vi.advanceTimersByTimeAsync(0);

    expect(h.esInstances[0].closed).toBe(true);
    expect(h.esInstances).toHaveLength(2);
    expect(h.esInstances[1].url).toBe("/debug/stream?from=0");
    expect(h.client.bootId).toBe("boot-2");
    expect(h.statusStore.data?.boot_id).toBe("boot-2");
    expect(h.eventStore.decisions, "the old process's decision log is gone").toHaveLength(0);
    expect(h.eventStore.totalSeen, "the store was cleared, then the same event id was ingested again from the new snapshot").toBe(1);
    h.client.dispose();
  });

  it("a status frame carrying a different boot_id re-syncs too", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    h.setStatus({ bootId: "boot-2" });
    h.esInstances[0].emit("status", watcherStatus("boot-2"));
    await vi.advanceTimersByTimeAsync(0);
    expect(h.esInstances).toHaveLength(2);
    expect(h.client.bootId).toBe("boot-2");
    h.client.dispose();
  });

  it("a re-bootstrap does not duplicate coverage gaps it already holds", async () => {
    const h = makeHarness();
    const gap = { t_start: "2026-07-25T00:00:10.000Z", t_end: "2026-07-25T00:00:20.000Z", reason: "sampler exited" };
    h.setSnapshot(emptySnapshot(3, { coverage_gaps: [gap] as Snapshot["coverage_gaps"] }));
    await h.client.start();
    h.esInstances[0].emitOpen();
    // A `reset` forces the snapshot (and its last 180s of gaps) to be re-read.
    h.esInstances[0].emit("reset", {});
    await vi.advanceTimersByTimeAsync(0);
    expect(h.esInstances).toHaveLength(2);
    expect(h.eventStore.gaps).toHaveLength(1);
    h.client.dispose();
  });

  it("no event for 45s closes the stream and counts one failure", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    await vi.advanceTimersByTimeAsync(44_000);
    expect(h.client.status).toBe("live");
    expect(h.esInstances[0].closed).toBe(false);

    await vi.advanceTimersByTimeAsync(2_000);
    expect(h.esInstances[0].closed).toBe(true);
    // One failure: reconnecting, and the retry (500ms later) has not yet opened.
    expect(["reconnecting", "connecting"]).toContain(h.client.status);
    expect(h.client.status).not.toBe("offline");
    h.client.dispose();
  });

  it("any event, including ping, resets the heartbeat clock", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    for (let i = 0; i < 4; i += 1) {
      await vi.advanceTimersByTimeAsync(30_000);
      h.esInstances[0].emit("ping", { boot_id: "boot-1" });
    }
    expect(h.esInstances[0].closed).toBe(false);
    expect(h.client.status).toBe("live");
    expect(h.esInstances).toHaveLength(1);
    h.client.dispose();
  });

  it("backoff doubles from 500ms and caps at 30s", async () => {
    const h = makeHarness();
    h.setStatus("fail");
    await h.client.start(); // failure 1
    const attempts = () => h.fetchCalls.filter((url) => url === "/debug/status").length;
    expect(attempts()).toBe(1);
    for (const delay of [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000]) {
      await vi.advanceTimersByTimeAsync(delay - 1);
      const before = attempts();
      await vi.advanceTimersByTimeAsync(1);
      expect(attempts(), `one attempt exactly ${delay}ms after the previous failure`).toBe(before + 1);
    }
    expect(h.client.status).toBe("offline");
    h.client.dispose();
  });

  it("offline after 5 consecutive failures; an open stream resets the count", async () => {
    const h = makeHarness();
    h.setStatus("fail");
    await h.client.start();
    await vi.advanceTimersByTimeAsync(500 + 1000 + 2000);
    expect(h.client.status).toBe("reconnecting"); // 4 failures
    await vi.advanceTimersByTimeAsync(4000);
    expect(h.client.status).toBe("offline"); // 5

    h.setStatus({ bootId: "boot-1" });
    await vi.advanceTimersByTimeAsync(8000);
    expect(h.esInstances).toHaveLength(1);
    h.esInstances[0].emitOpen();
    expect(h.client.status).toBe("live");

    // The count restarted: one new failure is "reconnecting", after 500ms.
    h.setStatus("fail");
    h.esInstances[0].emitError();
    expect(h.client.status).toBe("reconnecting");
    const before = h.fetchCalls.length;
    await vi.advanceTimersByTimeAsync(500);
    expect(h.fetchCalls.length).toBe(before + 1);
    h.client.dispose();
  });

  it("no retry timer runs while the tab is hidden", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    await vi.advanceTimersByTimeAsync(0); // let the first bootstrap's best-effort report/health fetches settle
    h.visibility.hidden = true;
    h.fetchCalls.length = 0;
    h.esInstances[0].emitError();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.fetchCalls, "a hidden tab does not poll a watcher that is off").toEqual([]);
    expect(h.esInstances).toHaveLength(1);
    h.client.dispose();
  });

  it("waking the tab retries immediately and restarts backoff at 500ms", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    h.visibility.hidden = true;
    h.setStatus("fail");
    h.esInstances[0].emitError();
    await vi.advanceTimersByTimeAsync(60_000);
    h.fetchCalls.length = 0;
    expect(h.client.status, "one failure, however long the tab stayed hidden").toBe("reconnecting");

    h.visibility.wake();
    await vi.advanceTimersByTimeAsync(0);
    expect(h.fetchCalls, "one attempt at once").toEqual(["/debug/status"]);

    // It failed: the next attempt is the backoff floor away, not 30s.
    await vi.advanceTimersByTimeAsync(499);
    expect(h.fetchCalls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(h.fetchCalls).toHaveLength(2);
    h.client.dispose();
  });

  it("waking a tab whose stream is healthy does nothing", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    h.fetchCalls.length = 0;
    h.visibility.wake();
    await vi.advanceTimersByTimeAsync(0);
    expect(bootstrapFetches(h.fetchCalls)).toEqual([]);
    expect(h.esInstances[0].closed).toBe(false);
    h.client.dispose();
  });

  it("a wake after a long sleep makes exactly one attempt, re-syncs, and never goes offline", async () => {
    const h = makeHarness();
    await h.client.start();
    h.esInstances[0].emitOpen();
    const seen: string[] = [];

    // The laptop slept: ten minutes pass with no timer having run, and the
    // watcher was restarted meanwhile.
    vi.setSystemTime(Date.now() + 10 * 60_000);
    h.setStatus({ bootId: "boot-2" });
    h.fetchCalls.length = 0;
    h.visibility.wake();
    seen.push(h.client.status);
    await vi.advanceTimersByTimeAsync(0);
    seen.push(h.client.status);

    expect(bootstrapFetches(h.fetchCalls)).toEqual(["/debug/status", "/debug/session", "/debug/snapshot?window=180s"]);
    expect(h.esInstances).toHaveLength(2);
    expect(h.client.bootId).toBe("boot-2");
    h.esInstances[1].emitOpen();
    seen.push(h.client.status);
    expect(seen.at(-1)).toBe("live");
    expect(seen).not.toContain("offline");

    // And nothing else fires afterwards: one attempt, not a burst.
    await vi.advanceTimersByTimeAsync(5_000);
    expect(h.esInstances).toHaveLength(2);
    h.client.dispose();
  });
});
