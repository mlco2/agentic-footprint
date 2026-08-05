// SessionStore (DATA-CONTRACT §3.4): the console's session dimension.
//
// Multi-agent watch serves several sessions side by side; this store holds
// every known one (full `SessionInfo` from `session` SSE frames /
// `GET /debug/session`, plus lighter `SessionSummary` rows from
// `GET /debug/sessions`) and the user's picker choice. `data` — the field
// every consumer already read when this store was a single-session holder —
// is now the *selected* session, so Impact/Attribution/Footer scope to the
// picker without knowing it exists. Timeline/Stream deliberately never read
// this store: all sessions' events stay visible there, unfiltered.
//
// Selection semantics mirror the live/pause toggle: `pinnedId === null`
// means "follow latest active" (greatest `t_last`, the server's own
// ordering key); a pinned id sticks until unpinned, even if another
// session becomes more recent.
import type { SessionInfo, SessionSummary } from "../types/debug";

export const DEBUG_SESSION_CAP = 256;
type SessionInfoWithEviction = SessionInfo & { evicted_session_id?: string };

export class SessionStore {
  sessions = $state<Record<string, SessionInfo>>({});
  summaries = $state<Record<string, SessionSummary>>({});
  pinnedId = $state<string | null>(null);
  private sessionOrder: string[] = [];

  /** Replace-on-arrival for one session's full info — from bootstrap's
   * `GET /debug/session` or a `session` SSE frame. */
  set(info: SessionInfoWithEviction): void {
    if (info.evicted_session_id) this.evict(info.evicted_session_id);
    if (!this.sessionOrder.includes(info.session_id)) this.sessionOrder.push(info.session_id);
    this.sessions = { ...this.sessions, [info.session_id]: info };
    this.enforceCap();
  }

  /** Replace the summary list — from `GET /debug/sessions`. */
  setSummaries(rows: SessionSummary[]): void {
    const next: Record<string, SessionSummary> = {};
    for (const row of rows.slice(0, DEBUG_SESSION_CAP)) next[row.session_id] = row;
    this.summaries = next;
    const known = new Set(Object.keys(next));
    const sessions = { ...this.sessions };
    for (const id of Object.keys(sessions)) {
      if (!known.has(id)) delete sessions[id];
    }
    this.sessions = sessions;
    this.sessionOrder = this.sessionOrder.filter((id) => known.has(id));
  }

  private evict(sessionId: string): void {
    const sessions = { ...this.sessions };
    const summaries = { ...this.summaries };
    delete sessions[sessionId];
    delete summaries[sessionId];
    this.sessions = sessions;
    this.summaries = summaries;
    this.sessionOrder = this.sessionOrder.filter((id) => id !== sessionId);
    if (this.pinnedId === sessionId) this.pinnedId = null;
  }

  private enforceCap(): void {
    while (this.sessionOrder.length > DEBUG_SESSION_CAP) {
      const evicted = this.sessionOrder[0];
      if (evicted) this.evict(evicted);
    }
  }

  /** `null` re-enters follow-latest mode. */
  pin(id: string | null): void {
    this.pinnedId = id;
  }

  /** Every known session as picker rows, latest-active first. Full infos
   * win over summaries for the same id (they're fresher and richer). */
  get list(): SessionSummary[] {
    const rows: Record<string, SessionSummary> = { ...this.summaries };
    for (const info of Object.values(this.sessions)) {
      rows[info.session_id] = {
        session_id: info.session_id,
        agent_app: info.session_meta?.agent_app ?? null,
        t_start: info.t_start,
        t_last: info.t_last ?? info.t_start,
        events: info.events ?? 0,
      };
    }
    return Object.values(rows).sort((a, b) => (a.t_last < b.t_last ? 1 : -1));
  }

  /** The picked session id, or the latest-active one in follow mode.
   * A pinned id that no longer exists (aged out server-side) falls back to
   * follow-latest rather than rendering a ghost. */
  get selectedId(): string | null {
    if (this.pinnedId !== null && (this.sessions[this.pinnedId] || this.summaries[this.pinnedId])) {
      return this.pinnedId;
    }
    return this.list[0]?.session_id ?? null;
  }

  /** The selected session's full info — the field every single-session
   * consumer already read. `null` until the selected session's full info
   * has arrived (a summary alone can't honestly stand in for
   * methodology/grid). */
  get data(): SessionInfo | null {
    const id = this.selectedId;
    return id === null ? null : (this.sessions[id] ?? null);
  }
}

export const sessionStore = new SessionStore();
