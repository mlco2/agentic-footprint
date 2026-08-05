// Health tab selectors (Task 9 / M9; DATA-CONTRACT §2.7, §3.5, §4;
// SCREENS.md §5). Pure module: no Svelte imports, no `Date.now()`/
// `Math.random()` — `nowMs` always arrives as an argument from
// `uiStore.nowMs` (global-constraints.md #6), and `health`/`session` are the
// replace-on-arrival objects `healthStore.data`/`sessionStore.data` already
// hold — the same "the object IS the memo key" pattern selectors/impact.ts
// uses for `reportStore`/`sessionStore`, not a synthetic revision counter.
//
// Global-constraints #1 ("the client computes nothing") applies at full
// force: every collector-table/conformance/rejected/ingestion field below is
// a health-payload field typeset through format.ts verbatim, or a plain
// idle-ms subtraction used ONLY to pick a dot class (display bucketing, not
// a reported quantity — mirrors selectors/timeline.ts's own `selectRail`
// idle-dot convention, which this file deliberately reuses the exact
// thresholds of rather than re-deriving them).
import type { CollectorHealth, HealthPayload, PythonDoctorRow, SessionInfo } from "../types/debug";
import type { WatchdogRailRow } from "./timeline";
import { eventStore } from "../stores/eventStore.svelte";
import { fmtBytes, fmtClock, fmtCount, fmtCpuPct, fmtEventsPerS, fmtMs, fmtPct } from "../format";
import { memo1 } from "./memo";

// ---------------------------------------------------------------------------
// Shared idle-dot convention (SCREENS.md §5: "cyan < 12s idle, neutral-400
// < 45s, magenta beyond. Clamp idle at 0" — the exact thresholds
// selectors/timeline.ts's `selectRail` already uses for the Timeline rail's
// collector dots; duplicated here as two constants + one function rather
// than imported, because timeline.ts's `dotClassFor` is module-private and
// importing eventStore's own rail concept into the Health tab would be the
// wrong-direction coupling — Health owns collector health, Timeline
// borrows/reuses ITS OWN idle read of the same idea from eventStore's
// last-seen map, a genuinely different data source (health.collectors[]
// here vs. eventStore.perCollectorLastSeenMs there).
// ---------------------------------------------------------------------------

export type DotClass = "dot-accent" | "dot-neutral" | "dot-alarm";

const IDLE_ACCENT_MS = 12_000;
const IDLE_NEUTRAL_MS = 45_000;

function dotClassForIdle(idleMs: number): DotClass {
  if (idleMs < IDLE_ACCENT_MS) return "dot-accent";
  if (idleMs < IDLE_NEUTRAL_MS) return "dot-neutral";
  return "dot-alarm";
}

// ---------------------------------------------------------------------------
// selectCollectorTable
// ---------------------------------------------------------------------------

export interface CollectorTableRow {
  name: string;
  version: string;
  transport: string;
  events: number;
  rateLabel: string;
  rejected: number;
  lastSeenLabel: string;
  emitsLabel: string;
  /** Clamped `>= 0` ms since `last_seen` (SCREENS.md §5: "Clamp idle at
   * 0 — spans stamped at end time can be marginally ahead of `now`") —
   * exposed alongside `dotClass` (rather than folded away) so the clamp
   * itself stays independently verifiable: every idle value in the
   * accent tier renders the same dot regardless of whether it was
   * clamped, so `dotClass` alone can't prove the clamp happened. */
  idleMs: number;
  dotClass: DotClass;
}

function buildCollectorRow(c: CollectorHealth, nowMs: number): CollectorTableRow {
  const lastSeenMs = Date.parse(c.last_seen);
  // "Clamp idle at 0" (SCREENS.md §5): end-stamped events/health snapshots
  // can be marginally ahead of the client's own `nowMs` — never render a
  // negative idle.
  const idleMs = Math.max(0, nowMs - lastSeenMs);
  return {
    name: c.name,
    version: c.version,
    transport: c.transport,
    events: c.events,
    rateLabel: fmtEventsPerS(c.events_per_s),
    rejected: c.rejected,
    lastSeenLabel: fmtClock(lastSeenMs),
    emitsLabel: c.emits.join(", "),
    idleMs,
    dotClass: dotClassForIdle(idleMs),
  };
}

