// Unit tests for the Pi collector. Node's built-in runner, no dependencies:
//   node --test collectors/pi/afoot-collector.test.ts
// (Node >= 23.6 runs TypeScript directly by stripping the types.)

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  createCollector,
  llmStatus,
  mapUsage,
  osName,
  sanitizeId,
  toolKind,
  type PiContext,
  type PiEvent,
} from "./afoot-collector.ts";

/** A fake Pi: records handlers, lets the test fire events, reads the spool back. */
function fakePi(sessionId = "ses-1") {
  const stateDir = mkdtempSync(join(tmpdir(), "afoot-pi-"));
  const handlers = new Map<string, (event: PiEvent, ctx: PiContext) => unknown>();
  let clock = Date.parse("2026-11-17T12:00:00.000Z");
  createCollector({ now: () => new Date(clock), env: { AFOOT_STATE_DIR: stateDir } }).register({
    on: (name, handler) => handlers.set(name, handler),
  });
  let sid = sessionId;
  const ctx: PiContext = { sessionManager: { getSessionId: () => sid } };
  return {
    stateDir,
    fire(name: string, payload: PiEvent = {}, advanceMs = 1000) {
      clock += advanceMs;
      return handlers.get(name)?.(payload, ctx);
    },
    switchSession(next: string) {
      sid = next;
    },
    has: (name: string) => handlers.has(name),
    facts(session = sid): Record<string, any>[] {
      const path = join(stateDir, "spool", `pi.${session}.jsonl`);
      if (!existsSync(path)) return [];
      return readFileSync(path, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l));
    },
  };
}

const assistant = (extra: Record<string, unknown> = {}) => ({
  message: {
    role: "assistant",
    provider: "anthropic",
    model: "claude-sonnet-4-5",
    responseId: "msg_0123456789",
    stopReason: "stop",
    usage: { input: 10, output: 5 },
    ...extra,
  },
});

test("an assistant message_end becomes exactly one llm_call; turn_end and agent_end add nothing", () => {
  const pi = fakePi();
  pi.fire("session_start");
  pi.fire("message_end", assistant());
  pi.fire("turn_end", assistant());
  pi.fire("agent_end", { messages: [assistant().message] });
  const calls = pi.facts().filter((f) => f.type === "llm_call");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].payload.model_id_requested, "claude-sonnet-4-5");
  assert.deepEqual(calls[0].payload.usage, { input_tokens: 10, output_tokens: 5 });
  assert.equal(pi.has("turn_end"), false);
  assert.equal(pi.has("agent_end"), false);
});

test("user and toolResult messages are not model calls", () => {
  const pi = fakePi();
  pi.fire("message_end", { message: { role: "user" } });
  pi.fire("message_end", { message: { role: "toolResult", toolCallId: "t" } });
  assert.equal(pi.facts().length, 0);
});

test("duration_ms runs from before_provider_request to message_end", () => {
  const pi = fakePi();
  pi.fire("before_provider_request");
  pi.fire("message_end", assistant(), 2500);
  assert.equal(pi.facts()[0].payload.duration_ms, 2500);
});

test("responseModel is reported as model_id_served only when it differs", () => {
  const pi = fakePi();
  pi.fire("message_end", assistant({ responseId: "a1", responseModel: "claude-sonnet-4-5" }));
  pi.fire("message_end", assistant({ responseId: "a2", responseModel: "claude-sonnet-4-5-20250929" }));
  const [same, differs] = pi.facts();
  assert.equal(same.payload.model_id_served, undefined);
  assert.equal(differs.payload.model_id_served, "claude-sonnet-4-5-20250929");
});

test("a message without provider or model is skipped, not spooled", () => {
  const pi = fakePi();
  pi.fire("message_end", { message: { role: "assistant", stopReason: "error" } });
  assert.equal(pi.facts().length, 0);
});

test("parallel tools are paired by toolCallId even when they finish out of order", () => {
  const pi = fakePi();
  pi.fire("tool_execution_start", { toolCallId: "a", toolName: "bash" });
  pi.fire("tool_execution_start", { toolCallId: "b", toolName: "read" });
  pi.fire("tool_execution_end", { toolCallId: "b", isError: false });
  pi.fire("tool_execution_end", { toolCallId: "a", isError: true });
  const spans = pi.facts();
  assert.deepEqual(
    spans.map((s) => [s.payload.span_id, s.payload.tool_name, s.payload.tool_kind, s.payload.status]),
    [
      ["b", "read", "file_op", "ok"],
      ["a", "bash", "bash", "error"],
    ],
  );
  const a = spans[1].payload;
  assert.equal(a.t_start, "2026-11-17T12:00:01.000Z");
  assert.equal(a.t_end, "2026-11-17T12:00:04.000Z");
  assert.deepEqual(spans[1].attribution, { tool_call_id: "a" });
});

test("an end without a start is ignored rather than invented", () => {
  const pi = fakePi();
  pi.fire("tool_execution_end", { toolCallId: "ghost", isError: false });
  assert.equal(pi.facts().length, 0);
});

