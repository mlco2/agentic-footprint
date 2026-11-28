# Codex 0.153.2 OTLP log export (real capture)

The request bodies `codex exec` (codex-cli 0.153.2, default model) POSTed to
`/v1/logs` on 2026-10-16, in arrival order, for a one-tool-call session
(`echo hi`). The exporter was `otlp-http` with `protocol = "json"`;
`Content-Type: application/json`, no compression.

Only these attributes were rewritten, everything else is verbatim:

- `user.email` → `user@example.com`
- `user.account_id` → `00000000-0000-4000-8000-000000000000`
- `host.name` → `host.local`

Unlike `crates/afoot-otlp/tests/fixtures/otlp/logs-codex.json` (hand-shaped),
these show what Codex really sends: `provider_name` is the display name
`"OpenAI"`, `success` is sometimes the string `"true"`, and the first
`response.completed` is a websocket prewarm with `output_token_count = 0`.
