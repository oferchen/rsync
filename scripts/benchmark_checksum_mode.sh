#!/usr/bin/env bash
# Checksum-mode (-c) benchmark harness for oc-rsync vs upstream rsync.
#
# Reproduces the CSM-1 baseline gap: a single `-c`/`--checksum` sync over
# an identical source and destination (so the only work is whole-file
# rehashing on both ends) currently runs ~1.5-1.7x slower in oc-rsync
# than in upstream rsync 3.4.x. The harness exists so the CSM-8 fix can
# prove the improvement against a stable, repeatable reference number,
# and so the CSM-4/5/6 audit findings (default-backend mismatch,
# simd_batch not wired on the -c path, whole-file buffer-size divergence)
# can each be measured in isolation.
#
# Design notes:
#   - Source and destination start byte-identical. With `-c` rsync must
#     still rehash every file on both sides to decide it can skip the
#     transfer, so wall time is dominated by strong-checksum cost rather
#     than I/O or wire traffic.
#   - hyperfine drives the comparison with --warmup 1 so page-cache
#     warmth is held constant across both cells, isolating CPU-bound
#     checksum cost from cold-read I/O.
#   - Both cells use the same fixed corpus and same `-avc` flag set.
#     The only variable is which rsync binary runs.
#   - Three corpus shapes exercise the audited gaps:
#       small_files   (default)  500 x 4 KiB   -> per-file overhead
#       medium_file              1 x 100 MiB   -> single-stream throughput
#       mixed                    50 files,
#                                4 KiB..4 MiB  -> realistic mix
#   - The summary prints oc/upstream wall time and the ratio so the
#     CSM-1 gap is visible without re-running stats by hand.
#
# Usage:
#   scripts/benchmark_checksum_mode.sh [OPTIONS]
#
# Options:
#   -n, --runs N        hyperfine measured runs per cell (default: 20;
#                       env override: RUNS)
#   -s, --scenario S    Corpus: small_files (default) | medium_file | mixed
#   -j, --json FILE     Write per-scenario hyperfine JSON to FILE
#       --keep-tmp      Leave fixtures on disk for triage
#   -h, --help          Show this help and exit
#
# Environment overrides:
#   OC_RSYNC          oc-rsync binary. Defaults: /usr/local/bin/oc-rsync-dev
#                     (rsync-profile container), then target/release/oc-rsync,
#                     then target/dist/oc-rsync, then PATH lookup.
#   UPSTREAM_RSYNC    Upstream rsync binary. Defaults to
#                     target/interop/upstream-install/3.4.2/bin/rsync,
#                     falling back to 3.4.1, then /usr/bin/rsync.
#   BENCH_ROOT        Working dir (must live under /tmp or /var/tmp).
#                     Default: /tmp/oc-rsync-bench-checksum-mode.
#   RUNS              Same as --runs (CLI flag wins if both set).
#
# Runs cleanly:
#   - inside the rsync-profile podman container, and
#   - on bare Linux CI.
#
# Fixtures are cleaned up on EXIT/INT/TERM unless --keep-tmp is given.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

RUNS="${RUNS:-20}"
SCENARIO="small_files"
JSON_FILE=""
KEEP_TMP=0

BENCH_ROOT="${BENCH_ROOT:-/tmp/oc-rsync-bench-checksum-mode}"

usage() {
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0"
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -n|--runs)     RUNS="$2"; shift 2 ;;
        -s|--scenario) SCENARIO="$2"; shift 2 ;;
        -j|--json)     JSON_FILE="$2"; shift 2 ;;
        --keep-tmp)    KEEP_TMP=1; shift ;;
        -h|--help)     usage 0 ;;
        *) echo "Unknown option: $1" >&2; usage 1 ;;
    esac
done

case "${SCENARIO}" in
    small_files|medium_file|mixed) ;;
    *)
        echo "ERROR: --scenario must be small_files, medium_file, or mixed (got: ${SCENARIO})" >&2
        exit 2
        ;;
esac

