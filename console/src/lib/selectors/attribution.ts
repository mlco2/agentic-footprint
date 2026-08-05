// Attribution tab selectors (Task 7 / M7; DATA-CONTRACT §2.4, §3.5, §4;
// SCREENS.md §3). Pure module: no Svelte imports, no `Date.now()`/
// `Math.random()` — everything here is a function of `eventStore`,
// `allocStore`, `sessionStore` and the caller's `selectedId`, per the file's
// own `(rev, allocRev, selectedId)` memo keys.
//
// This is the one place that renders `alloc.*` (AllocationTrace) fields into
// the sample list, the six-stat detail header, the sample-interval strip,
// the components table, the allocation table (with the agent-process and
// baseline/idle rows) and the policy aside's formula. Global-constraints #1
// applies with unusual force here: EXACTLY two display aggregations are
// sanctioned in this file —
//   1. avg power = total_j / interval seconds (buildStats)
//   2. attributed-J = Σ rows[].allocated_j (buildStats)
// — every other number rendered anywhere in this file's output is a trace
// field (or an energy_sample fact field) formatted through format.ts
// verbatim. This claim is scoped to THIS FILE ONLY — attribution.ts itself
// never computes `agent_process.allocated_j / total_j` as a display-scaling
// proportion, so a third aggregation doesn't sneak in here on top of the two
// above. The agent row's share column/bar is honestly "—" instead (see
// `buildAllocationTable`). That ratio IS computed elsewhere, sanctioned:
// selectors/inspector.ts's `buildFocusedSegments`/`buildFullTraceSegments`
// (ShareBarSegment helpers, Task 6; inspector.ts:464,570) compute the exact
// same `agent_process.allocated_j / total_j` fraction as pure ShareBar
// geometry (a segment's width as a fraction of the bar, not a new number
// rendered as text) — that's a second sanctioned display aggregation living
// in a different file, not a violation of this file's own two-aggregation
// budget above.
//
// `buildAside`'s formula substitution picks the trace's largest-share ROW
// (an argmax/selection over `.share`, comparison only) and typesets ITS
// existing fields verbatim into the formula shape — this is not a third
// aggregation: no arithmetic is performed on the numbers, only a choice of
// which already-real row to display (the same kind of selection
// `selectSampleList`'s "newest first" sort already performs on `ts`).
import type { FactEvent } from "../types/contract1";
import type { AllocationRow, AllocationTrace } from "../types/debug";
import { eventStore } from "../stores/eventStore.svelte";
import { allocStore } from "../stores/allocStore.svelte";
import { sessionStore } from "../stores/sessionStore.svelte";
import { fmtClock, fmtGridIntensity, fmtJoules, fmtMs, fmtMsCount, fmtPct, fmtWatts } from "../format";
import { isModelledMethod } from "./factFormat";
import type { ShareBarSegment } from "./inspector";
import { memo1 } from "./memo";
import { clamp, round3, widthWithFloor } from "./geometry";

type EnergySampleEvent = Extract<FactEvent, { type: "energy_sample" }>;

function isEnergySample(event: FactEvent): event is EnergySampleEvent {
  return event.type === "energy_sample";
}

function intervalLabel(startMs: number, endMs: number): string {
  return `${fmtClock(startMs)} → ${fmtClock(endMs)}`;
}

// ---------------------------------------------------------------------------
// selectSampleList
// ---------------------------------------------------------------------------

export interface SampleRow {
  sampleEventId: string;
  /** "hh:mm:ss → hh:mm:ss", from the energy_sample fact itself — available
   * even before/without a trace, so a pending row still shows its interval. */
  intervalLabel: string;
  selected: boolean;
  status: "ready" | "pending" | "unavailable";
  /** Total joules (cyan) — present only when `status === "ready"`. */
  totalLabel?: string;
  /** "N spans · idle N%" — idle share is `trace.baseline.share`, server-
   * supplied verbatim; N spans is `trace.rows.length` (a count, not a
   * computed value). Present only when `status === "ready"`. */
  metaLabel?: string;
  /** Magenta "L1 N%" flag, present only when `status === "ready"` AND
   * `l1_shadow_sum_share > 1` — paired with `l1FlagClass` so the rationed
   * magenta styling is a selector-owned decision (mirrors
   * selectors/stream.ts's `statusClass` convention), never a component
   * guessing at when to apply it. */
  l1FlagLabel?: string;
  l1FlagClass?: "status-alarm";
  /** Honest degraded-state text ("trace pending" / "trace unavailable
   * (outside window)") — present only when `status !== "ready"`. */
  pendingLabel?: string;
}

