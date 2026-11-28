# Codex setup guide

This guide configures Codex to export raw usage facts to
`agentic-footprint` (`afoot`). Codex exports native OTLP logs directly to the
resident receiver; no extra collector process is needed.

All commands below use the default local addresses:

- OTLP receiver: `http://127.0.0.1:4318`;
- debug console: `http://127.0.0.1:9414`.

## 1. Build and initialize `afoot`

The recommended onboarding path is the unified installer and setup wizard:

```sh
./install.sh
```

Or, when `afoot` is already installed:

```sh
afoot setup
```

The wizard verifies that an `afoot watch` receiver is healthy before inspecting
agents. On macOS/Linux it can install a per-user service; on Windows start
`afoot watch` in a persistent PowerShell window first. It then detects Codex and
Claude Code, configures Codex OTLP automatically when safe, and installs or
merges Claude Code collection. See
[`installation.md`](installation.md) for automation flags.

Confirm receiver health:

```sh
afoot setup --check
```

On macOS/Linux, `afoot service status` additionally checks the installed service.

For manual development from the repository root:

```sh
cargo build -p afoot-cli
export AFOOT_BIN="$PWD/target/debug/afoot"
export AFOOT_STATE_DIR="$HOME/.local/state/agentic-footprint"
```

Remote inference facts can be collected without the managed Python runtime.
To calculate remote impacts and measure local process energy, also run:

```sh
"$AFOOT_BIN" python setup
"$AFOOT_BIN" python doctor
```

For manual development without the installed service, start the control plane
before Codex:

```sh
"$AFOOT_BIN" watch --debug
```

If the managed Python runtime is intentionally not installed yet, collect and
inspect raw facts without sampling or estimation:

```sh
"$AFOOT_BIN" watch --debug --no-sidecars
```

Keep this process running. If port `4318` is already used, select another
loopback address with `--otlp-addr` and use the same address in the Codex
configuration below. `afoot watch` refuses a non-loopback `--otlp-addr` or
`--debug-addr`: neither listener has authentication.

The installed launchd/systemd service runs without `--debug`. On macOS inspect
`$AFOOT_STATE_DIR/logs/`; on Linux use
`journalctl --user -u agentic-footprint-watch.service`. Run
`afoot console` to open the debug UI on the running service (see
[Open the debug console](debug-console.md)).

## 2. Configure Codex

### Persistent configuration

For a config that does not already configure `otel`, the unified wizard appends
the table below, backing the file up first and refusing a conflicting existing
exporter:

```sh
afoot setup --agents codex
afoot setup --agents codex --endpoint http://127.0.0.1:5000/v1/logs
```

The target must be the Codex home your sessions actually read. Setup
configures the primary home (`$CODEX_HOME`, else `~/.codex`) and, when the
`--project` directory (default: the current directory) contains a `.codex/`
directory, `<project>/.codex/config.toml` as well. That covers a shell alias
like `alias codex='CODEX_HOME="$PWD/.codex" codex'`, which gives every project
directory its own home: run setup (or `afoot setup --check`) once from each such
project:

```sh
afoot setup --agents codex --project /path/to/project
```

Setup cannot see a shell alias, so a home outside both locations still needs
`CODEX_HOME=/path/to/home afoot setup --agents codex`.

`codex doctor --summary` prints which `config.toml` a session in the current
directory loads.

To merge manually instead: open `${CODEX_HOME:-$HOME/.codex}/config.toml` and
merge this section into the existing file. Do not add a second `[otel]` table
if one already exists.

```toml
[otel]
environment = "dev"
log_user_prompt = false
metrics_exporter = "none"
trace_exporter = "none"
exporter = { otlp-http = {
  endpoint = "http://127.0.0.1:4318/v1/logs",
  protocol = "json"
} }
```

Important details:

- include `/v1/logs` in the endpoint;
- use `protocol = "json"`; the current `afoot` receiver accepts OTLP HTTP/JSON;
- keep `log_user_prompt = false` unless prompt text export is explicitly
  desired; `afoot` does not need prompt contents;
- metrics and traces are not needed by the Codex normalizer.

Check installation, authentication, connectivity, and whether the
configuration loads:

```sh
codex doctor --summary
```

`codex doctor` can report unrelated authentication or network failures while
still showing `Configuration / config: loaded`. To make unknown configuration
keys fatal for a real turn, add `--strict-config` to `codex exec`:

```sh
codex exec --strict-config "Reply with exactly: config-ok"
```

Then use Codex normally:

```sh
codex
# or
codex exec "Inspect this repository and summarize the test layout"
```

Ensure `afoot setup --check` passes first. The receiver needs the initial
`codex.conversation_starts` record to associate later calls with their session
and provider.

### One-shot configuration

To test export without modifying `config.toml`:

```sh
codex exec \
  -c 'otel.exporter={otlp-http={endpoint="http://127.0.0.1:4318/v1/logs",protocol="json"}}' \
  -c 'otel.log_user_prompt=false' \
  -c 'otel.metrics_exporter="none"' \
  -c 'otel.trace_exporter="none"' \
  "Reply with exactly: telemetry-ok"
```

The `-c` values are parsed as TOML and override the user configuration for that
invocation.

### Verify Codex export

Codex telemetry is not written to the spool: the receiver in `afoot watch`
normalizes it and stores it directly in `$AFOOT_STATE_DIR/state.db`, once a
minute (a new session is written at once). After a Codex turn, wait for that
flush, then check:

```sh
"$AFOOT_BIN" report --format text
"$AFOOT_BIN" doctor
```

The text report's `EVENT_COUNTS` column should include `llm_call=` for the
session. `afoot doctor` checks the
receiver and flags a running `codex` process that started before its
telemetry configuration last changed.

With `afoot watch --debug`, or `afoot console` beside the background service, open
the debug console:

```text
http://127.0.0.1:9414
```

A complete session should produce:

- `session_meta` from `codex.conversation_starts`;
- `llm_call` from token-bearing `codex.sse_event/response.completed` records;
- `action_span` from `codex.tool_result` records.
