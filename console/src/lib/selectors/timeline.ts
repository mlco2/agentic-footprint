// Timeline tab selectors (DATA-CONTRACT §3.5, §3.6). Pure module: no Svelte
// imports, no `Date.now()`/`Math.random()` — `nowMs` always arrives as an
// argument from `uiStore.nowMs` (global-constraints.md #6: "selectors
// receive nowMs as an argument and never read the clock"), and every
// `Date.parse` here is on a string the payload itself already carries
// (mirrors selectors/stream.ts's own convention).
//
// `selectTimelineLanes` is the ONE place all Layout-A geometry math lives —
// `LaneChart.svelte` renders `LaneModel.bars` as a single flat
// absolutely-positioned loop (DATA-CONTRACT §3.6: "lane geometry is data,
// not markup") and does no positioning arithmetic of its own.
import type { EnergySample, FactEvent } from "../types/contract1";
import type { SpanRecord } from "../stores/eventStore.svelte";
import { eventStore } from "../stores/eventStore.svelte";
import type { DecisionFrame, GapFrame, WatchdogEntry } from "../types/debug";
import { fmtBytes, fmtClock, fmtCpuPct, fmtEventsPerS, fmtJoules, fmtMs, fmtTokens, fmtWatts } from "../format";
import { USAGE_SOURCE_RANK } from "./stream";
import { memo1 } from "./memo";
import { clamp, round3, widthWithFloor } from "./geometry";

// ---------------------------------------------------------------------------
// Geometry constants (SCREENS.md §1 Layout A — every value here is binding;
// the inter-lane gaps (4/8/6px) and the 0.12% minimum bar width mirror the
// prototype `Debug Console.dc.html`, the "visual reference for Layout A"
// the brief names, since SCREENS.md's prose doesn't spell out gap px).
// ---------------------------------------------------------------------------

/** Trailing window ending at `nowMs` — frozen for free when `nowMs` itself
 * is frozen (uiStore only advances `nowMs` while live). */
export const WINDOW_MS = 180_000;

const LLM_LANE_H = 22;
const GAP_AFTER_LLM = 4;
const TRACK_H = 21;
const MAX_TRACKS = 7;
const GAP_AFTER_SPANS = 8;
const ENERGY_LANE_H = 54;
const GAP_AFTER_ENERGY = 6;
const PROCESS_LANE_H = 34;
/** Mirrors `--space-2` (10px) — a plain number because JS geometry code
 * can't read a CSS custom property; kept in lockstep with that token by
 * this comment rather than by import. */
const BOTTOM_PAD = 10;
/** Floor so a zero-duration llm_call tick or energy sample is still a
 * visible sliver, not an invisible 0-width box (prototype's own floor). */
const MIN_BAR_WIDTH_PCT = 0.12;
const AXIS_TICK_COUNT = 6;

// ---------------------------------------------------------------------------
// Sticky action_span track assignment (brief: "stable track assignment while
// a span stays in window"). Deliberately persisted at MODULE scope, not
// recomputed from scratch inside `computeTimelineLanes` — track stability is
// inherently a function of ASSIGNMENT HISTORY, not of a single (rev, nowMs,
// hiddenKey, selectedId) snapshot, so no formula derived purely from that
// tuple's current data can satisfy it. A from-scratch greedy re-fit on every
// call (sorted by tStartMs, first-fit into track 0 upward) is exactly the bug
// this replaces: a still-active span visually jumped rows whenever an
// unrelated, earlier-starting span aged out of the window and the refit
// re-filled track 0 with whatever was now first in sorted order.
//
// This is still fully deterministic (no `Date.now()`/`Math.random()`) and
// still test-isolated: every test re-imports this module fresh via
// `vi.resetModules()` (see timeline.test.ts's `freshEnv()`), which resets
// this state too, exactly like `memo1`'s own per-module cache.
//
// Once a span_id is assigned a track, it KEEPS that track forever — even
// after it ages out of the window (at which point nothing touches its entry
// again until it either reappears, e.g. because its orphan tail keeps it
// active, or is pruned below). Only `trackEndMs[i]`, the recorded "busy
// until" time for a track, is refreshed on every tick a span is still
// present — this is what lets a genuinely later, non-overlapping span
// legitimately reuse a track an expired span once held, without that reuse
// ever being able to disturb a span that is STILL present (see
// `assignTrack`'s two-pass caller below: existing spans always refresh
// before any brand-new span competes for a free slot).
const trackEndMs: number[] = new Array(MAX_TRACKS).fill(-Infinity);
const trackOfSpan = new Map<string, number>();

