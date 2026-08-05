#!/bin/sh
# collectors/claude-code/test_hooks.sh
#
# Plain-sh test runner for af-hook.sh (no bats, matching this repo's shell
# tooling budget). Pipes recorded/synthetic Claude Code hook payloads
# (test-data/*.json — see that directory's provenance notes: `.real.json`
# fixtures are sanitized captures of a real headless Claude Code session,
# `.synthetic.json` fixtures are handwritten from documented hook fields
# for tool kinds the captured session didn't exercise) through the shim
# into a fresh AF_STATE_DIR per case, validates every resulting spool line
# via `af validate-line` (crates/af-cli's hidden helper subcommand), and
# asserts the shapes the Task 9 brief describes.
#
# Wired into CI as a step after `cargo test --workspace`
# (.github/workflows/ci.yml).

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOOK="$SCRIPT_DIR/af-hook.sh"
DATA="$SCRIPT_DIR/test-data"
AF_BIN="$REPO_ROOT/target/debug/af"

CASES=0
FAILURES=0

pass() {
  CASES=$((CASES + 1))
  printf 'ok - %s\n' "$1"
}

fail() {
  CASES=$((CASES + 1))
  FAILURES=$((FAILURES + 1))
  printf 'not ok - %s\n' "$1"
}

echo "building af-cli..."
(cd "$REPO_ROOT" && cargo build -p af-cli -q)

# --- helpers ------------------------------------------------------------

validate_line() {
  # $1 = line; exit code mirrors `af validate-line`
  printf '%s' "$1" | "$AF_BIN" validate-line >/dev/null 2>&1
}

run_hook() {
  # $1 = state dir, $2 = payload file
  AF_STATE_DIR="$1" "$HOOK" <"$2"
}

spool_file_for() {
  # $1 = state dir, $2 = payload file (session_id read out of it)
  session_id="$(jq -r '.session_id' "$2")"
  printf '%s/spool/cc-hooks.%s.jsonl\n' "$1" "$session_id"
}

# Open-span scratch files live under tmp/openspans/<session_id>/, one
# directory per session — see af-hook.sh. Tests must address them the same
# way, or a shim that regressed to a flat directory would still pass.
openspan_dir_for() {
  # $1 = state dir, $2 = payload file (session_id read out of it)
  session_id="$(jq -r '.session_id' "$2")"
  printf '%s/tmp/openspans/%s\n' "$1" "$session_id"
}

openspan_file_for() {
  # $1 = state dir, $2 = payload file, $3 = tool_use_id
  printf '%s/%s\n' "$(openspan_dir_for "$1" "$2")" "$3"
}

line_count() {
  # $1 = file, 0 if it doesn't exist
  if [ -f "$1" ]; then
    wc -l <"$1" | tr -d ' '
  else
    echo 0
  fi
}

# True (exit 0) if $1 <= $2, comparing RFC3339 Z-normalized timestamp
# strings lexicographically, the same assumption `af report` relies on.
ts_le() {
  first="$(printf '%s\n%s\n' "$1" "$2" | sort | head -n1)"
  [ "$first" = "$1" ]
}

ts_lt() {
  [ "$1" != "$2" ] && ts_le "$1" "$2"
}

# True (exit 0) if $1 is an RFC3339 UTC timestamp with exactly three
# fractional digits — the millisecond precision af-hook.sh promises. This
# is a *format* assertion on purpose: asserting that any particular pair of
# timestamps differs by a sub-second amount would be timing-dependent and
# flaky, while the format is deterministic.
is_ms_ts() {
  printf '%s' "$1" |
    grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z$'
}

# Runs a PreToolUse/PostToolUse pair (one second apart, so t_start/t_end are
# distinct without relying on how long the shim itself takes) and asserts
# the resulting single action_span's tool_kind/execution_locus and schema
# validity.
assert_pair_kind_locus() {
  # $1=pre file $2=post file $3=expected tool_kind $4=expected locus $5=label
  state="$(mktemp -d)"
  run_hook "$state" "$1"
  sleep 1
  run_hook "$state" "$2"
  spool="$(spool_file_for "$state" "$2")"

  if [ -f "$spool" ]; then
    kind="$(jq -r '.payload.tool_kind' "$spool")"
    locus="$(jq -r '.payload.execution_locus' "$spool")"
  else
    kind="MISSING"
    locus="MISSING"
  fi

  if [ "$kind" = "$3" ] && [ "$locus" = "$4" ]; then
    pass "$5: tool_kind=$3 execution_locus=$4"
  else
    fail "$5: tool_kind/execution_locus (got $kind/$locus, want $3/$4)"
  fi

  if [ -f "$spool" ] && validate_line "$(cat "$spool")"; then
    pass "$5: emitted line validates"
  else
    fail "$5: emitted line validates"
  fi

  rm -rf "$state"
}

