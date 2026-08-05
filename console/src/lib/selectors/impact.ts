// Impact tab selectors (Task 8 / M8; DATA-CONTRACT §2.6, §3.5, §4;
// SCREENS.md §4; docs/design-log.md "impact_join assembly" +
// "statusline contract" entries). Pure module: no Svelte imports, no store
// singletons, no `Date.now()`/`Math.random()`. Every selector here takes the
// data it needs as a plain argument (the session-level `DebugReport`, and
// for the aside, the `SessionInfo`) — unlike selectors/attribution.ts (which
// necessarily reaches into `eventStore`/`allocStore`'s bulk mutable state),
// `ReportStore`/`SessionStore` each hold one small, replace-on-arrival
// object, so that object IS the natural memo key: `memo1`'s `Object.is`
// shallow-equality recomputes exactly when the store hands the container a
// new reference, and never otherwise.
//
// Global-constraints #1 ("the console computes nothing") applies with the
// same force this file's siblings already document. EXACTLY two derived
// computations are sanctioned anywhere in this file, both display-only and
// named at their call site:
//   1. Criteria-table split-bar proportions (`buildSplit`) — a MEAN of each
//      side's own range used ONLY to size a bar's width; never rendered as a
//      number or a percentage string.
//   2. The `af statusline` preview's range means (`buildStatusline`) — the
//      ONE sanctioned client-side mean anywhere in the console
//      (DATA-CONTRACT §2.6: "only the af statusline preview shows a range
//      mean, because that surface is specified that way"), routed through
//      `fmtStatuslineFloat` (the only formatter allowed to print `"nan"`).
// Every other number this file renders is `format.ts` applied to a report
// field verbatim, or a plain count field (`llm_calls`, `unmeasured_remote_spans`)
// typeset as-is — never summed, averaged or otherwise recomputed here.
import type { Criterion, ImpactJoin, Impacts } from "../types/contract2";
import type { DebugReport, ModelImpactGroup, ReportEstimationStatus, SessionInfo } from "../types/debug";
import { fmtGridIntensity, fmtJoules, fmtPct, fmtRange, fmtStatuslineFloat } from "../format";
import { memo1 } from "./memo";

/** The five Contract #2 criteria, in the `af statusline` field order minus
 * the reordering that line's own format applies (gwp, water, energy, adpe,
 * pe) — kept here in the schema's own declared order (`Impacts`) since the
 * criteria table and per-model table are not the statusline. */
export const CRITERIA = ["energy", "gwp", "adpe", "pe", "water"] as const;
export type CriterionKey = (typeof CRITERIA)[number];

// ---------------------------------------------------------------------------
// selectImpactCards
// ---------------------------------------------------------------------------

export type ImpactCardKey = "local" | "remote" | "combined";

export interface ImpactCardBadge {
  label: string;
  tone?: "alarm";
}

export interface ImpactCardModel {
  key: ImpactCardKey;
  /** "local · measured" / "remote · modelled" / "combined · cross-paradigm" (SCREENS.md §4). */
  eyebrow: string;
  /** DESIGN-SYSTEM §3 swatch: solid cyan (local) / ink hatch (remote) / cyan+hatch (combined). */
  swatch: ImpactCardKey;
  /** `fmtRange(criterion.total)` + unit, verbatim — or "not measured" (never "0"). */
  valueLabel: string;
  measured: boolean;
  /** usage/embodied life-cycle split, when the methodology provides it. */
  rangeLine?: string;
  secondaryLine?: string;
  badges: ImpactCardBadge[];
}

function usageEmbodiedLine(c: Criterion | undefined): string | undefined {
  if (!c) return undefined;
  const parts: string[] = [];
  if (c.usage) parts.push(`usage ${fmtRange(c.usage)}`);
  if (c.embodied) parts.push(`embodied ${fmtRange(c.embodied)}`);
  return parts.length > 0 ? parts.join(" · ") : undefined;
}

function valueLabel(c: Criterion | undefined): { label: string; measured: boolean } {
  return c ? { label: `${fmtRange(c.total)} ${c.unit}`, measured: true } : { label: "not measured", measured: false };
}

