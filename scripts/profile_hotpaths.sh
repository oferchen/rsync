#!/bin/bash
# Profile oc-rsync vs upstream rsync hot paths.
#
# Generates flamegraphs, strace syscall summaries, and perf stat comparisons.
# Runs inside the Arch Linux benchmark container (requires --privileged).
#
# Usage: profile_hotpaths.sh [--scenarios SCENARIOS] [--skip-flamegraph]
#
# Output is written to /results/ (mount a volume to extract).

set -euo pipefail

# Binaries
UPSTREAM="/usr/local/bin/upstream-rsync"
OC_RSYNC="/usr/local/bin/oc-rsync-dev"
OC_V058="/usr/local/bin/oc-rsync-v058"

# Configuration
RESULTS_DIR="/results"
BENCH_DIR="/tmp/rsync-profile"
SRC_DIR="$BENCH_DIR/src"
FLAMEGRAPH_FREQ=999
SKIP_FLAMEGRAPH=false
SKIP_SSH=false

SCENARIOS="initial,no_change,incremental"
COPY_MODES="delta:'-av',whole_file:'-avW',checksum:'-avc',compressed:'-avz'"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --scenarios)        SCENARIOS="$2"; shift 2 ;;
        --skip-flamegraph)  SKIP_FLAMEGRAPH=true; shift ;;
        --skip-ssh)         SKIP_SSH=true; shift ;;
        -h|--help)
            echo "Usage: $0 [--scenarios initial,no_change,incremental] [--skip-flamegraph] [--skip-ssh]"
            exit 0
            ;;
        *)  echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

mkdir -p "$RESULTS_DIR"/{flamegraphs,strace,perf_stat}
mkdir -p "$BENCH_DIR"

# ---------------------------------------------------------------------------
# Test data generation (10,000 files, ~290 MB)
# ---------------------------------------------------------------------------

generate_test_data() {
    log "Generating test data (10,000 files, ~290 MB)..."
    rm -rf "$SRC_DIR"
    mkdir -p "$SRC_DIR"/{small,medium,large}

    # 9,000 x 1KB
    for i in $(seq 0 8999); do
        dd if=/dev/urandom of="$SRC_DIR/small/file_$(printf '%05d' "$i").txt" \
            bs=1024 count=1 2>/dev/null
    done

    # 800 x 100KB
    for i in $(seq 0 799); do
        dd if=/dev/urandom of="$SRC_DIR/medium/file_$(printf '%04d' "$i").bin" \
            bs=102400 count=1 2>/dev/null
    done

    # 200 x 1MB
    for i in $(seq 0 199); do
        dd if=/dev/urandom of="$SRC_DIR/large/file_$(printf '%04d' "$i").dat" \
            bs=1048576 count=1 2>/dev/null
    done

    local total_size total_files
    total_size=$(du -sb "$SRC_DIR" | cut -f1)
    total_files=$(find "$SRC_DIR" -type f | wc -l)
    log "Test data: $(( total_size / 1024 / 1024 )) MB, $total_files files"
}

# ---------------------------------------------------------------------------
# Daemon setup
# ---------------------------------------------------------------------------

DAEMON_PID=""
DAEMON_PORT=""

start_daemon() {
    DAEMON_PORT=$(python3 -c "import socket; s=socket.socket(); s.bind(('',0)); print(s.getsockname()[1]); s.close()")
    local daemon_dst="$BENCH_DIR/daemon_dest"
    mkdir -p "$daemon_dst"

    cat > "$BENCH_DIR/rsyncd.conf" <<CONF
port = $DAEMON_PORT
use chroot = false

[bench]
    path = $SRC_DIR
    read only = true

[dest]
    path = $daemon_dst
    read only = false
CONF

    "$UPSTREAM" --daemon --config="$BENCH_DIR/rsyncd.conf" --no-detach &
    DAEMON_PID=$!
    sleep 1

    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        log "ERROR: daemon failed to start"
        return 1
    fi
    log "rsync daemon started on port $DAEMON_PORT (pid $DAEMON_PID)"
}

stop_daemon() {
    if [[ -n "${DAEMON_PID:-}" ]]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
}

trap stop_daemon EXIT

# ---------------------------------------------------------------------------
# Profiling functions
# ---------------------------------------------------------------------------

clean_dst() {
    rm -rf "$BENCH_DIR/dst"
    mkdir -p "$BENCH_DIR/dst"
}