function computeCollectorTable(health: HealthPayload | undefined, nowMs: number): CollectorTableRow[] {
  if (!health) return [];
  return health.collectors.map((c) => buildCollectorRow(c, nowMs));
}

const memoCollectorTable = memo1((health: HealthPayload | undefined, nowMs: number) => computeCollectorTable(health, nowMs));

/** One row per `health.collectors[]`, `[]` before the first health payload
 * arrives (SCREENS.md §5 collector table; global-constraints.md #6's empty
 * state is the caller's job, not this selector's). */
export function selectCollectorTable(health: HealthPayload | undefined, nowMs: number): CollectorTableRow[] {
  return memoCollectorTable(health, nowMs);
}

// ---------------------------------------------------------------------------
// selectConformance — Gap #9, deferred by decision (brief: BOTH branches)
// ---------------------------------------------------------------------------

export interface ConformanceBarRow {
  field: string;
  pctLabel: string;
  fractionLabel: string;
  /** 0..100, clamped — the 4px bar's width. */
  barPct: number;
  /** Reuses `.dot-accent`/`.dot-neutral`/`.dot-alarm` as background-color
   * classes on the bar fill, not as literal status dots — same three
   * Broadsheet tones (console.css), same thresholds' semantics. */
  colorClass: DotClass;
  note?: string;
}

export type ConformanceModel = { kind: "pending" } | { kind: "bars"; rows: ConformanceBarRow[] };

function conformanceColorClass(pct: number): ConformanceBarRow["colorClass"] {
  // SCREENS.md §5: "4px bar (cyan > 90%, neutral 60–90%, magenta below)".
  if (pct > 90) return "dot-accent";
  if (pct >= 60) return "dot-neutral";
  return "dot-alarm";
}

function computeConformance(health: HealthPayload | undefined): ConformanceModel {
  // `health.conformance` absent (mock's case, and the real server's case —
  // docs/design-log.md: "`conformance` is deliberately absent") OR `health`
  // itself not yet arrived: both read as "not counted", never as an empty
  // table of zeroes (HealthPayload's own doc comment). Gap #9 is DEFERRED
  // BY DECISION — this pending panel is the M1 empty-state text verbatim.
  if (!health || health.conformance === undefined) return { kind: "pending" };

  const rows: ConformanceBarRow[] = health.conformance.map((row) => {
    const pct = row.total > 0 ? (row.present / row.total) * 100 : 0;
    return {
      field: row.field,
      pctLabel: fmtPct(row.total > 0 ? row.present / row.total : 0),
      fractionLabel: `${fmtCount(row.present)}/${fmtCount(row.total)}`,
      barPct: Math.max(0, Math.min(100, pct)),
      colorClass: conformanceColorClass(pct),
      note: row.note,
    };
  });
  return { kind: "bars", rows };
}

const memoConformance = memo1((health: HealthPayload | undefined) => computeConformance(health));

/** Pending-panel vs. bars, per gap #9's deferred-decision state (brief:
 * "Implement BOTH branches"). */
export function selectConformance(health: HealthPayload | undefined): ConformanceModel {
  return memoConformance(health);
}

// ---------------------------------------------------------------------------
// selectRejected
// ---------------------------------------------------------------------------

export interface RejectedRow {
  tsLabel: string;
  reason: string;
  origin: string;
  /** 1-based line number, from the start of the file (docs/design-log.md,
   * "`af_spool::tail` reject shape changed") — typeset via `fmtCount`
   * (grouped, no unit), never `fmtBytes` (a size-scaling formatter that
   * would misrepresent a line number as a magnitude). */
  lineLabel: string;
  /** The rejected line's byte position within the origin file — same
   * `fmtCount` reasoning as `lineLabel`; NOT `fmtBytes`, which this row's
   * doc comment on `fmtCount` (format.ts) explains is the wrong formatter
   * for a cursor position rather than a size. */
  byteOffsetLabel: string;
  raw: string;
}

