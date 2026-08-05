#!/bin/sh
# collectors/claude-code/af-hook.sh
#
# Claude Code hooks collector ("cc-hooks"). One shim handles all six hook
# events (SessionStart, PreToolUse, PostToolUse, PostToolUseFailure, Stop,
# SessionEnd) — the same script is registered for every event in
# .claude/settings.json (see README.md), and dispatch happens on the hook
# payload's own `hook_event_name` field rather than needing six separate
# scripts.
#
# POSIX sh (not bash-only); the only external dependency is jq. Never reads
# transcripts — only the hook JSON on stdin.
#
# Contract: this shim ALWAYS exits 0, no matter what goes wrong internally
# (malformed stdin JSON, jq missing from PATH, a corrupted open-span file,
# an unwritable state dir, ...). It runs inside a live Claude Code session
# on every tool call; collection is best-effort and the running session is
# sacred — this shim must never be the reason a turn fails. All real work
# happens inside main(), invoked as a subshell so a `set -e` failure deep
# inside it only unwinds that subshell, never this process; the invocation
# is additionally guarded with `|| true` and followed by an unconditional
# `exit 0`. Errors are best-effort logged to
# `$AF_STATE_DIR/tmp/hook-errors.log` and otherwise swallowed.
#
# FORK BUDGET. Claude Code *blocks on this script* for every tool call, so
# every `jq` process it starts is latency the user waits through twice per
# tool call (PreToolUse + PostToolUse). Field-at-a-time extraction cost 13
# jq forks per tool call (~67 ms of pure process startup on the measured
# machine). There are now two jq programs and one invocation of each per
# emitted event:
#
#   * $JQ_EXTRACT — one pass over the hook JSON that emits every field this
#     shim reads *plus* the timestamp, as `@sh`-quoted shell assignments
#     (`@sh` is what makes `eval` safe here: jq single-quotes each value,
#     so no payload field can be read as shell syntax) — and it is also
#     what gives every event in one invocation a single `now` sample.
#   * emit_event() — one invocation builds the whole Contract #1 envelope
#     *and* its payload, so an event costs one fork rather than five.
#
# Per invocation: SessionStart 3, PreToolUse 2, PostToolUse 2,
# Stop/SessionEnd 1 + 1 per open span it closes.
#
# session_id and tool_use_id come straight from Claude Code's hook JSON and
# are used to build file paths (spool filename, open-span filename) — see
# sanitize_id() below, applied to both right after extraction.
#
# The hook-event mapping, tool-kind/locus rules, open-span lifecycle, and
# timestamp behavior are implemented and tested alongside this collector.

set -u

SCHEMA_VERSION="0.1.0"
COLLECTOR_NAME="cc-hooks"
COLLECTOR_VERSION="0.1.0"

# ${HOME:-/tmp} rather than bare $HOME: under `set -u` a missing $HOME
# would otherwise abort the script here, before main()'s safety net is
# even in place.
STATE_DIR="${AF_STATE_DIR:-${HOME:-/tmp}/.local/state/agentic-footprint}"
SPOOL_DIR="$STATE_DIR/spool"
# Open-span scratch files are partitioned per session:
# $OPENSPAN_ROOT/<SESSION_ID>/<tool_use_id>. The per-session directory is
# what makes the Stop/SessionEnd sweep safe. Flat, every concurrent Claude
# Code session shared one directory, so the first session to stop swept
# every *other* session's in-flight spans too — closing them with a
# fabricated end time, attributing them to the wrong session_id, and
# leaving the sessions that really owned them with nothing to close.
# $OPENSPAN_DIR is derived from it inside main(), once SESSION_ID is known
# and sanitized.
OPENSPAN_ROOT="$STATE_DIR/tmp/openspans"

# jq is this shim's only external dependency. If it isn't on PATH, no-op
# silently rather than fail loudly at the session's expense — collection
# is best-effort, jq is not.
command -v jq >/dev/null 2>&1 || exit 0

# ---------------------------------------------------------------------------
# The two jq programs
# ---------------------------------------------------------------------------

