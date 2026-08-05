#!/bin/sh
# Runs the live end-to-end suites — the #[ignore]d tests that spawn a real
# coding-agent session against a real `af watch`. Manual invocation only:
# they cost tokens, need the agent CLI installed and logged in, and take
# minutes. A plain `cargo test` (and any future CI default job) keeps
# excluding them.
#
#   scripts/test-live.sh                    # Claude Code, OpenCode, Codex
#   scripts/test-live.sh claude-code        # one agent suite
#   scripts/test-live.sh opencode
#   scripts/test-live.sh codex native_otel  # one suite + test-name filter
#   scripts/test-live.sh smoke              # legacy Claude test-name filter
#
# Knobs (read by the harness, see crates/af-cli/tests/common/live.rs):
#   AF_LIVE_MODEL         Claude Code model alias/id (default: haiku)
#   AF_LIVE_TIMEOUT_SECS  Claude Code per-session wall-clock budget (default: 300)
#   AF_LIVE_OPENCODE_REPO source checkout used instead of installed opencode
#   AF_LIVE_CODEX_MODEL   Codex model id (default: gpt-5.4-mini)
set -eu

cd "$(dirname "$0")/.."

suite="${1:-all}"
if [ "$#" -gt 0 ]; then
    shift
fi

case "$suite" in
    all)
        cargo test -p af-cli --test live_claude_code -- --ignored --nocapture --test-threads=1 "$@"
        cargo test -p af-cli --test live_opencode -- --ignored --nocapture --test-threads=1 "$@"
        exec cargo test -p af-cli --test live_codex -- --ignored --nocapture --test-threads=1 "$@"
        ;;
    claude-code|opencode|codex)
        exec cargo test -p af-cli --test "live_$(printf '%s' "$suite" | tr '-' '_')" \
            -- --ignored --nocapture --test-threads=1 "$@"
        ;;
    *)
        exec cargo test -p af-cli --test live_claude_code \
            -- --ignored --nocapture --test-threads=1 "$suite" "$@"
        ;;
esac
