// EventStore (DATA-CONTRACT §3.2): bounded fact history + a time-bucketed
// span index, so "which spans overlap this sample?" is a bucket lookup, not
// a full scan. Bulk data (the ring, the span map, the bucket index, the
// decision/reject lists, the counters) is plain non-reactive state; `rev` is
// the only reactive signal, and it only advances when `flush()` is called —
// once per 1 Hz tick, from AfClient's single interval (architecture rule:
// "bumped once per flush batch, not per frame"). Ingest methods never touch
// `rev` directly; they only flip an internal `dirty` flag.
//
// This store never reads the clock: every timestamp it stores is parsed
// once, at ingest, from the `ts`/`t_start`/`t_end` strings the server sent
// ("timestamps parsed once on ingest to ms epoch"). Query methods take
// explicit `tStartMs`/`tEndMs` bounds from the caller (a selector, ultimately
// driven by `uiStore.nowMs`) rather than calling `Date.now()` themselves.
import type { ActionSpan, FactEvent } from "../types/contract1";
import type { DecisionFrame, GapFrame, OpenActionSpanEvent, RejectFrame, WatchdogEntry } from "../types/debug";

/** 2-second bucket width (DATA-CONTRACT §3.2). */
const BUCKET_MS = 2000;

/** Workaround for a TypeScript quirk, not a contract redefinition: every
 * generated Contract #1 interface carries a `[k: string]: unknown` index
 * signature (mirroring JSON Schema's `additionalProperties`), and `keyof`
 * over such an interface widens to `string | number` — which is exactly
 * what `OpenActionSpanEvent`'s `Omit<ActionSpan, "t_end">` (debug.ts) is
 * built from. The Omit's resulting mapped type then resolves every field to
 * `unknown` instead of its real type. Re-picking the same literal keys
 * directly (no `keyof`/`Omit` involved) sidesteps the widening and recovers
 * the real per-field types — the values themselves are untouched. */
type OpenSpanPayloadFields = Pick<ActionSpan, "span_id" | "tool_name" | "tool_kind" | "execution_locus" | "status" | "pids" | "t_start">;

/** One `action_span`'s current view in the store: open (`tEndMs === null`,
 * from a snapshot's `open_spans`) or closed (from a `fact` frame). A later
 * closing `fact` for the same `span_id` REPLACES this record in place —
 * never a second entry. `span_id` is treated as opaque and stable, per
 * DATA-CONTRACT §3.2 — never synthesised, always the server's own id. */
export interface SpanRecord {
  span_id: string;
  tool_name: string;
  tool_kind: ActionSpan["tool_kind"];
  execution_locus: ActionSpan["execution_locus"];
  status?: ActionSpan["status"];
  pids?: number[];
  tStart: string;
  tStartMs: number;
  /** `null` while open (no `t_end` yet). */
  tEnd: string | null;
  tEndMs: number | null;
  /** Inclusive bucket range this span is currently inserted into. For an
   * open span this grows lazily as queries reach further forward in time
   * (see `spansOverlapping`); ring eviction removes the span from exactly
   * this range, never a stale wider one. */
  bucketStart: number;
  bucketEnd: number;
  /** Ring `seq` of the fact currently backing this record, or `null` for a
   * still-open span with no backing ring entry yet. Eviction of the ring
   * slot at this `seq` is what triggers removing the span from the map and
   * its buckets — see `evictOldest`. */
  ringSeq: number | null;
}

interface RingEntry {
  seq: number;
  ts: string;
  tsMs: number;
  event: FactEvent;
}

/** A `DecisionFrame` as stored — plus `seq`, a monotonically increasing
 * counter assigned once at ingest (never per-render, never derived from
 * array position). Decision frames carry no id of their own, and re-deriving
 * a key from `(kind, ts)` or an array index breaks Svelte's `{#each}`
 * identity tracking the moment a new decision arrives and shifts every
 * older row's index (`selectDecisionLog` renders newest-first) — `seq` is
 * stable across that shift because it's assigned once and never recomputed.
 * Deliberately NOT a synthesised `event_id`/`span_id`: those stay exactly
 * what the server sent (global-constraints.md #6), this is purely a
 * client-side render key. */
export interface DecisionRecord extends DecisionFrame {
  seq: number;
}

function bucketIdx(tMs: number, t0Ms: number): number {
  return Math.floor((tMs - t0Ms) / BUCKET_MS);
}

/** The last bucket a half-open interval [startMs, endMs) occupies. A
 * zero-length interval still occupies its start bucket. */
