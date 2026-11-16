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
import type { AgentProcessInfo, AgentStatus, WatcherStatus } from "../types/debug";

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
  /** One entry per running process the watcher judged on its own — empty
   * for a watcher that only lists processes. */
  processes: ProcessRowView[];
  /** "1 of 3 not exporting": processes that need something done while the
   * agent as a whole is receiving. */
  summary: string | null;
}

export interface ProcessRowView {
  pid: number;
  /** The directory it runs in, home shortened to `~`; `null` if unknown. */
  where: string | null;
  mark: AgentMark;
  label: string;
  note: string | null;
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

/** Mark and wording for every state but `receiving`, whose label carries
 * a recency. */
const WORDING: Record<string, [AgentMark, string]> = {
  needs_restart: ["◐", "configured, needs restart"],
  waiting: ["◐", "configured, waiting for first event"],
  misconfigured: ["○", "misconfigured"],
  not_configured: ["○", "not configured"],
  not_installed: ["○", "not installed"],
};

/** `/Users/me/x` and `/home/me/x` read as `~/x` — only where a path
 * starts (the text's start, or after a space), and never for the shared
 * directory macOS keeps beside the real homes. */
function tilde(text: string): string {
  return text.replace(/(^|\s)\/(?:Users|home)\/(?!Shared(?:\/|$))[^/\s]+(?=\/|\s|$)/g, "$1~");
}

const NOT_EXPORTING = new Set(["needs_restart", "misconfigured", "not_configured"]);

function processView(process: AgentProcessInfo, nowMs: number): ProcessRowView {
  const base = { pid: process.pid, where: process.cwd ? tilde(process.cwd) : null, action: process.action ?? null };
  const detail = process.detail ? tilde(process.detail) : null;
  switch (process.state) {
    case "receiving":
      return { ...base, mark: "●", label: recency(process.last_event_at ?? null, nowMs), note: null };
    case "needs_restart":
      return { ...base, mark: "◐", label: "configured, needs restart", note: "started before its settings changed" };
    case "waiting": {
      // The safety net for setups the watcher does not model: silence is
      // stated as silence, with how long it has lasted.
      const started = Date.parse(process.started_at);
      const since = Number.isNaN(started) ? "" : ` in ${coarse(nowMs - started)}`;
      return { ...base, mark: "◐", label: `configured, nothing received${since}`, note: detail };
    }
    case "unknown":
      return { ...base, mark: "○", label: "cannot inspect this process", note: null };
    default: {
      const [mark, label] = WORDING[String(process.state)] ?? ["○", String(process.state)];
      return { ...base, mark, label, note: detail };
    }
  }
}

function view(agent: AgentStatus, nowMs: number): AgentRowView {
  const receiving = agent.state === "receiving";
  // A state this console does not know (a newer watcher) is shown as the
  // watcher spelled it rather than dropped or guessed at.
  const [mark, label] = receiving ? (["●", recency(agent.last_event_at, nowMs)] as const) : (WORDING[agent.state] ?? ["○", String(agent.state)]);
  const judged = (agent.processes ?? []).filter((process) => process.state !== undefined);
  const idle = judged.filter((process) => NOT_EXPORTING.has(String(process.state))).length;
  // With a row per process, each says its own reason; the agent row keeps
  // only what is about the agent itself.
  const ownNote = receiving || agent.state === "needs_restart" ? staleNote(agent) : agent.detail;
  return {
    id: agent.id,
    name: agent.name,
    mark,
    label,
    note: judged.length > 0 ? agent.detail : ownNote,
    action: agent.action,
    processes: judged.map((process) => processView(process, nowMs)),
    summary: receiving && idle > 0 ? `${idle} of ${judged.length} not exporting` : null,
  };
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
