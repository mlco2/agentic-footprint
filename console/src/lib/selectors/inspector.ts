// Inspector model builders (DATA-CONTRACT §3.5; SCREENS.md §1 Layout A + §2
// Stream both use this ONE component/model). Pure module: no Svelte imports,
// no `Date.now()`/`Math.random()`.
//
// Task 6 (M6): unifies selection across every surface. `uiStore.selectedId`
// may arrive here as any of THREE id shapes, all opaque server ids, never
// synthesised:
//   - a fact's own `event_id` (Stream row, correlated row, decision-log ref,
//     a closed action_span's own row, an energy_sample bar on Timeline)
//   - a `span_id` (Timeline action_span/orphan bar — LaneChart bars key
//     action_span by `span_id`, not `event_id`, per selectors/timeline.ts —
//     or a decision-log ref about a span)
//   - (the above two coincide once a span closes: its own fact's `event_id`
//     is a third, also-valid way to reach it)
// `resolveSelection` is the one place that turns any of these into either a
// backing `FactEvent` or (for a still-open span with no closing fact yet) the
// `SpanRecord` eventStore already tracks — every other selector in this file
// builds on top of that single resolution so all three routes converge on
// one model for "the same record", per this task's brief.
//
// This file also owns the span-energy and sample-share bar models
// (DATA-CONTRACT §4's two Inspector rows: "energy · l2_cpu_time" and "share
// bars"). Both are pure READS of `allocStore`/`eventStore` — fetching missing
// traces is the tab container's job (`selectRelevantSampleIds` tells the
// container which sample ids to fetch; this module never calls
// `allocStore.fetch` itself).
import type { FactEvent } from "../types/contract1";
import type { AllocationRow, AllocationTrace } from "../types/debug";
import type { SpanRecord } from "../stores/eventStore.svelte";
import { eventStore } from "../stores/eventStore.svelte";
import { allocStore } from "../stores/allocStore.svelte";
import { fmtClock, fmtJoules, fmtMs, fmtOffset, fmtPct, fmtTokens } from "../format";
import { factsOf, isAlarmUsageSource, isErrorStatus, isModelledMethod } from "./factFormat";
import { memo1 } from "./memo";

// ---------------------------------------------------------------------------
// Selection resolution — the one place all three route kinds converge
// ---------------------------------------------------------------------------

type Resolved = { kind: "fact"; event: FactEvent; tsMs: number } | { kind: "openSpan"; record: SpanRecord };

/** Resolves `uiStore.selectedId` (event_id OR span_id) to either a backing
 * fact or an open span's `SpanRecord`. `null` when the id is unknown (e.g.
 * ring-evicted, or never real — same "selecting an unknown id highlights
 * nothing but doesn't crash" contract `selectInspector` already had). */
function resolveSelection(selectedId: string): Resolved | null {
  const fact = eventStore.facts.find(({ event }) => event.event_id === selectedId);
  if (fact) return { kind: "fact", event: fact.event, tsMs: fact.tsMs };

  const record = eventStore.spans.get(selectedId);
  if (!record) return null;

  if (record.tEndMs !== null) {
    // Closed span selected BY its span_id (Timeline bar / a decision-log ref
    // naming the span) — find the actual closing `action_span` fact for full
    // field fidelity. `SpanRecord` deliberately doesn't retain `cgroup` or
    // `attribution` (eventStore.ts's `OpenSpanPayloadFields`/`ingestClosedSpan`
    // only index the fields the bucket/track logic needs), so reaching the
    // real fact is what makes this route produce an IDENTICAL model to
    // selecting the same span via its own event_id.
    const closing = eventStore.facts.find(({ event }) => event.type === "action_span" && event.payload.span_id === selectedId);
    if (closing) return { kind: "fact", event: closing.event, tsMs: closing.tsMs };
    // Unreachable in practice: `evictSlotAt` removes a span's ring slot and
    // its `spanMap` entry together, so a closed record with no backing fact
    // shouldn't exist. Degrade to the open-span rendering rather than
    // returning null — a partial, honest model beats a silent gap.
  }
  return { kind: "openSpan", record };
}