function lastBucketOf(startMs: number, endMs: number, t0Ms: number): number {
  const end = Math.max(startMs, endMs - 1);
  return bucketIdx(end, t0Ms);
}

export class EventStore {
  rev = $state(0);
  private dirty = false;

  /** Memoised result of the `facts` getter (see below) — materialising the
   * ring into a plain array is an O(capacity) walk, and Stream's three
   * selectors each read `facts` once per recompute, so without this a
   * single tick's flush pays that walk three times over. Invalidated in
   * `flush()`, the same point that bumps `rev` — never per-ingest — which
   * is safe because every reader of `facts` is a `memo1`-wrapped selector
   * gated on `rev`, so nothing observes a partial (mid-tick) ring anyway. */
  private _factsCache: readonly { event: FactEvent; tsMs: number }[] | null = null;

  readonly capacity: number;
  private readonly t0Ms: number;

  // --- ring (fixed-size circular buffer of ingested `fact` frames) ---
  private ring: (RingEntry | undefined)[];
  private writeIndex = 0;
  private count = 0;
  private nextSeq = 0;
  private _totalSeen = 0;

  // --- span index ---
  private spanMap = new Map<string, SpanRecord>();
  private buckets = new Map<number, Set<string>>();

  // --- incremental counters (retained-set only, not all-time) ---
  private _perType = new Map<FactEvent["type"], number>();
  private _perCollector = new Map<string, number>();

  // --- per-collector last-seen (ms epoch, ALL-TIME — never decremented on
  // ring eviction, mirroring totalSeen). Task 5's genuinely-missing
  // accessor: the Timeline rail's collector dot state (SCREENS.md "status
  // dot ... accent <12s, neutral-400 <45s, magenta beyond") needs a
  // recency signal that didn't previously exist on this store — only
  // per-collector event *counts* did. Kept as a tiny separate map rather
  // than folded into `_perCollector` so the count map's existing shape
  // (and the eventStore.test.ts shadow model that mirrors it) stays
  // untouched. */
  private _perCollectorLastSeenMs = new Map<string, number>();

  // --- other bounded/replaced collections ---
  private _decisions: DecisionRecord[] = [];
  private _decisionSeq = 0;
  private _rejects: RejectFrame[] = [];
  private _gaps: GapFrame[] = [];
  private _watchdog: WatchdogEntry[] = [];

  // --- open-span cap (fix: unbounded growth — see ingestOpenSpan's doc) ---
  private readonly openSpanCap: number;
  private openSpanOrder: string[] = []; // FIFO of currently-open span_ids, oldest first
  private _openSpanDropped = 0;

  static readonly DECISION_LOG_CAP = 500;
  static readonly REJECT_LIST_CAP = 200;
  static readonly OPEN_SPAN_CAP = 512;

  constructor(capacity = 4000, t0Ms = 0, openSpanCap = EventStore.OPEN_SPAN_CAP) {
    this.capacity = capacity;
    this.t0Ms = t0Ms;
    this.ring = new Array(capacity);
    this.openSpanCap = openSpanCap;
  }

  private markDirty(): void {
    this.dirty = true;
  }

  /** Bumps `rev` once if anything changed since the last flush; a no-op
   * otherwise. Called from AfClient's single 1 Hz interval — never from
   * ingest methods themselves. */
  flush(): void {
    if (this.dirty) {
      this.rev += 1;
      this.dirty = false;
      this._factsCache = null;
    }
  }

  // ---------------------------------------------------------------------
  // Bucket bookkeeping
  // ---------------------------------------------------------------------

  private insertIntoBuckets(spanId: string, fromBucket: number, toBucketInclusive: number): void {
    for (let b = fromBucket; b <= toBucketInclusive; b += 1) {
      let set = this.buckets.get(b);
      if (!set) {
        set = new Set();
        this.buckets.set(b, set);
      }
      set.add(spanId);
    }
  }

  private removeFromBuckets(spanId: string, fromBucket: number, toBucketInclusive: number): void {
    for (let b = fromBucket; b <= toBucketInclusive; b += 1) {
      const set = this.buckets.get(b);
      if (!set) continue;
      set.delete(spanId);
      if (set.size === 0) this.buckets.delete(b);
    }
  }

  // ---------------------------------------------------------------------
  // Ring eviction
  // ---------------------------------------------------------------------

