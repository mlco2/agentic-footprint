# Uninstall

There is no `af uninstall` command yet. These are the manual steps, in the
order that loses nothing: stop the receiver, restore the agents'
configuration, then remove the program and, if you want, its data.

`<state>` below is the state directory: `~/.local/state/agentic-footprint`
on macOS and Linux, `%LOCALAPPDATA%\agentic-footprint` on Windows, or
`AF_STATE_DIR` if you set it.

## 1. Stop and remove the background receiver

=== "macOS"

    ```sh
    launchctl bootout "gui/$(id -u)/dev.agentic-footprint.af-watch"
    rm ~/Library/LaunchAgents/dev.agentic-footprint.af-watch.plist
    ```

=== "Linux"

    ```sh
    systemctl --user disable --now agentic-footprint-watch.service
    rm ~/.config/systemd/user/agentic-footprint-watch.service
    systemctl --user daemon-reload
    ```

    The unit is under `$XDG_CONFIG_HOME/systemd/user` when that variable is
    set.

=== "Windows"

    ```powershell
    schtasks /End /TN agentic-footprint-af-watch
    schtasks /Delete /TN agentic-footprint-af-watch /F
    ```

If you run `af watch` in a terminal instead, stop it with Ctrl-C.

## 2. Restore the agents' configuration

With the receiver gone, a configured agent keeps trying to export to it. That
loses nothing and costs little, but the configuration should go.

`af setup` saved a copy of each file it changed, beside it, as
`<file>.bak.<timestamp>`. If you have not edited the file since, put the
oldest copy back:

```sh
ls ~/.claude/settings.json.bak.* ~/.codex/config.toml.bak.*
cp ~/.claude/settings.json.bak.<timestamp> ~/.claude/settings.json
```

If you have edited the file since, or there is no copy (the file did not
exist before `af setup`), remove only what `af setup` added.

**Claude Code** (`~/.claude/settings.json`, and `.claude/settings.json` in a
project you set up):

- under `env`: `CLAUDE_CODE_ENABLE_TELEMETRY`, `OTEL_LOGS_EXPORTER`,
  `OTEL_METRICS_EXPORTER`, `OTEL_EXPORTER_OTLP_PROTOCOL`,
  `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_LOGS_EXPORT_INTERVAL`;
- under `hooks`: each entry whose command ends in
  `integrations/claude-code/af-hook.sh` or runs `af hook`.

Leave any of these that you set yourself for another telemetry receiver.

**Codex** (`~/.codex/config.toml`, or `config.toml` in your `CODEX_HOME`): the
`[otel]` table that follows the comment
`# agentic-footprint: native Codex OTLP logs.`

**OpenCode**: the collector plugin
`agentic-footprint.ts` in OpenCode's plugin directory
(`$XDG_CONFIG_HOME/opencode/plugin/`, or `~/.config/opencode/plugin/`), and
any `agentic-footprint.ts.bak.*` copies in the directory above it.

Restart the agents afterwards: a running session keeps the configuration it
started with.

## 3. Remove the program

=== "macOS / Linux"

    ```sh
    rm ~/.local/bin/af
    ```

    Remove the `PATH` line you added to your shell profile, if you added one
    for `af` alone.

=== "Windows"

    From the checkout:

    ```powershell
    .\install.ps1 -Uninstall
    ```

    This removes `af.exe` and its entry in your user `PATH`.

## 4. Keep or remove the data

Everything `af` recorded is in `<state>`. Nothing else on the computer holds
it.

To keep the history and remove the rest, keep the files whose names start
with `state.db` (the recorded events and estimates) and the `spool/` folder
(what the hooks wrote). Everything else in `<state>` can be deleted, `venv/`
(the Python runtime) being the largest. A later install finds the history
again.

To remove everything:

=== "macOS / Linux"

    ```sh
    rm -rf ~/.local/state/agentic-footprint
    ```

=== "Windows"

    ```powershell
    Remove-Item -Recurse -Force "$env:LOCALAPPDATA\agentic-footprint"
    ```

This cannot be undone.
