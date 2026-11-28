# Error-handling policy

The project has two different reliability obligations:

1. a collector must not break the coding-agent session it observes;
2. the control plane must not hide configuration, durability, or data-loss
   failures from its operator.

## Diagnostic format

Human diagnostics written to stderr use:

```text
afoot[component] <level>: message
```

Levels are `info`, `warn`, and `error`. Examples:

```text
afoot[opencode] warn: sequence gap for ses_...: expected 4, received 6
afoot[otlp] warn: dropped 2 claimed OTLP record(s) that failed to normalize
afoot[claude-hook] warn: skipped 1 corrupt open-span file
```

Top-level command failures remain `afoot <command>: <context>` and exit non-zero.
Machine-readable data stays on stdout; diagnostics never share stdout with JSON
or cursor output.

## Collector policy

Collectors and receiver request handlers are best-effort relative to the agent:

- malformed individual source events are skipped with a warning;
- claimed records that cannot normalize increment a dropped counter and are
  recorded as a metadata-only reject (see [Rejected data](#rejected-data));
- unrelated records are unclaimed, not errors;
- transient stream failures reconnect with bounded backoff;
- spool append and cursor persistence failures are errors and must not advance
  durable source state;
- hook collectors always exit successfully so they cannot fail the host agent;
  persistent hook errors go to `$AFOOT_STATE_DIR/tmp/hook-errors.log`.

A warning must correspond to an observable counter, reject row, or specific
skipped source item whenever practical.

## Control-plane policy

CLI configuration/startup operations fail fast and return non-zero for:

- invalid command/configuration input;
- conflicting integration configuration;
- inability to bind required listeners;
- inability to open/migrate the store;
- writes whose failure would make setup or persistence ambiguous.

Errors should add operation and path context before reaching `main`. The CLI
adds the command prefix once; lower layers should not duplicate it.

Resident processing degrades only where missing data is an honest state:
missing estimators produce `pending`, unknown execution locations stay
`unknown`, and missing provider correlation stays `unknown`.

## Rejected data

- A malformed spool line or a refused OTLP record is recorded as metadata
  only — when, the source, the reason, attribute names and a size — in the
  store's `rejects` table, a ring of the newest 1,000. The raw line or body is
  never copied anywhere: it is whatever a collector or exporter sent.
- A refused OTLP record never becomes a transport failure for the agent's
  exporter; the request is answered `200` and the reject is counted. The
  exceptions are limits, not content: a body over the size cap gets `413`, as
  does a request with more log records than the receiver normalizes in one go,
  and a full queue or unreadable body gets a retryable `503`.
- OTLP rejects reach the `rejects` table with the next flush of `afoot watch`,
  like the facts beside them; until then they are visible only in the
  receiver's counters (`/debug/health`).
- A reject that cannot be recorded is logged and never costs the accepted
  facts beside it.
- Source files and accepted spool lines are append-only and are never deleted
  by reject handling.

## Testing requirements

Every new failure path should test the applicable behavior:

- exit status for CLI failures;
- no host-agent failure for hook/receiver degradation;
- counter or reject-row evidence for dropped data;
- no cursor advancement before successful spool append;
- stable stdout format when diagnostics are emitted to stderr.
