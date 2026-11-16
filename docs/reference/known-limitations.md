# Known limitations

Agentic Footprint is alpha software. Commands and stored data can change
between versions; the JSON documents for other programs follow the
[versioning rule](machine-readable-output.md#versioning). This page lists
what af does not do yet.

## No release has been published yet

There is no published release. Until there is one:

- the only way to install is from a source checkout, with `./install.sh` or
  `.\install.ps1`;
- `af self update` cannot be used. It updates only an af that the installer
  placed from a release binary, and exits 2 with an explanation for a build
  from a checkout. Update a checkout build by pulling and rerunning the
  installer;
- the release inputs of the installer (`AF_BINARY_URL`, `AF_BINARY_SHA256`)
  and the steps in [Verify a release](../how-to/verify-release.md) have no
  published artifact to point at.

The [release process](release-process.md) describes how releases will be
produced.

## Impacts are modelled, not measured

The impact figures for model calls (energy, greenhouse gas emissions, water,
abiotic resource depletion, primary energy) are estimates of remote
inference from EcoLogits. Each is a range with a minimum and a maximum. af
does not measure them.

Local machine energy is measured by sampling, then attributed to sessions
and actions; see [energy attribution](../explanation/energy-attribution.md).
`af summary` carries the remote estimates only.

## Unknown models have no impact figure

A model the estimator does not know gets no impact estimate. Its calls are
stored with the status `unknown_model`:

- their tokens are counted in every total;
- their impacts are in no sum, so a total that includes such calls is a lower
  bound;
- `af summary` reports how many under `unestimated_by_reason` and names the
  models under `models_not_estimated`.

A call can also be `pending` for a short time, until the estimator has
processed it. When no estimator is available (`af watch --no-sidecars`, or no
managed Python runtime), calls stay `pending`.

## The background service and `af console` are macOS and Linux only

`af service` uses launchd on macOS and a systemd user session on Linux.
`af console` needs a Unix socket. On Windows, run `af watch` in the
foreground and keep it running; use `af watch --debug` for the debug console.
On Linux without a systemd user session the service is unavailable too, and
`af watch` runs in the foreground.

A state directory whose path is too long for a Unix socket (about 100 bytes)
gets no console socket. The collector works; `af console` cannot attach to
it.

## No command to stop, uninstall or revert

`af service` can install, start and report on the background service. There
is no command yet to stop it or remove it. There is also no command to revert
what `af setup` wrote to agent configuration files. `af setup` makes a
timestamped backup (`<file>.bak.<timestamp>`) beside each existing file
before changing it; restoring one is a manual step.

## Daily totals use one UTC offset

`af summary` cuts local days with a single UTC offset for the whole period:
the machine's offset when the command runs, or `--utc-offset-minutes`. When a
daylight-saving change falls inside the period, the days before the change
are cut an hour away from local midnight. Calls within an hour of midnight on
those days can be counted in the neighbouring day, and the period then starts
an hour away from local midnight.

## `af status` does not know whether data is arriving

`af status` reads configuration and the process table. It reports a
configured agent as `waiting` whether or not its events are being received.
The running collector knows, and says so in the debug console and on
`GET /debug/status`. `af summary` shows the last stored event under
`collector.last_event_at`.

## Results lag by up to a minute

`af watch` writes to the store once a minute. `af summary`, `af report`, the
status line and the debug console can be that far behind a live session; see
[why writes are batched](../explanation/rebuild-architecture.md#why-writes-are-batched).

## Supported agents

The default build configures Claude Code and Codex. Other integrations are
experimental and are not part of the default binary.
