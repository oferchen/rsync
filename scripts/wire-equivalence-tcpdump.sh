#!/usr/bin/env sh
# wire-equivalence-tcpdump.sh
# ---------------------------------------------------------------------------
# Captures rsync daemon traffic produced by upstream rsync 3.4.1 and by
# oc-rsync running the same push scenario, then compares the application
# payloads byte-for-byte (and via SHA-256) to flag wire divergences.
#
# Designed for the `rsync-profile` container (or any Linux host with both
# binaries plus tcpdump/tshark). It runs entirely under a dedicated scratch
# directory so the host bind-mounted workspace is never touched destructively.
#
# Usage:
#   sh scripts/wire-equivalence-tcpdump.sh [--scenario small-tree]
#   # Or, after `chmod +x`, `scripts/wire-equivalence-tcpdump.sh ...`
#
# Environment overrides:
#   OC_RSYNC_BIN          path to oc-rsync (default: target/release/oc-rsync)
#   UPSTREAM_RSYNC_BIN    path to upstream rsync (default: rsync from PATH)
#   WIRE_EQUIV_KEEP       keep scratch dir on exit when set to 1
#   WIRE_EQUIV_PORT_BASE  TCP port base for ephemeral daemons (default: 28730)
#   WIRE_EQUIV_IFACE      capture interface (default: lo)
#
# Exit codes:
#   0  payload hashes match
#   1  payload hashes differ
#   2  prerequisite missing or transfer failed
#   77 skipped (not running on Linux, or required tools absent)
# ---------------------------------------------------------------------------

set -eu

log() {
    printf '[wire-equiv] %s\n' "$*"
}

skip() {
    log "SKIP: $*"
    exit 77
}

fail() {
    log "FAIL: $*"
    exit 2
}

scenario="small-tree"
while [ $# -gt 0 ]; do
    case "$1" in
        --scenario)
            scenario="$2"
            shift 2
            ;;
        --scenario=*)
            scenario="${1#--scenario=}"
            shift
            ;;
        -h|--help)
            sed -n '1,30p' "$0"
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

uname_s=$(uname -s 2>/dev/null || echo unknown)
case "${uname_s}" in
    Linux) : ;;
    *) skip "tcpdump capture path only runs on Linux (saw ${uname_s})" ;;
esac

for tool in tcpdump tshark sha256sum mktemp; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        skip "required tool not on PATH: ${tool}"
    fi
done

upstream_bin="${UPSTREAM_RSYNC_BIN:-}"
if [ -z "${upstream_bin}" ]; then
    if command -v rsync >/dev/null 2>&1; then
        upstream_bin=$(command -v rsync)
    else
        skip "upstream rsync binary not found; set UPSTREAM_RSYNC_BIN"
    fi
fi

oc_bin="${OC_RSYNC_BIN:-}"
if [ -z "${oc_bin}" ]; then
    if command -v oc-rsync >/dev/null 2>&1; then
        oc_bin=$(command -v oc-rsync)
    else
        # Best-effort: look beside the repo root.
        candidate="$(cd "$(dirname "$0")/.." && pwd)/target/release/oc-rsync"
        if [ -x "${candidate}" ]; then
            oc_bin="${candidate}"
        else
            skip "oc-rsync binary not found; set OC_RSYNC_BIN"
        fi
    fi
fi

if ! "${upstream_bin}" --version >/dev/null 2>&1; then
    fail "upstream binary not runnable: ${upstream_bin}"
fi
if ! "${oc_bin}" --version >/dev/null 2>&1; then
    fail "oc-rsync binary not runnable: ${oc_bin}"
fi

port_base="${WIRE_EQUIV_PORT_BASE:-28730}"
iface="${WIRE_EQUIV_IFACE:-lo}"
up_port=$((port_base + 0))
oc_port=$((port_base + 1))

scratch_root="${TMPDIR:-/tmp}/oc-rsync-wire-equiv.$$"
mkdir -p "${scratch_root}"
chmod 700 "${scratch_root}"
log "scratch dir: ${scratch_root}"

up_pid=""
oc_pid=""
up_tcpdump_pid=""
oc_tcpdump_pid=""

cleanup() {
    # Stop captures first so packets flush.
    for pid in "${up_tcpdump_pid}" "${oc_tcpdump_pid}"; do
        if [ -n "${pid}" ]; then
            kill "${pid}" >/dev/null 2>&1 || true
            wait "${pid}" >/dev/null 2>&1 || true
        fi
    done
    for pid in "${up_pid}" "${oc_pid}"; do
        if [ -n "${pid}" ]; then
            kill "${pid}" >/dev/null 2>&1 || true
            wait "${pid}" >/dev/null 2>&1 || true
        fi
    done
    if [ "${WIRE_EQUIV_KEEP:-0}" != "1" ]; then
        # Defensive: never blow away anything outside scratch_root.
        case "${scratch_root}" in
            "${TMPDIR:-/tmp}/oc-rsync-wire-equiv."*)
                rm -rf -- "${scratch_root}"
                ;;
            *)
                log "refusing to remove unexpected scratch path: ${scratch_root}"
                ;;
        esac
    else
        log "kept scratch dir at ${scratch_root}"
    fi
}
trap cleanup EXIT INT TERM HUP

