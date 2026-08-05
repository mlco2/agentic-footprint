// Seeded randomized oracle test for EventStore (brief §"Tests"). This is the
// core deliverable, not a smoke test: it independently re-implements the
// *specification* (FIFO ring eviction, the 2s bucket index, open-span lazy
// bucket extension, incremental counters) as a parallel "shadow" model, then
// after every batch of ingests asserts the real store's internal state
// matches a brute-force answer computed from that shadow model — never by
// reading the store's own internals back at itself.
import { describe, expect, it } from "vitest";
import { EventStore } from "../src/lib/stores/eventStore.svelte";
import type { FactEvent } from "../src/lib/types/contract1";
import type { OpenActionSpanEvent } from "../src/lib/types/debug";

const CAPACITY = 64;
const T0 = 0;
const BUCKET_MS = 2000;

// Same small deterministic PRNG dev/scenario.ts uses — no Math.random.
function mulberry32(seed: number): () => number {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function iso(tMs: number): string {
  return new Date(tMs).toISOString();
}

let idCounter = 0;
function nextEventId(): string {
  idCounter += 1;
  return `evt_${idCounter}`;
}

const COLLECTORS = ["claude-code", "codecarbon-sampler", "otlp-cc"];
const NON_SPAN_TYPES: Array<Exclude<FactEvent["type"], "action_span">> = [
  "llm_call",
  "energy_sample",
  "process_sample",
  "session_meta",
];

function envelopeBase(type: FactEvent["type"], tMs: number, collector: string) {
  return {
    schema_version: "0.1.0",
    event_id: nextEventId(),
    ts: iso(tMs),
    collector: { name: collector, version: "0.0.0" },
    session_id: "ses_test",
    type,
  } as const;
}

function makeNonSpanFact(type: Exclude<FactEvent["type"], "action_span">, tMs: number, collector: string): FactEvent {
  const base = envelopeBase(type, tMs, collector);
  switch (type) {
    case "llm_call":
      return { ...base, type: "llm_call", payload: { provider: "acme", model_id_requested: "m1", usage: {}, usage_source: "api_response" } } as FactEvent;
    case "energy_sample":
      return { ...base, type: "energy_sample", payload: { t_start: iso(tMs - 1000), t_end: iso(tMs), components: [{ kind: "cpu", energy_j: 1, method: "rapl" }] } } as FactEvent;
    case "process_sample":
      return { ...base, type: "process_sample", payload: { t_start: iso(tMs - 1000), t_end: iso(tMs), processes: [] } } as FactEvent;
    case "session_meta":
      return { ...base, type: "session_meta", payload: { agent_app: { name: "test-agent" } } } as FactEvent;
  }
}

function makeClosedSpanFact(spanId: string, tStartMs: number, tEndMs: number, tsMs: number, collector: string): FactEvent {
  const base = envelopeBase("action_span", tsMs, collector);
  return {
    ...base,
    type: "action_span",
    payload: {
      span_id: spanId,
      tool_name: `Tool(${spanId})`,
      tool_kind: "bash",
      execution_locus: "local",
      t_start: iso(tStartMs),
      t_end: iso(tEndMs),
      status: "ok",
    },
  } as FactEvent;
}

function makeOpenSpanEvent(spanId: string, tStartMs: number, collector: string): OpenActionSpanEvent {
  const base = envelopeBase("action_span", tStartMs, collector);
  return {
    ...base,
    type: "action_span",
    payload: {
      span_id: spanId,
      tool_name: `Tool(${spanId})`,
      tool_kind: "bash",
      execution_locus: "local",
      t_start: iso(tStartMs),
      status: "ok",
    },
  } as OpenActionSpanEvent;
}

function bucketIdx(tMs: number): number {
  return Math.floor((tMs - T0) / BUCKET_MS);
}

function lastBucketOf(startMs: number, endMs: number): number {
  return bucketIdx(Math.max(startMs, endMs - 1));
}

// --- shadow model: an independent re-implementation of the spec ------------

interface ShadowSpan {
  span_id: string;
  tStartMs: number;
  tEndMs: number | null;
  bucketStart: number;
  bucketEnd: number;
  ringSeq: number | null;
}

interface ShadowRingEntry {
  seq: number;
  event: FactEvent;
}

class ShadowModel {
  ring: ShadowRingEntry[] = [];
  spans = new Map<string, ShadowSpan>();
  everSeenSpanIds = new Set<string>();
  perType = new Map<string, number>();
  perCollector = new Map<string, number>();
  totalSeen = 0;
  private nextSeq = 0;

  private bumpCounters(event: FactEvent, delta: 1 | -1): void {
    const t = this.perType.get(event.type) ?? 0;
    const nt = t + delta;
    if (nt <= 0) this.perType.delete(event.type);
    else this.perType.set(event.type, nt);

    const c = this.perCollector.get(event.collector.name) ?? 0;
    const nc = c + delta;
    if (nc <= 0) this.perCollector.delete(event.collector.name);
    else this.perCollector.set(event.collector.name, nc);
  }

  ingestFact(event: FactEvent): void {
    this.totalSeen += 1;
    const seq = this.nextSeq;
    this.nextSeq += 1;

    if (this.ring.length >= CAPACITY) {
      const evicted = this.ring.shift()!;
      this.bumpCounters(evicted.event, -1);
      if (evicted.event.type === "action_span") {
        const spanId = (evicted.event as Extract<FactEvent, { type: "action_span" }>).payload.span_id;
        const span = this.spans.get(spanId);
        if (span && span.ringSeq === evicted.seq) {
          this.spans.delete(spanId);
        }
      }
    }
    this.ring.push({ seq, event });
    this.bumpCounters(event, 1);

    if (event.type === "action_span") {
      const payload = (event as Extract<FactEvent, { type: "action_span" }>).payload;
      const tStartMs = Date.parse(payload.t_start);
      const tEndMs = Date.parse(payload.t_end);
      const startBucket = bucketIdx(tStartMs);
      const endBucket = lastBucketOf(tStartMs, tEndMs);
      this.everSeenSpanIds.add(payload.span_id);
      this.spans.set(payload.span_id, {
        span_id: payload.span_id,
        tStartMs,
        tEndMs,
        bucketStart: startBucket,
        bucketEnd: endBucket,
        ringSeq: seq,
      });
    }
  }

  ingestOpenSpan(open: OpenActionSpanEvent): void {
    // See eventStore.svelte.ts's OpenSpanPayloadFields comment: OpenActionSpanEvent's
    // Omit-derived payload type resolves every field to `unknown` due to a
    // keyof-widening quirk from the generated types' index signatures.
    const payload = open.payload as { span_id: string; t_start: string };
    if (this.spans.has(payload.span_id)) return;
    const tStartMs = Date.parse(payload.t_start);
    const startBucket = bucketIdx(tStartMs);
    this.everSeenSpanIds.add(payload.span_id);
    this.spans.set(payload.span_id, {
      span_id: payload.span_id,
      tStartMs,
      tEndMs: null,
      bucketStart: startBucket,
      bucketEnd: startBucket,
      ringSeq: null,
    });
  }

  /** Mirrors the store's lazy bucket extension for open spans — must be
   * called with the query's own end bucket (computed exactly as the store
   * computes it), or the two models diverge on a dimension that isn't a bug. */
  private extendOpenSpansTo(endBucket: number): void {
    for (const span of this.spans.values()) {
      if (span.tEndMs === null && span.bucketEnd < endBucket) {
        span.bucketEnd = endBucket;
      }
    }
  }

  bruteForceOverlap(tStartMs: number, tEndMs: number): string[] {
    this.extendOpenSpansTo(lastBucketOf(tStartMs, tEndMs));
    const out: string[] = [];
    for (const span of this.spans.values()) {
      const end = span.tEndMs ?? tEndMs;
      if (span.tStartMs < tEndMs && tStartMs < end) out.push(span.span_id);
    }
    return out.sort();
  }

  bruteForceBuckets(): Map<number, Set<string>> {
    const buckets = new Map<number, Set<string>>();
    for (const span of this.spans.values()) {
      for (let b = span.bucketStart; b <= span.bucketEnd; b += 1) {
        let set = buckets.get(b);
        if (!set) {
          set = new Set();
          buckets.set(b, set);
        }
        set.add(span.span_id);
      }
    }
    return buckets;
  }
}

function bucketsEqual(actual: ReadonlyMap<number, ReadonlySet<string>>, expected: Map<number, Set<string>>): boolean {
  if (actual.size !== expected.size) return false;
  for (const [bucket, expectedSet] of expected) {
    const actualSet = actual.get(bucket);
    if (!actualSet) return false;
    if (actualSet.size !== expectedSet.size) return false;
    for (const id of expectedSet) if (!actualSet.has(id)) return false;
  }
  return true;
}

describe("EventStore — seeded randomized oracle (ring + bucket span index)", () => {
  it("bucket index, overlap queries, counters, and eviction stay correct across ~2000 mixed ingests", () => {
    const store = new EventStore(CAPACITY, T0);
    const shadow = new ShadowModel();
    const rng = mulberry32(0xe1f5_ca57);

    let clockMs = 0;
    const openPending: string[] = [];
    let spanCounter = 0;
    const TOTAL = 2000;
    const CHECK_EVERY = 50;

    for (let i = 1; i <= TOTAL; i += 1) {
      clockMs += Math.floor(rng() * 300); // roughly-increasing clock, not strictly monotonic across fact types
      const collector = COLLECTORS[Math.floor(rng() * COLLECTORS.length)];
      const r = rng();

      if (r < 0.15) {
        // non-span fact
        const type = NON_SPAN_TYPES[Math.floor(rng() * NON_SPAN_TYPES.length)];
        const fact = makeNonSpanFact(type, clockMs, collector);
        store.ingestFact(fact);
        shadow.ingestFact(fact);
      } else if (r < 0.35 && openPending.length > 0) {
        // close a pending open span
        const spanId = openPending.shift()!;
        const openSpan = shadow.spans.get(spanId)!;
        // Random duration, deliberately including bucket-edge-crossing cases:
        // exact multiples of BUCKET_MS, zero-length, and multi-bucket spans.
        const durationChoice = rng();
        const duration = durationChoice < 0.15 ? 0 : durationChoice < 0.3 ? BUCKET_MS : Math.floor(rng() * BUCKET_MS * 5);
        const tEndMs = openSpan.tStartMs + duration;
        const fact = makeClosedSpanFact(spanId, openSpan.tStartMs, tEndMs, clockMs, collector);
        store.ingestFact(fact);
        shadow.ingestFact(fact);
      } else if (r < 0.55) {
        // brand-new open span (kept pending for a possible later close)
        spanCounter += 1;
        const spanId = `open_${spanCounter}`;
        const open = makeOpenSpanEvent(spanId, clockMs, collector);
        store.ingestOpenSpan(open);
        shadow.ingestOpenSpan(open);
        openPending.push(spanId);
      } else {
        // brand-new already-closed span, straight through the ring
        spanCounter += 1;
        const spanId = `closed_${spanCounter}`;
        const durationChoice = rng();
        const duration = durationChoice < 0.15 ? 0 : durationChoice < 0.3 ? BUCKET_MS : Math.floor(rng() * BUCKET_MS * 5);
        const fact = makeClosedSpanFact(spanId, clockMs, clockMs + duration, clockMs + duration, collector);
        store.ingestFact(fact);
        shadow.ingestFact(fact);
      }

      if (i % CHECK_EVERY !== 0) continue;

      // Exercise (and mirror) the lazy bucket-extension path with a query
      // window straddling "now", sometimes exactly on a bucket boundary.
      const queryEndMs = clockMs + Math.floor(rng() * BUCKET_MS * 3);
      const queryStartMs = Math.max(0, queryEndMs - Math.floor(rng() * BUCKET_MS * 4));

      const actualOverlap = store.spansOverlapping(queryStartMs, queryEndMs).map((s) => s.span_id).sort();
      const expectedOverlap = shadow.bruteForceOverlap(queryStartMs, queryEndMs);
      expect(actualOverlap, `batch ${i}: spansOverlapping mismatch`).toEqual(expectedOverlap);

      const expectedBuckets = shadow.bruteForceBuckets();
      expect(bucketsEqual(store.bucketsDebug(), expectedBuckets), `batch ${i}: bucket index diverged from brute-force rebuild`).toBe(true);

      // Counters == recount from the shadow's retained-ring contents.
      const expectedPerType = new Map<string, number>();
      const expectedPerCollector = new Map<string, number>();
      for (const { event } of shadow.ring) {
        expectedPerType.set(event.type, (expectedPerType.get(event.type) ?? 0) + 1);
        expectedPerCollector.set(event.collector.name, (expectedPerCollector.get(event.collector.name) ?? 0) + 1);
      }
      expect(Object.fromEntries(store.perType), `batch ${i}: perType counter mismatch`).toEqual(Object.fromEntries(expectedPerType));
      expect(Object.fromEntries(store.perCollector), `batch ${i}: perCollector counter mismatch`).toEqual(Object.fromEntries(expectedPerCollector));
      expect(store.totalSeen, `batch ${i}: totalSeen mismatch`).toBe(shadow.totalSeen);
      expect(store.retained, `batch ${i}: retained mismatch`).toBe(shadow.ring.length);

      // Evicted spans (ever seen, no longer tracked) must be absent from
      // every bucket — not just the ones their own range used to occupy.
      for (const spanId of shadow.everSeenSpanIds) {
        if (shadow.spans.has(spanId)) continue;
        for (const set of store.bucketsDebug().values()) {
          expect(set.has(spanId), `batch ${i}: evicted span ${spanId} leaked into a bucket`).toBe(false);
        }
      }
    }
  });

  it("open-span close-and-replace: never duplicates, and retracts over-extended speculative buckets", () => {
    const store = new EventStore(CAPACITY, T0);
    const spanId = "spn_replace";

    store.ingestOpenSpan(makeOpenSpanEvent(spanId, 1000, "claude-code"));
    expect(store.spans.size).toBe(1);
    expect(store.spans.get(spanId)!.tEndMs).toBeNull();

    // Query far ahead of the eventual real close — this lazily extends the
    // open span's bucket coverage well past where it will actually end.
    const farAheadMs = 1000 + BUCKET_MS * 10;
    store.spansOverlapping(1000, farAheadMs);
    const midflight = store.spans.get(spanId)!;
    const speculativeBucketEnd = midflight.bucketEnd;
    expect(speculativeBucketEnd).toBeGreaterThan(midflight.bucketStart);
    for (let b = midflight.bucketStart; b <= speculativeBucketEnd; b += 1) {
      expect(store.bucketsDebug().get(b)?.has(spanId)).toBe(true);
    }

    // The real close arrives, ending well before the speculative extension.
    const realEndMs = 1000 + 500; // same bucket as the start
    store.ingestFact(makeClosedSpanFact(spanId, 1000, realEndMs, realEndMs, "claude-code"));

    expect(store.spans.size).toBe(1); // replaced in place, never duplicated
    const closed = store.spans.get(spanId)!;
    expect(closed.tEndMs).toBe(realEndMs);
    const realBucketEnd = Math.floor((realEndMs - 1) / BUCKET_MS);
    expect(closed.bucketEnd).toBe(realBucketEnd);

    // Every speculative bucket beyond the real end must have been retracted.
    for (let b = realBucketEnd + 1; b <= speculativeBucketEnd; b += 1) {
      expect(store.bucketsDebug().get(b)?.has(spanId) ?? false, `stale speculative bucket ${b} not retracted`).toBe(false);
    }
    // The real bucket range must still hold it.
    for (let b = closed.bucketStart; b <= realBucketEnd; b += 1) {
      expect(store.bucketsDebug().get(b)?.has(spanId)).toBe(true);
    }
  });

  it("ring eviction of a closed span removes it from the span map and every one of its buckets", () => {
    const store = new EventStore(4, T0); // tiny capacity to force eviction deterministically
    store.ingestFact(makeClosedSpanFact("spn_a", 0, BUCKET_MS * 3, BUCKET_MS * 3, "claude-code"));
    const spanBucketStart = 0;
    const spanBucketEnd = 2;
    for (let b = spanBucketStart; b <= spanBucketEnd; b += 1) {
      expect(store.bucketsDebug().get(b)?.has("spn_a")).toBe(true);
    }

    // Fill past capacity with unrelated facts so spn_a's ring slot is evicted.
    for (let i = 0; i < 4; i += 1) {
      store.ingestFact(makeNonSpanFact("llm_call", BUCKET_MS * 3 + i, "otlp-cc"));
    }

    expect(store.spans.has("spn_a")).toBe(false);
    for (let b = spanBucketStart; b <= spanBucketEnd; b += 1) {
      expect(store.bucketsDebug().get(b)?.has("spn_a") ?? false).toBe(false);
    }
    expect(store.retained).toBe(4);
    expect(store.totalSeen).toBe(5);
  });

  it("batches rev via flush(), not per ingest", () => {
    const store = new EventStore(64, T0);
    expect(store.rev).toBe(0);
    store.ingestFact(makeNonSpanFact("session_meta", 0, "claude-code"));
    store.ingestFact(makeNonSpanFact("llm_call", 100, "claude-code"));
    store.ingestFact(makeClosedSpanFact("spn_x", 200, 300, 300, "claude-code"));
    expect(store.rev, "rev must not move before flush()").toBe(0);
    store.flush();
    expect(store.rev).toBe(1);
    store.flush(); // no changes since last flush — must not bump again
    expect(store.rev).toBe(1);
  });

  it("facts getter is cached between flushes (same reference) and rebuilt fresh after each flush", () => {
    const store = new EventStore(64, T0);
    store.ingestFact(makeNonSpanFact("llm_call", 0, "claude-code"));
    store.ingestFact(makeNonSpanFact("energy_sample", 100, "codecarbon-sampler"));

    const first = store.facts;
    const second = store.facts;
    expect(second, "two reads between flushes must return the exact same array reference").toBe(first);
    expect(second).toHaveLength(2);

    store.flush();
    const third = store.facts;
    expect(third, "a post-flush read must rebuild — a fresh reference, not the pre-flush cache").not.toBe(first);
    expect(third).toEqual(first); // same contents — only the reference changed

    // A second flush with nothing new to report must not invalidate again
    // needlessly, but the getter itself must still stay consistent.
    store.flush();
    const fourth = store.facts;
    expect(fourth).toBe(third); // no ingest happened since the last flush -> still cached

    store.ingestFact(makeNonSpanFact("process_sample", 200, "claude-code"));
    const fifth = store.facts;
    expect(fifth, "an ingest without an intervening flush does not itself invalidate the cache (selectors only read facts post-flush)").toBe(fourth);
    store.flush();
    const sixth = store.facts;
    expect(sixth).not.toBe(fifth);
    expect(sixth).toHaveLength(3);
  });

  it("facts cache invalidates across ring eviction: a post-eviction read is a fresh reference with the evicted fact genuinely absent", () => {
    const store = new EventStore(4, T0); // tiny capacity to force eviction deterministically
    const evicted = makeNonSpanFact("llm_call", 0, "claude-code");
    store.ingestFact(evicted);
    store.ingestFact(makeNonSpanFact("llm_call", 100, "claude-code"));
    store.ingestFact(makeNonSpanFact("llm_call", 200, "claude-code"));
    store.ingestFact(makeNonSpanFact("llm_call", 300, "claude-code"));
    store.flush();
    const beforeEviction = store.facts;
    expect(beforeEviction).toHaveLength(4);
    expect(beforeEviction.some((f) => f.event.event_id === evicted.event_id)).toBe(true);

    // Push the ring PAST capacity — the oldest fact (`evicted`) must be
    // evicted to make room.
    store.ingestFact(makeNonSpanFact("llm_call", 400, "claude-code"));
    store.flush();
    const afterEviction = store.facts;

    expect(afterEviction, "eviction must invalidate the memoised cache — a fresh reference, never the stale pre-eviction one").not.toBe(beforeEviction);
    expect(afterEviction).toHaveLength(4); // still capped at capacity, not grown
    expect(afterEviction.some((f) => f.event.event_id === evicted.event_id), "the evicted fact must be genuinely gone, not merely reordered").toBe(false);
  });

  it("decision `seq` is assigned once at ingest and survives ring eviction unrenumbered — retained rows keep their original seq/key", () => {
    const store = new EventStore(64, T0);
    for (let i = 0; i < EventStore.DECISION_LOG_CAP + 10; i += 1) {
      store.ingestDecision({ kind: "ingest", ts: iso(i), text: `d${i}` });
    }
    expect(store.decisions.length).toBe(EventStore.DECISION_LOG_CAP);
    // The oldest 10 decisions were shifted out of the ring — the retained
    // rows' `seq` must be their ORIGINAL ingest-order value (10..509), never
    // renumbered from the post-eviction array index (which would restart at
    // 0 and break DecisionRow's stable-key contract, selectors/timeline.ts's
    // `selectDecisionLog`, the moment a newer decision arrives and shifts
    // every row's index again).
    expect(store.decisions[0].seq).toBe(10);
    expect(store.decisions[0].text).toBe("d10");
    expect(store.decisions.at(-1)!.seq).toBe(EventStore.DECISION_LOG_CAP + 9);
    expect(store.decisions.at(-1)!.text).toBe(`d${EventStore.DECISION_LOG_CAP + 9}`);
    // Monotonic and unique across the whole retained window.
    for (let i = 1; i < store.decisions.length; i += 1) {
      expect(store.decisions[i].seq).toBeGreaterThan(store.decisions[i - 1].seq);
    }
  });

  it("decision log and reject list are capped rings (500 / 200)", () => {
    const store = new EventStore(64, T0);
    for (let i = 0; i < EventStore.DECISION_LOG_CAP + 10; i += 1) {
      store.ingestDecision({ kind: "ingest", ts: iso(i), text: `d${i}` });
    }
    expect(store.decisions.length).toBe(EventStore.DECISION_LOG_CAP);
    expect(store.decisions[0].text).toBe(`d${10}`); // oldest 10 evicted
    expect(store.decisions.at(-1)!.text).toBe(`d${EventStore.DECISION_LOG_CAP + 9}`);

    for (let i = 0; i < EventStore.REJECT_LIST_CAP + 5; i += 1) {
      store.ingestReject({ ts: iso(i), reason: `r${i}`, origin: "x", line: i, byte_offset: i, raw: "{}" });
    }
    expect(store.rejects.length).toBe(EventStore.REJECT_LIST_CAP);
    expect(store.rejects[0].reason).toBe("r5");
  });

  it("watchdog is a full-replacement array, not merged incrementally", () => {
    const store = new EventStore(64, T0);
    store.replaceWatchdog([{ pid: 1, span_id: "a", cmd: "x", cpu_pct: 1, rss_bytes: 1, state: "open" }]);
    expect(store.watchdog.length).toBe(1);
    store.replaceWatchdog([]);
    expect(store.watchdog.length).toBe(0);
  });

  it("caps concurrently-open spans (fix: unbounded growth) — oldest-opened is evicted first, buckets cleaned", () => {
    // A tiny openSpanCap (3rd ctor arg) so the flood is fast and the
    // expected eviction order is exact, not just "bounded eventually".
    const store = new EventStore(4000, T0, 4);
    for (let i = 0; i < 6; i += 1) {
      store.ingestOpenSpan(makeOpenSpanEvent(`o_${i}`, i * 100, "claude-code"));
    }

    expect(store.spans.size).toBe(4);
    expect(store.openSpanDropped).toBe(2);
    expect(store.spans.has("o_0")).toBe(false); // oldest-opened
    expect(store.spans.has("o_1")).toBe(false);
    expect(store.spans.has("o_2")).toBe(true);
    expect(store.spans.has("o_5")).toBe(true); // most recently opened

    // Cleaned from every bucket too, not just the map.
    for (const set of store.bucketsDebug().values()) {
      expect(set.has("o_0")).toBe(false);
      expect(set.has("o_1")).toBe(false);
    }
  });

  it("flooding open spans well past the cap never exceeds it, and totalSeen/retained (the fact-ring counters) stay untouched by open-span-only traffic", () => {
    const store = new EventStore(4000, T0, 8);
    for (let i = 0; i < 500; i += 1) {
      store.ingestOpenSpan(makeOpenSpanEvent(`flood_${i}`, i, "claude-code"));
    }
    expect(store.spans.size).toBe(8);
    expect(store.openSpanDropped).toBe(500 - 8);
    // ingestOpenSpan never touches the ring — these counters are specifically
    // about ingested `fact` frames (the footer's "showing N of M").
    expect(store.totalSeen).toBe(0);
    expect(store.retained).toBe(0);
  });

  it("closing an open span frees its slot in the open-span cap without counting as a drop", () => {
    const store = new EventStore(4000, T0, 2);
    store.ingestOpenSpan(makeOpenSpanEvent("a", 0, "claude-code"));
    store.ingestOpenSpan(makeOpenSpanEvent("b", 100, "claude-code"));
    expect(store.spans.size).toBe(2);

    store.ingestFact(makeClosedSpanFact("a", 0, 500, 500, "claude-code")); // closes "a"
    store.ingestOpenSpan(makeOpenSpanEvent("c", 200, "claude-code")); // must not evict "b" — "a" freed a slot

    expect(store.openSpanDropped).toBe(0);
    expect(store.spans.has("b")).toBe(true);
    expect(store.spans.has("c")).toBe(true);
    expect(store.spans.get("a")!.tEndMs).toBe(500); // "a" itself remains (now closed) — only its open-slot was freed
  });
});
