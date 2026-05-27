#!/usr/bin/env bash
# Peak RSS measurement for flist allocation at scale.
#
# Wraps the criterion benchmark with /usr/bin/time to capture peak RSS -
# something criterion benchmarks cannot measure because criterion reuses
# the process across iterations.
#
# Usage:
#   scripts/benchmark_rss_flist.sh [bench_filter]
#
# Examples:
#   scripts/benchmark_rss_flist.sh                          # all RSS benchmarks
#   scripts/benchmark_rss_flist.sh rss_memory_profile       # 1M allocation only
#   scripts/benchmark_rss_flist.sh rss_flat/1000000         # flat 1M only
#
# Inside a container:
#   podman exec rsync-profile bash /workspace/scripts/benchmark_rss_flist.sh
#
# Refs: RSS-1.a (million-file fixture), #966 (RSS gap), #971 (1M-file RSS).

set -euo pipefail

FILTER="${1:-rss_}"

# Detect /usr/bin/time variant.
if [[ "$(uname)" == "Darwin" ]]; then
    TIME_CMD=(/usr/bin/time -l)
elif [[ -x /usr/bin/time ]]; then
    TIME_CMD=(/usr/bin/time -v)
else
    echo "ERROR: /usr/bin/time not found" >&2
    exit 1
fi

echo "=== flist RSS benchmark ==="
echo "Filter: $FILTER"
echo ""

stderr_file=$(mktemp)

"${TIME_CMD[@]}" cargo bench \
    -p protocol \
    --bench flist_rss_fixture \
    -- "$FILTER" \
    --sample-size 10 \
    2>"$stderr_file" || true

echo ""
echo "=== Peak RSS ==="
if [[ "$(uname)" == "Darwin" ]]; then
    grep "maximum resident set size" "$stderr_file" || true
else
    grep "Maximum resident set size" "$stderr_file" || true
fi

# Forward all stderr (criterion output + RSS)
cat "$stderr_file" >&2
rm -f "$stderr_file"
