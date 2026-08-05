# Codex setup guide

This guide configures Codex to export raw usage facts to
`agentic-footprint` (`af`). Codex exports native OTLP logs directly to the
resident receiver; no extra collector process is needed.

All commands below use the default local addresses:

- OTLP receiver: `http://127.0.0.1:4318`;
- debug console: `http://127.0.0.1:9414`.

## 1. Build and initialize `af`

The recommended onboarding path is the unified installer and setup wizard:

```sh
./install.sh
```

Or, when `af` is already installed:

```sh
af setup
```

The wizard verifies that an `af watch` receiver is healthy before inspecting
agents. On macOS/Linux it can install a per-user service; on Windows start
`af watch` in a persistent PowerShell window first. It then detects Codex and
Claude Code, configures Codex OTLP automatically when safe, and installs or
merges Claude Code collection. See
[`installation.md`](installation.md) for automation flags.

Confirm receiver health:

```sh
af setup --check
```

On macOS/Linux, `af service status` additionally checks the installed service.

For manual development from the repository root:

```sh
cargo build -p af-cli
export AF_BIN="$PWD/target/debug/af"
export AF_STATE_DIR="$HOME/.local/state/agentic-footprint"
```

Remote inference facts can be collected without the managed Python runtime.
To calculate remote impacts and measure local process energy, also run:

```sh
"$AF_BIN" python setup
"$AF_BIN" python doctor
```

For manual development without the installed service, start the control plane
before Codex:

```sh
"$AF_BIN" watch --debug
```

If the managed Python runtime is intentionally not installed yet, collect and
inspect raw facts without sampling or estimation:

```sh
"$AF_BIN" watch --debug --no-sidecars
```

Keep this process running. If port `4318` is already used, select another
loopback address with `--otlp-addr` and use the same address in the Codex
configuration below.

The installed launchd/systemd service runs without `--debug`. On macOS inspect
`$AF_STATE_DIR/logs/`; on Linux use
`journalctl --user -u agentic-footprint-watch.service`. Run a separate
`af watch --debug --no-otlp` when only the debug UI is needed.

## 2. Configure Codex

### Persistent configuration

For a config that does not already configure `otel`, the unified wizard appends
the table below, backing the file up first and refusing a conflicting existing
exporter:

```sh
af setup --agents codex
af setup --agents codex --endpoint http://127.0.0.1:5000/v1/logs
```

The target must be the Codex home your sessions actually read. If you launch
Codex with a non-default home — for example a shell alias like
`alias codex='CODEX_HOME="$PWD/.codex" codex'`, which gives every project
directory its own home — run the script once per home:

```sh
CODEX_HOME=/path/to/project/.codex af setup --agents codex
```

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
- use `protocol = "json"`; the current `af` receiver accepts OTLP HTTP/JSON;
- keep `log_user_prompt = false` unless prompt text export is explicitly
  desired; `af` does not need prompt contents;
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

Ensure `af setup --check` passes first. The receiver needs the initial
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

After a Codex turn, confirm that a spool file exists:

```sh
ls "$AF_STATE_DIR"/spool/otlp-codex.*.jsonl
```

With `af watch --debug`, open the debug console:

```text
http://127.0.0.1:9414
```

A complete session should produce:

- `session_meta` from `codex.conversation_starts`;
- `llm_call` from token-bearing `codex.sse_event/response.completed` records;
- `action_span` from `codex.tool_result` records.
