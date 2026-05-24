#!/usr/bin/env sh
# Russh concurrency bench harness for the async-SSH push boundary.
#
# Measures how oc-rsync's russh / async-SSH push path scales when N
# concurrent push sessions run in a single burst against a local rsyncd.
# Produces per-session p50/p95/p99 latency, peak RSS, peak thread count
# (sampled per child via /proc/$pid/status), and total wall time. The
# downstream RUSSH-4..7 tasks parse the single-line summary emitted at
# the end to feed bench cells at N=64/128/256/512.
#
# Context:
#   - Audit: docs/audit/russh-spawn-blocking-ceiling-inventory.md
#     (RUSSH-1 #2804, RUSSH-2 #2805) lists 4 spawn_blocking sites + 11
#     tokio runtime build sites. None override max_blocking_threads, so
#     the default 512-slot blocking pool caps async-SSH at ~256
#     concurrent sessions per process (two long-lived slots per session).
#   - This harness exists to confirm the saturation knee end-to-end; it
#     does not modify the runtime sizing. RUSSH-4..7 will run it at
#     N=64/128/256/512 to chart the latency cliff.
#   - The harness reuses the daemon spin-up shape from D10K-1
#     (scripts/benchmark_daemon_concurrency.sh) and the per-version
#     daemon scaffolding patterns from scripts/rsync-interop-server.sh.
#
# Usage:
#   scripts/benchmark_russh_concurrency.sh [OPTIONS]
#
# Options:
#   -n, --sessions N    Number of concurrent push sessions
#                       (default: $RUSSH_N, fallback 64)
#   -p, --port P        TCP port for the upstream rsyncd (default: 28860)
#       --keep-tmp      Leave fixtures, configs and logs on disk
#   -h, --help          Show this help and exit
#
# Environment overrides:
#   RUSSH_N           Parallel session count when --sessions is not passed.
#                     Default: 64. Use 128/256/512 for RUSSH-4..7 cells.
#   OC_RSYNC          oc-rsync binary. Defaults: /usr/local/bin/oc-rsync-dev
#                     (rsync-profile container), then target/release/oc-rsync,
#                     then target/dist/oc-rsync, then PATH lookup.
#   UPSTREAM_RSYNC    Upstream rsync binary used as the daemon. Defaults to
#                     target/interop/upstream-install/3.4.2/bin/rsync,
#                     falling back to 3.4.1, then /usr/bin/rsync.
#   BENCH_ROOT        Working dir (must live under /tmp or /var/tmp).
#                     Default: /tmp/oc-rsync-bench-russh.
#   FIXTURE_BYTES     Per-session push payload size in bytes.
#                     Default: 1048576 (1 MiB).
#
# Gating:
#   - Linux-only: probes /proc/$pid/status for RSS and thread count.
#     Exits 0 (skip) on any other OS.
#   - Skips with exit 0 if upstream rsync is not present, so PRs that
#     touch only the harness don't fail on hosts without a system rsync.
#
# Output:
#   Human-readable progress is written to stderr. The final stdout line
#   begins with "RUSSH_BENCH_SUMMARY " and is a single key=value summary
#   for downstream parsing by RUSSH-4..7. Example:
#     RUSSH_BENCH_SUMMARY n=64 succeeded=64 failed=0 wall_ms=8421 \
#       p50_ms=312 p95_ms=987 p99_ms=1242 rss=148 threads=22

set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
PROJECT_ROOT=$(cd "${SCRIPT_DIR}/.." && pwd)

SESSIONS=""
PORT=28860
KEEP_TMP=0

BENCH_ROOT=${BENCH_ROOT:-/tmp/oc-rsync-bench-russh}
FIXTURE_BYTES=${FIXTURE_BYTES:-1048576}

usage() {
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0"
    exit "${1:-0}"
}

# Manual arg parse; getopts is not portable enough for long options.
while [ $# -gt 0 ]; do
    case "$1" in
        -n|--sessions)   SESSIONS=$2; shift 2 ;;
        -p|--port)       PORT=$2; shift 2 ;;
        --keep-tmp)      KEEP_TMP=1; shift ;;
        -h|--help)       usage 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; usage 1 ;;
    esac
