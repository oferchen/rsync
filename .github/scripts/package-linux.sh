#!/usr/bin/env bash
set -euo pipefail

TARGET="$1"

# install packagers
cargo install cargo-deb --locked
cargo install cargo-generate-rpm --locked

# stage the canonical binary for packagers
BIN_PATH="target/${TARGET}/release/oc-rsync"
if [ ! -f "${BIN_PATH}" ]; then
  echo "expected ${BIN_PATH} to exist; build step must run first" >&2
  exit 1
fi

install -Dm755 "${BIN_PATH}" "target/dist/oc-rsync"

# build deb without rebuilding the binary (we already built it)
cargo deb --target "${TARGET}" --no-build

# build rpm
cargo generate-rpm --target "${TARGET}"

