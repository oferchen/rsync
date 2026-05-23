#!/usr/bin/env bash
# Daemon cold-start benchmark harness for oc-rsync vs upstream rsync.
#
# Reproduces the DIS-1 baseline gap: a single rsync pull from an oc-rsync
# daemon currently costs ~1.35s end-to-end vs ~0.36s for upstream rsync on
# the same corpus and host (~3.7x slower). The harness exists so DIS-2
# profiling and DIS-4.a-e audits can measure progress against a stable
# reference number, and DIS-6 fixes can prove regressions/improvements.
#
# Design notes:
#   - Two daemons (oc-rsync, upstream) run side-by-side on different ports
#     with identical module config and identical corpus.
#   - hyperfine drives the comparison with --warmup 0 so the first-invocation
#     daemon-handshake + flist + small-transfer cost dominates each sample.
#   - Each measured run uses an upstream rsync CLIENT against each daemon.
#     Holding the client constant isolates the daemon-side cost.
#   - Each run's destination is wiped via hyperfine --prepare so every
#     sample is a fresh cold-start (no quick-check skip, no warm cache state
#     inside the receiver).
#   - The summary prints oc/upstream wall time and the ratio so the gap is
#     visible without re-running statistics by hand.
#
# Usage:
#   scripts/benchmark_daemon_cold_start.sh [OPTIONS]
#
# Options:
#   -n, --runs N        hyperfine measured runs per daemon (default: 20)
#   -s, --scenario S    Corpus: small_files (default) | medium_file
#   -p, --port-base P   Starting TCP port (default: 28800; oc=P, upstream=P+1)
#   -j, --json FILE     Write per-scenario hyperfine JSON to FILE
#       --keep-tmp      Leave fixtures, configs and logs on disk for triage
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
#                     Default: /tmp/oc-rsync-bench-cold-start.
#
# Runs cleanly:
#   - inside the rsync-profile podman container, and
#   - on bare Linux CI.
#
# Both daemons are torn down on EXIT/INT/TERM.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

RUNS=20
SCENARIO="small_files"
PORT_BASE=28800
JSON_FILE=""
KEEP_TMP=0

BENCH_ROOT="${BENCH_ROOT:-/tmp/oc-rsync-bench-cold-start}"

usage() {
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0"
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -n|--runs)      RUNS="$2"; shift 2 ;;
        -s|--scenario)  SCENARIO="$2"; shift 2 ;;
        -p|--port-base) PORT_BASE="$2"; shift 2 ;;
        -j|--json)      JSON_FILE="$2"; shift 2 ;;
        --keep-tmp)     KEEP_TMP=1; shift ;;
        -h|--help)      usage 0 ;;
        *) echo "Unknown option: $1" >&2; usage 1 ;;
    esac
done

case "${SCENARIO}" in
    small_files|medium_file) ;;
    *) echo "ERROR: --scenario must be small_files or medium_file (got: ${SCENARIO})" >&2; exit 2 ;;
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

# Fixed corpora. Small enough that cold-start (TCP handshake, @RSYNCD greeting,
# auth probe, module open, flist exchange, generator/receiver wiring) dominates
# the wall time. Both implementations transfer the same bytes.
generate_small_files() {
    local dir="$1" count=500 size=1024
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
    dd if=/dev/urandom of="${dir}/medium.dat" bs=1M count=10 \
        status=none 2>/dev/null
}

# Daemon lifecycle ---------------------------------------------------------

OC_PID=""
UP_PID=""
OC_DEST=""
UP_DEST=""
OC_PORT=0
UP_PORT=0

write_oc_conf() {
    local path="$1" pid_file="$2" port="$3" dest="$4"
    cat > "${path}" <<CONF
pid file = ${pid_file}
port = ${port}
use chroot = false
numeric ids = yes

[bench]
    path = ${dest}
    comment = oc-rsync cold-start bench target
    read only = false
    write only = false
CONF
}

write_upstream_conf() {
    local path="$1" pid_file="$2" port="$3" dest="$4" identity="$5"
    cat > "${path}" <<CONF
pid file = ${pid_file}
port = ${port}
use chroot = false
${identity}numeric ids = yes
[bench]
    path = ${dest}
    comment = upstream rsync cold-start bench target
    read only = false
    write only = false
CONF
}