  private decrementCounters(event: FactEvent): void {
    const typeCount = this._perType.get(event.type) ?? 0;
    if (typeCount <= 1) this._perType.delete(event.type);
    else this._perType.set(event.type, typeCount - 1);

    const collectorName = event.collector.name;
    const collectorCount = this._perCollector.get(collectorName) ?? 0;
    if (collectorCount <= 1) this._perCollector.delete(collectorName);
    else this._perCollector.set(collectorName, collectorCount - 1);
  }

  private incrementCounters(event: FactEvent): void {
    this._perType.set(event.type, (this._perType.get(event.type) ?? 0) + 1);
    this._perCollector.set(event.collector.name, (this._perCollector.get(event.collector.name) ?? 0) + 1);
  }

  /** Updates a collector's last-seen ms from an ingested fact's own `ts`
   * (already-parsed, never `Date.now()`) — monotonic per collector: an
   * out-of-order-arriving older fact never regresses a newer last-seen. */
  private touchLastSeen(collectorName: string, tsMs: number): void {
    const prev = this._perCollectorLastSeenMs.get(collectorName);
    if (prev === undefined || tsMs > prev) this._perCollectorLastSeenMs.set(collectorName, tsMs);
  }

  /** Evicts the ring slot about to be overwritten (if the ring is at
   * capacity), cleaning up its span/bucket footprint. */
  private evictSlotAt(index: number): void {
    const evicted = this.ring[index];
    if (!evicted) return;
    this.decrementCounters(evicted.event);
    if (evicted.event.type === "action_span") {
      const spanId = (evicted.event as Extract<FactEvent, { type: "action_span" }>).payload.span_id;
      const record = this.spanMap.get(spanId);
      // Only remove if this ring slot is still the authoritative backing
      // for the span — guards against (impossible today, but cheap to
      // guard) a stale eviction racing a newer close of the same span_id.
      if (record && record.ringSeq === evicted.seq) {
        this.removeFromBuckets(spanId, record.bucketStart, record.bucketEnd);
        this.spanMap.delete(spanId);
      }
    }
  }

  // ---------------------------------------------------------------------
  // Ingestion
  // ---------------------------------------------------------------------

  /** Ingests one `fact` frame — from `/debug/snapshot`'s `events[]` (in
   * order) or a `fact` stream frame. Closes/replaces an open span in place
   * when `fact.type === "action_span"` matches an already-open `span_id`. */
  ingestFact(fact: FactEvent): void {
    this._totalSeen += 1;

    const seq = this.nextSeq;
    this.nextSeq += 1;
    const tsMs = Date.parse(fact.ts);

    if (this.count === this.capacity) {
      this.evictSlotAt(this.writeIndex);
    } else {
      this.count += 1;
    }
    this.ring[this.writeIndex] = { seq, ts: fact.ts, tsMs, event: fact };
    this.writeIndex = (this.writeIndex + 1) % this.capacity;

    this.incrementCounters(fact);
    this.touchLastSeen(fact.collector.name, tsMs);

    if (fact.type === "action_span") {
      this.ingestClosedSpan(fact as Extract<FactEvent, { type: "action_span" }>, seq);
    }

    this.markDirty();
  }

  private ingestClosedSpan(fact: Extract<FactEvent, { type: "action_span" }>, seq: number): void {
    const { payload } = fact;
    const spanId = payload.span_id;
    const tStartMs = Date.parse(payload.t_start);
    const tEndMs = Date.parse(payload.t_end);
    const startBucket = bucketIdx(tStartMs, this.t0Ms);
    const endBucket = lastBucketOf(tStartMs, tEndMs, this.t0Ms);

    const existing = this.spanMap.get(spanId);
    if (existing) {
      // Was open (or, in principle, previously closed) — reconcile the
      // bucket range to the now-known real extent, then replace in place.
      if (existing.tEndMs === null) {
        // Closing now: this span_id no longer counts against the open-span
        // cap (see ingestOpenSpan) — free its slot for a future open.
        this.removeFromOpenOrder(spanId);
      }
      if (existing.bucketEnd > endBucket) {
        this.removeFromBuckets(spanId, endBucket + 1, existing.bucketEnd);
      } else if (existing.bucketEnd < endBucket) {
        this.insertIntoBuckets(spanId, existing.bucketEnd + 1, endBucket);
      }
      existing.tool_name = payload.tool_name;
      existing.tool_kind = payload.tool_kind;
      existing.execution_locus = payload.execution_locus;
      existing.status = payload.status;
      existing.pids = payload.pids;
      existing.tStart = payload.t_start;
      existing.tStartMs = tStartMs;
      existing.tEnd = payload.t_end;
      existing.tEndMs = tEndMs;
      existing.bucketStart = startBucket;
      existing.bucketEnd = endBucket;
      existing.ringSeq = seq;
      return;
    }

    this.insertIntoBuckets(spanId, startBucket, endBucket);
    this.spanMap.set(spanId, {
      span_id: spanId,
      tool_name: payload.tool_name,
      tool_kind: payload.tool_kind,
      execution_locus: payload.execution_locus,
      status: payload.status,
      pids: payload.pids,
      tStart: payload.t_start,
      tStartMs,
      tEnd: payload.t_end,
      tEndMs,
      bucketStart: startBucket,
      bucketEnd: endBucket,
      ringSeq: seq,
    });
  }