# --- scenario tree -----------------------------------------------------------
source_tree="${scratch_root}/source"
mkdir -p "${source_tree}"
case "${scenario}" in
    small-tree)
        i=0
        while [ "${i}" -lt 100 ]; do
            # Deterministic content keyed by index so both transfers produce
            # the same delta workload.
            printf 'wire-equivalence-file-%03d-payload\n' "${i}" \
                > "${source_tree}/file-$(printf '%03d' "${i}").txt"
            i=$((i + 1))
        done
        ;;
    single-file)
        printf 'wire-equivalence-single-file\n' > "${source_tree}/single.txt"
        ;;
    *)
        fail "unknown scenario: ${scenario}"
        ;;
esac
log "scenario '${scenario}' prepared in ${source_tree}"

# --- daemon configs ----------------------------------------------------------
up_dest="${scratch_root}/up-dest"
oc_dest="${scratch_root}/oc-dest"
mkdir -p "${up_dest}" "${oc_dest}"

up_conf="${scratch_root}/upstream-daemon.conf"
oc_conf="${scratch_root}/oc-daemon.conf"
up_log="${scratch_root}/upstream-daemon.log"
oc_log="${scratch_root}/oc-daemon.log"
up_pidfile="${scratch_root}/upstream-daemon.pid"
oc_pidfile="${scratch_root}/oc-daemon.pid"

write_conf() {
    conf=$1
    pidfile=$2
    port=$3
    dest=$4
    cat > "${conf}" <<EOF
pid file = ${pidfile}
port = ${port}
use chroot = false
numeric ids = yes

[wire]
    path = ${dest}
    comment = wire equivalence target
    read only = false
EOF
}

write_conf "${up_conf}" "${up_pidfile}" "${up_port}" "${up_dest}"
write_conf "${oc_conf}" "${oc_pidfile}" "${oc_port}" "${oc_dest}"

# --- captures ---------------------------------------------------------------
up_pcap="${scratch_root}/upstream.pcap"
oc_pcap="${scratch_root}/oc.pcap"

log "starting tcpdump capture on ${iface} for ports ${up_port}, ${oc_port}"

# -i ${iface}: loopback; -U: write immediately; -s 0: full snaplen.
# tcpdump is backgrounded in the current shell (NOT a subshell) so $! is
# observable and the job survives.
tcpdump -i "${iface}" -U -s 0 -w "${up_pcap}" "tcp port ${up_port}" \
    >/dev/null 2>&1 &
up_tcpdump_pid=$!
tcpdump -i "${iface}" -U -s 0 -w "${oc_pcap}" "tcp port ${oc_port}" \
    >/dev/null 2>&1 &
oc_tcpdump_pid=$!

# Give tcpdump a moment to attach before traffic starts.
sleep 1
if ! kill -0 "${up_tcpdump_pid}" 2>/dev/null \
        || ! kill -0 "${oc_tcpdump_pid}" 2>/dev/null; then
    fail "tcpdump exited immediately (need CAP_NET_RAW or root?)"
fi

# --- daemons ----------------------------------------------------------------
log "starting upstream rsync daemon on 127.0.0.1:${up_port}"
"${upstream_bin}" --daemon --no-detach --config "${up_conf}" \
    --log-file "${up_log}" >/dev/null 2>&1 &
up_pid=$!

log "starting oc-rsync daemon on 127.0.0.1:${oc_port}"
OC_RSYNC_DAEMON_FALLBACK=0 \
    "${oc_bin}" --daemon --no-detach --config "${oc_conf}" \
    --log-file "${oc_log}" >/dev/null 2>&1 &
oc_pid=$!

# Wait for both ports to accept connections. Uses python3 when available
# (portable POSIX socket probe); otherwise falls back to a fixed sleep.
wait_port() {
    target=$1
    if command -v python3 >/dev/null 2>&1; then
        python3 - "${target}" <<'PY'
import socket, sys, time
port = int(sys.argv[1])
deadline = time.time() + 10
while time.time() < deadline:
    s = socket.socket()
    s.settimeout(0.5)
    try:
        s.connect(("127.0.0.1", port))
        s.close()
        sys.exit(0)
    except OSError:
        time.sleep(0.25)
sys.exit(1)
PY
    else
        sleep 3
    fi
}

if ! wait_port "${up_port}"; then
    fail "upstream daemon did not accept on port ${up_port}"