# --- Case: SessionStart emits exactly 2 valid lines ----------------------

state1="$(mktemp -d)"
run_hook "$state1" "$DATA/session_start.real.json"
spool1="$(spool_file_for "$state1" "$DATA/session_start.real.json")"

n1="$(line_count "$spool1")"
if [ "$n1" = "2" ]; then
  pass "SessionStart writes exactly 2 lines"
else
  fail "SessionStart writes exactly 2 lines (got $n1)"
fi

if [ -f "$spool1" ]; then
  all_valid=1
  while IFS= read -r line; do
    validate_line "$line" || all_valid=0
  done <"$spool1"
  if [ "$all_valid" = "1" ]; then
    pass "SessionStart lines validate"
  else
    fail "SessionStart lines validate"
  fi

  types="$(jq -r '.type' "$spool1" | sort | tr '\n' ',')"
  if [ "$types" = "action_span,session_meta," ]; then
    pass "SessionStart emits bootstrap action_span + session_meta"
  else
    fail "SessionStart emits bootstrap action_span + session_meta (got: $types)"
  fi

  bad_ts1=0
  for t1 in $(jq -r '.ts' "$spool1"); do
    is_ms_ts "$t1" || bad_ts1=1
  done
  if [ "$bad_ts1" = "0" ]; then
    pass "SessionStart: every envelope ts has millisecond precision"
  else
    fail "SessionStart: every envelope ts has millisecond precision"
  fi
else
  fail "SessionStart lines validate"
  fail "SessionStart emits bootstrap action_span + session_meta"
  fail "SessionStart: every envelope ts has millisecond precision"
fi
rm -rf "$state1"

# --- Case: Pre+PostToolUse (Bash) -> exactly 1 action_span ---------------

state2="$(mktemp -d)"
run_hook "$state2" "$DATA/pretooluse_bash.real.json"
sleep 1
run_hook "$state2" "$DATA/posttooluse_bash.real.json"
spool2="$(spool_file_for "$state2" "$DATA/posttooluse_bash.real.json")"

n2="$(line_count "$spool2")"
if [ "$n2" = "1" ]; then
  pass "Pre+PostToolUse writes exactly 1 line"
else
  fail "Pre+PostToolUse writes exactly 1 line (got $n2)"
fi

if [ -f "$spool2" ] && validate_line "$(cat "$spool2")"; then
  pass "Pre+PostToolUse line validates"
else
  fail "Pre+PostToolUse line validates"
fi

if [ -f "$spool2" ]; then
  span_id2="$(jq -r '.payload.span_id' "$spool2")"
  expected_id2="$(jq -r '.tool_use_id' "$DATA/posttooluse_bash.real.json")"
  if [ "$span_id2" = "$expected_id2" ]; then
    pass "Pre+PostToolUse span_id == tool_use_id"
  else
    fail "Pre+PostToolUse span_id == tool_use_id (got $span_id2, want $expected_id2)"
  fi

  kind2="$(jq -r '.payload.tool_kind' "$spool2")"
  if [ "$kind2" = "bash" ]; then
    pass "Pre+PostToolUse tool_kind == bash"
  else
    fail "Pre+PostToolUse tool_kind == bash (got $kind2)"
  fi

  status2="$(jq -r '.payload.status' "$spool2")"
  if [ "$status2" = "ok" ]; then
    pass "Pre+PostToolUse status == ok"
  else
    fail "Pre+PostToolUse status == ok (got $status2)"
  fi

  t_start2="$(jq -r '.payload.t_start' "$spool2")"
  t_end2="$(jq -r '.payload.t_end' "$spool2")"
  if ts_lt "$t_start2" "$t_end2"; then
    pass "Pre+PostToolUse t_start < t_end"
  else
    fail "Pre+PostToolUse t_start < t_end (got $t_start2 / $t_end2)"
  fi

  # Sub-second granularity: with millisecond timestamps a Pre/Post pair
  # *can* differ inside one wall-clock second, which is what lets the
  # attribution join give a fast tool call a nonzero duration. Asserting a
  # specific sub-second delta would be flaky, so this asserts the format
  # that makes it representable.
  if is_ms_ts "$t_start2" && is_ms_ts "$t_end2"; then
    pass "Pre+PostToolUse t_start/t_end have millisecond precision"
  else
    fail "Pre+PostToolUse t_start/t_end have millisecond precision (got $t_start2 / $t_end2)"
  fi

  attr2="$(jq -r '.attribution.tool_call_id' "$spool2")"
  if [ "$attr2" = "$expected_id2" ]; then
    pass "Pre+PostToolUse attribution.tool_call_id == tool_use_id"
  else
    fail "Pre+PostToolUse attribution.tool_call_id == tool_use_id (got $attr2)"
  fi
