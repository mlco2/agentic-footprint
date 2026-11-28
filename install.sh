#!/bin/sh
# Install the agentic-footprint `afoot` binary, then run its native setup wizard.
#
# Current distribution-neutral inputs:
#   AFOOT_INSTALL_BINARY=/path/to/afoot       install an already-built binary
#   AFOOT_BINARY_URL=https://.../afoot        download a release binary directly
#   AFOOT_BINARY_SHA256=<hex>               required SHA-256 of AFOOT_BINARY_URL
#   AFOOT_SOURCE_URL=https://.../src.tar.gz download source and build with Cargo
#   AFOOT_SOURCE_SHA256=<hex>               required SHA-256 of AFOOT_SOURCE_URL
#
# From a source checkout, no environment variable is needed:
#   ./install.sh
#
# Once a canonical release host exists, the public curl command only needs to
# set AFOOT_BINARY_URL and AFOOT_BINARY_SHA256 before piping this script.
set -eu

BIN_DIR="${AFOOT_INSTALL_BIN_DIR:-$HOME/.local/bin}"
RUN_SETUP=1
RUN_PYTHON_SETUP=1
ASSUME_YES=0
PROJECT="$PWD"
SETUP_ARGS=""

usage() {
  cat <<'USAGE'
Usage: install.sh [options] [-- <afoot setup arguments>]

Options:
  --bin-dir DIR     install `afoot` into DIR (default: ~/.local/bin)
  --project DIR     project directory passed to `afoot setup`
  --yes             apply wizard changes without prompting
  --no-python       skip managed Python runtime provisioning
  --no-setup        install only; do not run `afoot setup`
  -h, --help        show this help

Inputs for non-checkout installation:
  AFOOT_INSTALL_BINARY path to an existing `afoot` binary
  AFOOT_BINARY_URL     URL of a standalone `afoot` binary
  AFOOT_BINARY_SHA256  SHA-256 of the AFOOT_BINARY_URL download (required)
  AFOOT_SOURCE_URL     source archive URL to build when no binary URL is available
  AFOOT_SOURCE_SHA256  SHA-256 of the AFOOT_SOURCE_URL download (required)
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin-dir)
      [ "$#" -ge 2 ] || { echo "install.sh: --bin-dir needs a value" >&2; exit 2; }
      BIN_DIR=$2
      shift 2
      ;;
    --project)
      [ "$#" -ge 2 ] || { echo "install.sh: --project needs a value" >&2; exit 2; }
      PROJECT=$2
      shift 2
      ;;
    --yes|-y)
      ASSUME_YES=1
      shift
      ;;
    --no-setup)
      RUN_SETUP=0
      shift
      ;;
    --no-python)
      RUN_PYTHON_SETUP=0
      shift
      ;;
    --)
      shift
      SETUP_ARGS="$*"
      break
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "install.sh: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "install.sh: required command not found: $1" >&2
    exit 1
  }
}

binary_version() {
  version="$("$1" --version 2>/dev/null | head -n 1 || true)"
  if [ -n "$version" ]; then
    printf '%s\n' "$version"
  else
    printf '%s\n' "unknown (this binary predates version reporting)"
  fi
}

confirm_upgrade() {
  [ "$ASSUME_YES" -eq 0 ] || return 0

  prompt="Upgrade afoot at $DEST? [Y/n] "
  answer=""
  if [ -r /dev/tty ] && [ -w /dev/tty ] && tty </dev/tty >/dev/null 2>&1; then
    printf '%s' "$prompt" >/dev/tty
    IFS= read -r answer </dev/tty || answer=""
  else
    printf '%s' "$prompt" >&2
    if ! IFS= read -r answer; then
      echo >&2
      echo "install.sh: existing installation requires confirmation; rerun with --yes" >&2
      exit 1
    fi
  fi

  case "$answer" in
    ""|y|Y|yes|YES|Yes) return 0 ;;
    *) return 1 ;;
  esac
}

build_source_checkout() {
  source_dir=$1
  need cargo
  need npm
  echo "Building console assets"
  (cd "$source_dir" && npm --prefix console ci && npm --prefix console run build)
  echo "Building afoot from source"
  (cd "$source_dir" && cargo build --release -p afoot-cli)
}

make_temp_dir() {
  if command -v mktemp >/dev/null 2>&1; then
    mktemp -d "${TMPDIR:-/tmp}/afoot-install.XXXXXX"
  else
    dir="${TMPDIR:-/tmp}/afoot-install.$$"
    (umask 077 && mkdir "$dir")
    printf '%s\n' "$dir"
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "install.sh: checksum requested but sha256sum/shasum is unavailable" >&2
    exit 1
  fi
}

# Refuses a download that does not match the SHA-256 the caller supplied. A
# URL alone is not a source of trust: whoever serves it (or sits between)
# decides what runs next, with the user's permissions.
verify_download() {
  actual="$(sha256_file "$1")"
  expected="$(printf '%s' "$2" | tr 'A-F' 'a-f')"
  [ "$actual" = "$expected" ] || {
    echo "install.sh: checksum mismatch for $3" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    exit 1
  }
}

require_checksum() {
  [ -n "$2" ] || {
    echo "install.sh: $1 needs ${1%_URL}_SHA256, the SHA-256 of what it points at" >&2
    echo "  (from the release's SHA256SUMS); nothing was downloaded" >&2
    exit 1
  }
}

