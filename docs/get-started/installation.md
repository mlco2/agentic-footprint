# Install and setup wizard

`install.sh` is the macOS/Linux bootstrap entry point and `install.ps1` is its
Windows counterpart. Both install the native `afoot` binary and run the same
receiver-first `afoot setup` wizard available from the CLI.

## From a source checkout

```sh
./install.sh
```

The shell script builds a release binary, installs it to `~/.local/bin/afoot`,
provisions the managed Python runtime and embedded sidecar sources, installs
and starts a resident `afoot watch` user service, verifies its OTLP receiver, then
opens the agent setup prompt. Claude Code is configured in user settings so
new projects work without repeating setup; Codex already uses user
configuration.

The managed runtime is a Python virtual environment under
`$AFOOT_STATE_DIR/venv`, created with `uv` when it is on `PATH` (otherwise with a
matching system Python's `venv` and `pip`) from the pins in
`python/manifest.toml`. `uv` runs with `--no-config`, so a `uv.toml` or
`[tool.uv]` in the directory you install from cannot change where packages come
from. The sidecars then run as `python -I` from a directory afoot owns, so code or
a `.codecarbon.config` in your project is never picked up.

Source-checkout installs rebuild the console before compiling Rust so the
binary always embeds the current UI. If `afoot` already exists at the destination,
the installer shows the installed and candidate versions and asks before
replacing it. Use `--yes` to approve upgrades non-interactively. If another
`afoot` earlier on `PATH` would hide the installed binary, the installer prints a
warning with both paths.

Non-interactive setup:

```sh
./install.sh --yes
```

Install without configuring agents:

```sh
./install.sh --no-setup
```

Skip Python provisioning only when intentionally running without local energy
sampling or remote impact estimation:

```sh
./install.sh --no-python
```

Custom install and project paths:

```sh
./install.sh \
  --bin-dir "$HOME/bin" \
  --project /absolute/path/to/project
```

## Curl-ready inputs

Signed and checksummed release binaries are published on the
[GitHub releases page](https://github.com/mlco2/agentic-footprint/releases).

!!! warning "SHA-256 verification is required for URL installs"
    Both `install.sh` and `install.ps1` refuse a URL download unless you pass its SHA-256 (`AFOOT_BINARY_SHA256` or `AFOOT_SOURCE_SHA256`) from the release's `SHA256SUMS`. On mismatch, installation stops before anything is placed or built. See [Verify a release](../guides/verify-release.md) to verify Sigstore signatures and provenance.

=== "macOS / Linux"

    ```sh
    curl -LsSf https://raw.githubusercontent.com/mlco2/agentic-footprint/main/install.sh | \
      AFOOT_BINARY_URL=https://github.com/mlco2/agentic-footprint/releases/download/vX.Y.Z/afoot-<target> \
      AFOOT_BINARY_SHA256=<sha256> \
      sh -s -- --yes
    ```

    You can also install a local binary or build from a verified source archive:

    ```sh
    AFOOT_INSTALL_BINARY=/absolute/path/to/afoot ./install.sh
    AFOOT_SOURCE_URL=https://github.com/mlco2/agentic-footprint/archive/refs/tags/vX.Y.Z.tar.gz \
      AFOOT_SOURCE_SHA256=<sha256> ./install.sh
    ```

=== "Windows 11"

    From a PowerShell prompt in a source checkout:

    ```powershell
    .\install.ps1
    ```

    For a published release binary, provide its URL and checksum:

    ```powershell
    $env:AFOOT_BINARY_URL = 'https://github.com/mlco2/agentic-footprint/releases/download/vX.Y.Z/afoot-x86_64-pc-windows-msvc.exe'
    $env:AFOOT_BINARY_SHA256 = '<sha256>'
    .\install.ps1 -Yes
    ```

The shell script only obtains and places the binary. All agent detection and
configuration logic lives in `afoot setup`, so future distribution channels share
the same onboarding behavior.

## Windows 11 service notes

The installer upgrades `%LOCALAPPDATA%\Programs\agentic-footprint\afoot.exe`
atomically and adds that directory to the user `PATH`. On Windows the
background receiver is a scheduled task (`agentic-footprint-afoot-watch`) that
starts `afoot watch` when you log on; `afoot setup` offers to register it, and
`afoot service install` does it directly. It is registered for your account
only and needs no administrator rights. This has not yet been verified on a
standard (non-administrator) Windows account; where an organisation's policy
forbids users from creating tasks, registration fails and `afoot` prints the
`afoot watch` command to keep running in a PowerShell window instead.

## Wizard commands

```sh
afoot setup
afoot setup --dry-run
afoot setup --check
afoot setup --yes
afoot setup --global
afoot setup --agents codex,claude-code,opencode,pi
afoot setup --project /absolute/path/to/project
afoot setup --endpoint http://127.0.0.1:5000/v1/logs
```

The current wizard:

- checks the receiver before inspecting any agent configuration;
- when needed, offers to install and start `afoot watch` through the per-user
  service manager (launchd on macOS, a systemd user session on Linux, Task
  Scheduler on Windows);
- where there is none, prints the foreground `afoot watch` command instead;
- verifies `POST /v1/logs` is answered by this version of the afoot receiver
  (the `X-Agentic-Footprint` response header) before detecting or proposing
  agent changes;
- detects Codex, Claude Code, OpenCode, and Pi on `PATH`;
- safely appends Codex native OTLP configuration when no `[otel]` table
  exists, in the primary Codex home and in `<project>/.codex/` when present;
- refuses to overwrite an existing conflicting Codex exporter or invalid TOML;
- reports Claude Code settings in a higher-precedence scope (project,
  `settings.local.json`) that would disable telemetry or hooks, or redirect
  the logs exporter;
- on macOS/Linux, installs the embedded Claude Code shell hook under
  `$AFOOT_STATE_DIR/integrations/`; on Windows, registers the built-in `afoot hook`
  collector;
- merges Claude hooks and OTLP environment into either the selected project's
  `.claude/settings.json` or, with `--global`, `~/.claude/settings.json`;
- installs the OpenCode collector plugin under OpenCode's global `plugin/`
  directory (backing up any previous copy in the parent config directory);
- installs the Pi collector extension under Pi's global `extensions/` directory;
- creates timestamped backups before changing existing configuration files;

`--check` exits non-zero when the resident receiver is unreachable, an
installed selected agent needs changes, or a configuration conflict exists.
When the receiver is unavailable, `--dry-run` prints only the receiver plan;
agent configuration is not inspected until receiver health passes.

If you decline the recommended background receiver, setup exits without
inspecting or changing agent configuration. Start the foreground receiver with
the command printed by the wizard, keep it running, then rerun `afoot setup`.

## Updating

No release has been published yet, so `afoot self update` has nothing to
install. An afoot built from a source checkout is updated by pulling and
rerunning `./install.sh`. Once releases exist:

```sh
afoot self update            # the latest release
afoot self update X.Y.Z      # an exact release
afoot self update --dry-run  # show what would change
```

`afoot self update` updates an afoot that the installer placed from a GitHub release
binary. It checks the download against the release's `SHA256SUMS` and its
Sigstore signature, which must come from this repository's release workflow
for that tag. The signature is verified inside `afoot`, with nothing extra to
install. `--allow-unsigned` skips the signature and relies on the checksum
alone, which comes from the same release. It then replaces the binary, refreshes
the managed CodeCarbon/EcoLogits runtime, and restarts the background
receiver. The runtime moves to the newest releases published at least 7 days
ago, and only after they pass a smoke test; otherwise the current runtime is
kept. Run from a source-checkout build, it changes nothing and exits 2.

## When data does not arrive

```sh
afoot doctor
```

`afoot doctor` is read-only. It checks that the receiver answering on the
endpoint is this afoot and that the managed CodeCarbon/EcoLogits runtime works,
lists running `claude`/`codex` processes that started
before the telemetry configuration they read at startup last changed, and lists
recent sessions that recorded tool activity but no token telemetry. Claude Code
reloads hooks while it runs but not its OTLP exporter settings, so a process
started before setup records tool calls and never token usage: restart it. A
process started before an unrelated edit to the same settings file is reported
too; restarting it is harmless. It exits 1 when anything needs attention.
`afoot watch` writes received telemetry once a minute, so a session that just
started can be reported for a minute before its first token telemetry is
stored.

Service commands are idempotent across macOS, Linux, and Windows:

```sh
afoot service install
afoot service start
afoot service status
```

On macOS, logs are written under `$AFOOT_STATE_DIR/logs/`. On Linux, inspect them
with `journalctl --user -u agentic-footprint-watch.service`. Both service
definitions set a `077` umask, so the logs and every file the service creates
are readable by your account only. The background
service deliberately does not use `--debug` and listens on no debug port; run
`afoot console` to open the debug UI on it, for as long as that command runs
(see [Open the debug console](../guides/debug-console.md)).

There is no command yet to stop or remove the service, or to revert what
`afoot setup` wrote; see [known limitations](../reference/known-limitations.md).

If launchd or a systemd user session is unavailable, setup stops before agent
configuration and prints a manual `afoot watch` command. Keep that process
running and rerun `afoot setup`; a reachable manually managed receiver satisfies
the setup check.
