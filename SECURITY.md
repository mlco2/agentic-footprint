# Security policy

## Supported versions

Until the first stable release, security fixes are applied to the latest
published version only.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting feature for the repository. Do
not open a public issue for an unpatched vulnerability.

Include the affected version, platform, reproduction steps, impact, and any
suggested mitigation. Maintainers will acknowledge a complete report as soon
as practical and coordinate disclosure after a fix is available.

## Scope

Security-sensitive areas include local service installation, agent
configuration changes, loopback HTTP handling, spool and SQLite persistence,
archive/install verification, subprocess supervision, and handling of data
that may contain prompts, tool names, paths, or model metadata.

## Hardening in place

These are the protections a report should be measured against; a way around
any of them is in scope.

- **Loopback listeners.** The OTLP receiver and the `/debug` API have no
  authentication, so `afoot watch` refuses a `--otlp-addr` or `--debug-addr`
  that is not a loopback address (`127.0.0.1`, `::1`) before starting
  anything, and `afoot service install` refuses a non-loopback endpoint. Both
  servers also answer `403` to a request whose `Host` is not loopback (the
  DNS-rebinding guard). The receiver refuses browser-originated requests
  (`Origin`, a cross-site `Sec-Fetch-Site`) and anything but
  `application/json`, and reads at most 4 MiB of a body.
- **Owner-only state.** On macOS and Linux, the state directory is `0700`, so
  other accounts cannot reach anything in it, and the data files afoot writes
  are created owner-only: `afoot watch` and `afoot report` restrict the state
  directory, the SQLite store (and so its WAL and shared-memory
  files) is `0600`, the Claude Code shell hook runs under `umask 077`, the
  native `afoot hook` creates its directories and files owner-only, and the
  launchd and systemd units set a `077` umask for the service and its logs.
  Spool lines carry tool inputs from the session, which is why this matters
  even on a single-user machine.
- **No copies of rejected input.** A malformed spool line or a refused OTLP
  record is kept only as metadata (time, source, reason, attribute names,
  size) in the store's `rejects` table, trimmed to the newest 1,000 rows. The
  rejected content itself is never written anywhere.
- **Isolated Python.** The CodeCarbon and EcoLogits sidecars run as
  `python -I` from a directory afoot owns, so nothing on `PYTHONPATH`, in the
  user site, or in the directory afoot was started from is imported, and a
  project's `.codecarbon.config` is not read. `uv` runs with `--no-config`,
  so a project's `uv.toml` or `[tool.uv]` cannot choose the index the managed
  runtime is installed from; environment variables such as `UV_INDEX_URL`
  still apply.
- **Verified downloads.** `install.sh` and `install.ps1` refuse a URL install
  without the download's SHA-256, and refuse a download that does not match
  it. `afoot self update` checks the release's `SHA256SUMS` and, unless run with
  `--allow-unsigned`, verifies its Sigstore signature against this
  repository's release workflow before replacing the binary.
- **Supply chain.** CI actions are pinned by commit, dependency updates wait
  seven days after a release, and CI audits the Rust, console, and Python
  locks; see `docs/contributing/developer-guide.md`.
