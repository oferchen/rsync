#!/usr/bin/env bash
# Full vs incremental flist memory benchmark over a daemon push.
#
# Companion to scripts/benchmark_flist_memory.sh, which measures local
# (sender-side) RSS. This script targets the daemon push path so the
# receiver/generator process tree is the one under measurement. That is
# the regime upstream's INC_RECURSE optimisation is designed for: the
# receiver streams file lists segment-by-segment instead of holding the
# full list in core.
#
# Scope:
#
#   Dataset A  100K empty files (100 dirs x 1000 files)
#   Dataset B  1M empty files   (1000 dirs x 1000 files)
#
#   Mode A  --no-inc-recursive  (full flist, pre-protocol-30 behaviour)
#   Mode B  default             (receiver INC_RECURSE)
#
# For each (dataset, mode) the script records peak RSS via /usr/bin/time
# -v of the oc-rsync client process pushing into a temporary rsync daemon
# spawned by this script. The daemon is torn down on EXIT. Bytes-per-file
# overhead = peak_rss_bytes / file_count is reported in the markdown
# table.
#
# Inode budget: before any dataset is generated the script runs `df -i`
# against BENCH_ROOT and skips datasets whose inode count would exceed
# 80 % of the filesystem's free inodes.
#
# Refs: #1864 (this script's task), #966 (RSS gap), #971 (1M-file RSS).
#
# Usage:
#   scripts/benchmark_flist_memory_daemon.sh [--scales 100k|1m|both]
#                                            [--runs N] [--summary]
#                                            [--port P]
#
# Inside the rsync-profile container:
#   podman exec rsync-profile bash \
#       /workspace/scripts/benchmark_flist_memory_daemon.sh --summary
#
# Environment variables:
#   OC_RSYNC    Path to oc-rsync binary. Default: /usr/local/bin/oc-rsync-dev
#               inside the container, else target/release/oc-rsync, else
#               PATH lookup.
#   BENCH_ROOT  Where fixtures, daemon module and PID/conf live. Default
#               /tmp/oc-rsync-bench-daemon. MUST resolve under /tmp or
#               /var/tmp; the cleanup helper refuses paths outside that.
#   RUNS        Iterations per mode (default 3, median reported).
#   DAEMON_PORT Starting port for the temp daemon (default 28730).
#
# Operator script, not a CI gate. Wall time can reach 30 min at the 1M
# scale; the per-PR signal lives in protocol's file_entry_memory bench.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BENCH_ROOT="${BENCH_ROOT:-/tmp/oc-rsync-bench-daemon}"
RUNS="${RUNS:-3}"
DAEMON_PORT="${DAEMON_PORT:-28730}"
SCALES_ARG="both"
EMIT_SUMMARY=0

default_oc_rsync() {
    if [[ -x /usr/local/bin/oc-rsync-dev ]]; then
        echo /usr/local/bin/oc-rsync-dev
    elif [[ -x "${PROJECT_ROOT}/target/release/oc-rsync" ]]; then
        echo "${PROJECT_ROOT}/target/release/oc-rsync"
    else
        echo oc-rsync
    fi
}

OC_RSYNC="${OC_RSYNC:-$(default_oc_rsync)}"

usage() {
    sed -n '1,52p' "$0"
}

while (( $# > 0 )); do
    case "$1" in
        --scales)  SCALES_ARG="${2:-}"; shift 2 ;;
        --runs)    RUNS="${2:-}";       shift 2 ;;
        --port)    DAEMON_PORT="${2:-}"; shift 2 ;;
        --summary) EMIT_SUMMARY=1;       shift   ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$SCALES_ARG" in
    100k) SCALES=("100k") ;;
    1m)   SCALES=("1m") ;;
    both) SCALES=("100k" "1m") ;;
    *) echo "ERROR: --scales must be 100k, 1m, or both (got: $SCALES_ARG)" >&2; exit 2 ;;
esac