else
  fail "Pre+PostToolUse span_id == tool_use_id"
  fail "Pre+PostToolUse tool_kind == bash"
  fail "Pre+PostToolUse status == ok"
  fail "Pre+PostToolUse t_start < t_end"
  fail "Pre+PostToolUse t_start/t_end have millisecond precision"
  fail "Pre+PostToolUse attribution.tool_call_id == tool_use_id"
fi
rm -rf "$state2"

# --- Case: PostToolUse without a matching PreToolUse -> status unknown ---

state3="$(mktemp -d)"
run_hook "$state3" "$DATA/posttooluse_edit.synthetic.json"
spool3="$(spool_file_for "$state3" "$DATA/posttooluse_edit.synthetic.json")"

if [ -f "$spool3" ]; then
  status3="$(jq -r '.payload.status' "$spool3")"
  if [ "$status3" = "unknown" ]; then
    pass "PostToolUse without PreToolUse: status == unknown"
  else
    fail "PostToolUse without PreToolUse: status == unknown (got $status3)"
  fi

  t_start3="$(jq -r '.payload.t_start' "$spool3")"
  t_end3="$(jq -r '.payload.t_end' "$spool3")"
  if [ "$t_start3" = "$t_end3" ]; then
    pass "PostToolUse without PreToolUse: t_start == t_end"
  else
    fail "PostToolUse without PreToolUse: t_start == t_end (got $t_start3 / $t_end3)"
  fi

  if validate_line "$(cat "$spool3")"; then
    pass "PostToolUse without PreToolUse: line validates"
  else
    fail "PostToolUse without PreToolUse: line validates"
  fi
else
  fail "PostToolUse without PreToolUse: status == unknown"
  fail "PostToolUse without PreToolUse: t_start == t_end"
  fail "PostToolUse without PreToolUse: line validates"
fi
rm -rf "$state3"

# --- Case: Pre+PostToolUseFailure (Bash) -> exactly 1 action_span, error --
# Claude Code fires PostToolUseFailure *instead of* PostToolUse when a tool
# call fails, so the failure event has to close the open-span exactly like
# PostToolUse does — otherwise every failed tool call (a debugging agent's
# most interesting ones) dangles until the Stop sweep invents an end time.

state2b="$(mktemp -d)"
run_hook "$state2b" "$DATA/pretooluse_bash.real.json"
sleep 1
run_hook "$state2b" "$DATA/posttoolusefailure_bash.real.json"
spool2b="$(spool_file_for "$state2b" "$DATA/posttoolusefailure_bash.real.json")"

n2b="$(line_count "$spool2b")"
if [ "$n2b" = "1" ]; then
  pass "Pre+PostToolUseFailure writes exactly 1 line"
else
  fail "Pre+PostToolUseFailure writes exactly 1 line (got $n2b)"
fi

if [ -f "$spool2b" ]; then
  status2b="$(jq -r '.payload.status' "$spool2b")"
  if [ "$status2b" = "error" ]; then
    pass "Pre+PostToolUseFailure status == error"
  else
    fail "Pre+PostToolUseFailure status == error (got $status2b)"
  fi

  t_start2b="$(jq -r '.payload.t_start' "$spool2b")"
  t_end2b="$(jq -r '.payload.t_end' "$spool2b")"
  if ts_lt "$t_start2b" "$t_end2b"; then
    pass "Pre+PostToolUseFailure closes the real observed span (t_start < t_end)"
  else
    fail "Pre+PostToolUseFailure closes the real observed span (got $t_start2b / $t_end2b)"
  fi

  span_id2b="$(jq -r '.payload.span_id' "$spool2b")"
  expected_id2b="$(jq -r '.tool_use_id' "$DATA/posttoolusefailure_bash.real.json")"
  kind2b="$(jq -r '.payload.tool_kind' "$spool2b")"
  if [ "$span_id2b" = "$expected_id2b" ] && [ "$kind2b" = "bash" ]; then
    pass "Pre+PostToolUseFailure span_id == tool_use_id, tool_kind == bash"
  else
    fail "Pre+PostToolUseFailure span_id/tool_kind (got $span_id2b / $kind2b)"
  fi

  if validate_line "$(cat "$spool2b")"; then
    pass "Pre+PostToolUseFailure line validates"
  else
    fail "Pre+PostToolUseFailure line validates"
  fi

  openspan2b="$(openspan_file_for "$state2b" "$DATA/posttoolusefailure_bash.real.json" "$expected_id2b")"
  if [ -f "$openspan2b" ]; then
    fail "Pre+PostToolUseFailure deletes the open-span file"
  else
    pass "Pre+PostToolUseFailure deletes the open-span file"
  fi
