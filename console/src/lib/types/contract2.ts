// GENERATED FILE — do not hand-edit.
// Source: schemas/v0.1/derived.schema.json
// Regenerate with `npm run gen:types` (console/scripts/gen-types.mjs).
// `npm run gen:types:check` fails if this file drifts from a fresh regeneration.

/**
 * Uncertainty range. min == max expresses a point value.
 */
export interface Range {
  min: number;
  max: number;
  [k: string]: unknown;
}

/**
 * One impact criterion with usage/embodied life-cycle split when the methodology provides it.
 */
export interface Criterion {
  /**
   * e.g. kWh, kgCO2eq, kgSbeq, MJ, L
   */
  unit: string;
  total: Range;
  usage?: Range;
  embodied?: Range;
  [k: string]: unknown;
}

/**
 * Full EcoLogits criteria set.
 */
export interface Impacts {
  energy?: Criterion;
  gwp?: Criterion;
  adpe?: Criterion;
  pe?: Criterion;
  water?: Criterion;
  [k: string]: unknown;
}

/**
 * Estimated impacts of one llm_call event (remote inference).
 */
export interface ImpactEstimate {
  /**
   * The llm_call event this estimate derives from.
   */
  event_id: string;
  /**
   * Failure honesty: an unestimable call is surfaced, never skipped or zeroed. `missing_usage` is a call whose llm_call carried no token count — it never reaches the estimator, and reporting it as `error` or `pending` would attribute the gap to the estimator rather than to the collector that could not observe the usage.
   */
  estimation_status: "ok" | "unknown_model" | "missing_zone" | "missing_usage" | "pending" | "error";
  impacts?: Impacts;
  methodology: {
    /**
     * Version of the methodology data artifact used.
     */
    version: string;
    source: "bundled" | "local_dataset" | "ecologits_api" | "self_hosted_api";
    ecologits_version?: string;
    [k: string]: unknown;
  };
  [k: string]: unknown;
}

/**
 * Joined local-measured + remote-estimated impacts for one attribution unit over an interval. The combined total crosses measurement paradigms (measured + modeled) and MUST be presented as such.
 */
export interface ImpactJoin {
  unit: {
    level: "session" | "task" | "tool_call";
    session_id?: string;
    task_id?: string;
    tool_call_id?: string;
    [k: string]: unknown;
  };
  t_start: string;
  t_end: string;
  /**
   * Policy used to apportion machine energy to this unit. Recorded so results are interpretable and re-computable.
   */
  attribution_policy: "l1_wall_clock" | "l2_cpu_time" | "l3_cgroup";
  local_measured?: {
    energy?: Criterion;
    gwp?: Criterion;
    /**
     * True when idle/baseline energy was separated out rather than attributed to actions.
     */
    baseline_share_excluded?: boolean;
    /**
     * Fraction of the unit's wall time covered by energy samples.
     */
    coverage?: number;
    [k: string]: unknown;
  };
  remote_estimated?: {
    impacts?: Impacts;
    llm_calls?: number;
    [k: string]: unknown;
  };
  /**
   * Sum of local measured + remote estimated per criterion, ranges preserved. Cross-paradigm: label accordingly in presentation.
   */
  combined_total?: {
    energy?: Criterion;
    gwp?: Criterion;
    [k: string]: unknown;
  };
  /**
   * Count of execution_locus=remote action spans excluded from the local join and not (yet) estimable.
   */
  unmeasured_remote_spans?: number;
  [k: string]: unknown;
}