/** Returns `spanId`'s sticky track index, assigning one via first-fit (the
 * lowest track whose recorded end is `<=` `startMs`) if it doesn't have one
 * yet, or `undefined` if all `MAX_TRACKS` are occupied by other spans. An
 * already-assigned span never moves — only its track's recorded occupancy
 * end is refreshed to `effectiveEndMs` (an open span running longer, or an
 * orphan tail now extending to `nowMs`). */
function assignTrack(spanId: string, startMs: number, effectiveEndMs: number): number | undefined {
  const existing = trackOfSpan.get(spanId);
  if (existing !== undefined) {
    trackEndMs[existing] = effectiveEndMs;
    return existing;
  }
  for (let i = 0; i < MAX_TRACKS; i += 1) {
    if (trackEndMs[i] <= startMs) {
      trackEndMs[i] = effectiveEndMs;
      trackOfSpan.set(spanId, i);
      return i;
    }
  }
  return undefined; // dropped: beyond the 7-track cap
}

// ---------------------------------------------------------------------------
// selectTimelineLanes
// ---------------------------------------------------------------------------

export interface Bar {
  /** Opaque record id (`span_id`/`event_id`) for a clickable bar, or `""`
   * for one with no backing selectable record (a coverage-gap band). Never
   * synthesised as if it were a real server id — `""` is a sentinel, not a
   * fabricated identity. */
  id: string;
  kind: "llm_call" | "action_span" | "energy_sample" | "process_sample" | "gap" | "orphan";
  leftPct: number;
  widthPct: number;
  topPx: number;
  heightPx: number;
  fillVar: string;
  hatch: "none" | "neutral" | "alarm";
  borderVar: string;
  selected: boolean;
  /** Full hover numbers (README "Interactions & behaviour" — watts, joules
   * per component + method, duration, locus, pid, span_id), built entirely
   * via format.ts. */
  title: string;
}

export interface LaneModel {
  bars: Bar[];
  laneLabels: { label: string; topPx: number }[];
  axisTicks: { leftPct: number; label: string }[];
  windowLabel: string;
  spanCount: number;
  plotHeightPx: number;
  /** Count of `action_span`s in the trailing window that couldn't be drawn
   * because all `MAX_TRACKS` (7) tracks were already occupied
   * (`assignTrack` returning `undefined`) — a truncation, not an alarm (the
   * "+N spans not shown" cue Timeline.svelte renders is neutral text, the
   * same "showing N of M" convention as elsewhere in the console, never
   * magenta). `0` when every overlapping span got a track. */
  droppedSpans: number;
}

function llmCallTitle(payload: Extract<FactEvent, { type: "llm_call" }>["payload"], durationMs: number): string {
  const parts = [payload.model_id_requested];
  if (payload.usage.input_tokens !== undefined) parts.push(`in ${fmtTokens(payload.usage.input_tokens)}`);
  if (payload.usage.output_tokens !== undefined) parts.push(`out ${fmtTokens(payload.usage.output_tokens)}`);
  if (payload.usage.thought_tokens) parts.push(`think ${fmtTokens(payload.usage.thought_tokens)}`);
  parts.push(payload.usage_source, fmtMs(durationMs));
  return parts.join(" · ");
}

function spanTitle(rec: SpanRecord, barEndMs: number): string {
  const remote = rec.execution_locus === "remote";
  const parts = [rec.tool_name, fmtMs(barEndMs - rec.tStartMs), `${rec.tool_kind}/${rec.execution_locus}`];
  if (remote) parts.push("excluded from local energy join");
  else if (rec.pids && rec.pids.length > 0) parts.push(`pid ${rec.pids[0]}`);
  parts.push(rec.span_id);
  return parts.join(" · ");
}

function orphanTitle(wd: WatchdogEntry, spanEndMs: number, nowMs: number): string {
  return `orphaned · pid ${wd.pid} outlived ${wd.span_id} by ${fmtMs(nowMs - spanEndMs)}`;
}

function energyTitle(payload: EnergySample, watts: number, intervalMs: number): string {
  const comps = payload.components.map((c) => `${c.kind} ${fmtJoules(c.energy_j)} ${c.method}`).join(" · ");
  return `${fmtWatts(watts)} avg over ${fmtMs(intervalMs)} · ${comps}`;
}