WORK="$(make_temp_dir)"
trap 'rm -rf "$WORK"' EXIT HUP INT TERM
SOURCE_BINARY=""

if [ -n "${AFOOT_INSTALL_BINARY:-}" ]; then
  SOURCE_BINARY=$AFOOT_INSTALL_BINARY
  [ -f "$SOURCE_BINARY" ] || {
    echo "install.sh: AFOOT_INSTALL_BINARY does not exist: $SOURCE_BINARY" >&2
    exit 1
  }
elif [ -n "${AFOOT_BINARY_URL:-}" ]; then
  require_checksum AFOOT_BINARY_URL "${AFOOT_BINARY_SHA256:-}"
  need curl
  SOURCE_BINARY="$WORK/afoot"
  echo "Downloading afoot from $AFOOT_BINARY_URL"
  curl --fail --location --silent --show-error "$AFOOT_BINARY_URL" --output "$SOURCE_BINARY"
  verify_download "$SOURCE_BINARY" "$AFOOT_BINARY_SHA256" "$AFOOT_BINARY_URL"
elif [ -f "Cargo.toml" ] && [ -f "crates/afoot-cli/Cargo.toml" ]; then
  build_source_checkout "$PWD"
  SOURCE_BINARY="$PWD/target/release/afoot"
elif [ -n "${AFOOT_SOURCE_URL:-}" ]; then
  require_checksum AFOOT_SOURCE_URL "${AFOOT_SOURCE_SHA256:-}"
  need curl
  need cargo
  need tar
  ARCHIVE="$WORK/source.tar.gz"
  SOURCE_DIR="$WORK/source"
  mkdir -p "$SOURCE_DIR"
  echo "Downloading afoot source from $AFOOT_SOURCE_URL"
  curl --fail --location --silent --show-error "$AFOOT_SOURCE_URL" --output "$ARCHIVE"
  verify_download "$ARCHIVE" "$AFOOT_SOURCE_SHA256" "$AFOOT_SOURCE_URL"
  tar -xzf "$ARCHIVE" -C "$SOURCE_DIR" --strip-components=1
  build_source_checkout "$SOURCE_DIR"
  SOURCE_BINARY="$SOURCE_DIR/target/release/afoot"
else
  cat >&2 <<'ERROR'
install.sh: no install source is available.

Run this script from an agentic-footprint source checkout, or provide one of:
  AFOOT_INSTALL_BINARY=/path/to/afoot
  AFOOT_BINARY_URL=https://host/path/to/afoot
  AFOOT_SOURCE_URL=https://host/path/to/source.tar.gz
ERROR
  exit 1
fi

[ -x "$SOURCE_BINARY" ] || chmod +x "$SOURCE_BINARY"
mkdir -p "$BIN_DIR"
DEST="$BIN_DIR/afoot"
TEMP_DEST="$BIN_DIR/.afoot.$$.tmp"

if [ -e "$DEST" ]; then
  [ -f "$DEST" ] || {
    echo "install.sh: install destination is not a file: $DEST" >&2
    exit 1
  }
  if cmp -s "$SOURCE_BINARY" "$DEST"; then
    echo "afoot is already up to date at $DEST ($(binary_version "$DEST"))"
  else
    echo "Existing afoot installation detected at $DEST"
    echo "  installed: $(binary_version "$DEST")"
    echo "  candidate: $(binary_version "$SOURCE_BINARY")"
    if ! confirm_upgrade; then
      echo "Upgrade cancelled; existing binary left unchanged."
      exit 0
    fi
  fi
fi

cp "$SOURCE_BINARY" "$TEMP_DEST"
chmod +x "$TEMP_DEST"
mv "$TEMP_DEST" "$DEST"

echo "Installed afoot to $DEST ($(binary_version "$DEST"))"
case ":${PATH:-}:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Add $BIN_DIR to PATH to invoke afoot directly." ;;
esac

# Only a published release binary can be self-updated later; a source build
# or AFOOT_INSTALL_BINARY copy must be updated the way it was installed.
case "${AFOOT_BINARY_URL:-}" in
  https://github.com/mlco2/agentic-footprint/releases/download/*)
    "$DEST" self record-install --binary "$DEST"
    ;;
esac

RESOLVED_AFOOT="$(command -v afoot 2>/dev/null || true)"
if [ -n "$RESOLVED_AFOOT" ] && [ "$RESOLVED_AFOOT" != "$DEST" ]; then
  echo "Warning: your shell currently resolves afoot to $RESOLVED_AFOOT" >&2
  echo "         put $BIN_DIR earlier in PATH or invoke $DEST directly" >&2
fi

if [ "$RUN_PYTHON_SETUP" -eq 1 ]; then
  echo "Provisioning the managed Python runtime"
  "$DEST" python setup
fi

[ "$RUN_SETUP" -eq 1 ] || exit 0

set -- setup --global --project "$PROJECT"
if [ "$ASSUME_YES" -eq 1 ]; then
  set -- "$@" --yes
fi
if [ -n "$SETUP_ARGS" ]; then
  # Setup passthrough is intentionally shell-word based for curl convenience.
  # Users needing literal whitespace can invoke the installed `afoot setup`.
  # shellcheck disable=SC2086
  set -- "$@" $SETUP_ARGS
fi

if [ "$ASSUME_YES" -eq 0 ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then
  "$DEST" "$@" </dev/tty >/dev/tty
else
  "$DEST" "$@"
fi