else
  fail "Pre+PostToolUseFailure status == error"
  fail "Pre+PostToolUseFailure closes the real observed span (t_start < t_end)"
  fail "Pre+PostToolUseFailure span_id == tool_use_id, tool_kind == bash"
  fail "Pre+PostToolUseFailure line validates"
  fail "Pre+PostToolUseFailure deletes the open-span file"
fi
rm -rf "$state2b"

# --- Case: PostToolUseFailure with is_interrupt -> status cancelled --------

state2c="$(mktemp -d)"
run_hook "$state2c" "$DATA/pretooluse_bash.real.json"
sleep 1
run_hook "$state2c" "$DATA/posttoolusefailure_interrupt.synthetic.json"
spool2c="$(spool_file_for "$state2c" "$DATA/posttoolusefailure_interrupt.synthetic.json")"

if [ -f "$spool2c" ]; then
  status2c="$(jq -r '.payload.status' "$spool2c")"
  if [ "$status2c" = "cancelled" ]; then
    pass "PostToolUseFailure is_interrupt: status == cancelled"
  else
    fail "PostToolUseFailure is_interrupt: status == cancelled (got $status2c)"
  fi
  if validate_line "$(cat "$spool2c")"; then
    pass "PostToolUseFailure is_interrupt: line validates"
  else
    fail "PostToolUseFailure is_interrupt: line validates"
  fi
else
  fail "PostToolUseFailure is_interrupt: status == cancelled"
  fail "PostToolUseFailure is_interrupt: line validates"
fi
rm -rf "$state2c"

# --- Case: PostToolUseFailure without a matching PreToolUse -> unknown -----
# A span whose start was never observed stays `unknown` even though the
# outcome is known: the failure payload's own duration_ms is Claude Code's
# measurement, not this collector's, and t_start is never fabricated.

state2d="$(mktemp -d)"
run_hook "$state2d" "$DATA/posttoolusefailure_bash.real.json"
spool2d="$(spool_file_for "$state2d" "$DATA/posttoolusefailure_bash.real.json")"

if [ -f "$spool2d" ]; then
  status2d="$(jq -r '.payload.status' "$spool2d")"
  t_start2d="$(jq -r '.payload.t_start' "$spool2d")"
  t_end2d="$(jq -r '.payload.t_end' "$spool2d")"
  if [ "$status2d" = "unknown" ] && [ "$t_start2d" = "$t_end2d" ]; then
    pass "PostToolUseFailure without PreToolUse: status=unknown t_start==t_end"
  else
    fail "PostToolUseFailure without PreToolUse: status=unknown t_start==t_end (got status=$status2d t_start=$t_start2d t_end=$t_end2d)"
  fi
  if validate_line "$(cat "$spool2d")"; then
    pass "PostToolUseFailure without PreToolUse: line validates"
  else
    fail "PostToolUseFailure without PreToolUse: line validates"
  fi
else
  fail "PostToolUseFailure without PreToolUse: status=unknown t_start==t_end"
  fail "PostToolUseFailure without PreToolUse: line validates"
fi
rm -rf "$state2d"

# --- Case: SessionEnd closes a dangling open-span -------------------------

state4="$(mktemp -d)"
run_hook "$state4" "$DATA/pretooluse_bash.real.json"
tool_use_id4="$(jq -r '.tool_use_id' "$DATA/pretooluse_bash.real.json")"
openspan4="$(openspan_file_for "$state4" "$DATA/pretooluse_bash.real.json" "$tool_use_id4")"

if [ -f "$openspan4" ]; then
  pass "PreToolUse creates an open-span file"
else
  fail "PreToolUse creates an open-span file"
fi

sleep 1
run_hook "$state4" "$DATA/sessionend.real.json"

if [ -f "$openspan4" ]; then
  fail "SessionEnd deletes the open-span file"
else
  pass "SessionEnd deletes the open-span file"
fi

spool4="$(spool_file_for "$state4" "$DATA/sessionend.real.json")"
n4="$(line_count "$spool4")"
if [ "$n4" = "1" ]; then
  pass "SessionEnd writes exactly 1 closure line"
else
  fail "SessionEnd writes exactly 1 closure line (got $n4)"
fi

