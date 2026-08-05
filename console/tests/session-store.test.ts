// SessionStore selection semantics + ReportStore per-session keying — the
// client half of the multi-session contract (see crates/af-cli/tests/
// watch.rs `two_sessions_are_served_side_by_side` for the server half).
import { describe, expect, it, vi } from "vitest";

import { DEBUG_SESSION_CAP, SessionStore } from "../src/lib/stores/sessionStore.svelte";
import { ReportStore } from "../src/lib/stores/reportStore.svelte";
import type { DebugReport, SessionInfo } from "../src/lib/types/debug";

function info(id: string, tLast: string, agent = "claude-code"): SessionInfo {
  return {
    session_id: id,
    session_meta: { agent_app: { name: agent } } as SessionInfo["session_meta"],
    t_start: "2026-07-26T10:00:00.000Z",
    t_last: tLast,
    events: 3,
    attribution_policy: "l2_cpu_time",
    methodology: { version: "v", source: "bundled" },
    grid: { zone: "WOR", g_co2e_per_kwh: null, source: "test" },
    state_dir: "/tmp/x",
    schema_version: "0.1.0",
    mode: "watch --debug",
  };
}

describe("SessionStore selection", () => {
  it("follows the latest-active session until pinned, then sticks", () => {
    const store = new SessionStore();
    store.set(info("sess-old", "2026-07-26T10:00:05.000Z"));
    store.set(info("sess-new", "2026-07-26T10:00:09.000Z", "codex"));

    // Follow mode: latest t_last wins.
    expect(store.selectedId).toBe("sess-new");
    expect(store.data?.session_id).toBe("sess-new");

    // Pinning sticks even when the other session becomes more recent.
    store.pin("sess-old");
    store.set(info("sess-new", "2026-07-26T10:00:30.000Z", "codex"));
    expect(store.selectedId).toBe("sess-old");

    // Unpinning re-enters follow mode.
    store.pin(null);
    expect(store.selectedId).toBe("sess-new");
  });

  it("a pinned id that is no longer known falls back to follow-latest", () => {
    const store = new SessionStore();
    store.set(info("sess-a", "2026-07-26T10:00:05.000Z"));
    store.pin("sess-gone");
    expect(store.selectedId).toBe("sess-a");
  });

  it("lists sessions latest-first with their agent identity", () => {
    const store = new SessionStore();
    store.set(info("sess-a", "2026-07-26T10:00:05.000Z"));
    store.set(info("sess-b", "2026-07-26T10:00:09.000Z", "codex"));
    expect(store.list.map((row) => row.session_id)).toEqual(["sess-b", "sess-a"]);
    expect(store.list[0].agent_app?.name).toBe("codex");
  });

  it("matches the server's session cap and evicts oldest first", () => {
    const store = new SessionStore();
    for (let i = 0; i <= DEBUG_SESSION_CAP; i += 1) {
      store.set(info(`sess-${i}`, `2026-07-26T10:${String(i).padStart(2, "0")}:00.000Z`));
    }
    expect(store.list).toHaveLength(DEBUG_SESSION_CAP);
    expect(store.sessions["sess-0"]).toBeUndefined();
    expect(store.sessions[`sess-${DEBUG_SESSION_CAP}`]).toBeDefined();
  });

  it("reconciles a server eviction hint and releases a pinned ghost", () => {
    const store = new SessionStore();
    store.set(info("sess-old", "2026-07-26T10:00:00.000Z"));
    store.pin("sess-old");
    store.set({
      ...info("sess-new", "2026-07-26T10:01:00.000Z"),
      evicted_session_id: "sess-old",
    });
    expect(store.sessions["sess-old"]).toBeUndefined();
    expect(store.pinnedId).toBeNull();
    expect(store.selectedId).toBe("sess-new");
  });
});

describe("ReportStore per-session keying", () => {
  const report = (sessionId: string | undefined): DebugReport =>
    ({ level: "session", session_id: sessionId, impact_join: {}, by_model: [], estimation_status_histogram: {} }) as unknown as DebugReport;

  it("keeps one report per session instead of last-writer-wins", () => {
    const store = new ReportStore();
    store.set(report("sess-a"));
    store.set(report("sess-b"));
    expect(store.forSession("sess-a")?.session_id).toBe("sess-a");
    expect(store.forSession("sess-b")?.session_id).toBe("sess-b");
  });

  it("falls back to the unlabeled slot only when it cannot cross sessions", () => {
    const store = new ReportStore();
    // Unlabeled report (older server / mock): trustworthy for any session.
    store.set(report(undefined));
    expect(store.forSession("sess-a")).toBeDefined();

    // Labeled report for another session: never shown as sess-a's.
    store.set(report("sess-b"));
    expect(store.forSession("sess-a")).toBeUndefined();
  });

  it("matches the server's session cap and removes explicit evictions", () => {
    const store = new ReportStore();
    for (let i = 0; i <= DEBUG_SESSION_CAP; i += 1) store.set(report(`sess-${i}`));
    expect(Object.keys(store.bySession)).toHaveLength(DEBUG_SESSION_CAP);
    expect(store.forSession("sess-0")).toBeUndefined();

    store.set({
      level: "session",
      session_id: "sess-256",
      report_version: 2,
      evicted_session_ids: ["sess-1"],
    });
    expect(store.forSession("sess-1")).toBeUndefined();
  });

  it("turns compact invalidations into deduplicated direct report fetches", async () => {
    let resolveFetch!: (response: Response) => void;
    const fetchImpl = vi.fn(() => new Promise<Response>((resolve) => { resolveFetch = resolve; }));
    const store = new ReportStore(fetchImpl as typeof fetch);

    store.set({ level: "session", session_id: "sess-a", report_version: 7 });
    store.set({ level: "session", session_id: "sess-a", report_version: 8 });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl).toHaveBeenCalledWith("/debug/report?level=session&session_id=sess-a");

    resolveFetch(new Response(JSON.stringify({ ...report("sess-a"), report_version: 8 }), { status: 200 }));
    await vi.waitFor(() => expect(store.forSession("sess-a")).toBeDefined());
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it("does not let an older direct response overwrite a newer report", async () => {
    const responses: Array<(response: Response) => void> = [];
    const fetchImpl = vi.fn(() => new Promise<Response>((resolve) => responses.push(resolve)));
    const store = new ReportStore(fetchImpl as typeof fetch);

    store.set({ level: "session", session_id: "sess-a", report_version: 4 });
    store.set({ ...report("sess-a"), report_version: 5 } as DebugReport);
    responses[0](new Response(JSON.stringify({ ...report("sess-a"), report_version: 4 }), { status: 200 }));
    await Promise.resolve();
    await Promise.resolve();

    expect((store.forSession("sess-a") as DebugReport & { report_version?: number }).report_version).toBe(5);
  });
});
