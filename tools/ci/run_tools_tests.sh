#!/usr/bin/env bash
# Run the unittest modules under tools/tests/.
#
# Discovery is by pattern rather than an enumerated list. tools/tests/
# accumulated three modules that no workflow ran, and an enumerated list is
# precisely how a fourth would join them.
#
# The test count is asserted rather than inferred from the exit status. On
# Python 3.12+ a run that collects nothing exits 5 ("NO TESTS RAN") and the
# status check below catches it; on earlier versions it exits 0, which is
# indistinguishable from a passing run. The count assertion is what makes this
# script correct on both, and it does not depend on which Python the runner
# happens to ship.
#
# The modules import their subjects as `tools.<module>`, so the repository root
# has to be importable; the top-level directory is the test directory itself
# because tools/ is a namespace package and `discover` cannot import it as a
# start directory.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

output=$(PYTHONPATH=. python3 -m unittest discover \
	--start-directory tools/tests \
	--top-level-directory tools/tests \
	--pattern 'test_*.py' 2>&1) && status=0 || status=$?

# Some subjects under test print GitHub Actions workflow commands (`::error`)
# on their failure paths, and asserting on those paths is the point of the
# tests. Echoed raw, a passing run would decorate itself with red annotations,
# so workflow-command parsing is suspended around the output.
if [ -n "${GITHUB_ACTIONS:-}" ]; then
	token="tools-tests-$RANDOM$RANDOM"
	echo "::stop-commands::$token"
	printf '%s\n' "$output"
	echo "::$token::"
else
	printf '%s\n' "$output"
fi

if [ "$status" -ne 0 ]; then
	exit "$status"
fi

if ! grep -qE '^Ran [1-9][0-9]* tests?' <<<"$output"; then
	echo "error: unittest discover matched no tests under tools/tests/" >&2
	echo "A pattern that matches nothing exits 0 and reads as success." >&2
	exit 1
fi
