// Agent status selectors (selectors/agents.ts): the text the masthead marks,
// the onboarding panel and the Health tab's agent table all render. Pure —
// `nowMs` is an argument, never read from a clock.
import { describe, expect, it } from "vitest";
import { selectAgentRows, selectWatcherLine } from "../src/lib/selectors/agents";
import type { AgentProcessInfo, AgentState, AgentStatus, WatcherStatus } from "../src/lib/types/debug";

const NOW = Date.parse("2026-11-16T09:20:00.000Z");

function agent(state: AgentState, overrides: Partial<AgentStatus> = {}): AgentStatus {
  return {
    id: "claude-code",
    name: "Claude Code",
    state,
    detail: null,
    action: null,
    config_path: null,
    processes: [],
    sessions: 0,
    last_event_at: null,
    ...overrides,
  };
}

function status(agents: AgentStatus[], overrides: Partial<WatcherStatus> = {}): WatcherStatus {
  return {
    boot_id: "b1",
    started_at: new Date(NOW - 4 * 60_000).toISOString(),
    af_version: "0.1.0",
    flush_interval_ms: 60000,
    receiver: { endpoint: "127.0.0.1:4318", note: null },
    agents,
    ...overrides,
  };
}

function row(a: AgentStatus) {
  return selectAgentRows(status([a]), NOW)[0];
}

describe("selectAgentRows", () => {
  it("is empty without a status", () => {
    expect(selectAgentRows(null, NOW)).toEqual([]);
  });

  it("receiving shows recency in seconds under a minute", () => {
    const r = row(agent("receiving", { last_event_at: new Date(NOW - 3_000).toISOString(), sessions: 1 }));
    expect(r.mark).toBe("●");
    expect(r.label).toBe("receiving · 3s ago");
    expect(r.note).toBeNull();
  });

  it("receiving turns to idle wording from 60s on", () => {
    expect(row(agent("receiving", { last_event_at: new Date(NOW - 59_000).toISOString() })).label).toBe("receiving · 59s ago");
    expect(row(agent("receiving", { last_event_at: new Date(NOW - 60_000).toISOString() })).label).toBe("receiving · idle 1m");
    expect(row(agent("receiving", { last_event_at: new Date(NOW - 12 * 60_000).toISOString() })).label).toBe("receiving · idle 12m");
    expect(row(agent("receiving", { last_event_at: new Date(NOW - 3 * 3_600_000).toISOString() })).label).toBe("receiving · idle 3h");
  });

  it("a last event stamped slightly in the future reads as just now, never negative", () => {
    expect(row(agent("receiving", { last_event_at: new Date(NOW + 2_000).toISOString() })).label).toBe("receiving · 0s ago");
  });

  it("needs_restart names the stale process", () => {
    const r = row(
      agent("needs_restart", {
        action: "restart claude",
        processes: [
          { pid: 6363, started_at: "2026-11-16T08:00:00.000Z", stale: true },
          { pid: 7000, started_at: "2026-11-16T09:10:00.000Z", stale: false },
        ],
      }),
    );
    expect(r.mark).toBe("◐");
    expect(r.label).toBe("configured, needs restart");
    expect(r.note).toBe("PID 6363 started before its settings changed — restart it");
    expect(r.action).toBe("restart claude");
  });

  it("several stale processes are all named", () => {
    const r = row(
      agent("needs_restart", {
        processes: [
          { pid: 1, started_at: "x", stale: true },
          { pid: 2, started_at: "x", stale: true },
        ],
      }),
    );
    expect(r.note).toBe("PIDs 1, 2 started before their settings changed — restart them");
  });

  it("a receiving agent still mentions its stale process", () => {
    const r = row(
      agent("receiving", {
        last_event_at: new Date(NOW - 1_000).toISOString(),
        processes: [{ pid: 6363, started_at: "x", stale: true }],
      }),
    );
    expect(r.label).toBe("receiving · 1s ago");
    expect(r.note).toBe("PID 6363 started before its settings changed — restart it");
  });

  it.each([
    ["waiting", "◐", "configured, waiting for first event"],
    ["misconfigured", "○", "misconfigured"],
    ["not_configured", "○", "not configured"],
    ["not_installed", "○", "not installed"],
  ] as const)("%s -> %s %s", (state, mark, label) => {
    const r = row(agent(state));
    expect(r.mark).toBe(mark);
    expect(r.label).toBe(label);
  });

  it("misconfigured carries the server's detail; not_configured its action", () => {
    expect(row(agent("misconfigured", { detail: "exports telemetry to http://x, not to http://y" })).note).toBe("exports telemetry to http://x, not to http://y");
    expect(row(agent("not_configured", { action: "af setup" })).action).toBe("af setup");
  });

  it("an unknown state from a newer watcher is shown as-is, not dropped", () => {
    const r = row(agent("something_new" as AgentState));
    expect(r.mark).toBe("○");
    expect(r.label).toBe("something_new");
  });

  it("keeps the server's order and ids", () => {
    const rows = selectAgentRows(status([agent("waiting"), agent("not_installed", { id: "codex", name: "Codex" })]), NOW);
    expect(rows.map((r) => [r.id, r.name])).toEqual([
      ["claude-code", "Claude Code"],
      ["codex", "Codex"],
    ]);
  });
});

