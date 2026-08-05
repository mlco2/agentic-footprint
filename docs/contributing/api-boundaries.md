# API and crate boundaries

The workspace is organized by data ownership, not by agent. Agent-specific
code stops at normalization; every later stage consumes shared contracts.

## Dependency direction

```text
collectors / OTLP normalizers
            │
            ▼
        af-events
            │
     ┌──────┴──────┐
     ▼             ▼
 af-spool       af-store
     │             │
     └──────┬──────┘
            ▼
         af-core ◀── af-sidecar
            │
            ▼
          af-cli ─── af-console
```

`af-cli` is the composition root. Lower crates must not depend on CLI modules
or on a specific coding agent.

## Topic ownership

- **Wire facts:** `af-events` and `schemas/`. Adding a fact field starts here.
- **Filesystem transport:** `af-spool`. Filename grammar, tail offsets, and
  rejected-line persistence have one implementation.
- **OTLP transport and source parsing:** `af-otlp`. Normalizers claim records
  through descriptors and return envelope/unclaimed/dropped outcomes.
- **Persistence:** `af-store`. Callers do not issue ad-hoc SQL outside this
  crate.
- **Methodology:** `af-core` and managed sidecars. Collectors never estimate.
- **Process orchestration and UX:** `af-cli`. Setup, watch, reporting, and HTTP
  presentation are binary concerns.

## Public API conventions

- Re-export the supported surface from each crate's `lib.rs`; keep helper
  modules private unless downstream code must name them.
- Use domain nouns for durable values (`Envelope`, `Store`, `SessionTree`) and
  verb functions for operations (`tail`, `correlate`, `serve`).
- Long-running resources use `*Handle`; batch operations return `*Outcome` or
  a typed value.
- Inputs cross crate boundaries as typed Contract #1/domain structs, not
  source-specific JSON. Raw `serde_json::Value` is acceptable only at an
  external protocol boundary or for opaque methodology payloads.
- Source-specific attributes and compatibility fallbacks remain in the source
  normalizer.
- Additive schema compatibility belongs in `af-events`; consumers should not
  independently reinterpret the wire schema.

## Error types by boundary

- `af-events` exposes `RejectReason` because callers branch on validation
  categories.
- `af-store` exposes a typed `Error`/`Result` because SQLite vs JSON failures
  are meaningful to callers.
- `af-spool` uses `std::io::Result` for direct filesystem operations and
  carries rejected-line metadata as values.
- `af-core`, `af-sidecar`, `af-otlp::serve`, and CLI commands use
  `anyhow::Result` where failures combine multiple external systems and are
  ultimately rendered to a human operator.

Do not introduce a workspace-wide error enum merely for uniform syntax. Use a
typed error when callers recover by variant; use contextual `anyhow` at
orchestration boundaries. Runtime behavior is standardized separately in
[`error-handling.md`](error-handling.md).
