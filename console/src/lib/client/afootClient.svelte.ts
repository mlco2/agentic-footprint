// AfootClient (DATA-CONTRACT §3.1): owns fetch + EventSource. Connects in a
// strict order — status, session, then snapshot, then subscribe — so the chart is
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
import type {
  DebugReport,
  GapFrame,
  HealthPayload,
  OpenActionSpanEvent,
  PingFrame,
  Snapshot,
  SseEventName,
  SseFrame,
  SessionInfo,
  SessionSummary,
  WatcherStatus,
} from "../types/debug";
import { AllocStore, allocStore as defaultAllocStore } from "../stores/allocStore.svelte";
import { EventStore, eventStore as defaultEventStore } from "../stores/eventStore.svelte";
import { SessionStore, sessionStore as defaultSessionStore } from "../stores/sessionStore.svelte";
import { ReportStore, reportStore as defaultReportStore } from "../stores/reportStore.svelte";
import { HealthStore, healthStore as defaultHealthStore } from "../stores/healthStore.svelte";
import { StatusStore, statusStore as defaultStatusStore } from "../stores/statusStore.svelte";
import { UiStore, uiStore as defaultUiStore } from "../stores/uiStore.svelte";
import { boundFetch } from "./boundFetch";

export type AfootClientStatus = "connecting" | "live" | "reconnecting" | "offline";

/** The subset of the browser `EventSource` API AfootClient depends on —
 * narrowed so tests can inject a fake without implementing the whole DOM
 * interface. A real `EventSource` instance satisfies this structurally. */
export interface EventSourceLike {
  addEventListener(type: string, listener: (ev: { data: string; lastEventId?: string }) => void): void;
  close(): void;
}

export type EventSourceFactory = (url: string) => EventSourceLike;

/** What AfootClient needs to know about whether anyone is looking — narrowed
 * for the same reason as `EventSourceLike`. */
export interface VisibilityLike {
  isHidden(): boolean;
  /** Calls back when the tab becomes visible, the window regains focus, or
   * the browser reports the network is back. */
  onWake(cb: () => void): void;
}

/** The real thing: `document.hidden`, `visibilitychange`, `focus`, `online`.
 * Inert outside a browser (unit tests of other modules import this file). */
const browserVisibility: VisibilityLike = {
  isHidden: () => typeof document !== "undefined" && document.hidden,
  onWake(cb) {
    if (typeof document === "undefined" || typeof window === "undefined") return;
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) cb();
    });
    window.addEventListener("focus", cb);
    window.addEventListener("online", cb);
  },
};

export interface AfootClientDeps {
  eventStore?: EventStore;
  allocStore?: AllocStore;
  sessionStore?: SessionStore;
  reportStore?: ReportStore;
  healthStore?: HealthStore;
  statusStore?: StatusStore;
  uiStore?: UiStore;
  fetchImpl?: typeof fetch;
  createEventSource?: EventSourceFactory;
  visibility?: VisibilityLike;
}

const SSE_EVENT_NAMES: SseEventName[] = ["fact", "decision", "alloc", "reject", "gap", "watchdog", "report", "health", "session", "sessions", "reset", "status", "ping"];

const BACKOFF_MIN_MS = 500;
/** While the watcher is off this is the whole cost of an open console: one
 * refused loopback connection every 30s, and none at all while the tab is
 * hidden (see `scheduleReconnect`). */
const BACKOFF_MAX_MS = 30000;
/** Three missed server pings (one every 15s of silence). */
const HEARTBEAT_TIMEOUT_MS = 45000;
const OFFLINE_AFTER_CONSECUTIVE_FAILURES = 5;
const DEDUP_CAP = 8192;
const TICK_MS = 1000;

export class AfootClient {
  status = $state<AfootClientStatus>("connecting");
  /** Reactive so the masthead/footer can render "SSE paused · buffering (N)"
   * without polling. */
  pausedBuffered = $state(0);
  pausedDropped = $state(0);

  /** Sequence of the last frame applied or buffered — not reactive; no UI
   * surface needs it live, it's bookkeeping for reconnect/`?from=`. */
  lastSeq = 0;

