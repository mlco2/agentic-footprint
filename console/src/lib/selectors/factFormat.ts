// Shared per-fact display formatting (DATA-CONTRACT §3.5/§4): one-line
// "facts" summaries and provenance badge classes, used by BOTH
// `selectors/stream.ts` (the Stream table's `facts`/`source · method`
// columns) and `selectors/inspector.ts` (the Correlated section's
// summaries). Split into its own module specifically so those two files
// don't import each other — stream.ts re-exports from here (and delegates to
// inspector.ts for selectInspector/selectCorrelated) to keep its own public
// API unchanged, while inspector.ts depends only on this module, never on
// stream.ts, avoiding a circular import between the two.
//
// Pure module: no Svelte imports, no `Date.now()`/`Math.random()` — the only
// non-determinism-adjacent thing here is `Date.parse` on strings the payload
// itself already carries (an action_span's own `t_start`/`t_end`), which is
// display arithmetic (duration for the facts column), not a clock read.
import type { ActionSpan, EnergySample, FactEvent, LlmCall } from "../types/contract1";
import { fmtJoules, fmtMs, fmtTokens } from "../format";

const MEASURED_METHODS = new Set<EnergySample["components"][number]["method"]>(["rapl", "powermetrics", "nvml"]);

/** Provenance rank per RFC Annex B / DATA-CONTRACT: decreasing reliability
 * `api_response > agent_telemetry > transcript > estimated`. DESIGN-SYSTEM §7
 * rationale: only the bottom two (transcript, estimated — "known accuracy
 * issues" / outright modelled) get the alarm (magenta) treatment; the top
 * two stay neutral. Exported so components/tests can key off the rank
 * without re-deriving it. */
export const USAGE_SOURCE_RANK: Record<LlmCall["usage_source"], number> = {
  api_response: 0,
  agent_telemetry: 1,
  transcript: 2,
  estimated: 3,
};

/** True for the `usage_source` values DESIGN-SYSTEM §3 alarms on
 * (transcript, estimated — `USAGE_SOURCE_RANK` 2/3), reusing the exact same
 * rank table `usageSourceBadgeClass` derives its own class from rather than
 * restating "transcript"/"estimated" as a second, driftable string list. */
export function isAlarmUsageSource(usageSource: LlmCall["usage_source"]): boolean {
  return USAGE_SOURCE_RANK[usageSource] >= 2;
}

/** CSS class for the `usage_source` badge — rank 0/1 (api_response,
 * agent_telemetry) are non-alarm; rank 2/3 (transcript, estimated) are the
 * alarm treatment DESIGN-SYSTEM §3 reserves for "transcript-sourced usage". */
export function usageSourceBadgeClass(usageSource: LlmCall["usage_source"]): string {
  return `badge-prov-${USAGE_SOURCE_RANK[usageSource]}`;
}

/** True for the modelled (`tdp_model`) energy-component method — the same
 * check `methodBadgeClass` uses to choose `"badge-modelled"`, exposed as a
 * boolean for call sites that need the measured/modelled axis without a
 * badge class string (e.g. a share-bar hatch flag). */
export function isModelledMethod(method: EnergySample["components"][number]["method"]): boolean {
  return method === "tdp_model";
}

/** CSS class for an energy component's `method` — solid ink/measured
 * (rapl/powermetrics/nvml) vs the neutral-hatch "modelled" treatment for
 * `tdp_model` (DESIGN-SYSTEM §3). Neither is the alarm case. */
export function methodBadgeClass(method: EnergySample["components"][number]["method"]): string {
  if (MEASURED_METHODS.has(method)) return "badge-measured";
  if (isModelledMethod(method)) return "badge-modelled";
  return "badge-neutral";
}

/** Single source of truth for the `status === "error"` alarm check, reused
 * everywhere a fact's (or a still-open span's) status feeds a `tone`/class
 * decision — DESIGN-SYSTEM §3's alarm case, never derived by re-comparing
 * the string literal at each call site. */
export function isErrorStatus(status: string | undefined): boolean {
  return status === "error";
}

function shortAttributionId(id: string): string {
  return id.length <= 12 ? id : `…${id.slice(-8)}`;
}

/** "task/tool id short form or —" — prefers the most specific correlation
 * key available (tool_call_id), falling back toward the session as each
 * level is absent. Collectors emit what they can observe (contract1.ts:
 * "emit what the collector can observe; omit the rest"). */
