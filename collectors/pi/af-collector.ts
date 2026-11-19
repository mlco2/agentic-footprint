// collectors/pi/af-collector.ts
//
// Pi collector: a Pi extension that turns Pi's in-process lifecycle events
// into Contract #1 facts (schemas/v0.1/events.schema.json) and appends them
// to `$AF_STATE_DIR/spool/pi.<session-id>.jsonl`, the same file transport the
// Claude Code hook and the OpenCode collector use. `af watch` / `af report`
// ingest the spool; nothing is estimated here.
//
// Exactly-once boundaries (chosen from capture-events.ts research and Pi's
// extension docs):
//   - session_start                          → one `session_meta`
//   - message_end with role "assistant"      → one `llm_call`
//     (the same message is repeated in turn_end and agent_end; those are
//     deliberately ignored, or every call would be counted two or three times)
//   - tool_execution_start + tool_execution_end, paired by toolCallId
//                                            → one `action_span`
//     (in parallel tool mode ends arrive in completion order, not start order)
//   - session_shutdown closes still-open spans as `cancelled`.
//
// Privacy: never reads prompts, message content, tool arguments or tool
// results — only identifiers, model names, token counts and timestamps.
//
// Safety: collection is best-effort and the running Pi session comes first.
// Every handler is wrapped so a collector failure can never break a turn;
// failures go to `$AF_STATE_DIR/tmp/pi-collector-errors.log`.
//
// Load it explicitly:  pi --extension /absolute/path/to/af-collector.ts
// or copy it into Pi's global extensions directory (~/.pi/agent/extensions/).

import { appendFileSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export const COLLECTOR = { name: "pi", version: "0.1.0" } as const;
const SCHEMA_VERSION = "0.1.0";

// ---- Pi's extension API, only the parts this collector reads -------------

type Usage = {
  input?: number;
  output?: number;
  cacheRead?: number;
  cacheWrite?: number;
  reasoning?: number;
};

type AssistantMessage = {
  role?: string;
  api?: string;
  provider?: string;
  model?: string;
  responseModel?: string;
  responseId?: string;
  stopReason?: string;
  timestamp?: number;
  usage?: Usage;
};

export type PiContext = {
  sessionManager?: { getSessionId?: () => string | undefined };
};

export type PiEvent = Record<string, unknown>;

export type PiApi = {
  on: (name: string, handler: (event: PiEvent, ctx: PiContext) => unknown) => void;
};

// ---- Contract #1 shapes ---------------------------------------------------

type Status = "ok" | "error" | "cancelled" | "unknown";
type ToolKind = "bash" | "mcp" | "file_op" | "subagent" | "web" | "other";
type Locus = "local" | "remote" | "hybrid" | "unknown";

type Fact = {
  schema_version: string;
  event_id: string;
  ts: string;
  collector: { name: string; version: string };
  session_id: string;
  type: "session_meta" | "llm_call" | "action_span";
  attribution?: { tool_call_id?: string };
  payload: Record<string, unknown>;
};

// ---- Pure mapping helpers (exported for tests) ----------------------------

/** Same rule as the Claude Code hook: keep [A-Za-z0-9._-], never start with a dot. */
export function sanitizeId(raw: string): string {
  const clean = raw.replace(/[^A-Za-z0-9._-]/g, "");
  return clean === "" || clean.startsWith(".") ? `x${clean}` : clean;
}

/** Mirrors af's own resolution in crates/af-cli/src/paths.rs. */
export function stateDir(env: NodeJS.ProcessEnv = process.env): string {
  if (env.AF_STATE_DIR) return env.AF_STATE_DIR;
  if (process.platform === "win32") {
    return join(env.LOCALAPPDATA ?? homedir(), "agentic-footprint");
  }
  return join(env.HOME ?? homedir(), ".local", "state", "agentic-footprint");
}

/** Same values as Rust's std::env::consts::OS, which `af hook` reports. */
export function osName(platform: string = process.platform): string {
  if (platform === "darwin") return "macos";
  if (platform === "win32") return "windows";
  return platform;
}

export function llmStatus(stopReason: string | undefined): Status {
  switch (stopReason) {
    case "stop":
    case "length":
    case "toolUse":
      return "ok";
    case "error":
      return "error";
    case "aborted":
      return "cancelled";
    default:
      return "unknown";
  }
}

/** Pi's built-in tools are read, bash, edit, write, grep, find, ls. */
export function toolKind(name: string): ToolKind {
  const n = name.toLowerCase();
  if (n === "bash" || n === "shell") return "bash";
  if (["read", "edit", "write", "grep", "find", "ls", "patch"].includes(n)) return "file_op";
  if (n === "task" || n === "agent" || n === "subagent") return "subagent";
  if (n.startsWith("mcp")) return "mcp";
  if (n.includes("fetch") || n.includes("search")) return "web";
  return "other";
}

export function executionLocus(kind: ToolKind): Locus {
  switch (kind) {
    case "bash":
    case "file_op":
    case "subagent":
      return "local";
    case "web":
      return "remote";
    default:
      return "unknown";
  }
}

/** Keeps only non-negative integer token counts; the schema rejects anything else. */
function tokens(value: unknown): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : undefined;
}

