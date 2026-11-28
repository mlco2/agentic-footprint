// agentic-footprint: OpenCode collector plugin (installed by `afoot setup`).
//
// OpenCode loads every file in its global plugin directory
// (`$XDG_CONFIG_HOME/opencode/plugin/`, default `~/.config/opencode/plugin/`).
// This plugin turns OpenCode's own bus events (the `event` hook, as in
// OpenCode 1.19.x) into Contract #1 facts and appends them to
// `$AFOOT_STATE_DIR/spool/opencode.<session>.jsonl`, the same file transport as
// the Claude Code hook and the Pi extension:
//
//   session.created                              -> session_meta
//   message.part.updated, part "step-finish"     -> llm_call (one per model call)
//   message.part.updated, tool completed / error -> action_span
//
// Temporary: released OpenCode sessions do not write the durable event
// stream `afoot collect opencode` reads. It never reads prompts, message text,
// tool inputs, outputs or error messages, and a failure never reaches
// OpenCode. Only the plugin is exported: OpenCode treats every export of
// this file as a plugin.

import { appendFileSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const SCHEMA_VERSION = "0.1.0";
const COLLECTOR = { name: "opencode", version: "0.1.0-plugin" };

type Status = "ok" | "error" | "cancelled" | "unknown";
type Fact = Record<string, unknown> & { session_id: string };
type Tokens = { input?: number; output?: number; reasoning?: number; cache?: { read?: number; write?: number } };

export const AgenticFootprint = async (_input: { directory?: string }) => {
  // Per assistant message: what a step-finish needs to name the model call.
  const messages = new Map<string, { provider: string; model: string; agent?: string }>();
  const stepStartedAt = new Map<string, number>();
  const written = new Set<string>();
  const sessionsSeen = new Set<string>();

  function stateDir(): string {
    const env = process.env;
    if (env.AFOOT_STATE_DIR) return env.AFOOT_STATE_DIR;
    if (process.platform === "win32") return join(env.LOCALAPPDATA ?? homedir(), "agentic-footprint");
    return join(env.HOME ?? homedir(), ".local", "state", "agentic-footprint");
  }

  function sanitize(raw: string): string {
    const clean = raw.replace(/[^A-Za-z0-9._-]/g, "");
    return clean === "" || clean.startsWith(".") ? `x${clean}` : clean;
  }

  function append(sessionId: string, eventId: string, type: string, payload: Record<string, unknown>, attribution?: Record<string, unknown>): void {
    // OpenCode re-sends part updates; an event_id is written once per process
    // (and the store ignores one it already holds).
    if (written.has(eventId)) return;
    const fact: Fact = {
      schema_version: SCHEMA_VERSION,
      event_id: eventId,
      ts: new Date().toISOString(),
      collector: COLLECTOR,
      session_id: sessionId,
      type,
      payload,
    };
    if (attribution) fact.attribution = attribution;
    const dir = join(stateDir(), "spool");
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    appendFileSync(join(dir, `${COLLECTOR.name}.${sanitize(sessionId)}.jsonl`), `${JSON.stringify(fact)}\n`, { mode: 0o600 });
    written.add(eventId);
  }

  function sessionMeta(sessionId: string): void {
    if (sessionsSeen.has(sessionId)) return;
    const os = process.platform === "darwin" ? "macos" : process.platform === "win32" ? "windows" : process.platform;
    // Stable ID: a resumed session is stored once.
    append(sessionId, `opencode-session-meta-${sanitize(sessionId)}`, "session_meta", { agent_app: { name: "opencode" }, os });
    sessionsSeen.add(sessionId);
  }

  function count(value: unknown): number | undefined {
    return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.round(value) : undefined;
  }

  function usage(tokens: Tokens | undefined): Record<string, number> {
    const out: Record<string, number> = {};
    const pairs: [unknown, string][] = [
      [tokens?.input, "input_tokens"],
      [tokens?.output, "output_tokens"],
      // OpenCode's reasoning count is separate from output (as in the
      // durable-stream collector).
      [tokens?.reasoning, "thought_tokens"],
      [tokens?.cache?.read, "cached_read_tokens"],
      [tokens?.cache?.write, "cached_write_tokens"],
    ];
    for (const [value, key] of pairs) {
      const n = count(value);
      if (n !== undefined) out[key] = n;
    }
    return out;
  }

  // Same table as `afoot collect opencode` (crates/afoot-cli/src/cmd/opencode.rs).
  function toolKind(name: string): string {
    const n = name.toLowerCase();
    if (["bash", "shell", "terminal"].includes(n)) return "bash";
    if (["edit", "write", "read", "patch", "multiedit"].includes(n)) return "file_op";
    if (["task", "agent", "subagent"].includes(n)) return "subagent";
    if (["webfetch", "websearch", "fetch", "search"].includes(n)) return "web";
    if (n.startsWith("mcp")) return "mcp";
    return "other";
  }

  function locus(kind: string): string {
    if (kind === "bash" || kind === "file_op" || kind === "subagent") return "local";
    if (kind === "web") return "remote";
    return "unknown";
  }

  function onMessage(info: any): void {
    if (!info || info.role !== "assistant" || typeof info.id !== "string") return;
    if (typeof info.sessionID === "string") sessionMeta(info.sessionID);
    if (typeof info.providerID === "string" && typeof info.modelID === "string") {
      messages.set(info.id, { provider: info.providerID, model: info.modelID, agent: typeof info.mode === "string" ? info.mode : undefined });
    }
  }

  function onPart(part: any): void {
    if (!part || typeof part.id !== "string" || typeof part.sessionID !== "string") return;
    const sid: string = part.sessionID;
    if (part.type === "step-start" && typeof part.messageID === "string") {
      stepStartedAt.set(part.messageID, Date.now());
      return;
    }
    if (part.type === "step-finish" && typeof part.messageID === "string") {
      const message = messages.get(part.messageID);
      // Contract #1 needs provider and model; without them it is not a model
      // call we can attribute (same rule as the other collectors).
      if (!message) return;
      sessionMeta(sid);
      const payload: Record<string, unknown> = {
        provider: message.provider,
        model_id_requested: message.model,
        usage: usage(part.tokens),
        usage_source: "agent_telemetry",
        status: "ok" as Status,
        streaming: true,
      };
      const started = stepStartedAt.get(part.messageID);
      stepStartedAt.delete(part.messageID);
      if (started !== undefined) payload.duration_ms = Math.max(0, Date.now() - started);
      const attribution: Record<string, unknown> = { task_id: part.messageID };
      if (message.agent) attribution.agent_id = message.agent;
      append(sid, `opencode-llm-${sanitize(sid)}-${sanitize(part.id)}`, "llm_call", payload, attribution);
      return;
    }
    if (part.type === "tool" && typeof part.tool === "string" && typeof part.callID === "string") {
      const state = part.state ?? {};
      if (state.status !== "completed" && state.status !== "error") return;
      const start = count(state.time?.start);
      const end = count(state.time?.end);
      if (start === undefined || end === undefined) return;
      sessionMeta(sid);
      let status: Status = "ok";
      if (state.status === "error") {
        // Only classified, never copied: the error text may hold user data.
        const text = String(state.error ?? "").toLowerCase();
        status = text.includes("abort") || text.includes("cancel") ? "cancelled" : "error";
      }
      const kind = toolKind(part.tool);
      append(
        sid,
        `opencode-span-${sanitize(sid)}-${sanitize(part.id)}`,
        "action_span",
        {
          span_id: part.callID,
          tool_name: part.tool,
          tool_kind: kind,
          execution_locus: locus(kind),
          t_start: new Date(start).toISOString(),
          t_end: new Date(end).toISOString(),
          status,
        },
        { tool_call_id: part.callID },
      );
    }
  }

  return {
    async event({ event }: { event: { type?: string; properties?: any } }) {
      try {
        const props = event?.properties ?? {};
        if (event?.type === "session.created" && typeof props.info?.id === "string") sessionMeta(props.info.id);
        else if (event?.type === "message.updated") onMessage(props.info);
        else if (event?.type === "message.part.updated") onPart(props.part);
      } catch {
        // Never let collection break an OpenCode session.
      }
    },
  };
};
