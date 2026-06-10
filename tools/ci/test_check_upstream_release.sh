#!/usr/bin/env bash
# test_check_upstream_release.sh - unit tests for check_upstream_release.sh.
#
# Exercises the script against mocked NEWS files and Cargo.toml inputs and
# asserts the exit codes for equal, ahead, and error cases.

set -uo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
target="${script_dir}/check_upstream_release.sh"

tmp=$(mktemp -d)
trap 'rm -rf "${tmp}"' EXIT

failures=0

write_news() {
    cat >"${tmp}/NEWS" <<EOF
# NEWS for rsync $1 (placeholder date)

Released on placeholder date.

# NEWS for 3.4.1 (14 Jan 2025)
EOF
}

write_cargo() {
    cat >"${tmp}/Cargo.toml" <<EOF
[workspace.metadata.oc_rsync]
upstream_version = "$1"
rust_version = "0.0.0"
protocol = 32
EOF
}

run_case() {
    local label="$1" expected="$2" news="$3"
    NEWS_URL="${news}" CARGO_TOML="${tmp}/Cargo.toml" \
        bash "${target}" >"${tmp}/out" 2>"${tmp}/err"
    local actual=$?
    if [[ "${actual}" -ne "${expected}" ]]; then
        echo "FAIL ${label}: expected ${expected}, got ${actual}" >&2
        sed 's/^/  out: /' "${tmp}/out" >&2
        sed 's/^/  err: /' "${tmp}/err" >&2
        failures=$((failures + 1))
    else
        echo "ok ${label} (exit ${actual})"
    fi
}

write_news "3.4.4"; write_cargo "3.4.4"
run_case "equal versions returns 0" 0 "${tmp}/NEWS"

write_news "3.5.0"; write_cargo "3.4.4"
run_case "upstream ahead returns 1" 1 "${tmp}/NEWS"

write_cargo "3.4.4"
run_case "missing NEWS returns 2" 2 "${tmp}/missing-NEWS"

if [[ "${failures}" -ne 0 ]]; then
    echo "${failures} test(s) failed" >&2
    exit 1
fi
echo "all tests passed"
