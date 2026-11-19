# Pi setup guide

This guide configures [Pi](https://github.com/badlogic/pi-mono) (the
`pi` coding agent) to report usage facts to `agentic-footprint` (`af`).

Pi has no OpenTelemetry exporter. It loads TypeScript **extensions** into its
own process, and `af` collects through one: `collectors/pi/af-collector.ts`
subscribes to Pi's lifecycle events and appends Contract #1 facts to the
spool, where `af watch` ingests them like any other collector.

Pi support is part of the default `af` build. The v0.1.0 release predates it;
releases built after it include it.

## 1. Install `af`

Install `af` as described in [Install and configure](installation.md), or,
for manual development, from the repository root:

```sh
cargo build -p af-cli
export AF_BIN="$PWD/target/debug/af"
```

Pi must be installed and on `PATH` (`pi --version`), and logged in to a
provider (`pi`, then `/login`).

## 2. Install the collector extension

### With `af setup` (recommended)

```sh
"$AF_BIN" setup --agents pi --dry-run   # show the plan
"$AF_BIN" setup --agents pi             # apply it
"$AF_BIN" setup --agents pi --check     # verify
```

Setup writes the collector, embedded in `af`, to Pi's global extensions
directory as `agentic-footprint.ts`:

- `$PI_CODING_AGENT_DIR/extensions/` when `PI_CODING_AGENT_DIR` is set;
- otherwise `~/.pi/agent/extensions/`.

Pi loads every `*.ts` file in that directory at startup, so every Pi session
in every project is collected with no further step. Rerunning setup after
upgrading `af` replaces an older copy; the old file is backed up under a
name ending in `.bak.<timestamp>`, which Pi does not load.

Like every agent, setup needs a healthy receiver first: on macOS and Linux it
offers to install the background service (see
[Install and configure](installation.md)).

### Manually

Without `af setup`, either copy the extension into Pi's extensions directory:

```sh
mkdir -p ~/.pi/agent/extensions
cp collectors/pi/af-collector.ts ~/.pi/agent/extensions/agentic-footprint.ts
```

or load it for a single run:

```sh
pi --extension /absolute/path/to/collectors/pi/af-collector.ts
```

Do not do both: Pi would load the collector twice. The store keeps one copy
of each fact (the event IDs are deterministic), but the spool would hold
duplicate lines.

## 3. Start the control plane

With the background service installed, nothing more is needed. For manual
development, start the receiver before Pi:

```sh
"$AF_BIN" watch --debug
```

To estimate remote impacts and measure local energy, provision the managed
Python runtime once with `"$AF_BIN" python setup`; otherwise add
`--no-sidecars`.

The extension writes to `$AF_STATE_DIR/spool/` when Pi's environment sets
`AF_STATE_DIR`, otherwise to af's default state directory. If `af watch`
runs with a non-default `AF_STATE_DIR`, export the same value in the shell
that starts Pi.

## 4. Verify collection

Run any Pi session, then:

```sh
"$AF_BIN" status      # Pi: waiting, then receiving
"$AF_BIN" summary     # by_agent lists Pi with its calls and tokens
```

The spool file is `$AF_STATE_DIR/spool/pi.<session-id>.jsonl`. `af watch`
writes to the store once a minute, so results can lag a live session by that
much. In the [debug console](debug-console.md), the **Stream** tab lists one
row per fact with collector `pi@0.1.0`.

## What is collected

| Pi event | Fact |
|---|---|
| `session_start` | `session_meta` |
| `message_end` with an assistant message | `llm_call`: provider, model, token usage, status, duration |
| `tool_execution_start` + `tool_execution_end`, paired by `toolCallId` | `action_span`: tool, kind, start, end, status |
| `session_shutdown` | closes spans still open as `cancelled` |

Pi repeats each assistant message in `turn_end` and `agent_end`; the
collector ignores both so that every model call is counted once.

The extension never reads prompts, message content, tool arguments or tool
results. A failure inside it is logged to
`$AF_STATE_DIR/tmp/pi-collector-errors.log` and never reaches the Pi
session.

## Troubleshooting

### `af setup` reports Pi as not installed

`pi` is not on `PATH` in the shell that runs `af setup`.

### `af status` reports Pi as `misconfigured`

The installed extension comes from another `af` version. Rerun
`af setup --agents pi`.

### No `pi.*.jsonl` file appears

Check that Pi loads the extension: it must be in the extensions directory
above (or passed with `--extension`), and Pi reads `PI_CODING_AGENT_DIR`
from its own environment. Then check that Pi's `AF_STATE_DIR` matches the
receiver's.

### LLM calls stay `unknown_model`

Remote impacts are estimated by EcoLogits from the provider and model name
Pi reports. Some Pi provider IDs are not EcoLogits provider names; for
example Pi reports a ChatGPT login as `openai-codex`, which EcoLogits does not
know, so those calls are counted but not estimated. Local energy is still
measured.

### Codex models are refused with a ChatGPT account

Pi 0.73.1's built-in `openai-codex` model list is out of date: listed models
answer `400 … not supported when using Codex with a ChatGPT account`. Pass a
current model explicitly, for example
`pi --model openai-codex/gpt-5.6-luna`.