  private removeFromOpenOrder(spanId: string): void {
    const idx = this.openSpanOrder.indexOf(spanId);
    if (idx !== -1) this.openSpanOrder.splice(idx, 1);
  }

  /** Ingests a currently-running span from `/debug/snapshot`'s `open_spans[]`
   * — no `t_end` yet, so no ring slot (it isn't a `fact` frame; the eventual
   * closing fact is what occupies a ring slot and subjects the span to
   * eviction — see `evictSlotAt`). Replaces any existing record for the same
   * `span_id` (a redundant bootstrap should never duplicate).
   *
   * Because an open span has no ring slot, a span whose closing fact never
   * arrives (lost, or the server genuinely never closes it) would otherwise
   * grow `spanMap`/the bucket index forever — every other collection in this
   * store is capped (ring `capacity`, decisions 500, rejects 200), so this
   * one needs its own bound too. Policy: cap concurrently-open spans at
   * `openSpanCap` (default 512), FIFO — the oldest-*opened* span is evicted
   * (map entry + its buckets removed) to make room. This is a pure defensive
   * bound against a pathological/lost-close case, not a claim about the
   * fact-stream's "showing N of M" (`retained`/`totalSeen`): those counters
   * are specifically about the fact ring and stay untouched here, so they
   * keep meaning exactly what their own doc comments say. `openSpanDropped`
   * exposes this eviction separately for anyone (tests, a future health
   * panel) that wants to know it happened. */
  ingestOpenSpan(open: OpenActionSpanEvent): void {
    const payload = open.payload as OpenSpanPayloadFields;
    const spanId = payload.span_id;
    const tStartMs = Date.parse(payload.t_start);
    const startBucket = bucketIdx(tStartMs, this.t0Ms);

    const existing = this.spanMap.get(spanId);
    if (existing) return; // already known (open or closed) — never regress a closed span back to open

    if (this.openSpanOrder.length >= this.openSpanCap) {
      const oldestId = this.openSpanOrder.shift();
      if (oldestId !== undefined) {
        const oldest = this.spanMap.get(oldestId);
        if (oldest) {
          this.removeFromBuckets(oldestId, oldest.bucketStart, oldest.bucketEnd);
          this.spanMap.delete(oldestId);
        }
        this._openSpanDropped += 1;
      }
    }

    this.insertIntoBuckets(spanId, startBucket, startBucket);
    this.spanMap.set(spanId, {
      span_id: spanId,
      tool_name: payload.tool_name,
      tool_kind: payload.tool_kind,
      execution_locus: payload.execution_locus,
      status: payload.status,
      pids: payload.pids,
      tStart: payload.t_start,
      tStartMs,
      tEnd: null,
      tEndMs: null,
      bucketStart: startBucket,
      bucketEnd: startBucket,
      ringSeq: null,
    });
    this.openSpanOrder.push(spanId);
    this.markDirty();
  }

  ingestDecision(decision: DecisionFrame): void {
    const seq = this._decisionSeq;
    this._decisionSeq += 1;
    this._decisions.push({ ...decision, seq });
    if (this._decisions.length > EventStore.DECISION_LOG_CAP) this._decisions.shift();
    this.markDirty();
  }

  ingestReject(reject: RejectFrame): void {
    this._rejects.push(reject);
    if (this._rejects.length > EventStore.REJECT_LIST_CAP) this._rejects.shift();
    this.markDirty();
  }

  ingestGap(gap: GapFrame): void {
    this._gaps.push(gap);
    this.markDirty();
  }

  /** Full replacement, per DATA-CONTRACT §2.3's `watchdog` frame semantics. */
  replaceWatchdog(entries: WatchdogEntry[]): void {
    this._watchdog = entries;
    this.markDirty();
  }

  // ---------------------------------------------------------------------
  // Queries
  // ---------------------------------------------------------------------

