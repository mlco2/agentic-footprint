# Event schema reference

Collector facts use Contract #1 schemas under [`schemas/v0.1/`](../../schemas/v0.1/).

Every event has a stable envelope containing an event identifier, timestamp,
session identifier, source metadata, and one typed payload. Current payloads
include session metadata, LLM calls, action spans, machine energy samples, and
per-session process samples.

Collectors preserve source facts. Estimation, correlation, and environmental
impact calculation happen in the Rust control plane after ingestion.

Schema changes must remain additive within a version. Incompatible changes
require a new schema version and migration guidance.
