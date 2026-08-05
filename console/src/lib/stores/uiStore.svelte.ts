// UiStore: view-only state (tab, selection, filters, live/pause, clock).
// Small scalars, so each is its own `$state` field rather than a bulk
// non-reactive structure behind a `rev` counter (that pattern is reserved
// for EventStore/AllocStore's actual bulk data).
//
// `nowMs` is the one place outside AfClient allowed to read the wall clock
// (architecture rule: "No Date.now() outside the client/uiStore tick
// path") — and even here, only from `tick()`, called once per second by
// AfClient's single setInterval, never from a query or a render path.
import { SvelteSet } from "svelte/reactivity";

export type Tab = "timeline" | "stream" | "attribution" | "impact" | "health";

export class UiStore {
  tab = $state<Tab>("timeline");
  selectedId = $state<string | null>(null);
  readonly hiddenTypes = new SvelteSet<string>();
  live = $state(true);
  nowMs = $state(Date.now());

  setTab(tab: Tab): void {
    this.tab = tab;
  }

  select(id: string | null): void {
    this.selectedId = id;
  }

  setLive(live: boolean): void {
    this.live = live;
  }

  toggleLive(): void {
    this.live = !this.live;
  }

  toggleHiddenType(type: string): void {
    if (this.hiddenTypes.has(type)) this.hiddenTypes.delete(type);
    else this.hiddenTypes.add(type);
  }

  /** Called once per second by AfClient's single interval. Freezes `nowMs`
   * while paused (global-constraints.md #6: "pause = freeze now"). */
  tick(): void {
    if (this.live) this.nowMs = Date.now();
  }
}

export const uiStore = new UiStore();
