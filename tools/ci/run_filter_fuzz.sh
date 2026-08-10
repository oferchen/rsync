#!/usr/bin/env bash
# run_filter_fuzz.sh - drive the filter_differential fuzz target.
#
# Differentially fuzzes oc-rsync's filter engine against the upstream rsync
# binary. Default duration is 60 seconds, which is enough for a smoke check.
# For a meaningful local hunt, bump FUZZ_SECONDS to 14400 (4 hours) or more.
#
# Usage:
#   bash tools/ci/run_filter_fuzz.sh                       # 60-second smoke
#   FUZZ_SECONDS=600 bash tools/ci/run_filter_fuzz.sh      # 10-minute run
#   FUZZ_SECONDS=14400 bash tools/ci/run_filter_fuzz.sh    # 4-hour soak
#   OC_RSYNC_UPSTREAM_BIN=/path/to/rsync \
#       bash tools/ci/run_filter_fuzz.sh                   # pin upstream binary
#
# Findings (panics, divergences) land in fuzz/artifacts/filter_differential/.

set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fuzz_dir="${workspace_root}/fuzz"
target_name="filter_differential"
duration="${FUZZ_SECONDS:-60}"

if ! command -v cargo >/dev/null; then
    echo "error: cargo is required" >&2
    exit 2
fi

# cargo-fuzz needs nightly. We do not auto-install: a missing toolchain is a
# clear actionable error and avoids silent rustup downloads in CI.
if ! cargo +nightly --version >/dev/null 2>&1; then
    echo "error: a nightly Rust toolchain is required (rustup toolchain install nightly)" >&2
    exit 2
fi
if ! cargo +nightly fuzz --help >/dev/null 2>&1; then
    echo "error: cargo-fuzz is required (cargo install cargo-fuzz)" >&2
    exit 2
fi

# Try to discover an upstream binary so the user knows up front whether the
# differential probe will actually fire.
upstream="${OC_RSYNC_UPSTREAM_BIN:-}"
if [[ -z "${upstream}" ]]; then
    for candidate in \
        "${workspace_root}/target/interop/upstream-install/3.4.4/bin/rsync" \
        /opt/homebrew/bin/rsync \
        /usr/local/bin/rsync \
        /usr/bin/rsync; do
        if [[ -x "${candidate}" ]]; then
            upstream="${candidate}"
            break
        fi
    done
fi

if [[ -z "${upstream}" ]]; then
    echo "warning: no upstream rsync binary found - the harness will only" >&2
    echo "         exercise the oc-rsync side. Set OC_RSYNC_UPSTREAM_BIN" >&2
    echo "         or run tools/ci/run_interop.sh to fetch one." >&2
else
    echo "upstream rsync: ${upstream}"
fi

echo "fuzz target:   ${target_name}"
echo "duration:      ${duration}s"

host_triple=$(rustc -vV | awk '/^host:/ { print $2 }')

cd "${fuzz_dir}"
exec env OC_RSYNC_UPSTREAM_BIN="${upstream}" \
    cargo +nightly fuzz run "${target_name}" \
        --target "${host_triple}" \
        -- -max_total_time="${duration}"
