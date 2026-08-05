import { appendFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

type Message = {
  role?: string;
  provider?: string;
  model?: string;
  responseModel?: string;
  responseId?: string;
  stopReason?: string;
  timestamp?: number;
  usage?: {
    input?: number;
    output?: number;
    cacheRead?: number;
    cacheWrite?: number;
    cacheWrite1h?: number;
    reasoning?: number;
    totalTokens?: number;
    cost?: {
      input?: number;
      output?: number;
      cacheRead?: number;
      cacheWrite?: number;
      total?: number;
    };
  };
};

type Context = {
  mode?: string;
  cwd?: string;
  sessionManager?: {
    getSessionId?: () => string;
    getSessionFile?: () => string | undefined;
  };
};

type ExtensionApi = {
  on: (name: string, handler: (event: Record<string, unknown>, context: Context) => void) => void;
};

const output = process.env.AF_PI_CAPTURE;

function messageSummary(value: unknown): Json {
  if (!value || typeof value !== "object") return null;
  const message = value as Message;
  return {
    role: message.role ?? null,
    provider: message.provider ?? null,
    model: message.model ?? null,
    response_model: message.responseModel ?? null,
    response_id: message.responseId ?? null,
    stop_reason: message.stopReason ?? null,
    timestamp_ms: message.timestamp ?? null,
    usage: message.usage
      ? {
          input: message.usage.input ?? null,
          output: message.usage.output ?? null,
          cache_read: message.usage.cacheRead ?? null,
          cache_write: message.usage.cacheWrite ?? null,
          cache_write_1h: message.usage.cacheWrite1h ?? null,
          reasoning: message.usage.reasoning ?? null,
          total_tokens: message.usage.totalTokens ?? null,
          cost: message.usage.cost
            ? {
                input: message.usage.cost.input ?? null,
                output: message.usage.cost.output ?? null,
                cache_read: message.usage.cost.cacheRead ?? null,
                cache_write: message.usage.cost.cacheWrite ?? null,
                total: message.usage.cost.total ?? null,
              }
            : null,
        }
      : null,
  };
}

function eventSummary(name: string, event: Record<string, unknown>, context: Context): Json {
  const summary: Record<string, Json> = {
    observed_at: new Date().toISOString(),
    event: name,
    source_type: typeof event.type === "string" ? event.type : null,
    mode: context.mode ?? null,
    cwd: context.cwd ?? null,
    session_id: context.sessionManager?.getSessionId?.() ?? null,
    session_file: context.sessionManager?.getSessionFile?.() ?? null,
  };

  for (const key of ["reason", "turnIndex", "timestamp", "toolCallId", "toolName", "isError", "status"] as const) {
    const value = event[key];
    if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
      summary[key] = value;
    }
  }

  if ("message" in event) summary.message = messageSummary(event.message);
  if (Array.isArray(event.messages)) summary.messages = event.messages.map(messageSummary);
  if (Array.isArray(event.toolResults)) {
    summary.tool_results = event.toolResults.map((result) => {
      if (!result || typeof result !== "object") return null;
      const value = result as Record<string, unknown>;
      return {
        role: typeof value.role === "string" ? value.role : null,
        tool_call_id: typeof value.toolCallId === "string" ? value.toolCallId : null,
        tool_name: typeof value.toolName === "string" ? value.toolName : null,
        is_error: typeof value.isError === "boolean" ? value.isError : null,
        timestamp_ms: typeof value.timestamp === "number" ? value.timestamp : null,
      };
    });
  }

  return summary;
}

function write(record: Json): void {
  if (!output) return;
  mkdirSync(dirname(output), { recursive: true });
  appendFileSync(output, `${JSON.stringify(record)}\n`, { encoding: "utf8", mode: 0o600 });
}

export default function captureExtension(pi: ExtensionApi): void {
  const events = [
    "session_start",
    "session_shutdown",
    "before_agent_start",
    "agent_start",
    "agent_end",
    "agent_settled",
    "turn_start",
    "turn_end",
    "message_start",
    "message_end",
    "tool_execution_start",
    "tool_execution_end",
    "model_select",
    "tool_call",
    "tool_result",
  ];

  for (const name of events) {
    pi.on(name, (event, context) => write(eventSummary(name, event, context)));
  }
}