  /** The watcher process this client last synced with (`/debug/status`'s
   * `boot_id`); `null` before the first status, or when the watcher has no
   * status route. A different one arriving later means a restart. */
  bootId: string | null = null;

  private readonly eventStoreRef: EventStore;
  private readonly allocStoreRef: AllocStore;
  private readonly sessionStoreRef: SessionStore;
  private readonly reportStoreRef: ReportStore;
  private readonly healthStoreRef: HealthStore;
  private readonly statusStoreRef: StatusStore;
  private readonly visibilityRef: VisibilityLike;
  private readonly uiStoreRef: UiStore;
  private readonly fetchImplRef: typeof fetch;
  private readonly createEventSource: EventSourceFactory;

  private es: EventSourceLike | null = null;
  private intervalId: ReturnType<typeof setInterval> | null = null;
  private tickerStarted = false;

  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private backoffMs = BACKOFF_MIN_MS;
  private consecutiveFailures = 0;
  /** Bumped by every `connect()`; an older attempt that finds it changed
   * after an `await` stops without touching anything. */
  private connectGen = 0;
  /** When the open stream last showed any sign of life (open, a frame, a
   * ping) — the heartbeat check compares against this. */
  private lastEventAtMs = 0;
  /** Whether the watcher behind the current attempt is known to ping — see
   * `checkHeartbeat`. */
  private heartbeatArmed = false;
  /** Whether the stores hold a snapshot-based sync with the process named
   * by `bootId`, so `lastSeq` is a cursor that process issued. Cleared by
   * a `reset`, a new `boot_id`, and until the first full sync completes. */
  private synced = false;
  private wakeRegistered = false;

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

  constructor(deps: AfootClientDeps = {}) {
    this.eventStoreRef = deps.eventStore ?? defaultEventStore;
    this.allocStoreRef = deps.allocStore ?? defaultAllocStore;
    this.sessionStoreRef = deps.sessionStore ?? defaultSessionStore;
    this.reportStoreRef = deps.reportStore ?? defaultReportStore;
    this.healthStoreRef = deps.healthStore ?? defaultHealthStore;
    this.statusStoreRef = deps.statusStore ?? defaultStatusStore;
    this.visibilityRef = deps.visibility ?? browserVisibility;
    this.uiStoreRef = deps.uiStore ?? defaultUiStore;
    this.fetchImplRef = deps.fetchImpl ?? boundFetch;
    this.createEventSource = deps.createEventSource ?? ((url: string) => new EventSource(url) as unknown as EventSourceLike);
  }

  // ---------------------------------------------------------------------
  // Connecting
  // ---------------------------------------------------------------------

  /** The ticker is started unconditionally, decoupled from whether the
   * first connection succeeds — `uiStore`'s clock (while live) should keep
   * advancing either way, and retries never need a second one
   * (`startTicker` is idempotent). */
  async start(): Promise<void> {
    this.status = "connecting";
    this.startTicker();
    if (!this.wakeRegistered) {
      this.wakeRegistered = true;
      this.visibilityRef.onWake(() => this.handleWake());
    }
    await this.connect();
  }

