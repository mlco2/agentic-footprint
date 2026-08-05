// Dev-only mock of the af `/debug/*` HTTP+SSE surface (DATA-CONTRACT §2).
// Wired into both `configureServer` (vite dev) and `configurePreviewServer`
// (vite preview) in vite.config.ts, so `npm run dev` and `npm run preview`
// both serve it. The real Rust control-plane endpoints arrive later from
// another workstream — until then this mock IS the executable contract, and
// console/src/ code is written against it. Nothing under console/dev/ (this
// file included) may ever be imported by console/src/.
//
// Chaos toggles (dev-only, never present in a real af server):
//   POST /debug/__mock/drop-sse — closes every currently-open SSE connection once.
//   POST /debug/__mock/stall    — stop emitting frames for 10s without closing any connection.
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Connect, Plugin } from "vite";
import { buildScenario } from "./scenario";
import type { FactEvent } from "../src/lib/types/contract1";
import type {
  AllocationTrace,
  DebugReport,
  GapFrame,
  HealthPayload,
  OpenActionSpanEvent,
  SessionInfo,
  Snapshot,
  SseEventName,
  SseFrame,
  SseFrameDataMap,
  WatchdogEntry,
  WatchdogFrame,
} from "../src/lib/types/debug";

/** DATA-CONTRACT §2.3's `watchdog` SSE frame wraps the list as
 * `{ pids: [...] }`; §2.2's `Snapshot.watchdog` is a bare `WatchdogEntry[]`.
 * This is the one place that translates between the two shapes, so the
 * doc's own asymmetry lives in a single, testable spot rather than being
 * re-derived at each call site. */
export function toSnapshotWatchdog(frame: WatchdogFrame | undefined): WatchdogEntry[] {
  return frame?.pids ?? [];
}

// This is the one place in the console/dev/ mock allowed to read the wall
// clock: it anchors "scenario atMs 0" to the moment this module first loads
// (i.e. server start), per the brief. scenario.ts itself never does this.
const SERVER_START_WALL_MS = Date.now();

const rawScenario = buildScenario();
// dev/fixtures/session.json's `t_start` is a fixed, historical date (scenario.ts
// stays deterministic on purpose — see its own doc comment). Left as-is, every
// ISO timestamp baked into the built scenario (fact/decision/gap/alloc/watchdog
// dates) would stay pinned to that fixed date forever, regardless of when the
// dev server actually runs — invisible to any wall-clock-windowed consumer.
// Task 5's Timeline is the first one (trailing-180s-from-`nowMs` window):
// without this rebase, nothing the mock serves ever falls inside that window.
// `shiftDates` performs the "wall-clock shifting... happens only in
// mock-plugin.ts" this file's own comment already promised, by translating
// every embedded ISO date string by the same delta, uniformly, so every
// relative interval (durations, overlaps, the gap's own span) is preserved
// exactly — only the absolute epoch moves.
const FIXTURE_T0_MS = Date.parse(rawScenario.session.t_start);
const SHIFT_MS = SERVER_START_WALL_MS - FIXTURE_T0_MS;

function isIsoDateString(value: string): boolean {
  return /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/.test(value);
}

function shiftDates<T>(value: T): T {
  if (typeof value === "string") {
    return (isIsoDateString(value) ? new Date(Date.parse(value) + SHIFT_MS).toISOString() : value) as T;
  }
  if (Array.isArray(value)) return value.map((v) => shiftDates(v)) as T;
  if (value instanceof Map) {
    const out = new Map();
    for (const [k, v] of value) out.set(k, shiftDates(v));
    return out as T;
  }
  if (value !== null && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) out[k] = shiftDates(v);
    return out as T;
  }
  return value;
}

const scenario = shiftDates(rawScenario);
const frames = scenario.frames;
const T0_MS = Date.parse(scenario.session.t_start);
// One full pass through the fixture's frames, in scenario-relative ms. The
// last frame is the health tick at TOTAL_MS in scenario.ts; pad slightly so
// the loop doesn't restart mid-tie with the very last frame's timestamp.
const LOOP_MS = frames[frames.length - 1].atMs + 1000;

function virtualNowMs(): number {
  return Date.now() - SERVER_START_WALL_MS;
}

function loopIndexOf(virtualMs: number): number {
  return Math.floor(virtualMs / LOOP_MS);
}

function withinLoopMs(virtualMs: number): number {
  return virtualMs - loopIndexOf(virtualMs) * LOOP_MS;
}

