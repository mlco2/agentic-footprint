# Debug API reference

`af watch --debug` serves the embedded console and a loopback-only HTTP API.
The default address is `127.0.0.1:9414`.

| Route | Description |
|---|---|
| `GET /debug/sessions` | Bounded list of known sessions. |
| `GET /debug/session?session_id=…` | Current correlated session state. |
| `GET /debug/snapshot?window=180s` | Recent events, decisions, and coverage gaps. |
| `GET /debug/stream?from=N` | Server-sent event stream with cursor replay. |
| `GET /debug/alloc/{sample_event_id}` | Per-sample energy allocation trace. |
| `GET /debug/report` | Latest materialized impact report. |
| `GET /debug/health` | Collector, receiver, sampler, estimator, and rejection health. |

`--debug-addr` must be a loopback address (`127.0.0.1`, `::1`); `af watch`
refuses anything else at startup. The server also rejects non-loopback `Host`
values and only reflects loopback origins for browser access. It is a
diagnostic interface, not a network API.

`/debug/health` lists one row per collector and session (with its transport:
`jsonl spool` or `POST /v1/logs`), the receiver's own counters under
`otlp_receiver` (`logs_requests`, `logs_accepted`, `logs_dropped`,
`logs_unclaimed`, `bodies_quarantined`), and the most recent rejects, which
carry metadata only. Values come from what `af watch` has flushed to the store,
so they can be up to one flush interval (a minute) old; the receiver counters
are live.

Full reports are retained once per bounded session entry. The event ring and
live subscriber queues carry compact invalidations instead of duplicating full
report histories.
