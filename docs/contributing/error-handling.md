# Error-handling policy

The project has two different reliability obligations:

1. a collector must not break the coding-agent session it observes;
2. the control plane must not hide configuration, durability, or data-loss
   failures from its operator.

## Diagnostic format

Human diagnostics written to stderr use:

```text
af[component] <level>: message
```

Levels are `info`, `warn`, and `error`. Examples:

```text
af[opencode] warn: sequence gap for ses_...: expected 4, received 6
af[otlp] error: failed to write spool under ...
af[claude-hook] warn: skipped 1 corrupt open-span file
```

Top-level command failures remain `af <command>: <context>` and exit non-zero.
Machine-readable data stays on stdout; diagnostics never share stdout with JSON
or cursor output.

## Collector policy

Collectors and receiver request handlers are best-effort relative to the agent:

- malformed individual source events are skipped with a warning;
- claimed records that cannot normalize increment a dropped counter and are
  quarantined when the transport permits it;
- unrelated records are unclaimed, not errors;
- transient stream failures reconnect with bounded backoff;
- spool append and cursor persistence failures are errors and must not advance
  durable source state;
- hook collectors always exit successfully so they cannot fail the host agent;
  persistent hook errors go to `$AF_STATE_DIR/tmp/hook-errors.log`.

A warning must correspond to an observable counter, quarantine artifact, or
specific skipped source item whenever practical.

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

## Quarantine and rejected data

- Raw malformed spool lines go under `$AF_STATE_DIR/rejected/` with source and
  reason metadata.
- Malformed or claimed-but-dropped OTLP batches are quarantined without
  returning a transport failure to the agent exporter.
- Quarantine write failures are logged as errors because the evidence itself
  could not be preserved.
- Source files and accepted spool lines are append-only and are never deleted by
  quarantine logic.

## Testing requirements

Every new failure path should test the applicable behavior:

- exit status for CLI failures;
- no host-agent failure for hook/receiver degradation;
- counter or quarantine evidence for dropped data;
- no cursor advancement before successful spool append;
- stable stdout format when diagnostics are emitted to stderr.
