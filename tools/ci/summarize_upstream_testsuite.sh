#!/usr/bin/env bash
# Render an upstream-testsuite run into $GITHUB_STEP_SUMMARY.
#
# Usage: summarize_upstream_testsuite.sh <output-log> <heading>
#
# WHY A SCRIPT AND NOT AN INLINE `run:` BLOCK
# ---------------------------------------------------------------------------
# This renderer is identical for every leg - only the log path and the heading
# differ. It was inline YAML, so each new leg meant another ~60-line copy, and
# the copies are already diverging in the Linux workflow. A leg that renders a
# DIFFERENT summary from its sibling is a reporting defect that no gate can
# see, because the summary is not an assertion.
#
# The exit status is deliberately 0 on a missing log: this runs under
# `if: always()`, after a step that may itself have failed, and the job's
# conclusion must stay the testsuite's, never the summariser's.

set -euo pipefail

output_log=${1:?usage: summarize_upstream_testsuite.sh <output-log> <heading>}
heading=${2:?usage: summarize_upstream_testsuite.sh <output-log> <heading>}

summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}

{
    echo "## ${heading}"
    echo ""
} >>"$summary"

if [[ ! -f "$output_log" ]]; then
    echo "No test output found." >>"$summary"
    exit 0
fi

{
    echo '```'
    sed -n '/^----/,$ p' "$output_log"
    echo '```'
    echo ""
} >>"$summary"

# An unexpected PASS is called out ahead of the failures on purpose: it is the
# outcome most likely to be misread as good news. It means the committed
# manifest no longer describes the tree, and the fix is to regenerate from the
# emitted artifact - never to hand-edit the row.
upass_lines=$(grep '^UPASS' "$output_log" || true)
if [[ -n "$upass_lines" ]]; then
    {
        echo "### Unexpected Passes (UPASS)"
        echo ""
        echo "A cell the manifest expects to fail now passes. Re-baseline the manifest from this run's emitted artifact - do NOT edit it by hand."
        echo ""
        echo '```'
        echo "$upass_lines"
        echo '```'
        echo ""
    } >>"$summary"
fi

fail_lines=$(grep '^FAIL' "$output_log" || true)
if [[ -n "$fail_lines" ]]; then
    {
        echo "### Failures"
        echo ""
        echo '```'
        echo "$fail_lines"
        echo '```'
        echo ""
    } >>"$summary"
fi

{
    echo "<details><summary>Full test results</summary>"
    echo ""
    echo "| Status | Test |"
    echo "|--------|------|"
    # `|| true` on the grep, not on the loop: with no matching rows grep exits
    # 1 and `set -e` would abort the script before the closing </details>,
    # leaving the summary with an unterminated block.
    grep -E '^(PASS|FAIL|XFAIL|UPASS|SKIP)' "$output_log" 2>/dev/null | awk '{print "| " $1 " | " $2 " |"}' || true
    echo ""
    echo "</details>"
} >>"$summary"
