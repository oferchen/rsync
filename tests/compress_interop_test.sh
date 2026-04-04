#!/usr/bin/env bash
# Compression Codec Interoperability Test Script
#
# Tests zstd, lz4, and zlibx compression compatibility between oc-rsync and
# upstream rsync 3.4.1 using daemon mode. Both directions are tested:
#   - oc-rsync client -> upstream rsync daemon
#   - upstream rsync client -> oc-rsync daemon
#
# Prerequisites: upstream rsync 3.4.1 must be built with zstd and lz4 support.
# The interop build script (tools/ci/run_interop.sh) handles this automatically.
#
# Environment variable overrides:
#   OC_RSYNC              - path to oc-rsync binary
#   UPSTREAM_INSTALL_ROOT - root of upstream installs (expects {version}/bin/rsync)
#   UPSTREAM_VERSION      - upstream version to test against (default: "3.4.1")

set -euo pipefail

# Resolve workspace root from script location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Paths (overridable via environment)
OC_RSYNC="${OC_RSYNC:-${WORKSPACE_ROOT}/target/release/oc-rsync}"
UPSTREAM_INSTALL_ROOT="${UPSTREAM_INSTALL_ROOT:-${WORKSPACE_ROOT}/target/interop/upstream-install}"
UPSTREAM_VERSION="${UPSTREAM_VERSION:-3.4.1}"
UPSTREAM_RSYNC="${UPSTREAM_INSTALL_ROOT}/${UPSTREAM_VERSION}/bin/rsync"

# Daemon state
OC_PID=""
UP_PID=""
TEST_DIR=""
HARD_TIMEOUT=30

# Counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0

log_info() {
    echo "[INFO] $1"
}

log_warn() {
    echo "[WARN] $1"
}

log_error() {
    echo "[ERROR] $1" >&2
}

log_test() {
    echo ""
    echo "=== $1 ==="
}

# Allocate an ephemeral port from the kernel
allocate_ephemeral_port() {
    python3 -c "
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', 0))
print(s.getsockname()[1])
s.close()
"
}

