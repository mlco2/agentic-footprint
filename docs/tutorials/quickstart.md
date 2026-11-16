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
af doctor
```

`af doctor` checks the receiver, the managed CodeCarbon/EcoLogits runtime, and
whether running agents predate their telemetry configuration, and says how to
fix each. On macOS and Linux, `af service status` also checks the installed
per-user service.

## 3. Run an agent session

Start a new Claude Code or Codex session after setup. Agentic Footprint records
usage facts and action spans while the receiver is running.

## 4. Inspect the result

```sh
af report
```

For a live diagnostic view:

- with a macOS/Linux background receiver, run `af console`: it attaches to
  the receiver that is already running and stops when you press Ctrl-C;
- with a foreground receiver, start it as `af watch --debug` instead.

Open `http://127.0.0.1:9414/` in a browser. See
[Open the debug console](../how-to/debug-console.md).

For totals over a day, a week or a month as JSON, run `af summary`.

## Next steps

- [Install and configure](../how-to/installation.md)
- [Claude Code setup](../how-to/claude-code.md)
- [Codex setup](../how-to/codex.md)
- [Known limitations](../reference/known-limitations.md)
- [Understand energy attribution](../explanation/energy-attribution.md)
