# Install and setup wizard

`install.sh` is the macOS/Linux bootstrap entry point and `install.ps1` is its
Windows counterpart. Both install the native `af` binary and run the same
receiver-first `af setup` wizard available from the CLI.

## From a source checkout

```sh
./install.sh
```

The shell script builds a release binary, installs it to `~/.local/bin/af`,
provisions the managed Python runtime and embedded sidecar sources, installs
and starts a resident `af watch` user service, verifies its OTLP receiver, then
opens the agent setup prompt. Claude Code is configured in user settings so
new projects work without repeating setup; Codex already uses user
configuration.

Source-checkout installs rebuild the console before compiling Rust so the
binary always embeds the current UI. If `af` already exists at the destination,
the installer shows the installed and candidate versions and asks before
replacing it. Use `--yes` to approve upgrades non-interactively. If another
`af` earlier on `PATH` would hide the installed binary, the installer prints a
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

Release archives are signed and checksummed. Until the canonical repository URL
is published, the installer accepts explicit release inputs:

```sh
curl -LsSf https://example.invalid/install.sh | \
  AF_BINARY_URL=https://example.invalid/releases/af \
  AF_BINARY_SHA256=<sha256> \
  sh -s -- --yes
```

It can also install a locally supplied binary or build a source archive:

```sh
AF_INSTALL_BINARY=/absolute/path/to/af ./install.sh
AF_SOURCE_URL=https://example.invalid/source.tar.gz ./install.sh
```

The shell script only obtains and places the binary. All agent detection and
configuration logic lives in `af setup`, so future curl, Homebrew, npm, and
other distribution methods share exactly the same onboarding behavior.

## Windows 11

From a PowerShell prompt in a source checkout:

```powershell
.\install.ps1
```

For a published release binary, provide its URL and checksum:

```powershell
$env:AF_BINARY_URL = 'https://example.invalid/releases/af-x86_64-pc-windows-msvc.exe'
$env:AF_BINARY_SHA256 = '<sha256>'
.\install.ps1 -Yes
```

The installer upgrades `%LOCALAPPDATA%\Programs\agentic-footprint\af.exe`
atomically and adds that directory to the user `PATH`. Windows runs the
receiver in the foreground by default; start `af watch` in a persistent
PowerShell window, then rerun `af setup` to configure agents.

## Wizard commands

```sh
af setup
af setup --dry-run
af setup --check
af setup --yes
af setup --global
af setup --agents codex,claude-code
af setup --project /absolute/path/to/project
af setup --endpoint http://127.0.0.1:5000/v1/logs
```

The current wizard:

- checks the receiver before inspecting any agent configuration;
- on macOS and Linux, when needed, offers to install and start `af watch`
  through the supported per-user service manager;
- on Windows, prints the foreground `af watch` command instead of installing
  a background task;
- verifies `POST /v1/logs` is answered by this version of the af receiver
  (the `X-Agentic-Footprint` response header) before detecting or proposing
  agent changes;
- detects Codex and Claude Code on `PATH`;
- safely appends Codex native OTLP configuration when no `[otel]` table
  exists, in the primary Codex home and in `<project>/.codex/` when present;
- refuses to overwrite an existing conflicting Codex exporter or invalid TOML;
- reports Claude Code settings in a higher-precedence scope (project,
  `settings.local.json`) that would disable telemetry or hooks, or redirect
  the logs exporter;
- on macOS/Linux, installs the embedded Claude Code shell hook under
  `$AF_STATE_DIR/integrations/`; on Windows, registers the built-in `af hook`
  collector;
- merges Claude hooks and OTLP environment into either the selected project's
  `.claude/settings.json` or, with `--global`, `~/.claude/settings.json`;
- creates timestamped backups before changing existing configuration files;

`--check` exits non-zero when the resident receiver is unreachable, an
installed selected agent needs changes, or a configuration conflict exists.
When the receiver is unavailable, `--dry-run` prints only the receiver plan;
agent configuration is not inspected until receiver health passes.

If you decline the recommended background receiver, setup exits without
inspecting or changing agent configuration. Start the foreground receiver with
the command printed by the wizard, keep it running, then rerun `af setup`.

## Updating

```sh
af self update            # the latest release
af self update 0.3.0      # an exact release
af self update --dry-run  # show what would change
```

`af self update` updates an af that the installer placed from a GitHub release
binary. It checks the download against the release's `SHA256SUMS` (and its
sigstore signature when `cosign` is installed), replaces the binary, refreshes
the managed CodeCarbon/EcoLogits runtime, and restarts the background
receiver. The runtime moves to the newest releases published at least 7 days
ago, and only after they pass a smoke test; otherwise the current runtime is
kept. An af built from a source checkout is updated by pulling and rerunning
`./install.sh`.

## When data does not arrive

```sh
af doctor
```

`af doctor` is read-only. It checks that the receiver answering on the
endpoint is this af, lists running `claude`/`codex` processes that started
before the telemetry configuration they read at startup last changed, and lists
recent sessions that recorded tool activity but no token telemetry. Claude Code
reloads hooks while it runs but not its OTLP exporter settings, so a process
started before setup records tool calls and never token usage: restart it. A
process started before an unrelated edit to the same settings file is reported
too; restarting it is harmless. It exits 1 when anything needs attention.

On macOS and Linux, service commands are idempotent:

```sh
af service install
af service start
af service status
```

On macOS, logs are written under `$AF_STATE_DIR/logs/`. On Linux, inspect them
with `journalctl --user -u agentic-footprint-watch.service`. The background
service deliberately does not use `--debug`; run a separate foreground
`af watch --debug --no-otlp` only when inspecting the debug UI.

If launchd or a systemd user session is unavailable, setup stops before agent
configuration and prints a manual `af watch` command. Keep that process
running and rerun `af setup`; a reachable manually managed receiver satisfies
the setup check.
