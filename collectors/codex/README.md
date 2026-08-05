# Codex collector

For the complete copy-paste setup, verification, privacy, and troubleshooting
procedure, see
[`docs/codex-opencode-user-guide.md`](../../docs/codex-opencode-user-guide.md).

Codex uses the existing `af watch` OTLP receiver; no wrapper or app-server
proxy is required for ordinary CLI/TUI sessions.

Enable native Codex log export to the receiver:

```sh
af setup --agents codex
```

The wizard backs up and updates the effective `$CODEX_HOME/config.toml` when
there is no conflicting OTEL configuration. The resulting table is:

```toml
[otel]
environment = "dev"
log_user_prompt = false
metrics_exporter = "none"
trace_exporter = "none"
exporter = { otlp-http = {
  endpoint = "http://127.0.0.1:4318/v1/logs",
  protocol = "json"
} }
```

The `otlp-codex` normalizer maps:

- `codex.conversation_starts` → `session_meta`;
- token-bearing `codex.sse_event` records whose kind is
  `response.completed` → `llm_call`;
- `codex.tool_result` → `action_span`.

The provider on each `llm_call` is the `provider_name` that
`conversation_starts` declared for that conversation (Codex is
provider-configurable via `model_providers`, so it is never assumed). A
`response.completed` for a conversation whose start this receiver never saw
gets `provider: "unknown"` — if the start was missed, the receiver was not
running, and guessing would be dishonest. Start `af watch` before the Codex
session.

MCP tool calls get `execution_locus: "unknown"`, matching the Claude Code
hook collector's honest choice: an MCP server is as likely a local process
as a remote service, and the event does not say which.

Codex 0.142.0 emits a second duration-only `response.completed` record at the
same timestamp. It is intentionally ignored to avoid double-counting one
provider response. Tool start time is reconstructed from the completion
timestamp and native `duration_ms`.

App-server v2 remains the higher-fidelity control protocol for products that
already embed Codex, but it is experimental and is not required by this
collector.

Receiver degradation and control-plane failures follow the shared
[`error-handling policy`](../../docs/error-handling.md).