function gapTitle(gap: GapFrame, durationMs: number): string {
  return `NO COVERAGE · ${fmtMs(durationMs)} — ${gap.reason} (${gap.collector})`;
}

/** Full hover numbers (this file's `Bar` doc comment: "pid ... duration") —
 * the aggregate cpu-ms/tree-count this bar's height is proportional to,
 * PLUS each watched tree's own `pid`/`cpu_time_delta_ms` verbatim (the same
 * per-process fields `processSampleInspectorRows`, selectors/inspector.ts,
 * already surfaces in the Inspector's raw rows — this just puts them on the
 * hover title too, not a new number). */
function processSampleTitle(payload: Extract<FactEvent, { type: "process_sample" }>["payload"], cpuMs: number, count: number): string {
  const summary = `${fmtMs(cpuMs)} cpu across ${count} watched tree${count === 1 ? "" : "s"}`;
  const perProcess = payload.processes.map((p) => `pid ${p.pid} ${fmtMs(p.cpu_time_delta_ms)}`).join(", ");
  return perProcess.length > 0 ? `${summary} · ${perProcess}` : summary;
}

function buildAxisTicks(tStart: number, nowMs: number): { leftPct: number; label: string }[] {
  const ticks: { leftPct: number; label: string }[] = [];
  for (let i = 0; i <= AXIS_TICK_COUNT; i += 1) {
    const tMs = tStart + i * (WINDOW_MS / AXIS_TICK_COUNT);
    const label = i === AXIS_TICK_COUNT ? "now" : `−${fmtMs(nowMs - tMs)}`;
    ticks.push({ leftPct: round3((i / AXIS_TICK_COUNT) * 100), label });
  }
  return ticks;
}

