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
# af runs uv without configuration discovery; anything else is a regression.
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
    if [ -f "$AF_TEST_WATCH_PID" ]; then
      kill "$(cat "$AF_TEST_WATCH_PID")" 2>/dev/null || true
    fi
    "$AF_TEST_AF_BINARY" watch --otlp-addr "$AF_TEST_OTLP_ADDR" >"$AF_TEST_WATCH_LOG" 2>&1 &
    echo $! >"$AF_TEST_WATCH_PID"
    exit 0
    ;;
  status)
    test -f "$AF_TEST_WATCH_PID"
    kill -0 "$(cat "$AF_TEST_WATCH_PID")"
    exit 0
    ;;
  is-active)
    [ "${2:-}" = "--quiet" ] && shift
    test -f "$AF_TEST_WATCH_PID"
    kill -0 "$(cat "$AF_TEST_WATCH_PID")"
    exit 0
    ;;
esac
exit 1
SH
chmod +x "$TMP/fake-bin/systemctl"

(cd "$REPO_ROOT" && cargo build -p af-cli -q)
# A free loopback port, so a developer's own receiver on 4318 is neither
# disturbed nor mistaken for the one under test.
AF_TEST_OTLP_ADDR="127.0.0.1:$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
export AF_TEST_OTLP_ADDR
ENDPOINT="http://$AF_TEST_OTLP_ADDR/v1/logs"
HOME="$TMP/home" \
CODEX_HOME="$TMP/home/.codex" \
AF_STATE_DIR="$TMP/home/.local/state/agentic-footprint" \
AF_INSTALL_BINARY="$REPO_ROOT/target/debug/af" \
AF_INSTALL_BIN_DIR="$TMP/install-bin" \
AF_SERVICE_MANAGER=systemd \
AF_TEST_AF_BINARY="$TMP/install-bin/af" \
AF_TEST_WATCH_PID="$TMP/watch.pid" \
AF_TEST_WATCH_LOG="$TMP/watch.log" \
XDG_CONFIG_HOME="$TMP/home/.config" \
XDG_RUNTIME_DIR="$TMP/runtime" \
PATH="$TMP/fake-bin:$PATH" \
  "$REPO_ROOT/install.sh" --yes --project "$TMP/project" -- --endpoint "$ENDPOINT" >/dev/null

test -x "$TMP/install-bin/af"
test -f "$TMP/home/.codex/config.toml"
test -f "$TMP/home/.claude/settings.json"
test -x "$TMP/home/.local/state/agentic-footprint/integrations/claude-code/af-hook.sh"
test -f "$TMP/home/.local/state/agentic-footprint/python/af_sampler/__main__.py"
test -f "$TMP/home/.local/state/agentic-footprint/python/af_estimator/__main__.py"
# A local binary is not a release install: self update must refuse it.
test ! -e "$TMP/home/.local/state/agentic-footprint/install-receipt.json"
test -f "$TMP/home/.config/systemd/user/agentic-footprint-watch.service"
grep -q "ExecStart=.*/af watch --require-receiver --otlp-addr $AF_TEST_OTLP_ADDR" \
  "$TMP/home/.config/systemd/user/agentic-footprint-watch.service"
! grep -q -- '--debug' "$TMP/home/.config/systemd/user/agentic-footprint-watch.service"
kill -0 "$(cat "$TMP/watch.pid")"

HOME="$TMP/home" \
CODEX_HOME="$TMP/home/.codex" \
AF_STATE_DIR="$TMP/home/.local/state/agentic-footprint" \
AF_SERVICE_MANAGER=systemd \
AF_TEST_AF_BINARY="$TMP/install-bin/af" \
AF_TEST_WATCH_PID="$TMP/watch.pid" \
AF_TEST_WATCH_LOG="$TMP/watch.log" \
XDG_CONFIG_HOME="$TMP/home/.config" \
XDG_RUNTIME_DIR="$TMP/runtime" \
PATH="$TMP/fake-bin:$PATH" \
  "$TMP/install-bin/af" setup --check --global --project "$TMP/project" --endpoint "$ENDPOINT" >/dev/null

HOME="$TMP/home" \
AF_STATE_DIR="$TMP/home/.local/state/agentic-footprint" \
AF_SERVICE_MANAGER=systemd \
AF_TEST_AF_BINARY="$TMP/install-bin/af" \
AF_TEST_WATCH_PID="$TMP/watch.pid" \
AF_TEST_WATCH_LOG="$TMP/watch.log" \
XDG_CONFIG_HOME="$TMP/home/.config" \
XDG_RUNTIME_DIR="$TMP/runtime" \
PATH="$TMP/fake-bin:$PATH" \
  "$TMP/install-bin/af" service install --endpoint "$ENDPOINT" >/dev/null
kill -0 "$(cat "$TMP/watch.pid")"

UPGRADE_DIR="$TMP/upgrade-bin"
mkdir -p "$UPGRADE_DIR"
cat >"$UPGRADE_DIR/af" <<'SH'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then echo "af 0.0.1"; exit 0; fi
echo old
SH
cat >"$TMP/candidate-af" <<'SH'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then echo "af 0.1.0"; exit 0; fi
echo new
SH
chmod +x "$UPGRADE_DIR/af" "$TMP/candidate-af"

printf 'n\n' | HOME="$TMP/home" \
  AF_INSTALL_BINARY="$TMP/candidate-af" \
  AF_INSTALL_BIN_DIR="$UPGRADE_DIR" \
  "$REPO_ROOT/install.sh" --no-python --no-setup >"$TMP/decline.log" 2>&1
grep -Fq "Upgrade af at $UPGRADE_DIR/af? [Y/n]" "$TMP/decline.log"
test "$("$UPGRADE_DIR/af")" = old

HOME="$TMP/home" \
  AF_INSTALL_BINARY="$TMP/candidate-af" \
  AF_INSTALL_BIN_DIR="$UPGRADE_DIR" \
  "$REPO_ROOT/install.sh" --yes --no-python --no-setup >"$TMP/upgrade.log" 2>&1
grep -q 'installed: af 0.0.1' "$TMP/upgrade.log"
grep -q 'candidate: af 0.1.0' "$TMP/upgrade.log"
test "$("$UPGRADE_DIR/af")" = new

HOME="$TMP/home" \
  AF_INSTALL_BINARY="$TMP/candidate-af" \
  AF_INSTALL_BIN_DIR="$UPGRADE_DIR" \
  "$REPO_ROOT/install.sh" --no-python --no-setup >"$TMP/current.log" 2>&1
grep -q 'already up to date' "$TMP/current.log"

# A URL install without a checksum is refused before anything is fetched:
# the URL is unreachable, so reaching the download would fail differently.
for input in AF_BINARY_URL AF_SOURCE_URL; do
  if env HOME="$TMP/home" AF_INSTALL_BIN_DIR="$TMP/url-bin" \
      "$input=http://127.0.0.1:9/af" \
      sh -c 'cd / && "$0" --no-python --no-setup' "$REPO_ROOT/install.sh" >"$TMP/url.log" 2>&1; then
    echo "install.sh accepted $input without a checksum" >&2
    exit 1
  fi
  grep -q "SHA256" "$TMP/url.log" || { cat "$TMP/url.log" >&2; exit 1; }
  ! grep -q "Downloading" "$TMP/url.log"
  test ! -e "$TMP/url-bin/af"
done

echo "ok - install.sh installs af and completes wizard setup"