export function attributionOf(event: FactEvent): string {
  const a = event.attribution;
  if (!a) return "—";
  const id = a.tool_call_id ?? a.task_id ?? a.subagent_id ?? a.agent_id;
  return id ? shortAttributionId(id) : "—";
}

/** Like `factsOf`/`sourceMethodOf`/`attributionOf` below, switches on
 * `event.type` rather than a blind payload cast — only `llm_call` and
 * `action_span` carry a `status` field on the wire (contract1.ts); every
 * other type has nothing to report here. */
export function statusOf(event: FactEvent): string {
  switch (event.type) {
    case "llm_call":
    case "action_span":
      return event.payload.status ?? "—";
    case "energy_sample":
    case "process_sample":
    case "session_meta":
      return "—";
  }
}

// ---------------------------------------------------------------------------
// facts column — one-line, per-type, display-formatting-only summaries
// ---------------------------------------------------------------------------

function llmCallFacts(payload: LlmCall): string {
  const parts = [payload.model_id_requested, `in ${fmtTokens(payload.usage.input_tokens ?? 0)}`, `out ${fmtTokens(payload.usage.output_tokens ?? 0)}`];
  if (payload.usage.thought_tokens) parts.push(`think ${fmtTokens(payload.usage.thought_tokens)}`);
  if (payload.duration_ms !== undefined) parts.push(fmtMs(payload.duration_ms));
  return parts.join(" · ");
}

function actionSpanFacts(payload: ActionSpan): string {
  const durationMs = Date.parse(payload.t_end) - Date.parse(payload.t_start);
  return `${payload.tool_name} · ${fmtMs(durationMs)} · ${payload.tool_kind}/${payload.execution_locus}`;
}

/** `total_j` is never on the raw `energy_sample` payload per schema (only an
 * allocation trace carries it, DATA-CONTRACT §2.4) — but the type's
 * `[k: string]: unknown` index signature allows a future collector to add it,
 * so this checks for it rather than assuming its absence. Per this task's
 * verification rule: use it directly if present, else render "…" — NEVER
 * sum `components[].energy_j` to manufacture one (that would be exactly the
 * client-side arithmetic global-constraints.md #1 forbids). Each component's
 * own `energy_j` is still shown individually — formatting one already-real
 * number is not "computing" a quantity. */
function energySampleFacts(payload: EnergySample & { total_j?: unknown }): string {
  const intervalMs = Date.parse(payload.t_end) - Date.parse(payload.t_start);
  const comps = payload.components.map((c) => `${c.kind} ${fmtJoules(c.energy_j)}`).join(" · ");
  const total = typeof payload.total_j === "number" ? fmtJoules(payload.total_j) : "…";
  return `${fmtMs(intervalMs)} · total ${total} · ${comps}`;
}

function processSampleFacts(payload: Extract<FactEvent, { type: "process_sample" }>["payload"]): string {
  const n = payload.processes.length;
  return `${n} process${n === 1 ? "" : "es"}`;
}

function sessionMetaFacts(payload: Extract<FactEvent, { type: "session_meta" }>["payload"]): string {
  const app = payload.agent_app;
  return app.version ? `${app.name} ${app.version}` : app.name;
}

/** Exported for `selectCorrelated`'s summaries (same formatting, no
 * derived quantities) as well as `selectStreamRows`. */
export function factsOf(event: FactEvent): string {
  switch (event.type) {
    case "llm_call":
      return llmCallFacts(event.payload);
    case "action_span":
      return actionSpanFacts(event.payload);
    case "energy_sample":
      return energySampleFacts(event.payload);
    case "process_sample":
      return processSampleFacts(event.payload);
    case "session_meta":
      return sessionMetaFacts(event.payload);
  }
}

/** "source / method badge" (DATA-CONTRACT §4): `llm_call.usage_source`;
 * `energy_sample.components[].method`. Other types have nothing to show
 * here. */
export function sourceMethodOf(event: FactEvent): { text: string; className: string } {
  if (event.type === "llm_call") {
    return { text: event.payload.usage_source, className: usageSourceBadgeClass(event.payload.usage_source) };
  }
  if (event.type === "energy_sample") {
    const methods = Array.from(new Set(event.payload.components.map((c) => c.method)));
    const classes = new Set(methods.map((m) => methodBadgeClass(m)));
    const className = classes.has("badge-modelled") ? "badge-modelled" : classes.has("badge-measured") ? "badge-measured" : "badge-neutral";
    return { text: methods.join(" + "), className };
  }
  return { text: "—", className: "badge-neutral" };
}
