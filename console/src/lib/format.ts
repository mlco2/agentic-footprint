// The app-wide, single point for numeric/date display (global-constraints.md
// #1: "All numeric display goes through console/src/lib/format.ts — the only
// module allowed to touch numbers"). Every function here is pure formatting:
// unit scaling (J -> kJ, ms -> s, a server-supplied 0..1 share -> a percent
// string) and rounding for display. None of it computes an impact, an
// attribution, an apportionment or an average — callers pass in numbers the
// control plane already computed; this module only decides how many digits
// and which unit suffix to show.
//
// No Date.now()/Math.random() here — every input is a value the caller
// already has (a parsed ms timestamp, a payload field), never sampled by this
// module itself.

/** Guards against NaN/Infinity ever reaching `toFixed`/`toLocaleString` and
 * printing "NaN"/"Infinity" — every formatter routes through this first. */
function safe(n: number): number {
  return Number.isFinite(n) ? n : 0;
}

function withCommas(n: number, decimals: number): string {
  return n.toLocaleString("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
}

/** `hh:mm:ss.SSS`, local wall-clock (this is a localhost dev tool; there is
 * no server-timezone concept to honour instead). */
export function fmtClock(ms: number): string {
  const d = new Date(safe(ms));
  const p2 = (x: number): string => String(x).padStart(2, "0");
  const p3 = (x: number): string => String(x).padStart(3, "0");
  return `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())}.${p3(d.getMilliseconds())}`;
}

/** Joules. Sub-10J values keep 2 decimals — SCREENS.md: "Sub-joule shares
 * must show 2 decimals. Rounding to '0 J' hides exactly the small-share rows
 * the L1-vs-L2 comparison exists to expose." Above that, whole joules read
 * fine, and above 1000 the kJ unit takes over. */
export function fmtJoules(j: number): string {
  const v = safe(j);
  if (v === 0) return "0 J";
  const abs = Math.abs(v);
  if (abs < 10) return `${withCommas(v, 2)} J`;
  if (abs < 1000) return `${withCommas(v, 0)} J`;
  return `${withCommas(v / 1000, 2)} kJ`;
}

/** Watts. Mirrors `fmtJoules`'s precision tiers (2 decimals below 10, whole
 * below 1000, kW above) so a power reading and an energy reading printed
 * side by side never look inconsistently rounded. */
export function fmtWatts(w: number): string {
  const v = safe(w);
  if (v === 0) return "0 W";
  const abs = Math.abs(v);
  if (abs < 10) return `${withCommas(v, 2)} W`;
  if (abs < 1000) return `${withCommas(v, 1)} W`;
  return `${withCommas(v / 1000, 2)} kW`;
}

/** Milliseconds, as a human duration: `420ms`, `9.20s` / `9.2s`, `1m04s`. */
export function fmtMs(ms: number): string {
  const v = safe(ms);
  const abs = Math.abs(v);
  if (abs < 1000) return `${withCommas(v, 0)}ms`;
  if (abs < 60_000) {
    const seconds = v / 1000;
    return `${withCommas(seconds, abs < 10_000 ? 2 : 1)}s`;
  }
  const totalSeconds = Math.round(abs) / 1000;
  const minutes = Math.floor(totalSeconds / 60);
  const restSeconds = Math.round(totalSeconds - minutes * 60);
  const sign = v < 0 ? "-" : "";
  return `${sign}${minutes}m${withCommas(restSeconds, 0)}s`;
}

/** A raw millisecond COUNT typeset as its own unit — `120 ms`, `32,000 ms` —
 * distinct from `fmtMs`'s human-duration conversion (which turns >=1000ms
 * into seconds/minutes). Needed only where a value's own name is literally
 * `_ms` and the display must read as that variable, not as a duration: the
 * Attribution aside's formula substitution types `cpu_delta_ms`/
 * `denominator_cpu_ms` into `share_i = cpu_delta_ms_i / denominator_cpu_ms`
 * exactly as that equation's units read. */
export function fmtMsCount(ms: number): string {
  return `${withCommas(safe(ms), 0)} ms`;
}

/** A signed short duration for correlated-event offsets: `+0.4s` / `−2.1s`
 * (the minus is U+2212, matching the prototype's convention — a real minus
 * sign, not a hyphen, since this always reads next to a `+`). Always one
 * decimal of seconds; the ±6s correlation window never needs finer or
 * coarser granularity than that. */
export function fmtOffset(ms: number): string {
  const v = safe(ms);
  const sign = v < 0 ? "−" : "+";
  const seconds = Math.abs(v) / 1000;
  return `${sign}${seconds.toFixed(1)}s`;
}

/** Token counts: `340`, `1.2k`. */
export function fmtTokens(n: number): string {
  const v = safe(n);
  const abs = Math.abs(v);
  if (abs < 1000) return withCommas(v, 0);
  return `${withCommas(v / 1000, 1)}k`;
}

/** Renders a server-supplied `{min, max}` range as `min–max` (en dash) —
 * NEVER averaged (global-constraints.md #6: "ranges rendered as min–max,
 * never averaged"). Precision is chosen per-value so a small usage-share
 * criterion (e.g. 0.0028 kWh) doesn't collapse to "0". */
export function fmtRange(range: { min: number; max: number }): string {
  return `${fmtPlainNumber(range.min)}–${fmtPlainNumber(range.max)}`;
}

function fmtPlainNumber(n: number): string {
  const v = safe(n);
  if (v === 0) return "0";
  const abs = Math.abs(v);
  const decimals = abs < 0.01 ? 6 : abs < 1 ? 4 : abs < 10 ? 2 : 1;
  let s = v.toFixed(decimals);
  if (s.includes(".")) s = s.replace(/0+$/, "").replace(/\.$/, "");
  return s;
}

/** A server-supplied 0..1 share as a percent string. Below 10% keeps 1
 * decimal (a 2.24% share reads meaningfully different from a rounded "2%");
 * at or above 10% a whole percent is precise enough. */
export function fmtPct(share: number): string {
  const pct = safe(share) * 100;
  if (pct === 0) return "0%";
  const decimals = Math.abs(pct) < 10 ? 1 : 0;
  return `${withCommas(pct, decimals)}%`;
}

/** A collector's `events_per_s` (DATA-CONTRACT §2.7). The real
 * `af watch --debug` server always sends `null` here — "a rate over a
 * session's whole span is not the rate anyone reads it as"
 * (docs/design-log.md) — so this renders "—" rather than fabricating a
 * rate client-side (which would violate global-constraints.md #1: the
 * client computes nothing). */
export function fmtEventsPerS(eventsPerS: number | null): string {
  if (eventsPerS === null) return "—";
  const v = safe(eventsPerS);
  return `${withCommas(v, Math.abs(v) < 10 ? 2 : 1)}/s`;
}

/** `grid.g_co2e_per_kwh` (DATA-CONTRACT §2.1). `null` without an estimator
 * sidecar to resolve a zone's electricity mix — a defaulted grid intensity
 * is exactly the invented number the project forbids. Renders
 * "n/a · {source}" so the gap reads as explained, never as a silent 0
 * gCO2e/kWh (global-constraints.md #6: "not measured" never rendered as 0). */
export function fmtGridIntensity(gCo2ePerKwh: number | null, source: string): string {
  if (gCo2ePerKwh === null) return `n/a · ${source}`;
  const v = safe(gCo2ePerKwh);
  return `${withCommas(v, Math.abs(v) < 10 ? 1 : 0)} gCO2e/kWh`;
}

/** The Impact tab's `af statusline` preview values ONLY (selectors/impact.ts:
 * `buildStatusline` — the single sanctioned client-side mean, docs/design-log.md
 * "statusline contract": "Missing or unmeasured impacts print as `nan`.").
 * Unlike every other formatter in this file, a non-finite input renders the
 * literal string `"nan"` rather than being coerced to 0 by `safe()` — that
 * coercion is exactly wrong here, since the whole point of this one preview
 * is to distinguish "computed a mean" from "nothing to average". Finite
 * values are plain decimals (never scientific notation, mirroring the real
 * CLI's own `f64` `Display` convention), trimmed of trailing zeros. */
export function fmtStatuslineFloat(n: number): string {
  if (!Number.isFinite(n)) return "nan";
  if (n === 0) return "0";
  const abs = Math.abs(n);
  const decimals = abs < 0.001 ? 8 : abs < 0.01 ? 6 : abs < 1 ? 4 : abs < 10 ? 2 : 1;
  let s = n.toFixed(decimals);
  if (s.includes(".")) s = s.replace(/0+$/, "").replace(/\.$/, "");
  return s;
}

/** A watchdog entry's own `cpu_pct` (DATA-CONTRACT §2.5) — already a 0..100
 * percentage as the payload reports it (NOT a 0..1 share like `fmtPct`
 * expects), so this is a distinct formatter rather than `fmtPct(cpu/100)`,
 * which would just be re-deriving the same display from an extra division.
 * One decimal, since watchdog cpu% is a live, jittery reading where a whole
 * percent hides exactly the small-vs-idle distinction the panel is for. */
export function fmtCpuPct(pct: number): string {
  return `${withCommas(safe(pct), 1)}%`;
}

/** A plain non-negative integer POSITION/COUNT, thousands-grouped but with
 * no unit suffix and no magnitude scaling — for a spool file's `byte_offset`
 * (a cursor position, not a size: `fmtBytes`'s KB/MB scaling would misrepresent
 * "how far into this file" as an approximate quantity) and a rejected line's
 * 1-based `line` number (Health tab, DATA-CONTRACT §2.7/§2.3 `reject`
 * frame). Whole numbers only — a fractional byte offset or line number is
 * not a value this formatter's callers ever produce. */
export function fmtCount(n: number): string {
  return withCommas(Math.round(safe(n)), 0);
}

const BYTE_UNITS = ["KB", "MB", "GB", "TB"] as const;

/** Binary-scaled (1024) byte sizes: `512 B`, `1.80 GB`. */
export function fmtBytes(bytes: number): string {
  const v = safe(bytes);
  const abs = Math.abs(v);
  if (abs < 1024) return `${withCommas(v, 0)} B`;
  let value = v;
  let unitIndex = -1;
  while (Math.abs(value) >= 1024 && unitIndex < BYTE_UNITS.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const unit = BYTE_UNITS[Math.max(0, unitIndex)];
  return `${withCommas(value, Math.abs(value) < 10 ? 2 : 1)} ${unit}`;
}
