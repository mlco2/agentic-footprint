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
| `af status` | Print what is installed and running (service, receiver, collector, agents) as JSON (read-only). |
| `af summary` | Print aggregated tokens and estimated impacts as JSON (read-only). |
| `af console` | Open the debug console of the `af watch` that is already running (macOS/Linux). |
| `af doctor` | Diagnose why an agent's data is not arriving (read-only). |
| `af self update` | Update a release-installed af to the latest release, with its managed runtime. |
| `af statusline` | Render read-only session impact values for Claude Code. |
| `af hook` | Claude Code hook collector: append one hook event's facts to the spool (registered by `af setup` on Windows). |

Use `af <command> --help` for the authoritative option list from the installed
version.

`af summary`, `af status`, `af doctor`, `af statusline` and `af setup --check`
change nothing. `af setup`, `af service install`, `af service start` and
`af self update` change the machine; see
[Use af from another program](../how-to/integrate.md#commands-that-change-the-machine).

## `af watch`

`af watch` serves the OTLP receiver on `--otlp-addr` (default
`127.0.0.1:4318`) and, with `--debug`, the debug API on `--debug-addr`
(default `127.0.0.1:9414`). Both accept loopback addresses only; any other
address fails at startup with `af listens on loopback addresses only
(127.0.0.1, ::1)`. It writes to the store once a minute, not per received
batch; see
[why writes are batched](../explanation/rebuild-architecture.md#why-writes-are-batched).

## `af summary`

```sh
af summary [--period today|7d|30d] [--utc-offset-minutes N]
```

Prints one JSON document on stdout: token counts and estimated remote impacts
in total, by agent, by model and per local day. It opens the store read-only
and never ingests, estimates or writes, so it is safe to run repeatedly from
another program. It reports what `af watch` had stored when it ran, which can
be up to a minute behind a live session.

| Option | Default | Meaning |
|---|---|---|
| `--period` | `today` | Local calendar days covered, ending today: `today` is 1 day, `7d` is 7, `30d` is 30. |
| `--utc-offset-minutes` | this machine's current offset, or 0 (UTC) when it cannot be read | Minutes east of UTC that a day is cut in. Negative values are accepted. One offset applies to the whole period. |

Impacts are sums over the calls that have an estimate (`estimated_calls`);
`unestimated_by_reason` says why the others have none, and
`models_not_estimated` names the models the estimator has no figures for.

It exits 0 with a document even when the state directory holds no store yet
(every count is then zero). It exits 1, with the reason on stderr, when the
store exists and cannot be read. The fields are described in
[machine-readable output](machine-readable-output.md#af-summary);
`schemas/summary/v1.schema.json` is the contract.

## `af status`

```sh
af status
```

Prints one JSON document on stdout: the af version, whether `af self update`
manages this binary, whether the background service is supported and
installed, whether the receiver answers, whether a collector is running,
whether the managed Python runtime works, and one row per supported agent. It takes no options, starts and installs nothing,
writes nothing, and exits 0. See
[machine-readable output](machine-readable-output.md#af-status) for the
fields.

## `af console`

```sh
af console [--addr 127.0.0.1:9414]
```

Serves the debug console of the `af watch` that is already running, normally
the background service. It relays `--addr` (default `127.0.0.1:9414`,
loopback only) to `console.sock`, a socket in the state directory that only
your account can open. It starts no receiver and stores nothing.

`af watch` builds what the console shows only while an `af console` (or a
browser tab on it) is attached, and tells a console that attaches the last
three minutes; older sessions are listed and loaded when selected.

`af console` runs until you press Ctrl-C; the collector keeps running. It
exits 1 at once when no collector answers on the socket, when `--addr` is
already in use (another `af console`, or an `af watch --debug`), or when
`--addr` is not a loopback address.

It is available on macOS and Linux only. On Windows it exits with an error;
run the collector as `af watch --debug` there. A state directory whose path is
too long for a Unix socket (about 100 bytes) gets no socket: `af watch` prints
`no console socket` with the reason on stderr and carries on, and `af console`
cannot attach to it.

When `af watch --debug` is started beside a running collector that already
holds the receiver port, it exits and points to `af console`. See
[Open the debug console](../how-to/debug-console.md).

## `af setup`

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
did not complete successfully. `af summary` and `af status` print versioned
documents meant for other programs; see
[machine-readable output](machine-readable-output.md). Collector hooks are intentionally best-effort
so they cannot fail the coding-agent process they observe.
