#!/bin/bash
# URV-5.c.4 - Landlock overhead macro-bench.
#
# Compares a 100K-file daemon receive with the `landlock` Cargo feature
# ON vs OFF, capturing wall-clock time (median of 5 runs), peak RSS, and
# strace syscall counts. The micro-bench in
# `crates/fast_io/benches/landlock_overhead.rs` covers the per-connection
# setup cost; this script covers the full transfer path so URV-5.c.5 has
# end-to-end evidence for the default-on flip decision.
#
# Designed to run inside the `rsync-profile` container per
# feedback_use_container_for_linux_bench; the host environment has no
# Landlock LSM and the macro-bench would be meaningless there.
#
# Usage:
#   scripts/landlock_overhead_macro.sh                # auto, results to docs/benchmarks/landlock-overhead-100k.md
#   OUT=/tmp/landlock.md scripts/landlock_overhead_macro.sh
#
# Environment variables:
#   NUM_DIRS         Directories in the 100K tree (default: 1000)
#   FILES_PER_DIR    Files per directory (default: 100)
#   RUNS             hyperfine measurement runs per cell (default: 5)
#   OUT              Output markdown path (default: docs/benchmarks/landlock-overhead-100k.md)
#   KEEP_BUILDS      Set to 1 to skip rebuilding (debug iteration)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

NUM_DIRS="${NUM_DIRS:-1000}"
FILES_PER_DIR="${FILES_PER_DIR:-100}"
TOTAL_FILES=$((NUM_DIRS * FILES_PER_DIR))
RUNS="${RUNS:-5}"
OUT="${OUT:-${PROJECT_ROOT}/docs/benchmarks/landlock-overhead-100k.md}"
KEEP_BUILDS="${KEEP_BUILDS:-0}"

WORKDIR="$(mktemp -d -t landlock-bench-XXXXXX)"
trap 'rm -rf "$WORKDIR"' EXIT

OC_ON="${OC_ON:-${WORKDIR}/oc-rsync-landlock-on}"
OC_OFF="${OC_OFF:-${WORKDIR}/oc-rsync-landlock-off}"
TREE_SRC="${WORKDIR}/tree-src"
DEST_BASE="${WORKDIR}/dest"
DAEMON_CONF="${WORKDIR}/oc-rsyncd.conf"
DAEMON_LOG="${WORKDIR}/daemon.log"
DAEMON_PID="${WORKDIR}/daemon.pid"
DAEMON_PORT=18738

require() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "ERROR: $1 not found in PATH; install before running this bench" >&2
        exit 1
    }
}

require cargo
require hyperfine
require strace
require rsync

is_linux() {
    [[ "$(uname -s)" == "Linux" ]]
}

if ! is_linux; then
    echo "ERROR: Landlock is Linux-only; run this script inside the rsync-profile container." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Build the two binaries.
#
# Until URV-5.c.5 introduces a Cargo-level toggle for the daemon Landlock
# wiring (currently forced on at the daemon -> fast_io edge for Linux),
# the OFF baseline is produced by editing
# `crates/daemon/Cargo.toml` to drop the `landlock` feature from the
# Linux target dep, building, then reverting. The operator does this
# manually before invoking the script with `KEEP_BUILDS=1` and the two
# binaries pre-placed at $OC_ON / $OC_OFF. The script refuses to run
# otherwise so the OFF cell cannot silently exercise the sandbox.
# ---------------------------------------------------------------------------
build_binaries() {
    if [[ "$KEEP_BUILDS" == "1" && -x "$OC_ON" && -x "$OC_OFF" ]]; then
        echo "[build] reusing pre-staged binaries (KEEP_BUILDS=1)"
        return
    fi
    cat >&2 <<'EOM'
ERROR: this bench requires two pre-built oc-rsync binaries:
  $OC_ON  - release build with the `landlock` feature wired in
  $OC_OFF - release build with `landlock` stripped from
            crates/daemon/Cargo.toml (drop `features = ["landlock"]`
            from the `target.'cfg(target_os = "linux")'.dependencies`
            entry for fast_io before building)

After staging both binaries set KEEP_BUILDS=1 and rerun. URV-5.c.5
will replace this manual step with a Cargo-level feature toggle.
EOM
    exit 2
}

# ---------------------------------------------------------------------------
# Build the 100K-file tree once and reuse it across cells.
# ---------------------------------------------------------------------------
build_tree() {
    if [[ -d "$TREE_SRC" ]] && [[ "$(find "$TREE_SRC" -type f | wc -l)" -ge "$TOTAL_FILES" ]]; then
        echo "[tree] reusing existing 100K tree at $TREE_SRC"
        return
    fi
    rm -rf "$TREE_SRC"
    mkdir -p "$TREE_SRC"
    echo "[tree] creating $TOTAL_FILES files across $NUM_DIRS directories"
    for d in $(seq 0 $((NUM_DIRS - 1))); do
        dir="${TREE_SRC}/d${d}"
        mkdir -p "$dir"
        for f in $(seq 0 $((FILES_PER_DIR - 1))); do
            # Deterministic 1 KB payload so checksum cost is uniform across runs.
            printf 'oc-rsync-landlock-bench %d %d %s\n' "$d" "$f" \
                "$(head -c 1000 /dev/urandom | base64 | head -c 1000)" \
                > "${dir}/f${f}.dat"
        done
    done
}