# Everything this shim reads out of the hook JSON, in one pass, as shell
# assignments to be `eval`ed (see the fork-budget note above).
#
# The timestamp rides along because it is derived from jq's `now` anyway:
# RFC 3339 UTC with millisecond precision, which `date` cannot produce
# portably (sub-second output needs GNU's non-POSIX `+%N`; macOS's BSD
# `date` has no equivalent). Second precision would collapse most tool
# calls to `t_start == t_end`, which the attribution join reads as a
# degenerate span and cannot pay any energy to.
#
# One `now` per hook invocation: `$t` is sampled once, and both the seconds
# and the millisecond remainder are derived from it, so the two halves can
# never straddle a second boundary and report e.g. 12:00:01.999 for an
# instant that was really 12:00:00.999. Every event a single invocation
# emits carries that same instant — a hook invocation *is* one instant as
# far as this collector can honestly claim to know. jq's strftime is
# gmtime-based, so the result is UTC and the trailing `Z` is honest.
JQ_EXTRACT='
  now as $t
  | (($t | strftime("%Y-%m-%dT%H:%M:%S"))
     + "."
     + ((($t * 1000 | floor) % 1000 | tostring | ("00" + .)[-3:]))
     + "Z") as $ts
  | [ "hk_session_raw="     + ((.session_id // "unknown") | tostring | @sh),
      "hk_event="           + ((.hook_event_name // "") | tostring | @sh),
      "hk_tool_use_id_raw=" + ((.tool_use_id // "") | tostring | @sh),
      "hk_tool_name="       + ((.tool_name // "") | tostring | @sh),
      "hk_interrupt="       + ((.is_interrupt // false) | tostring | @sh),
      "hk_now="             + ($ts | @sh)
    ]
  | join("\n")'

# tool_name -> {kind, locus}. One definition, used by every emitted
# action_span: the closing PostToolUse knows the name from the hook
# payload, the Stop/SessionEnd sweep reads it back out of the open-span
# file, and both must classify it identically.
#
# mcp__* -> execution_locus "unknown": the tool name alone doesn't reveal
# whether the MCP server is a local stdio process or a remote HTTP server,
# and this collector never invents a locus it can't observe — an honest
# "unknown" beats a guessed "local" or "remote". WebFetch/WebSearch are the
# only tools this shim can be sure are remote-network by name alone.
JQ_TOOL_CLASS='def tool_class($n):
  if $n == "Bash" then {kind: "bash", locus: "local"}
  elif ($n | startswith("mcp__")) then {kind: "mcp", locus: "unknown"}
  elif (["Edit", "Write", "Read", "NotebookEdit", "Glob", "Grep"] | index($n))
    then {kind: "file_op", locus: "local"}
  elif (["Task", "Agent"] | index($n)) then {kind: "subagent", locus: "local"}
  elif (["WebFetch", "WebSearch"] | index($n)) then {kind: "web", locus: "remote"}
  else {kind: "other", locus: "unknown"}
  end;
'

# The Contract #1 envelope, written once. `+` preserves the left operand's
# key order, so the emitted field order is stable and independent of which
# payload follows: schema_version, event_id, ts, collector, session_id,
# [attribution,] type, payload.
JQ_ENVELOPE='{
    schema_version: $schema_version,
    event_id: $event_id,
    ts: $ts,
    collector: {name: $collector_name, version: $collector_version},
    session_id: $session_id
  }
  + (if $tool_call_id == "" then {} else {attribution: {tool_call_id: $tool_call_id}} end)
  + {type: $type, payload: '

# --- payload expressions, one per emitted shape ---------------------------

# Bootstrap action_span: session_meta is schema-frozen (no room for a
# process-id field), so the shim's own $PPID — which IS the Claude Code
# process, since Claude Code spawns hook commands as direct children when
# the command is an executable path rather than a `sh -c` wrapper (see
# README.md) — travels as `pids` on a zero-length action_span instead.
JQ_BOOTSTRAP_PAYLOAD='{
    span_id: ("session-boot-" + $session_id),
    tool_name: "__session__",
    tool_kind: "other",
    execution_locus: "local",
    t_start: $ts,
    t_end: $ts,
    pids: $pids,
    status: "ok"
  }'

# version omitted: not present in any hook payload field; geo_zone omitted:
# user-configured, not auto-detected here (brief + schema).
JQ_META_PAYLOAD='{agent_app: {name: "claude-code"}, os: $os}'

# One closing action_span, for both the PostToolUse/PostToolUseFailure path
# and the Stop/SessionEnd sweep. `$span` is the open-span file's contents
# (a one-element array, `[]` when there is no file) — see emit_event.
#
# Never fabricate a start time we didn't observe: with no usable open-span
# record (hooks enabled mid-session, the file lost, a truncated write) the
# span collapses to a point and is marked `unknown`, which wins over the
# known outcome on purpose — the span itself was never observed, and a
# `duration_ms` reported by the failure payload is Claude Code's
# measurement, not ours.
#
# tool_name comes from the hook payload when it has one and from the
# open-span file otherwise, which is what lets the sweep — whose Stop
# payload names no tool — classify the spans it closes.
JQ_SPAN_PAYLOAD='(
    ($span[0].t_start // "") as $observed_start
    | (if $tool_name == "" then ($span[0].tool_name // "" | tostring) else $tool_name end) as $name
    | tool_class($name) as $class
    | {
        span_id: $span_id,
        tool_name: $name,
        tool_kind: $class.kind,
        execution_locus: $class.locus,
        t_start: (if $observed_start == "" then $t_end else $observed_start end),
        t_end: $t_end,
        status: (if $observed_start == "" then "unknown" else $status end)
      }
  )'

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Reduces session_id/tool_use_id to a single safe path component before
# either is used to build a filename: strips every character outside
# [A-Za-z0-9._-] (in particular '/', which is what makes traversal
# impossible — a stripped id can never contain a path separator), then
# guards the empty string or a leading '.' (hidden file, or literally ".."
# once dots survive the strip) by prefixing "x". $1 = raw id, safe value
# printed on stdout. The conformance vectors in
# tests/fixtures/sanitize-vectors.json pin this rule across all three
# implementations of it (here, af-otlp, af_sampler).
sanitize_id() {
  clean="$(printf '%s' "$1" | tr -cd 'A-Za-z0-9._-')"
  case "$clean" in
    '' | .*) clean="x$clean" ;;
  esac
  printf '%s' "$clean"
}

# uuidgen (macOS/BSD/most Linux) lowercased, per the brief. Falls back to
# /proc/sys/kernel/random/uuid (Linux without uuid-runtime installed, e.g.
# a minimal CI image) and finally to a synthetic-but-unique id string if
# neither is available — always well over the schema's `event_id`
# `minLength: 16`.
new_event_id() {
  if command -v uuidgen >/dev/null 2>&1; then
    uuidgen | tr 'A-Z' 'a-z'
  elif [ -r /proc/sys/kernel/random/uuid ]; then
    cat /proc/sys/kernel/random/uuid
  else
    rand="0"
    if [ -r /dev/urandom ]; then
      rand="$(od -An -N4 -tu4 /dev/urandom 2>/dev/null | tr -d ' ')"
    fi
    printf 'af-fallback-%s-%s-%s\n' "$(date -u +%s)" "$$" "${rand:-0}"
  fi
}

# Appends one Contract #1 event line to this session's spool file. A single
# printf keeps each line complete; collectors never delete spool lines once
# written.
emit() {
  mkdir -p "$SPOOL_DIR"
  printf '%s\n' "$1" >>"$SPOOL_DIR/$COLLECTOR_NAME.$SESSION_ID.jsonl"
}

# Builds one event with a single jq invocation and appends it.
#   $1 = event type          $3 = attribution tool_call_id ("" = no block)
#   $2 = envelope ts         $4 = payload expression (one of JQ_*_PAYLOAD)
# The payload fields that vary travel in the ev_* variables below rather
# than as more positional parameters; an unused one costs nothing.
#   ev_span_id ev_tool_name ev_status   read by JQ_SPAN_PAYLOAD
#   ev_os ev_pids                       read by the SessionStart payloads
#   ev_span_file  when it names an existing file, its contents are handed
#                 to the payload as $span (`[]` otherwise). A file that
#                 does not parse as JSON makes jq fail, which is how the
#                 caller detects a corrupt open-span record: this function
#                 returns nonzero and emits nothing.
emit_event() {
  ev_type="$1"
  ev_ts="$2"
  ev_attr="$3"
  ev_payload="$4"
  if [ -f "$ev_span_file" ]; then
    set -- --slurpfile span "$ev_span_file"
  else
    set -- --argjson span '[]'
  fi
  ev_json="$(jq -n -c "$@" \
    --arg schema_version "$SCHEMA_VERSION" \
    --arg event_id "$(new_event_id)" \
    --arg ts "$ev_ts" \
    --arg collector_name "$COLLECTOR_NAME" \
    --arg collector_version "$COLLECTOR_VERSION" \
    --arg session_id "$SESSION_ID" \
    --arg type "$ev_type" \
    --arg tool_call_id "$ev_attr" \
    --arg span_id "$ev_span_id" \
    --arg tool_name "$ev_tool_name" \
    --arg t_end "$ev_ts" \
    --arg status "$ev_status" \
    --arg os "$ev_os" \
    --argjson pids "$ev_pids" \
    "$JQ_TOOL_CLASS$JQ_ENVELOPE$ev_payload}")" || return 1
  [ -n "$ev_json" ] || return 1
  emit "$ev_json"
}

# All real work happens here. Defined as `main() ( ... )` — a function
# whose body is a subshell — specifically so `set -e` can be used freely
# inside without any risk of an unexpected failure killing this process:
# an `exit` (explicit, or errexit-triggered) inside a subshell only
# terminates that subshell, never its parent. The invocation below adds a
# second layer (`|| true; exit 0`) so nothing this function does can ever
# surface as a nonzero exit from the script.
main() (
  set -e

  # Defaults, so that stdin jq cannot parse at all (which prints its own
  # error to the hook error log and produces no assignments) lands in the
  # unknown-event branch instead of tripping `set -u`.
  hk_session_raw="unknown"
  hk_event=""
  hk_tool_use_id_raw=""
  hk_tool_name=""
  hk_interrupt="false"
  hk_now=""

  # Safe because every value jq interpolates here goes through `@sh`.
  eval "$(jq -r "$JQ_EXTRACT")"

  SESSION_ID="$(sanitize_id "$hk_session_raw")"
  # Safe as a path component because SESSION_ID has been through
  # sanitize_id: it can hold no separator, so this can only ever name a
  # direct child of $OPENSPAN_ROOT.
  OPENSPAN_DIR="$OPENSPAN_ROOT/$SESSION_ID"

  ev_span_id=""
  ev_tool_name=""
  ev_status=""
  ev_os=""
  ev_pids="null"
  ev_span_file=""

  case "$hk_event" in
    SessionStart)
      ev_pids="[$PPID]"
      emit_event action_span "$hk_now" "" "$JQ_BOOTSTRAP_PAYLOAD"

      ev_os="$(uname -s | tr 'A-Z' 'a-z')-$(uname -r)"
      emit_event session_meta "$hk_now" "" "$JQ_META_PAYLOAD"
      ;;

    PreToolUse)
      # No spool write here — PreToolUse only opens the span. Without a
      # tool_use_id there's no key to open it under, so there's nothing this
      # hook can usefully record.
      if [ -n "$hk_tool_use_id_raw" ]; then
        tool_use_id="$(sanitize_id "$hk_tool_use_id_raw")"
        mkdir -p "$OPENSPAN_DIR"
        jq -n -c --arg t_start "$hk_now" --arg tool_name "$hk_tool_name" \
          '{t_start: $t_start, tool_name: $tool_name}' >"$OPENSPAN_DIR/$tool_use_id"
      fi
      ;;

    PostToolUse | PostToolUseFailure)
      # PostToolUseFailure is the *other* half of PostToolUse, not an extra:
      # Claude Code fires exactly one of the two per tool call, and a tool
      # call that fails (nonzero exit, denied permission, interrupt) gets
      # ONLY the failure event. Verified empirically against Claude Code
      # v2.1.220 during the Task 15 acceptance run — a shim registered for
      # PostToolUse alone leaves every failed tool call's span open until
      # the Stop sweep closes it with a fabricated end time, which for a
      # debugging agent is precisely the interesting subset (the failing
      # test run). Both branches share the closing logic below; only the
      # `status` of an observed span differs.
      [ -n "$hk_tool_use_id_raw" ] || exit 0
      tool_use_id="$(sanitize_id "$hk_tool_use_id_raw")"

      # The outcome to report for a span whose start we actually observed.
      # `is_interrupt: true` is the user/agent cancelling a running tool,
      # which the schema names `cancelled`; every other failure is `error`.
      if [ "$hk_event" = "PostToolUseFailure" ]; then
        if [ "$hk_interrupt" = "true" ]; then
          ev_status="cancelled"
        else
          ev_status="error"
        fi
      else
        ev_status="ok"
      fi

      # Envelope ts is the invocation's single `now`, which is also t_end:
      # the event is emitted the instant the span closes, and a fresh
      # sample would only add jq-fork latency to a hook in the session's
      # hot path.
      ev_span_id="$tool_use_id"
      ev_tool_name="$hk_tool_name"
      ev_span_file="$OPENSPAN_DIR/$tool_use_id"
      if ! emit_event action_span "$hk_now" "$tool_use_id" "$JQ_SPAN_PAYLOAD"; then
        # The open-span file exists but doesn't parse (crash mid-write, disk
        # issue, ...). Close the span honestly, with no start time, exactly
        # as if the file had never been there.
        ev_span_file=""
        emit_event action_span "$hk_now" "$tool_use_id" "$JQ_SPAN_PAYLOAD" || true
      fi
      # Removed either way, so a stale file can't be mis-parsed again by a
      # later Stop/SessionEnd sweep.
      rm -f "$OPENSPAN_DIR/$tool_use_id"
      ;;

    Stop | SessionEnd)
      # Close any open-spans still on disk **for this session only**: a
      # PreToolUse that never got a matching PostToolUse (the turn was
      # interrupted, Claude Code crashed mid-tool-call, ...). Only files
      # whose content actually parses as JSON are treated as open-spans and
      # emitted; anything else is a corrupt/stray file — counted and
      # removed, nothing emitted for it, never crashes the shim.
      #
      # $OPENSPAN_DIR is this session's directory, so a concurrent session's
      # in-flight spans are not visible here and cannot be swept.
      [ -d "$OPENSPAN_DIR" ] || exit 0
      corrupt_count=0
      for span_file in "$OPENSPAN_DIR"/*; do
        [ -e "$span_file" ] || continue

        # The sweep's `now` is the invocation's, shared with the envelope
        # ts, so an envelope can never claim to predate the span it closes.
        ev_span_file="$span_file"
        ev_span_id="$(basename "$span_file")"
        ev_tool_name=""
        ev_status="unknown"
        emit_event action_span "$hk_now" "$ev_span_id" "$JQ_SPAN_PAYLOAD" ||
          corrupt_count=$((corrupt_count + 1))
        rm -f "$span_file"
      done
      if [ "$corrupt_count" -gt 0 ]; then
        printf 'af[claude-hook] warn: Stop/SessionEnd skipped %d corrupt open-span file(s) under %s\n' \
          "$corrupt_count" "$OPENSPAN_DIR" >&2
      fi
      # The sweep emptied this session's directory; remove it so a
      # long-lived state dir doesn't accumulate one empty directory per
      # session ever run. Best-effort and non-recursive: rmdir refuses a
      # non-empty directory, which is exactly the safety wanted if a
      # concurrent PreToolUse in this same session just wrote a new file.
      rmdir "$OPENSPAN_DIR" 2>/dev/null || true
      ;;

    *)
      # Unregistered/unknown hook event: nothing to do. Not an error — keeps
      # the shim forward-compatible with hook events this collector doesn't
      # model yet, rather than failing a Claude Code turn over it.
      exit 0
      ;;
  esac
)

if mkdir -p "$STATE_DIR/tmp" 2>/dev/null; then
  main "$@" 2>>"$STATE_DIR/tmp/hook-errors.log" || true
else
  # Can't even create the tmp dir (read-only filesystem, permissions,
  # a state dir that doesn't exist and can't be created, ...) — there's
  # nowhere safe to log to. Run silently; the session must never see this
  # shim fail either way.
  main "$@" >/dev/null 2>&1 || true
fi
exit 0
