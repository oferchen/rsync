#!/usr/bin/env bash
# RSS-1.b / RSS-1.c: Peak RSS measurement for oc-rsync vs upstream rsync.
#
# Measures peak resident set size at scale across multiple modes:
#   - Actual push (-a): real transfer, flist fully materialized
#   - Dry-run (-an): no transfer, lighter flist path
#   - --no-inc-recursive: full flist held in memory (no streaming)
#
# Usage (inside rsync-profile container):
#   bash /workspace/scripts/rss_measure.sh [RUNS]
#
# Prerequisites:
#   - /tmp/rss_1m populated with 1M files (run rss_gen_files.c)
#   - /usr/bin/rsync (upstream 3.4.1)
#   - /workspace/target/release/oc-rsync built
#
# Refs: RSS-1.b (#2917), RSS-1.c (#2918), #966 (RSS gap).
set -euo pipefail

RUNS="${1:-3}"
TREE="/tmp/rss_1m"

if [ ! -d "$TREE" ]; then
    echo "ERROR: $TREE not found. Create it first:" >&2
    echo "  gcc -O2 -o /tmp/rss_gen_files /workspace/scripts/rss_gen_files.c" >&2
    echo "  /tmp/rss_gen_files" >&2
    exit 1
fi

FILE_COUNT=$(find "$TREE" -type f | wc -l)
UPSTREAM_VER=$(/usr/bin/rsync --version 2>&1 | head -1 || true)
OC_VER=$(/workspace/target/release/oc-rsync --version 2>&1 | head -1 || true)

echo "=== RSS Benchmark ==="
echo "Tree: $TREE ($FILE_COUNT files)"
echo "Upstream: $UPSTREAM_VER"
echo "oc-rsync: $OC_VER"
echo "Runs: $RUNS"
echo ""

extract_rss() {
    grep "Maximum resident set size" | awk '{print $NF}'
}

run_measure() {
    local label="$1"
    local binary="$2"
    shift 2
    local flags=("$@")

    local results=()
    for run in $(seq 1 "$RUNS"); do
        rm -rf /tmp/rss_dest
        rss=$( { /usr/bin/time -v "$binary" "${flags[@]}" "$TREE/" /tmp/rss_dest/; } 2>&1 | extract_rss )
        echo "  Run $run: ${rss} KB ($((rss / 1024)) MB)"
        results+=("$rss")
    done

    local sorted=($(printf '%s\n' "${results[@]}" | sort -n))
    local mid=$(( RUNS / 2 ))
    local median=${sorted[$mid]}
    local lo=${sorted[0]}
    local hi=${sorted[$((RUNS - 1))]}
    local spread
    spread=$(awk "BEGIN { printf \"%.1f\", ($hi - $lo) / $median * 100 }")

    echo "  Median: ${median} KB ($((median / 1024)) MB), range: ${lo}-${hi} KB, spread: ${spread}%"
    echo ""
}

# Baseline (empty dir)
echo "=== Baseline (empty directory) ==="
rm -rf /tmp/rss_empty /tmp/rss_dest
mkdir -p /tmp/rss_empty

echo "Upstream:"
rss=$( { /usr/bin/time -v /usr/bin/rsync -an /tmp/rss_empty/ /tmp/rss_dest/; } 2>&1 | extract_rss )
echo "  RSS: ${rss} KB ($((rss / 1024)) MB)"

echo "oc-rsync:"
rm -rf /tmp/rss_dest
rss=$( { /usr/bin/time -v /workspace/target/release/oc-rsync -an /tmp/rss_empty/ /tmp/rss_dest/; } 2>&1 | extract_rss )
echo "  RSS: ${rss} KB ($((rss / 1024)) MB)"
echo ""

# Actual push - default (INC_RECURSE)
echo "=== Actual push (-a, default INC_RECURSE) ==="
echo "Upstream rsync:"
run_measure "upstream-push" /usr/bin/rsync -a

echo "oc-rsync:"
run_measure "oc-rsync-push" /workspace/target/release/oc-rsync -a

# Actual push - no-inc-recursive
echo "=== Actual push (-a, --no-inc-recursive) ==="
echo "Upstream rsync:"
run_measure "upstream-push-noinc" /usr/bin/rsync -a --no-inc-recursive

echo "oc-rsync:"
run_measure "oc-rsync-push-noinc" /workspace/target/release/oc-rsync -a --no-inc-recursive

# Dry-run - default
echo "=== Dry-run (-an, default INC_RECURSE) ==="
echo "Upstream rsync:"
run_measure "upstream-dry" /usr/bin/rsync -an

echo "oc-rsync:"
run_measure "oc-rsync-dry" /workspace/target/release/oc-rsync -an

rm -rf /tmp/rss_empty /tmp/rss_dest
echo "Done."
