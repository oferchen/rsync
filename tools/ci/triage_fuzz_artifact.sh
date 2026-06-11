#!/usr/bin/env bash
# triage_fuzz_artifact.sh - reproduce a crash artifact from one of the
# differential filter fuzz targets locally.
#
# The overnight fuzz workflow uploads any reproducers under
# fuzz/artifacts/<target>/... as a workflow artifact. Download the artifact,
# extract it, and feed each reproducer path to this script. The target is
# inferred from the path; pass --target to override.
#
# Usage:
#   bash tools/ci/triage_fuzz_artifact.sh fuzz/artifacts/filter_differential/crash-abc
#   bash tools/ci/triage_fuzz_artifact.sh --target filter_rules_vs_upstream ./crash-abc
#
# Exit codes:
#   0   reproducer ran cleanly (no divergence reproduced - investigate flake)
#   1   reproducer triggered the recorded divergence (expected during triage)
#   2   invalid invocation or missing tooling

set -uo pipefail

usage() {
    cat >&2 <<'USAGE'
Usage: triage_fuzz_artifact.sh [--target <name>] <artifact-path>

Reproduces a libFuzzer crash artifact against one of the differential filter
fuzz targets (filter_differential or filter_rules_vs_upstream).
USAGE
    exit 2
}

target=""
artifact=""
while (($#)); do
    case "$1" in
        --target)
            shift
            [[ $# -gt 0 ]] || usage
            target="$1"
            ;;
        --target=*)
            target="${1#--target=}"
            ;;
        -h|--help)
            usage
            ;;
        --)
            shift
            artifact="${1:-}"
            break
            ;;
        -*)
            echo "error: unknown flag $1" >&2
            usage
            ;;
        *)
            if [[ -z "${artifact}" ]]; then
                artifact="$1"
            else
                echo "error: unexpected extra positional argument: $1" >&2
                usage
            fi
            ;;
    esac
    shift
done

[[ -n "${artifact}" ]] || usage

if [[ ! -f "${artifact}" ]]; then
    echo "error: artifact not found: ${artifact}" >&2
    exit 2
fi

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fuzz_dir="${workspace_root}/fuzz"

if [[ -z "${target}" ]]; then
    case "${artifact}" in
        *filter_rules_vs_upstream*) target="filter_rules_vs_upstream" ;;
        *filter_differential*)      target="filter_differential" ;;
        *)
            echo "error: cannot infer fuzz target from path; pass --target" >&2
            exit 2
            ;;
    esac
fi

case "${target}" in
    filter_differential|filter_rules_vs_upstream) ;;
    *)
        echo "error: unknown target '${target}'" >&2
        exit 2
        ;;
esac

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required" >&2
    exit 2
fi
if ! cargo +nightly --version >/dev/null 2>&1; then
    echo "error: nightly toolchain required (rustup toolchain install nightly)" >&2
    exit 2
fi
if ! cargo +nightly fuzz --help >/dev/null 2>&1; then
    echo "error: cargo-fuzz required (cargo install cargo-fuzz)" >&2
    exit 2
fi

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
    echo "warning: no upstream rsync binary discovered; the differential" >&2
    echo "         compare will be skipped. Set OC_RSYNC_UPSTREAM_BIN or run" >&2
    echo "         tools/ci/run_interop.sh." >&2
fi

# Resolve the artifact to an absolute path before changing directories.
case "${artifact}" in
    /*) artifact_abs="${artifact}" ;;
    *)  artifact_abs="$(cd "$(dirname "${artifact}")" && pwd)/$(basename "${artifact}")" ;;
esac

echo "target:        ${target}"
echo "artifact:      ${artifact_abs}"
echo "upstream:      ${upstream:-<none>}"

cd "${fuzz_dir}"
set +e
env OC_RSYNC_UPSTREAM_BIN="${upstream}" \
    cargo +nightly fuzz run "${target}" "${artifact_abs}"
status=$?
set -e

if [[ ${status} -eq 0 ]]; then
    echo "reproducer ran cleanly - the divergence did not trigger; check whether"
    echo "the fix is already in tree or whether the reproducer is flaky."
    exit 0
fi
echo "reproducer triggered failure (exit ${status}); see libFuzzer output above."
exit 1
