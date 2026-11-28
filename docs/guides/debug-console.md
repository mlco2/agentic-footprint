# Open the debug console

The debug console is a local web page that shows what `afoot watch` is
receiving and how it attributes it: sessions, events, decisions, allocation
traces, and collector health. This guide opens it on a collector that is
already running, such as the background service.

`afoot console` is available on macOS and Linux. On Windows, see
[Windows and foreground receivers](#windows-and-foreground-receivers).

## Open it

With the background service (or any `afoot watch`) running:

```sh
afoot console
```

It prints the address on stderr and keeps running:

```text
afoot console: http://127.0.0.1:9414/ (Ctrl-C to close; the collector keeps running)
```

Open that address in a browser. Press Ctrl-C when you are done. This stops
the console only; the collector and its receiver are not affected.

`afoot console` starts no receiver and stores nothing. It relays a loopback port
to `console.sock`, a socket in the state directory that only your account can
open.

## What you will see

The collector builds diagnostics only while a console is attached. When you
attach:

- sessions that are active now appear with their last three minutes of
  events;
- older sessions are listed in the session picker and are loaded when you
  select one;
- events from before you attached, beyond those three minutes, are not shown
  as live events. Their results are in the session's report.

Numbers can be up to a minute old, because `afoot watch` writes to the store
once a minute. When you close the console, the collector discards what it had
built for it. See
[why diagnostics are on demand](../contributing/architecture/rebuild-architecture.md#why-diagnostics-are-on-demand).

## Use another port

If `9414` is taken, choose another loopback address:

```sh
afoot console --addr 127.0.0.1:9500
```

Only loopback addresses (`127.0.0.1`, `::1`) are accepted.

## Use the same state directory

`afoot console` finds the collector through the state directory. If the
collector runs with a non-default `AFOOT_STATE_DIR`, set the same value:

```sh
AFOOT_STATE_DIR=/path/to/state afoot console
```

## Windows and foreground receivers

Windows has no console socket. Run the receiver in the foreground with the
debug server enabled:

```powershell
afoot watch --debug
```

Then open `http://127.0.0.1:9414/`. The same works on macOS and Linux when
you run the receiver yourself instead of the service. In this mode
diagnostics are built from startup for as long as the process runs.

Do not start `afoot watch --debug` beside a running service to get a console. It
cannot bind the receiver port the service holds, so it exits and points to
`afoot console`.

## Troubleshooting

### `no collector is running (nothing answers on …/console.sock)`

No `afoot watch` is running for this state directory. Start one with
`afoot service install`, or run `afoot watch` in a terminal. Check with
`afoot service status`.

### `cannot serve the console on 127.0.0.1:9414`

Another `afoot console` or an `afoot watch --debug` already holds the port. Use
that one, or pass `--addr` with another port.

### `afoot watch: no console socket (…)` in the service log

The collector could not create `console.sock`. One cause is a state directory
whose path is too long for a Unix socket (about 100 bytes). The collector
keeps working without the socket; move the state directory to a shorter path
to use `afoot console`. On macOS the service log is under `$AFOOT_STATE_DIR/logs/`;
on Linux use `journalctl --user -u agentic-footprint-watch.service`.

### The collector stops while the console is open

Requests are answered `503` with `collector_not_running` until the collector
is back. `afoot console` keeps trying and attaches again by itself.

The routes behind the page are described in the
[debug API reference](../reference/debug-api.md).
