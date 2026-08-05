import { defineConfig, devices } from "@playwright/test";

// e2e smoke suite (fix-wave finding #1 — the plan promised this and it
// never shipped). Chromium only, against `vite preview` serving the
// production build with the SAME mockDebugServer() `/debug/*` mock the dev
// server uses: vite.config.ts's `configurePreviewServer` hook attaches it
// too, in default (mock) mode — `preview`/`e2e` never pass `--mode real`.
//
// webServer deliberately does NOT build here. `console/dist` is expected to
// already exist: `npm run e2e` (package.json) builds it itself for a
// one-command local workflow, while CI's `console-e2e` job (ci.yml) reuses
// the `console` job's already-built `dist` artifact instead of paying for a
// second build — that's the whole point of splitting build-then-preview
// instead of folding `npm run build` into this file's `command`.
const PORT = 4173;

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: "list",
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    actionTimeout: 15_000,
    trace: "retain-on-failure",
  },
  webServer: {
    // Fixed port + strictPort: fail fast on a port collision rather than
    // vite silently picking a different one Playwright wasn't told about.
    command: `npm run preview -- --port ${PORT} --strictPort`,
    url: `http://127.0.0.1:${PORT}`,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
