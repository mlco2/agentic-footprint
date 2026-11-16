// Agent status selectors: what the masthead marks, the onboarding panel and
// the Health tab's agent table render from `statusStore.data`
// (`GET /debug/status`). Pure module: no Svelte imports, no `Date.now()` —
// `nowMs` arrives as an argument from `uiStore.nowMs`.
//
// The state itself is decided by the watcher (crates/af-cli/src/cmd/
// agents.rs); nothing here second-guesses it. What this module adds is
// wording, and recency: `receiving` is sticky server-side for the watcher's
// lifetime, so "3s ago" versus "idle 12m" is derived here from
// `last_event_at` — a subtraction used only to choose words, not a reported
// quantity.
import type { AgentStatus, WatcherStatus } from "../types/debug";

/** State is never carried by the mark alone: every row has `label` too. */
export type AgentMark = "●" | "◐" | "○";

export interface AgentRowView {
  id: string;
  name: string;
  mark: AgentMark;
  label: string;
  /** A second line explaining the state, when there is something to say. */
  note: string | null;
  /** A command to run, when the watcher names an obvious next step. */
  action: string | null;
}

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;

/** "12s", "4m", "2h" — whole units, rounded down, never negative. */
function coarse(ms: number): string {
  const elapsed = Math.max(0, ms);
  if (elapsed < MINUTE) return `${Math.floor(elapsed / SECOND)}s`;
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)}m`;
  return `${Math.floor(elapsed / HOUR)}h`;
}

function recency(lastEventAt: string | null, nowMs: number): string {
  const at = lastEventAt === null ? NaN : Date.parse(lastEventAt);
  if (Number.isNaN(at)) return "receiving";
  const elapsed = Math.max(0, nowMs - at);
  return elapsed < MINUTE ? `receiving · ${coarse(elapsed)} ago` : `receiving · idle ${coarse(elapsed)}`;
}

function staleNote(agent: AgentStatus): string | null {
  const stale = (agent.processes ?? []).filter((process) => process.stale).map((process) => process.pid);
  if (stale.length === 0) return null;
  return stale.length === 1
    ? `PID ${stale[0]} started before its settings changed — restart it`
    : `PIDs ${stale.join(", ")} started before their settings changed — restart them`;
}

function view(agent: AgentStatus, nowMs: number): AgentRowView {
  const base = { id: agent.id, name: agent.name, action: agent.action };
  switch (agent.state) {
    case "receiving":
      return { ...base, mark: "●", label: recency(agent.last_event_at, nowMs), note: staleNote(agent) };
    case "needs_restart":
      return { ...base, mark: "◐", label: "configured, needs restart", note: staleNote(agent) };
    case "waiting":
      return { ...base, mark: "◐", label: "configured, waiting for first event", note: agent.detail };
    case "misconfigured":
      return { ...base, mark: "○", label: "misconfigured", note: agent.detail };
    case "not_configured":
      return { ...base, mark: "○", label: "not configured", note: agent.detail };
    case "not_installed":
      return { ...base, mark: "○", label: "not installed", note: agent.detail };
    default:
      // A state this console does not know (a newer watcher): shown as the
      // watcher spelled it rather than dropped or guessed at.
      return { ...base, mark: "○", label: String(agent.state), note: agent.detail };
  }
}

export function selectAgentRows(status: WatcherStatus | null, nowMs: number): AgentRowView[] {
  return status === null ? [] : status.agents.map((agent) => view(agent, nowMs));
}

/** The onboarding panel's one-line description of the watcher itself. */
export function selectWatcherLine(status: WatcherStatus | null, nowMs: number): string {
  if (status === null) return "";
  const started = Date.parse(status.started_at);
  const up = Number.isNaN(started) ? "up" : `up ${coarse(nowMs - started)}`;
  const receiver =
    status.receiver.endpoint !== null
      ? `receiver ${status.receiver.endpoint}`
      : `no receiver${status.receiver.note ? ` (${status.receiver.note})` : ""}`;
  // Always in seconds: it is a setting (`--batch-interval-ms`), and "60s"
  // is how it reads next to "within a minute" below it.
  const flushMs = status.flush_interval_ms;
  const flush = flushMs <= 0 ? "flushes immediately" : flushMs < SECOND ? `flushes every ${flushMs}ms` : `flushes every ${Math.round(flushMs / SECOND)}s`;
  return `${up} · ${receiver} · ${flush}`;
}