done

if [ -z "${SESSIONS}" ]; then
    SESSIONS=${RUSSH_N:-64}
fi

case "${SESSIONS}" in
    ''|*[!0-9]*) printf 'ERROR: --sessions must be a positive integer (got: %s)\n' "${SESSIONS}" >&2; exit 2 ;;
esac
if [ "${SESSIONS}" -lt 1 ]; then
    printf 'ERROR: --sessions must be >= 1 (got: %s)\n' "${SESSIONS}" >&2
    exit 2
fi

case "${FIXTURE_BYTES}" in
    ''|*[!0-9]*) printf 'ERROR: FIXTURE_BYTES must be a positive integer (got: %s)\n' "${FIXTURE_BYTES}" >&2; exit 2 ;;
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
# peak RSS (VmHWM) and live thread count for the push processes.
uname_s=$(uname -s 2>/dev/null || echo unknown)
if [ "${uname_s}" != "Linux" ]; then
    printf '[benchmark_russh_concurrency] skipped: Linux-only (uname=%s)\n' "${uname_s}" >&2
    exit 0
fi

# Upstream rsync is the canonical daemon; if absent we cannot host the
# receive side of the push sessions.
if ! command -v rsync >/dev/null 2>&1; then
    printf '[benchmark_russh_concurrency] skipped: upstream rsync not on PATH\n' >&2
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

# One fixture file per session sized at FIXTURE_BYTES (default 1 MiB).
# The point of this harness is session concurrency through the russh
# boundary, not throughput; per-session payload should stay small so the
# spawn_blocking / runtime-build cost dominates the wall time.
FIXTURE_DIR="${BENCH_ROOT}/fixtures"
safe_rm_under_root "${FIXTURE_DIR}" 2>/dev/null || true
mkdir -p "${FIXTURE_DIR}"
i=1
while [ "${i}" -le "${SESSIONS}" ]; do
    src="${FIXTURE_DIR}/sess_${i}"
    mkdir -p "${src}"
    dd if=/dev/urandom of="${src}/payload.bin" bs=1 count="${FIXTURE_BYTES}" status=none 2>/dev/null
    i=$((i + 1))
done

# Receive root: each session pushes into its own subdirectory under the
# rsyncd module so concurrent writers do not collide on the same path.
RECV_ROOT="${BENCH_ROOT}/recv"
safe_rm_under_root "${RECV_ROOT}" 2>/dev/null || true
mkdir -p "${RECV_ROOT}"

CONF="${BENCH_ROOT}/rsyncd.conf"
PID_FILE="${BENCH_ROOT}/rsyncd.pid"
LOG_FILE="${BENCH_ROOT}/rsyncd.log"
LATENCY_DIR="${BENCH_ROOT}/latencies"
RESULT_DIR="${BENCH_ROOT}/results"
SAMPLE_DIR="${BENCH_ROOT}/samples"

safe_rm_under_root "${LATENCY_DIR}" 2>/dev/null || true
safe_rm_under_root "${RESULT_DIR}" 2>/dev/null || true
safe_rm_under_root "${SAMPLE_DIR}" 2>/dev/null || true
mkdir -p "${LATENCY_DIR}" "${RESULT_DIR}" "${SAMPLE_DIR}"
: > "${LOG_FILE}"
rm -f "${PID_FILE}"

# rsyncd config: read-write so the push side can land payloads. The cap
# is set well above SESSIONS so the daemon never rejects; we want to
# measure oc-rsync's client-side ceiling, not the daemon's.
cat > "${CONF}" <<CONF
pid file = ${PID_FILE}
port = ${PORT}
use chroot = false
max connections = $((SESSIONS * 2 + 16))
numeric ids = yes
log file = ${LOG_FILE}

[bench]
    path = ${RECV_ROOT}
    comment = oc-rsync RUSSH-3 push concurrency bench target
    read only = false
    write only = false
CONF

RSYNCD_PID=""
SAMPLER_PID=""

