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

The managed runtime is a Python virtual environment under
`$AF_STATE_DIR/venv`, created with `uv` when it is on `PATH` (otherwise with a
matching system Python's `venv` and `pip`) from the pins in
`python/manifest.toml`. `uv` runs with `--no-config`, so a `uv.toml` or
`[tool.uv]` in the directory you install from cannot change where packages come
from. The sidecars then run as `python -I` from a directory af owns, so code or
a `.codecarbon.config` in your project is never picked up.

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

!!! note "No release has been published yet"
    Installing from a source checkout, as above, is the only way to install
    today. The inputs in this section are for release binaries and source
    archives, and have nothing published to point at until the first
    release. See [known limitations](../reference/known-limitations.md).

Release archives will be signed and checksummed. The installer accepts
explicit release inputs:

A URL input always needs its SHA-256 (`AF_BINARY_SHA256`, `AF_SOURCE_SHA256`,
from the release's `SHA256SUMS`); the installer refuses before downloading
anything without it. After the download it compares the file's SHA-256 with
the one supplied, case-insensitively, and stops with both values on a
mismatch, before anything is installed or built. A URL by itself is not a
source of trust: whoever serves it decides what runs next. The checksum is only
as trustworthy as where you copied it from, so take it from the release page,
or check the release first as described in
[Verify a release](verify-release.md). `AF_INSTALL_BINARY` and source-checkout
installs are not checksummed: you already have the file.

```sh
curl -LsSf https://example.invalid/install.sh | \
  AF_BINARY_URL=https://example.invalid/releases/af \
  AF_BINARY_SHA256=<sha256> \
  sh -s -- --yes
```

It can also install a locally supplied binary or build a source archive:

```sh
AF_INSTALL_BINARY=/absolute/path/to/af ./install.sh
AF_SOURCE_URL=https://example.invalid/source.tar.gz AF_SOURCE_SHA256=<sha256> ./install.sh
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

No release has been published yet, so `af self update` has nothing to
install. An af built from a source checkout is updated by pulling and
rerunning `./install.sh`. Once releases exist:

```sh
af self update            # the latest release
af self update X.Y.Z      # an exact release
af self update --dry-run  # show what would change
```

`af self update` updates an af that the installer placed from a GitHub release
binary. It checks the download against the release's `SHA256SUMS` and its
Sigstore signature, which must come from this repository's release workflow
for that tag. The signature is verified inside `af`, with nothing extra to
install. `--allow-unsigned` skips the signature and relies on the checksum
alone, which comes from the same release. It then replaces the binary, refreshes
the managed CodeCarbon/EcoLogits runtime, and restarts the background
receiver. The runtime moves to the newest releases published at least 7 days
ago, and only after they pass a smoke test; otherwise the current runtime is
kept. Run from a source-checkout build, it changes nothing and exits 2.

## When data does not arrive

```sh
af doctor
```

`af doctor` is read-only. It checks that the receiver answering on the
endpoint is this af and that the managed CodeCarbon/EcoLogits runtime works,
lists running `claude`/`codex` processes that started
before the telemetry configuration they read at startup last changed, and lists
recent sessions that recorded tool activity but no token telemetry. Claude Code
reloads hooks while it runs but not its OTLP exporter settings, so a process
started before setup records tool calls and never token usage: restart it. A
process started before an unrelated edit to the same settings file is reported
too; restarting it is harmless. It exits 1 when anything needs attention.
`af watch` writes received telemetry once a minute, so a session that just
started can be reported for a minute before its first token telemetry is
stored.

On macOS and Linux, service commands are idempotent:

```sh
af service install
af service start
af service status
```

On macOS, logs are written under `$AF_STATE_DIR/logs/`. On Linux, inspect them
with `journalctl --user -u agentic-footprint-watch.service`. Both service
definitions set a `077` umask, so the logs and every file the service creates
are readable by your account only. The background
service deliberately does not use `--debug` and listens on no debug port; run
`af console` to open the debug UI on it, for as long as that command runs
(see [Open the debug console](debug-console.md)).

There is no command yet to stop or remove the service, or to revert what
`af setup` wrote; see [known limitations](../reference/known-limitations.md).

If launchd or a systemd user session is unavailable, setup stops before agent
configuration and prints a manual `af watch` command. Keep that process
running and rerun `af setup`; a reachable manually managed receiver satisfies
the setup check.
