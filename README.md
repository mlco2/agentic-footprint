# Agentic Footprint

> **Beta software.** Interfaces, schemas and results may change between
> releases.

Agentic Footprint (`afoot`) measures local machine energy and estimates remote
inference impacts for coding-agent sessions, separating measured local energy
from modeled remote ranges and reporting unestimated calls alongside totals.

## What it measures

- **Local energy, measured.** A CodeCarbon sampler measures the machine's
  energy and attributes it to the agent's processes.
- **Agent actions, attributed.** Tool calls and shell commands are recorded
  as spans, so energy can be broken down by session, task and tool.
- **Remote inference, modeled.** Token counts per model call are turned into
  energy and emission estimates with EcoLogits.

Local measurements and remote estimates are reported in separate fields. Calls
without an estimate remain counted in token totals and are listed under
`models_not_estimated`. See
[energy attribution](docs/contributing/architecture/energy-attribution.md) and
the [known limitations](docs/reference/known-limitations.md).

## Supported agents

| Agent | How `afoot` collects | Guide |
|---|---|---|
| Claude Code | Native OpenTelemetry logs, plus hooks for actions | [Claude Code](docs/agents/claude-code.md) |
| Codex | Native OpenTelemetry logs | [Codex](docs/agents/codex.md) |
| OpenCode | A plugin `afoot setup` installs | [OpenCode](docs/agents/opencode.md) |
| Pi | An extension `afoot setup` installs | [Pi](docs/agents/pi.md) |

`afoot setup` detects which of these are installed and configures them.

## Install

Platforms: macOS (Apple silicon and Intel), Linux (x86-64), Windows 11
(x86-64).

### From a release

Binaries for every platform, with checksums and Sigstore signatures, are on
the [releases page](https://github.com/mlco2/agentic-footprint/releases).
[Verify a release](docs/guides/verify-release.md) explains how to check
one, and [Install and configure](docs/get-started/installation.md#curl-ready-inputs)
how to install a downloaded binary with the installer.

### From source

From a checkout of this repository, on macOS or Linux:

```sh
./install.sh
```

On Windows 11 (PowerShell):

```powershell
.\install.ps1
```

The installer builds `afoot` and puts it in `~/.local/bin` on macOS and Linux
(add that directory to your `PATH` if the installer says it is missing), or
adds it to the user `PATH` on Windows. It then provisions the managed
CodeCarbon/EcoLogits runtime and runs the setup wizard, which checks the
receiver first and can install it as a per-user background service. The
[quickstart](docs/get-started/quickstart.md) lists what a source build needs.

To remove everything, see [Uninstall](docs/get-started/uninstall.md).

## Usage

Once an agent session has run:

```sh
afoot report                 # per-session results
afoot summary --period 7d    # tokens and estimated impacts, as JSON
afoot status                 # what is installed and running, as JSON
afoot doctor                 # why an agent's data is not arriving
afoot console                # debug console of the running receiver (macOS, Linux)
```

`afoot summary` and `afoot status` are read-only and meant for scripts; see
[use afoot from another program](docs/guides/integrate.md) and the
[CLI reference](docs/reference/cli.md).

## How it works

1. **Collectors** emit raw facts (tokens, joules, action spans): agents send
   OpenTelemetry logs to a local receiver, while hooks, plugins, extensions
   and the energy sampler append JSONL to a local spool.
2. **A Rust control plane** (`afoot watch`) ingests both into a local store and
   owns all estimation methodology. It runs EcoLogits and CodeCarbon in a
   managed Python environment and aggregates results by session, task and
   tool.
3. **Presentation layers** (the CLI, the status line, the debug console)
   read the aggregated results.

Facts follow versioned [event schemas](schemas/v0.1/) (Contract #1). More in
the [architecture overview](docs/contributing/architecture/rebuild-architecture.md) and
[crate boundaries](docs/contributing/architecture/api-boundaries.md).

## Documentation

The full documentation starts at [docs/index.md](docs/index.md). The site is
built with Zensical from `mkdocs.yml`:

```sh
scripts/docs.sh build
scripts/docs.sh serve
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). `make help` lists the maintenance
targets; `make check` runs the standard local validation (lint, tests,
repository hygiene).

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
