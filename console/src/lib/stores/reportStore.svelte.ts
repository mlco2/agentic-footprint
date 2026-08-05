// ReportStore (DATA-CONTRACT §3.4, §2.6): replace-on-arrival, keyed by
// report level. The `session` level arrives continuously over SSE (`report`
// frames); `task`/`tool` are fetched lazily on demand (selectors that need
// them land in a later task — this is just the fetch capability).
import type { DebugReport } from "../types/debug";
import { boundFetch } from "../client/boundFetch";
import { DEBUG_SESSION_CAP } from "./sessionStore.svelte";

type Level = DebugReport["level"];
type VersionedReport = DebugReport & { report_version?: number };
type ReportNotification = Pick<DebugReport, "level"> & {
  session_id?: string;
  report_version: number;
  evicted_session_ids?: string[];
};

export class ReportStore {
  levels = $state<Partial<Record<Level, DebugReport>>>({});
  /** Session-level reports by `session_id` — the multi-session server
   * stamps each report frame with the session it is about, and one
   * session's frame must not clobber another's. Reports without a
   * `session_id` (older servers, the mock) only land in `levels`. */
  bySession = $state<Record<string, DebugReport>>({});
  private inflight = new Map<Level, Promise<DebugReport>>();
  private refreshInflight = new Map<string, Promise<void>>();
  private desiredVersions = new Map<string, number>();
  private storedVersions = new Map<string, number>();
  private sessionOrder: string[] = [];
  private readonly fetchImpl: typeof fetch;

  constructor(fetchImpl: typeof fetch = boundFetch) {
    this.fetchImpl = fetchImpl;
  }

  /** Replace-on-arrival for one level — from an SSE `report` frame. Keyed by
   * the payload's own `level` field, which is correct here: a pushed frame
   * was never "requested" at some other level, so what the server says it
   * is IS the key. (Contrast `fetchLevel`, which keys by request instead —
   * see its comment.) */
  set(report: DebugReport | ReportNotification): void {
    if ("evicted_session_ids" in report) {
      for (const sessionId of report.evicted_session_ids ?? []) this.evict(sessionId);
    }
    if (!("impact_join" in report)) {
      if (report.session_id) this.invalidate(report.session_id, report.report_version);
      return;
    }
    this.storeFull(report);
  }

  private storeFull(report: VersionedReport): void {
    if (report.session_id) {
      const currentVersion = this.storedVersions.get(report.session_id) ?? -1;
      const nextVersion = report.report_version ?? currentVersion + 1;
      if (nextVersion < currentVersion) return;
      this.storedVersions.set(report.session_id, nextVersion);
      this.trackSession(report.session_id);
      this.bySession = { ...this.bySession, [report.session_id]: report };
    }
    this.levels = { ...this.levels, [report.level]: report };
  }

  private invalidate(sessionId: string, version: number): void {
    const desired = Math.max(this.desiredVersions.get(sessionId) ?? -1, version);
    this.desiredVersions.set(sessionId, desired);
    this.trackSession(sessionId);
    if (!this.refreshInflight.has(sessionId)) {
      const refresh = this.refreshSession(sessionId).finally(() => this.refreshInflight.delete(sessionId));
      this.refreshInflight.set(sessionId, refresh);
    }
  }

  private async refreshSession(sessionId: string): Promise<void> {
    while (this.desiredVersions.has(sessionId)) {
      const desired = this.desiredVersions.get(sessionId) ?? -1;
      try {
        const encoded = encodeURIComponent(sessionId);
        const res = await this.fetchImpl(`/debug/report?level=session&session_id=${encoded}`);
        if (!res.ok) return;
        const report = (await res.json()) as VersionedReport;
        if (!this.desiredVersions.has(sessionId)) return;
        this.storeFull(report);
        const received = report.report_version ?? desired;
        if (received >= (this.desiredVersions.get(sessionId) ?? desired)) {
          this.desiredVersions.delete(sessionId);
          return;
        }
      } catch {
        return;
      }
    }
  }

  evict(sessionId: string): void {
    const next = { ...this.bySession };
    delete next[sessionId];
    this.bySession = next;
    this.desiredVersions.delete(sessionId);
    this.storedVersions.delete(sessionId);
    this.sessionOrder = this.sessionOrder.filter((id) => id !== sessionId);
    if (this.levels.session?.session_id === sessionId) {
      const { session: _evicted, ...levels } = this.levels;
      this.levels = levels;
    }
  }

  private trackSession(sessionId: string): void {
    if (!this.sessionOrder.includes(sessionId)) this.sessionOrder.push(sessionId);
    while (this.sessionOrder.length > DEBUG_SESSION_CAP) {
      const evicted = this.sessionOrder[0];
      if (evicted) this.evict(evicted);
    }
  }

  get(level: Level): DebugReport | undefined {
    return this.levels[level];
  }

  /** The session-level report for one session, falling back to the
   * unlabeled `levels.session` slot — which is the same report on servers
   * that predate multi-session, and the only report there is on the mock. */
  forSession(sessionId: string | null): DebugReport | undefined {
    if (sessionId !== null) {
      const scoped = this.bySession[sessionId];
      if (scoped) return scoped;
      const unscoped = this.levels.session;
      // An unlabeled report is only trustworthy for `sessionId` when the
      // server doesn't label reports at all — if it names a *different*
      // session, showing it here would silently cross sessions.
      if (unscoped && (unscoped.session_id === undefined || unscoped.session_id === sessionId)) {
        return unscoped;
      }
      return undefined;
    }
    return this.levels.session;
  }

  /** Lazily fetches `GET /debug/report?level=task|session|tool` and stores
   * the result, with in-flight dedup per level.
   *
   * The real `af watch --debug` server IGNORES `?level=` and always serves
   * the session-level report (docs/design-log.md: "`?level=` is ignored —
   * session only"). So the cache slot here is keyed by what THIS call
   * requested, not by `report.level` (which `set()` uses) — otherwise a
   * `fetchLevel("task")` caller against the real server would cache its
   * result under `levels.session` and `get("task")` would stay undefined
   * forever, re-fetching on every call instead of caching. The stored
   * `DebugReport` object's own `.level` field is left exactly as the server
   * reported it (e.g. still `"session"`) rather than rewritten to match the
   * request — never claim the server computed something at a level it
   * didn't. */
  async fetchLevel(level: Level): Promise<DebugReport> {
    const existing = this.inflight.get(level);
    if (existing) return existing;

    const promise = (async () => {
      const res = await this.fetchImpl(`/debug/report?level=${encodeURIComponent(level)}`);
      if (!res.ok) throw new Error(`GET /debug/report?level=${level} failed: ${res.status}`);
      const report = (await res.json()) as DebugReport;
      if (level === "session" && report.session_id) this.storeFull(report);
      else this.levels = { ...this.levels, [level]: report };
      return report;
    })();

    this.inflight.set(level, promise);
    try {
      return await promise;
    } finally {
      this.inflight.delete(level);
    }
  }
}

export const reportStore = new ReportStore();