test("session_shutdown closes still-open spans as cancelled", () => {
  const pi = fakePi();
  pi.fire("tool_execution_start", { toolCallId: "long", toolName: "bash" });
  pi.fire("session_shutdown");
  const [span] = pi.facts();
  assert.equal(span.payload.status, "cancelled");
  pi.fire("session_shutdown");
  assert.equal(pi.facts().length, 1, "a second shutdown must not close it again");
});

test("a resumed session re-emits session_meta with the same event_id (the store keeps one)", () => {
  const pi = fakePi();
  pi.fire("session_start", { reason: "startup" });
  pi.fire("session_start", { reason: "resume" });
  const metas = pi.facts();
  assert.equal(metas.length, 2);
  assert.equal(metas[0].event_id, metas[1].event_id);
  assert.deepEqual(metas[0].payload.agent_app, { name: "pi" });
});

test("switching session writes to the new session's spool file", () => {
  const pi = fakePi("first");
  pi.fire("session_start");
  pi.switchSession("second");
  pi.fire("session_start");
  assert.equal(pi.facts("first").length, 1);
  assert.equal(pi.facts("second").length, 1);
});

test("a handler failure is logged and never thrown into Pi", () => {
  const pi = fakePi();
  // A getter that throws stands in for any unexpected shape from Pi.
  const hostile = {
    get message() {
      throw new Error("boom");
    },
  };
  assert.doesNotThrow(() => pi.fire("message_end", hostile as unknown as PiEvent));
  const log = readFileSync(join(pi.stateDir, "tmp", "pi-collector-errors.log"), "utf8");
  assert.match(log, /message_end: boom/);
});

test("handlers return undefined so they never rewrite Pi's messages", () => {
  const pi = fakePi();
  assert.equal(pi.fire("message_end", assistant()), undefined);
});

test("mapping helpers", () => {
  assert.equal(sanitizeId("ses/../x y"), "ses..xy");
  assert.equal(sanitizeId(".hidden"), "x.hidden");
  assert.equal(sanitizeId(""), "x");
  assert.equal(llmStatus("toolUse"), "ok");
  assert.equal(llmStatus("aborted"), "cancelled");
  assert.equal(llmStatus("weird"), "unknown");
  assert.equal(toolKind("grep"), "file_op");
  assert.equal(toolKind("mcp__github"), "mcp");
  assert.equal(toolKind("my_custom_tool"), "other");
  assert.equal(osName("darwin"), "macos");
  assert.deepEqual(mapUsage({ input: 3, output: -1, cacheRead: 1.5, reasoning: 2 }), {
    input_tokens: 3,
    thought_tokens: 2,
  });
});

test("two model calls sharing a provider responseId stay two llm_calls (Ollama reuses chatcmpl-N)", () => {
  const pi = fakePi();
  pi.fire("message_end", assistant({ responseId: "chatcmpl-123", timestamp: 1_000 }));
  pi.fire("message_end", assistant({ responseId: "chatcmpl-123", timestamp: 2_000 }));
  const ids = pi.facts().filter((f) => f.type === "llm_call").map((f) => f.event_id);
  assert.equal(ids.length, 2);
  assert.equal(new Set(ids).size, 2);
});

test("an llm_call event_id is stable for the same message (a replay deduplicates)", () => {
  const first = fakePi();
  first.fire("message_end", assistant({ responseId: "chatcmpl-7", timestamp: 1_000 }));
  const second = fakePi();
  second.fire("message_end", assistant({ responseId: "chatcmpl-7", timestamp: 1_000 }), 5_000);
  assert.equal(first.facts()[0].event_id, second.facts()[0].event_id);
});

test("a tool call ID reused in a later turn stays two action_spans", () => {
  const pi = fakePi();
  pi.fire("tool_execution_start", { toolCallId: "call_0", toolName: "bash" });
  pi.fire("tool_execution_end", { toolCallId: "call_0", isError: false });
  pi.fire("tool_execution_start", { toolCallId: "call_0", toolName: "read" });
  pi.fire("tool_execution_end", { toolCallId: "call_0", isError: false });
  const spans = pi.facts().filter((f) => f.type === "action_span");
  assert.deepEqual(spans.map((s) => s.payload.tool_name), ["bash", "read"]);
  assert.equal(new Set(spans.map((s) => s.event_id)).size, 2);
});

test("a second start with an open tool call ID closes the first as unknown instead of overwriting it", () => {
  const pi = fakePi();
  pi.fire("tool_execution_start", { toolCallId: "", toolName: "bash" });
  pi.fire("tool_execution_start", { toolCallId: "", toolName: "read" });
  pi.fire("tool_execution_end", { toolCallId: "", isError: false });
  const spans = pi.facts().filter((f) => f.type === "action_span");
  assert.deepEqual(spans.map((s) => [s.payload.tool_name, s.payload.status]), [["bash", "unknown"], ["read", "ok"]]);
});

test("a short session ID still gives a session_meta event_id of at least 16 characters", () => {
  const pi = fakePi("a");
  pi.fire("session_start");
  assert.ok(pi.facts()[0].event_id.length >= 16);
});
