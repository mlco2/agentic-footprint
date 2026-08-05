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
