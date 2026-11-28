// Replays a recorded/synthetic Pi event sequence through the collector, the
// way Pi itself would call the extension's handlers.
//
//   node collectors/pi/test-data/replay.ts <fixture.jsonl> <session-id>
//
// Each fixture line is {"event": <pi event name>, "payload": <event object>}.
// The clock advances one second per fixture line, so output is deterministic.
// Writes to $AFOOT_STATE_DIR/spool/pi.<session-id>.jsonl.

import { readFileSync } from "node:fs";
import { createCollector, type PiContext, type PiEvent } from "../afoot-collector.ts";

const [fixture, sessionId] = process.argv.slice(2);
if (!fixture || !sessionId) {
  console.error("usage: replay.ts <fixture.jsonl> <session-id>");
  process.exit(2);
}

let clock = Date.parse("2026-11-17T12:00:00.000Z");
const handlers = new Map<string, (event: PiEvent, ctx: PiContext) => unknown>();
createCollector({ now: () => new Date(clock) }).register({
  on: (name, handler) => handlers.set(name, handler),
});

const ctx: PiContext = { sessionManager: { getSessionId: () => sessionId } };
for (const line of readFileSync(fixture, "utf8").split("\n")) {
  if (line.trim() === "") continue;
  const { event, payload } = JSON.parse(line) as { event: string; payload: PiEvent };
  clock += 1000;
  handlers.get(event)?.(payload, ctx); // events the collector doesn't listen to are skipped, as in Pi
}