modify_src() {
    log "Modifying 10% of source files..."
    local count=0
    find "$SRC_DIR" -type f | head -1000 | while IFS= read -r f; do
        dd if=/dev/urandom of="$f" bs=64 count=1 seek=$(( $(stat -c%s "$f" 2>/dev/null || stat -f%z "$f") / 2 / 64 )) conv=notrunc 2>/dev/null || true
        count=$((count + 1))
    done
}

run_strace_profile() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/strace/${name}_${scenario}.txt"

    log "  strace -f -c: $name ($scenario)"
    strace -f -c -o "$output" -- \
        "$binary" -a "$SRC_DIR/" "$dst/" 2>/dev/null || true
}

run_strace_daemon() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/strace/${name}_daemon_${scenario}.txt"

    log "  strace -f -c (daemon): $name ($scenario)"
    strace -f -c -o "$output" -- \
        "$binary" -a "rsync://localhost:$DAEMON_PORT/bench/" "$dst/" 2>/dev/null || true
}

run_perf_stat() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/perf_stat/${name}_${scenario}.txt"

    log "  perf stat: $name ($scenario)"
    perf stat -d -o "$output" -- \
        "$binary" -a "$SRC_DIR/" "$dst/" 2>/dev/null || true
}

run_perf_stat_daemon() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/perf_stat/${name}_daemon_${scenario}.txt"

    log "  perf stat (daemon): $name ($scenario)"
    perf stat -d -o "$output" -- \
        "$binary" -a "rsync://localhost:$DAEMON_PORT/bench/" "$dst/" 2>/dev/null || true
}

run_flamegraph() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/flamegraphs/${name}_${scenario}.svg"

    log "  flamegraph: $name ($scenario)"
    flamegraph -o "$output" --freq "$FLAMEGRAPH_FREQ" -- \
        "$binary" -a "$SRC_DIR/" "$dst/" 2>/dev/null || true
    if [[ -f "$output" ]]; then
        log "    saved: $output"
    fi
}

run_flamegraph_daemon() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/flamegraphs/${name}_daemon_${scenario}.svg"

    log "  flamegraph (daemon): $name ($scenario)"
    flamegraph -o "$output" --freq "$FLAMEGRAPH_FREQ" -- \
        "$binary" -a "rsync://localhost:$DAEMON_PORT/bench/" "$dst/" 2>/dev/null || true
    if [[ -f "$output" ]]; then
        log "    saved: $output"
    fi
}

run_strace_ssh_pull() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/strace/${name}_ssh_pull_${scenario}.txt"

    log "  strace -f -c (ssh pull): $name ($scenario)"
    strace -f -c -o "$output" -- \
        "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "localhost:$SRC_DIR/" "$dst/" 2>/dev/null || true
}

run_strace_ssh_push() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/strace/${name}_ssh_push_${scenario}.txt"

    log "  strace -f -c (ssh push): $name ($scenario)"
    strace -f -c -o "$output" -- \
        "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "$SRC_DIR/" "localhost:$dst/" 2>/dev/null || true
}

run_perf_stat_ssh_pull() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/perf_stat/${name}_ssh_pull_${scenario}.txt"

    log "  perf stat (ssh pull): $name ($scenario)"
    perf stat -d -o "$output" -- \
        "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "localhost:$SRC_DIR/" "$dst/" 2>/dev/null || true
}

run_perf_stat_ssh_push() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/perf_stat/${name}_ssh_push_${scenario}.txt"

    log "  perf stat (ssh push): $name ($scenario)"
    perf stat -d -o "$output" -- \
        "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "$SRC_DIR/" "localhost:$dst/" 2>/dev/null || true
}

run_flamegraph_ssh_pull() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/flamegraphs/${name}_ssh_pull_${scenario}.svg"

    log "  flamegraph (ssh pull): $name ($scenario)"
    flamegraph -o "$output" --freq "$FLAMEGRAPH_FREQ" -- \
        "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "localhost:$SRC_DIR/" "$dst/" 2>/dev/null || true
    if [[ -f "$output" ]]; then
        log "    saved: $output"
    fi
}

run_flamegraph_ssh_push() {
    local binary="$1"
    local name="$2"
    local scenario="$3"
    local dst="$BENCH_DIR/dst"
    local output="$RESULTS_DIR/flamegraphs/${name}_ssh_push_${scenario}.svg"

    log "  flamegraph (ssh push): $name ($scenario)"
    flamegraph -o "$output" --freq "$FLAMEGRAPH_FREQ" -- \
        "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "$SRC_DIR/" "localhost:$dst/" 2>/dev/null || true
    if [[ -f "$output" ]]; then
        log "    saved: $output"
    fi
}

