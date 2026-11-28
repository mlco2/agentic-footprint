# Known limitations

Agentic Footprint is in beta. Commands and stored data can still change
between versions; the JSON documents for other programs follow the
[versioning rule](machine-readable-output.md#versioning). This page lists
what `afoot` does not do yet.

## Impacts are modelled, not measured

The impact figures for model calls (energy, greenhouse gas emissions, water,
abiotic resource depletion, primary energy) are estimates of remote
inference from EcoLogits. Each is a range with a minimum and a maximum. `afoot`
does not measure them.

Local machine energy is measured by sampling, then attributed to sessions
and actions; see [energy attribution](../contributing/architecture/energy-attribution.md).
`afoot summary` carries the remote estimates only.

## Unknown models have no impact figure

A model the estimator does not know gets no impact estimate. Its calls are
stored with the status `unknown_model`:

- their tokens are counted in every total;
- their impacts are in no sum, so a total that includes such calls is a lower
  bound;
- `afoot summary` reports how many under `unestimated_by_reason` and names the
  models under `models_not_estimated`.

A call can also be `pending` for a short time, until the estimator has
processed it. When no estimator is available (`afoot watch --no-sidecars`, or no
managed Python runtime), calls stay `pending`.

## `afoot console` is macOS and Linux only

`afoot service` uses launchd on macOS, a systemd user session on Linux and a
Task Scheduler logon task on Windows. On Linux without a systemd user
session the service is unavailable, and `afoot watch` runs in the foreground.

The Windows task is new and has not yet been verified on a standard
(non-administrator) account. A console window may appear briefly at logon
before `afoot watch` gives it up. Task Scheduler stops the task by terminating
the process, without the orderly shutdown the other platforms get. The
service writes to `logs\watch.log` in the state directory, which is not
rotated.

`afoot console` needs a Unix socket, so it does not exist on Windows; stop the
task and run `afoot watch --debug` there for the debug console.

A state directory whose path is too long for a Unix socket (about 100 bytes)
gets no console socket. The collector works; `afoot console` cannot attach to
it.

## History is kept without limit

`afoot` keeps every recorded event, and the spool files they came from. There is
no retention, archive or compaction yet. `afoot status` reports what they take
under `storage`; [Uninstall](../get-started/uninstall.md#4-keep-or-remove-the-data)
says which files hold the history.

## `receiving` in `afoot status` covers the last 24 hours

`afoot status` reads the store, not the running collector: an agent is
`receiving` when it has a stored event in the last 24 hours. An agent that
stopped sending within that time is still reported as `receiving`. The
running collector reports what it has received since it started, in the
debug console and on `GET /debug/status`.

## Results lag by up to a minute

`afoot watch` writes to the store once a minute. `afoot summary`, `afoot report`, the
status line and the debug console can be that far behind a live session; see
[why writes are batched](../contributing/architecture/rebuild-architecture.md#why-writes-are-batched).

## Supported agents

The default build configures Claude Code, Codex,
[OpenCode](../agents/opencode.md) and [Pi](../agents/pi.md).

- **Local process-tree attribution (L2):** only Claude Code currently emits
  session root PIDs for CPU-weighted local attribution. Codex, OpenCode, and Pi
  record remote inference and action spans, and fall back to wall-clock/baseline
  local attribution.
- **Pi compaction:** Pi's context compaction and branch summaries call the model
  directly, without the `message_end` event the collector listens to, so their
  tokens are not collected. Pi sessions that compact are undercounted.
