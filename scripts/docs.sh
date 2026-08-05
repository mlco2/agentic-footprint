#!/bin/sh
set -eu

ACTION=${1:-build}
DOCS_HOST=${DOCS_HOST:-localhost}
DOCS_PORT=${DOCS_PORT:-8000}

case "$ACTION" in
  build)
    exec uvx --from zensical==0.0.44 zensical build -f mkdocs.yml
    ;;
  serve)
    exec uvx --from zensical==0.0.44 zensical serve -f mkdocs.yml \
      --dev-addr "$DOCS_HOST:$DOCS_PORT"
    ;;
  *)
    echo "usage: scripts/docs.sh [build|serve]" >&2
    exit 2
    ;;
esac