  /** The one way in: first load, every retry, a `reset`, a heartbeat
   * timeout and a new `boot_id` all come through here, so a cursor is
   * never reused without first asking which watcher process is answering.
   *
   * Strict order: status (the probe — which watcher is this?), then either
   * a resume from the last frame seen (same process, already in sync) or
   * the full sync: session, snapshot, subscribe. A thrown fetch (network down, watcher not up) is
   * exactly as much of a "not connected" fact as an SSE `error`, so it is
   * registered the same way and retried with backoff.
   *
   * `connectGen` makes a superseded attempt abandon itself: a wake or a
   * new `boot_id` can start a second `connect()` while the first is still
   * awaiting a fetch, and only the latest may touch the stores. */
  private async connect(): Promise<void> {
    const gen = ++this.connectGen;
    this.cancelReconnectTimer();
    this.closeEventSource();
    this.heartbeatArmed = false;
    try {
      const status = await this.fetchStatus();
      if (gen !== this.connectGen) return;
      // The same process this client is already in sync with: its cursor is
      // still that process's cursor, so resume from it. The server replays
      // everything after it — including the decision and reject frames a
      // snapshot does not carry — or answers `reset` when it no longer can.
      const sameProcess = status !== null && this.synced && status.boot_id === this.bootId;
      if (status) {
        // Another process than the one whose history is held: that
        // history cannot be merged with the new one by sequence number.
        // That includes a first boot id arriving after a sync with a
        // watcher that had none (an upgrade from one predating the status
        // route): whatever is held came from some other process.
        if (status.boot_id !== this.bootId && (this.bootId !== null || this.synced)) this.clearForNewBoot();
        this.bootId = status.boot_id;
        this.statusStoreRef.set(status);
        this.heartbeatArmed = true;
      } else {
        this.statusStoreRef.markUnavailable();
      }
      if (sameProcess) {
        this.subscribe(this.lastSeq);
        return;
      }
      this.synced = false;

      // `null` is "the watcher has no session yet", not a failure.
      const session = await this.fetchJson<SessionInfo | null>("/debug/session");
      if (gen !== this.connectGen) return;
      if (session) this.sessionStoreRef.set(session);

      const snapshot = await this.fetchJson<Snapshot>("/debug/snapshot?window=180s");
      if (gen !== this.connectGen) return;
      this.ingestSnapshot(snapshot);
      this.synced = true;

      this.subscribe(snapshot.as_of_seq);
      void this.fetchReportAndHealthOnce();
    } catch {
      if (gen === this.connectGen) this.registerFailureAndScheduleRetry();
    }
  }

  /** `GET /debug/status`. A network failure throws (the watcher is not
   * reachable, which fails the whole attempt). An HTTP error or a `null`
   * body means the watcher answered and has no status to give — an `afoot`
   * older than the route — and returns `null`: the console still works,
   * it just cannot show agents. */
  private async fetchStatus(): Promise<WatcherStatus | null> {
    const res = await this.fetchImplRef("/debug/status");
    if (!res.ok) return null;
    const body = (await res.json()) as WatcherStatus | null;
    return body && typeof body.boot_id === "string" ? body : null;
  }

  /** Everything held describes a watcher process that is gone. */
  private clearForNewBoot(): void {
    this.eventStoreRef.clear();
    this.allocStoreRef.clear();
    this.sessionStoreRef.clear();
    this.reportStoreRef.clear();
    this.healthStoreRef.clear();
    this.seenIds.clear();
    this.seenRing.fill(undefined);
    this.seenWriteIndex = 0;
    this.seenCount = 0;
    this.dropPauseBuffer();
    this.lastSeq = 0;
  }

