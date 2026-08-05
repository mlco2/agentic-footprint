// Selector memoisation utility (DATA-CONTRACT §3.5). A pure module: no
// Svelte imports, no runes, no `Date.now()`/`Math.random()` — selectors that
// use this must stay deterministic in (storeRevision, tab, layout,
// selectedId, filters) alone, or memoisation silently stops working.
//
// The actual per-panel selectors (selectTimelineLanes, selectStreamRows, …)
// land in a later task; this file lands only the caching primitive plus its
// unit test, per the M3 brief.

/** Shallow-equality over two argument tuples of the same arity, by `Object.is`
 * per element (not `===`, so `NaN` compares equal to itself like every other
 * primitive comparison in this codebase's memoisation). */
function shallowEqualArgs(a: readonly unknown[], b: readonly unknown[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (!Object.is(a[i], b[i])) return false;
  }
  return true;
}

/** Memoises `fn` against its *most recent* call only (a depth-1 cache, hence
 * `memo1`): if every argument is shallow-equal (`Object.is`) to the previous
 * call's arguments, the previous result is returned by reference without
 * calling `fn` again. Any single changed argument — including a changed
 * object/array reference, since this is shallow, not deep, equality —
 * triggers a recompute. */
export function memo1<Args extends readonly unknown[], R>(fn: (...args: Args) => R): (...args: Args) => R {
  let hasCache = false;
  let lastArgs: Args | null = null;
  let lastResult: R;

  return (...args: Args): R => {
    if (hasCache && lastArgs !== null && shallowEqualArgs(lastArgs, args)) {
      return lastResult;
    }
    lastResult = fn(...args);
    lastArgs = args;
    hasCache = true;
    return lastResult;
  };
}
