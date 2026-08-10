#!/usr/bin/env bash
# MDF-8 - diff upstream rsync --debug=FILTER1,2,3,4 against oc-rsync filter output
#
# Runs upstream rsync and oc-rsync against the MDF-7 complex fixture with
# the maximal `--debug=FILTER1,2,3,4` switch, captures the filter-decision
# log lines on each side, normalises them, and diffs the two streams.
#
# Exit codes:
#   0  - logs are identical after normalisation (parity).
#   1  - logs diverge after normalisation (regression).
#   2  - usage / argument error.
#   77 - skipped: required binary or fixture not available.
#
# Defaults are tuned for local development; CI overrides via flags.
set -euo pipefail

UPSTREAM="/usr/bin/rsync"
OC_RSYNC="target/release/oc-rsync"
FIXTURE="tests/fixtures/filter-rules/mdf-7-complex/source"
OUTPUT="/tmp/mdf-8-diff"
STRICT=0
DELETE_EXCLUDED=0

usage() {
    cat >&2 <<'EOF'
Usage: mdf_8_filter_diff_harness.sh [options]

Options:
  --upstream PATH      Path to upstream rsync binary (default: /usr/bin/rsync)
  --oc-rsync PATH      Path to oc-rsync binary       (default: target/release/oc-rsync)
  --fixture PATH       Source fixture directory      (default: tests/fixtures/filter-rules/mdf-7-complex/source)
  --output DIR         Output directory for logs     (default: /tmp/mdf-8-diff)
  --strict             Compare level-2+ FILTER traces as well (future work).
  --delete-excluded    Run both sides with --delete --delete-excluded so the diff
                       harness also catches UTS-DD-exclude.1/.4/.5 regressions
                       (see docs/design/fil-aud-3-mdf-gap-tests-spec.md section 2.7).
  -h, --help           Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --upstream)        UPSTREAM="$2"; shift 2 ;;
        --oc-rsync)        OC_RSYNC="$2"; shift 2 ;;
        --fixture)         FIXTURE="$2"; shift 2 ;;
        --output)          OUTPUT="$2"; shift 2 ;;
        --strict)          STRICT=1; shift ;;
        --delete-excluded) DELETE_EXCLUDED=1; shift ;;
        -h|--help)         usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
    esac
done

if [[ ! -x "$UPSTREAM" ]] && ! command -v "$UPSTREAM" >/dev/null 2>&1; then
    echo "skip: upstream rsync not found at '$UPSTREAM'" >&2
    exit 77
fi
if [[ ! -x "$OC_RSYNC" ]] && ! command -v "$OC_RSYNC" >/dev/null 2>&1; then
    echo "skip: oc-rsync not found at '$OC_RSYNC'" >&2
    exit 77
fi
if [[ ! -d "$FIXTURE" ]]; then
    echo "skip: fixture not found at '$FIXTURE'" >&2
    exit 77
fi

mkdir -p "$OUTPUT"
UPSTREAM_DEST="/tmp/mdf-8/upstream-dest"
OC_DEST="/tmp/mdf-8/oc-rsync-dest"
mkdir -p "$UPSTREAM_DEST" "$OC_DEST"

# Strip a known prefix from a stream so absolute and per-CI workspace
# paths normalise to a stable relative form. We strip:
#   - the fixture absolute path
#   - the destination root prefixes
#   - any leading "[role pid]" tag (e.g. "[receiver=3.4.1]")
#   - terminal colour escapes
normalise() {
    local fixture_abs="$1"
    sed \
        -e "s|$fixture_abs/||g" \
        -e "s|$fixture_abs||g" \
        -e "s|/tmp/mdf-8/upstream-dest/||g" \
        -e "s|/tmp/mdf-8/oc-rsync-dest/||g" \
        -e 's/\x1b\[[0-9;]*m//g' \
        -e 's/^\[[a-z]*=[^]]*\] //' \
        | tr '[:upper:]' '[:lower:]' \
        | LC_ALL=C sort -u
}

FIXTURE_ABS="$(cd "$FIXTURE" && pwd)"

EXTRA_FLAGS=()
if [[ "$DELETE_EXCLUDED" -eq 1 ]]; then
    EXTRA_FLAGS+=(--delete --delete-excluded)
fi