function buildLocalCard(join: ImpactJoin): ImpactCardModel {
  const local = join.local_measured;
  const energy = local?.energy;
  const { label, measured } = valueLabel(energy);

  const secondaryParts: string[] = [];
  if (local?.coverage !== undefined) secondaryParts.push(`coverage ${fmtPct(local.coverage)}`);
  if (local?.baseline_share_excluded === true) secondaryParts.push("baseline excluded");
  else if (local?.baseline_share_excluded === false) secondaryParts.push("baseline included");

  const badges: ImpactCardBadge[] = [];
  if (local?.gwp) badges.push({ label: `gwp ${fmtRange(local.gwp.total)} ${local.gwp.unit}` });

  return {
    key: "local",
    eyebrow: "local · measured",
    swatch: "local",
    valueLabel: label,
    measured,
    rangeLine: usageEmbodiedLine(energy),
    secondaryLine: secondaryParts.length > 0 ? secondaryParts.join(" · ") : undefined,
    badges,
  };
}

function buildRemoteCard(join: ImpactJoin): ImpactCardModel {
  const remote = join.remote_estimated;
  const energy = remote?.impacts?.energy;
  const { label, measured } = valueLabel(energy);

  const badges: ImpactCardBadge[] = [];
  for (const key of ["adpe", "pe", "water"] as const) {
    const c = remote?.impacts?.[key];
    if (c) badges.push({ label: `${key} ${fmtRange(c.total)} ${c.unit}` });
  }

  return {
    key: "remote",
    eyebrow: "remote · modelled",
    swatch: "remote",
    valueLabel: label,
    measured,
    rangeLine: usageEmbodiedLine(energy),
    secondaryLine: remote?.llm_calls !== undefined ? `${remote.llm_calls} llm_call${remote.llm_calls === 1 ? "" : "s"}` : undefined,
    badges,
  };
}

function buildCombinedCard(join: ImpactJoin): ImpactCardModel {
  const combined = join.combined_total;
  const energy = combined?.energy;
  const { label, measured } = valueLabel(energy);

  const badges: ImpactCardBadge[] = [];
  if (join.unmeasured_remote_spans !== undefined) {
    const n = join.unmeasured_remote_spans;
    badges.push({ label: `${n} unmeasured remote span${n === 1 ? "" : "s"}`, tone: n > 0 ? "alarm" : undefined });
  }

  return {
    key: "combined",
    eyebrow: "combined · cross-paradigm",
    swatch: "combined",
    valueLabel: label,
    measured,
    secondaryLine: combined?.gwp ? `gwp ${fmtRange(combined.gwp.total)} ${combined.gwp.unit}` : undefined,
    badges,
  };
}

function computeImpactCards(report: DebugReport | undefined): ImpactCardModel[] {
  if (!report) return [];
  const join = report.impact_join;
  return [buildLocalCard(join), buildRemoteCard(join), buildCombinedCard(join)];
}

const memoCards = memo1((report: DebugReport | undefined) => computeImpactCards(report));

/** Three cards, verbatim from `impact_join.local_measured` / `remote_estimated`
 * / `combined_total` — `[]` only when no report has arrived yet. */
export function selectImpactCards(report: DebugReport | undefined): ImpactCardModel[] {
  return memoCards(report);
}

// ---------------------------------------------------------------------------
// selectCriteriaTable
// ---------------------------------------------------------------------------

export interface CriteriaRow {
  criterion: CriterionKey;
  unit: string;
  localLabel: string;
  localMeasured: boolean;
  remoteLabel: string;
  remoteMeasured: boolean;
  combinedLabel: string;
  combinedMeasured: boolean;
  /** SANCTIONED derived value #1 (this file's header comment): each side's
   * own range MEAN, used only to size the split-bar's two segments —
   * 0..1, summing to 1 unless neither side has anything (both 0, an empty
   * track). Never formatted or rendered as text anywhere. */
  splitLocalFraction: number;
  splitRemoteFraction: number;
}

function localCriterionOf(join: ImpactJoin, key: CriterionKey): Criterion | undefined {
  if (key === "energy") return join.local_measured?.energy;
  if (key === "gwp") return join.local_measured?.gwp;
  return undefined; // local_measured never carries adpe/pe/water (design-log: "the local side measures no other").
}

function combinedCriterionOf(join: ImpactJoin, key: CriterionKey): Criterion | undefined {
  if (key === "energy") return join.combined_total?.energy;
  if (key === "gwp") return join.combined_total?.gwp;
  return undefined; // schema allows no other criterion in combined_total.
}

