#!/bin/sh
# statusline/test_statusline.sh
#
# Plain-sh test runner for ecologits-bar.sh (no bats, matching this repo's
# shell tooling budget — same shape as collectors/claude-code/test_hooks.sh).
#
# The bar is exercised against a **stub** `af` (via $AF_BIN) that prints a
# known five-number line, so this suite tests exactly what the bar owns:
# that it hands Claude Code's status JSON through unmodified, that it maps
# the five fields to the right metrics in the right order, that the unit
# auto-scaling and the ECOLOGITS_METRICS selection still behave, and — most
# importantly — that no failure of the control plane can break a user's
# status line.
#
# The numbers `af` actually produces for a seeded store are asserted on the
# Rust side (crates/af-cli/tests/statusline.rs); the golden line used here
# is that test's expected output verbatim, so the two ends of the contract
# are pinned to the same digits.
#
# Wired into CI as a step after the hook collector tests
# (.github/workflows/ci.yml).

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BAR="$SCRIPT_DIR/ecologits-bar.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

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
  printf '  expected: %s\n' "$2"
  printf '  actual:   %s\n' "$3"
}

# --- stub `af` -------------------------------------------------------------
# Prints $STUB_LINE (default: the golden line), saves the stdin it received
# so a case can assert the status JSON was passed through untouched, and
# exits with $STUB_EXIT.

STUB="$WORK/af"
cat >"$STUB" <<'STUB_EOF'
#!/bin/sh
cat >"$STUB_STDIN"
[ "${STUB_EXIT:-0}" -ne 0 ] && exit "$STUB_EXIT"
printf '%s\n' "${STUB_LINE?}"
STUB_EOF
chmod +x "$STUB"

# The exact line `af statusline` prints for the Task 12 golden fixture
# (see crates/af-cli/tests/statusline.rs::EXPECTED_LINE):
#   gwp 0.000188 kgCO2eq | water 3.75e-5 L | energy 0.000383 kWh
#   | adpe 1.875e-9 kgSbeq | pe 0.00375 MJ
GOLDEN_LINE="0.00018793293055555555 0.000037500000000000003 0.0003834722222222222 0.0000000018750000000000002 0.00375"

STATUS_JSON='{"hook_event_name":"Status","session_id":"sess-basic","transcript_path":"/tmp/t.jsonl","model":{"id":"claude-opus-4-6","display_name":"Opus 4.6"}}'

# --- helpers ---------------------------------------------------------------

run_bar() {
  # $1 = stdin payload; remaining env comes from the caller.
  printf '%s' "$1" | STUB_STDIN="$WORK/stdin.json" AF_BIN="$STUB" \
    HOME="$WORK/home" bash "$BAR"
}

# Strips the SGR colour codes the bar wraps its line in, so the assertions
# read as plain text.
plain() {
  sed 's/'"$(printf '\033')"'\[[0-9;]*m//g'
}

check() {
  # $1 = case name, $2 = expected, $3 = actual
  if [ "$2" = "$3" ]; then
    pass "$1"
  else
    fail "$1" "$2" "$3"
  fi
}

mkdir -p "$WORK/home"

# --- Case: the five fields map to the five metrics, auto-scaled ------------

actual="$(STUB_LINE="$GOLDEN_LINE" run_bar "$STATUS_JSON" | plain)"
check "default metrics render gwp/wcf/energy, auto-scaled" \
  "🔥 188 mgCO₂eq | 💧 0.04 mL | ⚡️ 383 mWh" "$actual"

# --- Case: the status JSON reaches `af` byte-for-byte ----------------------
# `af statusline` extracts the session id itself; if the bar reformatted or
# truncated the payload, the control plane would answer for the wrong
# session (or for none).

actual="$(cat "$WORK/stdin.json")"
check "the status JSON is handed to af unmodified" "$STATUS_JSON" "$actual"

# --- Case: every metric, in the configured order ---------------------------

actual="$(ECOLOGITS_METRICS="pe adpe model" STUB_LINE="$GOLDEN_LINE" \
  run_bar "$STATUS_JSON" | plain)"
