import { defineConfig, type Plugin, type ProxyOptions } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { configDefaults } from "vitest/config";
import { stripRemoteImports } from "./dev/strip-remote-imports";

// Local-first debug console: dev server binds to localhost only, and no
// plugin here may reach out to the network at dev or build time.
//
// Two modes, selected by `--mode real` (npm run dev:real / preview:real;
// plain `npm run dev` / `npm run preview` stay on the default "development"
// mode, unchanged):
//
//   default (mock): mockDebugServer() stands in for the real Rust af
//   `/debug/*` endpoints, wired into both the dev and preview servers.
//
//   real: `/debug/*` is proxied to a real `af watch --debug` process
//   instead (AF_DEBUG_TARGET env var, default http://127.0.0.1:9414) and
//   the mock plugin is not loaded AT ALL — not just unattached. Importing
//   ./dev/mock-plugin runs its own top-level `setInterval` broadcast loop
//   as a side effect of the module loading, so "disabled" has to mean
//   "never imported", not "imported but inert", or that loop would still
//   tick forever in real mode.
//
// Nothing under console/dev/ is ever bundled into console/src/.
export default defineConfig(async ({ mode }) => {
  const isReal = mode === "real";
  const debugTarget = process.env.AF_DEBUG_TARGET ?? "http://127.0.0.1:9414";

  const plugins: Plugin[] = [stripRemoteImports()];
  if (!isReal) {
    const { mockDebugServer } = await import("./dev/mock-plugin");
    plugins.push(mockDebugServer());
  }
  plugins.push(svelte());

  // SSE (`GET /debug/stream`) must reach the browser frame-by-frame, not
  // buffered — the real server flushes each frame by hand
  // (`Request::into_writer`, per docs/design-log.md) specifically so a
  // chunked-encoding buffer doesn't sit on it for minutes; a proxy that
  // re-introduced buffering on this leg would silently undo that. `ws:
  // false` because `/debug/stream` is plain HTTP SSE, never a WebSocket
  // upgrade — only EventSource's plain GET is proxied here.
  const debugProxy: Record<string, ProxyOptions> = {
    "/debug": {
      target: debugTarget,
      changeOrigin: true,
      ws: false,
      configure(proxy) {
        proxy.on("proxyReq", (proxyReq) => {
          proxyReq.setHeader("Connection", "keep-alive");
        });
      },
    },
  };

  return {
    plugins,
    server: {
      host: "127.0.0.1",
      proxy: isReal ? debugProxy : undefined,
    },
    preview: {
      host: "127.0.0.1",
      proxy: isReal ? debugProxy : undefined,
    },
    // Vitest-only: resolve "svelte" to its client build so a test can
    // `mount()`/`unmount()` a real component (Task 5's hidden-tab-discipline
    // test). Without the "browser" condition, "svelte"'s package.json
    // exports map serves the server build, whose `mount()` throws
    // "not available on the server". Scoped to `process.env.VITEST` so
    // `npm run dev`/`build`/`preview` are entirely unaffected.
    resolve: {
      conditions: process.env.VITEST ? ["browser"] : undefined,
    },
    // `console/e2e/**` is a Playwright suite (its own `test`/`expect` from
    // `@playwright/test`, run via `npm run e2e`/`playwright.config.ts`) —
    // vitest's default `**/*.spec.ts` include would otherwise also try to
    // collect it as a unit test file and fail on Playwright's own
    // "test.describe.configure() called outside a config file" guard.
    test: {
      exclude: [...configDefaults.exclude, "e2e/**"],
    },
  };
});