/** The `span_id` a selection is "about", regardless of which id form
 * selected it — `null` when the selection isn't an action_span at all. Takes
 * an already-`resolveSelection`d value rather than `selectedId` itself so
 * callers that need both the resolution AND the span id (buildSpanEnergy,
 * computeRelevantSampleIds) resolve the selection exactly once per model
 * build, not once here plus once in the caller. */
function spanIdOfResolved(resolved: Resolved | null): string | null {
  if (!resolved) return null;
  if (resolved.kind === "openSpan") return resolved.record.span_id;
  return resolved.event.type === "action_span" ? resolved.event.payload.span_id : null;
}

function spanIntervalOf(spanId: string): { startMs: number; endMs: number } | null {
  const record = eventStore.spans.get(spanId);
  if (!record) return null;
  // An open span's upper bound is unbounded (no clock read — this is a
  // structural "still running", not "as of now"); `spansOverlapping`'s own
  // convention of taking the caller's window bound doesn't apply here since
  // this module never receives a `nowMs` argument (selectSpanEnergy's
  // signature is `(rev, allocRev, selectedId)` per this task's brief).
  return { startMs: record.tStartMs, endMs: record.tEndMs ?? Infinity };
}

// ---------------------------------------------------------------------------
// InspectorRow / InspectorModel (moved from stream.ts — see this file's header)
// ---------------------------------------------------------------------------

export interface InspectorRow {
  key: string;
  value: string;
  /** `"alarm"` = DESIGN-SYSTEM §3's magenta failure-honesty cases
   * (transcript/estimated usage, `status: error`). `"modelled"` = the
   * neutral-hatch measured/modelled axis (`execution_locus: remote`,
   * `method: tdp_model`) — a different, non-alarm semantic. Absent = plain. */
  tone?: "alarm" | "modelled";
}

export interface InspectorModel {
  /** Record kind — also gates which of `selectSpanEnergy`/`selectSampleShare`
   * a container should even ask for (only meaningful for `action_span` /
   * `energy_sample` respectively; both selectors also independently return
   * `null` for any other kind). */
  kind: FactEvent["type"];
  eyebrow: string;
  title: string;
  sub: string;
  rows: InspectorRow[];
  rawJson: string;
}

function llmCallInspectorRows(event: Extract<FactEvent, { type: "llm_call" }>): InspectorRow[] {
  const p = event.payload;
  const a = event.attribution;
  const rows: InspectorRow[] = [
    { key: "provider", value: p.provider },
    { key: "model_id_requested", value: p.model_id_requested },
  ];
  if (p.model_id_served) rows.push({ key: "model_id_served", value: p.model_id_served });
  if (p.endpoint) rows.push({ key: "endpoint", value: p.endpoint });
  rows.push({ key: "input_tokens", value: p.usage.input_tokens !== undefined ? fmtTokens(p.usage.input_tokens) : "—" });
  rows.push({ key: "output_tokens", value: p.usage.output_tokens !== undefined ? fmtTokens(p.usage.output_tokens) : "—" });
  if (p.usage.thought_tokens !== undefined) rows.push({ key: "thought_tokens", value: fmtTokens(p.usage.thought_tokens) });
  if (p.usage.cached_read_tokens !== undefined) rows.push({ key: "cached_read_tokens", value: fmtTokens(p.usage.cached_read_tokens) });
  if (p.usage.cached_write_tokens !== undefined) rows.push({ key: "cached_write_tokens", value: fmtTokens(p.usage.cached_write_tokens) });
  const alarmSource = isAlarmUsageSource(p.usage_source);
  rows.push({ key: "usage_source", value: p.usage_source, tone: alarmSource ? "alarm" : undefined });
  if (p.duration_ms !== undefined) rows.push({ key: "duration_ms", value: fmtMs(p.duration_ms) });
  const status = p.status ?? "—";
  rows.push({ key: "status", value: status, tone: isErrorStatus(status) ? "alarm" : undefined });
  if (p.streaming !== undefined) rows.push({ key: "streaming", value: String(p.streaming) });
  rows.push({ key: "task_id", value: a?.task_id ?? "—" });
  rows.push({ key: "tool_call_id", value: a?.tool_call_id ?? "—" });
  return rows;
}

