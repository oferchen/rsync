#!/usr/bin/env bash
# Compile-gate for the standalone fuzz workspaces.
#
# The fuzz workspaces are deliberately excluded from the root cargo workspace
# (`Cargo.toml` `exclude`) so `libfuzzer-sys` never enters ordinary builds.
# The flip side is that no PR gate compiles them: API drift in a main
# workspace crate can break a fuzz target and stay invisible until a
# scheduled fuzz workflow runs. This script closes that hole with a plain
# `cargo check` plus `cargo fmt --check` per fuzz workspace.
#
# No fuzzing and no sanitizer instrumentation happen here: `cargo check`
# type-checks the fuzz targets without linking libFuzzer, so the stable
# toolchain is sufficient and the run stays cheap and cache-friendly.

set -euo pipefail

cd "$(dirname "$0")/../.."

FUZZ_WORKSPACES=(
    fuzz
    crates/protocol/fuzz
    crates/filters/fuzz
)

for ws in "${FUZZ_WORKSPACES[@]}"; do
    if [[ ! -f "${ws}/Cargo.toml" ]]; then
        echo "error: expected fuzz workspace ${ws}/Cargo.toml is missing" >&2
        exit 1
    fi
    echo "==> cargo check (${ws})"
    cargo check --manifest-path "${ws}/Cargo.toml"
    echo "==> cargo fmt --check (${ws})"
    cargo fmt --manifest-path "${ws}/Cargo.toml" --all -- --check
done
