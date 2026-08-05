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

The server rejects non-loopback authority values and only reflects loopback
origins for browser access. It is a diagnostic interface, not a network API.

Full reports are retained once per bounded session entry. The event ring and
live subscriber queues carry compact invalidations instead of duplicating full
report histories.