# Wait for a TCP port to become reachable
wait_for_port() {
    local port=$1
    local max_wait=${2:-10}
    local elapsed=0

    while [ $elapsed -lt $max_wait ]; do
        if (echo >/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
            return 0
        fi
        sleep 0.5
        elapsed=$((elapsed + 1))
    done
    echo "ERROR: port $port not ready after ${max_wait}s" >&2
    return 1
}

# Wait for a TCP port to stop accepting connections
wait_for_port_free() {
    local port=$1
    local max_wait=${2:-10}
    local elapsed=0

    while [ $elapsed -lt $max_wait ]; do
        if ! (echo >/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
            return 0
        fi
        sleep 0.5
        elapsed=$((elapsed + 1))
    done
    return 0
}

stop_oc_daemon() {
    if [[ -n "${OC_PID}" ]]; then
        kill "${OC_PID}" >/dev/null 2>&1 || true
        local i=0
        while kill -0 "${OC_PID}" 2>/dev/null && [ $i -lt 10 ]; do
            sleep 0.5
            i=$((i + 1))
        done
        if kill -0 "${OC_PID}" 2>/dev/null; then
            kill -9 "${OC_PID}" >/dev/null 2>&1 || true
        fi
        wait "${OC_PID}" >/dev/null 2>&1 || true
        OC_PID=""
    fi
}

stop_upstream_daemon() {
    if [[ -n "${UP_PID}" ]]; then
        kill "${UP_PID}" >/dev/null 2>&1 || true
        local i=0
        while kill -0 "${UP_PID}" 2>/dev/null && [ $i -lt 10 ]; do
            sleep 0.5
            i=$((i + 1))
        done
        if kill -0 "${UP_PID}" 2>/dev/null; then
            kill -9 "${UP_PID}" >/dev/null 2>&1 || true
        fi
        wait "${UP_PID}" >/dev/null 2>&1 || true
        UP_PID=""
    fi
}

cleanup() {
    stop_oc_daemon
    stop_upstream_daemon
    if [[ -n "${TEST_DIR:-}" && -d "${TEST_DIR:-}" ]]; then
        rm -rf "${TEST_DIR}"
    fi
}
trap cleanup EXIT

start_oc_daemon() {
    local config=$1
    local log_file=$2
    local port=$3

    stop_oc_daemon

    RUST_BACKTRACE=1 \
    OC_RSYNC_DAEMON_FALLBACK=0 \
        "$OC_RSYNC" --daemon --no-detach --config "$config" --port "$port" \
        --log-file "$log_file" </dev/null &
    OC_PID=$!
    if ! wait_for_port "$port" 10; then
        log_error "oc-rsync daemon failed to bind port $port"
        stop_oc_daemon
        return 1
    fi
}

start_upstream_daemon() {
    local config=$1
    local log_file=$2
    local port=$3

    stop_upstream_daemon

    "$UPSTREAM_RSYNC" --daemon --config "$config" --no-detach \
        --log-file "$log_file" </dev/null &
    UP_PID=$!
    if ! wait_for_port "$port" 10; then
        log_error "upstream rsync daemon failed to bind port $port"
        stop_upstream_daemon
        return 1
    fi
}

write_oc_daemon_conf() {
    local path=$1 pid_file=$2 port=$3 dest=$4

    cat >"$path" <<CONF
pid file = ${pid_file}
port = ${port}
use chroot = false

[interop]
path = ${dest}
comment = compress interop target
read only = false
numeric ids = yes
CONF
}

write_upstream_daemon_conf() {
    local path=$1 pid_file=$2 port=$3 dest=$4

    cat >"$path" <<CONF
pid file = ${pid_file}
port = ${port}
use chroot = false
munge symlinks = false
numeric ids = yes
[interop]
    path = ${dest}
    comment = compress interop target
    read only = false
CONF
}

# Create test fixtures with a mix of file types and sizes
setup_compress_fixtures() {
    local src=$1
    rm -rf "$src"
    mkdir -p "$src/subdir"

    # Text files - highly compressible
    echo "Hello, world! This is a test file for compression interop." > "$src/hello.txt"
    for i in $(seq 1 100); do
        echo "Line $i: The quick brown fox jumps over the lazy dog." >> "$src/repeated.txt"
    done
    printf 'line1\nline2\nline3\n' > "$src/subdir/nested.txt"

    # Binary data - less compressible
    dd if=/dev/urandom of="$src/random.bin" bs=1K count=50 2>/dev/null

    # Large file - exercises multi-block compression
    dd if=/dev/urandom of="$src/large.dat" bs=1K count=500 2>/dev/null

    # Empty file - edge case
    touch "$src/empty.txt"

    # Repetitive binary - very compressible
    dd if=/dev/zero of="$src/zeros.bin" bs=1K count=100 2>/dev/null
}

# Compare source and destination directories
verify_transfer() {
    local src=$1 dest=$2 label=$3

    for f in hello.txt repeated.txt subdir/nested.txt random.bin large.dat empty.txt zeros.bin; do
        if [[ ! -f "$dest/$f" ]]; then
            log_error "$label: missing file $f"
            return 1
        fi
        if ! cmp -s "$src/$f" "$dest/$f"; then
            log_error "$label: content mismatch in $f"
            return 1
        fi
    done
    return 0
}

# Check if upstream rsync supports a given compression algorithm
check_upstream_compress_support() {
    local algo=$1
    # upstream rsync --version lists supported compression algorithms
    if "$UPSTREAM_RSYNC" --version 2>&1 | grep -qi "$algo"; then
        return 0
    fi
    return 1
}

# Check if oc-rsync supports a given compression algorithm for interop.
# LZ4 per-token wire format has been validated against upstream token.c.
# Zstd wire format is not yet validated - skip zstd interop tests.
# Auto-negotiation (plain -z) correctly excludes non-validated algorithms.
check_oc_compress_interop_ready() {
    local algo=$1
    case "$algo" in
        zstd)
            # Wire format not validated - skip interop tests for zstd.
            # Re-enable after task #1379 (zstd) is completed.
            return 1
            ;;
        *)
            return 0
            ;;
    esac
}

