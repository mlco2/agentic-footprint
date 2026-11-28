# API and crate boundaries

The workspace is organized by data ownership, not by agent. Agent-specific
code stops at normalization; every later stage consumes shared contracts.

## Dependency direction

```text
collectors / OTLP normalizers
            │
            ▼
        afoot-events
            │
     ┌──────┴──────┐
     ▼             ▼
 afoot-spool       afoot-store
     │             │
     └──────┬──────┘
            ▼
         afoot-core ◀── afoot-sidecar
            │
            ▼
          afoot-cli ─── afoot-console
```

`afoot-cli` is the composition root. Lower crates must not depend on CLI modules
or on a specific coding agent.

## Topic ownership

- **Wire facts:** `afoot-events` and `schemas/`. Adding a fact field starts here.
- **Filesystem transport:** `afoot-spool`. Filename grammar and tail offsets
  have one implementation; lines that fail to parse are handed back to the
  caller, not stored.
- **OTLP transport and source parsing:** `afoot-otlp`. Normalizers claim records
  through descriptors and return envelope/unclaimed/dropped outcomes. The
  server writes nothing: it sends normalized events and metadata-only rejects
  over a bounded channel to its owner (`afoot watch`), which stores them.
- **Persistence:** `afoot-store`. Callers do not issue ad-hoc SQL outside this
  crate. This includes the `rejects` table, the one place rejected input is
  recorded, as metadata only.
- **Methodology:** `afoot-core` and managed sidecars. Collectors never estimate.
- **Process orchestration and UX:** `afoot-cli`. Setup, watch, reporting, and HTTP
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
- Additive schema compatibility belongs in `afoot-events`; consumers should not
  independently reinterpret the wire schema.

## Error types by boundary

- `afoot-events` exposes `RejectReason` because callers branch on validation
  categories.
- `afoot-store` exposes a typed `Error`/`Result` because SQLite vs JSON failures
  are meaningful to callers.
- `afoot-spool` uses `std::io::Result` for direct filesystem operations and
  returns rejected lines as values (`RejectedLine`); the caller keeps only
  their metadata.
- `afoot-core`, `afoot-sidecar`, `afoot-otlp::serve`, and CLI commands use
  `anyhow::Result` where failures combine multiple external systems and are
  ultimately rendered to a human operator.

Do not introduce a workspace-wide error enum merely for uniform syntax. Use a
typed error when callers recover by variant; use contextual `anyhow` at
orchestration boundaries. Runtime behavior is standardized separately in
[`error-handling.md`](error-handling.md).