function actionSpanInspectorRows(event: Extract<FactEvent, { type: "action_span" }>): InspectorRow[] {
  const p = event.payload;
  const a = event.attribution;
  const remote = p.execution_locus === "remote";
  const status = p.status ?? "—";
  return [
    { key: "tool_kind", value: p.tool_kind },
    { key: "execution_locus", value: p.execution_locus, tone: remote ? "modelled" : undefined },
    { key: "t_start", value: p.t_start },
    { key: "t_end", value: p.t_end },
    { key: "pids", value: p.pids && p.pids.length > 0 ? p.pids.join(", ") : "none observed" },
    { key: "cgroup", value: p.cgroup ?? "not set" },
    { key: "status", value: status, tone: isErrorStatus(status) ? "alarm" : undefined },
    { key: "task_id", value: a?.task_id ?? "—" },
    { key: "subagent_id", value: a?.subagent_id ?? "—" },
    { key: "tool_call_id", value: a?.tool_call_id ?? "—" },
  ];
}

/** Parallel to `actionSpanInspectorRows`, sourced from a `SpanRecord`
 * instead of a `FactEvent` — the still-open-span case: no closing fact
 * exists yet, so `cgroup`/`attribution` (never retained on `SpanRecord`,
 * see `resolveSelection`'s comment) honestly render as unknown rather than
 * fabricated. */
function openActionSpanInspectorRows(rec: SpanRecord): InspectorRow[] {
  const remote = rec.execution_locus === "remote";
  const status = rec.status ?? "—";
  return [
    { key: "tool_kind", value: rec.tool_kind },
    { key: "execution_locus", value: rec.execution_locus, tone: remote ? "modelled" : undefined },
    { key: "t_start", value: rec.tStart },
    { key: "t_end", value: "— (open)" },
    { key: "pids", value: rec.pids && rec.pids.length > 0 ? rec.pids.join(", ") : "none observed" },
    { key: "cgroup", value: "not retained for an open span" },
    { key: "status", value: status, tone: isErrorStatus(status) ? "alarm" : undefined },
    { key: "task_id", value: "—" },
    { key: "subagent_id", value: "—" },
    { key: "tool_call_id", value: "—" },
  ];
}

/** Honest raw-JSON view for an open span: exactly the fields `SpanRecord`
 * retains (see `OpenSpanPayloadFields` in eventStore.ts) — never a
 * fabricated envelope (`ts`/`collector`/`event_id` were never stored for a
 * still-open span, since it isn't a ring `fact` frame). */
function openSpanRawView(rec: SpanRecord): Record<string, unknown> {
  return {
    span_id: rec.span_id,
    tool_name: rec.tool_name,
    tool_kind: rec.tool_kind,
    execution_locus: rec.execution_locus,
    status: rec.status,
    pids: rec.pids,
    t_start: rec.tStart,
    t_end: null,
  };
}

function energySampleInspectorRows(event: Extract<FactEvent, { type: "energy_sample" }>): InspectorRow[] {
  const p = event.payload;
  const rows: InspectorRow[] = [
    { key: "t_start", value: p.t_start },
    { key: "t_end", value: p.t_end },
  ];
  if (p.host_id) rows.push({ key: "host_id", value: p.host_id });
  p.components.forEach((c, i) => {
    const label = c.label ? ` · ${c.label}` : "";
    rows.push({
      key: `component[${i}] ${c.kind}`,
      value: `${fmtJoules(c.energy_j)} · ${c.method}${label}`,
      tone: isModelledMethod(c.method) ? "modelled" : undefined,
    });
  });
  return rows;
}