wait_for_port() {
    local port="$1" name="$2"
    local i
    for i in $(seq 1 50); do
        if "${UPSTREAM_RSYNC}" "rsync://127.0.0.1:${port}/" >/dev/null 2>&1; then
            echo "  ${name} ready on port ${port}"
            return 0
        fi
        sleep 0.1
    done
    echo "ERROR: ${name} on port ${port} did not become ready in 5s" >&2
    return 1
}

start_daemons() {
    OC_PORT="${PORT_BASE}"
    UP_PORT=$((PORT_BASE + 1))

    OC_DEST="${BENCH_ROOT}/oc-dest"
    UP_DEST="${BENCH_ROOT}/up-dest"
    mkdir -p "${OC_DEST}" "${UP_DEST}"

    local oc_conf="${BENCH_ROOT}/oc.conf"
    local up_conf="${BENCH_ROOT}/upstream.conf"
    local oc_pid_file="${BENCH_ROOT}/oc.pid"
    local up_pid_file="${BENCH_ROOT}/upstream.pid"
    local oc_log="${BENCH_ROOT}/oc.log"
    local up_log="${BENCH_ROOT}/upstream.log"

    : > "${oc_log}"
    : > "${up_log}"
    rm -f "${oc_pid_file}" "${up_pid_file}"

    # rsync daemons running as root accept the "uid =" / "gid =" knobs;
    # rootless containers reject them. Emit identity lines only when root.
    local up_identity=""
    if [[ "$(id -u)" -eq 0 ]]; then
        printf -v up_identity 'uid = %s\ngid = %s\n' "$(id -u)" "$(id -g)"
    fi

    write_oc_conf "${oc_conf}" "${oc_pid_file}" "${OC_PORT}" "${OC_DEST}"
    write_upstream_conf "${up_conf}" "${up_pid_file}" "${UP_PORT}" "${UP_DEST}" "${up_identity}"

    # OC_RSYNC_DAEMON_FALLBACK=0 forces oc-rsync's native daemon path.
    # Without it the binary may delegate to the system rsync, which would
    # measure the wrong receiver.
    OC_RSYNC_DAEMON_FALLBACK=0 "${OC_RSYNC}" --daemon --config "${oc_conf}" \
        --port "${OC_PORT}" --log-file "${oc_log}" &
    OC_PID=$!

    "${UPSTREAM_RSYNC}" --daemon --no-detach --config "${up_conf}" \
        --log-file "${up_log}" &
    UP_PID=$!

    if ! wait_for_port "${OC_PORT}" "oc-rsync daemon"; then
        echo "--- oc-rsync log ---" >&2
        tail -n 40 "${oc_log}" >&2 || true
        return 1
    fi
    if ! wait_for_port "${UP_PORT}" "upstream rsync daemon"; then
        echo "--- upstream rsync log ---" >&2
        tail -n 40 "${up_log}" >&2 || true
        return 1
    fi
}

stop_daemons() {
    if [[ -n "${OC_PID}" ]]; then
        kill "${OC_PID}" 2>/dev/null || true
        wait "${OC_PID}" 2>/dev/null || true
        OC_PID=""
    fi
    if [[ -n "${UP_PID}" ]]; then
        kill "${UP_PID}" 2>/dev/null || true
        wait "${UP_PID}" 2>/dev/null || true
        UP_PID=""
    fi
}

