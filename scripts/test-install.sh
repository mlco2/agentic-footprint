#!/bin/sh
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TMP="$(mktemp -d)"
cleanup() {
  if [ -f "$TMP/watch.pid" ]; then
    kill "$(cat "$TMP/watch.pid")" 2>/dev/null || true
    wait "$(cat "$TMP/watch.pid")" 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$TMP/fake-bin" "$TMP/project" "$TMP/home" "$TMP/install-bin"
for name in codex claude opencode jq; do
  cat >"$TMP/fake-bin/$name" <<'SH'
#!/bin/sh
exit 0
SH
  chmod +x "$TMP/fake-bin/$name"
done
cat >"$TMP/fake-bin/uv" <<'SH'
#!/bin/sh
# afoot runs uv without configuration discovery; anything else is a regression.
if [ "$1" = "--no-config" ]; then
  shift
elif [ "$1" != "--version" ]; then
  echo "fake uv: expected --no-config, got: $*" >&2
  exit 1
fi
if [ "$1" = "--version" ]; then
  echo "uv 0-test"
  exit 0
fi
if [ "$1" = "venv" ]; then
  target=$4
  mkdir -p "$target/bin"
  cat >"$target/bin/python" <<'PY'
#!/bin/sh
exit 0
PY
  chmod +x "$target/bin/python"
  exit 0
fi
if [ "$1" = "pip" ]; then
  exit 0
fi
exit 1
SH
chmod +x "$TMP/fake-bin/uv"
cat >"$TMP/fake-bin/systemctl" <<'SH'
#!/bin/sh
set -eu
while [ "${1:-}" = "--user" ]; do shift; done
case "${1:-}" in
  daemon-reload) exit 0 ;;
  enable)
    shift
    [ "${1:-}" = "--now" ] && shift
    exit 0
    ;;
  restart)
    if [ -f "$AFOOT_TEST_WATCH_PID" ]; then
      kill "$(cat "$AFOOT_TEST_WATCH_PID")" 2>/dev/null || true
    fi
    "$AFOOT_TEST_AFOOT_BINARY" watch --otlp-addr "$AFOOT_TEST_OTLP_ADDR" >"$AFOOT_TEST_WATCH_LOG" 2>&1 &
    echo $! >"$AFOOT_TEST_WATCH_PID"
    exit 0
    ;;
  status)
    test -f "$AFOOT_TEST_WATCH_PID"
    kill -0 "$(cat "$AFOOT_TEST_WATCH_PID")"
    exit 0
    ;;
  is-active)
    [ "${2:-}" = "--quiet" ] && shift
    test -f "$AFOOT_TEST_WATCH_PID"
    kill -0 "$(cat "$AFOOT_TEST_WATCH_PID")"
    exit 0
    ;;
esac
exit 1
SH
chmod +x "$TMP/fake-bin/systemctl"

(cd "$REPO_ROOT" && cargo build -p afoot-cli -q)
# A free loopback port, so a developer's own receiver on 4318 is neither
# disturbed nor mistaken for the one under test.
AFOOT_TEST_OTLP_ADDR="127.0.0.1:$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
export AFOOT_TEST_OTLP_ADDR
ENDPOINT="http://$AFOOT_TEST_OTLP_ADDR/v1/logs"
HOME="$TMP/home" \
CODEX_HOME="$TMP/home/.codex" \
AFOOT_STATE_DIR="$TMP/home/.local/state/agentic-footprint" \
AFOOT_INSTALL_BINARY="$REPO_ROOT/target/debug/afoot" \
AFOOT_INSTALL_BIN_DIR="$TMP/install-bin" \
AFOOT_SERVICE_MANAGER=systemd \
AFOOT_TEST_AFOOT_BINARY="$TMP/install-bin/afoot" \
AFOOT_TEST_WATCH_PID="$TMP/watch.pid" \
AFOOT_TEST_WATCH_LOG="$TMP/watch.log" \
XDG_CONFIG_HOME="$TMP/home/.config" \
XDG_RUNTIME_DIR="$TMP/runtime" \
PATH="$TMP/fake-bin:$PATH" \
  "$REPO_ROOT/install.sh" --yes --project "$TMP/project" -- --endpoint "$ENDPOINT" >/dev/null

test -x "$TMP/install-bin/afoot"
test -f "$TMP/home/.codex/config.toml"
test -f "$TMP/home/.claude/settings.json"
test -x "$TMP/home/.local/state/agentic-footprint/integrations/claude-code/afoot-hook.sh"
test -f "$TMP/home/.local/state/agentic-footprint/python/afoot_sampler/__main__.py"
test -f "$TMP/home/.local/state/agentic-footprint/python/afoot_estimator/__main__.py"
# A local binary is not a release install: self update must refuse it.
test ! -e "$TMP/home/.local/state/agentic-footprint/install-receipt.json"
test -f "$TMP/home/.config/systemd/user/agentic-footprint-watch.service"
grep -q "ExecStart=.*/afoot watch --require-receiver --otlp-addr $AFOOT_TEST_OTLP_ADDR" \
  "$TMP/home/.config/systemd/user/agentic-footprint-watch.service"