check "ECOLOGITS_METRICS selects and orders metrics" \
  "🛢️ 3.8 kJ | ⛏️ 1.88 µgSbeq | 🤖 Opus 4.6" "$actual"

# --- Case: unknown metric keys are ignored, empty selection falls back -----

actual="$(ECOLOGITS_METRICS="banana gwp" STUB_LINE="$GOLDEN_LINE" \
  run_bar "$STATUS_JSON" | plain)"
check "unknown metric keys are ignored" "🔥 188 mgCO₂eq" "$actual"

actual="$(ECOLOGITS_METRICS="banana" STUB_LINE="$GOLDEN_LINE" \
  run_bar "$STATUS_JSON" | plain)"
check "an all-unknown selection falls back to the default three" \
  "🔥 188 mgCO₂eq | 💧 0.04 mL | ⚡️ 383 mWh" "$actual"

# --- Case: bigger numbers scale up to the coarse units ---------------------

actual="$(ECOLOGITS_METRICS="gwp wcf energy adpe pe" \
  STUB_LINE="2.5 1.25 3.75 1.5 12.5" run_bar "$STATUS_JSON" | plain)"
check "values above 1 render in the base units" \
  "🔥 2.50 kgCO₂eq | 💧 1.25 L | ⚡️ 3.75 kWh | ⛏️ 1.50 kgSbeq | 🛢️ 12.50 MJ" \
  "$actual"

# --- Case: sub-microgram ADPe never renders as a bare 0 --------------------
# A rendered "0" means "unmeasured" everywhere in this bar. ADPe for a
# single small-model call is nanograms of antimony equivalent, and rounding
# that to "0 µgSbeq" reported a real measurement as an absent one — the
# exact confusion the zero-means-unmeasured rule exists to prevent.

adpe_only() {
  # $1 = kgSbeq value
  ECOLOGITS_METRICS="adpe" STUB_LINE="0 0 0 $1 0" run_bar "$STATUS_JSON" | plain
}

check "ADPe just under a microgram keeps two decimals" \
  "⛏️ 0.50 µgSbeq" "$(adpe_only 0.0000000005)"

check "ADPe in the nanogram range renders in nanograms" \
  "⛏️ 5.00 ngSbeq" "$(adpe_only 0.000000000005)"

check "ADPe below the finest unit says so rather than rounding to 0" \
  "⛏️ <0.01 ngSbeq" "$(adpe_only 0.000000000000001)"

check "ADPe of ten micrograms or more stays whole-numbered" \
  "⛏️ 42 µgSbeq" "$(adpe_only 0.000000042)"

# …while a true zero still reads as unmeasured.
check "a true-zero ADPe still renders as 0" "⛏️ 0" "$(adpe_only 0)"

# --- Case: an all-zero answer (nothing stored yet) renders zeros -----------

actual="$(STUB_LINE="0 0 0 0 0" run_bar "$STATUS_JSON" | plain)"
check "zeros render as 0, not as a small-looking quantity" \
  "🔥 0 | 💧 0 | ⚡️ 0" "$actual"

# --- Case: `af` fails -> zeros, and the bar still exits 0 ------------------

# The exit code is asserted on the bar itself, not on a pipeline (whose
# status would be the last command's, i.e. `plain`'s). The whole thing runs
# in an explicit subshell because assignments prefixing a *function* call
# persist in the calling shell — `STUB_EXIT=1` would otherwise leak into
# every later case and quietly turn this suite into one long zeros test.
if ( STUB_EXIT=1 STUB_LINE="$GOLDEN_LINE" run_bar "$STATUS_JSON" >"$WORK/out" ); then
  pass "a failing af exits the bar 0"
else
  fail "a failing af exits the bar 0" "exit 0" "exit $?"
fi
check "a failing af renders zeros" "🔥 0 | 💧 0 | ⚡️ 0" "$(plain <"$WORK/out")"

# --- Case: `af` is not installed at all -> zeros ---------------------------

