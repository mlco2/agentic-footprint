// Shared row-click keyboard pattern (Phase 2 Package A a11y cluster).
// EventTable rows, Inspector correlated rows, Attribution sample rows,
// DecisionLog refs and LaneChart bars each hand-rolled the identical triple
// — `role="button"`, `tabindex="0"`, and an Enter-or-Space `onkeydown` next
// to the element's own `onclick` — so a keyboard user can activate a
// non-<button> clickable row/bar exactly like a mouse user. One Svelte
// action, applied as `use:pressable={() => onSelect(id)}`, sets the ARIA
// role and wires both click and keyboard activation to the same handler; a
// call site only supplies what "activated" means for that element.
import type { Action } from "svelte/action";

export const pressable: Action<HTMLElement, () => void> = (node, handler) => {
  node.setAttribute("role", "button");
  node.setAttribute("tabindex", "0");

  let current = handler;

  function onClick(): void {
    current();
  }
  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault(); // Space must not also scroll the page.
      current();
    }
  }

  node.addEventListener("click", onClick);
  node.addEventListener("keydown", onKeydown);

  return {
    update(next) {
      current = next;
    },
    destroy() {
      node.removeEventListener("click", onClick);
      node.removeEventListener("keydown", onKeydown);
    },
  };
};
