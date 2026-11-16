# CLI reference

The release binary is named `af`.

| Command | Purpose |
|---|---|
| `af setup` | Check or start the receiver, then detect and configure supported coding agents. |
| `af service install` | Install, start, and verify the macOS/Linux per-user receiver. |
| `af service start` | Restart an installed macOS/Linux receiver. |
| `af service status` | Check the macOS/Linux service manager and OTLP endpoint. |
| `af report` | Ingest pending facts, update derived results, and print a report. |
| `af replay` | Rebuild derived estimates and joins from retained raw facts. |
| `af watch` | Run resident ingestion, sampling, estimation, and optional debugging. |
| `af summary` | Print aggregated tokens and estimated impacts as JSON (read-only). |
| `af console` | Open the debug console of the `af watch` that is already running (macOS/Linux). |
| `af doctor` | Diagnose why an agent's data is not arriving (read-only). |
| `af self update` | Update af to the latest release, with its managed runtime. |
| `af statusline` | Render read-only session impact values for Claude Code. |
| `af hook` | Claude Code hook collector: append one hook event's facts to the spool (registered by `af setup` on Windows). |

Use `af <command> --help` for the authoritative option list from the installed
version.

`af watch` serves the OTLP receiver on `--otlp-addr` (default
`127.0.0.1:4318`) and, with `--debug`, the debug API on `--debug-addr`
(default `127.0.0.1:9414`). Both accept loopback addresses only; any other
address fails at startup with `af listens on loopback addresses only
(127.0.0.1, ::1)`. It writes to the store once a minute, not per received
batch; see
[why writes are batched](../explanation/rebuild-architecture.md#why-writes-are-batched).

`af summary --period today|7d|30d` prints one JSON document: token counts and
estimated remote impacts in total, by agent, by model and per local day. It
opens the store read-only and never ingests, estimates or writes, so it is safe
to run repeatedly from another program; `schemas/summary/v1.schema.json` is its
contract. Impacts are sums over the calls that have an estimate
(`estimated_calls`); `unestimated_by_reason` says why the others have none.

`af console` serves the debug console on `--addr` (default
`127.0.0.1:9414`, loopback only) by relaying to the running `af watch`, which
it reaches over `console.sock`, a socket in the state directory that only your
account can open. It starts no receiver and stores nothing. `af watch` builds
what the console shows only while an `af console` (or a browser tab on it) is
attached, and tells a console that attaches the last three minutes; older
sessions are listed and loaded when selected. Beside a running receiver,
`af watch --debug` exits and points to `af console`.

`af setup` treats receiver health as a prerequisite: it does not inspect or
propose agent-specific changes until the local OTLP receiver responds.

## Supported integrations

The default release configures Claude Code and Codex. Experimental integrations
are excluded from the default build and public setup surface.

## State directory

Set `AF_STATE_DIR` to override the default state location. The directory owns
the spool, the SQLite store (`state.db`, which also holds received OTLP
telemetry and the metadata-only `rejects` log), the managed Python environment,
integration helpers, and resident-service logs. On macOS and Linux it is
restricted to its owner (`0700`).

The default is `~/.local/state/agentic-footprint` on macOS/Linux and
`%LOCALAPPDATA%\agentic-footprint` on Windows. Windows runs `af watch` in the
foreground by default; the `af service` lifecycle is currently available only
through macOS launchd and Linux systemd user services.

## Output contract

Commands that emit JSON keep machine-readable data on stdout and diagnostics
on stderr. A non-zero exit status means the requested control-plane operation
did not complete successfully. Collector hooks are intentionally best-effort
so they cannot fail the coding-agent process they observe.