function buildCriteriaRow(join: ImpactJoin, key: CriterionKey): CriteriaRow {
  const local = localCriterionOf(join, key);
  const remote = join.remote_estimated?.impacts?.[key];
  const combined = combinedCriterionOf(join, key);

  const unit = local?.unit ?? remote?.unit ?? combined?.unit ?? "—";
  const localMeasured = local !== undefined;
  const remoteMeasured = remote !== undefined;
  const combinedMeasured = combined !== undefined;

  let combinedLabel: string;
  if (combined) combinedLabel = `${fmtRange(combined.total)} ${combined.unit}`;
  else if (!localMeasured && remoteMeasured) combinedLabel = "not measured · remote only";
  else combinedLabel = "not measured";

  const localWeight = local ? Math.max(0, (local.total.min + local.total.max) / 2) : 0;
  const remoteWeight = remote ? Math.max(0, (remote.total.min + remote.total.max) / 2) : 0;
  const weightSum = localWeight + remoteWeight;

  return {
    criterion: key,
    unit,
    localLabel: local ? `${fmtRange(local.total)} ${local.unit}` : "not measured",
    localMeasured,
    remoteLabel: remote ? `${fmtRange(remote.total)} ${remote.unit}` : "not measured",
    remoteMeasured,
    combinedLabel,
    combinedMeasured,
    splitLocalFraction: weightSum > 0 ? localWeight / weightSum : 0,
    splitRemoteFraction: weightSum > 0 ? remoteWeight / weightSum : 0,
  };
}

function computeCriteriaTable(report: DebugReport | undefined): CriteriaRow[] {
  if (!report) return [];
  return CRITERIA.map((key) => buildCriteriaRow(report.impact_join, key));
}

const memoCriteriaTable = memo1((report: DebugReport | undefined) => computeCriteriaTable(report));

/** One row per Contract #2 criterion (energy/gwp/adpe/pe/water), always all
 * five — a criterion with no local measurement reads "not measured" in the
 * local column (never "0"), and "not measured · remote only" in the
 * combined column when local is missing but remote has something
 * (SCREENS.md §4). */
export function selectCriteriaTable(report: DebugReport | undefined): CriteriaRow[] {
  return memoCriteriaTable(report);
}

// ---------------------------------------------------------------------------
// selectPerModel
// ---------------------------------------------------------------------------

export interface PerModelCell {
  label: string;
  measured: boolean;
}

export interface PerModelRow {
  modelId: string;
  /** True when ANY estimate for this model carries `estimation_status:
   * "unknown_model"` — DESIGN-SYSTEM §3: magenta, cross-paradigm eyebrow. */
  isUnknown: boolean;
  /** Distinct non-"ok" statuses this model's estimates carry, joined —
   * present only for `isUnknown` rows (SCREENS.md/brief: "unknown_model rows
   * magenta with their status"). */
  statusLabel?: string;
  cells: Record<CriterionKey, PerModelCell>;
}

export interface PerModelTableModel {
  rows: PerModelRow[];
  /** The report's OWN totals — `impact_join.remote_estimated.impacts`,
   * rendered verbatim. Never a client-side sum across `rows` (brief: "the
   * report's totals are server totals — render them, don't sum"); this is
   * also why an `unknown_model` row is naturally excluded from it — the
   * server never adds a non-"ok" estimate into this figure
   * (docs/design-log.md, "impact_join assembly": "Only each criterion's
   * total range is summed... across ok estimates"). */
  totals: Record<CriterionKey, PerModelCell>;
  llmCallsLabel?: string;
}

function cellsFor(impacts: Impacts | undefined): Record<CriterionKey, PerModelCell> {
  const out = {} as Record<CriterionKey, PerModelCell>;
  for (const key of CRITERIA) {
    const c = impacts?.[key];
    out[key] = c ? { label: `${fmtRange(c.total)} ${c.unit}`, measured: true } : { label: "not measured", measured: false };
  }
  return out;
}

function buildPerModelRow(group: ModelImpactGroup): PerModelRow {
  const statuses = Array.from(new Set(group.estimates.map((e) => e.estimation_status)));
  const isUnknown = statuses.includes("unknown_model");
  return {
    modelId: group.model_id,
    isUnknown,
    statusLabel: isUnknown ? statuses.join(", ") : undefined,
    cells: cellsFor(group.impacts),
  };
}

function computePerModel(report: DebugReport | undefined): PerModelTableModel {
  if (!report) return { rows: [], totals: cellsFor(undefined) };
  const join = report.impact_join;
  return {
    rows: report.by_model.map(buildPerModelRow),
    totals: cellsFor(join.remote_estimated?.impacts),
    llmCallsLabel: join.remote_estimated?.llm_calls !== undefined ? String(join.remote_estimated.llm_calls) : undefined,
  };
}

const memoPerModel = memo1((report: DebugReport | undefined) => computePerModel(report));

