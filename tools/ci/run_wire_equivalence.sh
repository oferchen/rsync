#!/usr/bin/env sh
# run_wire_equivalence.sh - drive the tcpdump wire-equivalence harness.
#
# This wrapper is safe to call unconditionally from CI:
#   - On Linux with tcpdump, tshark, and both rsync binaries it runs the
#     full capture-and-diff cycle.
#   - On macOS, Windows, or hosts missing prerequisites it skips with a
#     clear message and exit code 0 so it never blocks a pipeline.
#
# Set WIRE_EQUIV_REQUIRED=1 to turn skips into hard failures (used by the
# nightly interop matrix once the container image ships tcpdump/tshark).

set -eu

workspace_root=$(cd "$(dirname "$0")/../.." && pwd)
script="${workspace_root}/scripts/wire-equivalence-tcpdump.sh"

if [ ! -f "${script}" ]; then
    printf 'wire-equivalence script missing: %s\n' "${script}" >&2
    exit 2
fi

uname_s=$(uname -s 2>/dev/null || echo unknown)
if [ "${uname_s}" != "Linux" ]; then
    printf '[run_wire_equivalence] skipped: tcpdump capture path is Linux-only (saw %s)\n' \
        "${uname_s}"
    [ "${WIRE_EQUIV_REQUIRED:-0}" = "1" ] && exit 2
    exit 0
fi

missing=""
for tool in tcpdump tshark sha256sum; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        missing="${missing} ${tool}"
    fi
done
if [ -n "${missing}" ]; then
    printf '[run_wire_equivalence] skipped: missing tool(s):%s\n' "${missing}"
    [ "${WIRE_EQUIV_REQUIRED:-0}" = "1" ] && exit 2
    exit 0
fi

if [ -z "${UPSTREAM_RSYNC_BIN:-}" ] && ! command -v rsync >/dev/null 2>&1; then
    printf '[run_wire_equivalence] skipped: no upstream rsync on PATH\n'
    [ "${WIRE_EQUIV_REQUIRED:-0}" = "1" ] && exit 2
    exit 0
fi

if [ -z "${OC_RSYNC_BIN:-}" ] && ! command -v oc-rsync >/dev/null 2>&1; then
    candidate="${workspace_root}/target/release/oc-rsync"
    if [ ! -x "${candidate}" ]; then
        printf '[run_wire_equivalence] skipped: oc-rsync binary not built (looked for %s)\n' \
            "${candidate}"
        [ "${WIRE_EQUIV_REQUIRED:-0}" = "1" ] && exit 2
        exit 0
    fi
    OC_RSYNC_BIN="${candidate}"
    export OC_RSYNC_BIN
fi

printf '[run_wire_equivalence] running %s\n' "${script}"
# Use sh explicitly so the executable bit is not required.
exec sh "${script}" "$@"
