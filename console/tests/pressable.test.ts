// @vitest-environment happy-dom
//
// Tests for console/src/lib/actions/pressable.ts (Phase 2 Package A a11y
// cluster: the shared row-click keyboard pattern extracted from
// EventTable/Inspector/Attribution/DecisionLog/LaneChart). Exercises the
// action directly against a plain DOM node — no Svelte mount needed, since
// `pressable` is a plain `(node, handler) => ActionReturn` function.
import { describe, expect, it, vi } from "vitest";
import { pressable } from "../src/lib/actions/pressable";

function keydown(key: string): KeyboardEvent {
  return new KeyboardEvent("keydown", { key, cancelable: true });
}

describe("pressable", () => {
  it("sets role=button and tabindex=0 on the node", () => {
    const node = document.createElement("div");
    pressable(node, () => {});
    expect(node.getAttribute("role")).toBe("button");
    expect(node.getAttribute("tabindex")).toBe("0");
  });

  it("invokes the handler on click", () => {
    const node = document.createElement("div");
    const handler = vi.fn();
    pressable(node, handler);
    node.dispatchEvent(new MouseEvent("click"));
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("invokes the handler on Enter and on Space, not on other keys", () => {
    const node = document.createElement("div");
    const handler = vi.fn();
    pressable(node, handler);

    node.dispatchEvent(keydown("Enter"));
    node.dispatchEvent(keydown(" "));
    node.dispatchEvent(keydown("a"));
    node.dispatchEvent(keydown("Tab"));

    expect(handler).toHaveBeenCalledTimes(2);
  });

  it("preventDefault()s Space (so the page doesn't also scroll), not other keys", () => {
    const node = document.createElement("div");
    pressable(node, () => {});

    const space = keydown(" ");
    node.dispatchEvent(space);
    expect(space.defaultPrevented).toBe(true);

    const other = keydown("a");
    node.dispatchEvent(other);
    expect(other.defaultPrevented).toBe(false);
  });

  it("update() swaps the handler without re-adding listeners", () => {
    const node = document.createElement("div");
    const first = vi.fn();
    const second = vi.fn();
    const ret = pressable(node, first);
    ret?.update?.(second);

    node.dispatchEvent(new MouseEvent("click"));
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("destroy() removes both listeners", () => {
    const node = document.createElement("div");
    const handler = vi.fn();
    const ret = pressable(node, handler);
    ret?.destroy?.();

    node.dispatchEvent(new MouseEvent("click"));
    node.dispatchEvent(keydown("Enter"));
    expect(handler).not.toHaveBeenCalled();
  });
});
