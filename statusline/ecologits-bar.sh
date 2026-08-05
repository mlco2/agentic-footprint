#!/usr/bin/env bash
#
# agentic-footprint impact bar for Claude Code  (drop-in component)
#
# This prints ONE line with the environmental impact — greenhouse gas,
# freshwater, energy, and more — of the current session, as measured and
# estimated by the LOCAL control plane (`af`). It is meant to be called from
# inside YOUR OWN statusline.sh, which keeps full ownership of its output.
# Add this after your line prints:
#
#     printf '%s' "$input" | ~/.claude/ecologits-bar.sh
#
# where $input holds the JSON Claude Code sent on stdin (the canonical
# `input=$(cat)` at the top of a statusline script). The bar reads that JSON on
# its own stdin and appends its line below yours.
#
# HOW THIS VARIANT DIFFERS from the upstream EcoLogits bar
# (https://github.com/DuarteVi/ecologits-statusline), which this is adapted
# from and whose rendering it keeps verbatim:
#
#   * No network, no API key, no cache, no background refresh. The numbers
#     come from one synchronous `af statusline` call, which reads the local
#     control plane's already-computed session record and never estimates,
#     ingests or writes anything. If `af` is missing or has nothing stored
#     for this session yet, the bar renders zeros.
#   * The numbers are NOT token-count estimates of the remote inference
#     alone: `af` joins the ecologits estimate for the session's LLM calls
#     with the *measured* local energy of the agent's own tool execution on
#     this machine. Zeros mean "not measured yet", never "no impact".
#   * No model resolution. The control plane learns the model from the
#     session's own events, so nothing here needs to guess an API model id;
#     the `model` metric simply displays what Claude Code reports on stdin.
#   * The electricity-mix zone is the control plane's business too
#     (`af report --zone`, `$AF_ZONE`) — not this script's.
#
# Repo: https://github.com/<org>/agentic-footprint
# Impact methodology powered by EcoLogits — https://ecologits.ai
#
# Configuration: edit ~/.claude/ecologits.config.sh (sourced below). Each value
# can also be overridden by an exported environment variable of the same name:
#   ECOLOGITS_METRICS   impacts to display      (default: "gwp wcf energy")
#                       one or more of: gwp wcf energy adpe pe model
#   AF_BIN              path to the `af` binary (default: af, from $PATH)
#
# Dependencies: bash, jq, awk, and the `af` binary.

input=$(cat)

CONFIG_FILE="$HOME/.claude/ecologits.config.sh"

# Load user configuration (real exported env vars still take precedence,
# because the config file uses `: "${VAR:=default}"` assignments).
[ -f "$CONFIG_FILE" ] && . "$CONFIG_FILE"

GRAY='\033[90m'; RESET='\033[0m'

