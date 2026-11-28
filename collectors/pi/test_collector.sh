#!/bin/sh
# Pi collector checks: unit tests, then a fixture session replayed through the
# extension, every spooled line validated by afoot, and the spool ingested into
# the store and summarised as Pi.
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
AFOOT_BIN="$REPO_ROOT/target/debug/afoot"
STATE_DIR="$(mktemp -d)"
trap 'rm -rf "$STATE_DIR"' EXIT

(cd "$REPO_ROOT" && cargo build -p afoot-cli -q)

# Node >= 23.6 runs the TypeScript directly; there is no build step.
node --test "$SCRIPT_DIR/afoot-collector.test.ts" >/dev/null

AFOOT_STATE_DIR="$STATE_DIR" node "$SCRIPT_DIR/test-data/replay.ts" \
  "$SCRIPT_DIR/test-data/session.jsonl" ses_fixture

SPOOL="$STATE_DIR/spool/pi.ses_fixture.jsonl"
# 6 facts: session_meta, 2 llm_call (the copies in turn_end/agent_end must not
# count), 3 action_span (two parallel tools finishing out of order, one still
# open at shutdown → cancelled). The poison lines (assistant message with no
# model, a tool end with no start) must be skipped.
test "$(wc -l <"$SPOOL" | tr -d ' ')" = 6

while IFS= read -r line; do
  printf '%s' "$line" | "$AFOOT_BIN" validate-line >/dev/null
done <"$SPOOL"

AFOOT_STATE_DIR="$STATE_DIR" "$AFOOT_BIN" report --format text 2>"$STATE_DIR/report.err" >/dev/null
grep -q "ingested 6 new event(s) from 1 spool file(s), 0 rejected" "$STATE_DIR/report.err"

AFOOT_STATE_DIR="$STATE_DIR" "$AFOOT_BIN" summary --period 30d >"$STATE_DIR/summary.json"
node -e '
const s = JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8"));
const pi = s.by_agent.find((a) => a.agent === "pi");
if (!pi || pi.calls !== 2 || pi.tokens.input !== 2700 || pi.tokens.output !== 125) {
  console.error("unexpected summary:", JSON.stringify(s.by_agent));
  process.exit(1);
}
' "$STATE_DIR/summary.json"

echo "ok - Pi collector fixtures validate, ingest and summarise as Pi"
