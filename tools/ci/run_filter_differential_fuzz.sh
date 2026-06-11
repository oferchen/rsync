#!/usr/bin/env bash
# run_filter_differential_fuzz.sh - drive the filter_rules_vs_upstream fuzz
# target for a bounded duration and report whether any divergence was found.
#
# Differentially fuzzes oc-rsync's filter chain against an upstream rsync
# binary by replaying the same randomly-generated rules + path through both
# engines. Default duration is 300 seconds (FUZZ_DURATION). Findings are
# preserved under fuzz/artifacts/filter_rules_vs_upstream/.
#
# Usage:
#   bash tools/ci/run_filter_differential_fuzz.sh                       # 5 min
#   FUZZ_DURATION=900 bash tools/ci/run_filter_differential_fuzz.sh     # 15 min
#   OC_RSYNC_UPSTREAM_BIN=/path/to/rsync \
#       bash tools/ci/run_filter_differential_fuzz.sh                   # pin upstream
#
# Exit codes:
#   0   no divergence detected within the time budget, or the harness was
#       skipped cleanly (missing nightly, missing cargo-fuzz, missing upstream
#       rsync, or unsupported platform)
#   1   a divergence was detected and reproducer artifacts were written
#   2   internal error (missing cargo binary)

set -uo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fuzz_dir="${workspace_root}/fuzz"
target_name="filter_rules_vs_upstream"
duration="${FUZZ_DURATION:-300}"

skip() {
    echo "skip: $*" >&2
    exit 0
}

# cargo-fuzz only ships sanitizer support for Linux and macOS. Skip cleanly on
# every other platform so the script can be wired into a multi-OS CI matrix
# without conditional branches in the workflow.
uname_s=$(uname -s)
case "${uname_s}" in
    Linux|Darwin) ;;
    *) skip "platform ${uname_s} is not supported by cargo-fuzz" ;;
esac

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required" >&2
    exit 2
fi

# A nightly toolchain is required by cargo-fuzz. Treat absence as a clean skip
# so the script is safe to invoke unconditionally in CI.
if ! cargo +nightly --version >/dev/null 2>&1; then
    skip "nightly Rust toolchain not installed (rustup toolchain install nightly)"
fi
if ! cargo +nightly fuzz --help >/dev/null 2>&1; then
    skip "cargo-fuzz not installed (cargo install cargo-fuzz)"
fi

# Discover an upstream rsync binary. Absent upstream is a clean skip: there is
# nothing to compare against.
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
    skip "no upstream rsync binary reachable (set OC_RSYNC_UPSTREAM_BIN or run tools/ci/run_interop.sh)"
fi

echo "upstream rsync: ${upstream}"
echo "fuzz target:   ${target_name}"
echo "duration:      ${duration}s"

host_triple=$(rustc -vV | awk '/^host:/ { print $2 }')

cd "${fuzz_dir}"
set +e
env OC_RSYNC_UPSTREAM_BIN="${upstream}" \
    cargo +nightly fuzz run "${target_name}" \
        --target "${host_triple}" \
        -- -max_total_time="${duration}"
status=$?
set -e

artifacts_dir="${fuzz_dir}/artifacts/${target_name}"
if [[ -d "${artifacts_dir}" ]] && find "${artifacts_dir}" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; then
    echo "divergence found: see ${artifacts_dir}" >&2
    exit 1
fi

if [[ ${status} -ne 0 ]]; then
    echo "fuzzer exited ${status} without recording artifacts" >&2
    exit 1
fi

echo "no divergence detected within ${duration}s"
exit 0