function computeRejected(health: HealthPayload | undefined): RejectedRow[] {
  if (!health) return [];
  return health.rejected.map((r) => ({
    tsLabel: fmtClock(Date.parse(r.ts)),
    reason: r.reason,
    origin: r.origin,
    lineLabel: fmtCount(r.line),
    byteOffsetLabel: fmtCount(r.byte_offset),
    raw: r.raw,
  }));
}

const memoRejected = memo1((health: HealthPayload | undefined) => computeRejected(health));

/** Quarantined spool lines, verbatim from `health.rejected[]` (SCREENS.md
 * §5: "reason in magenta, origin right-aligned, raw line in
 * `--color-neutral-700` `pre-wrap`" — the tone/layout is `RejectedList.svelte`'s
 * job, this selector only carries the fields). */
export function selectRejected(health: HealthPayload | undefined): RejectedRow[] {
  return memoRejected(health);
}

// ---------------------------------------------------------------------------
// selectHealthAside — Ingestion KVs, Watchdog (Timeline rail model shape
// reused), python doctor
// ---------------------------------------------------------------------------

export interface IngestionKvRow {
  label: string;
  value: string;
}

export interface DoctorRow {
  key: string;
  value: string;
  dotClass: DotClass;
}

export interface HealthAsideModel {
  ingestion: IngestionKvRow[];
  watchdog: WatchdogRailRow[];
  /** Magenta italic summary line, `null` when nothing is orphaned — same
   * convention/wording as selectors/timeline.ts's `selectRail.orphanSummary`. */
  orphanSummary: string | null;
  doctor: DoctorRow[];
}

function doctorDotClass(status: string): DotClass {
  // PythonDoctorRow.status is a plain `string` on the wire (debug.ts's own
  // doc comment doesn't narrow it to a union — `af python doctor --json` is
  // gap #11, a real CLI surface this console does not own the vocabulary
  // of). Brief: "status ok=cyan, warn=neutral, error=magenta" — an unknown
  // fourth value falls back to neutral (not magenta), since a status this
  // file doesn't recognise is not evidence of an actual alarm.
  if (status === "ok") return "dot-accent";
  if (status === "error") return "dot-alarm";
  return "dot-neutral";
}

/** `health.collectors[].spool_file`/`byte_offset` joined with
 * `session.state_dir` per the documented spool layout (docs/design-log.md:
 * "All spool files live under `~/.local/state/agentic-footprint/spool/`, or
 * `$AF_STATE_DIR/spool/` if set") — string concatenation of two already-real
 * payload fields with a fixed, documented separator, not a fabricated path
 * segment (global-constraints.md #1 forbids computing NUMBERS the server
 * didn't supply; it does not forbid composing a display path out of two
 * verbatim strings, the same category of composition
 * selectors/impact.ts's `usageEmbodiedLine` already does with " · "). A
 * collector with no `spool_file` (e.g. `otlp-cc`, which arrives over HTTP,
 * not a spool file) is skipped — nothing to show, not a fabricated "n/a".
 */
