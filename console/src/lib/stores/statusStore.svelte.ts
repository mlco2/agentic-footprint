// StatusStore: a thin replace-on-arrival holder for the watcher-level status
// (`GET /debug/status`, then `status` SSE frames) — which watcher process
// this is, its receiver, and one row per coding agent. Independent of any
// session, so it is what the console can show before the first event.
//
// `unavailable` is a distinct fact from "not loaded yet": the watcher
// answered, and has no status route (an `af` older than the route, reached
// through the Vite dev proxy). The UI must say "status unavailable" then,
// not sit on a loading state forever.
import type { WatcherStatus } from "../types/debug";

export class StatusStore {
  data = $state<WatcherStatus | null>(null);
  unavailable = $state(false);

  set(status: WatcherStatus): void {
    this.data = status;
    this.unavailable = false;
  }

  markUnavailable(): void {
    this.data = null;
    this.unavailable = true;
  }

  clear(): void {
    this.data = null;
    this.unavailable = false;
  }
}

export const statusStore = new StatusStore();
