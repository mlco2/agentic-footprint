# Agentic Footprint

<div class="afoot-hero" markdown>
**Agentic Footprint (`afoot`)** measures local machine energy and estimates remote inference impacts for coding-agent sessions. Everything runs locally on your machine—telemetry stays in your local SQLite store.

```sh
./install.sh --yes          # build, set up Python runtime, start receiver, configure agents
afoot summary --period 7d   # view token usage and estimated remote impacts as JSON
```
</div>

!!! info "Beta software"
    Signed release binaries are available on the [GitHub releases page](https://github.com/mlco2/agentic-footprint/releases), or you can build from source. Interfaces and stored schemas may change between releases.

## Explore the documentation

<div class="grid cards" markdown>

-   **Get Started**

    ---

    Install `afoot`, start the per-user background receiver, and record your first measured agent session in minutes.

    - [Quickstart](get-started/quickstart.md)
    - [Install and configure](get-started/installation.md)
    - [Uninstall](get-started/uninstall.md)

-   **Supported Agents**

    ---

    `afoot setup` detects installed agents and configures collection automatically.

    - [Claude Code](agents/claude-code.md) <span class="afoot-badge">OTLP</span><span class="afoot-badge">Hooks</span>
    - [Codex](agents/codex.md) <span class="afoot-badge">OTLP</span>
    - [OpenCode](agents/opencode.md) <span class="afoot-badge">Plugin</span>
    - [Pi](agents/pi.md) <span class="afoot-badge">Extension</span>

-   **Guides & Integration**

    ---

    Inspect live sessions in the local web UI, consume JSON outputs from other apps, or verify signed downloads.

    - [Debug console](guides/debug-console.md)
    - [Programmatic integration](guides/integrate.md)
    - [Verify releases](guides/verify-release.md)

-   **Reference**

    ---

    Command options, versioned JSON schemas, debug HTTP/SSE routes, and current constraints.

    - [CLI reference](reference/cli.md)
    - [Machine-readable output](reference/machine-readable-output.md)
    - [Known limitations](reference/known-limitations.md)

</div>

## What `afoot` reports

| Layer | Source | Output |
|---|---|---|
| **Token usage** | Native agent OTLP logs, plugins, or extensions | Exact counts (`input`, `output`, `thought`, `cache_read`, `cache_write`) |
| **Remote inference impact** | EcoLogits model over token counts | Min–max ranges (`energy`, `gwp`, `water`, `adpe`, `pe`); unestimated models are listed under `models_not_estimated` |
| **Local machine energy** | Shared CodeCarbon sampler + `psutil` | Measured joules attributed to sessions and tool spans (`afoot report`, `afoot console`) |

!!! tip "Integrating with another app?"
    Use `afoot status` and `afoot summary` rather than reading `state.db` directly. Both are read-only and emit versioned single-line JSON. See [Programmatic integration](guides/integrate.md).
