# Quickstart

This tutorial installs Agentic Footprint, connects a supported coding agent,
and opens the local debug console.

## 1. Install from a checkout

```sh
git clone <repository-url>
cd agentic-footprint
./install.sh --yes
```

The installer builds `af`, provisions the managed CodeCarbon and EcoLogits
runtime, starts the resident receiver, and configures detected Claude Code and
Codex installations.

## 2. Verify the receiver

```sh
af service status
af setup --check
af python doctor
```

All three commands should complete successfully. `af python doctor` explains
how to repair a missing or incomplete managed Python environment.

## 3. Run an agent session

Start a new Claude Code or Codex session after setup. Agentic Footprint records
usage facts and action spans in the background.

## 4. Inspect the result

```sh
af report
```

For a live diagnostic view, run a foreground debug process without starting a
second OTLP receiver:

```sh
af watch --debug --no-otlp
```

Open `http://127.0.0.1:9414/` in a browser. The resident service continues to
own collection on `127.0.0.1:4318`; the foreground process only exposes the
debug interface.

## Next steps

- [Install and configure](../how-to/installation.md)
- [Claude Code setup](../how-to/claude-code.md)
- [Codex setup](../how-to/codex.md)
- [Understand energy attribution](../explanation/energy-attribution.md)