# ---- Everything this script reads out of Claude Code's status JSON, in one
#      jq pass: the session/transcript pair that says whether the input is
#      usable at all, and the model name for the (purely cosmetic — no
#      estimation depends on it) "model" metric. `?` on the model lookups so
#      a `model` field that isn't an object degrades to "unknown" instead of
#      failing the whole extraction.
META=$(printf '%s' "$input" | jq -r '
  [ .session_id // "",
    .transcript_path // "",
    (.model.display_name? // .model.id? // "")
  ] | .[]' 2>/dev/null)
{ IFS= read -r SESSION; IFS= read -r TRANSCRIPT; IFS= read -r ECO_MODEL; } <<<"$META"

# No usable input? Most likely the snippet's $input wasn't the captured
# stdin (e.g. your script names it differently, or never ran `input=$(cat)`).
# Print a visible hint rather than a normal-looking bar that never advances.
if [ -z "$SESSION" ] && [ -z "$TRANSCRIPT" ]; then
  printf '%b\n' "${GRAY}🤖 agentic-footprint: no input — is your captured stdin named \$input?${RESET}"
  exit 0
fi

ECO_METRICS="${ECOLOGITS_METRICS:-gwp wcf energy}"
[ -z "$ECO_MODEL" ] && ECO_MODEL="unknown"

# ---- The numbers: one synchronous call into the local control plane -------
#
# `af statusline` takes the same status JSON on stdin, reads the stored
# session record read-only, and prints exactly one line of five plain
# decimals — always, including when it knows nothing. The `|| printf` is for
# the cases `af` never gets to answer at all (not installed, not executable):
# a status line must not lose its bar because a binary is missing.
LINE=$("${AF_BIN:-af}" statusline <<<"$input" 2>/dev/null || printf '0 0 0 0 0')
read -r GWP WCF ENERGY ADPE PE <<<"$LINE"

# ---- The metrics, one table -----------------------------------------------
#
# One row per metric key: what to draw, which of the five numbers it shows,
# and the unit ladder to auto-scale it with. This is also the list of keys
# ECOLOGITS_METRICS may name — a second copy of that list is how a metric
# ends up selectable but unrenderable (or vice versa).
#
#   key | emoji | value variable | unit ladder
#
# A ladder is `;`-separated rungs, read left to right; the first rung whose
# `value x test-scale` reaches its threshold wins and prints
# `value x print-scale` with that many decimals, followed by its unit:
#
#   test-scale : threshold : print-scale : decimals : unit
#
# The last rung of every ladder has threshold 0, so it always matches (a
# value that reaches no rung would render as "0" — a lie, see below).
# `-1` decimals prints the unit text on its own, with no number.
#
# A rendered "0" means "unmeasured" everywhere in this bar, so a positive
# value must never render as one. ADPe is the metric where that bites, and
# why its ladder is the long one: a single small-model call is a few
# nanograms of antimony equivalent, and "%.0f µgSbeq" printed every one of
# them as "0" — indistinguishable from a control plane that had no number
# at all. The sub-microgram range keeps enough precision to stay non-zero,
# drops to nanograms below that, and anything smaller than the finest unit
# says "<0.01 ngSbeq" rather than rounding itself away.
#
# The `model` row has no ladder: it shows a name, not a quantity.
METRIC_TABLE="\
gwp|🔥|GWP|1:1:1:2:kgCO₂eq;1000:10:1000:0:gCO₂eq;1:0.001:1000:1:gCO₂eq;1:0:1000000:0:mgCO₂eq
wcf|💧|WCF|1:1:1:2:L;1000:10:1000:0:mL;1000:1:1000:1:mL;1:0:1000:2:mL
energy|⚡️|ENERGY|1:1:1:2:kWh;1000:10:1000:0:Wh;1000:1:1000:1:Wh;1:0:1000000:0:mWh
adpe|⛏️|ADPE|1:1:1:2:kgSbeq;1:0.001:1000:1:gSbeq;1000000:10:1000000:0:mgSbeq;1:0.000001:1000000:1:mgSbeq;1000000000:10:1000000000:0:µgSbeq;1000000000:0.01:1000000000:2:µgSbeq;1000000000000:0.01:1000000000000:2:ngSbeq;1:0:1:-1:<0.01 ngSbeq
pe|🛢️|PE|1:1:1:2:MJ;1000:10:1000:0:kJ;1:0.001:1000:1:kJ;1:0:1000000:0:J
model|🤖|ECO_MODEL|"

# $1 = metric key. Sets M_EMOJI/M_VAR/M_LADDER from the table and returns 0,
# or returns 1 for a key the table doesn't have (which is what makes an
# unknown ECOLOGITS_METRICS entry ignorable rather than blank).
metric_lookup() {
  local key
  while IFS='|' read -r key M_EMOJI M_VAR M_LADDER; do
    [ "$key" = "$1" ] && return 0
  done <<<"$METRIC_TABLE"
  return 1
}

# $1 = unit ladder, $2 = value. Anything non-positive or unparseable is
# "unmeasured" and renders as a bare "0".
fmt_metric() {
  awk -v v="$2" -v ladder="$1" 'BEGIN{
    if (v=="" || v+0<=0) { print "0"; exit }
    n = split(ladder, rungs, ";")
    for (i = 1; i <= n; i++) {
      split(rungs[i], rung, ":")
      if (v * rung[1] >= rung[2] + 0) {
        if (rung[4] == "-1") printf "%s", rung[5];
        else                 printf "%." rung[4] "f %s", v * rung[3], rung[5];
        exit
      }
    }
    print "0"
  }'
}

# Build the eco line from the selected metrics, in order. Metrics the control
# plane has no number for yet render as "0" via the formatter — never "…".
SELECTED=()
for key in $ECO_METRICS; do
  metric_lookup "$key" && SELECTED+=("$key")   # unknown keys are ignored
done
[ "${#SELECTED[@]}" -eq 0 ] && SELECTED=(gwp wcf energy)

ECO_LINE=""
for key in "${SELECTED[@]}"; do
  metric_lookup "$key"
  value="${!M_VAR}"
  if [ -n "$M_LADDER" ]; then
    piece="$M_EMOJI $(fmt_metric "$M_LADDER" "$value")"
  else
    piece="$M_EMOJI ${value#claude-}"
  fi
  if [ -z "$ECO_LINE" ]; then ECO_LINE="$piece"; else ECO_LINE="$ECO_LINE | $piece"; fi
done

# ---- Render: one line, appended below whatever your status line printed -----
printf '%b\n' "${GRAY}${ECO_LINE}${RESET}"
