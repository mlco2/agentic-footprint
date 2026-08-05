// AfClient (DATA-CONTRACT §3.1): owns fetch + EventSource. Bootstraps in a
// strict order — session, then snapshot, then subscribe — so the chart is
// never shown empty while a live stream races ahead of the backfill it
// needs. Also the one place in the app allowed to run `setInterval`
// (architecture rule: exactly one, 1 Hz, in the whole app): it drives both
// `uiStore`'s clock tick and the batched `flush()` that bumps each bulk
// store's `rev`.
//
// Interface note for the real control-plane server (not just this mock):
// `EventSource` cannot set a `Last-Event-ID` request header on its *first*
// connect — only the browser's own automatic reconnects do that. So the
// snapshot's `as_of_seq` is instead passed as the query param `?from=` on
// every `/debug/stream` open (first connect and every manual reconnect
// alike); the real server must honour `?from=` the same way it honours
// `Last-Event-ID`, or a client's first connection after a snapshot will
// replay from the wrong point.
import type { FactEvent } from "../types/contract1";
import type { DebugReport, HealthPayload, OpenActionSpanEvent, Snapshot, SseEventName, SseFrame, SessionInfo, SessionSummary } from "../types/debug";
import { AllocStore, allocStore as defaultAllocStore } from "../stores/allocStore.svelte";
import { EventStore, eventStore as defaultEventStore } from "../stores/eventStore.svelte";
import { SessionStore, sessionStore as defaultSessionStore } from "../stores/sessionStore.svelte";
import { ReportStore, reportStore as defaultReportStore } from "../stores/reportStore.svelte";
import { HealthStore, healthStore as defaultHealthStore } from "../stores/healthStore.svelte";
import { UiStore, uiStore as defaultUiStore } from "../stores/uiStore.svelte";
import { boundFetch } from "./boundFetch";

export type AfClientStatus = "connecting" | "live" | "reconnecting" | "offline";

/** The subset of the browser `EventSource` API AfClient depends on —
 * narrowed so tests can inject a fake without implementing the whole DOM
 * interface. A real `EventSource` instance satisfies this structurally. */
export interface EventSourceLike {
  addEventListener(type: string, listener: (ev: { data: string; lastEventId?: string }) => void): void;
  close(): void;
}

export type EventSourceFactory = (url: string) => EventSourceLike;

export interface AfClientDeps {
  eventStore?: EventStore;
  allocStore?: AllocStore;
  sessionStore?: SessionStore;
  reportStore?: ReportStore;
  healthStore?: HealthStore;
  uiStore?: UiStore;
  fetchImpl?: typeof fetch;
  createEventSource?: EventSourceFactory;
}

const SSE_EVENT_NAMES: SseEventName[] = ["fact", "decision", "alloc", "reject", "gap", "watchdog", "report", "health", "session", "reset"];

const BACKOFF_MIN_MS = 500;
const BACKOFF_MAX_MS = 8000;
const OFFLINE_AFTER_CONSECUTIVE_FAILURES = 5;
const DEDUP_CAP = 8192;
const TICK_MS = 1000;

export class AfClient {
  status = $state<AfClientStatus>("connecting");
  /** Reactive so the masthead/footer can render "SSE paused · buffering (N)"
   * without polling. */
  pausedBuffered = $state(0);
  pausedDropped = $state(0);

  /** Sequence of the last frame applied or buffered — not reactive; no UI
   * surface needs it live, it's bookkeeping for reconnect/`?from=`. */
  lastSeq = 0;

  private readonly eventStoreRef: EventStore;
  private readonly allocStoreRef: AllocStore;
  private readonly sessionStoreRef: SessionStore;
  private readonly reportStoreRef: ReportStore;
  private readonly healthStoreRef: HealthStore;
  private readonly uiStoreRef: UiStore;
  private readonly fetchImplRef: typeof fetch;
  private readonly createEventSource: EventSourceFactory;

  private es: EventSourceLike | null = null;
  private intervalId: ReturnType<typeof setInterval> | null = null;
  private tickerStarted = false;

  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private backoffMs = BACKOFF_MIN_MS;
  private consecutiveFailures = 0;