# ---------------------------------------------------------------------------
# Run profiling for a scenario
# ---------------------------------------------------------------------------

profile_scenario() {
    local scenario="$1"

    log "=== Profiling scenario: $scenario ==="

    case "$scenario" in
        initial)
            ;;
        no_change)
            ;;
        incremental)
            modify_src
            ;;
    esac

    local binaries=("$UPSTREAM" "$OC_V058" "$OC_RSYNC")
    local names=("upstream" "v058" "dev")

    for i in "${!binaries[@]}"; do
        local binary="${binaries[$i]}"
        local name="${names[$i]}"

        if [[ ! -x "$binary" ]]; then
            log "  SKIP $name: binary not found"
            continue
        fi

        # Local copy profiling
        clean_dst
        if [[ "$scenario" == "no_change" ]]; then
            "$binary" -a "$SRC_DIR/" "$BENCH_DIR/dst/" 2>/dev/null || true
        fi
        run_strace_profile "$binary" "$name" "$scenario"

        clean_dst
        if [[ "$scenario" == "no_change" ]]; then
            "$binary" -a "$SRC_DIR/" "$BENCH_DIR/dst/" 2>/dev/null || true
        fi
        run_perf_stat "$binary" "$name" "$scenario"

        if ! $SKIP_FLAMEGRAPH; then
            clean_dst
            if [[ "$scenario" == "no_change" ]]; then
                "$binary" -a "$SRC_DIR/" "$BENCH_DIR/dst/" 2>/dev/null || true
            fi
            run_flamegraph "$binary" "$name" "$scenario"
        fi

        # Daemon profiling
        if [[ -n "${DAEMON_PORT:-}" ]]; then
            clean_dst
            if [[ "$scenario" == "no_change" ]]; then
                "$binary" -a "rsync://localhost:$DAEMON_PORT/bench/" "$BENCH_DIR/dst/" 2>/dev/null || true
            fi
            run_strace_daemon "$binary" "$name" "$scenario"

            clean_dst
            if [[ "$scenario" == "no_change" ]]; then
                "$binary" -a "rsync://localhost:$DAEMON_PORT/bench/" "$BENCH_DIR/dst/" 2>/dev/null || true
            fi
            run_perf_stat_daemon "$binary" "$name" "$scenario"

            if ! $SKIP_FLAMEGRAPH; then
                clean_dst
                if [[ "$scenario" == "no_change" ]]; then
                    "$binary" -a "rsync://localhost:$DAEMON_PORT/bench/" "$BENCH_DIR/dst/" 2>/dev/null || true
                fi
                run_flamegraph_daemon "$binary" "$name" "$scenario"
            fi
        fi

        # SSH profiling
        if ! $SKIP_SSH; then
            # SSH Pull
            clean_dst
            if [[ "$scenario" == "no_change" ]]; then
                "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "localhost:$SRC_DIR/" "$BENCH_DIR/dst/" 2>/dev/null || true
            fi
            run_strace_ssh_pull "$binary" "$name" "$scenario"

            clean_dst
            if [[ "$scenario" == "no_change" ]]; then
                "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "localhost:$SRC_DIR/" "$BENCH_DIR/dst/" 2>/dev/null || true
            fi
            run_perf_stat_ssh_pull "$binary" "$name" "$scenario"

            if ! $SKIP_FLAMEGRAPH; then
                clean_dst
                if [[ "$scenario" == "no_change" ]]; then
                    "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "localhost:$SRC_DIR/" "$BENCH_DIR/dst/" 2>/dev/null || true
                fi
                run_flamegraph_ssh_pull "$binary" "$name" "$scenario"
            fi

            # SSH Push
            clean_dst
            if [[ "$scenario" == "no_change" ]]; then
                "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "$SRC_DIR/" "localhost:$BENCH_DIR/dst/" 2>/dev/null || true
            fi
            run_strace_ssh_push "$binary" "$name" "$scenario"

            clean_dst
            if [[ "$scenario" == "no_change" ]]; then
                "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "$SRC_DIR/" "localhost:$BENCH_DIR/dst/" 2>/dev/null || true
            fi
            run_perf_stat_ssh_push "$binary" "$name" "$scenario"

            if ! $SKIP_FLAMEGRAPH; then
                clean_dst
                if [[ "$scenario" == "no_change" ]]; then
                    "$binary" -a -e 'ssh -o StrictHostKeyChecking=no' "$SRC_DIR/" "localhost:$BENCH_DIR/dst/" 2>/dev/null || true
                fi
                run_flamegraph_ssh_push "$binary" "$name" "$scenario"
            fi
        fi
    done
}