! grep -q -- '--debug' "$TMP/home/.config/systemd/user/agentic-footprint-watch.service"
kill -0 "$(cat "$TMP/watch.pid")"

HOME="$TMP/home" \
CODEX_HOME="$TMP/home/.codex" \
AFOOT_STATE_DIR="$TMP/home/.local/state/agentic-footprint" \
AFOOT_SERVICE_MANAGER=systemd \
AFOOT_TEST_AFOOT_BINARY="$TMP/install-bin/afoot" \
AFOOT_TEST_WATCH_PID="$TMP/watch.pid" \
AFOOT_TEST_WATCH_LOG="$TMP/watch.log" \
XDG_CONFIG_HOME="$TMP/home/.config" \
XDG_RUNTIME_DIR="$TMP/runtime" \
PATH="$TMP/fake-bin:$PATH" \
  "$TMP/install-bin/afoot" setup --check --global --project "$TMP/project" --endpoint "$ENDPOINT" >/dev/null

HOME="$TMP/home" \
AFOOT_STATE_DIR="$TMP/home/.local/state/agentic-footprint" \
AFOOT_SERVICE_MANAGER=systemd \
AFOOT_TEST_AFOOT_BINARY="$TMP/install-bin/afoot" \
AFOOT_TEST_WATCH_PID="$TMP/watch.pid" \
AFOOT_TEST_WATCH_LOG="$TMP/watch.log" \
XDG_CONFIG_HOME="$TMP/home/.config" \
XDG_RUNTIME_DIR="$TMP/runtime" \
PATH="$TMP/fake-bin:$PATH" \
  "$TMP/install-bin/afoot" service install --endpoint "$ENDPOINT" >/dev/null
kill -0 "$(cat "$TMP/watch.pid")"

UPGRADE_DIR="$TMP/upgrade-bin"
mkdir -p "$UPGRADE_DIR"
cat >"$UPGRADE_DIR/afoot" <<'SH'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then echo "afoot 0.0.1"; exit 0; fi
echo old
SH
cat >"$TMP/candidate-afoot" <<'SH'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then echo "afoot 0.1.0"; exit 0; fi
echo new
SH
chmod +x "$UPGRADE_DIR/afoot" "$TMP/candidate-afoot"

printf 'n\n' | HOME="$TMP/home" \
  AFOOT_INSTALL_BINARY="$TMP/candidate-afoot" \
  AFOOT_INSTALL_BIN_DIR="$UPGRADE_DIR" \
  "$REPO_ROOT/install.sh" --no-python --no-setup >"$TMP/decline.log" 2>&1
grep -Fq "Upgrade afoot at $UPGRADE_DIR/afoot? [Y/n]" "$TMP/decline.log"
test "$("$UPGRADE_DIR/afoot")" = old

HOME="$TMP/home" \
  AFOOT_INSTALL_BINARY="$TMP/candidate-afoot" \
  AFOOT_INSTALL_BIN_DIR="$UPGRADE_DIR" \
  "$REPO_ROOT/install.sh" --yes --no-python --no-setup >"$TMP/upgrade.log" 2>&1
grep -q 'installed: afoot 0.0.1' "$TMP/upgrade.log"
grep -q 'candidate: afoot 0.1.0' "$TMP/upgrade.log"
test "$("$UPGRADE_DIR/afoot")" = new

HOME="$TMP/home" \
  AFOOT_INSTALL_BINARY="$TMP/candidate-afoot" \
  AFOOT_INSTALL_BIN_DIR="$UPGRADE_DIR" \
  "$REPO_ROOT/install.sh" --no-python --no-setup >"$TMP/current.log" 2>&1
grep -q 'already up to date' "$TMP/current.log"

# A URL install without a checksum is refused before anything is fetched:
# the URL is unreachable, so reaching the download would fail differently.
for input in AFOOT_BINARY_URL AFOOT_SOURCE_URL; do
  if env HOME="$TMP/home" AFOOT_INSTALL_BIN_DIR="$TMP/url-bin" \
      "$input=http://127.0.0.1:9/afoot" \
      sh -c 'cd / && "$0" --no-python --no-setup' "$REPO_ROOT/install.sh" >"$TMP/url.log" 2>&1; then
    echo "install.sh accepted $input without a checksum" >&2
    exit 1
  fi
  grep -q "SHA256" "$TMP/url.log" || { cat "$TMP/url.log" >&2; exit 1; }
  ! grep -q "Downloading" "$TMP/url.log"
  test ! -e "$TMP/url-bin/afoot"
done

echo "ok - install.sh installs afoot and completes wizard setup"
