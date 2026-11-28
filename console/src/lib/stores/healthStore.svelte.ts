// HealthStore (DATA-CONTRACT §3.4): a thin replace-on-arrival holder for
// `/debug/health` (fetched once at bootstrap, then refreshed by `health`
// SSE frames). `data?.conformance` is intentionally allowed to be
// `undefined` — its absence means the team declined gap #9's counters, and
// that must survive round-trip rather than being defaulted to `[]` (which
// would misreport "counted zero" instead of "not counted").
import type { HealthPayload } from "../types/debug";

export class HealthStore {
  data = $state<HealthPayload | null>(null);

  set(payload: HealthPayload): void {
    this.data = payload;
  }

  /** Forget everything — the watcher behind the connection is a different
   * process (AfootClient, on a `boot_id` change). */
  clear(): void {
    this.data = null;
  }
}

export const healthStore = new HealthStore();
