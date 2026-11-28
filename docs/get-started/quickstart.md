# Quickstart

This tutorial installs Agentic Footprint, connects a supported coding agent,
and opens the local debug console.

## Before you start

This tutorial builds `afoot` from a source checkout; to install a signed release
binary instead, see [Install and configure](installation.md#curl-ready-inputs).

!!! info "Prerequisites for building from source"
    - `git`
    - Rust, via [rustup](https://rustup.rs) (the repository pins its own toolchain, downloaded automatically on first build)
    - Node.js 22 with `npm`, to build the debug console (`install.sh` runs this automatically on macOS and Linux; on Windows source builds, run `npm --prefix console ci && npm --prefix console run build` before `.\install.ps1` if you want the debug UI instead of the placeholder page)
    - [uv](https://docs.astral.sh/uv/getting-started/installation/), which creates the managed Python runtime and fetches Python automatically (without `uv`, a system Python 3.12 with `venv` is used)

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
    runtime, starts the per-user receiver, and configures detected Claude Code,
    Codex, OpenCode, and Pi installations.

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
whether running agents predate their telemetry configuration, and prints the
fix for each issue. `afoot service status` checks the installed per-user service.

!!! tip "Removing `afoot` later"
    See [Uninstall](uninstall.md) for per-platform removal steps and how to keep or delete recorded history.

## 3. Run an agent session

Start a new Claude Code, Codex, OpenCode, or Pi session after setup. Agentic
Footprint records usage facts and action spans while the receiver is running.

## 4. Inspect the result

```sh
afoot report
```

For a live diagnostic view:

- with a macOS/Linux background receiver, run `afoot console`: it attaches to
  the receiver that is already running and stops when you press Ctrl-C;
- with a foreground receiver, start it as `afoot watch --debug` instead.

Open `http://127.0.0.1:9414/` in a browser. See
[Open the debug console](../guides/debug-console.md).

For totals over a day, a week or a month as JSON, run `afoot summary`.

## Next steps

- [Install and configure](installation.md)
- [Claude Code](../agents/claude-code.md) · [Codex](../agents/codex.md) · [OpenCode](../agents/opencode.md) · [Pi](../agents/pi.md)
- [Known limitations](../reference/known-limitations.md)
- [Energy attribution](../contributing/architecture/energy-attribution.md)
