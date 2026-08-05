#!/bin/sh
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
AF_BIN="$REPO_ROOT/target/debug/af"
STATE_DIR="$(mktemp -d)"
trap 'rm -rf "$STATE_DIR"' EXIT

(cd "$REPO_ROOT" && cargo build -p af-cli -q)
AF_STATE_DIR="$STATE_DIR" "$AF_BIN" collect opencode \
  --session-id ses_fixture \
  --input "$SCRIPT_DIR/test-data/session.sse" \
  --pid 4242 \
  --opencode-version test \
  >"$STATE_DIR/latest"

test "$(cat "$STATE_DIR/latest")" = 10

SPOOL="$STATE_DIR/spool/opencode.ses_fixture.jsonl"
# 5 facts: session_meta, llm_call, bash span, provider-executed mcp span,
# local-unknown mcp span. The fixture's poison tail (a step pair with no
# model, an unparseable frame) must be skipped, never spooled and never
# fatal.
test "$(wc -l <"$SPOOL" | tr -d ' ')" = 5

while IFS= read -r line; do
  printf '%s' "$line" | "$AF_BIN" validate-line >/dev/null
done <"$SPOOL"

jq -e 'select(.type == "llm_call") | .payload.provider == "test-provider" and .payload.usage.cached_read_tokens == 3 and .payload.duration_ms == 1000' "$SPOOL" >/dev/null
jq -e 'select(.type == "action_span" and .payload.span_id == "call_fixture_bash") | .payload.tool_kind == "bash" and .payload.execution_locus == "local" and .payload.pids == [4242]' "$SPOOL" >/dev/null
# provider-executed stays remote: there the event does say so.
jq -e 'select(.type == "action_span" and .payload.span_id == "call_fixture_remote") | .payload.tool_kind == "mcp" and .payload.execution_locus == "remote" and .payload.status == "error" and (.payload | has("pids") | not)' "$SPOOL" >/dev/null
# a plain MCP call is honestly unknown, and unknown loci carry no pids.
jq -e 'select(.type == "action_span" and .payload.span_id == "call_fixture_mcp_local") | .payload.tool_kind == "mcp" and .payload.execution_locus == "unknown" and (.payload | has("pids") | not)' "$SPOOL" >/dev/null

echo "ok - OpenCode collector fixtures validate"
