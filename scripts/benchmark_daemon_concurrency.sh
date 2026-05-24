#!/usr/bin/env sh
# Daemon concurrency bench harness for the thread-per-connection model.
#
# Measures how the oc-rsync daemon handles N concurrent client connections
# in a single burst against a configurable --max-connections cap. Produces
# accept-latency percentiles (p50/p95/p99), peak RSS, peak thread count,
# and total completion time. The downstream D10K-2..4 tasks parse the
# single-line summary emitted at the end to feed baseline + regression
# bench cells; D10K-7 wires this harness into CI via the wrapper at
# tools/ci/run_daemon_concurrency_bench.sh.
#
# Context:
#   - Memory note: project_daemon_10k_conn_ceiling documents the ~10K
#     concurrent-connection ceiling on the current thread-per-conn model.
#   - Related: DMC-2 (#2799, completed) shipped the daemon admission cap
#     integration test that validates --max-connections rejects callers
#     past the cap; this harness exercises the cap end-to-end under load.
#   - The harness reuses the daemon spin-up shape from DIS-1 (#2752,
#     scripts/benchmark_daemon_cold_start.sh) and the per-version daemon
#     scaffolding patterns from scripts/rsync-interop-server.sh.
#
# Usage:
#   scripts/benchmark_daemon_concurrency.sh [OPTIONS]
#
# Options:
#   -n, --conns N         Number of concurrent client connections
#                         (default: $D10K_N, fallback 1000)
#   -m, --max-conns M     Daemon --max-connections cap (default: N + 16)
#   -p, --port P          TCP port for oc-rsync daemon (default: 28840)
#       --keep-tmp        Leave fixtures, configs and logs on disk
#   -h, --help            Show this help and exit
#
# Environment overrides:
#   D10K_N            Parallel client count when --conns is not passed.
#                     Default: 1000. Use 5000 or 10000 for ceiling probes.
#   OC_RSYNC          oc-rsync binary. Defaults: /usr/local/bin/oc-rsync-dev
#                     (rsync-profile container), then target/release/oc-rsync,
#                     then target/dist/oc-rsync, then PATH lookup.
#   UPSTREAM_RSYNC    Upstream rsync client binary. Defaults to
#                     target/interop/upstream-install/3.4.2/bin/rsync,
#                     falling back to 3.4.1, then /usr/bin/rsync.
#   BENCH_ROOT        Working dir (must live under /tmp or /var/tmp).
#                     Default: /tmp/oc-rsync-bench-d10k.
#
# Gating:
#   - Linux-only: probes /proc/$pid/status for RSS and thread count.
#     Exits 0 (skip) on any other OS.
#   - Skips with exit 0 if upstream rsync client is not present, so
#     PRs that touch only the harness don't fail on hosts without
#     a system rsync.
#
# Output:
#   Human-readable progress is written to stderr. The final stdout line
#   begins with "D10K_BENCH_SUMMARY " and is a single key=value summary
#   for downstream parsing by D10K-2..4. Example:
#     D10K_BENCH_SUMMARY n=1000 max_conns=1016 accepted=1000 \
#       rejected=0 failed=0 wall_ms=4321 \
#       p50_ms=12 p95_ms=87 p99_ms=142 peak_rss_kb=412800 peak_threads=1024

set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
PROJECT_ROOT=$(cd "${SCRIPT_DIR}/.." && pwd)

CONNS=""
MAX_CONNS=""
PORT=28840
KEEP_TMP=0

BENCH_ROOT=${BENCH_ROOT:-/tmp/oc-rsync-bench-d10k}

usage() {
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0"
    exit "${1:-0}"
}

# Manual arg parse; getopts is not portable enough for long options.
while [ $# -gt 0 ]; do
    case "$1" in
        -n|--conns)      CONNS=$2; shift 2 ;;
        -m|--max-conns)  MAX_CONNS=$2; shift 2 ;;
        -p|--port)       PORT=$2; shift 2 ;;
        --keep-tmp)      KEEP_TMP=1; shift ;;
        -h|--help)       usage 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; usage 1 ;;
    esac
done

if [ -z "${CONNS}" ]; then
    CONNS=${D10K_N:-1000}
fi

case "${CONNS}" in
    ''|*[!0-9]*) printf 'ERROR: --conns must be a positive integer (got: %s)\n' "${CONNS}" >&2; exit 2 ;;
esac
if [ "${CONNS}" -lt 1 ]; then
    printf 'ERROR: --conns must be >= 1 (got: %s)\n' "${CONNS}" >&2
    exit 2
fi

if [ -z "${MAX_CONNS}" ]; then
    MAX_CONNS=$((CONNS + 16))
fi

case "${MAX_CONNS}" in
    ''|*[!0-9]*) printf 'ERROR: --max-conns must be a positive integer (got: %s)\n' "${MAX_CONNS}" >&2; exit 2 ;;