function processSampleInspectorRows(event: Extract<FactEvent, { type: "process_sample" }>): InspectorRow[] {
  const p = event.payload;
  const rows: InspectorRow[] = [
    { key: "t_start", value: p.t_start },
    { key: "t_end", value: p.t_end },
  ];
  p.processes.forEach((proc, i) => {
    rows.push({ key: `process[${i}] pid`, value: String(proc.pid) });
    rows.push({ key: `process[${i}] cpu_time_delta`, value: fmtMs(proc.cpu_time_delta_ms) });
  });
  return rows;
}

function sessionMetaInspectorRows(event: Extract<FactEvent, { type: "session_meta" }>): InspectorRow[] {
  const p = event.payload;
  const rows: InspectorRow[] = [
    { key: "agent_app", value: p.agent_app.version ? `${p.agent_app.name} ${p.agent_app.version}` : p.agent_app.name },
  ];
  if (p.os) rows.push({ key: "os", value: p.os });
  if (p.hardware?.cpu_model) rows.push({ key: "cpu_model", value: p.hardware.cpu_model });
  if (p.hardware?.gpu_models?.length) rows.push({ key: "gpu_models", value: p.hardware.gpu_models.join(", ") });
  if (p.hardware?.ram_gb !== undefined) rows.push({ key: "ram_gb", value: String(p.hardware.ram_gb) });
  if (p.geo_zone) rows.push({ key: "geo_zone", value: p.geo_zone });
  if (p.power_source) rows.push({ key: "power_source", value: p.power_source });
  return rows;
}

function inspectorRowsOf(event: FactEvent): InspectorRow[] {
  switch (event.type) {
    case "llm_call":
      return llmCallInspectorRows(event);
    case "action_span":
      return actionSpanInspectorRows(event);
    case "energy_sample":
      return energySampleInspectorRows(event);
    case "process_sample":
      return processSampleInspectorRows(event);
    case "session_meta":
      return sessionMetaInspectorRows(event);
  }
}

function inspectorTitleAndSub(event: FactEvent, tsMs: number): { title: string; sub: string } {
  switch (event.type) {
    case "llm_call":
      return { title: event.payload.model_id_requested, sub: `${event.payload.provider} · ${event.event_id}` };
    case "action_span": {
      const durationMs = Date.parse(event.payload.t_end) - Date.parse(event.payload.t_start);
      return { title: event.payload.tool_name, sub: `${event.payload.span_id} · ${fmtMs(durationMs)}` };
    }
    case "energy_sample": {
      const startMs = Date.parse(event.payload.t_start);
      const endMs = Date.parse(event.payload.t_end);
      return { title: "energy_sample", sub: `${fmtClock(startMs)} → ${fmtClock(endMs)}` };
    }
    case "process_sample":
      return { title: "process_sample", sub: fmtClock(tsMs) };
    case "session_meta":
      return { title: event.payload.agent_app.name, sub: event.payload.os ?? event.event_id };
  }
}

function buildInspector(selectedId: string | null): InspectorModel | null {
  if (selectedId === null) return null;
  const resolved = resolveSelection(selectedId);
  if (!resolved) return null;

  if (resolved.kind === "openSpan") {
    const rec = resolved.record;
    return {
      kind: "action_span",
      eyebrow: "action_span",
      title: rec.tool_name,
      sub: `${rec.span_id} · open`,
      rows: openActionSpanInspectorRows(rec),
      rawJson: JSON.stringify(openSpanRawView(rec), null, 2),
    };
  }

  const { event, tsMs } = resolved;
  const { title, sub } = inspectorTitleAndSub(event, tsMs);
  return {
    kind: event.type,
    eyebrow: event.type,
    title,
    sub,
    rows: inspectorRowsOf(event),
    rawJson: JSON.stringify(event, null, 2),
  };
}

const memoInspector = memo1((_rev: number, selectedId: string | null) => buildInspector(selectedId));

/** Resolves `selectedId` — an `event_id` OR a `span_id` (see this file's
 * header) — to one Inspector model. Every selection route (Timeline bar,
 * Stream row, correlated row, decision-log ref) funnels through
 * `uiStore.selectedId`, so all four produce an identical model for "the same
 * record" regardless of which id shape reached it. */
export function selectInspector(rev: number, selectedId: string | null): InspectorModel | null {
  return memoInspector(rev, selectedId);
}

