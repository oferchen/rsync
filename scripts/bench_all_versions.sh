#!/bin/bash
set -uo pipefail

RUNS=5

benchmark() {
    local label="$1" cmd="$2" setup="${3:-}"
    local all_times=""
    for i in $(seq 1 $RUNS); do
        if [ -n "$setup" ]; then eval "$setup" 2>/dev/null; fi
        local start_ns=$(date +%s%N)
        eval "$cmd" > /dev/null 2>&1 || true
        local end_ns=$(date +%s%N)
        local elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
        all_times="$all_times $elapsed_ms"
    done
    local avg=$(echo "$all_times" | tr ' ' '\n' | grep -v '^$' | awk '{s+=$1;n++} END{printf "%.2f", s/n/1000}')
    local runs_sec=$(echo "$all_times" | tr ' ' '\n' | grep -v '^$' | awk '{printf " %.2f", $1/1000}')
    printf "  %-20s %ss (runs:%s)\n" "$label" "$avg" "$runs_sec"
}

echo "================================================================"
echo "  COMPREHENSIVE BENCHMARK - ALL VERSIONS"
echo "  10K files, 420MB, aarch64 loopback (Debian container)"
echo "================================================================"
echo ""
echo "NOTE: SSH transfers only work with oc-rsync HEAD (c782d40a)"
echo "  because older versions lack -e.LsfxCIvu capability string."
echo "  Older versions fail silently on SSH and are excluded."
echo ""

echo "============== SSH TRANSFERS (HEAD vs upstream) ================"
echo ""

for mode in "SSH PULL" "SSH PUSH"; do
    for scenario in "Initial" "No-change"; do
        echo "=== $mode - $scenario ==="

        if [ "$scenario" = "Initial" ]; then
            setup="rm -rf /tmp/bench/dst && mkdir -p /tmp/bench/dst"
        else
            setup=""
        fi

        case "$mode" in
            "SSH PULL") src="localhost:/tmp/bench/src/"; dst="/tmp/bench/dst/" ;;
            "SSH PUSH") src="/tmp/bench/src/"; dst="localhost:/tmp/bench/dst/" ;;
        esac

        benchmark "upstream-rsync" "rsync -a $src $dst" "$setup"
        benchmark "oc-rsync-HEAD" "oc-rsync-dev -a $src $dst" "$setup"
        echo ""
    done
done

echo "============== DAEMON TRANSFERS (all versions) ================"
echo ""

for scenario in "Initial" "No-change"; do
    echo "=== DAEMON PULL - $scenario ==="

    if [ "$scenario" = "Initial" ]; then
        setup="rm -rf /tmp/bench/dst && mkdir -p /tmp/bench/dst"
    else
        setup=""
    fi

    benchmark "upstream-rsync" "rsync -a rsync://localhost:18730/bench/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-v0.5.4" "oc-rsync-v054 -a rsync://localhost:18730/bench/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-v0.5.5" "oc-rsync-v055 -a rsync://localhost:18730/bench/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-v0.5.7" "oc-rsync-v057 -a rsync://localhost:18730/bench/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-HEAD" "oc-rsync-dev -a rsync://localhost:18730/bench/ /tmp/bench/dst/" "$setup"
    echo ""
done

echo "============== LOCAL COPY (all versions) ======================"
echo ""

for scenario in "Initial" "No-change"; do
    echo "=== LOCAL COPY - $scenario ==="

    if [ "$scenario" = "Initial" ]; then
        setup="rm -rf /tmp/bench/dst && mkdir -p /tmp/bench/dst"
    else
        setup=""
    fi

    benchmark "upstream-rsync" "rsync -a /tmp/bench/src/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-v0.5.4" "oc-rsync-v054 -a /tmp/bench/src/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-v0.5.5" "oc-rsync-v055 -a /tmp/bench/src/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-v0.5.7" "oc-rsync-v057 -a /tmp/bench/src/ /tmp/bench/dst/" "$setup"
    benchmark "oc-rsync-HEAD" "oc-rsync-dev -a /tmp/bench/src/ /tmp/bench/dst/" "$setup"
    echo ""
done