case "${BENCH_ROOT}" in
    /tmp/*|/var/tmp/*) ;;
    *)
        echo "ERROR: BENCH_ROOT must live under /tmp or /var/tmp (got: ${BENCH_ROOT})" >&2
        echo "       Refusing to operate to prevent rm -rf accidents on bind mounts." >&2
        exit 1
        ;;
esac

default_oc_rsync() {
    if [[ -x /usr/local/bin/oc-rsync-dev ]]; then
        echo /usr/local/bin/oc-rsync-dev
    elif [[ -x "${PROJECT_ROOT}/target/release/oc-rsync" ]]; then
        echo "${PROJECT_ROOT}/target/release/oc-rsync"
    elif [[ -x "${PROJECT_ROOT}/target/dist/oc-rsync" ]]; then
        echo "${PROJECT_ROOT}/target/dist/oc-rsync"
    else
        command -v oc-rsync || echo oc-rsync
    fi
}

default_upstream_rsync() {
    local candidates=(
        "${PROJECT_ROOT}/target/interop/upstream-install/3.4.2/bin/rsync"
        "${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync"
        /usr/bin/rsync
        /usr/local/bin/rsync
    )
    local cand
    for cand in "${candidates[@]}"; do
        if [[ -x "${cand}" ]]; then
            echo "${cand}"
            return
        fi
    done
    command -v rsync || echo rsync
}

OC_RSYNC="${OC_RSYNC:-$(default_oc_rsync)}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-$(default_upstream_rsync)}"

check_prereqs() {
    if ! command -v hyperfine >/dev/null 2>&1; then
        echo "ERROR: hyperfine not found. Install with: cargo install hyperfine" >&2
        exit 1
    fi
    if [[ ! -x "${OC_RSYNC}" ]] && ! command -v "${OC_RSYNC}" >/dev/null 2>&1; then
        echo "ERROR: oc-rsync not found at: ${OC_RSYNC}" >&2
        exit 1
    fi
    if [[ ! -x "${UPSTREAM_RSYNC}" ]] && ! command -v "${UPSTREAM_RSYNC}" >/dev/null 2>&1; then
        echo "ERROR: upstream rsync not found at: ${UPSTREAM_RSYNC}" >&2
        exit 1
    fi
}

safe_rm_under_root() {
    local path="$1"
    case "${path}" in
        "${BENCH_ROOT}"/*) rm -rf -- "${path}" ;;
        *) echo "REFUSING to rm path outside BENCH_ROOT: ${path}" >&2; return 1 ;;
    esac
}

# Fixed corpora. Each shape exercises one audited cost driver:
#   small_files: per-file open/stat/hash overhead dominates.
#   medium_file: single-stream strong-checksum throughput dominates.
#   mixed:       realistic blend; smooths out both extremes.
# /dev/urandom guarantees incompressible content so no compression
# shortcut can mask the checksum cost.
generate_small_files() {
    local dir="$1" count=500 size=4096
    safe_rm_under_root "${dir}" || true
    mkdir -p "${dir}"
    local i
    for ((i = 1; i <= count; i++)); do
        dd if=/dev/urandom of="${dir}/file_${i}.dat" bs="${size}" count=1 \
            status=none 2>/dev/null
    done
}

generate_medium_file() {
    local dir="$1"
    safe_rm_under_root "${dir}" || true
    mkdir -p "${dir}"
    dd if=/dev/urandom of="${dir}/medium.dat" bs=1M count=100 \
        status=none 2>/dev/null
}

# 50 files spanning 4 KiB..4 MiB in geometric-ish steps. Sizes are
# deterministic so reruns hash the same byte budget.
generate_mixed() {
    local dir="$1"
    safe_rm_under_root "${dir}" || true
    mkdir -p "${dir}"
    local i size_kib
    for ((i = 1; i <= 50; i++)); do
        # Cycle through 4, 16, 64, 256, 1024, 4096 KiB.
        case $((i % 6)) in
            0) size_kib=4 ;;
            1) size_kib=16 ;;
            2) size_kib=64 ;;
            3) size_kib=256 ;;
            4) size_kib=1024 ;;
            5) size_kib=4096 ;;
        esac
        dd if=/dev/urandom of="${dir}/file_${i}.dat" bs=1024 count="${size_kib}" \
            status=none 2>/dev/null
    done
}

generate_corpus() {
    local src="$1"
    case "${SCENARIO}" in
        small_files) generate_small_files "${src}" ;;
        medium_file) generate_medium_file "${src}" ;;
        mixed)       generate_mixed "${src}" ;;
    esac
}

# Seed destination with byte-identical content. With identical
# size+mtime+content, `-c` is the only thing forcing both ends to
# re-read and re-hash every file; without `-c` rsync's quick-check
# would skip the work entirely.
seed_identical_dest() {
    local src="$1" dest="$2"
    safe_rm_under_root "${dest}" || true
    mkdir -p "${dest}"
    cp -a "${src}/." "${dest}/"
}

cleanup() {
    if (( ! KEEP_TMP )); then
        case "${BENCH_ROOT}" in
            /tmp/*|/var/tmp/*) rm -rf -- "${BENCH_ROOT}" ;;
        esac
    else
        echo "BENCH_ROOT preserved at: ${BENCH_ROOT}"
    fi
}
trap cleanup EXIT INT TERM

# Hyperfine driver ---------------------------------------------------------

# Both cells run `-avc src/ dest/` against the same prebuilt corpus.
# Destination is pre-seeded once (identical to source) before hyperfine
# starts, then left in place across runs; with `-c` rsync will still
# rehash every file on every invocation, which is exactly what we want
# to measure. --warmup 1 amortises the cold-read penalty so the
# steady-state CPU cost dominates.
run_checksum_bench() {
    local src="$1"
    local oc_dest="${BENCH_ROOT}/oc-dest"
    local up_dest="${BENCH_ROOT}/up-dest"

    seed_identical_dest "${src}" "${oc_dest}"
    seed_identical_dest "${src}" "${up_dest}"

    local oc_cmd="${OC_RSYNC} -avc ${src}/ ${oc_dest}/"
    local up_cmd="${UPSTREAM_RSYNC} -avc ${src}/ ${up_dest}/"

    local export_args=()
    if [[ -n "${JSON_FILE}" ]]; then
        export_args=(--export-json "${JSON_FILE}")
    fi

    echo ""
    echo "=== Checksum-mode hyperfine (scenario=${SCENARIO}, runs=${RUNS}) ==="
    echo "    oc-rsync:        ${OC_RSYNC}"
    echo "    upstream rsync:  ${UPSTREAM_RSYNC}"
    echo "    flags:           -avc (whole-file checksum compare)"
    echo ""

    hyperfine \
        --warmup 1 \
        --runs "${RUNS}" \
        --command-name "upstream-checksum" "${up_cmd}" \
        --command-name "oc-rsync-checksum" "${oc_cmd}" \
        "${export_args[@]}"
}

print_ratio() {
    # When --export-json is set, parse it and print the ratio explicitly
    # so the 1.5-1.7x reference gap is visible without re-reading the JSON.
    if [[ -z "${JSON_FILE}" ]] || ! command -v python3 >/dev/null 2>&1; then
        return 0
    fi
    if [[ ! -f "${JSON_FILE}" ]]; then
        return 0
    fi
    python3 - "${JSON_FILE}" "${SCENARIO}" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    data = json.load(fh)
scenario = sys.argv[2]
results = {r['command']: r for r in data['results']}
up = results.get('upstream-checksum')
oc = results.get('oc-rsync-checksum')
if not up or not oc:
    sys.exit(0)
up_ms = up['mean'] * 1000.0
oc_ms = oc['mean'] * 1000.0
ratio = oc_ms / up_ms if up_ms > 0 else float('inf')
print("")
print(f"=== Checksum-mode ratio (scenario={scenario}) ===")
print(f"  upstream-checksum mean:  {up_ms:8.2f} ms")
print(f"  oc-rsync-checksum mean:  {oc_ms:8.2f} ms")
print(f"  ratio (oc / upstream):   {ratio:6.2f}x")
print("  CSM-1 reference gap:       1.50x - 1.70x (upstream issue #970)")
PY
}

# Always export JSON; fall back to a temp path inside BENCH_ROOT if the
# caller did not request one, so the ratio block can still be printed.
ensure_json_path() {
    if [[ -z "${JSON_FILE}" ]]; then
        JSON_FILE="${BENCH_ROOT}/checksum_mode_${SCENARIO}.json"
    fi
}

main() {
    check_prereqs
    mkdir -p "${BENCH_ROOT}"
    ensure_json_path

    echo "================================================"
    echo "  Checksum-mode benchmark (CSM-1 harness)"
    echo "================================================"
    echo "  oc-rsync:        ${OC_RSYNC}"
    echo "  upstream rsync:  ${UPSTREAM_RSYNC}"
    echo "  BENCH_ROOT:      ${BENCH_ROOT}"
    echo "  Scenario:        ${SCENARIO}"
    echo "  Runs:            ${RUNS}"
    echo ""
    "${OC_RSYNC}" --version 2>/dev/null | head -1 || true
    "${UPSTREAM_RSYNC}" --version 2>/dev/null | head -1 || true
    echo ""

    local src="${BENCH_ROOT}/src"
    generate_corpus "${src}"

    run_checksum_bench "${src}"
    print_ratio

    echo ""
    echo "Done."
}

main "$@"