if [ -f "$spool4" ]; then
  status4="$(jq -r '.payload.status' "$spool4")"
  kind4="$(jq -r '.payload.tool_kind' "$spool4")"
  span_id4="$(jq -r '.payload.span_id' "$spool4")"
  if [ "$status4" = "unknown" ] && [ "$kind4" = "bash" ] && [ "$span_id4" = "$tool_use_id4" ]; then
    pass "SessionEnd closure: status=unknown tool_kind=bash span_id=tool_use_id"
  else
    fail "SessionEnd closure shape (got status=$status4 tool_kind=$kind4 span_id=$span_id4)"
  fi

  if validate_line "$(cat "$spool4")"; then
    pass "SessionEnd closure line validates"
  else
    fail "SessionEnd closure line validates"
  fi

  t_start4="$(jq -r '.payload.t_start' "$spool4")"
  t_end4="$(jq -r '.payload.t_end' "$spool4")"
  ts4="$(jq -r '.ts' "$spool4")"
  # The closure samples `now` once for both the envelope ts and t_end, so
  # the envelope can never claim to predate the span it closes.
  if is_ms_ts "$t_start4" && is_ms_ts "$t_end4" && [ "$ts4" = "$t_end4" ]; then
    pass "SessionEnd closure: ms timestamps, envelope ts == t_end"
  else
    fail "SessionEnd closure: ms timestamps, envelope ts == t_end (got $t_start4 / $t_end4 / ts=$ts4)"
  fi
else
  fail "SessionEnd closure: status=unknown tool_kind=bash span_id=tool_use_id"
  fail "SessionEnd closure line validates"
  fail "SessionEnd closure: ms timestamps, envelope ts == t_end"
fi
rm -rf "$state4"

# --- Case: Stop also closes a dangling open-span --------------------------

state5="$(mktemp -d)"
run_hook "$state5" "$DATA/pretooluse_edit.synthetic.json"
tool_use_id5="$(jq -r '.tool_use_id' "$DATA/pretooluse_edit.synthetic.json")"
session_id5="$(jq -r '.session_id' "$DATA/pretooluse_edit.synthetic.json")"
openspan5="$(openspan_file_for "$state5" "$DATA/pretooluse_edit.synthetic.json" "$tool_use_id5")"
sleep 1
# The Stop must come from the SAME session as the PreToolUse. The recorded
# stop.real.json fixture carries a different session_id, and this case used
# to pass with it only because the sweep was cross-session — i.e. it was
# asserting the bug. A synthetic Stop for this session is the honest form.
printf '%s' "{\"session_id\":\"$session_id5\",\"hook_event_name\":\"Stop\"}" \
  | AF_STATE_DIR="$state5" "$HOOK" >/dev/null 2>&1

if [ -f "$openspan5" ]; then
  fail "Stop deletes the open-span file"
else
  pass "Stop deletes the open-span file"
fi

spool5="$state5/spool/cc-hooks.$session_id5.jsonl"
if [ -f "$spool5" ]; then
  status5="$(jq -r '.payload.status' "$spool5")"
  kind5="$(jq -r '.payload.tool_kind' "$spool5")"
  if [ "$status5" = "unknown" ] && [ "$kind5" = "file_op" ]; then
    pass "Stop closure: status=unknown tool_kind=file_op"
  else
    fail "Stop closure shape (got status=$status5 tool_kind=$kind5)"
  fi
else
  fail "Stop closure: status=unknown tool_kind=file_op"
fi
rm -rf "$state5"

# --- Cases: tool_kind/execution_locus mapping per tool_name ---------------

assert_pair_kind_locus \
  "$DATA/pretooluse_edit.synthetic.json" "$DATA/posttooluse_edit.synthetic.json" \
  file_op local "Edit"

assert_pair_kind_locus \
  "$DATA/pretooluse_mcp.synthetic.json" "$DATA/posttooluse_mcp.synthetic.json" \
  mcp unknown "mcp__weather__get_forecast"

assert_pair_kind_locus \
  "$DATA/pretooluse_task.synthetic.json" "$DATA/posttooluse_task.synthetic.json" \
  subagent local "Task"

assert_pair_kind_locus \
  "$DATA/pretooluse_webfetch.synthetic.json" "$DATA/posttooluse_webfetch.synthetic.json" \
  web remote "WebFetch"

# --- Case: malformed stdin JSON never disturbs the session ---------------
# The shim must exit 0 no matter what garbage Claude Code (or a broken
# hook registration) puts on stdin, and must not leave any corrupt spool
# state behind.

state6="$(mktemp -d)"
if printf 'not valid json{{{' | AF_STATE_DIR="$state6" "$HOOK" >/dev/null 2>&1; then
  pass "Malformed stdin JSON: hook exits 0"
else
  fail "Malformed stdin JSON: hook exits 0"
fi