describe("selectWatcherLine", () => {
  it("uptime, receiver and flush interval", () => {
    expect(selectWatcherLine(status([]), NOW)).toBe("up 4m · receiver 127.0.0.1:4318 · flushes every 60s");
  });

  it("uptime under a minute and over an hour", () => {
    expect(selectWatcherLine(status([], { started_at: new Date(NOW - 12_000).toISOString() }), NOW)).toContain("up 12s ·");
    expect(selectWatcherLine(status([], { started_at: new Date(NOW - 2 * 3_600_000 - 60_000).toISOString() }), NOW)).toContain("up 2h ·");
  });

  it("says so when the watcher has no receiver", () => {
    const line = selectWatcherLine(status([], { receiver: { endpoint: null, note: "no OTLP receiver is running in this af watch process" } }), NOW);
    expect(line).toBe("up 4m · no receiver (no OTLP receiver is running in this af watch process) · flushes every 60s");
  });

  it("a zero flush interval is 'flushes immediately'", () => {
    expect(selectWatcherLine(status([], { flush_interval_ms: 0 }), NOW)).toContain("flushes immediately");
  });

  it("is empty without a status", () => {
    expect(selectWatcherLine(null, NOW)).toBe("");
  });
});

describe("per-process rows", () => {
  const proc = (overrides: Partial<AgentProcessInfo>): AgentProcessInfo => ({
    pid: 7,
    started_at: new Date(NOW - 12 * 60_000).toISOString(),
    stale: false,
    cwd: "/Users/dev/work/api",
    config_path: null,
    state: "waiting",
    detail: null,
    action: null,
    session_id: null,
    last_event_at: null,
    ...overrides,
  });

  it("a payload without per-process state has no sub-rows (older watcher)", () => {
    const r = row(agent("needs_restart", { processes: [{ pid: 1, started_at: "x", stale: true }] }));
    expect(r.processes).toEqual([]);
    expect(r.note).toBe("PID 1 started before its settings changed — restart it");
  });

  it("names the process, where it runs, its state, why, and the command", () => {
    const r = row(
      agent("not_configured", {
        id: "codex",
        name: "Codex",
        processes: [
          proc({
            pid: 74475,
            cwd: "/Users/dev/oss/codecarbon",
            state: "not_configured",
            detail: "/Users/dev/oss/codecarbon/.codex/config.toml has no telemetry configuration",
            action: "af setup --agents codex --project /Users/dev/oss/codecarbon",
          }),
        ],
      }),
    );
    expect(r.processes).toEqual([
      {
        pid: 74475,
        where: "~/oss/codecarbon",
        mark: "○",
        label: "not configured",
        note: "~/oss/codecarbon/.codex/config.toml has no telemetry configuration",
        action: "af setup --agents codex --project /Users/dev/oss/codecarbon",
      },
    ]);
    expect(r.note, "the process row says it; the agent row does not repeat it").toBeNull();
  });

  it.each([
    [{ state: "receiving", last_event_at: new Date(NOW - 3_000).toISOString() }, "●", "receiving · 3s ago"],
    [{ state: "needs_restart", stale: true }, "◐", "configured, needs restart"],
    [{ state: "waiting" }, "◐", "configured, nothing received in 12m"],
    [{ state: "misconfigured", detail: "exports telemetry to http://x" }, "○", "misconfigured"],
    [{ state: "unknown", cwd: null }, "○", "cannot inspect this process"],
  ] as const)("%j -> %s %s", (overrides, mark, label) => {
    const [p] = row(agent("waiting", { processes: [proc(overrides as Partial<AgentProcessInfo>)] })).processes;
    expect(p.mark).toBe(mark);
    expect(p.label).toBe(label);
  });

  it("a restart note and a Linux home are worded and shortened too", () => {
    const [p] = row(agent("needs_restart", { processes: [proc({ state: "needs_restart", stale: true, cwd: "/home/dev/x" })] })).processes;
    expect(p.note).toBe("started before its settings changed");
    expect(p.where).toBe("~/x");
    expect(row(agent("waiting", { processes: [proc({ cwd: "/srv/app" })] })).processes[0].where).toBe("/srv/app");
    expect(row(agent("waiting", { processes: [proc({ cwd: null })] })).processes[0].where).toBeNull();
  });

  it("says how many processes are not exporting when the agent as a whole is receiving", () => {
    const r = row(
      agent("receiving", {
        last_event_at: new Date(NOW - 1_000).toISOString(),
        processes: [proc({ pid: 1, state: "receiving", last_event_at: new Date(NOW - 1_000).toISOString() }), proc({ pid: 2, state: "not_configured" }), proc({ pid: 3, state: "unknown" })],
      }),
    );
    expect(r.summary).toBe("1 of 3 not exporting");
    expect(row(agent("waiting", { processes: [proc({})] })).summary).toBeNull();
  });
});
