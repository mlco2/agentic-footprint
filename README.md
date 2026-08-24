# Agentic Footprint

/!\ This project is in alpha /!\

Measure and attribute the environmental impact of coding agents. Agentic
Footprint combines measured local energy, process-level action attribution, and
modeled remote inference impacts while keeping uncertainty and coverage gaps
explicit.

Install from the current source checkout and configure detected coding agents.
On macOS or Linux:

```sh
./install.sh
```

On Windows 11 PowerShell:

```powershell
.\install.ps1
```

The installer places the native binary on the user `PATH`, provisions the
managed CodeCarbon/EcoLogits runtime, and runs the receiver-first setup wizard.
macOS and Linux can install a per-user background receiver; Windows uses a
foreground `af watch` process by default.

- **Documentation:** [docs/index.md](docs/index.md)
- **Contributor onboarding:** [CONTRIBUTING.md](CONTRIBUTING.md)
- **Unified installer and setup wizard:**
  [docs/how-to/installation.md](docs/how-to/installation.md)
- **Claude Code guide:**
  [docs/how-to/claude-code.md](docs/how-to/claude-code.md)
- **Codex guide:** [docs/how-to/codex.md](docs/how-to/codex.md)
- **Architecture and crate boundaries:**
  [docs/contributing/api-boundaries.md](docs/contributing/api-boundaries.md)
- **Event schemas (Contract #1):** [schemas/v0.1/](schemas/v0.1/)
- **License:** [MIT](LICENSE)

Architecture in one line: per-agent **collectors** emit raw facts (tokens, joules,
action spans) to a local JSONL spool → a standard **Rust control plane** owns all
estimation methodology (managed Python env running ecologits/codecarbon) and aggregates
at session/task/tool level → **presentation** layers (statusline, CLI, future
dashboards) consume the read model.

## Documentation

The site is built with Zensical using `mkdocs.yml`:

```sh
scripts/docs.sh build
scripts/docs.sh serve
```

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).
