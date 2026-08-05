# Claude Code collection user guide

For a new installation, prefer the unified installer and project setup wizard:

=== "macOS / Linux"

    ```sh
    ./install.sh
    # or, when af is already installed:
    af setup --agents claude-code --global
    ```

=== "Windows 11"

    ```powershell
    .\install.ps1
    af watch
    # in another PowerShell window:
    af setup --agents claude-code --global --yes
    ```

The wizard checks receiver health before inspecting Claude Code. On
macOS/Linux it can install a per-user receiver; Windows uses the foreground
receiver shown above. The wizard installs a shell hook on Unix or configures
the built-in `af hook` collector on Windows, merges `.claude/settings.json`,
and backs up an existing settings file. The manual procedure below remains
useful for auditing or custom deployments.

This guide installs and runs `agentic-footprint` locally for Claude Code.
The complete collection path uses three complementary inputs:

- Claude Code hooks record session and local tool-call lifecycle events;
- Claude Code OTLP logs provide model and token-usage facts for LLM calls;
- `af watch` runs the local machine sampler and joins measured local energy
  with remotely estimated inference impacts.

All data remains under one local state directory. The default is
`~/.local/state/agentic-footprint` on macOS/Linux and
`%LOCALAPPDATA%\agentic-footprint` on Windows; set `AF_STATE_DIR` everywhere
if you want another location.

## 1. Prerequisites

Install:

- Rust and Cargo;
- `jq` on macOS/Linux, required by the shell hook and richer shell statusline;
- `uv`, used by `af python setup` to create the managed Python environment;
- Claude Code.

From the repository root, verify the external commands:

```sh
cargo --version
uv --version
claude --version
```

On macOS/Linux, also verify `jq --version`. Windows setup uses the built-in
`af hook` collector and does not require `jq` or a Unix shell.

## 2. Build and expose `af`

Build the release binary:

```sh
cargo build --release -p af-cli
```

For the commands below, either use its absolute path or add it to your
shell's `PATH`:

```sh
export AF_REPO="$(pwd)"
export AF_BIN="$AF_REPO/target/release/af"
"$AF_BIN" --help
```

Keep `AF_REPO` and `AF_BIN` pointed at absolute paths. Claude Code hooks must
not depend on the current repository being the agentic-footprint repository.

## 3. Provision the estimator and sampler

Create the managed Python environment with the pinned CodeCarbon, EcoLogits,
and supporting dependencies:

```sh
"$AF_BIN" python setup
"$AF_BIN" python doctor
```

`python doctor` should report the managed environment as usable. Collection
can still run without it, but local energy will not be sampled and remote LLM
calls will remain pending until the environment is repaired and replayed.

## 4. Install the Claude Code hooks

The manual shell-hook instructions in this section apply to macOS/Linux. On
Windows, use `af setup --agents claude-code`; it registers the native
`"C:\path\to\af.exe" hook` command instead.

Make the hook executable:

```sh
chmod +x "$AF_REPO/collectors/claude-code/af-hook.sh"
```

Add the following to the Claude Code settings for the projects you want to
observe, normally `.claude/settings.json`. Replace every
`/absolute/path/to/agentic-footprint` with the actual absolute repository
path.

If the file already contains settings, merge the `hooks` object instead of
replacing the whole file.

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/agentic-footprint/collectors/claude-code/af-hook.sh"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/agentic-footprint/collectors/claude-code/af-hook.sh"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/agentic-footprint/collectors/claude-code/af-hook.sh"
          }
        ]
      }
    ],
    "PostToolUseFailure": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/agentic-footprint/collectors/claude-code/af-hook.sh"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/agentic-footprint/collectors/claude-code/af-hook.sh"
          }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/agentic-footprint/collectors/claude-code/af-hook.sh"
          }
        ]
      }
    ]
  }
}
```

Register all six events. In particular, `PostToolUseFailure` closes failed,
denied, and interrupted tool calls; without it those spans remain open until
the end-of-turn fallback sweep.

Use the script path directly. Do **not** wrap it in `sh -c`: the collector
uses its parent PID as the best available identity for the Claude Code root
process, and an extra shell would replace that parent.

The hook always exits successfully and is designed never to interrupt a
Claude Code session. Internal hook errors are written best-effort to
`$AF_STATE_DIR/tmp/hook-errors.log`.

## 5. Start the control plane

On macOS/Linux, the unified installer starts the resident receiver
automatically. Verify it before launching Claude Code:

```sh
af service status
```

On Windows, for a manual development checkout, or when no supported service
manager is available, choose the state directory and keep a foreground
receiver running:

```sh
export AF_STATE_DIR="$HOME/.local/state/agentic-footprint"
"$AF_BIN" watch
```

By default:

- Claude Code exports OTLP logs to `http://127.0.0.1:4318`;
- the debug console is served at `http://127.0.0.1:9414` when `--debug` is
  enabled;
- hooks append JSONL facts under `$AF_STATE_DIR/spool/`;
- estimates and derived joins are stored in `$AF_STATE_DIR/state.db`.

Optional location settings:

```sh
# Local machine electricity grid. --zone remains a compatibility alias.
"$AF_BIN" watch --debug --local-grid-zone FRA

# Audit override for the remote inference electricity region. If omitted,
# the estimator owns remote-region detection/defaulting.
"$AF_BIN" watch --debug --remote-region FRA
```

The installed macOS/Linux background service does not enable debug mode. On macOS inspect
`$AF_STATE_DIR/logs/watch.stderr.log`; on Linux run
`journalctl --user -u agentic-footprint-watch.service`. For the debug console,
temporarily stop the service or run a separate `af watch --debug --no-otlp`.