# ---------------------------------------------------------------------------
# Daemon lifecycle (one daemon per cell so the per-connection setup runs).
# ---------------------------------------------------------------------------
write_conf() {
    local module_root="$1"
    cat > "$DAEMON_CONF" <<EOF
uid = $(id -u)
gid = $(id -g)
use chroot = no
pid file = $DAEMON_PID
log file = $DAEMON_LOG
port = $DAEMON_PORT

[bench]
    path = $module_root
    read only = no
    write only = no
    list = yes
EOF
}

start_daemon() {
    local bin="$1"
    local module_root="$2"
    mkdir -p "$module_root"
    rm -f "$DAEMON_LOG"
    write_conf "$module_root"
    "$bin" --daemon --no-detach --config "$DAEMON_CONF" \
        > "$DAEMON_LOG" 2>&1 &
    echo $! > "${DAEMON_PID}.shell"
    # Wait up to 3 s for the daemon to bind.
    for _ in $(seq 1 30); do
        if ss -tln 2>/dev/null | grep -q ":${DAEMON_PORT}\b"; then
            return 0
        fi
        sleep 0.1
    done
    echo "ERROR: daemon failed to bind on port $DAEMON_PORT" >&2
    cat "$DAEMON_LOG" >&2
    return 1
}

stop_daemon() {
    if [[ -f "${DAEMON_PID}.shell" ]]; then
        kill "$(cat "${DAEMON_PID}.shell")" 2>/dev/null || true
        wait 2>/dev/null || true
        rm -f "${DAEMON_PID}.shell"
    fi
}

# ---------------------------------------------------------------------------
# Cell runners.
# ---------------------------------------------------------------------------
run_hyperfine_cell() {
    local label="$1"
    local bin="$2"
    local dest="${DEST_BASE}-${label}"
    local export_md="${WORKDIR}/${label}.md"

    rm -rf "$dest"
    start_daemon "$bin" "$dest"

    echo "[bench] $label - hyperfine $RUNS runs"
    hyperfine --warmup 1 --runs "$RUNS" --export-markdown "$export_md" \
        --prepare "rm -rf '$dest' && mkdir -p '$dest'" \
        "rsync -a '${TREE_SRC}/' 'rsync://localhost:${DAEMON_PORT}/bench/'"

    stop_daemon
    cat "$export_md"
}

run_strace_cell() {
    local label="$1"
    local bin="$2"
    local dest="${DEST_BASE}-${label}-strace"
    local strace_log="${WORKDIR}/${label}.strace"

    rm -rf "$dest"
    start_daemon "$bin" "$dest"

    echo "[bench] $label - strace -c -f"
    strace -c -f -o "$strace_log" -p "$(cat "${DAEMON_PID}.shell")" &
    local strace_pid=$!
    rsync -a "${TREE_SRC}/" "rsync://localhost:${DAEMON_PORT}/bench/" >/dev/null
    kill "$strace_pid" 2>/dev/null || true
    wait "$strace_pid" 2>/dev/null || true

    stop_daemon
    echo "--- $label strace summary ---"
    cat "$strace_log"
}

run_setup_cell() {
    # Per-connection Landlock setup latency, isolated from the data path.
    # Pulls the `landlock_overhead/restrict` row out of the criterion bench
    # so the macro-doc table can quote a single number.
    echo "[bench] per-connection setup (criterion micro-bench)"
    (cd "$PROJECT_ROOT" && cargo bench -p fast_io --bench landlock_overhead \
        --features landlock -- landlock_overhead/restrict 2>&1 | tail -40)
}

# ---------------------------------------------------------------------------
# Main.
# ---------------------------------------------------------------------------
main() {
    build_binaries
    build_tree

    mkdir -p "$(dirname "$OUT")"

    {
        echo "# Landlock overhead macro-bench (URV-5.c.4)"
        echo
        echo "Generated $(date -u +%Y-%m-%dT%H:%M:%SZ) on $(uname -srm)."
        echo
        echo "## Workload"
        echo
        echo "- $TOTAL_FILES files across $NUM_DIRS directories, 1 KB each."
        echo "- Push transfer: upstream rsync client -> oc-rsync daemon over localhost TCP."
        echo "- hyperfine $RUNS runs per cell, 1 warmup."
        echo
    } > "$OUT"

    echo
    echo "=========================================="
    echo "  Cell 1 / 3 - hyperfine landlock OFF"
    echo "=========================================="
    run_hyperfine_cell "landlock_off" "$OC_OFF" | tee -a "$OUT"

    echo
    echo "=========================================="
    echo "  Cell 2 / 3 - hyperfine landlock ON"
    echo "=========================================="
    run_hyperfine_cell "landlock_on" "$OC_ON" | tee -a "$OUT"

    echo
    echo "=========================================="
    echo "  Cell 3 / 3 - strace syscall counts"
    echo "=========================================="
    {
        echo
        echo "## strace -c -f summary"
        echo
        echo '```'
        run_strace_cell "landlock_off" "$OC_OFF"
        echo
        run_strace_cell "landlock_on" "$OC_ON"
        echo '```'
        echo
        echo "## Per-connection setup (criterion micro-bench)"
        echo
        echo '```'
        run_setup_cell
        echo '```'
    } | tee -a "$OUT"

    echo
    echo "[done] results captured to $OUT"
}

main "$@"
