# af debug console

A local-first web console for `af watch --debug`'s `/debug` HTTP+SSE surface
(`docs/design_handoff_af_debug_console/DATA-CONTRACT.md`). It streams the
control plane's live facts, decisions, allocation traces, watchdog state,
impact report and collector health straight from the debugging server —
never computing impacts, attribution, apportionment or grid conversions
itself (global-constraints.md #1: "the client computes nothing").

## Dev modes

The console runs against either of two `/debug` backends, selected at the
`vite` config level (`console/vite.config.ts`) via `--mode`:

| Command | Backend | Notes |
|---|---|---|
| `npm run dev` | `mockDebugServer()` (`console/dev/mock-plugin.ts`) | Default. A deterministic, contract-faithful replay of a rich fixture scenario (`console/dev/scenario.ts`) — no real `af` binary needed. |
| `npm run dev:real` | A real `af watch --debug` process | Proxies `/debug/*` to `AF_DEBUG_TARGET` (default `http://127.0.0.1:9414`, `af watch`'s own default `--debug-addr`). |
| `npm run preview` | mock | Same mock, served from the production build. |
| `npm run preview:real` | real | Same proxy, served from the production build. |

In real mode the mock plugin is not merely unattached — `vite.config.ts`
never even imports `console/dev/mock-plugin.ts`, because importing it runs
its own top-level `setInterval` broadcast loop as a side effect of module
load. "Disabled" means "never loaded."

### Running against the real server

```bash
# 1. Build the CLI (from the repo root):
cargo build -p af-cli

# 2. Start a resident af watch with the debug surface on, against whatever
#    AF_STATE_DIR you want the console to show (a live project, or a
#    tempdir seeded with fixture spool lines — see
#    crates/af-cli/tests/watch.rs for the pattern):
AF_STATE_DIR=/path/to/state ./target/debug/af watch --debug \
  --no-sidecars --no-otlp --debug-addr 127.0.0.1:9414

# 3. Point the console at it (from console/):
AF_DEBUG_TARGET=http://127.0.0.1:9414 npm run dev:real
```

CORS is wide open on the real server (`Access-Control-Allow-Origin: *`), so
the proxy isn't required for correctness — it exists so the console can use
plain relative `/debug/*` URLs in both modes alike (global-constraints.md
#4: "the client uses relative URLs only for /debug/* endpoints"), and so
`AF_DEBUG_TARGET` is the only thing that changes between a mock run and a
real one.

The SSE endpoint (`GET /debug/stream`) is proxied with `ws: false` (it's
plain HTTP SSE, never a WebSocket upgrade) and a `configure` hook that keeps
the proxied connection alive — verified end-to-end (see below) to not
introduce buffering beyond what the real server's own hand-flushed response
already accounts for. `node dev/check-sse-proxy.mjs` re-runs that
verification by hand (direct-vs-proxied first-frame latency + a verdict) —
see its own header comment for prerequisites; not wired into CI.

## Real-server behaviors v1

The real `af watch --debug` server (`crates/af-cli/src/cmd/{watch,debug_server,debug_frames}.rs`)
implements the same `/debug` contract as the mock, but deviates from the
mock's deliberately rich fixtures in specific, documented ways. All of
these are tolerated by the console's types (`console/src/lib/types/debug.ts`)
and stores — a null/absent value here is never rendered as a fabricated
number or a silent 0. Full rationale for each: `docs/design-log.md`, entry
"`af watch` resident mode, sampler lifecycle, and the `/debug` console
surface" (read this first for anything below that seems surprising).

- **`open_spans` is always `[]`.** The only span collector emits spans on
  close, so the control plane never observes one still open.
- **`GET /debug/report?level=` is ignored** — the real server always
  answers at session level, regardless of what was requested.
  `reportStore.fetchLevel(level)` caches its result under the level
  *requested*, not the payload's own (possibly different) `level` field,
  which is left exactly as the server reported it.
- **`health.conformance` is absent.** Gap #9's per-field presence counters
  were a design proposal, never confirmed; its absence must never render as
  an empty table (that would misreport "counted zero" as "not counted").
- **`CollectorHealth.events_per_s` is `null`.** A rate over a session's
  whole span isn't the rate anyone reads it as. Renders "—"
  (`fmtEventsPerS`).
- **`session.methodology.version`** may read literally
  `"unknown until the first estimate"`, with `ecologits_version` /
  `codecarbon_version` omitted entirely, until an estimate has run.
- **`session.grid.g_co2e_per_kwh` may be `null`** without an estimator
  sidecar to resolve a zone's electricity mix — a defaulted grid intensity
  is exactly the invented number the project forbids. `grid.source` says
  why. Renders `"n/a · {source}"` (`fmtGridIntensity`), never `0`.
- **`health.otlp_receiver.endpoint` may be `null`**, with an explanatory
  `note`, when no OTLP receiver is running in this `af watch` process (e.g.
  `--no-otlp`, or a reported-not-fatal bind failure) — found during this
  task's own e2e run, not originally in DATA-CONTRACT's own example.
- **Watchdog `cmd` is the owning span's `tool_name`, not a command line** —
  no collector reports argv.
- **Watchdog `orphaned`/`agent` states are currently unreachable** through
  `af watch` (the v1 sampler watch-list only watches the root tree once per
  session; the orphan-tail protocol is implemented and tested at the
  sampler level, but never triggered here).
- **Allocation traces carry extra `note` strings**: `denominator_note` on
  the trace, and `agent_process.note` explaining that `agent_process`
  doubles as the orphan bucket (`l2_cpu_time/v1` has no separate
  agent-process bucket). Both are optional — the mock's fixtures don't set
  them, and both shapes are valid.
- **The report's `estimation_status_histogram` may carry a sixth status,
  `missing_usage`** — a `llm_call` with no token count to estimate from
  never reaches the estimator, and is recorded directly. Added to the
  histogram only when it occurs (`Partial<Record<...>>`, not all-keys-required).

Mock fixtures (`console/dev/scenario.ts`, `console/dev/fixtures/*.json`)
stay rich and contract-faithful on purpose — none of the above is ever
reproduced there. Tolerance for all of it is instead proven by hand-built,
null-bearing unit fixtures pushed through the consuming code paths
(`console/tests/tolerance.test.ts`, plus the `fmtEventsPerS`/`fmtGridIntensity`
cases in `console/tests/format.test.ts`).

## End-to-end smoke tests

`console/e2e/console.spec.ts` (Playwright, Chromium only) drives a real
built-and-previewed console against the same deterministic mock scenario the
unit tests are proven against — tab navigation, SSE reaching "live", the
Timeline↔Stream click-path convergence, the coverage-gap band, the Health
tab's collector/conformance/quarantined panels, and pause/resume buffering.

Playwright's browser binary isn't an npm dependency — install it once per
machine:

```bash
npx playwright install chromium
```

Then, from `console/`:

```bash
npm run e2e   # builds, then runs the suite against `vite preview`
```

A few of the scenario's facts (the coverage gap, the first span to close)
only become visible once that much real wall-clock time has elapsed since
the mock server started — see `console/playwright.config.ts` and
`console/e2e/console.spec.ts`'s own header comments for why, and don't be
surprised if the run takes ~30s rather than finishing instantly. CI runs the
same suite in its own `console-e2e` job (`.github/workflows/ci.yml`),
installing Chromium there with `npx playwright install chromium --with-deps`
— that flag pulls in Linux system libraries `apt`-side and isn't needed (or
available) on a developer's own machine.

## Verifying against the real binary

`cargo build -p af-cli`, start a resident `af watch --debug` against a
tempdir `AF_STATE_DIR` (append Contract #1 spool lines from
`tests/fixtures/spool/basic-session/cc-hooks.sess-basic.jsonl`, or hand-roll
your own — see `crates/af-cli/tests/watch.rs`'s `append_lines` helper for
the exact spool file naming convention: `<collector>.<session_id>.jsonl`
under `$AF_STATE_DIR/spool/`), then `AF_DEBUG_TARGET=http://127.0.0.1:<port>
npm run dev:real`. Open the console, and expect: the masthead dot to reach
"live", the Stream tab's table and Inspector to populate from real facts
(sorted by `ts`, correlated rows working), new spool lines to appear within
about a second of being appended (the same `200ms` poll + `300ms` debounce
`af watch` documents), and zero console errors as `report`/`health`/`alloc`
frames — carrying the real server's nulls/notes above — flow through.

## Shipping inside `af` (embed/mount contract)

The built console is not served by Vite in production — it is compiled
into the `af` binary by `crates/af-console/`, a small Rust crate whose
whole job is `include_dir!`-embedding `console/dist/` and handing pages
back out:

```rust
pub struct StaticAsset { pub bytes: &'static [u8], pub content_type: &'static str, pub etag: &'static str }
pub fn asset(path: &str) -> Option<StaticAsset>;
pub fn is_placeholder() -> bool;
```

This is **implemented in `crates/af-cli/src/cmd/debug_server.rs`**: its
routing fallback (everything that doesn't match `/debug/*`) calls
`af_console::asset(path)` — a leading `/` and a trailing `?query` are both
fine, `asset` strips them itself, collapses repeated/empty path segments
(`//assets//x.js` resolves the same as `/assets/x.js`), and rejects `..`
path segments outright. `Some(StaticAsset)` is a 200: `bytes` with
`Content-Type: {content_type}`, `ETag: {etag}` and `Cache-Control:
no-cache`; a matching `If-None-Match` short-circuits to an empty `304`.
`None` is a 404 — the same shape every unmatched `/debug/*` path already
gets. There is **no SPA fallback to `index.html`** for unmatched paths —
every real route the console needs (`/`, `/index.html`, hashed
`/assets/*`) resolves directly, so an unmatched path is simply wrong, not
a client-side route to catch. `/debug/*` routing and CORS are unaffected
by the mount — it only ever answers on paths that route falls through on.

To run the console from inside the real binary:

```bash
npm --prefix console run build   # produces console/dist/
cargo build -p af-cli            # embeds it into af-console, links af-cli
af watch --debug                 # open http://127.0.0.1:9414/
```

If `console/dist/` is missing at `cargo build` time, `af-console`'s
`build.rs` falls back to a placeholder page (see "Build pipeline" below)
and `af watch --debug` logs a startup line to stderr — `debug console
embedded as placeholder — build console/ and rebuild to serve the UI` — so
a forgotten `npm run build` shows up immediately instead of silently
serving a near-empty page. The dev modes above (`npm run dev` /
`dev:real`) remain the fast iteration path; this section is for running
the console as it actually ships.

### Build pipeline

`console/dist/` must exist *before* `cargo build` compiles
`af-console`, because its `build.rs` only copies the directory — it never
invokes `npm`:

```bash
npm --prefix console run build   # produces console/dist/
cargo build                       # embeds it into af-console
```

Build the crate without that first step (a fresh checkout, a cargo-only
CI job, local iteration on the Rust side) and it still compiles: `build.rs`
falls back to a minimal placeholder `index.html` ("console not built — run
`npm --prefix console run build` and rebuild") and prints a
`cargo:warning` so the gap shows up in build output instead of silently
shipping stale or missing UI. `af_console::is_placeholder()` lets a caller
(or a test) tell which case is embedded at runtime.

### Review checklist

- Can you point at a joule the browser calculated? If yes, the port is
  not done (DATA-CONTRACT §0).
- Does every `/debug` field the console renders trace back to a real
  server response, with nulls/absences rendered honestly rather than
  defaulted to a fabricated number or a silent 0?
- Are relative `/debug/*` URLs the only URLs the client ever issues
  (global-constraints.md #4)?
- Does `asset()` 404 anything outside `console/dist/` — no traversal, no
  SPA fallback masking a real 404 as a 200?
- Is `cargo fmt --check` / `cargo clippy --workspace -- -D warnings` clean,
  and does `npm run check` / `npm test` / `npm run gen:types:check` /
  `npm run lint:hex` / `npm run lint:arith` / `npm run e2e` all pass?

## Interface notes for the control-plane team

Notes accumulated over the console build that affect the real `/debug`
server or its documentation, beyond what's already covered in "Real-server
behaviors v1" above:

- **`DebugReport` carries a `level` field** not shown in DATA-CONTRACT
  §2.6 (no jsonc example exists there). The console's `reportStore` caches
  a fetched report under the `level` it *requested*, not the payload's own
  `level`, which is left exactly as the server reported it — worth
  confirming the field is intentional and its values are stable.
- **Watchdog frame/snapshot shape asymmetry is deliberate**: the `/debug/stream`
  watchdog SSE frame's `data` is `{pids: WatchdogEntry[]}` (DATA-CONTRACT
  §2.3), while `Snapshot.watchdog` (§2.2) is a bare `WatchdogEntry[]` — same
  entries, different envelope depending on whether they arrive via the
  stream or a snapshot backfill.
- **`/debug/stream` must accept `?from=<seq>`** as a first-connection
  equivalent of `Last-Event-ID`, because `EventSource` cannot set custom
  headers on its initial request — only on reconnect, once the browser has
  seen at least one `id:` field. The real server already implements this.
- **`health.rejected` reuses `RejectFrame`**, whose `byte_offset` is
  mandatory, even though the §2.7 example elides it — the console does not
  treat a missing `byte_offset` as valid.
- **`health.otlp_receiver.endpoint` may be `null`**, with an explanatory
  `note`, when no OTLP receiver is running (e.g. `--no-otlp`, or a
  reported-not-fatal bind failure) — not in DATA-CONTRACT's own example,
  found during real-server e2e verification; the console's types are
  widened to tolerate it.
- **The real `/debug/health` carries extra fields beyond `HealthPayload`**
  (e.g. `rejected_total`) — harmless via structural typing (the console
  simply ignores fields it doesn't know about), but worth widening the
  console's `HealthPayload` type deliberately if any of those extra fields
  become something the UI should show.
- **`report` and `health` are not pushed on `/debug/stream` subscribe.**
  The console fetches both once at bootstrap (`GET /debug/report`,
  `GET /debug/health`) rather than waiting on the stream to deliver them —
  worth keeping in mind if a future stream-only client is ever built
  against this contract.

Full detail and rationale for the tolerances above (plus more real-server
deviations not specific to the backend interface, like `open_spans`
always `[]` or watchdog `cmd` being a tool name) live in "Real-server
behaviors v1" above and in `docs/design-log.md`.