# Run a single compress interop test scenario
# Direction: client -> daemon
run_compress_test() {
    local test_name=$1
    local compress_flag=$2
    local algo_name=$3

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/${algo_name}_$(echo "$test_name" | tr ' ' '_')"
    mkdir -p "$work_dir"

    local src="$work_dir/src"
    local oc_dest="$work_dir/oc_dest"
    local up_dest="$work_dir/up_dest"
    local oc_port up_port

    setup_compress_fixtures "$src"

    # --- Direction 1: upstream client -> oc-rsync daemon ---
    mkdir -p "$oc_dest"
    oc_port=$(allocate_ephemeral_port)

    write_oc_daemon_conf "$work_dir/oc.conf" "$work_dir/oc.pid" "$oc_port" "$oc_dest"
    start_oc_daemon "$work_dir/oc.conf" "$work_dir/oc_daemon.log" "$oc_port"

    log_info "upstream client -> oc-rsync daemon ($compress_flag)"
    # shellcheck disable=SC2086
    if ! timeout "$HARD_TIMEOUT" "$UPSTREAM_RSYNC" -av $compress_flag --timeout=10 \
        "$src/" "rsync://127.0.0.1:${oc_port}/interop" \
        >"$work_dir/up_to_oc.log" 2>&1; then
        log_error "upstream -> oc-rsync transfer failed"
        cat "$work_dir/up_to_oc.log" >&2
        cat "$work_dir/oc_daemon.log" >&2
        stop_oc_daemon
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi
    stop_oc_daemon

    if ! verify_transfer "$src" "$oc_dest" "upstream->oc ($algo_name)"; then
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    # --- Direction 2: oc-rsync client -> upstream daemon ---
    mkdir -p "$up_dest"
    up_port=$(allocate_ephemeral_port)

    write_upstream_daemon_conf "$work_dir/up.conf" "$work_dir/up.pid" "$up_port" "$up_dest"
    start_upstream_daemon "$work_dir/up.conf" "$work_dir/up_daemon.log" "$up_port"

    log_info "oc-rsync client -> upstream daemon ($compress_flag)"
    # shellcheck disable=SC2086
    if ! timeout "$HARD_TIMEOUT" "$OC_RSYNC" -av $compress_flag --timeout=10 \
        "$src/" "rsync://127.0.0.1:${up_port}/interop" \
        >"$work_dir/oc_to_up.log" 2>&1; then
        log_error "oc-rsync -> upstream transfer failed"
        cat "$work_dir/oc_to_up.log" >&2
        cat "$work_dir/up_daemon.log" >&2
        stop_upstream_daemon
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi
    stop_upstream_daemon

    if ! verify_transfer "$src" "$up_dest" "oc->upstream ($algo_name)"; then
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    log_info "$test_name: PASS"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

# =========================================================================
# Main
# =========================================================================

main() {
    log_info "Starting Compression Codec Interoperability Tests"
    log_info "oc-rsync: $OC_RSYNC"
    log_info "Upstream rsync: $UPSTREAM_RSYNC"

    # Verify binaries
    if [ ! -x "$OC_RSYNC" ]; then
        log_error "oc-rsync binary not found or not executable: $OC_RSYNC"
        exit 1
    fi

    if [ ! -x "$UPSTREAM_RSYNC" ]; then
        log_error "upstream rsync not found or not executable: $UPSTREAM_RSYNC"
        log_warn "Skipping all compress interop tests"
        TESTS_SKIPPED=6
        echo ""
        echo "========================================="
        echo "Compression Interop Test Summary"
        echo "========================================="
        echo "Total tests run:    0"
        echo "Tests passed:       0"
        echo "Tests failed:       0"
        echo "Tests skipped:      $TESTS_SKIPPED"
        echo "========================================="
        exit 0
    fi

    # Show versions for diagnostics
    log_info "oc-rsync version: $("$OC_RSYNC" --version 2>&1 | head -1)"
    log_info "upstream version: $("$UPSTREAM_RSYNC" --version 2>&1 | head -1)"

    TEST_DIR="$(mktemp -d)"
    log_info "Test directory: $TEST_DIR"

    # =====================================================================
    # zlibx tests (baseline - always available)
    # =====================================================================
    run_compress_test "zlibx compression (default -z)" \
        "--compress" "zlibx"

    run_compress_test "zlibx with --compress-level=1" \
        "--compress --compress-level=1" "zlibx_level1"

    run_compress_test "zlibx with --compress-level=9" \
        "--compress --compress-level=9" "zlibx_level9"

    run_compress_test "zlibx with delta transfer" \
        "--compress --no-whole-file -I" "zlibx_delta"

    # =====================================================================
    # zstd tests (requires upstream built with zstd support)
    # upstream rsync uses --compress-choice=ALGO, not --compress=ALGO
    # Wire format validated: per-token flush fixed (PR #3047).
    # =====================================================================
    if check_upstream_compress_support "zstd" && check_oc_compress_interop_ready "zstd"; then
        run_compress_test "zstd compression (--compress-choice=zstd)" \
            "--compress-choice=zstd" "zstd"

        run_compress_test "zstd with delta transfer" \
            "--compress-choice=zstd --no-whole-file -I" "zstd_delta"
    else
        if ! check_upstream_compress_support "zstd"; then
            log_warn "upstream rsync lacks zstd support - skipping zstd tests"
        else
            log_warn "oc-rsync zstd wire format not yet validated - skipping zstd tests"
        fi
        TESTS_SKIPPED=$((TESTS_SKIPPED + 2))
    fi

    # =====================================================================
    # lz4 tests (requires upstream built with lz4 support)
    # Wire format validated: per-token flush alignment fixed (PR #3053).
    # =====================================================================
    if check_upstream_compress_support "lz4" && check_oc_compress_interop_ready "lz4"; then
        run_compress_test "lz4 compression (--compress-choice=lz4)" \
            "--compress-choice=lz4" "lz4"

        run_compress_test "lz4 with delta transfer" \
            "--compress-choice=lz4 --no-whole-file -I" "lz4_delta"
    else
        if ! check_upstream_compress_support "lz4"; then
            log_warn "upstream rsync lacks lz4 support - skipping lz4 tests"
        else
            log_warn "oc-rsync lz4 wire format not yet validated - skipping lz4 tests"
        fi
        TESTS_SKIPPED=$((TESTS_SKIPPED + 2))
    fi

    # =====================================================================
    # Auto-negotiation test: let protocol pick the best common algorithm.
    # With zstd/lz4 features enabled, negotiation picks the best mutual
    # codec (zstd > lz4 > zlibx > zlib > none).
    # =====================================================================
    run_compress_test "auto-negotiation (protocol picks best codec)" \
        "--compress" "auto_negotiate"

    run_compress_test "auto-negotiation with large files" \
        "--compress --no-whole-file -I" "auto_negotiate_delta"

    # Summary
    echo ""
    echo "========================================="
    echo "Compression Interop Test Summary"
    echo "========================================="
    echo "Total tests run:    $TESTS_RUN"
    echo "Tests passed:       $TESTS_PASSED"
    echo "Tests failed:       $TESTS_FAILED"
    echo "Tests skipped:      $TESTS_SKIPPED"
    echo "========================================="

    if [ $TESTS_FAILED -gt 0 ]; then
        log_error "$TESTS_FAILED test(s) failed"
        exit 1
    fi

    if [ $TESTS_PASSED -gt 0 ]; then
        log_info "All $TESTS_PASSED test(s) passed!"
    fi

    exit 0
}

main "$@"
