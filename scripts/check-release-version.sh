#!/bin/sh
set -eu

TAG=${1:?usage: scripts/check-release-version.sh vX.Y.Z}
VERSION=${TAG#v}

[ "$TAG" != "$VERSION" ] || {
  echo "release tag must start with v: $TAG" >&2
  exit 1
}

WORKSPACE_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
[ "$VERSION" = "$WORKSPACE_VERSION" ] || {
  echo "tag version $VERSION does not match workspace version $WORKSPACE_VERSION" >&2
  exit 1
}

LOCK_VERSION=$(awk '
  $0 == "name = \"af-cli\"" { found = 1; next }
  found && /^version = / { gsub(/^version = \"|\"$/, ""); print; exit }
' Cargo.lock)
[ "$VERSION" = "$LOCK_VERSION" ] || {
  echo "tag version $VERSION does not match Cargo.lock af-cli version $LOCK_VERSION" >&2
  exit 1
}