if [ -d "$state6/spool" ]; then
  bad6=0
  for f in "$state6/spool"/*; do
    [ -e "$f" ] || continue
    while IFS= read -r line; do
      printf '%s' "$line" | jq -e . >/dev/null 2>&1 || bad6=1
    done <"$f"
  done
  if [ "$bad6" = "0" ]; then
    pass "Malformed stdin JSON: no corrupt spool lines"
  else
    fail "Malformed stdin JSON: no corrupt spool lines"
  fi
else
  pass "Malformed stdin JSON: no corrupt spool lines"
fi
rm -rf "$state6"

# --- Case: jq missing from PATH -> silent no-op, exit 0 -------------------

state7="$(mktemp -d)"
if PATH="/nonexistent-af-test-path" AF_STATE_DIR="$state7" "$HOOK" \
  <"$DATA/session_start.real.json" >/dev/null 2>&1; then
  pass "jq missing from PATH: hook exits 0"
else
  fail "jq missing from PATH: hook exits 0"
fi

if [ -d "$state7/spool" ]; then
  fail "jq missing from PATH: no spool dir created"
else
  pass "jq missing from PATH: no spool dir created"
fi
rm -rf "$state7"

# --- Case: tool_use_id path traversal is sanitized -------------------------
# A malicious/buggy tool_use_id containing "../" must never let PreToolUse
# escape $AF_STATE_DIR/tmp/openspans/ when building the open-span filename.

state8="$(mktemp -d)"
malicious_pre8='{"session_id":"76cb257d-0251-4eca-825c-42ab7dff67cd","hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"../../evil/pwned"}'
printf '%s' "$malicious_pre8" | AF_STATE_DIR="$state8" "$HOOK" >/dev/null 2>&1

# What the *unsanitized* join ("$OPENSPAN_DIR/../../evil/pwned") would have
# resolved to: two levels up from tmp/openspans is $state8 itself.
traversal_target8="$state8/evil/pwned"
if [ -e "$traversal_target8" ]; then
  fail "PreToolUse path traversal: nothing written outside tmp/openspans/"
else
  pass "PreToolUse path traversal: nothing written outside tmp/openspans/"
fi

openspan_count8="$(find "$state8/tmp/openspans" -type f 2>/dev/null | wc -l | tr -d ' ')"
if [ "$openspan_count8" = "1" ]; then
  pass "PreToolUse path traversal: exactly 1 file created inside tmp/openspans/"
else
  fail "PreToolUse path traversal: exactly 1 file created inside tmp/openspans/ (got $openspan_count8)"
fi

name8="$(find "$state8/tmp/openspans" -type f -exec basename {} \; 2>/dev/null)"
case "$name8" in
  */* | '')
    fail "PreToolUse path traversal: sanitized filename has no '/' (got '$name8')"
    ;;
  *)
    pass "PreToolUse path traversal: sanitized filename has no '/' (got '$name8')"
    ;;
esac
rm -rf "$state8"

# --- Case: session_id path traversal is sanitized ---------------------------
# A malicious/buggy session_id containing "/" must never let a spool write
# escape $AF_STATE_DIR/spool/ or create a stray intermediate directory.

state9="$(mktemp -d)"
malicious_start9='{"session_id":"weird/../id","hook_event_name":"SessionStart","source":"startup"}'
printf '%s' "$malicious_start9" | AF_STATE_DIR="$state9" "$HOOK" >/dev/null 2>&1

if [ -e "$state9/spool/cc-hooks.weird" ]; then
  fail "SessionStart path traversal: no stray 'cc-hooks.weird' path component"
else
  pass "SessionStart path traversal: no stray 'cc-hooks.weird' path component"
fi

spool_count9="$(find "$state9/spool" -maxdepth 1 -type f -name 'cc-hooks.*.jsonl' 2>/dev/null | wc -l | tr -d ' ')"
if [ "$spool_count9" = "1" ]; then
  pass "SessionStart path traversal: exactly 1 spool file, directly inside spool/"
else
  fail "SessionStart path traversal: exactly 1 spool file, directly inside spool/ (got $spool_count9)"
fi
rm -rf "$state9"

# --- Case: the shared sanitize_id conformance vectors ----------------------
# `sanitize_id` is implemented three times over — this shim (`tr -cd` plus a
# `case` guard), `crates/af-otlp/src/sanitize.rs` and
# `python/af_sampler/__main__.py` — because the three collectors that build
# spool filenames are written in three languages. Two of them disagreeing
# about what a session id may contain produces two filenames for one
# session, and the join then silently sees two sessions.
# tests/fixtures/sanitize-vectors.json is the one thing all three CAN share;
# crates/af-otlp/tests/sanitize_vectors.rs and python/tests/test_sampler.py
# read the same file.
#
# Driven through the shim's real SessionStart path rather than by calling
# the function directly: what has to hold is that the *spool filename* is
# the sanitized one, which is the only reason the function exists.

VECTORS="$REPO_ROOT/tests/fixtures/sanitize-vectors.json"
# One value per line (raw, then sanitized, per vector) rather than @tsv or
# @csv: a vector's raw form may itself contain a tab, which those encodings
# would escape. No vector contains a newline, for exactly this reason.
vectors_flat="$(mktemp)"
jq -r '.[] | .raw, .sanitized' "$VECTORS" >"$vectors_flat"