esac

case "${BENCH_ROOT}" in
    /tmp/*|/var/tmp/*) ;;
    *)
        printf 'ERROR: BENCH_ROOT must live under /tmp or /var/tmp (got: %s)\n' "${BENCH_ROOT}" >&2
        printf '       Refusing to operate to prevent rm -rf accidents on bind mounts.\n' >&2
        exit 1
        ;;
esac

# Linux-only gate. /proc/$pid/status is the cheapest portable way to read
# peak RSS (VmHWM) and live thread count for a long-running process.
uname_s=$(uname -s 2>/dev/null || echo unknown)
if [ "${uname_s}" != "Linux" ]; then
    printf '[benchmark_daemon_concurrency] skipped: Linux-only (uname=%s)\n' "${uname_s}" >&2
    exit 0
fi

# Upstream rsync is the canonical client; if absent we cannot drive load.
if ! command -v rsync >/dev/null 2>&1; then
    printf '[benchmark_daemon_concurrency] skipped: upstream rsync client not on PATH\n' >&2
    exit 0
fi

default_oc_rsync() {
    if [ -x /usr/local/bin/oc-rsync-dev ]; then
        printf '%s\n' /usr/local/bin/oc-rsync-dev
    elif [ -x "${PROJECT_ROOT}/target/release/oc-rsync" ]; then
        printf '%s\n' "${PROJECT_ROOT}/target/release/oc-rsync"
    elif [ -x "${PROJECT_ROOT}/target/dist/oc-rsync" ]; then
        printf '%s\n' "${PROJECT_ROOT}/target/dist/oc-rsync"
    else
        command -v oc-rsync || printf 'oc-rsync\n'
    fi
}

default_upstream_rsync() {
    for cand in \
        "${PROJECT_ROOT}/target/interop/upstream-install/3.4.2/bin/rsync" \
        "${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync" \
        /usr/bin/rsync \
        /usr/local/bin/rsync
    do
        if [ -x "${cand}" ]; then
            printf '%s\n' "${cand}"
            return
        fi
    done
    command -v rsync || printf 'rsync\n'
}

OC_RSYNC=${OC_RSYNC:-$(default_oc_rsync)}
UPSTREAM_RSYNC=${UPSTREAM_RSYNC:-$(default_upstream_rsync)}

if [ ! -x "${OC_RSYNC}" ] && ! command -v "${OC_RSYNC}" >/dev/null 2>&1; then
    printf 'ERROR: oc-rsync not found at: %s\n' "${OC_RSYNC}" >&2
    exit 1
fi
if [ ! -x "${UPSTREAM_RSYNC}" ] && ! command -v "${UPSTREAM_RSYNC}" >/dev/null 2>&1; then
    printf 'ERROR: upstream rsync not found at: %s\n' "${UPSTREAM_RSYNC}" >&2
    exit 1
fi

safe_rm_under_root() {
    path=$1
    case "${path}" in
        "${BENCH_ROOT}"/*) rm -rf -- "${path}" ;;
        *) printf 'REFUSING to rm path outside BENCH_ROOT: %s\n' "${path}" >&2; return 1 ;;
    esac
}

mkdir -p "${BENCH_ROOT}"

# Tiny fixed corpus. The point of this harness is connection concurrency,
# not throughput; per-client work should be small so accept-side scaling
# dominates the wall time.
CORPUS="${BENCH_ROOT}/corpus"
safe_rm_under_root "${CORPUS}" 2>/dev/null || true
mkdir -p "${CORPUS}"
i=1
while [ "${i}" -le 4 ]; do
    dd if=/dev/urandom of="${CORPUS}/file_${i}.dat" bs=512 count=1 status=none 2>/dev/null
    i=$((i + 1))
done

CONF="${BENCH_ROOT}/oc.conf"
PID_FILE="${BENCH_ROOT}/oc.pid"
LOG_FILE="${BENCH_ROOT}/oc.log"
LATENCY_DIR="${BENCH_ROOT}/latencies"
RESULT_DIR="${BENCH_ROOT}/results"
SAMPLE_DIR="${BENCH_ROOT}/samples"

safe_rm_under_root "${LATENCY_DIR}" 2>/dev/null || true
safe_rm_under_root "${RESULT_DIR}" 2>/dev/null || true
safe_rm_under_root "${SAMPLE_DIR}" 2>/dev/null || true
mkdir -p "${LATENCY_DIR}" "${RESULT_DIR}" "${SAMPLE_DIR}"
: > "${LOG_FILE}"
rm -f "${PID_FILE}"

cat > "${CONF}" <<CONF
pid file = ${PID_FILE}
port = ${PORT}
use chroot = false
max connections = ${MAX_CONNS}
numeric ids = yes

[bench]
    path = ${CORPUS}
    comment = oc-rsync D10K-1 concurrency bench target
    read only = true
CONF

OC_PID=""
SAMPLER_PID=""

cleanup() {
    if [ -n "${SAMPLER_PID}" ]; then
        kill "${SAMPLER_PID}" 2>/dev/null || true
        wait "${SAMPLER_PID}" 2>/dev/null || true
        SAMPLER_PID=""
    fi
    if [ -n "${OC_PID}" ]; then
        kill "${OC_PID}" 2>/dev/null || true
        wait "${OC_PID}" 2>/dev/null || true
        OC_PID=""
    fi
    if [ "${KEEP_TMP}" -eq 0 ]; then
        case "${BENCH_ROOT}" in
            /tmp/*|/var/tmp/*) rm -rf -- "${BENCH_ROOT}" ;;
        esac
    else
        printf 'BENCH_ROOT preserved at: %s\n' "${BENCH_ROOT}" >&2
    fi
}
trap cleanup EXIT INT TERM

# OC_RSYNC_DAEMON_FALLBACK=0 forces oc-rsync's native daemon path so this
# harness never accidentally measures the system rsync daemon.
OC_RSYNC_DAEMON_FALLBACK=0 "${OC_RSYNC}" --daemon --no-detach \
    --config "${CONF}" --port "${PORT}" --log-file "${LOG_FILE}" \
    >>"${LOG_FILE}" 2>&1 &
OC_PID=$!

# Wait for the listener to become accept-ready. A bare TCP probe is
# enough; we don't need a full @RSYNCD handshake here.
ready=0
attempt=1
while [ "${attempt}" -le 100 ]; do
    if "${UPSTREAM_RSYNC}" "rsync://127.0.0.1:${PORT}/" >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 0.1
    attempt=$((attempt + 1))
done
if [ "${ready}" -eq 0 ]; then
    printf 'ERROR: oc-rsync daemon did not become ready on port %s within 10s\n' "${PORT}" >&2
    tail -n 80 "${LOG_FILE}" >&2 || true
    exit 1
fi

printf '[benchmark_daemon_concurrency] daemon ready on port %s (pid=%s, max_conns=%s, n=%s)\n' \
    "${PORT}" "${OC_PID}" "${MAX_CONNS}" "${CONNS}" >&2

# Background sampler: every 50 ms record VmHWM (KB) and live thread count
# from /proc/$OC_PID/status. We aggregate at the end to get peak values.
sampler() {
    while kill -0 "${OC_PID}" 2>/dev/null; do
        status="/proc/${OC_PID}/status"
        if [ -r "${status}" ]; then
            rss=$(awk '/^VmHWM:/ { print $2 }' "${status}" 2>/dev/null || printf '0\n')
            threads=$(awk '/^Threads:/ { print $2 }' "${status}" 2>/dev/null || printf '0\n')
            printf '%s %s\n' "${rss:-0}" "${threads:-0}" >> "${SAMPLE_DIR}/samples.txt"
        fi
        sleep 0.05
    done
}
sampler &
SAMPLER_PID=$!

# Single client: emit accept latency in ms (wall time from spawn to first
# successful module-list reply) and exit status into a per-client result
# file. Module listing is the cheapest interaction that still exercises
# accept + greeting + module dispatch + close.
URL="rsync://127.0.0.1:${PORT}/"
client_runner() {
    idx=$1
    start_ns=$(date +%s%N 2>/dev/null || printf '0\n')
    # Use upstream rsync as the client. --contimeout caps a stuck connect
    # at 5s so a saturated accept queue surfaces as a failure rather than
    # hanging the whole harness.
    out=$("${UPSTREAM_RSYNC}" --contimeout=5 "${URL}" 2>&1)
    rc=$?
    end_ns=$(date +%s%N 2>/dev/null || printf '0\n')
    if [ "${start_ns}" -eq 0 ] || [ "${end_ns}" -eq 0 ]; then
        elapsed_ms=0
    else
        elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
    fi
    # Classify the outcome. Upstream rsync's daemon-side admission cap
    # surfaces as "@ERROR: max connections" on the client; we treat that
    # as "rejected" rather than "failed" so D10K-2..4 can report both.
    classification=failed
    if [ "${rc}" -eq 0 ]; then
        classification=accepted
    else
        case "${out}" in
            *"max connections"*|*"@ERROR"*) classification=rejected ;;
        esac
    fi
    printf '%s %s %s\n' "${idx}" "${elapsed_ms}" "${classification}" \
        > "${RESULT_DIR}/c_${idx}.txt"
}

printf '[benchmark_daemon_concurrency] launching %s parallel clients...\n' "${CONNS}" >&2

wall_start_ns=$(date +%s%N 2>/dev/null || printf '0\n')

# Spawn all clients as background shell jobs. POSIX sh does not have
# job arrays, so we track PIDs in a temp file and wait on each.
PIDS_FILE="${BENCH_ROOT}/pids.txt"
: > "${PIDS_FILE}"

i=1
while [ "${i}" -le "${CONNS}" ]; do
    client_runner "${i}" &
    printf '%s\n' "$!" >> "${PIDS_FILE}"
    i=$((i + 1))
done

# Wait for every client. We deliberately do not bail on the first failure;
# the harness reports counts so D10K-2..4 can see the saturation profile.
while read -r pid; do
    wait "${pid}" 2>/dev/null || true
done < "${PIDS_FILE}"

wall_end_ns=$(date +%s%N 2>/dev/null || printf '0\n')
if [ "${wall_start_ns}" -eq 0 ] || [ "${wall_end_ns}" -eq 0 ]; then
    wall_ms=0
else
    wall_ms=$(( (wall_end_ns - wall_start_ns) / 1000000 ))
fi

# Stop the sampler before aggregating to avoid a torn final read.
if [ -n "${SAMPLER_PID}" ]; then
    kill "${SAMPLER_PID}" 2>/dev/null || true
    wait "${SAMPLER_PID}" 2>/dev/null || true
    SAMPLER_PID=""
fi

# Aggregate. POSIX sh has no arrays; we lean on awk for percentiles.
accepted=0
rejected=0
failed=0
for f in "${RESULT_DIR}"/c_*.txt; do
    [ -f "${f}" ] || continue
    line=$(cat "${f}")
    cls=$(printf '%s\n' "${line}" | awk '{ print $3 }')
    case "${cls}" in
        accepted) accepted=$((accepted + 1)) ;;
        rejected) rejected=$((rejected + 1)) ;;
        *)        failed=$((failed + 1)) ;;
    esac
    printf '%s\n' "${line}" | awk '{ print $2 }' >> "${LATENCY_DIR}/all.txt"
done

# Percentiles over accept-side wall time. Only successful "accepted"
# clients are counted toward p50/p95/p99; rejected/failed clients
# distort the distribution (they fail fast on @ERROR).
: > "${LATENCY_DIR}/accepted.txt"
for f in "${RESULT_DIR}"/c_*.txt; do
    [ -f "${f}" ] || continue
    awk '$3 == "accepted" { print $2 }' "${f}" >> "${LATENCY_DIR}/accepted.txt"
done

percentile() {
    pct=$1
    file=$2
    if [ ! -s "${file}" ]; then
        printf '0\n'
        return
    fi
    sort -n "${file}" | awk -v p="${pct}" '
        { a[NR] = $1 }
        END {
            if (NR == 0) { print 0; exit }
            idx = int((p / 100.0) * NR + 0.5)
            if (idx < 1) idx = 1
            if (idx > NR) idx = NR
            print a[idx]
        }
    '
}

p50_ms=$(percentile 50 "${LATENCY_DIR}/accepted.txt")
p95_ms=$(percentile 95 "${LATENCY_DIR}/accepted.txt")
p99_ms=$(percentile 99 "${LATENCY_DIR}/accepted.txt")

peak_rss_kb=0
peak_threads=0
if [ -s "${SAMPLE_DIR}/samples.txt" ]; then
    peak_rss_kb=$(awk '{ if ($1 > m) m = $1 } END { print m + 0 }' "${SAMPLE_DIR}/samples.txt")
    peak_threads=$(awk '{ if ($2 > m) m = $2 } END { print m + 0 }' "${SAMPLE_DIR}/samples.txt")
fi

printf '\n[benchmark_daemon_concurrency] results:\n' >&2
printf '  n=%s max_conns=%s\n' "${CONNS}" "${MAX_CONNS}" >&2
printf '  accepted=%s rejected=%s failed=%s\n' "${accepted}" "${rejected}" "${failed}" >&2
printf '  wall_ms=%s\n' "${wall_ms}" >&2
printf '  accept latency p50/p95/p99 ms = %s / %s / %s\n' "${p50_ms}" "${p95_ms}" "${p99_ms}" >&2
printf '  peak_rss_kb=%s peak_threads=%s\n' "${peak_rss_kb}" "${peak_threads}" >&2

# Single-line summary on stdout for downstream parsing by D10K-2..4.
printf 'D10K_BENCH_SUMMARY n=%s max_conns=%s accepted=%s rejected=%s failed=%s wall_ms=%s p50_ms=%s p95_ms=%s p99_ms=%s peak_rss_kb=%s peak_threads=%s\n' \
    "${CONNS}" "${MAX_CONNS}" "${accepted}" "${rejected}" "${failed}" \
    "${wall_ms}" "${p50_ms}" "${p95_ms}" "${p99_ms}" \
    "${peak_rss_kb}" "${peak_threads}"