function buildSampleRow(event: EnergySampleEvent, selectedId: string | null): SampleRow {
  const startMs = Date.parse(event.payload.t_start);
  const endMs = Date.parse(event.payload.t_end);
  const label = intervalLabel(startMs, endMs);
  const selected = event.event_id === selectedId;

  const entry = allocStore.get(event.event_id);
  if (!entry) {
    return { sampleEventId: event.event_id, intervalLabel: label, selected, status: "pending", pendingLabel: "trace pending" };
  }
  if (entry.status === "unavailable") {
    return { sampleEventId: event.event_id, intervalLabel: label, selected, status: "unavailable", pendingLabel: "trace unavailable (outside window)" };
  }

  const trace = entry.trace;
  const spanWord = trace.rows.length === 1 ? "span" : "spans";
  const metaLabel = `${trace.rows.length} ${spanWord} · idle ${fmtPct(trace.baseline.share)}`;
  const l1Over = trace.l1_shadow_sum_share > 1;
  return {
    sampleEventId: event.event_id,
    intervalLabel: label,
    selected,
    status: "ready",
    totalLabel: fmtJoules(trace.total_j),
    metaLabel,
    l1FlagLabel: l1Over ? `L1 ${fmtPct(trace.l1_shadow_sum_share)}` : undefined,
    l1FlagClass: l1Over ? "status-alarm" : undefined,
  };
}

function computeSampleList(selectedId: string | null, sessionId: string | null): SampleRow[] {
  const samples = eventStore.facts.filter(
    (f): f is { event: EnergySampleEvent; tsMs: number } =>
      isEnergySample(f.event) && (sessionId === null || f.event.session_id === sessionId),
  );
  // Newest first (SCREENS.md §3) — by the fact's own `ts`, not ring/arrival
  // order (global-constraints #7: "stream/table rows sorted by ts, not
  // arrival"). `Array.sort` is stable, so genuine ties keep arrival order.
  samples.sort((a, b) => b.tsMs - a.tsMs);
  return samples.map(({ event }) => buildSampleRow(event, selectedId));
}

const memoSampleList = memo1((_rev: number, _allocRev: number, selectedId: string | null, sessionId: string | null) => computeSampleList(selectedId, sessionId));

/** Every energy_sample of the picked session, newest first — attribution is
 * a per-session computation, so the list scopes to the masthead picker
 * (`sessionId === null` means no session is known yet and nothing is
 * filtered). Rows for a sample whose trace hasn't arrived yet render
 * `pendingLabel` instead of fabricating totals — this selector never calls
 * `allocStore.fetch` itself; the tab container fetches whichever of these
 * ids aren't cached yet (brief: "trigger nothing themselves"). */
export function selectSampleList(rev: number, allocRev: number, selectedId: string | null, sessionId: string | null): SampleRow[] {
  return memoSampleList(rev, allocRev, selectedId, sessionId);
}

// ---------------------------------------------------------------------------
// Resolving the selected sample (energy_sample only — a span_id selection
// elsewhere just means this tab has nothing to show, which is fine: an
// honest empty detail, not an error)
// ---------------------------------------------------------------------------

function resolveSelectedSample(selectedId: string | null): EnergySampleEvent | null {
  if (selectedId === null) return null;
  for (const { event } of eventStore.facts) {
    if (isEnergySample(event) && event.event_id === selectedId) return event;
  }
  return null;
}

// ---------------------------------------------------------------------------
// selectAllocationDetail
// ---------------------------------------------------------------------------

export interface StatTile {
  label: string;
  value: string;
  tone?: "alarm";
}

/** SCREENS.md §3: "spans drawn to scale inside the sample's bounds, 19px
 * rows" — plural rows. Each span gets its OWN 19px band (stacked by `topPx`,
 * in `trace.rows` order — a display order, not a computed value), never
 * absolutely-positioned siblings sharing one track where overlapping spans
 * would occlude each other. */
export const STRIP_ROW_HEIGHT_PX = 19;

export interface IntervalStripRow {
  spanId: string;
  label: string;
  topPx: number;
  leftPct: number;
  widthPct: number;
  fillVar: string;
  hatch: "none" | "neutral";
  title: string;
}