# /usr/bin/time -v on Linux exposes "Maximum resident set size (kbytes)".
# macOS uses /usr/bin/time -l which reports bytes.
detect_time_cmd() {
    if [[ "$(uname)" == "Darwin" ]]; then
        TIME_CMD=(/usr/bin/time -l)
        RSS_PARSE=parse_rss_macos
    elif [[ -x /usr/bin/time ]]; then
        TIME_CMD=(/usr/bin/time -v)
        RSS_PARSE=parse_rss_linux
    else
        echo "ERROR: /usr/bin/time not found. Cannot measure peak RSS." >&2
        exit 1
    fi
}

parse_rss_linux() {
    grep "Maximum resident set size" "$1" | awk '{print $NF}'
}

parse_rss_macos() {
    grep "maximum resident set size" "$1" | awk '{print int($1 / 1024)}'
}

format_rss_mb() {
    awk -v kb="$1" 'BEGIN { printf "%.1f", kb/1024 }'
}

format_bytes_per_file() {
    local kb="$1" files="$2"
    if [[ -z "$files" || "$files" -eq 0 || "$kb" == "0" ]]; then
        echo "n/a"
        return
    fi
    awk -v kb="$kb" -v n="$files" 'BEGIN { printf "%.1f", (kb * 1024) / n }'
}