// ---------------------------------------------------------------------------
// selectCorrelated (moved from stream.ts — see this file's header)
// ---------------------------------------------------------------------------

/** ±6s correlation window (SCREENS.md §2: "events within ±6s of the selection"). */
const CORRELATION_WINDOW_MS = 6000;

/** Correlated section cap (this task's brief: "capped at 20"). */
const CORRELATED_CAP = 20;

export interface CorrelatedRow {
  id: string;
  type: FactEvent["type"];
  summary: string;
  /** Signed ms offset from the selected event — negative is earlier. */
  offsetMs: number;
  /** e.g. "−2.1s" / "+0.4s" (DESIGN-SYSTEM/SCREENS.md §2). */
  offsetLabel: string;
}

function buildCorrelated(selectedId: string | null): CorrelatedRow[] {
  if (selectedId === null) return [];
  const resolved = resolveSelection(selectedId);
  if (!resolved) return [];
  // Correlation is a ts-neighbourhood concept, defined only for a fact with
  // its own `ts` — an open span (no ring fact yet) has none, so it simply
  // has no correlated section (consistent with "selecting an id not visible
  // on the current tab is fine": there is nothing dishonest about an empty
  // list here, since there IS no ts to correlate against yet).
  if (resolved.kind !== "fact") return [];
  const t = resolved.tsMs;
  const selectedEventId = resolved.event.event_id;

  const rows: CorrelatedRow[] = [];
  for (const { event, tsMs } of eventStore.facts) {
    if (event.event_id === selectedEventId) continue;
    const offsetMs = tsMs - t;
    if (Math.abs(offsetMs) > CORRELATION_WINDOW_MS) continue;
    rows.push({ id: event.event_id, type: event.type, summary: factsOf(event), offsetMs, offsetLabel: fmtOffset(offsetMs) });
  }
  rows.sort((a, b) => Math.abs(a.offsetMs) - Math.abs(b.offsetMs));
  return rows.slice(0, CORRELATED_CAP);
}

const memoCorrelated = memo1((_rev: number, selectedId: string | null) => buildCorrelated(selectedId));

export function selectCorrelated(rev: number, selectedId: string | null): CorrelatedRow[] {
  return memoCorrelated(rev, selectedId);
}

// ---------------------------------------------------------------------------
// ShareBarSegment — the generic shape ShareBar.svelte renders (also reused
// by Task 7's Attribution allocation table)
// ---------------------------------------------------------------------------

export interface ShareBarSegment {
  label: string;
  value_j: number;
  /** A server `share` value (AllocationRow.share / baseline.share), or —
   * only where the trace carries no `.share` field at all (agent_process) —
   * `allocated_j / total_j`: a display-scaling proportion, never a computed
   * attribution (DATA-CONTRACT §0 / global-constraints #1). */
  fraction: number;
  /** DESIGN-SYSTEM §3 share-bar fills: solid accent = attributed/measured,
   * accent-300 = the agent's own process, hatched neutral = baseline/idle
   * (also used for an excluded/remote row — same "not locally measured"
   * bucket as every other remote/modelled value in this app; alarm hatch
   * never appears in a share bar, per DESIGN-SYSTEM §3). */
  fill: "accent" | "accent300" | "neutral-hatch";
  title: string;
}

function fmtSampleIntervalLabel(startMs: number, endMs: number): string {
  return `${fmtClock(startMs)} → ${fmtClock(endMs)}`;
}

// ---------------------------------------------------------------------------
// Overlapping energy_sample lookup — shared by selectSpanEnergy and
// selectRelevantSampleIds
// ---------------------------------------------------------------------------

interface OverlappingSample {
  sampleEventId: string;
  startMs: number;
  endMs: number;
}

/** Every `energy_sample` fact whose own `[t_start, t_end)` overlaps
 * `[startMs, endMs)`. A plain interval filter over the fact ring — NOT an
 * indexed query (eventStore's bucket index only tracks `action_span`s, see
 * eventStore.ts), and not an attribution: this only decides WHICH
 * already-existing samples' traces are relevant, the same filtering
 * discipline `buildCorrelated`'s ±6s window already uses. Ascending by
 * start time so the per-sample list in the Inspector reads chronologically. */