export interface ComponentRow {
  kind: string;
  label: string;
  jouleLabel: string;
  method: string;
  /** DESIGN-SYSTEM §3: solid = measured (rapl/powermetrics/nvml), hatched = modelled (tdp_model). */
  hatched: boolean;
}

export interface AllocTableRow {
  key: string;
  kind: "span" | "agent" | "baseline";
  label: string;
  locusLabel: string;
  overlapLabel: string;
  cpuDeltaLabel: string;
  /** Single-segment bar for the "l2 share" column — `[]` for the agent row
   * (agent_process carries no `.share` field on the wire; see this file's
   * header comment on the two-aggregation discipline). */
  shareSegments: ShareBarSegment[];
  shareLabel: string;
  l2JoulesLabel: string;
  l1JoulesLabel: string;
  excluded: boolean;
  /** `row.excluded_reason` (span rows) or `agent_process.note` (the agent
   * row) — DATA-CONTRACT real-server nuance: the note explains the
   * agent-process bucket doubles as the orphan bucket under `l2_cpu_time/v1`.
   * Rendered as the row's secondary line when present. */
  noteLabel?: string;
}

export interface Note {
  text: string;
  tone?: "alarm";
}

export interface DetailModel {
  sampleEventId: string;
  intervalLabel: string;
  status: "ready" | "pending" | "unavailable";
  /** Present only when `status === "ready"`. */
  stats?: StatTile[];
  intervalStrip?: IntervalStripRow[];
  components?: ComponentRow[];
  allocationRows?: AllocTableRow[];
  notes?: Note[];
}

/** Floor so a zero/near-zero-overlap row is still a visible sliver in the
 * interval strip, not an invisible 0-width box (mirrors timeline.ts's own
 * `MIN_BAR_WIDTH_PCT`). Pixel/percent geometry, not an attribution number —
 * never rendered as text. */
const MIN_STRIP_WIDTH_PCT = 1.5;

function buildStats(trace: AllocationTrace, sampleStartMs: number, sampleEndMs: number): StatTile[] {
  const intervalS = Math.max(0.001, (sampleEndMs - sampleStartMs) / 1000);
  // SANCTIONED aggregation #1: avg power = total_j / interval seconds.
  const avgPowerW = trace.total_j / intervalS;
  // SANCTIONED aggregation #2: attributed J = Σ rows[].allocated_j.
  const attributedJ = trace.rows.reduce((acc, r) => acc + r.allocated_j, 0);
  const l1Over = trace.l1_shadow_sum_share > 1;

  return [
    { label: "total measured", value: fmtJoules(trace.total_j) },
    { label: "avg power", value: fmtWatts(avgPowerW) },
    { label: "overlapping spans", value: String(trace.rows.length) },
    { label: "attributed", value: fmtJoules(attributedJ) },
    { label: "baseline / idle", value: `${fmtJoules(trace.baseline.allocated_j)} · ${fmtPct(trace.baseline.share)}` },
    { label: "l1 would sum to", value: fmtPct(trace.l1_shadow_sum_share), tone: l1Over ? "alarm" : undefined },
  ];
}

function stripRowTitle(row: AllocationRow): string {
  const base = `${row.tool_name} · ${fmtMs(row.overlap_ms)} overlap · ${fmtJoules(row.allocated_j)}`;
  return row.excluded ? `${base} · excluded (${row.excluded_reason ?? row.execution_locus})` : base;
}

/** Positions each row to scale within the sample's own bounds. A row's
 * real interval comes from `eventStore.spans` (the span_id's own
 * `t_start`/`t_end`) — the trace itself carries only `overlap_ms` (a
 * duration), not where in the interval it fell. When the span record is no
 * longer resolvable (e.g. ring-evicted), this honestly falls back to a
 * left-anchored bar sized by `overlap_ms` alone, rather than fabricating a
 * position or silently dropping the row. */