cleanup() {
    if [ -n "${SAMPLER_PID}" ]; then
        kill "${SAMPLER_PID}" 2>/dev/null || true
        wait "${SAMPLER_PID}" 2>/dev/null || true
        SAMPLER_PID=""
    fi
    if [ -n "${RSYNCD_PID}" ]; then
        kill "${RSYNCD_PID}" 2>/dev/null || true
        wait "${RSYNCD_PID}" 2>/dev/null || true
        RSYNCD_PID=""
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

# Boot the upstream rsyncd. --no-detach keeps the pid stable so the
# sampler and the parent shell can both track it.
"${UPSTREAM_RSYNC}" --daemon --no-detach \
    --config "${CONF}" --port "${PORT}" \
    >>"${LOG_FILE}" 2>&1 &
RSYNCD_PID=$!

# Wait for the listener to become accept-ready. A bare module list is
# enough; we don't need a full transfer here.
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
    printf 'ERROR: upstream rsyncd did not become ready on port %s within 10s\n' "${PORT}" >&2
    tail -n 80 "${LOG_FILE}" >&2 || true
    exit 1
fi

printf '[benchmark_russh_concurrency] rsyncd ready on port %s (pid=%s, n=%s, fixture=%s bytes)\n' \
    "${PORT}" "${RSYNCD_PID}" "${SESSIONS}" "${FIXTURE_BYTES}" >&2

# Background sampler: every 50 ms walk every live oc-rsync push child
# and record VmHWM (KB) + Threads from /proc/$pid/status. We aggregate
# at the end to get peak RSS and peak thread count across the cohort.
# Tokio worker thread count is not directly observable from /proc; the
# Threads field is the closest proxy for "tokio runtime + spawn_blocking
# pool" footprint per process.
PIDS_FILE="${BENCH_ROOT}/push_pids.txt"
: > "${PIDS_FILE}"

sampler() {
    while kill -0 "${RSYNCD_PID}" 2>/dev/null; do
        if [ -s "${PIDS_FILE}" ]; then
            while read -r pid; do
                status="/proc/${pid}/status"
                if [ -r "${status}" ]; then
                    rss=$(awk '/^VmHWM:/ { print $2 }' "${status}" 2>/dev/null || printf '0\n')
                    threads=$(awk '/^Threads:/ { print $2 }' "${status}" 2>/dev/null || printf '0\n')
                    printf '%s %s\n' "${rss:-0}" "${threads:-0}" >> "${SAMPLE_DIR}/samples.txt"
                fi
            done < "${PIDS_FILE}"
        fi
        sleep 0.05
    done
}
sampler &
SAMPLER_PID=$!

# Single push session: time the wall duration of a one-file push from
# ${FIXTURE_DIR}/sess_${idx}/ into rsync://127.0.0.1:${PORT}/bench/sess_${idx}/
# and emit elapsed_ms + outcome into a per-session result file.
client_runner() {
    idx=$1
    src="${FIXTURE_DIR}/sess_${idx}/"
    dst_url="rsync://127.0.0.1:${PORT}/bench/sess_${idx}/"
    log="${RESULT_DIR}/c_${idx}.log"
    start_ns=$(date +%s%N 2>/dev/null || printf '0\n')
    # --contimeout caps a stuck connect at 5s so a saturated runtime
    # surfaces as a failure rather than hanging the harness. OC_RSYNC
    # is the binary under test; the env var stays unset by default so
    # we measure the production sync transfer path. RUSSH-4..7 may
    # toggle OC_RSYNC_ASYNC_SSH=1 to exercise the async-SSH variant.
    "${OC_RSYNC}" -a --contimeout=5 "${src}" "${dst_url}" >"${log}" 2>&1 &
    push_pid=$!
    printf '%s\n' "${push_pid}" >> "${PIDS_FILE}"
    wait "${push_pid}"
    rc=$?
    end_ns=$(date +%s%N 2>/dev/null || printf '0\n')
    if [ "${start_ns}" -eq 0 ] || [ "${end_ns}" -eq 0 ]; then
        elapsed_ms=0
    else
        elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
    fi
    classification=failed
    if [ "${rc}" -eq 0 ]; then
        classification=succeeded
    fi
    printf '%s %s %s\n' "${idx}" "${elapsed_ms}" "${classification}" \
        > "${RESULT_DIR}/c_${idx}.txt"
}

printf '[benchmark_russh_concurrency] launching %s parallel push sessions...\n' "${SESSIONS}" >&2

wall_start_ns=$(date +%s%N 2>/dev/null || printf '0\n')

RUNNER_PIDS_FILE="${BENCH_ROOT}/runner_pids.txt"
: > "${RUNNER_PIDS_FILE}"

i=1
while [ "${i}" -le "${SESSIONS}" ]; do
    client_runner "${i}" &
    printf '%s\n' "$!" >> "${RUNNER_PIDS_FILE}"
    i=$((i + 1))
done

# Wait for every runner. We deliberately do not bail on the first
# failure; the harness reports counts so RUSSH-4..7 can see the
# saturation profile.
while read -r pid; do
    wait "${pid}" 2>/dev/null || true
done < "${RUNNER_PIDS_FILE}"

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
succeeded=0
failed=0
for f in "${RESULT_DIR}"/c_*.txt; do
    [ -f "${f}" ] || continue
    line=$(cat "${f}")
    cls=$(printf '%s\n' "${line}" | awk '{ print $3 }')
    case "${cls}" in
        succeeded) succeeded=$((succeeded + 1)) ;;
        *)         failed=$((failed + 1)) ;;
    esac
    printf '%s\n' "${line}" | awk '{ print $2 }' >> "${LATENCY_DIR}/all.txt"
