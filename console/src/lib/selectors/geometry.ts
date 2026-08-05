// Shared pure geometry helpers, deduplicated from selectors/timeline.ts's
// Layout-A bar positioning and selectors/attribution.ts's interval-strip
// positioning — both compute percent-based left/width geometry, clamped to
// [0, 100] and rounded to 3 decimals, with a floor so a near-zero (or
// zero-duration) span still renders as a visible sliver instead of an
// invisible 0-width box. Pure module: no Svelte imports, no
// `Date.now()`/`Math.random()` (global-constraints.md #5) — this is
// geometry, not a display aggregation, so it isn't on the lint:arith
// allowlist and doesn't need to be.

export function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

export function round3(n: number): number {
  return Math.round(n * 1000) / 1000;
}

/** Floors a computed width so a near-zero (or negative, pre-clamp) span
 * still renders as a visible sliver — never an invisible 0-width box.
 * Callers each pick their own floor constant (timeline.ts's
 * `MIN_BAR_WIDTH_PCT` vs. attribution.ts's `MIN_STRIP_WIDTH_PCT` differ, by
 * design — this only owns the shared `Math.max` shape both apply it with). */
export function widthWithFloor(width: number, floorPct: number): number {
  return Math.max(floorPct, width);
}