function overlappingSamplesForInterval(startMs: number, endMs: number): OverlappingSample[] {
  const out: OverlappingSample[] = [];
  for (const { event } of eventStore.facts) {
    if (event.type !== "energy_sample") continue;
    const sStart = Date.parse(event.payload.t_start);
    const sEnd = Date.parse(event.payload.t_end);
    if (sStart < endMs && startMs < sEnd) out.push({ sampleEventId: event.event_id, startMs: sStart, endMs: sEnd });
  }
  out.sort((a, b) => a.startMs - b.startMs);
  return out;
}

// ---------------------------------------------------------------------------
// selectSpanEnergy
// ---------------------------------------------------------------------------

export interface SpanEnergySampleRow {
  sampleEventId: string;
  /** "hh:mm:ss.SSS → hh:mm:ss.SSS" for this sample's own interval. */
  label: string;
  status: "ready" | "pending" | "unavailable";
  /** Present only when `status === "ready"` — this span's share vs agent vs
   * baseline WITHIN this one sample's trace. Never a 0-fraction "this span"
   * segment when the trace carries no row for this span at all — see
   * `noRowNote`. */
  segments?: ShareBarSegment[];
  /** Set only when `status === "ready"` AND the trace carries no row at all
   * for this span (a real, honest state — the sample overlapped the span's
   * interval but attribution produced nothing for it) — Inspector.svelte
   * renders this as an explicit neutral line instead of a fabricated
   * 0-fraction "this span" bar segment (`buildFocusedSegments` leaves that
   * segment out of `segments` entirely in this case). */
  noRowNote?: string;
}

export interface SpanEnergyModel {
  spanId: string;
  /** Σ over every READY overlapping trace's rows matching `spanId`, of
   * `allocated_j` — DATA-CONTRACT §4: "Inspector 'energy · l2_cpu_time' for a
   * span = Σ rows[].allocated_j across overlapping traces" — the ONE
   * sanctioned span-level sum, a display aggregation of server-attributed
   * values, and ONLY of rows whose `span_id` matches this span. */
  totalJ: number;
  /** `fmtJoules(totalJ)` once at least one overlapping trace is ready;
   * otherwise an honest pending/unavailable/no-samples label — NEVER "0 J"
   * when nothing is actually known yet (global-constraints #6: "'not
   * measured' never rendered as 0"). */
  totalLabel: string;
  samples: SpanEnergySampleRow[];
}

/** `matchingRows.length === 0` (a trace ready, but carrying no row at all
 * for this span) is handled by the caller (`buildSpanEnergy`), which shows
 * an explicit neutral note instead of rendering a 0-fraction "this span"
 * segment — so this function never receives an empty `matchingRows`. */
function buildFocusedSegments(spanId: string, matchingRows: AllocationRow[], trace: AllocationTrace): ShareBarSegment[] {
  const spanJ = matchingRows.reduce((acc, r) => acc + r.allocated_j, 0);
  const spanShare = matchingRows.reduce((acc, r) => acc + r.share, 0);
  const anyExcluded = matchingRows.some((r) => r.excluded);
  const allExcluded = matchingRows.every((r) => r.excluded);
  const excludedNote = anyExcluded ? ` · excluded (${matchingRows.find((r) => r.excluded)?.excluded_reason ?? "remote"})` : "";
  const spanTitle = `${spanId} · ${fmtJoules(spanJ)} · ${fmtPct(spanShare)}${excludedNote}`;
  const agentFraction = trace.total_j > 0 ? trace.agent_process.allocated_j / trace.total_j : 0;

  return [
    { label: "this span", value_j: spanJ, fraction: spanShare, fill: allExcluded ? "neutral-hatch" : "accent", title: spanTitle },
    {
      label: "agent process",
      value_j: trace.agent_process.allocated_j,
      fraction: agentFraction,
      fill: "accent300",
      title: `agent process · ${fmtJoules(trace.agent_process.allocated_j)}`,
    },
    {
      label: trace.baseline.label,
      value_j: trace.baseline.allocated_j,
      fraction: trace.baseline.share,
      fill: "neutral-hatch",
      title: `${trace.baseline.label} · ${fmtJoules(trace.baseline.allocated_j)} · ${fmtPct(trace.baseline.share)}`,
    },
  ];
}