/** Index of the last frame with atMs <= target, or -1. Hand-rolled instead of
 * Array#findLastIndex to avoid bumping the shared tsconfig's lib target
 * beyond ES2022 for one call site. */
function lastIndexAtOrBefore(target: number): number {
  for (let i = frames.length - 1; i >= 0; i -= 1) {
    if (frames[i].atMs <= target) return i;
  }
  return -1;
}

/** Global seq for "the last frame at or before this virtual ms". frames[0].atMs
 * is always 0, so this is always >= 0 once any time has elapsed. */
function currentSeq(virtualMs: number): number {
  const loop = loopIndexOf(virtualMs);
  const withinLoop = withinLoopMs(virtualMs);
  return loop * frames.length + Math.max(lastIndexAtOrBefore(withinLoop), 0);
}

function frameForSeq(seq: number): SseFrame {
  const idx = ((seq % frames.length) + frames.length) % frames.length;
  return frames[idx];
}

/** Frames whose virtual delivery time falls in (sinceSeq, uptoSeq], ascending. */
function framesBetween(sinceSeqExclusive: number, uptoSeqInclusive: number): Array<{ seq: number; frame: SseFrame }> {
  const out: Array<{ seq: number; frame: SseFrame }> = [];
  for (let seq = sinceSeqExclusive + 1; seq <= uptoSeqInclusive; seq += 1) {
    out.push({ seq, frame: frameForSeq(seq) });
  }
  return out;
}

/** Frames whose virtual delivery time falls in [uptoVirtualMs - windowMs, uptoVirtualMs]. */
function framesInWindow(uptoVirtualMs: number, windowMs: number): Array<{ seq: number; frame: SseFrame }> {
  const lower = Math.max(0, uptoVirtualMs - windowMs);
  const uptoLoop = loopIndexOf(uptoVirtualMs);
  const lowerLoop = loopIndexOf(lower);
  const out: Array<{ seq: number; frame: SseFrame }> = [];
  for (let loop = lowerLoop; loop <= uptoLoop; loop += 1) {
    for (let idx = 0; idx < frames.length; idx += 1) {
      const virtualAtMs = loop * LOOP_MS + frames[idx].atMs;
      if (virtualAtMs < lower || virtualAtMs > uptoVirtualMs) continue;
      out.push({ seq: loop * frames.length + idx, frame: frames[idx] });
    }
  }
  out.sort((a, b) => a.seq - b.seq);
  return out;
}

const actionSpanFacts = frames
  .filter((f): f is Extract<SseFrame, { event: "fact" }> => f.event === "fact" && (f.data as FactEvent).type === "action_span")
  .map((f) => f.data as Extract<FactEvent, { type: "action_span" }>);

/** Spans currently running as of `withinLoop` — DATA-CONTRACT §2.2:
 * "action_spans with no t_end yet". Derived from the (statically known,
 * eventual) t_start/t_end on each span's fact, per OpenActionSpanEvent's
 * contract: t_end is omitted, never guessed. */
function openSpansAt(withinLoop: number): OpenActionSpanEvent[] {
  return actionSpanFacts
    .filter((f) => {
      const startMs = Date.parse(f.payload.t_start) - T0_MS;
      const endMs = Date.parse(f.payload.t_end) - T0_MS;
      return startMs <= withinLoop && withinLoop < endMs;
    })
    .map((f) => {
      const { t_end: _t_end, ...payloadWithoutEnd } = f.payload;
      return { ...f, payload: payloadWithoutEnd };
    });
}

/** Indexed access (SseFrameDataMap[K]) resolves cleanly for a generic K;
 * Extract<SseFrame, {event: K}> does not distribute over a type parameter,
 * so it isn't used here. */
function latestOfKind<K extends SseEventName>(uptoFrameIndexInclusive: number, kind: K): SseFrameDataMap[K] | undefined {
  for (let i = uptoFrameIndexInclusive; i >= 0; i -= 1) {
    if (frames[i].event === kind) return frames[i].data as SseFrameDataMap[K];
  }
  // Wrap: very early in a fresh loop, fall back to the previous loop's tail.
  for (let i = frames.length - 1; i > uptoFrameIndexInclusive; i -= 1) {
    if (frames[i].event === kind) return frames[i].data as SseFrameDataMap[K];
  }
  return undefined;
}

// --- chaos state -----------------------------------------------------------
let stallUntilWallMs = 0;
const connections = new Set<ServerResponse>();

function isStalled(): boolean {
  return Date.now() < stallUntilWallMs;
}

