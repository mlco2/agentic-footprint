// Stream tab selectors (DATA-CONTRACT §3.5). Pure module: no Svelte imports,
// no `Date.now()`/`Math.random()`.
//
// Task 6: the Inspector/Correlated model builders (`selectInspector`,
// `selectCorrelated`, and their `InspectorRow`/`InspectorModel`/
// `CorrelatedRow` types) moved to `selectors/inspector.ts` so a shared
// selection-resolution helper could serve every surface (Timeline bar click,
// Stream row click, decision-log ref, correlated-row click) identically —
// see that file's own header comment. Re-exported here verbatim so this
// module's public API (what Stream.svelte/Timeline.svelte already import
// from "./stream") stays stable; nothing outside this file needs to change
// its import path.
import { eventStore } from "../stores/eventStore.svelte";
import { fmtClock } from "../format";
import { attributionOf, factsOf, isErrorStatus, sourceMethodOf, statusOf } from "./factFormat";
import { memo1 } from "./memo";
import type { FactEvent } from "../types/contract1";

export { USAGE_SOURCE_RANK, usageSourceBadgeClass, methodBadgeClass, factsOf, isErrorStatus } from "./factFormat";
export type { InspectorRow, InspectorModel, CorrelatedRow } from "./inspector";
export { selectInspector, selectCorrelated } from "./inspector";

/** Rendered rows are capped (SCREENS.md "Shared table geometry" + this
 * task's brief: "Rendered rows capped to ~400 ... slice + 'showing first N'
 * note if over"). Windowing is explicitly not required at this size. */
export const STREAM_ROW_CAP = 400;

// ---------------------------------------------------------------------------
// selectStreamRows
// ---------------------------------------------------------------------------

export interface StreamRow {
  /** `event_id` — also the row's React/Svelte key and `uiStore.selectedId` value. */
  id: string;
  ts: string;
  type: FactEvent["type"];
  collector: string;
  attribution: string;
  facts: string;
  sourceMethod: string;
  sourceMethodClass: string;
  status: string;
  statusClass: string;
}

export interface StreamRowsResult {
  rows: StreamRow[];
  /** Rows actually rendered (`rows.length`, i.e. `min(total, cap)`). */
  shown: number;
  /** Rows matching the current filter, before the render cap — pairs with
   * `shown` for the "N shown of M · newest first" label. */
  total: number;
}

function buildStreamRow(event: FactEvent, tsMs: number): StreamRow {
  const status = statusOf(event);
  const sourceMethod = sourceMethodOf(event);
  return {
    id: event.event_id,
    ts: fmtClock(tsMs),
    type: event.type,
    collector: `${event.collector.name}@${event.collector.version}`,
    attribution: attributionOf(event),
    facts: factsOf(event),
    sourceMethod: sourceMethod.text,
    sourceMethodClass: sourceMethod.className,
    status,
    statusClass: isErrorStatus(status) ? "status-alarm" : "status-neutral",
  };
}

function computeStreamRows(hiddenKey: string, cap: number): StreamRowsResult {
  const hidden = hiddenKey === "" ? null : new Set(hiddenKey.split(","));
  const matching = hidden === null ? eventStore.facts.slice() : eventStore.facts.filter(({ event }) => !hidden.has(event.type));
  // Sort newest-first BY TIMESTAMP, not ring/arrival order — SCREENS.md:
  // "Stream rows must be sorted by timestamp, not arrival order — spans and
  // llm_calls are stamped at their end time while energy samples arrive on
  // their own cadence, so insertion order is not chronological." `Array.sort`
  // is stable (ES2019+), so genuine ts ties keep their arrival order.
  matching.sort((a, b) => b.tsMs - a.tsMs);
  const total = matching.length;
  const rows = matching.slice(0, cap).map(({ event, tsMs }) => buildStreamRow(event, tsMs));
  return { rows, shown: rows.length, total };
}

const memoStreamRows = memo1((_rev: number, hiddenKey: string, cap: number) => computeStreamRows(hiddenKey, cap));

/** `hiddenTypes` is `uiStore.hiddenTypes` — a `SvelteSet` whose object
 * identity never changes (membership is mutated in place), so it cannot be
 * passed straight through `memo1`'s `Object.is` comparison and expect a
 * toggle to invalidate the cache. Reducing it to a sorted, comma-joined
 * string key first gives `memo1` a primitive to compare by value instead. */
export function selectStreamRows(rev: number, hiddenTypes: ReadonlySet<string>, cap: number = STREAM_ROW_CAP): StreamRowsResult {
  const hiddenKey = hiddenTypes.size === 0 ? "" : Array.from(hiddenTypes).sort().join(",");
  return memoStreamRows(rev, hiddenKey, cap);
}
