import { describe, expect, it, vi } from "vitest";
import { AllocStore } from "../src/lib/stores/allocStore.svelte";
import type { AllocationTrace } from "../src/lib/types/debug";

function makeTrace(id: string, totalJ = 10): AllocationTrace {
  return {
    sample_event_id: id,
    t_start: "2026-07-25T00:00:00.000Z",
    t_end: "2026-07-25T00:00:02.000Z",
    total_j: totalJ,
    components: [{ kind: "cpu", energy_j: totalJ, method: "rapl" }],
    attribution_policy: "l2_cpu_time",
    denominator_cpu_ms: 16000,
    rows: [],
    agent_process: { pid: 1, cpu_delta_ms: 10, allocated_j: 0.1 },
    baseline: { allocated_j: totalJ - 0.1, share: 0.99, label: "baseline/idle" },
    l1_shadow_sum_share: 0,
  };
}

function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function fakeResponse(status: number, body: unknown): Response {
  return {
    status,
    ok: status >= 200 && status < 300,
    json: async () => body,
  } as unknown as Response;
}

describe("AllocStore", () => {
  it("get() is a synchronous cache read that never fetches", () => {
    const fetchImpl = vi.fn();
    const store = new AllocStore(fetchImpl as unknown as typeof fetch);
    expect(store.get("nope")).toBeUndefined();
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("ingest() populates the cache without a network call, and replace is allowed on the same id", () => {
    const store = new AllocStore(vi.fn() as unknown as typeof fetch);
    store.ingest(makeTrace("s1", 10));
    expect(store.get("s1")).toEqual({ status: "ready", trace: makeTrace("s1", 10) });

    // Server re-emitting a trace for the same id (e.g. on sample close) replaces it wholesale.
    store.ingest(makeTrace("s1", 42));
    expect(store.get("s1")).toEqual({ status: "ready", trace: makeTrace("s1", 42) });
  });

  it("fetch() dedups two concurrent calls for the same id into a single request", async () => {
    const { promise: responsePromise, resolve } = deferred<Response>();
    const fetchImpl = vi.fn().mockReturnValue(responsePromise);
    const store = new AllocStore(fetchImpl as unknown as typeof fetch);

    const a = store.fetch("s1");
    const b = store.fetch("s1");

    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl).toHaveBeenCalledWith("/debug/alloc/s1");

    resolve(fakeResponse(200, makeTrace("s1")));
    const [entryA, entryB] = await Promise.all([a, b]);

    expect(entryA).toEqual({ status: "ready", trace: makeTrace("s1") });
    expect(entryB).toBe(entryA); // same resolved value, not just equal
    expect(fetchImpl).toHaveBeenCalledTimes(1);

    // Once resolved and cached, a further fetch() doesn't hit the network again.
    const c = await store.fetch("s1");
    expect(c).toEqual({ status: "ready", trace: makeTrace("s1") });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it("fetch() for different ids issues separate requests, not deduped against each other", async () => {
    const fetchImpl = vi.fn().mockImplementation(async (url: string) => fakeResponse(200, makeTrace(url.split("/").pop()!)));
    const store = new AllocStore(fetchImpl as unknown as typeof fetch);

    const [a, b] = await Promise.all([store.fetch("s1"), store.fetch("s2")]);
    expect(fetchImpl).toHaveBeenCalledTimes(2);
    expect(a).toEqual({ status: "ready", trace: makeTrace("s1") });
    expect(b).toEqual({ status: "ready", trace: makeTrace("s2") });
  });

  it("a 404 caches an `unavailable` marker instead of the trace, and doesn't retry on a later fetch()", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(fakeResponse(404, { error: "not_found" }));
    const store = new AllocStore(fetchImpl as unknown as typeof fetch);

    const entry = await store.fetch("missing");
    expect(entry).toEqual({ status: "unavailable" });
    expect(store.get("missing")).toEqual({ status: "unavailable" });

    const again = await store.fetch("missing");
    expect(again).toEqual({ status: "unavailable" });
    expect(fetchImpl).toHaveBeenCalledTimes(1); // cached — no retry
  });

  it("in-flight dedup does not apply across a 404: a fresh fetch() after caching `unavailable` returns the cache, not a new request", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(fakeResponse(404, {})).mockResolvedValueOnce(fakeResponse(200, makeTrace("late")));
    const store = new AllocStore(fetchImpl as unknown as typeof fetch);

    await store.fetch("late");
    expect(store.get("late")).toEqual({ status: "unavailable" });

    // Simulating the trace becoming available later would require a cache
    // invalidation this store deliberately doesn't offer (traces are
    // immutable once closed, per DATA-CONTRACT §3.3) — so a second fetch()
    // for the same id must keep returning the cached marker, not re-hit the
    // network path that would return the now-different second mock response.
    const stillUnavailable = await store.fetch("late");
    expect(stillUnavailable).toEqual({ status: "unavailable" });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it("flush() bumps rev once per batch of changes, not per ingest", () => {
    const store = new AllocStore(vi.fn() as unknown as typeof fetch);
    store.ingest(makeTrace("s1"));
    store.ingest(makeTrace("s2"));
    expect(store.rev).toBe(0);
    store.flush();
    expect(store.rev).toBe(1);
    store.flush();
    expect(store.rev).toBe(1);
  });
});