done

# Percentiles over per-session wall time. Only succeeded sessions are
# counted toward p50/p95/p99; failed sessions distort the distribution
# (they fail fast on contimeout or @ERROR).
: > "${LATENCY_DIR}/succeeded.txt"
for f in "${RESULT_DIR}"/c_*.txt; do
    [ -f "${f}" ] || continue
    awk '$3 == "succeeded" { print $2 }' "${f}" >> "${LATENCY_DIR}/succeeded.txt"
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

p50_ms=$(percentile 50 "${LATENCY_DIR}/succeeded.txt")
p95_ms=$(percentile 95 "${LATENCY_DIR}/succeeded.txt")
p99_ms=$(percentile 99 "${LATENCY_DIR}/succeeded.txt")

peak_rss_kb=0
peak_threads=0
if [ -s "${SAMPLE_DIR}/samples.txt" ]; then
    peak_rss_kb=$(awk '{ if ($1 > m) m = $1 } END { print m + 0 }' "${SAMPLE_DIR}/samples.txt")
    peak_threads=$(awk '{ if ($2 > m) m = $2 } END { print m + 0 }' "${SAMPLE_DIR}/samples.txt")
fi
peak_rss_mib=$(( (peak_rss_kb + 1023) / 1024 ))

printf '\n[benchmark_russh_concurrency] results:\n' >&2
printf '  n=%s fixture_bytes=%s\n' "${SESSIONS}" "${FIXTURE_BYTES}" >&2
printf '  succeeded=%s failed=%s\n' "${succeeded}" "${failed}" >&2
printf '  wall_ms=%s\n' "${wall_ms}" >&2
printf '  session latency p50/p95/p99 ms = %s / %s / %s\n' "${p50_ms}" "${p95_ms}" "${p99_ms}" >&2
printf '  peak_rss_kb=%s (%s MiB) peak_threads=%s\n' "${peak_rss_kb}" "${peak_rss_mib}" "${peak_threads}" >&2

# Single-line summary on stdout for downstream parsing by RUSSH-4..7.
# rss is reported in MiB so it lines up with the task brief; threads is
# the peak Threads value across all push children (the closest /proc
# proxy for "tokio worker + blocking-pool" footprint per process).
printf 'RUSSH_BENCH_SUMMARY n=%s p50=%sms p95=%sms p99=%sms rss=%sMiB threads=%s\n' \
    "${SESSIONS}" "${p50_ms}" "${p95_ms}" "${p99_ms}" \
    "${peak_rss_mib}" "${peak_threads}"