function buildSpanEnergy(selectedId: string | null): SpanEnergyModel | null {
  if (selectedId === null) return null;
  const resolved = resolveSelection(selectedId);
  const spanId = spanIdOfResolved(resolved);
  if (spanId === null) return null;
  const interval = spanIntervalOf(spanId);
  if (!interval) return null;

  const overlapping = overlappingSamplesForInterval(interval.startMs, interval.endMs);

  const samples: SpanEnergySampleRow[] = [];
  let totalJ = 0;
  let readyCount = 0;
  let pendingCount = 0;

  for (const s of overlapping) {
    const label = fmtSampleIntervalLabel(s.startMs, s.endMs);
    const entry = allocStore.get(s.sampleEventId);
    if (!entry) {
      pendingCount += 1;
      samples.push({ sampleEventId: s.sampleEventId, label, status: "pending" });
      continue;
    }
    if (entry.status === "unavailable") {
      samples.push({ sampleEventId: s.sampleEventId, label, status: "unavailable" });
      continue;
    }
    readyCount += 1;
    const matchingRows = entry.trace.rows.filter((r) => r.span_id === spanId);
    totalJ += matchingRows.reduce((acc, r) => acc + r.allocated_j, 0);
    if (matchingRows.length === 0) {
      // Ready trace, but no row at all for this span — an honest state, not
      // a 0-fraction "this span" bar segment (see `noRowNote`'s own doc
      // comment). Inspector.svelte renders this note instead of a ShareBar.
      samples.push({ sampleEventId: s.sampleEventId, label, status: "ready", noRowNote: "no allocation recorded for this span in this sample" });
      continue;
    }
    samples.push({ sampleEventId: s.sampleEventId, label, status: "ready", segments: buildFocusedSegments(spanId, matchingRows, entry.trace) });
  }

  const totalLabel =
    overlapping.length === 0 ? "no energy samples overlapping yet" : readyCount === 0 ? (pendingCount > 0 ? "trace pending" : "trace unavailable (outside window)") : fmtJoules(totalJ);

  return { spanId, totalJ, totalLabel, samples };
}

const memoSpanEnergy = memo1((_rev: number, _allocRev: number, selectedId: string | null) => buildSpanEnergy(selectedId));

/** `null` when the current selection isn't an action_span at all. `allocRev`
 * is a memo key only (this function never reads it directly) — it's what
 * makes the memoised result recompute once `allocStore.fetch` resolves and
 * the client's next tick flushes `allocStore.rev`. */
export function selectSpanEnergy(rev: number, allocRev: number, selectedId: string | null): SpanEnergyModel | null {
  return memoSpanEnergy(rev, allocRev, selectedId);
}

// ---------------------------------------------------------------------------
// selectSampleShare
// ---------------------------------------------------------------------------

export interface SampleShareModel {
  sampleEventId: string;
  status: "ready" | "pending" | "unavailable";
  /** Present only when `status === "ready"` — every span row + agent_process
   * + baseline segment of this sample's full trace. */
  segments?: ShareBarSegment[];
}

function rowFill(row: AllocationRow): ShareBarSegment["fill"] {
  // `execution_locus: remote` rows are excluded from the local energy join
  // — the same "not locally measured" bucket DESIGN-SYSTEM §3 gives every
  // other remote/modelled value elsewhere in this app (actionSpanInspectorRows'
  // own `modelled` tone, LaneChart's neutral hatch for remote spans), not the
  // alarm axis. This task's brief: "alarm hatch never appears in share bars
  // unless the row is an exclusion note" — the exclusion is flagged in the
  // segment's title text (`rowTitle` below), not by inventing a 4th fill
  // kind outside ShareBar's documented 3-value contract.
  return row.excluded ? "neutral-hatch" : "accent";
}