// --- SSE wire helpers --------------------------------------------------------
function writeSseEvent(res: ServerResponse, event: string, data: unknown, id?: number): void {
  let out = `event: ${event}\n`;
  if (id !== undefined) out += `id: ${id}\n`;
  out += `data: ${JSON.stringify(data)}\n\n`;
  res.write(out);
}

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json");
  res.end(JSON.stringify(body));
}

function parseWindowSeconds(url: string): number {
  const match = /[?&]window=(\d+)s\b/.exec(url);
  return match ? Number(match[1]) * 1000 : 180_000;
}

/** `EventSource` cannot set a `Last-Event-ID` request header on its first
 * connect (only the browser's own automatic reconnects do that) — so a
 * fresh client passes the snapshot's `as_of_seq` as `?from=` instead. M2's
 * `handleStream` only honoured the `Last-Event-ID` header; M3 (console
 * client) added this query-param fallback so a client's very first
 * `/debug/stream` open — right after its first snapshot — replays from the
 * correct point instead of only from "now". A real `Last-Event-ID` header
 * (present on the browser's own automatic reconnects) still takes
 * precedence when both are present. */
function parseFromParam(url: string): number | undefined {
  const match = /[?&]from=(\d+)\b/.exec(url);
  return match ? Number(match[1]) : undefined;
}

// --- live broadcast tick -----------------------------------------------------
const connectionCursors = new Map<ServerResponse, number>();

setInterval(() => {
  if (isStalled()) return;
  const nowSeq = currentSeq(virtualNowMs());
  for (const res of connections) {
    const cursor = connectionCursors.get(res) ?? nowSeq;
    if (cursor >= nowSeq) continue;
    for (const { seq, frame } of framesBetween(cursor, nowSeq)) {
      writeSseEvent(res, frame.event, frame.data, seq);
    }
    connectionCursors.set(res, nowSeq);
  }
}, 200).unref();

// --- request handling ---------------------------------------------------------
function handleSession(res: ServerResponse): void {
  sendJson(res, 200, scenario.session satisfies SessionInfo);
}

function handleSnapshot(req: IncomingMessage, res: ServerResponse): void {
  const windowMs = parseWindowSeconds(req.url ?? "");
  const nowVirtualMs = virtualNowMs();
  const windowed = framesInWindow(nowVirtualMs, windowMs);

  const events = windowed.filter((w) => w.frame.event === "fact").map((w) => w.frame.data as FactEvent);
  const allocations = windowed.filter((w) => w.frame.event === "alloc").map((w) => w.frame.data as AllocationTrace);
  const coverage_gaps = windowed.filter((w) => w.frame.event === "gap").map((w) => w.frame.data as GapFrame);
  const lastWatchdog = [...windowed].reverse().find((w) => w.frame.event === "watchdog");
  const watchdog = toSnapshotWatchdog(lastWatchdog?.frame.data as WatchdogFrame | undefined);

  const snapshot: Snapshot = {
    events,
    allocations,
    coverage_gaps,
    open_spans: openSpansAt(withinLoopMs(nowVirtualMs)),
    watchdog,
    as_of_seq: windowed.length > 0 ? windowed[windowed.length - 1].seq : currentSeq(nowVirtualMs),
  };
  sendJson(res, 200, snapshot);
}

function handleAlloc(id: string, res: ServerResponse): void {
  const trace = scenario.allocs.get(id);
  if (!trace) {
    sendJson(res, 404, { error: "not_found", sample_event_id: id });
    return;
  }
  sendJson(res, 200, trace);
}

/** DATA-CONTRACT §2.6: `GET /debug/report?level=session|task|tool` — no
 * other value is valid. A dev-aid mock that echoed an unrecognised `level`
 * back into a reshaped report (or silently fell back to `session`) would be
 * exactly the kind of "looks plausible, isn't real" response
 * global-constraints.md's honesty rules forbid elsewhere in this console —
 * so the mock itself refuses it instead of echoing garbage. */
const VALID_REPORT_LEVELS: ReadonlySet<DebugReport["level"]> = new Set(["session", "task", "tool"]);

