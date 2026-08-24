# Developer guide

## Bootstrap

```sh
./install.sh --no-setup
cargo test -p af-cli --test cli
```

For development without installing the binary:

```sh
cargo build -p af-cli
export AF_BIN="$PWD/target/debug/af"
export AF_STATE_DIR="$(mktemp -d)"
"$AF_BIN" setup --dry-run
```

Prerequisites used by the full repository include Rust/Cargo, `cargo-audit`,
`jq`, `uv`, and Node tooling for the console. Install the RustSec scanner with
`cargo install cargo-audit --locked`, then run it with `make audit`. Individual
deterministic Rust tests do not need agent credentials or network access.

## Repository map

| Area | Responsibility |
|---|---|
| `crates/af-events` | Contract #1 Rust types and validation |
| `crates/af-spool` | JSONL discovery, incremental tailing, and quarantine |
| `crates/af-otlp` | OTLP HTTP/JSON receiver and source normalizers |
| `crates/af-store` | Raw/derived SQLite persistence and migrations |
| `crates/af-sidecar` | Managed Python environment and JSON-stdio process client |
| `crates/af-core` | Correlation, estimation orchestration, attribution, joins, replay |
| `crates/af-cli` | User commands, setup wizard, resident control plane, debug API |
| `crates/af-console` | Embedded console assets |
| `collectors/` | Agent-native shims, fixtures, and collector-specific docs |
| `python/` | Estimator and sampler sidecars owned by the Rust control plane |
| `console/` | Debug console source and frontend tests |
| `schemas/` | Versioned external event contracts |

See [`api-boundaries.md`](api-boundaries.md) for dependency and public API
rules.

## Canonical commands

```sh
./install.sh                    # install binary and run setup wizard
af setup --dry-run              # inspect integration changes
af watch --debug                # run the resident control plane
af report --format json         # read current derived results
af replay --format json         # rebuild derived data from raw facts
```

Integration-specific installation must flow through `af setup`; collector
READMEs document runtime behavior and troubleshooting, not alternative install
systems.

## Validation order

Start narrow and expand only after focused tests pass:

```sh
cargo test -p af-cli setup -- --nocapture
scripts/test-install.sh
collectors/claude-code/test_hooks.sh
collectors/opencode/test_collector.sh
cargo test -p af-otlp
cargo test -p af-cli
cargo fmt --all -- --check
git diff --check
```

Loopback receiver/debug tests may require sandbox permission. Live agent tests
are manual and documented in [`testing-live.md`](testing-live.md).

## Adding or changing an integration

1. Identify the highest-fidelity public source exposed by the agent.
2. Record raw facts only; estimation belongs in `af-core`/sidecars.
3. Emit Contract #1 envelopes with stable source-derived event IDs.
4. Make replay and restart semantics explicit.
5. Add deterministic fixtures before a live test.
6. Add one `af setup` adapter or setup guidance; do not create a standalone
   installer for one integration.
7. Follow [`error-handling.md`](error-handling.md): collectors preserve the
   agent session, while control-plane configuration/startup failures are fatal.
8. Update the relevant user guide and this documentation index.

## Change discipline

- Preserve unrelated worktree changes; this repository often has concurrent
  work in multiple crates.
- Do not edit raw spool lines or derived state manually during tests.
- Do not add methodology or impact estimation to collectors.
- Keep source-specific parsing inside its collector/normalizer module.
- Archive completed plans instead of leaving dated restart documents in the
  active documentation root.