function computeTimelineLanes(hiddenKey: string, nowMs: number, selectedId: string | null): LaneModel {
  const hidden = hiddenKey === "" ? null : new Set(hiddenKey.split(","));
  const isHidden = (t: string): boolean => hidden !== null && hidden.has(t);

  const tStart = nowMs - WINDOW_MS;
  const posOf = (aMs: number, bMs: number): { leftPct: number; widthPct: number } => {
    const l = clamp(((aMs - tStart) / WINDOW_MS) * 100, 0, 100);
    const r = clamp(((bMs - tStart) / WINDOW_MS) * 100, 0, 100);
    return { leftPct: round3(l), widthPct: round3(widthWithFloor(r - l, MIN_BAR_WIDTH_PCT)) };
  };
  const overlapsWindow = (aMs: number, bMs: number): boolean => aMs < nowMs && tStart < bMs;

  const bars: Bar[] = [];
  const gapBars: Bar[] = []; // deferred: height = plotHeightPx, only known once every lane's y is final
  const laneLabels: { label: string; topPx: number }[] = [];
  let y = 0;
  let spanCount = 0;
  let droppedSpans = 0;

  // --- llm_call ticks: bar spans ts - duration_ms -> ts (DATA-CONTRACT §4) ---
  if (!isHidden("llm_call")) {
    laneLabels.push({ label: "llm_call", topPx: y });
    for (const { event, tsMs } of eventStore.facts) {
      if (event.type !== "llm_call") continue;
      const durationMs = event.payload.duration_ms ?? 0;
      const startMs = tsMs - durationMs;
      if (!overlapsWindow(startMs, tsMs)) continue;
      const alarm = USAGE_SOURCE_RANK[event.payload.usage_source] >= 2; // transcript, estimated
      const { leftPct, widthPct } = posOf(startMs, tsMs);
      bars.push({
        id: event.event_id,
        kind: "llm_call",
        leftPct,
        widthPct,
        topPx: y + 5,
        heightPx: 12,
        fillVar: alarm ? "transparent" : "var(--color-accent)",
        hatch: alarm ? "alarm" : "none",
        borderVar: event.payload.status === "error" ? "var(--tool-border-status-error)" : "var(--color-accent-700)",
        selected: event.event_id === selectedId,
        title: llmCallTitle(event.payload, durationMs),
      });
    }
    y += LLM_LANE_H + GAP_AFTER_LLM;
  }

  // --- action_span: ≤7 packed tracks, greedy first-fit by overlap ---
  if (!isHidden("action_span")) {
    const spanLaneTop = y;
    const overlapping = eventStore.spansOverlapping(tStart, nowMs);
    spanCount = overlapping.length;

    const orphanedSpanIds = new Set(eventStore.watchdog.filter((w) => w.state === "orphaned").map((w) => w.span_id));

    // Trackable set = spans overlapping the window UNION any orphaned span
    // whose own interval predates the window but whose tail (span end ->
    // now) still needs a track to be drawn on.
    const trackable = new Map<string, SpanRecord>();
    for (const s of overlapping) trackable.set(s.span_id, s);
    for (const id of orphanedSpanIds) {
      if (!trackable.has(id)) {
        const rec = eventStore.spans.get(id);
        if (rec) trackable.set(id, rec);
      }
    }

    // Prune sticky assignments for spans eventStore no longer holds at all
    // (ring-evicted) — bounds trackOfSpan's growth over a long session. Safe
    // to do every call: an evicted span_id can never reappear (span_ids are
    // opaque and never reused), so there is nothing to preserve continuity
    // with.
    for (const spanId of trackOfSpan.keys()) {
      if (!eventStore.spans.has(spanId)) trackOfSpan.delete(spanId);
    }

    const effectiveEndOf = (rec: SpanRecord): number => (orphanedSpanIds.has(rec.span_id) ? nowMs : (rec.tEndMs ?? nowMs));

    // Sorted by start time only to make brand-new-span assignment order
    // deterministic when several first appear in the same tick — it has NO
    // bearing on already-assigned spans, which never compete for a track
    // again. Two passes: every span already holding a track refreshes its
    // own track's occupancy FIRST, so a not-yet-refreshed (stale) entry can
    // never look prematurely "free" to a brand-new span assigned afterward.
    const sorted = Array.from(trackable.values()).sort((a, b) => a.tStartMs - b.tStartMs);
    const alreadyAssigned = sorted.filter((rec) => trackOfSpan.has(rec.span_id));
    const newlySeen = sorted.filter((rec) => !trackOfSpan.has(rec.span_id));
    for (const rec of alreadyAssigned) assignTrack(rec.span_id, rec.tStartMs, effectiveEndOf(rec));
    for (const rec of newlySeen) assignTrack(rec.span_id, rec.tStartMs, effectiveEndOf(rec));

    const trackIndexOf = trackOfSpan;
    // Lane height reflects the highest track index actually occupied by a
    // currently-trackable span (not the historical max ever used, and not a
    // recount of all MAX_TRACKS slots) — an idle low-numbered track a span
    // once held stays reserved (see `assignTrack`), it just doesn't force
    // extra lane height once nothing trackable is using it this tick.
    let maxActiveTrack = -1;
    for (const rec of trackable.values()) {
      const idx = trackIndexOf.get(rec.span_id);
      if (idx !== undefined && idx > maxActiveTrack) maxActiveTrack = idx;
    }
    const nTracks = Math.max(1, maxActiveTrack + 1);
    laneLabels.push({ label: "action_span", topPx: spanLaneTop });

    for (const rec of overlapping) {
      const trackIdx = trackIndexOf.get(rec.span_id);
      if (trackIdx === undefined) {
        droppedSpans += 1; // dropped by the track cap — surfaced via LaneModel.droppedSpans
        continue;
      }
      const barEnd = rec.tEndMs ?? nowMs; // open span: bar runs to "now"
      const remote = rec.execution_locus === "remote";
      const { leftPct, widthPct } = posOf(rec.tStartMs, barEnd);
      bars.push({
        id: rec.span_id,
        kind: "action_span",
        leftPct,
        widthPct,
        topPx: spanLaneTop + trackIdx * TRACK_H + 2,
        heightPx: TRACK_H - 4,
        fillVar: remote ? "transparent" : `var(--tool-fill-${rec.tool_kind}, var(--color-neutral-300))`,
        hatch: remote ? "neutral" : "none",
        borderVar: rec.status === "error" ? "var(--tool-border-status-error)" : `var(--tool-border-${rec.tool_kind}, var(--color-neutral-700))`,
        selected: rec.span_id === selectedId,
        title: spanTitle(rec, barEnd),
      });
    }

    // --- orphan tails: watchdog entry's span end -> now, alarm hatch, same track ---
    for (const wd of eventStore.watchdog) {
      if (wd.state !== "orphaned") continue;
      const rec = eventStore.spans.get(wd.span_id);
      if (!rec || rec.tEndMs === null) continue;
      const trackIdx = trackIndexOf.get(wd.span_id);
      if (trackIdx === undefined) continue;
      if (rec.tEndMs >= nowMs) continue; // no positive-width tail yet
      const { leftPct, widthPct } = posOf(rec.tEndMs, nowMs);
      bars.push({
        id: wd.span_id,
        kind: "orphan",
        leftPct,
        widthPct,
        topPx: spanLaneTop + trackIdx * TRACK_H + 2,
        heightPx: TRACK_H - 4,
        fillVar: "transparent",
        hatch: "alarm",
        borderVar: "var(--color-accent-2)",
        selected: wd.span_id === selectedId,
        title: orphanTitle(wd, rec.tEndMs, nowMs),
      });
    }

    y = spanLaneTop + nTracks * TRACK_H + GAP_AFTER_SPANS;
  }

  // --- energy_sample: bottom-anchored power bars, height ∝ watts / window max ---
  if (!isHidden("energy_sample")) {
    const energyLaneTop = y;
    laneLabels.push({ label: "energy_sample", topPx: energyLaneTop });

    const samples: { event: Extract<FactEvent, { type: "energy_sample" }>; startMs: number; endMs: number; watts: number }[] = [];
    for (const { event } of eventStore.facts) {
      if (event.type !== "energy_sample") continue;
      const startMs = Date.parse(event.payload.t_start);
      const endMs = Date.parse(event.payload.t_end);
      if (!overlapsWindow(startMs, endMs)) continue;
      const intervalS = Math.max(0.001, (endMs - startMs) / 1000);
      // Σ this ONE sample's own components / its own interval — DATA-CONTRACT
      // §4's explicitly-granted exception ("display scaling of server-measured
      // values ... use the payload's own numbers"), never an aggregate across
      // samples.
      const sumJ = event.payload.components.reduce((acc, c) => acc + c.energy_j, 0);
      samples.push({ event, startMs, endMs, watts: sumJ / intervalS });
    }
    const maxWatts = Math.max(1e-6, ...samples.map((s) => s.watts));
    for (const s of samples) {
      const h = Math.max(1, Math.round((s.watts / maxWatts) * ENERGY_LANE_H));
      const { leftPct, widthPct } = posOf(s.startMs, s.endMs);
      const selected = s.event.event_id === selectedId;
      bars.push({
        id: s.event.event_id,
        kind: "energy_sample",
        leftPct,
        widthPct,
        topPx: energyLaneTop + ENERGY_LANE_H - h,
        heightPx: h,
        fillVar: selected ? "var(--color-accent-700)" : "var(--color-accent)",
        hatch: "none",
        borderVar: "transparent",
        selected,
        title: energyTitle(s.event.payload, s.watts, s.endMs - s.startMs),
      });
    }

    // Coverage-gap bands: ONLY from server gap records, never inferred from
    // missing samples — full plot height, deferred until plotHeightPx is known.
    for (const gap of eventStore.gaps) {
      const gStart = Date.parse(gap.t_start);
      const gEnd = Date.parse(gap.t_end);
      if (!overlapsWindow(gStart, gEnd)) continue;
      const { leftPct, widthPct } = posOf(gStart, gEnd);
      gapBars.push({
        id: "",
        kind: "gap",
        leftPct,
        widthPct,
        topPx: 0,
        heightPx: 0, // patched below once plotHeightPx is final
        fillVar: "transparent",
        hatch: "alarm",
        borderVar: "var(--color-accent-2)",
        selected: false,
        title: gapTitle(gap, gEnd - gStart),
      });
    }

    y = energyLaneTop + ENERGY_LANE_H + GAP_AFTER_ENERGY;
  }

  // --- process_sample: bottom-anchored activity bars, height ∝ cpu-ms / window max ---
  if (!isHidden("process_sample")) {
    const processLaneTop = y;
    laneLabels.push({ label: "process_sample", topPx: processLaneTop });

    const samples: { event: Extract<FactEvent, { type: "process_sample" }>; startMs: number; endMs: number; cpuMs: number; count: number }[] = [];
    for (const { event } of eventStore.facts) {
      if (event.type !== "process_sample") continue;
      const startMs = Date.parse(event.payload.t_start);
      const endMs = Date.parse(event.payload.t_end);
      if (!overlapsWindow(startMs, endMs)) continue;
      const cpuMs = event.payload.processes.reduce((acc, p) => acc + p.cpu_time_delta_ms, 0);
      samples.push({ event, startMs, endMs, cpuMs, count: event.payload.processes.length });
    }
    const maxCpu = Math.max(1e-6, ...samples.map((s) => s.cpuMs));
    for (const s of samples) {
      const h = Math.max(1, Math.round((s.cpuMs / maxCpu) * PROCESS_LANE_H));
      const { leftPct, widthPct } = posOf(s.startMs, s.endMs);
      bars.push({
        id: s.event.event_id,
        kind: "process_sample",
        leftPct,
        widthPct,
        topPx: processLaneTop + PROCESS_LANE_H - h,
        heightPx: h,
        fillVar: "var(--color-neutral-400)",
        hatch: "none",
        borderVar: "transparent",
        selected: s.event.event_id === selectedId,
        title: processSampleTitle(s.event.payload, s.cpuMs, s.count),
      });
    }

    y = processLaneTop + PROCESS_LANE_H;
  }

  const plotHeightPx = y + BOTTOM_PAD;
  for (const g of gapBars) g.heightPx = plotHeightPx;

  return {
    bars: [...gapBars, ...bars], // gap bands first: painted behind everything else
    laneLabels,
    axisTicks: buildAxisTicks(tStart, nowMs),
    windowLabel: `${fmtClock(tStart)} → ${fmtClock(nowMs)}`,
    spanCount,
    plotHeightPx,
    droppedSpans,
  };
}

