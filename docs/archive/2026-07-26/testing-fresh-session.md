# Smoke test — instrument a fresh Claude Code session

End-to-end check that a new Claude Code session in this repo shows up in the debug console.

> The automated version of this procedure lives in
> `crates/af-cli/tests/live_claude_code.rs` — see
> [current live-testing guide](../../contributing/testing-live.md). This manual walkthrough remains the
> way to *watch* the console do it.

## One-time setup (skip if done)

```sh
npm --prefix console run build          # embed the real UI, not the placeholder
cargo install --path crates/af-cli      # installs `af` into ~/.cargo/bin
af python setup                         # optional: enables local energy sampling (needs uv + network)
```

Hooks + OTEL env are already configured in `.claude/settings.json` (project scope) —
they apply to sessions **started after** that file existed. `/hooks` shows them (read-only).

## Procedure

1. **Terminal A** — start the control plane and keep it running:
   ```sh
   af watch --debug
   ```
   Expect startup lines: OTLP receiver on `:4318`, debug console on `:9414`.
   (Add `--no-sidecars` to skip Python/energy; token + span data still flows.)

2. **Browser** — open `http://127.0.0.1:9414/`. All five tabs render empty states.

3. **Terminal B** — from this repo, start a **fresh** session and give it tool work:
   ```sh
   claude -p "run: echo hello, then read README.md"
   ```
   (An interactive session works the same; `-p` is just self-terminating.)

## Expected results and lag

| Signal | Where | Expected lag |
|---|---|---|
| SSE dot → live, session id in masthead | Masthead | first ingest pass (≤ ~2.5 s) |
| `action_span` bars (Bash, Read) | Timeline / Stream | 1–3 s after each tool call |
| `llm_call` with token usage | Timeline ticks / Stream / Impact | 5–10 s after the turn completes |
| `energy_sample` + allocation traces | Timeline power lane / Attribution | ~5 s cadence (sampler only) |
| Collector rows `cc-hooks` + `otlp-cc` | Health | with first events |
| `[ingest]` / `[span open]` lines | Decision log + Terminal A stderr | same as spans |

Session end (`SessionEnd`) closes any dangling spans; the Impact tab holds the
session `impact_join` (estimates stay `pending` without the estimator sidecar — that
is honest, not broken).

## If nothing shows up

- `ls ~/.local/state/agentic-footprint/spool/` — **empty?** Hooks aren't firing: the
  session predates the settings file, or wasn't started from this repo. Restart it here.
- Spool has `cc-hooks.*` but no `otlp-cc.*` — OTEL env didn't apply: same restart rule.
- `af: command not found` — `cargo install --path crates/af-cli` (PATH needs `~/.cargo/bin`).
- Placeholder page at `:9414` — console wasn't built before install; rerun both one-time steps.
- Port already bound — another `af watch` is running; kill it or use `--debug-addr`/`--otlp-addr`.