  /** Spans overlapping the half-open interval [tStartMs, tEndMs), found via
   * the bucket index (never a full scan of `spanMap`). Open spans are
   * lazily extended to cover buckets up to `tEndMs` first — the *caller*
   * supplies "now" via `tEndMs`; this store never reads the clock itself. */
  spansOverlapping(tStartMs: number, tEndMs: number): SpanRecord[] {
    const startBucket = bucketIdx(tStartMs, this.t0Ms);
    const endBucket = lastBucketOf(tStartMs, tEndMs, this.t0Ms);

    for (const record of this.spanMap.values()) {
      if (record.tEndMs === null && record.bucketEnd < endBucket) {
        this.insertIntoBuckets(record.span_id, record.bucketEnd + 1, endBucket);
        record.bucketEnd = endBucket;
      }
    }

    const seen = new Set<string>();
    const out: SpanRecord[] = [];
    for (let b = startBucket; b <= endBucket; b += 1) {
      const set = this.buckets.get(b);
      if (!set) continue;
      for (const spanId of set) {
        if (seen.has(spanId)) continue;
        seen.add(spanId);
        const record = this.spanMap.get(spanId);
        if (!record) continue;
        const recordEnd = record.tEndMs ?? tEndMs;
        if (record.tStartMs < tEndMs && tStartMs < recordEnd) out.push(record);
      }
    }
    return out;
  }

  // ---------------------------------------------------------------------
  // Read-only accessors
  // ---------------------------------------------------------------------

  get spans(): ReadonlyMap<string, SpanRecord> {
    return this.spanMap;
  }

  /** Every currently-retained `fact` frame (any type), oldest-to-newest ring
   * order — i.e. arrival order, NOT necessarily chronological (SCREENS.md:
   * spans/llm_calls are stamped at their end time while samples arrive on
   * their own cadence, so a caller that needs chronological order must sort
   * by `tsMs` itself). Pairs each event with its already-`Date.parse`d `ts`
   * (computed once at ingest) so selectors never re-parse a timestamp or
   * touch the clock. `spanMap` only tracks `action_span`s; this is the one
   * accessor that surfaces every other fact type (`llm_call`,
   * `energy_sample`, `process_sample`, `session_meta`) for a selector — e.g.
   * the Stream tab — that needs to iterate the whole stream, not just spans. */
  get facts(): readonly { event: FactEvent; tsMs: number }[] {
    if (this._factsCache) return this._factsCache;
    const out: { event: FactEvent; tsMs: number }[] = [];
    if (this.count > 0) {
      const start = this.count === this.capacity ? this.writeIndex : 0;
      for (let i = 0; i < this.count; i += 1) {
        const entry = this.ring[(start + i) % this.capacity];
        if (entry) out.push({ event: entry.event, tsMs: entry.tsMs });
      }
    }
    this._factsCache = out;
    return out;
  }

  get decisions(): readonly DecisionRecord[] {
    return this._decisions;
  }

  get rejects(): readonly RejectFrame[] {
    return this._rejects;
  }

  get gaps(): readonly GapFrame[] {
    return this._gaps;
  }

  get watchdog(): readonly WatchdogEntry[] {
    return this._watchdog;
  }

  /** All-time count of `ingestFact` calls — never decremented. Pairs with
   * `retained` for the footer's "showing N of M". */
  get totalSeen(): number {
    return this._totalSeen;
  }

  /** Facts currently held in the ring (<= capacity). */
  get retained(): number {
    return this.count;
  }

  get perType(): ReadonlyMap<FactEvent["type"], number> {
    return this._perType;
  }

  get perCollector(): ReadonlyMap<string, number> {
    return this._perCollector;
  }

  /** All-time last-seen ms per collector (see `touchLastSeen`) — the
   * Timeline rail's collector dot state input. Every collector that has
   * ever sent a fact appears here, even after its facts have all been
   * evicted from the ring (an idle-but-once-seen collector should read as
   * "long idle", magenta, not vanish from the rail). */
  get perCollectorLastSeenMs(): ReadonlyMap<string, number> {
    return this._perCollectorLastSeenMs;
  }

  /** Count of open spans evicted by the `openSpanCap` FIFO bound (see
   * `ingestOpenSpan`) — never decremented. */
  get openSpanDropped(): number {
    return this._openSpanDropped;
  }

  /** Exposed for test verification of the bucket-index invariant only —
   * selectors should use `spansOverlapping`, not this. */
  bucketsDebug(): ReadonlyMap<number, ReadonlySet<string>> {
    return this.buckets;
  }
}

export const eventStore = new EventStore();
