#!/usr/bin/env bash
# SSH push smoke regression bench (SSR-5).
#
# Times oc-rsync and upstream rsync doing identical SSH pushes at three
# file sizes and asserts that oc-rsync wall-clock stays within a fixed
# multiple of upstream. Designed to catch regressions like v0.6.1's
# subprocess SSH goodbye-phase deadlock (~200x slower) before release.
#
# Inputs (env):
#   OC_RSYNC          - path to the oc-rsync binary under test (required)
#   UPSTREAM_RSYNC    - path to upstream rsync (default: rsync on PATH)
#   HYPERFINE_RUNS    - hyperfine --runs value (default: 5)
#   HYPERFINE_WARMUP  - hyperfine --warmup value (default: 1)
#   SSH_TARGET        - "user@host" for the push target (default: $USER@localhost)
#   RESULTS_DIR       - where JSON results land (default: ./ssh-smoke-results)
#   RATIO_LIMIT       - max oc-rsync / upstream ratio per size (default: 1.5)
#
# Exit 0 on success, non-zero if any size exceeds RATIO_LIMIT or a
# transfer fails.

set -euo pipefail

OC_RSYNC="${OC_RSYNC:?OC_RSYNC must point at the oc-rsync binary}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-rsync}"
HYPERFINE_RUNS="${HYPERFINE_RUNS:-5}"
HYPERFINE_WARMUP="${HYPERFINE_WARMUP:-1}"
SSH_TARGET="${SSH_TARGET:-${USER}@localhost}"
RESULTS_DIR="${RESULTS_DIR:-ssh-smoke-results}"
RATIO_LIMIT="${RATIO_LIMIT:-1.5}"

command -v hyperfine >/dev/null || {
  echo "::error::hyperfine not found on PATH" >&2
  exit 2
}
command -v jq >/dev/null || {
  echo "::error::jq not found on PATH" >&2
  exit 2
}
[ -x "$OC_RSYNC" ] || {
  echo "::error::OC_RSYNC=$OC_RSYNC is not executable" >&2
  exit 2
}
command -v "$UPSTREAM_RSYNC" >/dev/null || {
  echo "::error::UPSTREAM_RSYNC=$UPSTREAM_RSYNC not found" >&2
  exit 2
}

mkdir -p "$RESULTS_DIR"

# Pre-flight: confirm SSH loopback works without a password prompt.
ssh -o BatchMode=yes -o StrictHostKeyChecking=no "$SSH_TARGET" true \
  || { echo "::error::ssh to $SSH_TARGET failed (need passwordless loopback)"; exit 2; }

# Sizes tested. Format: "label:bytes:destination_suffix".
SIZES=(
  "1KB:1024:size-1kb"
  "1MB:1048576:size-1mb"
  "100MB:104857600:size-100mb"
)

WORK_ROOT="$(mktemp -d -t ssh-smoke-bench-XXXXXX)"
trap 'rm -rf "$WORK_ROOT"' EXIT

declare -a SUMMARY_ROWS=()
FAILED=0

run_one_size() {
  local label="$1" bytes="$2" dst_suffix="$3"
  local src="$WORK_ROOT/src-$dst_suffix/payload.bin"
  local dst_oc="$WORK_ROOT/dst-oc-$dst_suffix"
  local dst_up="$WORK_ROOT/dst-up-$dst_suffix"
  local json="$RESULTS_DIR/ssh-push-${label}.json"

  mkdir -p "$(dirname "$src")" "$dst_oc" "$dst_up"
  # Reproducible payload from /dev/urandom; same file for both runs.
  head -c "$bytes" /dev/urandom > "$src"

  echo "::group::SSH push smoke: $label ($bytes bytes)"

  # Hyperfine resets the destination before each run so size + mtime
  # match-skip never triggers. The --prepare hook wipes the per-size
  # dst directory; the command itself does the push.
  hyperfine \
    --runs "$HYPERFINE_RUNS" \
    --warmup "$HYPERFINE_WARMUP" \
    --export-json "$json" \
    --command-name "oc-rsync-$label" \
    --prepare "rm -rf '$dst_oc' && mkdir -p '$dst_oc'" \
    "'$OC_RSYNC' -a '$src' '$SSH_TARGET:$dst_oc/'" \
    --command-name "upstream-$label" \
    --prepare "rm -rf '$dst_up' && mkdir -p '$dst_up'" \
    "'$UPSTREAM_RSYNC' -a '$src' '$SSH_TARGET:$dst_up/'"

  local oc_mean up_mean ratio status
  oc_mean=$(jq -r '.results[0].mean' "$json")
  up_mean=$(jq -r '.results[1].mean' "$json")
  ratio=$(awk -v a="$oc_mean" -v b="$up_mean" 'BEGIN { if (b == 0) print "inf"; else printf "%.3f", a / b }')
  status="ok"
  if awk -v r="$ratio" -v limit="$RATIO_LIMIT" 'BEGIN { exit !(r > limit) }'; then
    status="FAIL"
    FAILED=1
    echo "::error::$label: oc-rsync ${oc_mean}s vs upstream ${up_mean}s -> ratio ${ratio} exceeds ${RATIO_LIMIT}"
  fi

  SUMMARY_ROWS+=("$label|$oc_mean|$up_mean|$ratio|$RATIO_LIMIT|$status")
  echo "::endgroup::"
}

for entry in "${SIZES[@]}"; do
  IFS=':' read -r label bytes suffix <<<"$entry"
  run_one_size "$label" "$bytes" "$suffix"
done

# Markdown summary for GitHub step summary (and human readers).
{
  echo "## SSH push smoke bench (SSR-5)"
  echo
  echo "Ratio limit per size: \`${RATIO_LIMIT}x\` upstream."
  echo
  echo "| Size | oc-rsync (s) | upstream (s) | ratio | limit | status |"
  echo "|------|--------------|--------------|-------|-------|--------|"
  for row in "${SUMMARY_ROWS[@]}"; do
    IFS='|' read -r label oc up ratio limit status <<<"$row"
    echo "| $label | $oc | $up | ${ratio}x | ${limit}x | $status |"
  done
} | tee "$RESULTS_DIR/summary.md"

if [ "$FAILED" -ne 0 ]; then
  echo "::error::One or more SSH push sizes exceeded the ${RATIO_LIMIT}x upstream limit"
  exit 1
fi
