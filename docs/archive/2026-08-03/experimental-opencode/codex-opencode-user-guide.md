# Codex and OpenCode setup guide

This guide configures Codex and OpenCode to export raw usage and action facts
to `agentic-footprint` (`af`).

The two integrations use different paths:

- **Codex** exports native OTLP logs directly to the receiver inside
  `af watch`. No extra collector process is needed.
- **OpenCode** exposes durable per-session SSE events. Run
  `af collect opencode` for each OpenCode session while `af watch` ingests the
  resulting JSONL spool.

All commands below use the default local addresses:

- OTLP receiver: `http://127.0.0.1:4318`;
- debug console: `http://127.0.0.1:9414`;
- OpenCode server: `http://127.0.0.1:4096`.

## 1. Build and initialize `af`

The recommended onboarding path is the unified installer and setup wizard:

```sh
./install.sh
```

Or, when `af` is already installed:

```sh
af setup
```

The wizard installs and verifies a resident `af watch` user service, detects
Codex, Claude Code, and OpenCode together, configures Codex OTLP automatically
when safe, installs/merges Claude Code collection, and reports the current
OpenCode collector workflow. See
See the [current installation guide](../../../how-to/installation.md) for
automation flags.

Confirm the installed receiver is healthy:

```sh
af service status
```

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
before Codex or the OpenCode collector:

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

Ensure `af service status` succeeds first. The receiver needs the initial
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

## 3. Configure OpenCode

OpenCode collection requires a server because `af` subscribes to its durable
per-session event route. The normal local TUI can attach to this server.

### Start the OpenCode server

In the project directory you want OpenCode to work on:

```sh
opencode serve --hostname 127.0.0.1 --port 4096 &
echo $! > /tmp/af-opencode-server.pid
wait
```

Keep the server bound to `127.0.0.1`. The current `af collect opencode`
command does not send OpenCode Basic Auth credentials, so a password-protected
or remotely exposed server is not currently supported by this integration.

### Open or create a session

In another terminal, attach the TUI to the server:

```sh
opencode attach http://127.0.0.1:4096 --dir "$PWD"
```

Create a session or open the session you want to measure. To list the sessions
visible to this exact server and project directory:

```sh
curl --fail --silent --show-error \
  -H "x-opencode-directory: $PWD" \
  http://127.0.0.1:4096/api/session
```

If `jq` is installed, print only the useful fields:

```sh
curl --fail --silent --show-error \
  -H "x-opencode-directory: $PWD" \
  http://127.0.0.1:4096/api/session \
  | jq -r '.data[] | [.id, .directory, .title] | @tsv'
```

For a local server using the same OpenCode data directory, this CLI shortcut
is also useful:

```sh
opencode session list --format json --max-count 20
```

The collector accepts a session ID such as `ses_...`. It can start after the
session already exists because `--after 0` replays the durable history.

### Start the native OpenCode collector

Set the session ID and start collection:

```sh
export OPENCODE_SESSION_ID="ses_..."

"$AF_BIN" collect opencode \
  --url http://127.0.0.1:4096 \
  --session-id "$OPENCODE_SESSION_ID" \
  --directory "$PWD" \
  --after 0 \
  --pid "$(cat /tmp/af-opencode-server.pid)" \
  --opencode-version "$(opencode --version)"
```

`--pid` is optional. When supplied, local OpenCode action spans carry the
server root PID so `af watch` can attribute process-tree measurements. Omit it
if the server PID is unknown rather than supplying an unrelated PID.

After the first successful event, the collector stores an atomic cursor under:

```text
$AF_STATE_DIR/cursors/opencode/
```

On later starts, omit `--after`; the collector resumes automatically and keeps
reconnecting after EOF or network errors:

```sh
"$AF_BIN" collect opencode \
  --url http://127.0.0.1:4096 \
  --session-id "$OPENCODE_SESSION_ID" \
  --directory "$PWD" \
  --pid "$(cat /tmp/af-opencode-server.pid)"
```

Run one collector process per OpenCode session that should be measured.

### Verify OpenCode export

Confirm the per-session spool exists:

```sh
ls "$AF_STATE_DIR/spool/opencode.$OPENCODE_SESSION_ID.jsonl"
```

A complete session can produce:

- `session_meta` when the collector first creates the spool;
- `llm_call` from paired step start/ended or start/failed events;
- `action_span` from paired tool or shell lifecycle events.

These events should also appear in the `af watch --debug` console at
`http://127.0.0.1:9414`.

## 4. Reports and shutdown

Generate the current report at any time:

```sh
"$AF_BIN" report --format json
```

If estimators were installed after facts were collected, recompute derived
records:

```sh
"$AF_BIN" replay --format json
```

For OpenCode, stop the TUI first, then the collector, then the server. Stop
`af watch` last so it can ingest the final complete spool lines.

## 5. Troubleshooting

### Codex produces no `otlp-codex.*` spool

- Confirm `af service status` reports the receiver reachable.
- Confirm the Codex endpoint ends with `/v1/logs`.
- Confirm the protocol is `json`, not gRPC or protobuf.
- Run `codex doctor --summary` and confirm the configuration is loaded.
- Use `codex exec --strict-config ...` when unknown configuration keys must be
  rejected before a real turn.
- For a custom receiver port, reinstall the service with matching `--endpoint`
  and update the Codex endpoint.
- Restart Codex after changing persistent configuration.

### Codex calls use `provider: "unknown"`

The receiver likely missed the conversation-start record. Confirm
`af service status`, then begin a new Codex session.

### OpenCode collector cannot connect

- Confirm the server is listening on `127.0.0.1:4096`.
- Confirm `--url` points to the server, not the debug console or OTLP port.
- Confirm the server is not requiring Basic Auth.
- Pass the same project path to `opencode attach --dir` and collector
  `--directory`.

### OpenCode cursor is corrupt or belongs to another server

The collector fails closed instead of silently replaying from an uncertain
position. Inspect or remove the reported cursor file, then intentionally replay
from the start:

```sh
"$AF_BIN" collect opencode \
  --url http://127.0.0.1:4096 \
  --session-id "$OPENCODE_SESSION_ID" \
  --directory "$PWD" \
  --after 0
```

Stable OpenCode event IDs make replay deduplicable during ingestion, although
the JSONL spool can temporarily contain a replayed line if the previous process
stopped after append but before its cursor replacement.

### Events exist but impacts remain pending

Raw collection works without the managed estimator environment. Run:

```sh
"$AF_BIN" python setup
"$AF_BIN" python doctor
"$AF_BIN" replay --format json
```
