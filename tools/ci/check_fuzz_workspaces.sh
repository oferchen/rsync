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
#
# Lockfile drift is tolerated by design. The fuzz workspaces are excluded
# from the root workspace, so a dependency edge a main-workspace crate gains
# (e.g. a new `thiserror` use) never refreshes their standalone lockfiles.
# `cargo check` is run online and WITHOUT `--locked`: in one pass it both
# minimally re-syncs each fuzz lockfile against the current manifests and
# type-checks every fuzz target. A genuine API-drift compile break still
# fails loudly; only stale-lockfile false positives self-heal. An offline
# pre-sync (`cargo update --offline`) cannot do this - it dies on a cold
# registry cache and leaves `--locked` to reject the very drift this guard
# exists to absorb. Dropping `--locked` here is the sole sanctioned
# exception, recorded in tools/ci/check_locked_flags.sh's ALLOWLIST.

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