/** One row per `by_model` group; `unknown_model` rows are flagged (magenta)
 * but still listed (never dropped — the whole point is that an unrecognised
 * model is surfaced, not hidden), and are excluded from `totals` because
 * `totals` is never computed here at all (see `PerModelTableModel.totals`'s
 * own doc comment). */
export function selectPerModel(report: DebugReport | undefined): PerModelTableModel {
  return memoPerModel(report);
}

// ---------------------------------------------------------------------------
// Cross-paradigm note (static prose; SCREENS.md §4: "magenta eyebrow + ink
// prose") — no report fields are interpolated into it, so it carries no
// provenance risk and needs no selector/memoisation of its own.
// ---------------------------------------------------------------------------

export const CROSS_PARADIGM_NOTE: { eyebrow: string; prose: string[] } = {
  eyebrow: "cross-paradigm",
  prose: [
    "The combined figures above and in the criteria table's rightmost column add a real local measurement to a modelled remote estimate. They cross measurement paradigms — read them as an order-of-magnitude figure, never as a single-source measurement, and never compare them directly against another session's local-only or remote-only number.",
  ],
};

// ---------------------------------------------------------------------------
// selectImpactAside
// ---------------------------------------------------------------------------

export interface AsideRow {
  label: string;
  value: string;
  tone?: "alarm";
}

export interface HistogramRow {
  status: ReportEstimationStatus;
  count: number;
}

export interface MethodologyModel {
  versionLabel: string;
  sourceLabel: string;
  ecologitsLabel: string;
  codecarbonLabel: string;
  gridZoneLabel: string;
  gridIntensityLabel: string;
}

export interface ImpactAsideModel {
  tokenOnlyMisses: AsideRow[];
  histogram: HistogramRow[];
  /** Exact two-line `af statusline` preview text (docs/design-log.md
   * "statusline contract"): line 1 the fixed header, line 2 five
   * space-separated range means (`nan` when unmeasured). */
  statuslineLines: readonly [string, string];
  methodology?: MethodologyModel;
}

/** `local_measured.breakdown_j` (session-unit only) is an extra property
 * beyond Contract #2's declared schema (docs/design-log.md, "impact_join
 * assembly": "{attributed, baseline_idle, orphaned, total}" — "exposed so
 * [conservation] can be checked rather than trusted"). `contract2.ts`'s
 * generated `local_measured` type carries `[k: string]: unknown` for
 * exactly this reason. Read defensively — the mock's report fixture
 * (dev/scenario.ts) does not currently emit it, and a real server session
 * whose unit isn't `"session"` never carries it either, so this must be
 * `undefined`-safe rather than assumed present. */
function breakdownJOf(join: ImpactJoin): { attributed?: number; baseline_idle?: number; orphaned?: number; total?: number } | undefined {
  const raw = join.local_measured?.["breakdown_j"];
  return raw && typeof raw === "object" ? (raw as { attributed?: number; baseline_idle?: number; orphaned?: number; total?: number }) : undefined;
}

function buildTokenOnlyMisses(join: ImpactJoin): AsideRow[] {
  const local = join.local_measured;
  const breakdown = breakdownJOf(join);
  const rows: AsideRow[] = [];

  rows.push({ label: "local energy", value: local?.energy ? `${fmtRange(local.energy.total)} ${local.energy.unit}` : "not measured" });
  rows.push({ label: "coverage", value: local?.coverage !== undefined ? fmtPct(local.coverage) : "not reported" });
  rows.push({
    label: "baseline share excluded",
    value: local?.baseline_share_excluded === undefined ? "not reported" : local.baseline_share_excluded ? "yes" : "no",
  });
  rows.push({ label: "orphaned compute", value: breakdown?.orphaned !== undefined ? fmtJoules(breakdown.orphaned) : "not reported" });
  rows.push({
    label: "agent's own share",
    // l2_cpu_time/v1 has no separate agent-process bucket — it folds into
    // orphaned (docs/design-log.md, "attribution policy l2_cpu_time v1":
    // "agent_process.allocated_j carries the orphan bucket"). Stated only
    // when the breakdown is actually present AND the policy is the one this
    // claim is true of — never asserted as a guess for another policy.
    value:
      breakdown && join.attribution_policy === "l2_cpu_time"
        ? "folded into orphaned compute — l2_cpu_time/v1 has no separate agent bucket"
        : "not reported",
  });
  const remoteSpans = join.unmeasured_remote_spans;
  rows.push({
    label: "unmeasured remote spans",
    value: remoteSpans !== undefined ? `${remoteSpans} span${remoteSpans === 1 ? "" : "s"}` : "not reported",
    tone: remoteSpans !== undefined && remoteSpans > 0 ? "alarm" : undefined,
  });

  return rows;
}