Use `--no-sidecars` only when you intentionally want ingestion and
attribution without CodeCarbon sampling or EcoLogits estimation.

## 6. Launch Claude Code with OTLP enabled

In the shell that launches Claude Code, use the same `AF_STATE_DIR` as the
watch process and enable its OTLP log exporter:

```sh
export AF_STATE_DIR="$HOME/.local/state/agentic-footprint"
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL=http/json
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
export OTEL_LOGS_EXPORT_INTERVAL=2000

claude
```

The hooks and OTLP exporter are independent but complementary:

- hook files are named `cc-hooks.<session-id>.jsonl` and describe local
  actions;
- OTLP files are named `otlp-cc.<session-id>.jsonl` and contain normalized
  LLM-call facts.

Both must use the same Claude Code session ID for the control plane to join
the facts into one session.

## 7. Inspect the results

While `af watch` is running, open the debug console or inspect stderr:

```text
http://127.0.0.1:9414
```

You can also produce a report from another terminal. SQLite WAL mode allows
the report reader and resident watch writer to overlap:

```sh
export AF_STATE_DIR="$HOME/.local/state/agentic-footprint"
"$AF_BIN" report --format text
"$AF_BIN" report --format json
```

After changing methodology or an explicit remote-region override, replay all
derived records from the preserved raw facts:

```sh
"$AF_BIN" replay --format json
"$AF_BIN" replay --format json --remote-region FRA
```

Stop `af watch` with `Ctrl-C`. It performs a final ingest pass and shuts down
its sidecars. If you are ending a short scripted Claude Code run, allow a few
seconds for the OTLP exporter to flush before stopping the watch process.

## 8. Optional Claude Code statusline

The included statusline reads already-computed local data; it performs no
network request and does not ingest or estimate anything itself.

On macOS/Linux, create an executable wrapper so the richer shell statusline
always knows the absolute binary and script paths:

```sh
mkdir -p "$HOME/.claude"
cat > "$HOME/.claude/agentic-footprint-statusline.sh" <<EOF
#!/usr/bin/env bash
export AF_BIN="$AF_BIN"
export AF_STATE_DIR="${AF_STATE_DIR:-$HOME/.local/state/agentic-footprint}"
exec "$AF_REPO/statusline/ecologits-bar.sh"
EOF
chmod +x "$HOME/.claude/agentic-footprint-statusline.sh"
```

Then merge this setting into `.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/absolute/path/to/home/.claude/agentic-footprint-statusline.sh"
  }
}
```

If you already have a custom statusline, capture its stdin once and pipe the
same JSON to the included component after rendering your existing line:

```sh
input=$(cat)

# Render the existing statusline here.

printf '%s' "$input" | "$HOME/.claude/agentic-footprint-statusline.sh"
```

Configure displayed metrics in `~/.claude/ecologits.config.sh`, for example:

```sh
: "${ECOLOGITS_METRICS:=gwp wcf energy adpe pe model}"
```

On Windows, configure Claude Code to invoke the native read-only command
directly, replacing the path with the installed executable:

```json
{
  "statusLine": {
    "type": "command",
    "command": "\"C:\\Users\\you\\AppData\\Local\\Programs\\agentic-footprint\\af.exe\" statusline"
  }
}
```

## 9. Verify collection

After starting one Claude Code session and running at least one tool, check:

```sh
find "$AF_STATE_DIR/spool" -maxdepth 1 -type f -name '*.jsonl' -print
"$AF_BIN" report --format text
```

For the complete setup, expect both a `cc-hooks.*.jsonl` file and an
`otlp-cc.*.jsonl` file for the session. The report should show local action
spans and LLM calls; impact estimates may appear shortly after ingestion
because `af watch` estimates asynchronously.

## 10. Troubleshooting

### No `cc-hooks.*` file

- On macOS/Linux, confirm `jq` is available in the environment inherited by
  Claude Code and that the hook path is absolute and executable.
- On Windows, rerun `af setup --agents claude-code` and confirm the configured
  command points to the installed `af.exe hook`.
- Confirm the hook is registered directly, without `sh -c`.
- Inspect `$AF_STATE_DIR/tmp/hook-errors.log`.
- Run `collectors/claude-code/test_hooks.sh` from the repository root.

### No `otlp-cc.*` file

- Run `af setup --check` and confirm the receiver is reachable. On
  macOS/Linux, `af service status` also diagnoses the installed service.
- Confirm telemetry and OTLP variables are exported in the Claude Code
  process's environment.
- Confirm the endpoint is `http://127.0.0.1:4318` and the protocol is
  `http/json`.
- Check the launchd log files or systemd user journal for receiver or
  normalization errors.

### LLM calls remain pending

- Run `"$AF_BIN" python doctor`.
- Repair the environment with `"$AF_BIN" python setup`.
- Run `"$AF_BIN" replay --format json` after the estimator is available.

### Local energy is absent

- Confirm the managed Python environment passes `python doctor`.
- Do not start watch with `--no-sidecars`.
- Check watch stderr for sampler startup or permission errors.

### The statusline always shows zero

- Confirm the wrapper uses the same `AF_STATE_DIR` as `af watch`.
- Confirm `AF_BIN` points to an executable `af` binary.
- Confirm the Claude Code statusline input reaches the script unchanged.
- Run `printf '%s\n' '<status JSON>' | "$AF_BIN" statusline` to test the
  read-only backend separately.

For collector internals and payload details, see
`collectors/claude-code/README.md`.