const memoTimelineLanes = memo1((_rev: number, hiddenKey: string, nowMs: number, selectedId: string | null) => computeTimelineLanes(hiddenKey, nowMs, selectedId));

/** `hiddenTypes` is `uiStore.hiddenTypes`, a `SvelteSet` whose identity never
 * changes — reduced to a sorted comma-joined string first, same as
 * `selectStreamRows`, so `memo1`'s `Object.is` comparison sees a value that
 * actually changes when membership does. */
export function selectTimelineLanes(rev: number, nowMs: number, hiddenTypes: ReadonlySet<string>, selectedId: string | null): LaneModel {
  const hiddenKey = hiddenTypes.size === 0 ? "" : Array.from(hiddenTypes).sort().join(",");
  return memoTimelineLanes(rev, hiddenKey, nowMs, selectedId);
}

// ---------------------------------------------------------------------------
// selectDecisionLog
// ---------------------------------------------------------------------------

export interface DecisionRow {
  /** `eventStore`'s per-decision `seq`, assigned once at ingest — the
   * `{#each}` key DecisionLog.svelte renders on, so a new decision arriving
   * (which shifts every older row's array index, since rows render
   * newest-first) never disturbs an existing row's identity. */
  key: number;
  kind: DecisionFrame["kind"];
  prefixLabel: string;
  ts: string;
  text: string;
  ref?: string;
}