function buildIngestionRows(health: HealthPayload, session: SessionInfo | null): IngestionKvRow[] {
  const rows: IngestionKvRow[] = [];
  const spoolDir = session ? `${session.state_dir}/spool` : undefined;
  for (const c of health.collectors) {
    if (c.spool_file === undefined) continue;
    const path = spoolDir !== undefined ? `${spoolDir}/${c.spool_file}` : c.spool_file;
    rows.push({
      label: `${c.name} spool`,
      value: c.byte_offset !== undefined ? `${path} · byte ${fmtCount(c.byte_offset)}` : path,
    });
  }

  const otlp = health.otlp_receiver;
  rows.push({
    label: "otlp endpoint",
    value: otlp.endpoint === null ? (otlp.note ?? "no receiver") : `${otlp.endpoint} · ${otlp.protocol}`,
  });
  rows.push({ label: "otlp logs accepted", value: fmtCount(otlp.logs_accepted) });
  rows.push({ label: "otlp metrics discarded", value: fmtCount(otlp.metrics_discarded) });

  // Real-server-only counter (HealthPayload's own doc comment) — the mock
  // never sends it, so it's honestly absent from this list rather than
  // rendered as a fabricated "—" row; present only when the payload actually
  // carries it (global-constraints.md #6: "not measured" never rendered as 0).
  if (health.rejected_total !== undefined) {
    rows.push({ label: "rejected total", value: fmtCount(health.rejected_total) });
  }

  return rows;
}

/** `eventStore.watchdog` mapped to `timeline.ts`'s own `WatchdogRailRow`
 * shape (the brief's "reused from Timeline's rail model shape") — the SAME
 * TYPE and the same three `format.ts` calls `selectRail`'s watchdog mapping
 * uses, so `WatchdogList.svelte` (already built for the Timeline rail) can
 * render this aside's watchdog section unmodified. Deliberately NOT a call
 * to `selectRail` itself: that selector also computes the Timeline rail's
 * collector-dot table (which needs `nowMs`) and per-type counts, neither of
 * which the Health aside wants — calling it here would force this file's
 * memo key to carry a `nowMs` it has no other use for, and would make the
 * Health tab's watchdog rows recompute on every 1Hz clock tick even while
 * idle. */
function buildWatchdogRows(): WatchdogRailRow[] {
  return eventStore.watchdog.map((w) => ({
    pid: w.pid,
    cmd: w.cmd,
    cpuPctLabel: fmtCpuPct(w.cpu_pct),
    rssLabel: fmtBytes(w.rss_bytes),
    state: w.state,
  }));
}

function computeHealthAside(health: HealthPayload | undefined, rev: number, session: SessionInfo | null): HealthAsideModel {
  void rev; // memo key only — `eventStore.watchdog` is read directly, this file never reads `.rev` itself
  const watchdog = buildWatchdogRows();
  // Same orphan-summary wording as selectors/timeline.ts's `selectRail` —
  // the Health aside's Watchdog section is the same data, so it must read
  // identically, not as a subtly different re-derivation.
  const orphaned = eventStore.watchdog.filter((w) => w.state === "orphaned");
  const orphanSummary =
    orphaned.length === 0
      ? null
      : orphaned.map((w) => `pid ${w.pid} outlived ${w.span_id}${w.outlived_span_by_ms !== undefined ? ` by ${fmtMs(w.outlived_span_by_ms)}` : ""}`).join(" · ");

  const doctor: DoctorRow[] = health ? health.python.map((p: PythonDoctorRow) => ({ key: p.key, value: p.value, dotClass: doctorDotClass(p.status) })) : [];

  return {
    ingestion: health ? buildIngestionRows(health, session) : [],
    watchdog,
    orphanSummary,
    doctor,
  };
}

const memoHealthAside = memo1((health: HealthPayload | undefined, rev: number, session: SessionInfo | null) => computeHealthAside(health, rev, session));

/** Ingestion key/values (spool paths + byte offsets + otlp endpoint/
 * protocol/accepted/discarded, verbatim) + watchdog rows (Timeline rail
 * model shape, reused) + `af python doctor` rows with a severity dot
 * (SCREENS.md §5 aside). `rev` is `eventStore.rev` — the watchdog list's own
 * change signal, since `eventStore.watchdog` is bulk non-reactive state read
 * directly here, mirroring every other selector's `(rev, ...)` memo key. */
export function selectHealthAside(health: HealthPayload | undefined, rev: number, session: SessionInfo | null): HealthAsideModel {
  return memoHealthAside(health, rev, session);
}
