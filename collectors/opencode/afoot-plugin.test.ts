// Unit tests for the OpenCode plugin collector.
//   node --test collectors/opencode/afoot-plugin.test.ts
// Events are shaped like OpenCode 1.19.x's plugin bus (`event` hook).

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { AgenticFootprint } from "./afoot-plugin.ts";

async function fakeOpenCode() {
  const stateDir = mkdtempSync(join(tmpdir(), "afoot-oc-"));
  process.env.AFOOT_STATE_DIR = stateDir;
  const hooks = await AgenticFootprint({ directory: "/work/project" } as any);
  return {
    stateDir,
    send: (type: string, properties: Record<string, unknown>) => hooks.event({ event: { type, properties } }),
    facts(session = "ses_1"): Record<string, any>[] {
      const path = join(stateDir, "spool", `opencode.${session}.jsonl`);
      if (!existsSync(path)) return [];
      return readFileSync(path, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l));
    },
  };
}

const assistant = (id = "msg_a1", extra: Record<string, unknown> = {}) => ({
  info: { id, sessionID: "ses_1", role: "assistant", providerID: "anthropic", modelID: "claude-sonnet-4-5", mode: "build", time: { created: 1 }, ...extra },
});
const stepStart = (id: string, messageID = "msg_a1") => ({ part: { id, sessionID: "ses_1", messageID, type: "step-start" } });
const stepFinish = (id: string, messageID = "msg_a1", tokens = { input: 100, output: 20, reasoning: 5, cache: { read: 40, write: 3 } }) => ({
  part: { id, sessionID: "ses_1", messageID, type: "step-finish", reason: "tool-calls", cost: 0, tokens },
});
const tool = (id: string, callID: string, toolName: string, state: Record<string, unknown>) => ({
  part: { id, sessionID: "ses_1", messageID: "msg_a1", type: "tool", callID, tool: toolName, state },
});

test("session.created writes one session_meta for OpenCode", async () => {
  const oc = await fakeOpenCode();
  await oc.send("session.created", { info: { id: "ses_1" } });
  const [meta] = oc.facts();
  assert.equal(meta.type, "session_meta");
  assert.equal(meta.collector.name, "opencode");
  assert.equal(meta.payload.agent_app.name, "opencode");
  assert.ok(meta.event_id.length >= 16);
});

test("each finished step of an assistant message is one llm_call with that step's tokens", async () => {
  const oc = await fakeOpenCode();
  await oc.send("message.updated", assistant());
  await oc.send("message.part.updated", stepStart("prt_s1"));
  await oc.send("message.part.updated", stepFinish("prt_f1"));
  await oc.send("message.part.updated", stepStart("prt_s2"));
  await oc.send("message.part.updated", stepFinish("prt_f2", "msg_a1", { input: 300, output: 7, reasoning: 0, cache: { read: 0, write: 0 } }));
  const calls = oc.facts().filter((f) => f.type === "llm_call");
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[0].payload.usage, {
    input_tokens: 100, output_tokens: 20, thought_tokens: 5, cached_read_tokens: 40, cached_write_tokens: 3,
  });
  assert.equal(calls[0].payload.provider, "anthropic");
  assert.equal(calls[0].payload.model_id_requested, "claude-sonnet-4-5");
  assert.equal(calls[0].payload.usage_source, "agent_telemetry");
  assert.equal(calls[0].payload.status, "ok");
  assert.ok(calls[0].payload.duration_ms >= 0);
  assert.deepEqual(calls[0].attribution, { agent_id: "build", task_id: "msg_a1" });
  assert.equal(calls[1].payload.usage.input_tokens, 300);
});

test("a step-finish repeated by OpenCode is written once", async () => {
  const oc = await fakeOpenCode();
  await oc.send("message.updated", assistant());
  await oc.send("message.part.updated", stepFinish("prt_f1"));
  await oc.send("message.part.updated", stepFinish("prt_f1"));
  assert.equal(oc.facts().filter((f) => f.type === "llm_call").length, 1);
});

test("a step whose message never named a provider and model is skipped", async () => {
  const oc = await fakeOpenCode();
  await oc.send("message.part.updated", stepFinish("prt_f1", "msg_unknown"));
  assert.deepEqual(oc.facts().filter((f) => f.type === "llm_call"), []);
});

test("a tool part becomes one action_span when it completes, with OpenCode's own times", async () => {
  const oc = await fakeOpenCode();
  await oc.send("message.part.updated", tool("prt_t1", "call_1", "bash", { status: "pending", input: {}, raw: "" }));
  await oc.send("message.part.updated", tool("prt_t1", "call_1", "bash", { status: "running", input: { command: "ls" }, time: { start: 1_000 } }));
  await oc.send("message.part.updated", tool("prt_t1", "call_1", "bash", { status: "completed", input: { command: "ls" }, output: "secret", title: "ls", metadata: {}, time: { start: 1_000, end: 3_500 } }));
  await oc.send("message.part.updated", tool("prt_t1", "call_1", "bash", { status: "completed", input: {}, output: "", title: "", metadata: {}, time: { start: 1_000, end: 3_500 } }));
  const spans = oc.facts().filter((f) => f.type === "action_span");
  assert.equal(spans.length, 1);
  assert.deepEqual(
    [spans[0].payload.span_id, spans[0].payload.tool_name, spans[0].payload.tool_kind, spans[0].payload.execution_locus, spans[0].payload.status],
    ["call_1", "bash", "bash", "local", "ok"],
  );
  assert.equal(spans[0].payload.t_start, new Date(1_000).toISOString());
  assert.equal(spans[0].payload.t_end, new Date(3_500).toISOString());
  assert.deepEqual(spans[0].attribution, { tool_call_id: "call_1" });
});

test("a failed tool is an error span and an aborted one is cancelled", async () => {
  const oc = await fakeOpenCode();
  await oc.send("message.part.updated", tool("prt_t1", "call_1", "read", { status: "error", input: {}, error: "file not found", time: { start: 1, end: 2 } }));
  await oc.send("message.part.updated", tool("prt_t2", "call_2", "bash", { status: "error", input: {}, error: "Tool execution aborted", time: { start: 1, end: 2 } }));
  assert.deepEqual(oc.facts().filter((f) => f.type === "action_span").map((s) => s.payload.status), ["error", "cancelled"]);
});

test("no prompt, tool input, output or error text reaches the spool", async () => {
  const oc = await fakeOpenCode();
  await oc.send("session.created", { info: { id: "ses_1", title: "PRIVATE-TITLE" } });
  await oc.send("message.updated", assistant("msg_a1", { path: { cwd: "/PRIVATE-CWD", root: "/" } }));
  await oc.send("message.part.updated", stepFinish("prt_f1"));
  await oc.send("message.part.updated", tool("prt_t1", "call_1", "bash", { status: "error", input: { command: "PRIVATE-CMD" }, error: "PRIVATE-ERR", time: { start: 1, end: 2 } }));
  const spool = readFileSync(join(oc.stateDir, "spool", "opencode.ses_1.jsonl"), "utf8");
  for (const secret of ["PRIVATE-TITLE", "PRIVATE-CWD", "PRIVATE-CMD", "PRIVATE-ERR"]) assert.ok(!spool.includes(secret), secret);
});

test("malformed events and an unwritable state dir never throw into OpenCode", async () => {
  const oc = await fakeOpenCode();
  await oc.send("message.part.updated", {});
  await oc.send("message.updated", { info: null });
  process.env.AFOOT_STATE_DIR = "/dev/null/not-a-dir";
  await oc.send("session.created", { info: { id: "ses_1" } });
});
