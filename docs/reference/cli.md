# CLI reference

The release binary is named `af`.

| Command | Purpose |
|---|---|
| `af setup` | Detect and configure supported coding agents. |
| `af service install` | Install, start, and verify the resident receiver. |
| `af service start` | Restart an installed resident receiver. |
| `af service status` | Check the service manager and OTLP endpoint. |
| `af report` | Ingest pending facts, update derived results, and print a report. |
| `af replay` | Rebuild derived estimates and joins from retained raw facts. |
| `af watch` | Run resident ingestion, sampling, estimation, and optional debugging. |
| `af python setup` | Provision the managed CodeCarbon and EcoLogits environment. |
| `af python doctor` | Diagnose the managed Python environment. |
| `af statusline` | Render read-only session impact values for Claude Code. |

Use `af <command> --help` for the authoritative option list from the installed
version.

## Supported integrations

The default release configures Claude Code and Codex. Experimental integrations
are excluded from the default build and public setup surface.

## State directory

Set `AF_STATE_DIR` to override the default state location. The directory owns
the spool, SQLite store, managed Python environment, integration helpers,
reject evidence, and resident-service logs.

## Output contract

Commands that emit JSON keep machine-readable data on stdout and diagnostics
on stderr. A non-zero exit status means the requested control-plane operation
did not complete successfully. Collector hooks are intentionally best-effort
so they cannot fail the coding-agent process they observe.