vector_count=0
vector_failures=0
while IFS= read -r vec_raw && IFS= read -r vec_expected; do
  vector_count=$((vector_count + 1))
  state_v="$(mktemp -d)"
  jq -nc --arg s "$vec_raw" '{session_id: $s, hook_event_name: "SessionStart"}' |
    AF_STATE_DIR="$state_v" "$HOOK" >/dev/null 2>&1
  if [ ! -f "$state_v/spool/cc-hooks.$vec_expected.jsonl" ]; then
    vector_failures=$((vector_failures + 1))
    printf '  vector %s: wanted spool cc-hooks.%s.jsonl, got: %s\n' \
      "$vec_raw" "$vec_expected" "$(ls "$state_v/spool" 2>/dev/null | tr '\n' ' ')"
  fi
  rm -rf "$state_v"
done <"$vectors_flat"
rm -f "$vectors_flat"

if [ "$vector_count" -ge 10 ] && [ "$vector_failures" = "0" ]; then
  pass "sanitize vectors: all $vector_count shared vectors name the expected spool file"
else
  fail "sanitize vectors: $vector_failures of $vector_count shared vectors mismatched"
fi

# --- Case: corrupted open-span file during PostToolUse ---------------------
# A stale open-span file that doesn't parse as JSON must still be removed,
# and PostToolUse must still emit a valid, honestly-unknown span instead of
# crashing the shim.

state10="$(mktemp -d)"
tool_use_id10="$(jq -r '.tool_use_id' "$DATA/posttooluse_bash.real.json")"
openspan_dir10="$(openspan_dir_for "$state10" "$DATA/posttooluse_bash.real.json")"
mkdir -p "$openspan_dir10"
printf 'not valid json{{{' >"$openspan_dir10/$tool_use_id10"

if AF_STATE_DIR="$state10" "$HOOK" <"$DATA/posttooluse_bash.real.json" >/dev/null 2>&1; then
  pass "Corrupted open-span (PostToolUse): hook exits 0"
else
  fail "Corrupted open-span (PostToolUse): hook exits 0"
fi

if [ -f "$openspan_dir10/$tool_use_id10" ]; then
  fail "Corrupted open-span (PostToolUse): stale file removed"
else
  pass "Corrupted open-span (PostToolUse): stale file removed"
fi

spool10="$(spool_file_for "$state10" "$DATA/posttooluse_bash.real.json")"
if [ -f "$spool10" ]; then
  status10="$(jq -r '.payload.status' "$spool10")"
  t_start10="$(jq -r '.payload.t_start' "$spool10")"
  t_end10="$(jq -r '.payload.t_end' "$spool10")"
  if [ "$status10" = "unknown" ] && [ "$t_start10" = "$t_end10" ]; then
    pass "Corrupted open-span (PostToolUse): status=unknown t_start==t_end"
  else
    fail "Corrupted open-span (PostToolUse): status=unknown t_start==t_end (got status=$status10 t_start=$t_start10 t_end=$t_end10)"
  fi
  if validate_line "$(cat "$spool10")"; then
    pass "Corrupted open-span (PostToolUse): emitted line validates"
  else
    fail "Corrupted open-span (PostToolUse): emitted line validates"
  fi
else
  fail "Corrupted open-span (PostToolUse): status=unknown t_start==t_end"
  fail "Corrupted open-span (PostToolUse): emitted line validates"
fi
rm -rf "$state10"

# --- Case: corrupted open-span file during Stop/SessionEnd -----------------
# A stray file under tmp/openspans/ that isn't valid JSON must be counted
# and removed without emitting anything for it, and without stopping the
# rest of the sweep from running.

state11="$(mktemp -d)"
openspan_dir11="$(openspan_dir_for "$state11" "$DATA/stop.real.json")"
mkdir -p "$openspan_dir11"
printf 'not valid json{{{' >"$openspan_dir11/garbage-span"

if AF_STATE_DIR="$state11" "$HOOK" <"$DATA/stop.real.json" >/dev/null 2>&1; then
  pass "Corrupted open-span (Stop): hook exits 0"
else
  fail "Corrupted open-span (Stop): hook exits 0"
fi

if [ -f "$openspan_dir11/garbage-span" ]; then
  fail "Corrupted open-span (Stop): stray file removed"
else
  pass "Corrupted open-span (Stop): stray file removed"
fi

spool11="$(spool_file_for "$state11" "$DATA/stop.real.json")"
n11="$(line_count "$spool11")"
if [ "$n11" = "0" ]; then
  pass "Corrupted open-span (Stop): nothing emitted for the corrupt file"
else
  fail "Corrupted open-span (Stop): nothing emitted for the corrupt file (got $n11 line(s))"
fi
rm -rf "$state11"

