// AllocStore (DATA-CONTRACT §3.3): a memoised cache of allocation traces,
// keyed by `sample_event_id`. Traces are immutable once the sample closes,
// so caching is unconditional — this store never recomputes or patches a
// trace's contents (global-constraints.md #1: the client computes nothing).
import type { AllocationTrace } from "../types/debug";
import { boundFetch } from "../client/boundFetch";

/** `unavailable` marks a sample the server 404'd (outside the retained
 * window) — the UI shows "trace unavailable (outside window)" rather than
 * silently omitting the row or retrying forever. */
export type AllocationEntry = { status: "ready"; trace: AllocationTrace } | { status: "unavailable" };

export class AllocStore {
  rev = $state(0);
  private dirty = false;

  private map = new Map<string, AllocationEntry>();
  private inflight = new Map<string, Promise<AllocationEntry>>();
  private readonly fetchImpl: typeof fetch;

  constructor(fetchImpl: typeof fetch = boundFetch) {
    this.fetchImpl = fetchImpl;
  }

  private markDirty(): void {
    this.dirty = true;
  }

  flush(): void {
    if (this.dirty) {
      this.rev += 1;
      this.dirty = false;
    }
  }

  /** Insert-only, but replace is allowed on the same id — the server may
   * re-emit a trace when a sample closes (DATA-CONTRACT §3.3). Never patches
   * fields of an existing trace; always a whole-object replace. */
  ingest(trace: AllocationTrace): void {
    this.map.set(trace.sample_event_id, { status: "ready", trace });
    this.markDirty();
  }

  /** Synchronous cache read — does not fetch. */
  get(id: string): AllocationEntry | undefined {
    return this.map.get(id);
  }

  /** Fetches `/debug/alloc/{id}` when not already cached, with in-flight
   * dedup: concurrent callers for the same id share one request. A 404
   * caches an `unavailable` marker rather than retrying. */
  async fetch(id: string): Promise<AllocationEntry> {
    const cached = this.map.get(id);
    if (cached) return cached;

    const existing = this.inflight.get(id);
    if (existing) return existing;

    const promise = this.doFetch(id);
    this.inflight.set(id, promise);
    try {
      return await promise;
    } finally {
      this.inflight.delete(id);
    }
  }

  private async doFetch(id: string): Promise<AllocationEntry> {
    const res = await this.fetchImpl(`/debug/alloc/${encodeURIComponent(id)}`);
    if (res.status === 404) {
      const entry: AllocationEntry = { status: "unavailable" };
      this.map.set(id, entry);
      this.markDirty();
      return entry;
    }
    if (!res.ok) {
      throw new Error(`GET /debug/alloc/${id} failed: ${res.status}`);
    }
    const trace = (await res.json()) as AllocationTrace;
    const entry: AllocationEntry = { status: "ready", trace };
    this.map.set(id, entry);
    this.markDirty();
    return entry;
  }
}

export const allocStore = new AllocStore();
