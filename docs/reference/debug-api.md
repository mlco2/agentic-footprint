# Debug API reference

The debug API is a loopback-only HTTP interface that serves the embedded
debug console and the data it shows. It is a diagnostic interface, not a
network API: it has no authentication and only answers reads.

## Where it is served

| Started as | Served on | Diagnostics built |
|---|---|---|
| `afoot watch --debug` | TCP `--debug-addr`, default `127.0.0.1:9414` | For the life of the process. |
| `afoot watch` (the background service) | `console.sock` in the state directory | Only while a console is attached. |
| `afoot console` | TCP `--addr`, default `127.0.0.1:9414`, relayed to `console.sock` | By the collector it attaches to. |

On macOS and Linux every `afoot watch` serves these routes on `console.sock`,
a Unix socket in the state directory that only its owner can read and write
(mode `0600`, inside a `0700` directory). The background service listens on
no debug TCP port. `afoot console` binds a loopback port and relays each
connection to the socket, byte for byte, so a browser can reach it; see
[Open the debug console](../guides/debug-console.md).

A socket file left by a collector that died is replaced at the next start. A
socket that a running collector answers on is left alone. When the socket
cannot be created, for example because the state directory's path is too
long for a Unix socket (about 100 bytes), `afoot watch` prints `no console
socket` with the reason on stderr and carries on without it. Windows has no
console socket; use `afoot watch --debug` there.

## Routes

| Route | Description |
|---|---|
| `GET /debug/status` | The watcher: boot id, start time, version, receiver, one row per agent. |
| `GET /debug/sessions` | Bounded list of known sessions. |
| `GET /debug/session?session_id=…` | Current correlated session state. |
| `GET /debug/snapshot?window=180s` | Recent events, decisions, and coverage gaps. |
| `GET /debug/stream?from=N` | Server-sent event stream with cursor replay. |
| `GET /debug/alloc/{sample_event_id}` | Per-sample energy allocation trace. |
| `GET /debug/report?session_id=…` | Latest materialized impact report. |
| `GET /debug/health` | Collector, receiver, sampler, estimator, and rejection health. |

Any other path is answered from the embedded console's files. A route with
nothing published yet answers `200` with `null`; an allocation trace that is
not held answers `404`.

## Attaching and detaching

Behind `console.sock`, the collector builds diagnostics on demand. A console
is attached for as long as it holds a `/debug/stream` open. `afoot console`
holds one for its whole life, and each browser tab on it holds another.

When the first stream opens, the collector notices on its next loop, within
about a second, and then:

- refreshes and publishes its status;
- lists every session in the store and sends the list as a `sessions` frame;
- describes each session it is currently tracking, with the facts and traces
  of the last 180 seconds.

Older activity is not replayed. A session that is only listed is loaded when
it is asked for; see [on-demand sessions](#on-demand-sessions).

When the last stream closes, the collector forgets everything it had built
except its status, and stops building. Sequence numbers keep counting, so a
client that reconnects with a cursor from before is answered `reset` and
takes a new snapshot.

With `afoot watch --debug` none of this applies: diagnostics are built from
startup until the process exits.

## On-demand sessions

Behind `afoot console`, a session nothing has described since a console attached
answers `null` on `/debug/session?session_id=…` and
`/debug/report?session_id=…`. Asking is what makes the collector publish it:
its `session` and `report` frames follow on the stream, built from what the
store already holds. Nothing is recomputed or written. At most 32 such
requests are remembered between two collector loops.

Only the console's own pages may trigger this. A request counts when it
carries no `Origin` header, or an `Origin` that is a loopback origin
(`http://localhost`, `http://127.x.x.x` or `http://[::1]`, with any port).
A request with any other `Origin` still gets `null`, and the collector does
no work for it.

## Stream frames

`GET /debug/stream` is a server-sent event stream. `from=N`, or a
`Last-Event-ID` header, resumes after frame `N`.

| Event | Meaning |
|---|---|
| `status` | The `/debug/status` payload. Sent first on every connection, and again when it changes. |
| `sessions` | The whole `/debug/sessions` list, sent when the collector lists or relists its stored sessions. |
| `session` | One session's state. |
| `report` | A notice that a session's report changed; fetch `/debug/report`. |
| `health` | The `/debug/health` payload. |
| `fact`, `decision`, `alloc`, `reject` | Individual events, ingest and attribution decisions, allocation traces, and rejected input. |
| `ping` | Sent after 15 seconds of silence, with the watcher's `boot_id`. |
| `reset` | The cursor cannot be served (too old, or issued by another process). Take a new snapshot. |

The `sessions` frame exists because a page that connects just before the
collector has listed its sessions has already fetched an empty
`/debug/sessions`.

## Watcher status

`GET /debug/status` describes the `afoot watch` process itself. It is available
from startup, before any session exists.

| Field | Meaning |
|---|---|
| `boot_id` | Identifier of this process, different for any other. A client compares it across reconnects to learn that the collector restarted. |
| `started_at` | When the process started. |
| `afoot_version` | Version of the collector. |
| `flush_interval_ms` | How often the collector writes to the store. |
| `receiver` | `endpoint` and `note` of this process's OTLP receiver. |
| `agents` | One row per supported agent, in the shape `afoot status` prints; see [agent rows](machine-readable-output.md#agent-rows). |

Here the agent rows know what the collector has received since it started:
`state` can be `receiving`, and `sessions` and `last_event_at` are filled in.
The collector refreshes agent detection every 30 seconds, and only while
diagnostics are being built.

## Access rules

`--debug-addr` and `afoot console --addr` must be loopback addresses
(`127.0.0.1`, `::1`); anything else is refused at startup. The server also:

- refuses a request whose `Host` header is present and not loopback, with
  `403`;
- sends `Access-Control-Allow-Origin` only for a loopback `Origin`, which it
  reflects;
- answers `405` to any method other than `GET` and `OPTIONS`;
- answers `503` with `too_many_connections` above 32 concurrent requests.

`afoot console` relays at most 64 connections at once and answers `503` with
`too_many_connections` above that. While the collector is not running it
answers `503` with `collector_not_running`, and attaches again when the
collector comes back.

## Health

`/debug/health` lists one row per collector and session (with its transport:
`jsonl spool` or `POST /v1/logs`), the receiver's own counters under
`otlp_receiver` (`logs_requests`, `logs_accepted`, `logs_dropped`,
`logs_unclaimed`, `bodies_quarantined`, and `bodies_timed_out`: requests
given up on because their body did not arrive within ten seconds), and the most recent rejects, which
carry metadata only. Values come from what `afoot watch` has flushed to the store,
so they can be up to one flush interval (a minute) old; the receiver counters
are live.

Full reports are retained once per bounded session entry. The event ring and
live subscriber queues carry compact invalidations instead of duplicating full
report histories.