actual="$(printf '%s' "$STATUS_JSON" | AF_BIN="$WORK/no-such-binary" \
  HOME="$WORK/home" bash "$BAR" | plain)"
check "a missing af binary renders zeros" "🔥 0 | 💧 0 | ⚡️ 0" "$actual"

# --- Case: `af` answers garbage -> zeros, never a bogus number -------------

# `nan` is deliberately *not* among the tokens: `v+0` on it is
# implementation-defined (mawk yields nan, BSD awk yields 0), so asserting
# either way would be asserting a property of the test runner's awk. `af`
# itself never emits it — non-finite values are formatted as `0`.
actual="$(STUB_LINE="banana - -1 " run_bar "$STATUS_JSON" | plain)"
check "a non-numeric answer renders zeros" \
  "🔥 0 | 💧 0 | ⚡️ 0" "$actual"

# --- Case: short answer -> the missing fields render zeros -----------------

actual="$(ECOLOGITS_METRICS="gwp pe" STUB_LINE="0.5" run_bar "$STATUS_JSON" | plain)"
check "a truncated answer zeroes the missing fields only" \
  "🔥 500 gCO₂eq | 🛢️ 0" "$actual"

# --- Case: no session_id and no transcript_path -> the input hint ----------

actual="$(STUB_LINE="$GOLDEN_LINE" run_bar '{}' | plain)"
check "an unusable payload prints the captured-stdin hint" \
  "🤖 agentic-footprint: no input — is your captured stdin named \$input?" \
  "$actual"

# --- Case: no model in the payload -> the model metric says so -------------

actual="$(ECOLOGITS_METRICS="model" STUB_LINE="$GOLDEN_LINE" \
  run_bar '{"session_id":"sess-basic"}' | plain)"
check "a payload with no model renders 'unknown'" "🤖 unknown" "$actual"

# --- Case: the user config file is sourced ---------------------------------

mkdir -p "$WORK/home/.claude"
printf ': "${ECOLOGITS_METRICS:=wcf}"\n' >"$WORK/home/.claude/ecologits.config.sh"
actual="$(STUB_LINE="$GOLDEN_LINE" run_bar "$STATUS_JSON" | plain)"
check "~/.claude/ecologits.config.sh is sourced" "💧 0.04 mL" "$actual"

actual="$(ECOLOGITS_METRICS="gwp" STUB_LINE="$GOLDEN_LINE" run_bar "$STATUS_JSON" | plain)"
check "an exported env var still beats the config file" "🔥 188 mgCO₂eq" "$actual"
rm -rf "$WORK/home/.claude"

# --- Case: the real `af`, with nothing stored ------------------------------
# The stub cases above cannot catch a drift in the actual invocation (a
# renamed subcommand, a flag the binary now requires). This one calls the
# real binary against an empty state dir: the honest answer is zeros, and
# the bar must render them without an error line.

REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
AF_REAL="$REPO_ROOT/target/debug/af"
if [ ! -x "$AF_REAL" ] && command -v cargo >/dev/null 2>&1; then
  echo "building af-cli..."
  (cd "$REPO_ROOT" && cargo build -p af-cli -q)
fi
if [ -x "$AF_REAL" ]; then
  mkdir -p "$WORK/state"
  actual="$(printf '%s' "$STATUS_JSON" | AF_BIN="$AF_REAL" AF_STATE_DIR="$WORK/state" \
    HOME="$WORK/home" bash "$BAR" 2>"$WORK/err" | plain)"
  check "the real af with an empty state dir renders zeros" \
    "🔥 0 | 💧 0 | ⚡️ 0" "$actual"
  check "the real af writes nothing to stderr through the bar" "" "$(cat "$WORK/err")"
  check "the real af creates no database when asked for a statusline" \
    "" "$(ls "$WORK/state")"
else
  echo "# skipped: target/debug/af not built and cargo unavailable"
fi

# --- Summary ---------------------------------------------------------------

echo "----"
echo "$CASES cases, $FAILURES failed"
if [ "$FAILURES" -gt 0 ]; then
  exit 1
fi