export function mapUsage(usage: Usage | undefined): Record<string, number> {
  const out: Record<string, number> = {};
  const pairs: [keyof Usage, string][] = [
    ["input", "input_tokens"],
    ["output", "output_tokens"],
    ["reasoning", "thought_tokens"],
    ["cacheRead", "cached_read_tokens"],
    ["cacheWrite", "cached_write_tokens"],
  ];
  for (const [from, to] of pairs) {
    const v = tokens(usage?.[from]);
    if (v !== undefined) out[to] = v;
  }
  return out;
}

// ---- The collector --------------------------------------------------------

type OpenSpan = { toolName: string; tStart: string };

export type CollectorOptions = {
  /** Clock, injectable for deterministic tests. */
  now?: () => Date;
  env?: NodeJS.ProcessEnv;
};

export function createCollector(options: CollectorOptions = {}) {
  const now = options.now ?? (() => new Date());
  const env = options.env ?? process.env;

  // Per-session state. A Pi process can switch sessions (/new, /resume,
  // /fork); each switch fires session_shutdown then session_start.
  const openSpans = new Map<string, Map<string, OpenSpan>>();
  const requestStartedAt = new Map<string, number>();

  function sessionOf(ctx: PiContext): string | undefined {
    const raw = ctx?.sessionManager?.getSessionId?.();
    return typeof raw === "string" && raw !== "" ? sanitizeId(raw) : undefined;
  }

  function append(fact: Fact): void {
    const dir = join(stateDir(env), "spool");
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    // One appendFileSync call per line: O_APPEND keeps each line whole.
    appendFileSync(join(dir, `${COLLECTOR.name}.${fact.session_id}.jsonl`), `${JSON.stringify(fact)}\n`, {
      encoding: "utf8",
      mode: 0o600,
    });
  }

  function logError(where: string, error: unknown): void {
    try {
      const dir = join(stateDir(env), "tmp");
      mkdirSync(dir, { recursive: true, mode: 0o700 });
      const message = error instanceof Error ? error.message : String(error);
      appendFileSync(join(dir, "pi-collector-errors.log"), `${now().toISOString()} ${where}: ${message}\n`, {
        mode: 0o600,
      });
    } catch {
      // Nowhere left to report; never let it reach Pi.
    }
  }

  function fact(type: Fact["type"], sessionId: string, eventId: string, payload: Record<string, unknown>): Fact {
    return {
      schema_version: SCHEMA_VERSION,
      event_id: eventId,
      ts: now().toISOString(),
      collector: { name: COLLECTOR.name, version: COLLECTOR.version },
      session_id: sessionId,
      type,
      payload,
    };
  }

  function closeSpan(sessionId: string, toolCallId: string, open: OpenSpan, status: Status): void {
    const span = fact("action_span", sessionId, `pi-span-${sessionId}-${sanitizeId(toolCallId)}`, {
      span_id: toolCallId,
      tool_name: open.toolName,
      tool_kind: toolKind(open.toolName),
      execution_locus: executionLocus(toolKind(open.toolName)),
      t_start: open.tStart,
      t_end: now().toISOString(),
      status,
    });
    span.attribution = { tool_call_id: toolCallId };
    append(span);
  }

  const handlers: Record<string, (event: PiEvent, ctx: PiContext) => void> = {
    session_start(_event, ctx) {
      const sid = sessionOf(ctx);
      if (!sid) return;
      openSpans.set(sid, openSpans.get(sid) ?? new Map());
      // Stable ID: a resumed session re-fires session_start, and the store
      // ignores an event_id it already holds, so this is counted once.
      append(fact("session_meta", sid, `pi-session-${sid}`, { agent_app: { name: "pi" }, os: osName() }));
    },

    before_provider_request(_event, ctx) {
      const sid = sessionOf(ctx);
      if (sid) requestStartedAt.set(sid, now().getTime());
    },

    message_end(event, ctx) {
      const message = event.message as AssistantMessage | undefined;
      if (!message || message.role !== "assistant") return;
      const sid = sessionOf(ctx);
      if (!sid) return;
      // Contract #1 requires both; a message without them is not a model call
      // we can attribute (same rule as the OpenCode collector).
      if (typeof message.provider !== "string" || typeof message.model !== "string") return;

      const started = requestStartedAt.get(sid);
      requestStartedAt.delete(sid);
      const key = message.responseId ?? `${message.timestamp ?? now().getTime()}`;

      const payload: Record<string, unknown> = {
        provider: message.provider,
        model_id_requested: message.model,
        usage: mapUsage(message.usage),
        usage_source: "agent_telemetry",
        status: llmStatus(message.stopReason),
      };
      if (message.responseModel && message.responseModel !== message.model) {
        payload.model_id_served = message.responseModel;
      }
      if (typeof message.api === "string") payload.endpoint = message.api;
      if (started !== undefined) payload.duration_ms = Math.max(0, now().getTime() - started);

      append(fact("llm_call", sid, `pi-llm-${sid}-${sanitizeId(key)}`, payload));
    },

    tool_execution_start(event, ctx) {
      const sid = sessionOf(ctx);
      const id = event.toolCallId;
      const name = event.toolName;
      if (!sid || typeof id !== "string" || typeof name !== "string") return;
      const spans = openSpans.get(sid) ?? new Map<string, OpenSpan>();
      openSpans.set(sid, spans);
      spans.set(id, { toolName: name, tStart: now().toISOString() });
    },

    tool_execution_end(event, ctx) {
      const sid = sessionOf(ctx);
      const id = event.toolCallId;
      if (!sid || typeof id !== "string") return;
      const spans = openSpans.get(sid);
      const open = spans?.get(id);
      if (!open) return; // never saw the start: no honest t_start to report
      spans!.delete(id);
      closeSpan(sid, id, open, event.isError === true ? "error" : "ok");
    },

    session_shutdown(_event, ctx) {
      const sid = sessionOf(ctx);
      if (!sid) return;
      for (const [id, open] of openSpans.get(sid) ?? []) closeSpan(sid, id, open, "cancelled");
      openSpans.delete(sid);
      requestStartedAt.delete(sid);
    },
  };

  /** Registers every handler on Pi, each wrapped so it can never throw into Pi. */
  function register(pi: PiApi): void {
    for (const [name, handler] of Object.entries(handlers)) {
      pi.on(name, (event, ctx) => {
        try {
          handler(event ?? {}, ctx ?? {});
        } catch (error) {
          logError(name, error);
        }
        return undefined; // never modify Pi's events
      });
    }
  }

  return { register };
}

/** Pi's extension entry point. */
export default function afCollector(pi: PiApi): void {
  createCollector().register(pi);
}
