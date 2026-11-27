# OpenCode setup guide

This guide configures [OpenCode](https://opencode.ai) to report usage facts
to `agentic-footprint` (`af`).

OpenCode has no OpenTelemetry exporter that `af` uses. It loads **plugins**
into its own process, and `af` collects through one: `af setup` installs a
plugin that turns OpenCode's bus events into Contract #1 facts and appends
them to the spool, where `af watch` ingests them like any other collector.

OpenCode support is part of the default `af` build. The v0.1.0 release
predates it; releases built after it include it. The plugin was checked live
on OpenCode 1.19.5.

## 1. Install `af`

Install `af` as described in [Install and configure](installation.md), or,
for manual development, from the repository root:

```sh
cargo build -p af-cli
export AF_BIN="$PWD/target/debug/af"
```

OpenCode must be installed and on `PATH` (`opencode --version`), and logged
in to a provider.

## 2. Install the collector plugin

### With `af setup` (recommended)

```sh
"$AF_BIN" setup --agents opencode --dry-run   # show the plan
"$AF_BIN" setup --agents opencode             # apply it
"$AF_BIN" setup --agents opencode --check     # verify
```

Setup writes the plugin, embedded in `af`, to OpenCode's global plugin
directory as `agentic-footprint.ts`:

- `$XDG_CONFIG_HOME/opencode/plugin/` when `XDG_CONFIG_HOME` is set;
- otherwise `~/.config/opencode/plugin/` (on every platform, as OpenCode
  resolves it).

OpenCode loads every file in that directory at startup, so every OpenCode
session in every project is collected with no further step. Rerunning setup
after upgrading `af` replaces an older copy. The old file is backed up as
`agentic-footprint.ts.bak.<timestamp>` in the directory *above* `plugin/`:
OpenCode loads every file in `plugin/`, and a stray backup there hangs it at
startup.

Like every agent, setup needs a healthy receiver first: on macOS and Linux it
offers to install the background service (see
[Install and configure](installation.md)).

### Manually

Without `af setup`, copy the plugin into OpenCode's plugin directory:

```sh
mkdir -p ~/.config/opencode/plugin
cp collectors/opencode/af-plugin.ts ~/.config/opencode/plugin/agentic-footprint.ts
```

Keep nothing else of `af`'s in that directory: OpenCode loads every file in
it.

## 3. Start the control plane

With the background service installed, nothing more is needed. For manual
development, start the receiver before OpenCode:

```sh
"$AF_BIN" watch --debug
```

To estimate remote impacts and measure local energy, provision the managed
Python runtime once with `"$AF_BIN" python setup`; otherwise add
`--no-sidecars`.

The plugin writes to `$AF_STATE_DIR/spool/` when OpenCode's environment sets
`AF_STATE_DIR`, otherwise to af's default state directory. If `af watch`
runs with a non-default `AF_STATE_DIR`, export the same value in the shell
that starts OpenCode.

## 4. Verify collection

Run any OpenCode session (the TUI or `opencode run`), then:

```sh
"$AF_BIN" status      # OpenCode: waiting, then receiving
"$AF_BIN" summary     # by_agent lists OpenCode with its calls and tokens
```

The spool file is `$AF_STATE_DIR/spool/opencode.<session-id>.jsonl`.
`af watch` writes to the store once a minute, so results can lag a live
session by that much. In the [debug console](debug-console.md), the
**Stream** tab lists one row per fact with collector `opencode@0.1.0-plugin`.

## What is collected

| OpenCode event | Fact |
|---|---|
| `session.created` | `session_meta` |
| `message.part.updated` with a `step-finish` part | `llm_call`: provider and model (from its assistant message), token usage, status, duration |
| `message.part.updated` with a completed or failed tool part | `action_span`: tool, kind, start, end, status |

OpenCode re-sends part updates; each fact is written once per OpenCode
process, and the store ignores an event ID it already holds.

The plugin never reads prompts, message text, tool inputs, outputs or error
messages. A failure inside it is swallowed and never reaches the OpenCode
session.

## Limitations

- A model call that fails before its step finishes is not counted.
- `af collect opencode` follows OpenCode's durable per-session event stream
  instead of the plugin. Released OpenCode (1.19.5) writes that stream only
  for sessions created through its `/api/session` HTTP route, not for the
  TUI's or `opencode run`'s, so the plugin is what collects in practice. See
  [`collectors/opencode/README.md`](https://github.com/mlco2/agentic-footprint/blob/main/collectors/opencode/README.md).

## Troubleshooting

### `af setup` reports OpenCode as not installed

`opencode` is not on `PATH` in the shell that runs `af setup`.

### `af status` reports OpenCode as `not_configured`

The plugin is not in OpenCode's plugin directory. Run
`af setup --agents opencode`. If OpenCode runs with a different
`XDG_CONFIG_HOME` than `af setup`, the plugin went to another directory.

### `af status` reports OpenCode as `misconfigured`

The installed plugin comes from another `af` version. Rerun
`af setup --agents opencode`.

### OpenCode hangs at startup

Look for files other than `agentic-footprint.ts` in the plugin directory,
such as a backup copied there by hand, and move them out.

### No `opencode.*.jsonl` file appears

Check that the plugin is in the directory above, and that OpenCode's
`AF_STATE_DIR` matches the receiver's.