# ---------------------------------------------------------------------------
# Syscall comparison report
# ---------------------------------------------------------------------------

generate_comparison() {
    local report="$RESULTS_DIR/syscall_comparison.txt"
    log "Generating syscall comparison report..."

    {
        echo "================================================================"
        echo "  Syscall Comparison: upstream rsync vs oc-rsync"
        echo "================================================================"
        echo ""
        echo "Date: $(date)"
        echo "Upstream: $($UPSTREAM --version | head -1)"
        echo "oc-rsync dev: $($OC_RSYNC --version | head -1)"
        if [[ -x "$OC_V058" ]]; then
            echo "oc-rsync v0.5.8: $($OC_V058 --version | head -1)"
        fi
        echo ""

        IFS=',' read -ra scenario_list <<< "$SCENARIOS"
        for scenario in "${scenario_list[@]}"; do
            echo "--- Scenario: $scenario ---"
            echo ""

            for mode in "" "daemon_" "ssh_pull_" "ssh_push_"; do
                local label
                case "$mode" in
                    "")           label="Local Copy" ;;
                    "daemon_")    label="Daemon Pull" ;;
                    "ssh_pull_")  label="SSH Pull" ;;
                    "ssh_push_")  label="SSH Push" ;;
                esac
                echo "  [$label]"

                for name in upstream v058 dev; do
                    local file="$RESULTS_DIR/strace/${name}_${mode}${scenario}.txt"
                    if [[ -f "$file" ]]; then
                        echo "    $name:"
                        grep -E '^\s+[0-9]' "$file" | head -15 | sed 's/^/      /'
                        echo ""
                    fi
                done
            done
        done

        echo ""
        echo "--- perf stat summaries ---"
        echo ""
        for f in "$RESULTS_DIR"/perf_stat/*.txt; do
            if [[ -f "$f" ]]; then
                echo "  $(basename "$f" .txt):"
                grep -E '(instructions|cycles|cache-misses|branch-misses|task-clock)' "$f" | sed 's/^/    /' || true
                echo ""
            fi
        done

    } > "$report"

    log "Comparison report saved to $report"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    log "=== oc-rsync Hot Path Profiling ==="
    log "Scenarios: $SCENARIOS"
    log ""

    # Check binaries
    for bin in "$UPSTREAM" "$OC_RSYNC"; do
        if [[ ! -x "$bin" ]]; then
            log "ERROR: $bin not found"
            exit 1
        fi
    done

    # Check profiling tools
    for tool in strace perf; do
        if ! command -v "$tool" &>/dev/null; then
            log "WARNING: $tool not found, some profiles will be skipped"
        fi
    done

    if ! $SKIP_FLAMEGRAPH && ! command -v flamegraph &>/dev/null; then
        log "WARNING: flamegraph not found, skipping flamegraph generation"
        SKIP_FLAMEGRAPH=true
    fi

    generate_test_data

    # Setup SSH loopback for SSH profiling
    if ! $SKIP_SSH; then
        log "Setting up SSH loopback..."
        if ! ssh -o StrictHostKeyChecking=no -o BatchMode=yes localhost echo ok 2>/dev/null; then
            log "WARNING: SSH loopback failed, skipping SSH profiles"
            SKIP_SSH=true
        else
            log "SSH loopback OK"
        fi
    fi

    start_daemon

    IFS=',' read -ra scenario_list <<< "$SCENARIOS"
    for scenario in "${scenario_list[@]}"; do
        profile_scenario "$scenario"
    done

    generate_comparison

    log ""
    log "=== Profiling complete ==="
    log "Results directory: $RESULTS_DIR/"
    log ""
    log "Flamegraphs: $RESULTS_DIR/flamegraphs/"
    log "Strace:      $RESULTS_DIR/strace/"
    log "Perf stat:   $RESULTS_DIR/perf_stat/"
    log "Comparison:  $RESULTS_DIR/syscall_comparison.txt"
}

main "$@"
