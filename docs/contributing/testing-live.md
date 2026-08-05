# Live end-to-end tests

Each integrated coding agent gets a `live_<agent>.rs` suite under
`crates/af-cli/tests/` that spawns a **real agent session** against a real
`af watch --debug` and asserts the whole pipeline through the `/debug` HTTP
contract.

They are `#[ignore]`d, so `cargo test` (and any future default CI job) never
runs them: they spend tokens, need the agent CLI installed and logged in,
and take minutes. They run only on manual invocation:

This document focuses on the isolated repository test harness. OpenCode tests
are experimental and require an `af-cli` build with
`--features experimental-opencode`; they are not part of the first release.

```sh
scripts/test-live.sh                         # Claude Code → OpenCode → Codex
scripts/test-live.sh claude-code             # Claude Code only
scripts/test-live.sh opencode                # OpenCode only
scripts/test-live.sh codex                   # Codex only
scripts/test-live.sh codex native_otel       # one test-name filter
scripts/test-live.sh smoke                   # legacy Claude test-name filter
AF_LIVE_MODEL=sonnet scripts/test-live.sh claude-code
AF_LIVE_CODEX_MODEL=gpt-5.4-mini scripts/test-live.sh codex
```

Running `all` includes Claude Code's energy test and therefore requires the
managed Python environment created by `af python setup`. Use the agent selector
when validating only OpenCode or Codex.

## Recommended validation order

Run deterministic coverage before spending tokens on a live agent:

```sh
# Contract/receiver regression coverage, including Codex OTLP fixtures.
cargo test -p af-otlp

# OpenCode SSE-to-Contract fixture coverage.
collectors/opencode/test_collector.sh

# Compile every manual live test without running it.
cargo test -p af-cli --test live_claude_code --no-run
cargo test -p af-cli --test live_opencode --no-run
cargo test -p af-cli --test live_codex --no-run
```

Then run the relevant live suite:

```sh
scripts/test-live.sh opencode
scripts/test-live.sh codex
scripts/test-live.sh claude-code smoke
```

The OpenCode source-checkout form used during development is:

```sh
AF_LIVE_OPENCODE_REPO=/absolute/path/to/opencode \
  scripts/test-live.sh opencode
```

Use `scripts/test-live.sh all` only for a deliberate full matrix: it spends
tokens with three agents and also runs Claude Code's energy-sidecar test.

## What a live run touches (and what it can't)

Each test builds its own world:

| Concern | Isolation |
|---|---|
| spool / store / offsets | tempdir `AF_STATE_DIR` — the developer's real state dir is never read or written |
| agent project | temp project directory |
| ports | ephemeral `--debug-addr` / `--otlp-addr`, so a resident `af watch` can keep running |
| Claude settings | generated `--settings` file with hooks + OTLP env; this repo's `.claude/settings.json` never applies |
| OpenCode config | real installed/source-tree config and authentication are inherited |
| Codex config | user config/plugins are ignored; authentication is inherited; OTLP is injected with command-line config |

## Suites

`live_claude_code.rs`:

- `smoke_fresh_session_reaches_debug_console` — `--no-sidecars`, no Python:
  hook spans + OTLP token usage must reach `/debug/health`,
  `/debug/snapshot`, `/debug/session` and `/debug/report`, with zero
  rejects. Estimates staying `pending` is the documented honest degradation.
- `energy_sampling_attributes_local_compute` — full sidecars; needs
  `af python setup` (the harness symlinks the real managed venv into the
  temp state dir). Asserts `energy_sample`s and allocation traces exist.

`live_opencode.rs`:

- `durable_sse_reaches_debug_console` — starts an isolated OpenCode server,
  creates a session through its HTTP API, subscribes with the durable SSE
  collector, and verifies an `llm_call` reaches the debug snapshot. Provider
  failure is acceptable when it is represented honestly as an errored call.
- Requires either an installed `opencode` binary or
  `AF_LIVE_OPENCODE_REPO=/absolute/path/to/opencode` for a source checkout.

`live_codex.rs`:

- `native_otel_reaches_debug_console` — runs an ephemeral, read-only
  `codex exec` turn with native OTLP pointed at the harness receiver and
  verifies `session_meta`, token-bearing `llm_call`, and `exec_command`
  `action_span` events.
- Requires an installed and authenticated `codex` CLI. It passes
  `--ignore-user-config --ignore-rules`, so unrelated plugins, tools, and
  project policies do not alter the fixture turn.

Missing prerequisites fail with the remedy in the message (install/login
the CLI, run `af python setup`) rather than silently skipping — a live run
that skips its point looks exactly like a pass.

## Knobs

| Env | Default | Meaning |
|---|---|---|
| `AF_LIVE_MODEL` | `haiku` | Claude Code model alias/id; pick one EcoLogits knows when estimate values matter |
| `AF_LIVE_TIMEOUT_SECS` | `300` | Claude Code per-session wall-clock budget |
| `AF_LIVE_OPENCODE_REPO` | unset | absolute OpenCode source checkout; when unset, use the installed `opencode` binary |
| `AF_LIVE_CODEX_MODEL` | `gpt-5.4-mini` | explicit Codex live-test model; avoids inheriting a potentially expensive account default |

## Adding the next agent

1. Reuse `common/live.rs` for `LiveWatch`, temporary state, condition-based
   waits, and debug API polling. Add an agent driver there only when multiple
   tests share it; the OpenCode and Codex one-test drivers remain local to
   their suites.
2. Add `crates/af-cli/tests/live_<agent>.rs` with `#[ignore]`d tests
   asserting through `/debug` (reuse the smoke suite's assertions — the
   contract is agent-independent).
3. Add the selector and `all` ordering to `scripts/test-live.sh`.

## CI policy

The default CI job excludes live suites by construction. A future dedicated,
opt-in job may run them with credentials, a known model, and a generous
timeout. Deterministic replay and fixture suites remain the required release
gate.