check_prereqs() {
    if [[ ! -x "$OC_RSYNC" ]] && ! command -v "$OC_RSYNC" >/dev/null 2>&1; then
        echo "ERROR: oc-rsync not found at: $OC_RSYNC" >&2
        exit 1
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        echo "ERROR: python3 required for fixture generation" >&2
        exit 1
    fi
    case "$BENCH_ROOT" in
        /tmp/*|/var/tmp/*) ;;
        *)
            echo "ERROR: BENCH_ROOT must live under /tmp or /var/tmp (got: $BENCH_ROOT)" >&2
            echo "       Refusing to operate to prevent rm -rf accidents on bind mounts." >&2
            exit 1 ;;
    esac
    mkdir -p "$BENCH_ROOT"
}

# Path-guarded rm. Refuses to operate outside BENCH_ROOT.
safe_rm_under_root() {
    local path="$1"
    case "$path" in
        "$BENCH_ROOT"/*) rm -rf -- "$path" ;;
        *) echo "REFUSING to rm path outside BENCH_ROOT: $path" >&2; return 1 ;;
    esac
}

# Inode budget check. Returns 0 if at least <needed> inodes are free and
# we would consume less than 80 % of what is available. Returns 1 to
# signal skip otherwise.
#
# df -i layout differs by platform: Linux GNU coreutils prints "IFree" as
# column 4 of the data row; macOS BSD prints "ifree" as column 7. To
# stay portable we resolve the free-inode column from the header line by
# name match (case-insensitive). When df is missing or unparseable the
# check assumes the budget is OK and only warns.
inode_budget_ok() {
    local needed="$1"
    local target_dir="$2"
    local raw header data free col
    if ! raw=$(df -iP "$target_dir" 2>/dev/null) && ! raw=$(df -i "$target_dir" 2>/dev/null); then
        echo "WARN: df -i failed for $target_dir; assuming budget OK" >&2
        return 0
    fi
    header=$(echo "$raw" | awk 'NR==1')
    data=$(echo "$raw" | awk 'NR==2')
    col=$(echo "$header" | awk '{ for (i=1;i<=NF;i++) if (tolower($i) ~ /^ifree$|^iavail$/) { print i; exit } }')
    if [[ -n "$col" ]]; then
        free=$(echo "$data" | awk -v c="$col" '{print $c}')
    else
        # Fallback to GNU coreutils layout (field 4).
        free=$(echo "$data" | awk '{print $4}')
    fi
    if [[ -z "$free" || ! "$free" =~ ^[0-9]+$ ]]; then
        echo "WARN: could not parse free inodes from df -i $target_dir; assuming OK" >&2
        return 0
    fi
    # Source tree + dest tree + ~2 % slack for daemon temp files etc.
    local need_doubled=$(( needed * 2 + needed / 50 + 16 ))
    local ceiling=$(( free * 80 / 100 ))
    if (( need_doubled > ceiling )); then
        echo "SKIP: dataset needs ~${need_doubled} inodes; only ${ceiling}"\
             "(80 % of ${free} free) available under ${target_dir}" >&2
        return 1
    fi
    echo "  inode budget OK: need ~${need_doubled}, ceiling ${ceiling}, free ${free}"
    return 0
}

# Generate NUM_DIRS x FILES_PER_DIR empty files. Empty files keep focus on
# file-list memory (sender flist serialisation, receiver flist parse) and
# not on transfer bytes.
generate_fixture() {
    local root="$1" num_dirs="$2" files_per_dir="$3"
    local total=$((num_dirs * files_per_dir))

    if [[ -f "$root/.fixture-ok" ]]; then
        local existing
        existing=$(cat "$root/.fixture-ok" 2>/dev/null || echo 0)
        if [[ "$existing" == "$total" ]]; then
            echo "  Reusing existing fixture: $root ($total files)"
            return 0
        fi
    fi

    safe_rm_under_root "$root" || true
    mkdir -p "$root"

    echo "  Generating $total files ($num_dirs dirs x $files_per_dir) at $root"
    local start end
    start=$(date +%s)
    python3 - "$root" "$num_dirs" "$files_per_dir" <<'PY'
import os, pathlib, sys
root, num_dirs, files_per_dir = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
root_p = pathlib.Path(root)
for d in range(num_dirs):
    dpath = root_p / f"dir_{d:05d}"
    dpath.mkdir(parents=True, exist_ok=True)
    for f in range(files_per_dir):
        os.close(os.open(str(dpath / f"file_{f:05d}.dat"),
                         os.O_WRONLY | os.O_CREAT, 0o644))
PY
    end=$(date +%s)
    echo "$total" > "$root/.fixture-ok"
    echo "    Done in $((end - start))s"
}

# Daemon lifecycle ---------------------------------------------------------

DAEMON_PID=""
DAEMON_CONF=""
DAEMON_LOG=""
DAEMON_PIDFILE=""
DAEMON_DEST=""

start_daemon() {
    DAEMON_DEST="$BENCH_ROOT/daemon-module"
    DAEMON_CONF="$BENCH_ROOT/oc-rsyncd.conf"
    DAEMON_LOG="$BENCH_ROOT/oc-rsyncd.log"
    DAEMON_PIDFILE="$BENCH_ROOT/oc-rsyncd.pid"

    safe_rm_under_root "$DAEMON_DEST" || true
    mkdir -p "$DAEMON_DEST"
    : > "$DAEMON_LOG"
    rm -f "$DAEMON_PIDFILE"

    cat > "$DAEMON_CONF" <<CONF
pid file = $DAEMON_PIDFILE
port = $DAEMON_PORT
use chroot = false
numeric ids = yes

[bench]
    path = $DAEMON_DEST
    comment = flist memory benchmark target
    read only = false
    write only = false
CONF

    # OC_RSYNC_DAEMON_FALLBACK=0 forces native handling rather than
    # delegating to the system rsync binary. We need oc-rsync's own
    # receiver path under measurement.
    OC_RSYNC_DAEMON_FALLBACK=0 "$OC_RSYNC" --daemon --config "$DAEMON_CONF" \
        --port "$DAEMON_PORT" --log-file "$DAEMON_LOG" &
    DAEMON_PID=$!

    # Wait up to 5 s for the daemon to accept TCP. A bare module listing
    # via the oc-rsync client doubles as a readiness probe.
    local i
    for i in $(seq 1 25); do
        if "$OC_RSYNC" "rsync://127.0.0.1:${DAEMON_PORT}/" >/dev/null 2>&1; then
            echo "  daemon ready on port $DAEMON_PORT (pid $DAEMON_PID)"
            return 0
        fi
        sleep 0.2
    done

    echo "ERROR: daemon failed to start; tail of log:" >&2
    tail -n 40 "$DAEMON_LOG" >&2 || true
    return 1
}

stop_daemon() {
    if [[ -n "$DAEMON_PID" ]]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
        DAEMON_PID=""
    fi
    if [[ -n "$DAEMON_PIDFILE" && -f "$DAEMON_PIDFILE" ]]; then
        rm -f "$DAEMON_PIDFILE"
    fi
}

cleanup() {
    stop_daemon
}
trap cleanup EXIT INT TERM

# Run one push and capture peak RSS + wall time.
run_one() {
    local src="$1"; shift
    local extra_args=("$@")

    # Clear daemon dest so every run sees the same starting state. Empty
    # files mean the receiver still has to allocate per-entry state.
    safe_rm_under_root "$DAEMON_DEST" || true
    mkdir -p "$DAEMON_DEST"

    local stderr_file
    stderr_file=$(mktemp)

    local start end wall rss
    start=$(date +%s.%N)
    "${TIME_CMD[@]}" "$OC_RSYNC" -a "${extra_args[@]}" \
        "$src/" "rsync://127.0.0.1:${DAEMON_PORT}/bench/" \
        >/dev/null 2>"$stderr_file" || true
    end=$(date +%s.%N)

    wall=$(awk -v s="$start" -v e="$end" 'BEGIN { printf "%.3f", e - s }')
    rss=$($RSS_PARSE "$stderr_file")
    rm -f "$stderr_file"
    [[ -z "$rss" ]] && rss=0
    echo "$wall $rss"
}

median_run() {
    local label="$1"; shift
    local src="$1"; shift
    local -a walls=() rsses=()
    local i result
    for ((i=0; i<RUNS; i++)); do
        result=$(run_one "$src" "$@")
        walls+=("$(echo "$result" | awk '{print $1}')")
        rsses+=("$(echo "$result" | awk '{print $2}')")
        echo "    [$((i+1))/$RUNS] $label  wall=${walls[$i]}s  rss=$(format_rss_mb "${rsses[$i]}")MB" >&2
    done
    local sorted_w sorted_r mid
    sorted_w=$(printf '%s\n' "${walls[@]}" | sort -n)
    sorted_r=$(printf '%s\n' "${rsses[@]}" | sort -n)
    mid=$(( (RUNS + 1) / 2 ))
    echo "$(echo "$sorted_w" | sed -n "${mid}p") $(echo "$sorted_r" | sed -n "${mid}p")"
}

# Result arrays kept in parallel to avoid bash assoc-array sort headaches.
declare -a RES_MODE=() RES_SCALE=() RES_FILES=() RES_WALL=() RES_RSS=()

record() {
    RES_MODE+=("$1"); RES_SCALE+=("$2"); RES_FILES+=("$3")
    RES_WALL+=("$4"); RES_RSS+=("$5")
}

write_tsv() {
    local out="$1"
    {
        printf "mode\tscale\tfiles\twall_s\tpeak_rss_mb\tbytes_per_file\n"
        local i rss_mb bpf
        for ((i=0; i<${#RES_MODE[@]}; i++)); do
            if [[ "${RES_RSS[$i]}" == "0" ]]; then
                rss_mb="n/a"; bpf="n/a"
            else
                rss_mb=$(format_rss_mb "${RES_RSS[$i]}")
                bpf=$(format_bytes_per_file "${RES_RSS[$i]}" "${RES_FILES[$i]}")
            fi
            printf "%s\t%s\t%s\t%s\t%s\t%s\n" \
                "${RES_MODE[$i]}" "${RES_SCALE[$i]}" "${RES_FILES[$i]}" \
                "${RES_WALL[$i]}" "$rss_mb" "$bpf"
        done
    } > "$out"
}

emit_markdown_summary() {
    echo ""
    echo "## Daemon-push flist memory benchmark"
    echo ""
    echo "Client process measured with \`/usr/bin/time -v\`; daemon RSS not"
    echo "included. Bytes/file = peak_rss / file_count (client side)."
    echo ""
    echo "| Mode | Scale | Files | Wall (s) | Peak RSS (MB) | Bytes/file |"
    echo "|------|-------|------:|---------:|--------------:|-----------:|"
    local i rss_mb bpf
    for ((i=0; i<${#RES_MODE[@]}; i++)); do
        if [[ "${RES_RSS[$i]}" == "0" ]]; then
            rss_mb="n/a"; bpf="n/a"
        else
            rss_mb=$(format_rss_mb "${RES_RSS[$i]}")
            bpf=$(format_bytes_per_file "${RES_RSS[$i]}" "${RES_FILES[$i]}")
        fi
        echo "| ${RES_MODE[$i]} | ${RES_SCALE[$i]} | ${RES_FILES[$i]} | ${RES_WALL[$i]} | $rss_mb | $bpf |"
    done
    echo ""
    echo "Mode A = full flist (\`--no-inc-recursive\`)."
    echo "Mode B = default (receiver INC_RECURSE)."
}

run_scale() {
    local scale="$1" num_dirs="$2" files_per_dir="$3" total="$4"
    local src="$BENCH_ROOT/src-$scale"

    echo "=== Scale $scale ($total files = $num_dirs dirs x $files_per_dir) ==="

    if ! inode_budget_ok "$total" "$BENCH_ROOT"; then
        echo "  Skipping scale $scale (insufficient inode budget)" >&2
        return 0
    fi

    generate_fixture "$src" "$num_dirs" "$files_per_dir"
    echo ""

    echo "  Mode A (full flist, --no-inc-recursive):"
    local r_a; r_a=$(median_run "$scale-A" "$src" --no-inc-recursive)
    record "A_full_flist" "$scale" "$total" \
        "$(echo "$r_a" | awk '{print $1}')" "$(echo "$r_a" | awk '{print $2}')"

    echo "  Mode B (default, receiver INC_RECURSE):"
    local r_b; r_b=$(median_run "$scale-B" "$src")
    record "B_default" "$scale" "$total" \
        "$(echo "$r_b" | awk '{print $1}')" "$(echo "$r_b" | awk '{print $2}')"
    echo ""
}

main() {
    detect_time_cmd
    check_prereqs

    echo "================================================"
    echo "  Daemon-push flist memory benchmark"
    echo "================================================"
    echo "  oc-rsync:    $OC_RSYNC"
    echo "  BENCH_ROOT:  $BENCH_ROOT"
    echo "  Scales:      ${SCALES[*]}"
    echo "  Runs:        $RUNS"
    echo "  Daemon port: $DAEMON_PORT"
    echo ""
    "$OC_RSYNC" --version 2>/dev/null | head -1 || true
    echo ""

    start_daemon

    local scale num_dirs files_per_dir total
    for scale in "${SCALES[@]}"; do
        case "$scale" in
            100k) num_dirs=100;  files_per_dir=1000; total=100000  ;;
            1m)   num_dirs=1000; files_per_dir=1000; total=1000000 ;;
        esac
        run_scale "$scale" "$num_dirs" "$files_per_dir" "$total"
    done

    local timestamp out_dir tsv
    timestamp=$(date +%Y%m%d-%H%M%S)
    out_dir="${PROJECT_ROOT}/target/benchmarks"
    mkdir -p "$out_dir"
    tsv="${out_dir}/flist_memory_daemon_${timestamp}.tsv"

    write_tsv "$tsv"
    echo "Wrote: $tsv"

    if (( EMIT_SUMMARY )); then
        emit_markdown_summary | tee "${tsv%.tsv}.md"
    fi

    echo "Done."
}

main "$@"
