# Quickstart

This tutorial installs Agentic Footprint, connects a supported coding agent,
and opens the local debug console.

## 1. Install from a checkout

=== "macOS / Linux"

    ```sh
    git clone <repository-url>
    cd agentic-footprint
    ./install.sh --yes
    ```

    The installer builds `af`, provisions the managed CodeCarbon and EcoLogits
    runtime, starts the per-user receiver, and configures detected Claude Code
    and Codex installations.

=== "Windows 11"

    ```powershell
    git clone <repository-url>
    cd agentic-footprint
    .\install.ps1 -Yes
    af watch
    ```

    Keep the `af watch` PowerShell window running. In another terminal, run
    `af setup --yes`; agent configuration is not inspected until the receiver
    responds.

## 2. Verify the receiver

```sh
af setup --check
af python doctor
```

On macOS and Linux, `af service status` also checks the installed per-user
service. `af python doctor` explains how to repair a missing or incomplete
managed Python environment.

## 3. Run an agent session

Start a new Claude Code or Codex session after setup. Agentic Footprint records
usage facts and action spans while the receiver is running.

## 4. Inspect the result

```sh
af report
```

For a live diagnostic view:

- with a macOS/Linux background receiver, run `af watch --debug --no-otlp`;
- with a foreground receiver, start it as `af watch --debug` instead.

Open `http://127.0.0.1:9414/` in a browser.

## Next steps

- [Install and configure](../how-to/installation.md)
- [Claude Code setup](../how-to/claude-code.md)
- [Codex setup](../how-to/codex.md)
- [Understand energy attribution](../explanation/energy-attribution.md)