function buildIntervalStrip(trace: AllocationTrace, sampleStartMs: number, sampleEndMs: number): IntervalStripRow[] {
  const durationMs = Math.max(1, sampleEndMs - sampleStartMs);
  return trace.rows.map((row, index) => {
    const rec = eventStore.spans.get(row.span_id);
    let leftPct: number;
    let widthPct: number;
    if (rec) {
      const rowStartMs = clamp(rec.tStartMs, sampleStartMs, sampleEndMs);
      const rowEndMs = clamp(rec.tEndMs ?? sampleEndMs, sampleStartMs, sampleEndMs);
      leftPct = round3(((rowStartMs - sampleStartMs) / durationMs) * 100);
      widthPct = round3(widthWithFloor(((rowEndMs - rowStartMs) / durationMs) * 100, MIN_STRIP_WIDTH_PCT));
    } else {
      leftPct = 0;
      widthPct = round3(widthWithFloor((row.overlap_ms / durationMs) * 100, MIN_STRIP_WIDTH_PCT));
    }
    return {
      spanId: row.span_id,
      label: row.tool_name,
      // Own 19px row per span (SCREENS.md §3: "19px rows", plural) — stacked
      // in `trace.rows` order, so overlapping spans never occlude each other
      // as siblings in one shared track.
      topPx: index * STRIP_ROW_HEIGHT_PX,
      leftPct,
      widthPct,
      fillVar: row.excluded ? "transparent" : "var(--color-accent)",
      hatch: row.excluded ? "neutral" : "none",
      title: stripRowTitle(row),
    };
  });
}

function buildComponents(trace: AllocationTrace): ComponentRow[] {
  return trace.components.map((c) => ({
    kind: c.kind,
    label: c.label ?? c.kind,
    jouleLabel: fmtJoules(c.energy_j),
    method: c.method,
    hatched: isModelledMethod(c.method),
  }));
}

function buildAllocationTable(trace: AllocationTrace): AllocTableRow[] {
  const rows: AllocTableRow[] = trace.rows.map((row) => ({
    key: row.span_id,
    kind: "span",
    label: row.tool_name,
    locusLabel: row.execution_locus,
    overlapLabel: fmtMs(row.overlap_ms),
    cpuDeltaLabel: fmtMs(row.cpu_delta_ms),
    shareSegments: [
      {
        label: row.tool_name,
        value_j: row.allocated_j,
        fraction: row.share,
        fill: row.excluded ? "neutral-hatch" : "accent",
        title: stripRowTitle(row),
      },
    ],
    shareLabel: fmtPct(row.share),
    l2JoulesLabel: fmtJoules(row.allocated_j),
    l1JoulesLabel: fmtJoules(row.l1_allocated_j),
    excluded: row.excluded,
    noteLabel: row.excluded ? (row.excluded_reason ?? undefined) : undefined,
  }));

  rows.push({
    key: "agent_process",
    kind: "agent",
    label: "agent process",
    locusLabel: "agent",
    overlapLabel: "—",
    // agent_process carries no `.share` field on the wire — honestly "—",
    // never `allocated_j / total_j` (this file's header comment: that
    // exception is Task 6/Inspector-specific, not sanctioned here).
    cpuDeltaLabel: fmtMs(trace.agent_process.cpu_delta_ms),
    shareSegments: [],
    shareLabel: "—",
    l2JoulesLabel: fmtJoules(trace.agent_process.allocated_j),
    l1JoulesLabel: "—",
    excluded: false,
    noteLabel: trace.agent_process.note,
  });

  rows.push({
    key: "baseline",
    kind: "baseline",
    label: `${trace.baseline.label} · not attributed to any action`,
    locusLabel: "—",
    overlapLabel: "—",
    cpuDeltaLabel: "—",
    shareSegments: [
      {
        label: trace.baseline.label,
        value_j: trace.baseline.allocated_j,
        fraction: trace.baseline.share,
        fill: "neutral-hatch",
        title: `${trace.baseline.label} · ${fmtJoules(trace.baseline.allocated_j)} · ${fmtPct(trace.baseline.share)}`,
      },
    ],
    shareLabel: fmtPct(trace.baseline.share),
    l2JoulesLabel: fmtJoules(trace.baseline.allocated_j),
    l1JoulesLabel: "—",
    excluded: false,
  });

  return rows;
}

function buildNotes(trace: AllocationTrace): Note[] {
  const notes: Note[] = [];

  if (trace.l1_shadow_sum_share > 1) {
    notes.push({
      text: `L1 (wall-clock) shadow policy would over-attribute this sample — its rows would sum to ${fmtPct(
        trace.l1_shadow_sum_share,
      )} of the total. L2 (cpu-time) is used instead, so every row's share stays bounded by actual cpu-time consumed.`,
      tone: "alarm",
    });
  }

  const excludedRows = trace.rows.filter((r) => r.excluded);
  if (excludedRows.length > 0) {
    const reasons = Array.from(new Set(excludedRows.map((r) => r.excluded_reason ?? r.execution_locus))).join("; ");
    notes.push({
      text: `${excludedRows.length} span${excludedRows.length === 1 ? "" : "s"} excluded from local energy attribution: ${reasons}.`,
    });
  }

  notes.push({
    text: `${fmtJoules(trace.baseline.allocated_j)} (${fmtPct(trace.baseline.share)}) of this sample is baseline/idle draw — not attributed to any watched action.`,
  });

  return notes;
}