fi
if ! wait_port "${oc_port}"; then
    fail "oc-rsync daemon did not accept on port ${oc_port}"
fi

# --- transfers --------------------------------------------------------------
log "running upstream client -> upstream daemon"
if ! "${upstream_bin}" -a --no-motd --timeout=20 \
        "${source_tree}/" "rsync://127.0.0.1:${up_port}/wire" \
        >"${scratch_root}/upstream-client.out" 2>&1; then
    cat "${scratch_root}/upstream-client.out" >&2 || true
    fail "upstream client transfer failed"
fi

log "running upstream client -> oc-rsync daemon"
if ! "${upstream_bin}" -a --no-motd --timeout=20 \
        "${source_tree}/" "rsync://127.0.0.1:${oc_port}/wire" \
        >"${scratch_root}/oc-client.out" 2>&1; then
    cat "${scratch_root}/oc-client.out" >&2 || true
    fail "upstream client -> oc-rsync daemon transfer failed"
fi

# Give tcpdump a beat to flush trailing packets before we kill it.
sleep 1

kill "${up_tcpdump_pid}" >/dev/null 2>&1 || true
kill "${oc_tcpdump_pid}" >/dev/null 2>&1 || true
wait "${up_tcpdump_pid}" >/dev/null 2>&1 || true
wait "${oc_tcpdump_pid}" >/dev/null 2>&1 || true
up_tcpdump_pid=""
oc_tcpdump_pid=""

# --- payload extraction -----------------------------------------------------
# tshark reassembles TCP and strips IP/TCP headers. We project the per-direction
# payload streams so timing, window, and ephemeral source ports do not show up
# in the diff.
extract_payload() {
    pcap=$1
    server_port=$2
    out=$3
    # tcp.stream: per-flow id (deterministic ordering on a fresh loopback run).
    # tcp.srcport == server_port: server->client direction (daemon talking).
    # We export both directions in stream order, separated by NUL bytes between
    # records, which is enough for a stable hash without needing payload diff to
    # be valid bytes.
    tshark -r "${pcap}" -q -T fields \
        -e tcp.stream -e tcp.srcport -e tcp.dstport \
        -e tcp.payload \
        -Y "tcp.payload and (tcp.srcport == ${server_port} or tcp.dstport == ${server_port})" \
        2>/dev/null \
        | sort -k1,1n -s \
        > "${out}"
}

extract_payload "${up_pcap}" "${up_port}" "${scratch_root}/upstream.payload.tsv"
extract_payload "${oc_pcap}" "${oc_port}" "${scratch_root}/oc.payload.tsv"

# Strip the daemon-port column before hashing so the only port-dependent value
# (the server port itself) does not perturb the hash.
strip_ports() {
    awk -v server="$2" \
        'BEGIN{FS=OFS="\t"} { print $1, ($2==server?"S":"C"), $4 }' \
        "$1"
}

strip_ports "${scratch_root}/upstream.payload.tsv" "${up_port}" \
    > "${scratch_root}/upstream.payload.normalized"
strip_ports "${scratch_root}/oc.payload.tsv" "${oc_port}" \
    > "${scratch_root}/oc.payload.normalized"

up_hash=$(sha256sum "${scratch_root}/upstream.payload.normalized" | awk '{print $1}')
oc_hash=$(sha256sum "${scratch_root}/oc.payload.normalized" | awk '{print $1}')

log "upstream payload hash: ${up_hash}"
log "oc-rsync payload hash: ${oc_hash}"

# --- report ------------------------------------------------------------------
report="${scratch_root}/report.txt"
{
    printf 'scenario: %s\n' "${scenario}"
    printf 'upstream_bin: %s\n' "${upstream_bin}"
    printf 'oc_bin: %s\n' "${oc_bin}"
    printf 'upstream_pcap: %s\n' "${up_pcap}"
    printf 'oc_pcap: %s\n' "${oc_pcap}"
    printf 'upstream_payload_sha256: %s\n' "${up_hash}"
    printf 'oc_payload_sha256: %s\n' "${oc_hash}"
    if [ "${up_hash}" = "${oc_hash}" ]; then
        printf 'result: MATCH\n'
    else
        printf 'result: DIVERGE\n'
    fi
} | tee "${report}"

# Always emit a byte-level diff hint (truncated) for debugging.
if [ "${up_hash}" != "${oc_hash}" ]; then
    log "diff (first 40 lines):"
    diff -u "${scratch_root}/upstream.payload.normalized" \
        "${scratch_root}/oc.payload.normalized" | head -n 40 || true
    log "full diff and pcaps retained under ${scratch_root}"
    export WIRE_EQUIV_KEEP=1
    exit 1
fi

log "wire payloads are byte-equivalent for scenario '${scenario}'"
exit 0