const CANONICAL_STATUSES: readonly ReportEstimationStatus[] = ["ok", "pending", "unknown_model", "missing_zone", "error"];

function buildHistogram(report: DebugReport): HistogramRow[] {
  const h = report.estimation_status_histogram;
  const rows: HistogramRow[] = CANONICAL_STATUSES.map((status) => ({ status, count: h[status] ?? 0 }));
  // Real-server nuance (console/README.md, task context): the histogram MAY
  // include `missing_usage` — the pipeline's sixth status, folded in only
  // when it actually occurs (DebugReport's own doc comment). Never
  // zero-filled: its absence must read as "not applicable", not "0 occurred".
  if (h.missing_usage !== undefined) rows.push({ status: "missing_usage", count: h.missing_usage });
  return rows;
}

/** SANCTIONED derived value #2 (this file's header comment): `(min+max)/2`
 * of a criterion's `total` range — `NaN` when the criterion is absent, so
 * `fmtStatuslineFloat` prints the honest `"nan"` rather than a fabricated 0. */
function rangeMean(c: Criterion | undefined): number {
  if (!c) return NaN;
  return (c.total.min + c.total.max) / 2;
}

/** Sourcing fallback per docs/design-log.md "af statusline contract
 * (final)": gwp/energy prefer `combined_total` (only ever present when both
 * paradigms are genuinely complete) before falling back; gwp does NOT fall
 * back to the local measurement (a local-only gwp reads as "session emitted
 * almost nothing" rather than "the remote half is missing"); water/adpe/pe
 * exist only on the remote side. This preview renders `nan` for "nothing to
 * source", per the brief and the (superseded, but still normative for THIS
 * preview panel) "statusline contract" entry — not `0`, which is the real
 * `af statusline` CLI's own later, unrelated formatting choice for its
 * awk-consuming bar. */
function buildStatusline(join: ImpactJoin): readonly [string, string] {
  const gwp = rangeMean(join.combined_total?.gwp ?? join.remote_estimated?.impacts?.gwp);
  const water = rangeMean(join.remote_estimated?.impacts?.water);
  const energy = rangeMean(join.combined_total?.energy ?? join.local_measured?.energy ?? join.remote_estimated?.impacts?.energy);
  const adpe = rangeMean(join.remote_estimated?.impacts?.adpe);
  const pe = rangeMean(join.remote_estimated?.impacts?.pe);

  const line2 = [gwp, water, energy, adpe, pe].map(fmtStatuslineFloat).join(" ");
  return ["gwp wcf energy adpe pe", line2];
}

function buildMethodology(session: SessionInfo | null): MethodologyModel | undefined {
  if (!session) return undefined;
  return {
    versionLabel: session.methodology.version,
    sourceLabel: session.methodology.source,
    // Real-server nuance (DATA-CONTRACT/design-log): omitted, not guessed,
    // until the first estimate has run.
    ecologitsLabel: session.methodology.ecologits_version ?? "not yet known",
    codecarbonLabel: session.methodology.codecarbon_version ?? "not yet known",
    gridZoneLabel: session.grid.zone,
    // `g_co2e_per_kwh` can be `null` without an estimator sidecar — render
    // honestly via the shared formatter, never as 0 (format.ts's own doc comment).
    gridIntensityLabel: fmtGridIntensity(session.grid.g_co2e_per_kwh, session.grid.source),
  };
}

function computeAside(report: DebugReport | undefined, session: SessionInfo | null): ImpactAsideModel {
  if (!report) {
    return {
      tokenOnlyMisses: [],
      histogram: [],
      statuslineLines: ["gwp wcf energy adpe pe", ["nan", "nan", "nan", "nan", "nan"].join(" ")],
      methodology: buildMethodology(session),
    };
  }
  return {
    tokenOnlyMisses: buildTokenOnlyMisses(report.impact_join),
    histogram: buildHistogram(report),
    statuslineLines: buildStatusline(report.impact_join),
    methodology: buildMethodology(session),
  };
}

const memoAside = memo1((report: DebugReport | undefined, session: SessionInfo | null) => computeAside(report, session));

/** "What token-only misses" (6 rows) + estimation-status histogram +
 * `af statusline` preview + methodology block. `report`/`session` double as
 * the memo key (both are replace-on-arrival singleton objects), exactly
 * like `selectImpactCards`. */
export function selectImpactAside(report: DebugReport | undefined, session: SessionInfo | null): ImpactAsideModel {
  return memoAside(report, session);
}