  /** The real `afoot watch --debug` server pushes `report`/`health` frames per
   * ingest pass, not on subscribe (unlike the mock, whose `/debug/stream`
   * promptly resends the latest of each to every new connection) — so
   * without this, Impact/Health would sit on their empty states until the
   * next pass happens to run. Called once right after a (re)snapshot,
   * fire-and-forget: each fetch is independently non-fatal (a failure here
   * leaves that store exactly as empty as it already was; the next
   * `report`/`health` SSE frame fills it in normally), so this must never
   * feed into `connect()`'s own failure-and-retry path — it is not part of
   * "did we connect", it's a best-effort head start. A watcher with no
   * session yet answers `null` to both, which is left alone. */
  private async fetchReportAndHealthOnce(): Promise<void> {
    try {
      const report = await this.fetchJson<DebugReport | null>("/debug/report?level=session");
      if (report) this.reportStoreRef.set(report);
    } catch {
      // non-fatal — a `report` SSE frame will arrive on the next ingest pass
    }
    try {
      const health = await this.fetchJson<HealthPayload | null>("/debug/health");
      if (health) this.healthStoreRef.set(health);
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
        // `null`: the collector has not rebuilt this session since a console
        // attached. Asking is what makes it do so; the frames follow.
        const info = await this.fetchJson<SessionInfo | null>(`/debug/session?session_id=${encoded}`);
        if (info) this.sessionStoreRef.set(info);
      } catch {
        // non-fatal — the session frame republishes every pass
      }
      try {
        const report = await this.fetchJson<DebugReport | null>(
          `/debug/report?level=session&session_id=${encoded}`,
        );
        if (report) this.reportStoreRef.set(report);
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
    for (const gap of snapshot.coverage_gaps) this.ingestGapOnce(gap);
    this.eventStoreRef.replaceWatchdog(snapshot.watchdog);
    for (const alloc of snapshot.allocations) {
      if (this.markSeen(alloc.sample_event_id)) this.allocStoreRef.ingest(alloc);
    }
    this.lastSeq = snapshot.as_of_seq;
  }

  /** A coverage gap carries no id, and every reconnect re-reads the last
   * 180s of them from the snapshot: without this the same gap is appended
   * once per reconnect. Its own content is its identity. */
  private ingestGapOnce(gap: GapFrame): void {
    if (this.markSeen(`gap:${gap.collector}:${gap.t_start}:${gap.t_end}:${gap.reason}`)) this.eventStoreRef.ingestGap(gap);
  }

  // ---------------------------------------------------------------------
  // SSE subscription, liveness and retries
  // ---------------------------------------------------------------------

  private subscribe(fromSeq: number): void {
    const es = this.createEventSource(`/debug/stream?from=${fromSeq}`);
    this.es = es;
    // The heartbeat clock starts with the attempt, not with the first
    // event: a connection that is accepted and then says nothing at all is
    // as dead as one that went quiet later.
    this.lastEventAtMs = Date.now();
    for (const name of SSE_EVENT_NAMES) {
      es.addEventListener(name, (ev) => {
        if (this.es === es) this.handleFrame(name, ev);
      });
    }
    es.addEventListener("open", () => {
      if (this.es === es) this.onTransportAlive();
    });
    es.addEventListener("error", () => {
      if (this.es === es) this.handleError();
    });
  }

  private closeEventSource(): void {
    if (this.es) {
      this.es.close();
      this.es = null;
    }
  }

  /** Closed at once, not at retry time: a browser `EventSource` left open
   * after an error reconnects by itself, to the old cursor, behind this
   * client's back. */
  private handleError(): void {
    this.closeEventSource();
    this.registerFailureAndScheduleRetry();
  }

  /** Single source of truth for "the transport just failed" — an SSE
   * `error`, a failed `connect()` and a heartbeat timeout alike, so all
   * three count against the same consecutive-failure/backoff sequence and
   * land on the same `reconnecting`/`offline` status rule (N=5). */
  private registerFailureAndScheduleRetry(): void {
    this.consecutiveFailures += 1;
    this.status = this.consecutiveFailures >= OFFLINE_AFTER_CONSECUTIVE_FAILURES ? "offline" : "reconnecting";
    this.scheduleReconnect();
  }

  /** No timer while the tab is hidden: a console nobody is looking at does
   * not poll a watcher that is off. `handleWake` makes the attempt the
   * moment the tab is looked at again. */
  private scheduleReconnect(): void {
    if (this.reconnectTimer !== null) return;
    if (this.visibilityRef.isHidden()) return;
    const delay = this.backoffMs;
    this.backoffMs = Math.min(this.backoffMs * 2, BACKOFF_MAX_MS);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      void this.connect();
    }, delay);
  }

  private cancelReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  /** The tab became visible, the window got focus, or the network came
   * back. A stream that is open and has been heard from recently is left
   * alone; anything else gets one attempt now, with the backoff restarted
   * — whatever delay had built up was earned while nobody was waiting.
   *
   * This is also the laptop-wakes-from-sleep path: timers did not run
   * while it slept, so the heartbeat is stale, and the watcher may have
   * been restarted meanwhile. One full re-sync covers both. */
  private handleWake(): void {
    if (this.streamHealthy()) return;
    this.backoffMs = BACKOFF_MIN_MS;
    if (this.status === "live") this.status = "reconnecting";
    void this.connect();
  }

  /** Called from the 1 Hz tick: a stream that has said nothing — no frame,
   * no `ping` — for [HEARTBEAT_TIMEOUT_MS] is treated as lost. The server
   * pings every 15s of silence, so this is three missed in a row.
   *
   * Only for a watcher known to ping (`heartbeatArmed`: it served a status,
   * or sent a `status`/`ping` frame). One that predates the status route
   * is legitimately silent for a whole flush interval, and timing it out
   * would mean dropping and re-syncing a healthy connection every 45s. */
  private checkHeartbeat(): void {
    if (this.es !== null && !this.streamHealthy()) this.handleError();
  }

  /** A stream exists and — for a watcher that pings — has been heard from
   * within the heartbeat timeout. */
  private streamHealthy(): boolean {
    return this.es !== null && (!this.heartbeatArmed || Date.now() - this.lastEventAtMs < HEARTBEAT_TIMEOUT_MS);
  }

  /** The stream opened, or delivered something: the transport works. */
  private onTransportAlive(): void {
    this.lastEventAtMs = Date.now();
    this.consecutiveFailures = 0;
    this.backoffMs = BACKOFF_MIN_MS;
    this.status = "live";
  }

  // ---------------------------------------------------------------------
  // Frame handling
  // ---------------------------------------------------------------------

  private handleFrame(name: SseEventName, ev: { data: string; lastEventId?: string }): void {
    this.onTransportAlive();

    // Protocol frames: not ring frames (no id), never buffered by a pause —
    // they are about the connection, not about the session being watched.
    if (name === "status" || name === "ping") {
      this.handleProtocolFrame(name, ev.data);
      return;
    }

    if (ev.lastEventId) {
      const seq = Number(ev.lastEventId);
      if (Number.isFinite(seq)) this.lastSeq = seq;
    }

    if (name === "reset") {
      // If frames were buffered while paused, they're from the stream
      // generation that's about to be torn down by `connect()` — the
      // fresh re-snapshot it fetches supersedes them entirely. Replaying
      // them on a later resume would silently reintroduce stale data on top
      // of (or racing) the fresh snapshot, so they're dropped here, exactly
      // like any other buffer overflow: surfaced honestly through
      // `pausedDropped`, never silently discarded.
      if (!this.uiStoreRef.live) this.dropPauseBuffer();
      // The cursor cannot be served: only a fresh snapshot will do.
      this.synced = false;
      void this.connect();
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

  /** `status` replaces the watcher status; both it and `ping` carry the
   * `boot_id`, and one that differs from the process this client
   * bootstrapped from means a full re-sync (`connect()` does the clearing
   * once the new status confirms it). */
  private handleProtocolFrame(name: "status" | "ping", raw: string): void {
    let data: WatcherStatus | PingFrame;
    try {
      data = JSON.parse(raw);
    } catch {
      return;
    }
    this.heartbeatArmed = true;
    const bootId = typeof data?.boot_id === "string" ? data.boot_id : null;
    if (bootId !== null && this.bootId !== null && bootId !== this.bootId) {
      void this.connect();
      return;
    }
    if (name === "status" && bootId !== null) {
      this.bootId = bootId;
      this.statusStoreRef.set(data as WatcherStatus);
    }
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
        this.ingestGapOnce(frame.data);
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
      case "sessions":
        if (Array.isArray(frame.data)) this.sessionStoreRef.setSummaries(frame.data);
        return;
      case "reset":
      case "status":
      case "ping":
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
    this.checkHeartbeat();
    this.uiStoreRef.tick();
    this.drainPauseBufferIfLive();
    this.eventStoreRef.flush();
    this.allocStoreRef.flush();
  }

  /** Test/teardown hygiene: stops the interval and any pending reconnect,
   * closes the EventSource. Not part of the app's normal lifecycle (the
   * client lives for the page's whole life). */
  dispose(): void {
    this.connectGen += 1; // an attempt still awaiting a fetch abandons itself
    this.closeEventSource();
    this.cancelReconnectTimer();
    if (this.intervalId !== null) {
      clearInterval(this.intervalId);
      this.intervalId = null;
    }
  }
}

export const afootClient = new AfootClient();
