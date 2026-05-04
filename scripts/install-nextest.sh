#!/usr/bin/env bash
set -euo pipefail

if command -v cargo-nextest >/dev/null 2>&1; then
    echo "cargo-nextest already installed: $(cargo-nextest --version)"
    exit 0
fi

echo "Installing cargo-nextest with locked dependencies..."
cargo install cargo-nextest --locked

