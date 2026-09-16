#!/usr/bin/env bash
# Surface nextest FLAKY results - tests that passed only after a retry - so a
# retried pass is counted instead of scrolling away in the run log. Reads a
# captured nextest log and, when running under GitHub Actions, appends the
# flaky list to the step summary.
#
# Usage: nextest_flaky_summary.sh <nextest-log>
#
# `.config/nextest.toml` sets `final-status-level = "flaky"`, which makes
# nextest print exactly one line per flaky test after the summary, e.g.
#   FLAKY 2/3 [   0.011s] daemon::negotiation accepts_valid_credentials
# Counting those lines therefore counts flaky tests, not retry attempts.
#
# Always exits 0 when the log exists: a flaky test is a signal to investigate,
# not a gate - the retry policy in .config/nextest.toml already decides which
# tests may retry at all.
set -euo pipefail

if [ "$#" -ne 1 ] || [ ! -f "$1" ]; then
  echo "usage: $0 <nextest-log>" >&2
  exit 2
fi
log="$1"

flaky_lines="$(grep -E '^[[:space:]]*FLAKY [0-9]+/[0-9]+' "$log" || true)"
if [ -z "$flaky_lines" ]; then
  echo "flaky tests: 0"
  exit 0
fi

count="$(printf '%s\n' "$flaky_lines" | wc -l | tr -d '[:space:]')"
echo "flaky tests: $count"
printf '%s\n' "$flaky_lines"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Flaky tests: $count passed only after retry"
    echo '```'
    printf '%s\n' "$flaky_lines"
    echo '```'
  } >>"$GITHUB_STEP_SUMMARY"
fi