  /** FIFO of frames received while `uiStore.live === false`. The SSE
   * connection itself stays open while paused — pausing only affects
   * whether frames are applied immediately or held. */
  private pauseBuffer: SseFrame[] = [];

  /** Bounded dedup set for `event_id`/`sample_event_id`, so a `fact`/`alloc`
   * present in both the snapshot and the overlapping stream tail is only
   * ingested once. Eviction is a fixed-size ring (array + write index),
   * mirroring EventStore's own fact ring — O(1) per insert, unlike an
   * `Array.shift()` FIFO (O(n), and at DEDUP_CAP=8192 entries, on every
   * single ingested id). */
  private seenIds = new Set<string>();
  private readonly seenRing: (string | undefined)[] = new Array(DEDUP_CAP);
  private seenWriteIndex = 0;
  private seenCount = 0;

  constructor(deps: AfClientDeps = {}) {
    this.eventStoreRef = deps.eventStore ?? defaultEventStore;
    this.allocStoreRef = deps.allocStore ?? defaultAllocStore;
    this.sessionStoreRef = deps.sessionStore ?? defaultSessionStore;
    this.reportStoreRef = deps.reportStore ?? defaultReportStore;
    this.healthStoreRef = deps.healthStore ?? defaultHealthStore;
    this.uiStoreRef = deps.uiStore ?? defaultUiStore;
    this.fetchImplRef = deps.fetchImpl ?? boundFetch;
    this.createEventSource = deps.createEventSource ?? ((url: string) => new EventSource(url) as unknown as EventSourceLike);
  }

  // ---------------------------------------------------------------------
  // Bootstrap
  // ---------------------------------------------------------------------

  /** Strict order: session -> snapshot -> subscribe. The ticker is started
   * unconditionally, decoupled from bootstrap success/failure — `uiStore`'s
   * clock (while live) should keep advancing even if the initial connection
   * hasn't succeeded yet, and bootstrap retries never need a second one
   * (`startTicker` is idempotent). */
  async start(): Promise<void> {
    this.status = "connecting";
    this.startTicker();
    await this.bootstrap();
  }

  /** Session -> snapshot -> subscribe, but never left to fail silently: a
   * thrown fetch (network down, server not up yet, ...) is exactly as much
   * of a "not connected" fact as an SSE `error` event, so it's registered
   * the same way — status moves to `reconnecting`/`offline` (never left
   * standing at `connecting`, and never an unhandled rejection from the
   * `void this.bootstrap()` retry path) and a backoff retry is scheduled
   * that re-runs this whole method. */
  private async bootstrap(): Promise<void> {
    try {
      const session = await this.fetchJson<SessionInfo>("/debug/session");
      this.sessionStoreRef.set(session);

      const snapshot = await this.fetchJson<Snapshot>("/debug/snapshot?window=180s");
      this.ingestSnapshot(snapshot);

      this.subscribe(snapshot.as_of_seq);
      void this.fetchReportAndHealthOnce();
    } catch {
      this.registerFailureAndScheduleRetry(() => void this.bootstrap());
    }
  }

  /** The real `af watch --debug` server pushes `report`/`health` frames per
   * ingest pass, not on subscribe (unlike the mock, whose `/debug/stream`
   * promptly resends the latest of each to every new connection) — so
   * without this, Impact/Health would sit on their empty states until the
   * next pass happens to run. Called once right after a (re)snapshot,
   * fire-and-forget: each fetch is independently non-fatal (a failure here
   * leaves that store exactly as empty as it already was; the next
   * `report`/`health` SSE frame fills it in normally), so this must never
   * feed into `bootstrap()`/`rebootstrap()`'s own failure-and-retry path —
   * it is not part of "did we connect", it's a best-effort head start. */
  private async fetchReportAndHealthOnce(): Promise<void> {
    try {
      const report = await this.fetchJson<DebugReport>("/debug/report?level=session");
      this.reportStoreRef.set(report);
    } catch {
      // non-fatal — a `report` SSE frame will arrive on the next ingest pass
    }
    try {
      const health = await this.fetchJson<HealthPayload>("/debug/health");
      this.healthStoreRef.set(health);
    } catch {
      // non-fatal — a `health` SSE frame will arrive on the next ingest pass
    }
    try {
      // Seeds the picker so it doesn't sit empty until the first `session`
      // frames arrive. Older servers (and the mock) have no /debug/sessions
      // — that's exactly as non-fatal as the two fetches above.
      const sessions = await this.fetchJson<SessionSummary[]>("/debug/sessions");
      if (Array.isArray(sessions)) this.sessionStoreRef.setSummaries(sessions);
    } catch {
      // non-fatal — `session` SSE frames fill the picker as passes run
    }
  }