# --- Case: one session's Stop must not sweep another session's spans -------
# Two Claude Code windows open at once is the ordinary case, not an exotic
# one. With a flat tmp/openspans/ the first session to stop swept every
# other session's in-flight spans: it closed them with a fabricated end
# time and stamped them with its OWN session_id, so the spans were
# attributed to the wrong session and the session that really owned them
# had nothing left to close.

state12="$(mktemp -d)"
session_a12="aaaaaaaa-0000-4000-8000-000000000001"
session_b12="bbbbbbbb-0000-4000-8000-000000000002"

# Both sessions open a span.
printf '%s' "{\"session_id\":\"$session_a12\",\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Bash\",\"tool_use_id\":\"toolu_a12\"}" \
  | AF_STATE_DIR="$state12" "$HOOK" >/dev/null 2>&1
printf '%s' "{\"session_id\":\"$session_b12\",\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Edit\",\"tool_use_id\":\"toolu_b12\"}" \
  | AF_STATE_DIR="$state12" "$HOOK" >/dev/null 2>&1

if [ -f "$state12/tmp/openspans/$session_a12/toolu_a12" ] \
  && [ -f "$state12/tmp/openspans/$session_b12/toolu_b12" ]; then
  pass "Concurrent sessions: each PreToolUse writes into its own session directory"
else
  fail "Concurrent sessions: each PreToolUse writes into its own session directory"
fi

sleep 1
# Session A stops. Only A's span may be closed.
printf '%s' "{\"session_id\":\"$session_a12\",\"hook_event_name\":\"Stop\"}" \
  | AF_STATE_DIR="$state12" "$HOOK" >/dev/null 2>&1

if [ -f "$state12/tmp/openspans/$session_b12/toolu_b12" ]; then
  pass "Concurrent sessions: A's Stop leaves B's open span untouched"
else
  fail "Concurrent sessions: A's Stop swept B's open span"
fi

spool_a12="$state12/spool/cc-hooks.$session_a12.jsonl"
spool_b12="$state12/spool/cc-hooks.$session_b12.jsonl"

n_a12="$(line_count "$spool_a12")"
if [ "$n_a12" = "1" ]; then
  pass "Concurrent sessions: A's Stop closes exactly its own 1 span"
else
  fail "Concurrent sessions: A's Stop closes exactly its own 1 span (got $n_a12)"
fi

if [ -f "$spool_a12" ]; then
  span_a12="$(jq -r '.payload.span_id' "$spool_a12")"
  if [ "$span_a12" = "toolu_a12" ]; then
    pass "Concurrent sessions: A closed its own span, not B's"
  else
    fail "Concurrent sessions: A closed span '$span_a12' (expected toolu_a12)"
  fi
fi

n_b12="$(line_count "$spool_b12")"
if [ "$n_b12" = "0" ]; then
  pass "Concurrent sessions: nothing written to B's spool by A's Stop"
else
  fail "Concurrent sessions: A's Stop wrote $n_b12 line(s) into B's spool"
fi

# …and B can still close its own span afterwards.
printf '%s' "{\"session_id\":\"$session_b12\",\"hook_event_name\":\"SessionEnd\"}" \
  | AF_STATE_DIR="$state12" "$HOOK" >/dev/null 2>&1

n_b12_after="$(line_count "$spool_b12")"
if [ "$n_b12_after" = "1" ]; then
  pass "Concurrent sessions: B still closes its own span when it stops"
else
  fail "Concurrent sessions: B still closes its own span when it stops (got $n_b12_after)"
fi

if [ -f "$spool_b12" ]; then
  span_b12="$(jq -r '.payload.span_id' "$spool_b12")"
  kind_b12="$(jq -r '.payload.tool_kind' "$spool_b12")"
  session_field_b12="$(jq -r '.session_id' "$spool_b12")"
  if [ "$span_b12" = "toolu_b12" ] && [ "$kind_b12" = "file_op" ] \
    && [ "$session_field_b12" = "$session_b12" ]; then
    pass "Concurrent sessions: B's closure carries B's span, kind and session_id"
  else
    fail "Concurrent sessions: B's closure shape (got $span_b12 / $kind_b12 / $session_field_b12)"
  fi

  if validate_line "$(cat "$spool_b12")"; then
    pass "Concurrent sessions: B's closure line validates"
  else
    fail "Concurrent sessions: B's closure line validates"
  fi
fi

# The swept directories are cleaned up, so a long-lived state dir doesn't
# accumulate one empty directory per session ever run.
if [ -d "$state12/tmp/openspans/$session_a12" ] || [ -d "$state12/tmp/openspans/$session_b12" ]; then
  fail "Concurrent sessions: swept session directories are removed"
else
  pass "Concurrent sessions: swept session directories are removed"
fi
rm -rf "$state12"

# --- Summary ---------------------------------------------------------------

echo "----"
echo "$CASES cases, $FAILURES failed"
if [ "$FAILURES" -gt 0 ]; then
  exit 1
fi