cleanup() {
    stop_daemons
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

# We measure the client-side wall time of a pull from each daemon. The
# client binary is held constant (upstream rsync) so the only variable is
# the daemon implementation under test. --warmup 0 means the very first
# invocation is also measured, so per-process startup and per-connection
# handshake costs are not amortised away.
run_cold_start_bench() {
    local src="$1"
    local oc_dest_client="${BENCH_ROOT}/client-oc-out"
    local up_dest_client="${BENCH_ROOT}/client-up-out"
    mkdir -p "${oc_dest_client}" "${up_dest_client}"

    # Seed the daemon modules with the corpus. The client pulls FROM each
    # daemon, so the source-of-truth must live under each daemon's module.
    safe_rm_under_root "${OC_DEST}" || true
    safe_rm_under_root "${UP_DEST}" || true
    mkdir -p "${OC_DEST}" "${UP_DEST}"
    cp -a "${src}/." "${OC_DEST}/"
    cp -a "${src}/." "${UP_DEST}/"

    local oc_url="rsync://127.0.0.1:${OC_PORT}/bench/"
    local up_url="rsync://127.0.0.1:${UP_PORT}/bench/"

    local oc_cmd="${UPSTREAM_RSYNC} -a ${oc_url} ${oc_dest_client}/"
    local up_cmd="${UPSTREAM_RSYNC} -a ${up_url} ${up_dest_client}/"

    local prep_oc="rm -rf ${oc_dest_client} && mkdir -p ${oc_dest_client}"
    local prep_up="rm -rf ${up_dest_client} && mkdir -p ${up_dest_client}"

    local export_args=()
    if [[ -n "${JSON_FILE}" ]]; then
        export_args=(--export-json "${JSON_FILE}")
    fi

    echo ""
    echo "=== Cold-start hyperfine (scenario=${SCENARIO}, runs=${RUNS}) ==="
    echo "    client: ${UPSTREAM_RSYNC}"
    echo "    oc daemon URL:       ${oc_url}"
    echo "    upstream daemon URL: ${up_url}"
    echo ""

    # Per-command --prepare is hyperfine 1.12+; older versions accept only
    # one global --prepare. We pass per-command so each daemon sees a clean
    # destination on the client side per sample.
    hyperfine \
        --warmup 0 \
        --runs "${RUNS}" \
        --command-name "upstream-daemon" --prepare "${prep_up}" "${up_cmd}" \
        --command-name "oc-rsync-daemon" --prepare "${prep_oc}" "${oc_cmd}" \
        "${export_args[@]}"
}

print_ratio() {
    # When --export-json is set, parse it and print the ratio explicitly so
    # the 3.7x reference gap is visible without re-reading the JSON.
    if [[ -z "${JSON_FILE}" ]] || ! command -v python3 >/dev/null 2>&1; then
        return 0
    fi
    if [[ ! -f "${JSON_FILE}" ]]; then
        return 0
    fi
    python3 - "${JSON_FILE}" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    data = json.load(fh)
results = {r['command']: r for r in data['results']}
up = results.get('upstream-daemon')
oc = results.get('oc-rsync-daemon')
if not up or not oc:
    sys.exit(0)
up_ms = up['mean'] * 1000.0
oc_ms = oc['mean'] * 1000.0
ratio = oc_ms / up_ms if up_ms > 0 else float('inf')
print("")
print("=== Cold-start ratio ===")
print(f"  upstream-daemon mean:  {up_ms:8.2f} ms")
print(f"  oc-rsync-daemon mean:  {oc_ms:8.2f} ms")
print(f"  ratio (oc / upstream): {ratio:6.2f}x")
print("  DIS-1 reference gap:     ~3.70x  (1.35s vs 0.36s)")
PY
}

# Always export JSON; fall back to a temp path inside BENCH_ROOT if the
# caller did not request one, so the ratio block can still be printed.
ensure_json_path() {
    if [[ -z "${JSON_FILE}" ]]; then
        JSON_FILE="${BENCH_ROOT}/cold_start_${SCENARIO}.json"
    fi
}

main() {
    check_prereqs
    mkdir -p "${BENCH_ROOT}"
    ensure_json_path

    echo "================================================"
    echo "  Daemon cold-start benchmark (DIS-1 harness)"
    echo "================================================"
    echo "  oc-rsync:        ${OC_RSYNC}"
    echo "  upstream rsync:  ${UPSTREAM_RSYNC}"
    echo "  BENCH_ROOT:      ${BENCH_ROOT}"
    echo "  Scenario:        ${SCENARIO}"
    echo "  Runs:            ${RUNS}"
    echo "  Port base:       ${PORT_BASE}  (oc=${PORT_BASE}, upstream=$((PORT_BASE + 1)))"
    echo ""
    "${OC_RSYNC}" --version 2>/dev/null | head -1 || true
    "${UPSTREAM_RSYNC}" --version 2>/dev/null | head -1 || true
    echo ""

    local src="${BENCH_ROOT}/src"
    case "${SCENARIO}" in
        small_files) generate_small_files "${src}" ;;
        medium_file) generate_medium_file "${src}" ;;
    esac

    start_daemons
    run_cold_start_bench "${src}"
    print_ratio

    echo ""
    echo "Done."
}

main "$@"