  /** The picker's entry point: pin (or unpin with `null`) and, best-effort,
   * fetch what the newly selected session is missing — its full info and
   * its session-level report. Both non-fatal: the next pass's `session`/
   * `report` frames deliver the same data. */
  selectSession(sessionId: string | null): void {
    this.sessionStoreRef.pin(sessionId);
    if (sessionId === null) return;
    const encoded = encodeURIComponent(sessionId);
    void (async () => {
      try {
        const info = await this.fetchJson<SessionInfo>(`/debug/session?session_id=${encoded}`);
        this.sessionStoreRef.set(info);
      } catch {
        // non-fatal — the session frame republishes every pass
      }
      try {
        const report = await this.fetchJson<DebugReport>(
          `/debug/report?level=session&session_id=${encoded}`,
        );
        this.reportStoreRef.set(report);
      } catch {
        // non-fatal — the report frame republishes every pass
      }
    })();
  }

  private async fetchJson<T>(path: string): Promise<T> {
    const res = await this.fetchImplRef(path);
    if (!res.ok) throw new Error(`GET ${path} failed: ${res.status}`);
    return (await res.json()) as T;
  }

  private ingestSnapshot(snapshot: Snapshot): void {
    for (const event of snapshot.events) {
      if (this.markSeen(event.event_id)) this.eventStoreRef.ingestFact(event);
    }
    for (const open of snapshot.open_spans) {
      this.eventStoreRef.ingestOpenSpan(open as OpenActionSpanEvent);
    }
    for (const gap of snapshot.coverage_gaps) {
      this.eventStoreRef.ingestGap(gap);
    }
    this.eventStoreRef.replaceWatchdog(snapshot.watchdog);
    for (const alloc of snapshot.allocations) {
      if (this.markSeen(alloc.sample_event_id)) this.allocStoreRef.ingest(alloc);
    }
    this.lastSeq = snapshot.as_of_seq;
  }

  /** `event: reset` — the server's Last-Event-ID was too old to replay from.
   * Full re-bootstrap: re-snapshot, then resubscribe from the new
   * `as_of_seq`. Receiving the `reset` frame itself is real evidence the
   * transport just worked (handled by `onFrameReceived` before this is
   * called), but `closeEventSource()` below tears that connection down —
   * if the follow-up snapshot fetch then fails, the client is genuinely
   * disconnected and must not go on showing a `live` dot (global-constraints
   * #6: never stale-but-plausible). So this is guarded exactly like
   * `bootstrap()`: a failure registers as a connection failure and retries
   * with backoff, re-running this same method. */
  private async rebootstrap(): Promise<void> {
    this.closeEventSource();
    try {
      const snapshot = await this.fetchJson<Snapshot>("/debug/snapshot?window=180s");
      this.ingestSnapshot(snapshot);
      this.subscribe(snapshot.as_of_seq);
      void this.fetchReportAndHealthOnce();
    } catch {
      this.registerFailureAndScheduleRetry(() => void this.rebootstrap());
    }
  }

  // ---------------------------------------------------------------------
  // SSE subscription + reconnect
  // ---------------------------------------------------------------------

  private subscribe(fromSeq: number): void {
    const es = this.createEventSource(`/debug/stream?from=${fromSeq}`);
    this.es = es;
    for (const name of SSE_EVENT_NAMES) {
      es.addEventListener(name, (ev) => this.handleFrame(name, ev));
    }
    es.addEventListener("error", () => this.handleError());
  }

