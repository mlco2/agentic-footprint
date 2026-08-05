import { describe, expect, it, vi } from "vitest";
import { memo1 } from "../src/lib/selectors/memo";

describe("memo1", () => {
  it("returns the same reference and does not recompute when args are unchanged", () => {
    const compute = vi.fn((a: number, b: number) => ({ sum: a + b }));
    const memoised = memo1(compute);

    const first = memoised(2, 3);
    const second = memoised(2, 3);

    expect(second).toBe(first); // same object reference, not just deep-equal
    expect(compute).toHaveBeenCalledTimes(1);
  });

  it("recomputes when any argument changes", () => {
    const compute = vi.fn((a: number, b: number) => ({ sum: a + b }));
    const memoised = memo1(compute);

    const first = memoised(2, 3);
    const second = memoised(2, 4); // b changed
    const third = memoised(5, 4); // a changed

    expect(second).not.toBe(first);
    expect(third).not.toBe(second);
    expect(compute).toHaveBeenCalledTimes(3);
  });

  it("is shallow, not deep: a new-but-equal-by-value object argument recomputes", () => {
    const compute = vi.fn((filter: { type: string }) => ({ filter }));
    const memoised = memo1(compute);

    const first = memoised({ type: "fact" });
    const second = memoised({ type: "fact" }); // different reference, same shape

    expect(second).not.toBe(first);
    expect(compute).toHaveBeenCalledTimes(2);
  });

  it("a stable object reference hits the cache across calls", () => {
    const compute = vi.fn((filter: { type: string }) => ({ filter }));
    const memoised = memo1(compute);
    const stableArg = { type: "fact" };

    const first = memoised(stableArg);
    const second = memoised(stableArg);

    expect(second).toBe(first);
    expect(compute).toHaveBeenCalledTimes(1);
  });

  it("only remembers the single most recent call (depth-1 cache)", () => {
    const compute = vi.fn((n: number) => n * 2);
    const memoised = memo1(compute);

    memoised(1);
    memoised(2);
    memoised(1); // args match call #1, but cache now holds call #2 — must recompute

    expect(compute).toHaveBeenCalledTimes(3);
  });

  it("treats NaN as equal to itself (Object.is semantics, not ===)", () => {
    const compute = vi.fn((n: number) => n);
    const memoised = memo1(compute);

    memoised(NaN);
    memoised(NaN);

    expect(compute).toHaveBeenCalledTimes(1);
  });
});