function buildDetail(selectedId: string | null): DetailModel | null {
  const event = resolveSelectedSample(selectedId);
  if (!event) return null;

  const sampleStartMs = Date.parse(event.payload.t_start);
  const sampleEndMs = Date.parse(event.payload.t_end);
  const label = intervalLabel(sampleStartMs, sampleEndMs);

  const entry = allocStore.get(event.event_id);
  if (!entry) return { sampleEventId: event.event_id, intervalLabel: label, status: "pending" };
  if (entry.status === "unavailable") return { sampleEventId: event.event_id, intervalLabel: label, status: "unavailable" };

  const trace = entry.trace;
  // The trace carries its own t_start/t_end (verbatim) — used here instead
  // of the fact's, since the trace is the authoritative source for every
  // other number in this model and the two should agree exactly.
  const traceStartMs = Date.parse(trace.t_start);
  const traceEndMs = Date.parse(trace.t_end);

  return {
    sampleEventId: event.event_id,
    intervalLabel: label,
    status: "ready",
    stats: buildStats(trace, traceStartMs, traceEndMs),
    intervalStrip: buildIntervalStrip(trace, traceStartMs, traceEndMs),
    components: buildComponents(trace),
    allocationRows: buildAllocationTable(trace),
    notes: buildNotes(trace),
  };
}

const memoDetail = memo1((_rev: number, _allocRev: number, selectedId: string | null) => buildDetail(selectedId));

/** `null` only when nothing resolvable as an energy_sample is selected at
 * all (no selection, or the selection is some other record kind — an
 * action_span selected on Timeline/Stream, say). Once a real energy_sample
 * is selected, this ALWAYS returns a model — `status` carries the honest
 * pending/unavailable/ready distinction (mirrors `SampleShareModel`'s own
 * convention in selectors/inspector.ts), so the tab never has to guess why
 * the detail column is empty. */
export function selectAllocationDetail(rev: number, allocRev: number, selectedId: string | null): DetailModel | null {
  return memoDetail(rev, allocRev, selectedId);
}

// ---------------------------------------------------------------------------
// selectPolicyAside
// ---------------------------------------------------------------------------

export interface FormulaRow {
  key: string;
  label: string;
  cpuDeltaLabel: string;
  /** "—" for the agent-process row (no `.share` field on the wire). */
  shareLabel: string;
  allocatedLabel: string;
}

/** The formula's two lines, typeset with ONE span row's real numbers
 * substituted in place of the symbolic variables — SCREENS.md: "formula with
 * this sample's real numbers substituted". Every number here is `fmtMsCount`/
 * `fmtPct`/`fmtJoules` applied to a value read verbatim off the chosen row
 * (`cpu_delta_ms`, `share`, `allocated_j`) or the trace (`denominator_cpu_ms`,
 * `total_j`) — the division/multiplication shown is the SERVER's own
 * `share`/`allocated_j`, typeset into the equation shape, never recomputed
 * here (brief: "do NOT recompute and display your own arithmetic"). */
export interface FormulaSubstitution {
  /** Which row was substituted, e.g. "Bash(cargo test) — largest share this sample". */
  label: string;
  /** e.g. "share = 716 ms / 32,000 ms = 2.2%". */
  shareLine: string;
  /** e.g. "alloc_j = 2.2% × 88 J = 1.90 J". */
  allocLine: string;
}

export interface AsideModel {
  sampleEventId: string;
  status: "ready" | "pending" | "unavailable";
  /** Present only when `status === "ready"`. */
  policyId?: string;
  policyProse?: string[];
  /** The two literal formula lines (static template text, substituted with
   * real per-row numbers in `formulaRows`/`formulaSubstitution`, never
   * recomputed here): "share_i = cpu_delta_ms_i / denominator_cpu_ms" and
   * "alloc_j_i = share_i × total_j". */
  formulaLines?: string[];
  /** The largest-share row's own numbers typeset into the formula shape —
   * `undefined` only when the trace has no rows at all (nothing to
   * substitute honestly). */
  formulaSubstitution?: FormulaSubstitution;
  denominatorLabel?: string;
  denominatorNote?: string;
  totalJLabel?: string;
  /** Per-span (+ agent_process) cpu-delta/share/allocated rows — the
   * formula's variables substituted with THIS sample's real, verbatim trace
   * numbers (brief: "do NOT recompute and display your own arithmetic"). */
  formulaRows?: FormulaRow[];
  gridZone?: string;
  gridIntensityLabel?: string;
  /** Literal "auto-geolocated: never" (SCREENS.md §3), rendered in cyan. */
  geoNoteLabel?: string;
}

