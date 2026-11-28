# Quickstart

This tutorial installs Agentic Footprint, connects a supported coding agent,
and opens the local debug console.

## Before you start

No release has been published yet, so `afoot` is built from a source checkout.
You need:

- `git`;
- Rust, through [rustup](https://rustup.rs) (the checkout pins its own
  toolchain, which rustup downloads on the first build);
- Node.js 22 with `npm`, to build the debug console (macOS and Linux);
- [uv](https://docs.astral.sh/uv/getting-started/installation/), which builds
  the Python runtime the impact estimates use and brings the right Python
  with it. Without uv, a Python 3.12 with the `venv` module is used.

## 1. Install from a checkout

=== "macOS / Linux"

    ```sh
    git clone https://github.com/mlco2/agentic-footprint.git
    cd agentic-footprint
    ./install.sh --yes
    ```

    `afoot` is installed to `~/.local/bin`. If the installer says that directory
    is not on your `PATH`, add it, then open a new terminal:

    ```sh
    echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc   # or ~/.bashrc
    ```

    The installer builds `afoot`, provisions the managed CodeCarbon and EcoLogits
    runtime, starts the per-user receiver, and configures detected Claude Code
    and Codex installations.

=== "Windows 11"

    ```powershell
    git clone https://github.com/mlco2/agentic-footprint.git
    cd agentic-footprint
    .\install.ps1 -Yes
    ```

    The installer adds `afoot` to your user `PATH` (open a new terminal) and
    registers a scheduled task that starts the receiver when you log on. If
    the task cannot be registered, run `afoot watch` in a PowerShell window you
    keep open, then `afoot setup --yes` in another.

## 2. Verify the receiver

```sh
afoot setup --check
afoot doctor
```

`afoot doctor` checks the receiver, the managed CodeCarbon/EcoLogits runtime, and
whether running agents predate their telemetry configuration, and says how to
fix each. `afoot service status` also checks the installed per-user service.

To remove everything later, see [Uninstall](../how-to/uninstall.md).

## 3. Run an agent session

Start a new Claude Code or Codex session after setup. Agentic Footprint records
usage facts and action spans while the receiver is running.

## 4. Inspect the result

```sh
afoot report
```

For a live diagnostic view:

- with a macOS/Linux background receiver, run `afoot console`: it attaches to
  the receiver that is already running and stops when you press Ctrl-C;
- with a foreground receiver, start it as `afoot watch --debug` instead.

Open `http://127.0.0.1:9414/` in a browser. See
[Open the debug console](../how-to/debug-console.md).

For totals over a day, a week or a month as JSON, run `afoot summary`.

## Next steps

- [Install and configure](../how-to/installation.md)
- [Claude Code setup](../how-to/claude-code.md)
- [Codex setup](../how-to/codex.md)
- [Known limitations](../reference/known-limitations.md)
- [Understand energy attribution](../explanation/energy-attribution.md)