/** SCREENS.md: "Cap [attr] lines to ~8 of ~30 visible — the sampler emits
 * one per 2s and will otherwise bury [span open] and [ingest] entirely." */
const DECISION_LOG_VISIBLE_CAP = 30;
const ATTR_CAP = 8;

const PREFIX_LABEL: Record<DecisionFrame["kind"], string> = {
  ingest: "[ingest]",
  span_open: "[span open]",
  attr: "[attr]",
  orphan: "[orphan]",
};

function computeDecisionLog(): DecisionRow[] {
  const rows: DecisionRow[] = [];
  let attrSeen = 0;
  // eventStore.decisions is oldest -> newest (FIFO ring, cap 500, arrival
  // order already ~chronological — decisions are server-emitted log lines,
  // not re-sorted facts). Walk newest-first so both the ~30 cap and the
  // [attr] cap bite the OLDEST attr lines, keeping the most recent ones.
  for (let i = eventStore.decisions.length - 1; i >= 0 && rows.length < DECISION_LOG_VISIBLE_CAP; i -= 1) {
    const d = eventStore.decisions[i];
    if (d.kind === "attr") {
      attrSeen += 1;
      if (attrSeen > ATTR_CAP) continue; // drop the OLDEST attr lines beyond the cap
    }
    rows.push({
      key: d.seq,
      kind: d.kind,
      prefixLabel: PREFIX_LABEL[d.kind],
      ts: fmtClock(Date.parse(d.ts)),
      text: d.text,
      ref: d.ref,
    });
  }
  return rows;
}

const memoDecisionLog = memo1((_rev: number) => computeDecisionLog());