export function handleReport(req: IncomingMessage, res: ServerResponse): void {
  const url = new URL(req.url ?? "/debug/report", "http://127.0.0.1");
  const levelParam = url.searchParams.get("level") ?? "session";
  if (!VALID_REPORT_LEVELS.has(levelParam as DebugReport["level"])) {
    sendJson(res, 400, { error: "invalid level", level: levelParam, allowed: Array.from(VALID_REPORT_LEVELS) });
    return;
  }
  const level = levelParam as DebugReport["level"];
  if (level === "session") {
    sendJson(res, 200, scenario.report);
    return;
  }
  // task/tool: same fixture, reshaped smaller — one model's slice, at the
  // requested level, with a synthetic unit id so the shape is valid per type.
  const reshaped: DebugReport = {
    ...scenario.report,
    level,
    by_model: scenario.report.by_model.slice(0, 1),
    impact_join: {
      ...scenario.report.impact_join,
      unit:
        level === "task"
          ? { level: "task", session_id: scenario.session.session_id, task_id: "task_0001" }
          : { level: "tool_call", session_id: scenario.session.session_id, tool_call_id: "tool_0001" },
    },
  };
  sendJson(res, 200, reshaped);
}

function handleHealth(res: ServerResponse): void {
  sendJson(res, 200, scenario.health satisfies HealthPayload);
}

function handleStream(req: IncomingMessage, res: ServerResponse): void {
  res.statusCode = 200;
  res.setHeader("Content-Type", "text/event-stream");
  res.setHeader("Cache-Control", "no-cache");
  res.setHeader("Connection", "keep-alive");
  res.flushHeaders();

  const nowSeq = currentSeq(virtualNowMs());
  const lastEventIdHeader = req.headers["last-event-id"];
  const lastEventIdFromHeader = Array.isArray(lastEventIdHeader) ? lastEventIdHeader[0] : lastEventIdHeader;
  const fromParam = parseFromParam(req.url ?? "");
  const lastEventId = lastEventIdFromHeader ?? (fromParam !== undefined ? String(fromParam) : undefined);

  let cursor = nowSeq;
  if (lastEventId !== undefined) {
    const lastId = Number(lastEventId);
    const retentionFrames = frames.length * 2; // ~2 loops of replay history
    if (Number.isFinite(lastId) && lastId >= nowSeq - retentionFrames && lastId <= nowSeq) {
      for (const { seq, frame } of framesBetween(lastId, nowSeq)) {
        writeSseEvent(res, frame.event, frame.data, seq);
      }
      cursor = nowSeq;
    } else {
      writeSseEvent(res, "reset", {});
      cursor = nowSeq;
    }
  }

  // Always promptly send the freshest report/health so the UI isn't stale
  // while it waits for the next periodic tick.
  const withinLoop = withinLoopMs(virtualNowMs());
  const frameIndexNow = Math.max(lastIndexAtOrBefore(withinLoop), 0);
  const latestReport = latestOfKind(frameIndexNow, "report");
  if (latestReport) writeSseEvent(res, "report", latestReport, nowSeq);
  const latestHealth = latestOfKind(frameIndexNow, "health");
  if (latestHealth) writeSseEvent(res, "health", latestHealth, nowSeq);

  connections.add(res);
  connectionCursors.set(res, cursor);

  req.on("close", () => {
    connections.delete(res);
    connectionCursors.delete(res);
  });
}

/** Drain and discard a request body (the chaos toggles ignore it) so the
 * underlying socket doesn't hang waiting for the request to finish. */
function drain(req: IncomingMessage): Promise<void> {
  return new Promise((resolve) => {
    req.resume();
    req.on("end", () => resolve());
  });
}

function attach(middlewares: Connect.Server): void {
  middlewares.use((req, res, next) => {
    const url = req.url ?? "";
    const path = url.split("?")[0];

    if (path === "/debug/__mock/drop-sse" && req.method === "POST") {
      void drain(req).then(() => {
        for (const conn of connections) conn.end();
        connections.clear();
        connectionCursors.clear();
        sendJson(res, 200, { dropped: true });
      });
      return;
    }
    if (path === "/debug/__mock/stall" && req.method === "POST") {
      void drain(req).then(() => {
        stallUntilWallMs = Date.now() + 10_000;
        sendJson(res, 200, { stalled_for_ms: 10_000 });
      });
      return;
    }
    if (path === "/debug/session") return handleSession(res);
    if (path === "/debug/snapshot") return handleSnapshot(req, res);
    if (path === "/debug/report") return handleReport(req, res);
    if (path === "/debug/health") return handleHealth(res);
    if (path === "/debug/stream") return handleStream(req, res);
    const allocMatch = /^\/debug\/alloc\/([^/]+)$/.exec(path);
    if (allocMatch) return handleAlloc(decodeURIComponent(allocMatch[1]), res);

    next();
  });
}

export function mockDebugServer(): Plugin {
  return {
    name: "mock-debug-server",
    configureServer(server) {
      attach(server.middlewares);
    },
    configurePreviewServer(server) {
      attach(server.middlewares);
    },
  };
}
