#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required to run enforce-limits; install Rust and ensure cargo is on PATH" >&2
    exit 127
fi

cd "${repo_root}"

exec cargo run -p xtask -- enforce-limits "$@"