/** The row with the largest `.share` — a SELECTION (comparison only, no
 * arithmetic on the numbers) of which already-real row to typeset into the
 * formula, not a new aggregated value. `undefined` when the trace has no
 * rows at all. */
function largestShareRow(trace: AllocationTrace): AllocationRow | undefined {
  if (trace.rows.length === 0) return undefined;
  return trace.rows.reduce((best, row) => (row.share > best.share ? row : best), trace.rows[0]);
}

function buildFormulaSubstitution(trace: AllocationTrace): FormulaSubstitution | undefined {
  const row = largestShareRow(trace);
  if (!row) return undefined;
  return {
    label: `${row.tool_name} — largest share this sample`,
    shareLine: `share = ${fmtMsCount(row.cpu_delta_ms)} / ${fmtMsCount(trace.denominator_cpu_ms)} = ${fmtPct(row.share)}`,
    allocLine: `alloc_j = ${fmtPct(row.share)} × ${fmtJoules(trace.total_j)} = ${fmtJoules(row.allocated_j)}`,
  };
}

const POLICY_PROSE = [
  "L2 (cpu-time) attribution: each watched span's share of a sample's energy is its cpu-time delta over the machine's TOTAL cpu-time in the interval — not its wall-clock overlap.",
  "The denominator is machine-wide, not the sum of watched trees, so attributed spans + the agent process never silently consume 100% of a sample — the remainder is baseline/idle, made explicit rather than folded into an action's numbers.",
];

const FORMULA_LINES = ["share_i = cpu_delta_ms_i / denominator_cpu_ms", "alloc_j_i = share_i × total_j"];

const GEO_NOTE = "auto-geolocated: never";

function buildAside(selectedId: string | null): AsideModel | null {
  const event = resolveSelectedSample(selectedId);
  if (!event) return null;

  const entry = allocStore.get(event.event_id);
  if (!entry) return { sampleEventId: event.event_id, status: "pending" };
  if (entry.status === "unavailable") return { sampleEventId: event.event_id, status: "unavailable" };

  const trace = entry.trace;
  const formulaRows: FormulaRow[] = trace.rows.map((row) => ({
    key: row.span_id,
    label: row.tool_name,
    cpuDeltaLabel: fmtMs(row.cpu_delta_ms),
    shareLabel: fmtPct(row.share),
    allocatedLabel: fmtJoules(row.allocated_j),
  }));
  formulaRows.push({
    key: "agent_process",
    label: "agent process",
    cpuDeltaLabel: fmtMs(trace.agent_process.cpu_delta_ms),
    shareLabel: "—",
    allocatedLabel: fmtJoules(trace.agent_process.allocated_j),
  });

  const session = sessionStore.data;

  return {
    sampleEventId: event.event_id,
    status: "ready",
    policyId: trace.attribution_policy,
    policyProse: POLICY_PROSE,
    formulaLines: FORMULA_LINES,
    formulaSubstitution: buildFormulaSubstitution(trace),
    denominatorLabel: fmtMs(trace.denominator_cpu_ms),
    denominatorNote: trace.denominator_note,
    totalJLabel: fmtJoules(trace.total_j),
    formulaRows,
    gridZone: session?.grid.zone,
    gridIntensityLabel: session ? fmtGridIntensity(session.grid.g_co2e_per_kwh, session.grid.source) : undefined,
    geoNoteLabel: GEO_NOTE,
  };
}

const memoAside = memo1((_rev: number, _allocRev: number, selectedId: string | null) => buildAside(selectedId));

/** `null` only when nothing resolvable as an energy_sample is selected —
 * same convention as `selectAllocationDetail`. */
export function selectPolicyAside(rev: number, allocRev: number, selectedId: string | null): AsideModel | null {
  return memoAside(rev, allocRev, selectedId);
}