export function selectDecisionLog(rev: number): DecisionRow[] {
  return memoDecisionLog(rev);
}

// ---------------------------------------------------------------------------
// selectRail
// ---------------------------------------------------------------------------

export interface CollectorRailRow {
  name: string;
  evCount: number;
  eventsPerSLabel: string;
  rejectedCount: number;
  dotClass: "dot-accent" | "dot-neutral" | "dot-alarm";
}

export interface TypeRailRow {
  type: FactEvent["type"];
  count: number;
}

export interface WatchdogRailRow {
  pid: number;
  cmd: string;
  cpuPctLabel: string;
  rssLabel: string;
  state: WatchdogEntry["state"];
}

export interface RailModel {
  collectors: CollectorRailRow[];
  types: TypeRailRow[];
  watchdog: WatchdogRailRow[];
  /** Magenta italic summary line (SCREENS.md), `null` when nothing is orphaned. */
  orphanSummary: string | null;
}

/** Same fixed order FilterChips.svelte uses — this is the "reuse M4 counts"
 * the brief calls for, not a re-derivation of what counts as a type. */
const RAIL_TYPES: FactEvent["type"][] = ["llm_call", "action_span", "energy_sample", "process_sample", "session_meta"];

/** SCREENS.md/Health tab convention, reused here: accent <12s idle,
 * neutral-400 <45s, magenta beyond — "clamp idle at 0" because a fact's `ts`
 * can be marginally ahead of `nowMs` (client clock vs. server stamp). */
const IDLE_ACCENT_MS = 12_000;
const IDLE_NEUTRAL_MS = 45_000;

function dotClassFor(idleMs: number): CollectorRailRow["dotClass"] {
  if (idleMs < IDLE_ACCENT_MS) return "dot-accent";
  if (idleMs < IDLE_NEUTRAL_MS) return "dot-neutral";
  return "dot-alarm";
}

function computeRail(nowMs: number): RailModel {
  const names = new Set<string>([...eventStore.perCollector.keys(), ...eventStore.perCollectorLastSeenMs.keys()]);
  const collectors: CollectorRailRow[] = Array.from(names)
    .sort()
    .map((name) => {
      const lastSeenMs = eventStore.perCollectorLastSeenMs.get(name);
      const idleMs = lastSeenMs === undefined ? Infinity : Math.max(0, nowMs - lastSeenMs);
      // Reject frames carry no explicit collector field, only `origin` (the
      // spool file, conventionally "<collector-name>.<id>.jsonl" per
      // DATA-CONTRACT §2.2's own example) — a prefix join over two already-
      // real fields, not a fabricated count; under-counts (never over-
      // counts) if a real deployment names spool files differently.
      const rejectedCount = eventStore.rejects.filter((r) => r.origin.startsWith(`${name}.`)).length;
      return {
        name,
        evCount: eventStore.perCollector.get(name) ?? 0,
        // The real server always sends `events_per_s: null` per collector
        // (docs/design-log.md: "a rate over a session's whole span is not
        // the rate anyone reads it as") and eventStore doesn't track one —
        // fmtEventsPerS(null) renders "—", never a fabricated rate.
        eventsPerSLabel: fmtEventsPerS(null),
        rejectedCount,
        dotClass: dotClassFor(idleMs),
      };
    });

  const types: TypeRailRow[] = RAIL_TYPES.map((type) => ({ type, count: eventStore.perType.get(type) ?? 0 }));

  const watchdog: WatchdogRailRow[] = eventStore.watchdog.map((w) => ({
    pid: w.pid,
    cmd: w.cmd,
    cpuPctLabel: fmtCpuPct(w.cpu_pct),
    rssLabel: fmtBytes(w.rss_bytes),
    state: w.state,
  }));

  const orphaned = eventStore.watchdog.filter((w) => w.state === "orphaned");
  const orphanSummary =
    orphaned.length === 0
      ? null
      : orphaned.map((w) => `pid ${w.pid} outlived ${w.span_id}${w.outlived_span_by_ms !== undefined ? ` by ${fmtMs(w.outlived_span_by_ms)}` : ""}`).join(" · ");

  return { collectors, types, watchdog, orphanSummary };
}

const memoRail = memo1((_rev: number, nowMs: number) => computeRail(nowMs));

export function selectRail(rev: number, nowMs: number): RailModel {
  return memoRail(rev, nowMs);
}
