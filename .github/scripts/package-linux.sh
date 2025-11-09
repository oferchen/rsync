#!/usr/bin/env bash
set -euo pipefail

TARGET="$1"

# install packagers
cargo install cargo-deb --locked
cargo install cargo-generate-rpm --locked

# build deb without rebuilding the binary (we already built it)
cargo deb --target "${TARGET}" --no-build

# build rpm
cargo generate-rpm --target "${TARGET}"

