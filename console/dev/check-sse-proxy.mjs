#!/usr/bin/env node
// Manual timing harness: does vite.config.ts's `/debug` proxy (`ws: false`,
// a `configure()` hook that forces `Connection: keep-alive`) add material
// first-frame latency to `GET /debug/stream` versus talking to the real
// `af watch --debug` process directly? README.md's "Running against the
// real server" section already claims the proxy was "verified end-to-end...
// to not introduce buffering" — this script is that verification, committed
// so the check can be re-run by hand instead of re-derived from scratch.
//
// Usage (from console/):
//   1. Start a real af watch --debug process (README.md's "Running against
//      the real server" section has the exact command).
//   2. Point the dev server at it: AF_DEBUG_TARGET=http://127.0.0.1:9414
//      npm run dev:real (or preview:real).
//   3. node dev/check-sse-proxy.mjs [directUrl] [proxyUrl]
//      Defaults: directUrl = $AF_DEBUG_TARGET or http://127.0.0.1:9414,
//      proxyUrl = http://127.0.0.1:5173 (vite's default dev port).
//
// Connects to GET /debug/stream on both legs, measures wall-clock ms from
// request-sent to the first SSE `data:` frame received on each, and prints
// both plus the delta and a verdict. Deliberately built on node:http alone
// (no EventSource/extra deps) so it has nothing else to install. NOT wired
// into CI: it needs a live af process and a live vite dev server, neither
// available headless — this is a human-run diagnostic, not an automated gate.
import http from "node:http";
import { URL } from "node:url";

const directBase = process.argv[2] ?? process.env.AF_DEBUG_TARGET ?? "http://127.0.0.1:9414";
const proxyBase = process.argv[3] ?? "http://127.0.0.1:5173";

/** A few hundred ms of scheduling/networking noise is normal on localhost;
 * anything materially larger than this suggests the proxy leg is buffering
 * rather than forwarding each frame as the real server flushes it. */
const VERDICT_THRESHOLD_MS = 500;

/** Resolves with the elapsed ms from request-sent to the first chunk
 * containing an SSE `data:` line, or rejects with a descriptive error
 * (non-200 status, connection error, or a 10s timeout with no frame at all). */
function timeToFirstFrame(baseUrl) {
  return new Promise((resolve, reject) => {
    const url = new URL("/debug/stream", baseUrl);
    const startedAtMs = Date.now();
    const req = http.get(url, (res) => {
      if (res.statusCode !== 200) {
        reject(new Error(`${url} responded ${res.statusCode}`));
        req.destroy();
        return;
      }
      let sawFrame = false;
      res.on("data", (chunk) => {
        if (sawFrame) return;
        if (chunk.toString("utf8").includes("data:")) {
          sawFrame = true;
          const elapsedMs = Date.now() - startedAtMs;
          req.destroy();
          resolve(elapsedMs);
        }
      });
      res.on("end", () => {
        if (!sawFrame) reject(new Error(`${url} closed before any SSE frame arrived`));
      });
    });
    req.on("error", (err) => reject(new Error(`${url}: ${err.message}`)));
    req.setTimeout(10_000, () => {
      req.destroy();
      reject(new Error(`${url} timed out waiting for a first frame`));
    });
  });
}

async function main() {
  console.log(`direct:  ${new URL("/debug/stream", directBase)}`);
  console.log(`proxied: ${new URL("/debug/stream", proxyBase)}`);
  console.log("");

  const [directResult, proxyResult] = await Promise.allSettled([timeToFirstFrame(directBase), timeToFirstFrame(proxyBase)]);

  if (directResult.status === "rejected") console.error(`direct leg failed: ${directResult.reason.message}`);
  if (proxyResult.status === "rejected") console.error(`proxy leg failed: ${proxyResult.reason.message}`);

  if (directResult.status === "rejected" || proxyResult.status === "rejected") {
    console.log("\nverdict: INCONCLUSIVE — one or both legs failed (see errors above). Is af watch --debug and npm run dev:real/preview:real both actually running?");
    process.exitCode = 1;
    return;
  }

  const directMs = directResult.value;
  const proxyMs = proxyResult.value;
  const deltaMs = proxyMs - directMs;

  console.log(`direct first-frame:  ${directMs}ms`);
  console.log(`proxied first-frame: ${proxyMs}ms`);
  console.log(`delta:               ${deltaMs >= 0 ? "+" : ""}${deltaMs}ms`);
  console.log("");

  if (deltaMs > VERDICT_THRESHOLD_MS) {
    console.log(`verdict: SUSPECT — the proxied leg is ${deltaMs}ms slower than direct, beyond the ${VERDICT_THRESHOLD_MS}ms localhost noise floor. Check vite.config.ts's debugProxy configure() hook for re-introduced buffering.`);
    process.exitCode = 1;
  } else {
    console.log(`verdict: OK — the proxy adds no material first-frame latency (within ${VERDICT_THRESHOLD_MS}ms of direct).`);
  }
}

main();
