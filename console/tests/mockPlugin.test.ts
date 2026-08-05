// Contract test for dev/mock-plugin.ts's `?level=` validation (Phase 2
// Package C item 1). handleReport is the mock's own executable contract for
// DATA-CONTRACT §2.6 (`GET /debug/report?level=session|task|tool`) — an
// invalid `level` must be refused with 400 + a JSON error body, never
// echoed back into a reshaped report or silently defaulted to "session"
// (dev-aid honesty: the mock server IS the contract every console/src/ Task
// is written against, so it must not lie about what a real server would do
// with a malformed request).
import { describe, expect, it } from "vitest";
import type { IncomingMessage, ServerResponse } from "node:http";
import { handleReport } from "../dev/mock-plugin";

/** A minimal stand-in for `http.ServerResponse` covering exactly the surface
 * `sendJson` (mock-plugin.ts) touches: `statusCode`, `setHeader`, `end`. */
function fakeRes(): { res: ServerResponse; statusCode: () => number; body: () => unknown; headers: () => Record<string, string> } {
  let statusCode = 200;
  let body = "";
  const headers: Record<string, string> = {};
  const res = {
    setHeader(key: string, value: string) {
      headers[key] = value;
    },
    end(chunk?: string) {
      if (chunk !== undefined) body += chunk;
    },
  } as unknown as ServerResponse;
  Object.defineProperty(res, "statusCode", {
    get: () => statusCode,
    set: (v: number) => {
      statusCode = v;
    },
  });
  return { res, statusCode: () => statusCode, body: () => JSON.parse(body), headers: () => headers };
}

function fakeReq(url: string): IncomingMessage {
  return { url } as IncomingMessage;
}

describe("handleReport — ?level= validation (dev-aid honesty)", () => {
  it.each(["session", "task", "tool"])("accepts level=%s with a 200 and a report shaped for that level", (level) => {
    const { res, statusCode, body } = fakeRes();
    handleReport(fakeReq(`/debug/report?level=${level}`), res);
    expect(statusCode()).toBe(200);
    expect((body() as { level: string }).level).toBe(level);
  });

  it("defaults to level=session when the param is entirely absent", () => {
    const { res, statusCode, body } = fakeRes();
    handleReport(fakeReq("/debug/report"), res);
    expect(statusCode()).toBe(200);
    expect((body() as { level: string }).level).toBe("session");
  });

  it("rejects an invalid ?level= with 400 and a JSON error body — never echoed garbage, never silently defaulted", () => {
    const { res, statusCode, headers, body } = fakeRes();
    handleReport(fakeReq("/debug/report?level=bogus"), res);
    expect(statusCode()).toBe(400);
    expect(headers()["Content-Type"]).toBe("application/json");
    const parsed = body() as { error: string; level: string; allowed: string[] };
    expect(parsed.error).toBeTruthy();
    expect(parsed.level).toBe("bogus");
    expect(new Set(parsed.allowed)).toEqual(new Set(["session", "task", "tool"]));
  });

  it("rejects a case-mismatched level (e.g. Session) — no fuzzy/ci matching, honest about what was actually sent", () => {
    const { res, statusCode, body } = fakeRes();
    handleReport(fakeReq("/debug/report?level=Session"), res);
    expect(statusCode()).toBe(400);
    expect((body() as { level: string }).level).toBe("Session");
  });
});