set +e
"$UPSTREAM" -av --debug=FILTER1,2,3,4 --dry-run "${EXTRA_FLAGS[@]}" \
    "$FIXTURE_ABS/" "$UPSTREAM_DEST/" 2>&1 \
    | grep '\[FILTER' > "$OUTPUT/upstream-filter.log"
UPSTREAM_RC=$?

"$OC_RSYNC" -av --debug=FILTER1,2,3,4 --dry-run "${EXTRA_FLAGS[@]}" \
    "$FIXTURE_ABS/" "$OC_DEST/" 2>&1 \
    | grep -E '\[Filter\]|\[FILTER' > "$OUTPUT/oc-rsync-filter.log"
OC_RC=$?
set -e

# grep returns 1 when no matches; in this harness "no matches" is signal
# (level-2+ unimplemented), not failure. Capture rc but do not abort.
if [[ "$UPSTREAM_RC" -gt 1 ]]; then
    echo "warning: upstream rsync grep exited with $UPSTREAM_RC" >&2
fi
if [[ "$OC_RC" -gt 1 ]]; then
    echo "warning: oc-rsync grep exited with $OC_RC" >&2
fi

normalise "$FIXTURE_ABS" < "$OUTPUT/upstream-filter.log" > "$OUTPUT/upstream-filter-normalised.log"
normalise "$FIXTURE_ABS" < "$OUTPUT/oc-rsync-filter.log" > "$OUTPUT/oc-rsync-filter-normalised.log"

if [[ "$STRICT" -eq 0 ]]; then
    # Restrict comparison to level-1-shaped lines ("excluding" / "including"
    # decisions). Level-2 rule-load echoes and level-3/4 traces are
    # documented divergences (see docs/user/filter-rules-status.md).
    for f in "$OUTPUT/upstream-filter-normalised.log" "$OUTPUT/oc-rsync-filter-normalised.log"; do
        grep -E 'excluding|including' "$f" > "$f.tmp" || true
        mv "$f.tmp" "$f"
    done
fi

diff -u "$OUTPUT/upstream-filter-normalised.log" "$OUTPUT/oc-rsync-filter-normalised.log" \
    > "$OUTPUT/diff.txt" || true

DIFF_LINES="$(wc -l < "$OUTPUT/diff.txt" | tr -d ' ')"

UPSTREAM_LOG_LINES="$(wc -l < "$OUTPUT/upstream-filter.log" | tr -d ' ')"
OC_LOG_LINES="$(wc -l < "$OUTPUT/oc-rsync-filter.log" | tr -d ' ')"

echo "MDF-8 filter diff harness"
echo "  fixture           : $FIXTURE_ABS"
echo "  upstream rsync    : $UPSTREAM"
echo "  oc-rsync          : $OC_RSYNC"
echo "  output dir        : $OUTPUT"
echo "  strict mode       : $STRICT"
echo "  delete-excluded   : $DELETE_EXCLUDED"
echo "  upstream log lines: $UPSTREAM_LOG_LINES"
echo "  oc-rsync log lines: $OC_LOG_LINES"
echo "  diff lines        : $DIFF_LINES"

# JSON summary so FIL-AUD-5 close-out automation can grep for fixture presence
# and run-shape (delete_excluded, strict). Emitted unconditionally.
if [[ "$DIFF_LINES" -eq 0 ]]; then
    SUMMARY_STATUS="pass"
else
    SUMMARY_STATUS="fail"
fi
DELETE_EXCLUDED_JSON=$([[ "$DELETE_EXCLUDED" -eq 1 ]] && echo true || echo false)
STRICT_JSON=$([[ "$STRICT" -eq 1 ]] && echo true || echo false)
cat > "$OUTPUT/summary.json" <<EOF
{
  "fixture": "$FIXTURE_ABS",
  "delete_excluded": $DELETE_EXCLUDED_JSON,
  "strict": $STRICT_JSON,
  "upstream_log_lines": $UPSTREAM_LOG_LINES,
  "oc_rsync_log_lines": $OC_LOG_LINES,
  "diff_lines": $DIFF_LINES,
  "status": "$SUMMARY_STATUS"
}
EOF

if [[ "$DIFF_LINES" -eq 0 ]]; then
    echo "PASS: filter-decision streams match after normalisation."
    exit 0
fi
echo "FAIL: $DIFF_LINES diff lines - see $OUTPUT/diff.txt"
exit 1
