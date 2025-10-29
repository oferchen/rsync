#!/bin/sh
set -eu
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
CROSS_BIN="${SCRIPT_DIR}/bin"
case ":$PATH:" in
    *:"${CROSS_BIN}":*) ;;
    *) PATH="${CROSS_BIN}:$PATH" ;;
esac
exec "$@"