function rowTitle(row: AllocationRow): string {
  const base = `${row.tool_name} · ${fmtJoules(row.allocated_j)} · ${fmtPct(row.share)}`;
  return row.excluded ? `${base} · excluded (${row.excluded_reason ?? row.execution_locus})` : base;
}

function buildFullTraceSegments(trace: AllocationTrace): ShareBarSegment[] {
  const rowSegments: ShareBarSegment[] = trace.rows.map((row) => ({
    label: row.span_id,
    value_j: row.allocated_j,
    fraction: row.share,
    fill: rowFill(row),
    title: rowTitle(row),
  }));
  const agentFraction = trace.total_j > 0 ? trace.agent_process.allocated_j / trace.total_j : 0;
  return [
    ...rowSegments,
    {
      label: "agent process",
      value_j: trace.agent_process.allocated_j,
      fraction: agentFraction,
      fill: "accent300",
      title: trace.agent_process.note ? `agent process · ${fmtJoules(trace.agent_process.allocated_j)} · ${trace.agent_process.note}` : `agent process · ${fmtJoules(trace.agent_process.allocated_j)}`,
    },
    {
      label: trace.baseline.label,
      value_j: trace.baseline.allocated_j,
      fraction: trace.baseline.share,
      fill: "neutral-hatch",
      title: `${trace.baseline.label} · ${fmtJoules(trace.baseline.allocated_j)} · ${fmtPct(trace.baseline.share)}`,
    },
  ];
}

function buildSampleShare(selectedId: string | null): SampleShareModel | null {
  if (selectedId === null) return null;
  const resolved = resolveSelection(selectedId);
  if (!resolved || resolved.kind !== "fact" || resolved.event.type !== "energy_sample") return null;
  const sampleEventId = resolved.event.event_id;

  const entry = allocStore.get(sampleEventId);
  if (!entry) return { sampleEventId, status: "pending" };
  if (entry.status === "unavailable") return { sampleEventId, status: "unavailable" };
  return { sampleEventId, status: "ready", segments: buildFullTraceSegments(entry.trace) };
}

const memoSampleShare = memo1((_rev: number, _allocRev: number, selectedId: string | null) => buildSampleShare(selectedId));

/** `null` when the current selection isn't an energy_sample at all. */
export function selectSampleShare(rev: number, allocRev: number, selectedId: string | null): SampleShareModel | null {
  return memoSampleShare(rev, allocRev, selectedId);
}

// ---------------------------------------------------------------------------
// selectRelevantSampleIds — tells the tab container which traces to fetch
// ---------------------------------------------------------------------------

function computeRelevantSampleIds(selectedId: string | null): readonly string[] {
  if (selectedId === null) return [];
  const resolved = resolveSelection(selectedId);
  if (!resolved) return [];

  if (resolved.kind === "fact" && resolved.event.type === "energy_sample") {
    return [resolved.event.event_id];
  }

  const spanId = spanIdOfResolved(resolved);
  if (spanId === null) return [];
  const interval = spanIntervalOf(spanId);
  if (!interval) return [];
  return overlappingSamplesForInterval(interval.startMs, interval.endMs).map((s) => s.sampleEventId);
}

const memoRelevantSampleIds = memo1((_rev: number, selectedId: string | null) => computeRelevantSampleIds(selectedId));

/** Pure — never calls `allocStore.fetch`. The current selection's relevant
 * `sample_event_id`s (itself, for an energy_sample selection; every
 * overlapping sample, for an action_span selection) — the tab container
 * reads this and calls `allocStore.fetch` for whichever ids aren't cached
 * yet (`allocStore.get(id) === undefined`), per this task's brief: "the tab
 * container triggers fetches, the selector only reads". Doesn't need
 * `allocRev` as a memo key — it depends only on `eventStore` data (which
 * facts/spans exist), never on what's already in `allocStore`. */
export function selectRelevantSampleIds(rev: number, selectedId: string | null): readonly string[] {
  return memoRelevantSampleIds(rev, selectedId);
}