  private closeEventSource(): void {
    if (this.es) {
      this.es.close();
      this.es = null;
    }
  }

  private handleError(): void {
    this.registerFailureAndScheduleRetry(() => {
      this.closeEventSource();
      this.subscribe(this.lastSeq);
    });
  }

  /** Single source of truth for "the transport just failed" — used by the
   * SSE `error` listener, a failed initial bootstrap, and a failed
   * post-`reset` re-bootstrap alike, so all three count against the same
   * consecutive-failure/backoff sequence and land on the same
   * `reconnecting`/`offline` status rule (N=5). `retry` is whatever redoing
   * the failed step means for that caller (resubscribe vs. re-bootstrap). */
  private registerFailureAndScheduleRetry(retry: () => void): void {
    this.consecutiveFailures += 1;
    this.status = this.consecutiveFailures >= OFFLINE_AFTER_CONSECUTIVE_FAILURES ? "offline" : "reconnecting";
    this.scheduleReconnect(retry);
  }

  private scheduleReconnect(retry: () => void): void {
    if (this.reconnectTimer !== null) return;
    const delay = this.backoffMs;
    this.backoffMs = Math.min(this.backoffMs * 2, BACKOFF_MAX_MS);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      retry();
    }, delay);
  }

  private onFrameReceived(): void {
    this.consecutiveFailures = 0;
    this.backoffMs = BACKOFF_MIN_MS;
    if (this.status !== "live") this.status = "live";
  }

  // ---------------------------------------------------------------------
  // Frame handling
  // ---------------------------------------------------------------------

  private handleFrame(name: SseEventName, ev: { data: string; lastEventId?: string }): void {
    this.onFrameReceived();

    if (ev.lastEventId) {
      const seq = Number(ev.lastEventId);
      if (Number.isFinite(seq)) this.lastSeq = seq;
    }

    if (name === "reset") {
      // If frames were buffered while paused, they're from the stream
      // generation that's about to be torn down by `rebootstrap()` — the
      // fresh re-snapshot it fetches supersedes them entirely. Replaying
      // them on a later resume would silently reintroduce stale data on top
      // of (or racing) the fresh snapshot, so they're dropped here, exactly
      // like any other buffer overflow: surfaced honestly through
      // `pausedDropped`, never silently discarded.
      if (!this.uiStoreRef.live) this.dropPauseBuffer();
      void this.rebootstrap();
      return;
    }

    let data: unknown;
    try {
      data = JSON.parse(ev.data);
    } catch {
      return; // malformed frame — drop rather than crash the client
    }
    const frame = { event: name, data } as SseFrame;

    if (!this.uiStoreRef.live) {
      this.bufferFrame(frame);
      return;
    }
    this.applyFrame(frame);
  }

  /** Dispatch by SSE event name (DATA-CONTRACT §2.3 table). */
  private applyFrame(frame: SseFrame): void {
    switch (frame.event) {
      case "fact":
        if (this.markSeen(frame.data.event_id)) this.eventStoreRef.ingestFact(frame.data as FactEvent);
        return;
      case "alloc":
        if (this.markSeen(frame.data.sample_event_id)) this.allocStoreRef.ingest(frame.data);
        return;
      case "decision":
        this.eventStoreRef.ingestDecision(frame.data);
        return;
      case "reject":
        this.eventStoreRef.ingestReject(frame.data);
        return;
      case "gap":
        this.eventStoreRef.ingestGap(frame.data);
        return;
      case "watchdog":
        this.eventStoreRef.replaceWatchdog(frame.data.pids);
        return;
      case "report":
        this.reportStoreRef.set(frame.data);
        return;
      case "health":
        this.healthStoreRef.set(frame.data);
        return;
      case "session":
        this.sessionStoreRef.set(frame.data);
        return;
      case "reset":
        return; // handled in handleFrame before reaching applyFrame
    }
  }

  private markSeen(id: string): boolean {
    if (this.seenIds.has(id)) return false;
    this.seenIds.add(id);
    if (this.seenCount === DEDUP_CAP) {
      // Ring is full — evict the slot about to be overwritten before
      // reusing it, same eviction-before-overwrite shape as EventStore's
      // `evictSlotAt`.
      const evicted = this.seenRing[this.seenWriteIndex];
      if (evicted !== undefined) this.seenIds.delete(evicted);
    } else {
      this.seenCount += 1;
    }
    this.seenRing[this.seenWriteIndex] = id;
    this.seenWriteIndex = (this.seenWriteIndex + 1) % DEDUP_CAP;
    return true;
  }

  // ---------------------------------------------------------------------
  // Pause buffering
  // ---------------------------------------------------------------------

  private bufferFrame(frame: SseFrame): void {
    const cap = this.eventStoreRef.capacity;
    if (this.pauseBuffer.length >= cap) {
      this.pauseBuffer.shift();
      this.pausedDropped += 1;
    }
    this.pauseBuffer.push(frame);
    this.pausedBuffered = this.pauseBuffer.length;
  }

  /** Discards the whole pause buffer at once (as opposed to `bufferFrame`'s
   * one-at-a-time overflow drop) — used on `event: reset` while paused, see
   * `handleFrame`. Every dropped frame counts against `pausedDropped`, same
   * honesty contract as a capacity overflow. */
  private dropPauseBuffer(): void {
    if (this.pauseBuffer.length === 0) return;
    this.pausedDropped += this.pauseBuffer.length;
    this.pauseBuffer = [];
    this.pausedBuffered = 0;
  }

  /** Drains the pause buffer through the normal apply path, in order, and
   * closes the books on the whole pause episode — resets both
   * `pausedBuffered` and `pausedDropped` — called from every `tick()` while
   * `uiStore.live` is true (picks up a resume within one tick).
   *
   * Deliberately unconditional on `pauseBuffer.length`, not gated behind
   * "something is buffered": before `dropPauseBuffer()` existed, "a drop
   * happened" and "the buffer is non-empty" were the same fact, so gating
   * the reset on a non-empty buffer was equivalent to gating it on "there's
   * something to report." `dropPauseBuffer()` (reset-while-paused, see
   * `handleFrame`) broke that equivalence: it can leave `pausedDropped > 0`
   * with an already-empty buffer, and a length-gated reset would then never
   * run, leaving the counter elevated indefinitely until some unrelated
   * future pause cycle happened to touch it. */
  private drainPauseBufferIfLive(): void {
    if (!this.uiStoreRef.live) return;
    const toApply = this.pauseBuffer;
    this.pauseBuffer = [];
    this.pausedBuffered = 0;
    this.pausedDropped = 0;
    for (const frame of toApply) this.applyFrame(frame);
  }

  // ---------------------------------------------------------------------
  // The single 1 Hz interval
  // ---------------------------------------------------------------------

  private startTicker(): void {
    if (this.tickerStarted) return;
    this.tickerStarted = true;
    this.tick(); // immediate first tick — don't leave nowMs/flush stale for up to a second
    this.intervalId = setInterval(() => this.tick(), TICK_MS);
  }

  /** Public so tests can drive it deterministically instead of waiting on a
   * real 1 Hz timer; production code only ever reaches this via the
   * interval started in `start()`.
   *
   * Drain-then-flush, in that order: draining applies buffered frames by
   * calling straight into `ingestFact`/etc, which only flips each store's
   * internal `dirty` flag — `flush()` is what turns `dirty` into a `rev`
   * bump. Flushing first would mean a resume's data doesn't become
   * `rev`-visible until the *next* tick (up to another full second later),
   * since `dirty` gets set again only after that tick's flush already ran. */
  tick(): void {
    this.uiStoreRef.tick();
    this.drainPauseBufferIfLive();
    this.eventStoreRef.flush();
    this.allocStoreRef.flush();
  }

  /** Test/teardown hygiene: stops the interval and any pending reconnect,
   * closes the EventSource. Not part of the app's normal lifecycle (the
   * client lives for the page's whole life). */
  dispose(): void {
    this.closeEventSource();
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.intervalId !== null) {
      clearInterval(this.intervalId);
      this.intervalId = null;
    }
  }
}

export const afClient = new AfClient();
