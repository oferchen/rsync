#!/usr/bin/env bash
# RP28.f.2 - run client-mode rsync 2.6.9 daemon interop harness
#
# Orchestrates the F1-F12 fixture matrix from
# `docs/design/rp28-f-1-client-2-6-9-daemon-harness.md`. The topology is
# the inverse of RP28.e: rsync 2.6.9 runs as `--daemon --no-detach` on an
# ephemeral 127.0.0.1 port and oc-rsync drives pulls and pushes against
# the `[legacy]` module.
#
# Usage:
#   scripts/rp28_f_1_run.sh \
#     [--oc-rsync target/release/oc-rsync] \
#     [--rsync-2-6-9 /usr/local/bin/rsync-2.6.9]
#
# Exit codes:
#   0  - all 12 fixtures passed
#   1  - one or more fixtures failed
#   77 - either required binary missing (treat as skip)
#
# Task: RP28.f.2 (#2967). Parent: RP28.f (#2731). Spec: RP28.f.1 (#2966).

set -euo pipefail

OC_RSYNC="target/release/oc-rsync"
RSYNC_269="/usr/local/bin/rsync-2.6.9"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --oc-rsync)
      OC_RSYNC="$2"
      shift 2
      ;;
    --rsync-2-6-9)
      RSYNC_269="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '2,20p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ ! -x "${OC_RSYNC}" ]]; then
  echo "RP28.f.2 skip: oc-rsync binary not found at ${OC_RSYNC}" >&2
  exit 77
fi
if [[ ! -x "${RSYNC_269}" ]]; then
  echo "RP28.f.2 skip: rsync 2.6.9 binary not found at ${RSYNC_269}" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${RP28_F_ROOT:-/tmp/rp28-f}"
export RP28_F_ROOT="${ROOT}"

DAEMON_SHARE="${ROOT}/daemon-share"
CLIENT_SRC="${ROOT}/client-src"
CLIENT_DEST="${ROOT}/client-dest"
DAEMON_CONF="${ROOT}/rsyncd-2-6-9.conf"
DAEMON_PID_FILE="/tmp/rsyncd-2-6-9.pid"
DAEMON_LOG="${ROOT}/daemon.log"
PORT_FILE="${ROOT}/port.txt"

DAEMON_PID=""

cleanup() {
  local rc=$?
  if [[ -n "${DAEMON_PID}" ]] && kill -0 "${DAEMON_PID}" 2>/dev/null; then
    kill -TERM "${DAEMON_PID}" 2>/dev/null || true
    for _ in $(seq 1 10); do
      kill -0 "${DAEMON_PID}" 2>/dev/null || break
      sleep 0.5
    done
    kill -KILL "${DAEMON_PID}" 2>/dev/null || true
  fi
  rm -f "${DAEMON_PID_FILE}"
  if [[ ${rc} -ne 0 && -f "${DAEMON_LOG}" ]]; then
    echo "--- rsync 2.6.9 daemon log (tail) ---" >&2
    tail -n 200 "${DAEMON_LOG}" >&2 || true
  fi
  # Leave ROOT in place on failure for CI artifact capture; remove on success.
  if [[ ${rc} -eq 0 ]]; then
    rm -rf "${ROOT}"
  fi
}
trap cleanup EXIT
# Run the cleanup path (which stops the daemon) when an outer wall-clock guard
# (`timeout` in CI) sends SIGTERM/SIGINT. A wedged legacy proto-29 peer can stall
# a fixture; the outer guard bounds total runtime and this trap ensures the
# background daemon is still reaped. Exit 124 mirrors coreutils `timeout`.
trap 'exit 124' TERM INT

echo "RP28.f.2: oc-rsync=$(${OC_RSYNC} --version 2>&1 | head -1)"
echo "RP28.f.2: rsync-2.6.9=$(${RSYNC_269} --version 2>&1 | head -1)"

bash "${SCRIPT_DIR}/rp28_f_1_setup.sh"

# Ephemeral port. Bind to verify availability then release immediately;
# brief reuse race vs. the daemon start is the same trade-off RP28.e.2
# accepts and is tolerated by the bind-poll loop below.
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
echo "${PORT}" > "${PORT_FILE}"
echo "RP28.f.2: daemon port ${PORT}"

"${RSYNC_269}" --daemon --no-detach --config "${DAEMON_CONF}" --port "${PORT}" \
  > "${DAEMON_LOG}" 2>&1 &
DAEMON_PID=$!

# Wait up to 10s for the daemon to bind.
for _ in $(seq 1 20); do
  if (echo >/dev/tcp/127.0.0.1/"${PORT}") 2>/dev/null; then
    break
  fi
  if ! kill -0 "${DAEMON_PID}" 2>/dev/null; then
    echo "FATAL: rsync 2.6.9 daemon exited before binding port ${PORT}" >&2
    exit 1
  fi
  sleep 0.5
done
if ! (echo >/dev/tcp/127.0.0.1/"${PORT}") 2>/dev/null; then
  echo "FATAL: rsync 2.6.9 daemon failed to bind port ${PORT} within 10s" >&2
  exit 1
fi
echo "RP28.f.2: daemon bound 127.0.0.1:${PORT}"

declare -a RESULTS
PASS_COUNT=0
FAIL_COUNT=0

record() {
  local name="$1" status="$2" detail="${3:-}"
  RESULTS+=("${status} ${name} ${detail}")
  if [[ "${status}" == "PASS" ]]; then
    PASS_COUNT=$((PASS_COUNT + 1))
  else
    FAIL_COUNT=$((FAIL_COUNT + 1))
  fi
}

run_client() {
  # Runs oc-rsync as the client against the 2.6.9 daemon with a 60s
  # wall-clock guard and returns its exit status. Stderr is captured
  # per-fixture into the named file so the caller can inspect it.
  local stderr_path="$1"
  shift
  set +e
  timeout 60 "${OC_RSYNC}" "$@" 2> "${stderr_path}"
  local rc=$?
  set -e
  return ${rc}
}

reset_client_dest() {
  local sub="$1"
  rm -rf "${CLIENT_DEST}/${sub}"
  mkdir -p "${CLIENT_DEST}/${sub}"
}

reset_daemon_share_sub() {
  local sub="$1"
  rm -rf "${DAEMON_SHARE}/${sub}"
  mkdir -p "${DAEMON_SHARE}/${sub}"
}

verify_diff() {
  # Byte-identical tree compare. `--no-dereference` keeps symlinks compared
  # by target string rather than followed.
  local left="$1" right="$2"
  diff -r --no-dereference "${left}" "${right}"
}

check_daemon_quiet() {
  # Fail if the 2.6.9 daemon log shows hard errors. The legacy daemon
  # emits routine `rsync: connection from ...` notices; only `rsync error`
  # and protocol-fatal lines should fail the fixture.
  local label="$1"
  local quiet=1
  if grep -E "rsync error:" "${DAEMON_LOG}" >/dev/null 2>&1; then
    echo "FAIL ${label}: daemon log contains rsync error: line" >&2
    quiet=0
  fi
  if grep -E "protocol mismatch|unexpected EOF" "${DAEMON_LOG}" >/dev/null 2>&1; then
    echo "FAIL ${label}: daemon log contains protocol fatal" >&2
    quiet=0
  fi
  [[ ${quiet} -eq 1 ]]
}

check_client_quiet() {
  # Per spec section 5: client stderr must have no panics, no `error:`
  # log lines, and no `WARNING` other than expected protocol-downgrade
  # messages.
  local label="$1" stderr_path="$2"
  local quiet=1
  if [[ ! -f "${stderr_path}" ]]; then
    return 0
  fi
  if grep -E "panicked at|thread .* panicked" "${stderr_path}" >/dev/null 2>&1; then
    echo "FAIL ${label}: client stderr contains panic" >&2
    quiet=0
  fi
  if grep -E "^error:" "${stderr_path}" >/dev/null 2>&1; then
    echo "FAIL ${label}: client stderr contains error: line" >&2
    quiet=0
  fi
  if grep -E "^WARNING" "${stderr_path}" \
       | grep -vE "protocol downgrade|protocol version 28|protocol 28" \
       >/dev/null 2>&1; then
    echo "FAIL ${label}: client stderr contains unexpected WARNING" >&2
    quiet=0
  fi
  [[ ${quiet} -eq 1 ]]
}

fixture() {
  local name="$1"
  shift
  echo "=== ${name}: $* ==="
}

URL_BASE="rsync://127.0.0.1:${PORT}/legacy"

# ----- F1: empty dir, pull + push -----------------------------------------
fixture F1 "empty dir pull+push"
reset_client_dest f1
if run_client "${ROOT}/f1.pull.err" -av "${URL_BASE}/f1/" "${CLIENT_DEST}/f1/" \
   && verify_diff "${DAEMON_SHARE}/f1" "${CLIENT_DEST}/f1" \
   && check_client_quiet F1.pull "${ROOT}/f1.pull.err" \
   && check_daemon_quiet F1.pull; then
  record F1.pull PASS
else
  record F1.pull FAIL "see ${ROOT}/f1.pull.err"
fi
reset_daemon_share_sub f1_push
if run_client "${ROOT}/f1.push.err" -av "${CLIENT_SRC}/f1/" "${URL_BASE}/f1_push/" \
   && verify_diff "${CLIENT_SRC}/f1" "${DAEMON_SHARE}/f1_push" \
   && check_client_quiet F1.push "${ROOT}/f1.push.err" \
   && check_daemon_quiet F1.push; then
  record F1.push PASS
else
  record F1.push FAIL "see ${ROOT}/f1.push.err"
fi

# ----- F2: 100 small files, pull + push -----------------------------------
fixture F2 "100 small files pull+push"
reset_client_dest f2
if run_client "${ROOT}/f2.pull.err" -av "${URL_BASE}/f2/" "${CLIENT_DEST}/f2/" \
   && verify_diff "${DAEMON_SHARE}/f2" "${CLIENT_DEST}/f2" \
   && check_client_quiet F2.pull "${ROOT}/f2.pull.err" \
   && check_daemon_quiet F2.pull; then
  record F2.pull PASS
else
  record F2.pull FAIL "see ${ROOT}/f2.pull.err"
fi
reset_daemon_share_sub f2_push
if run_client "${ROOT}/f2.push.err" -av "${CLIENT_SRC}/f2/" "${URL_BASE}/f2_push/" \
   && verify_diff "${CLIENT_SRC}/f2" "${DAEMON_SHARE}/f2_push" \
   && check_client_quiet F2.push "${ROOT}/f2.push.err" \
   && check_daemon_quiet F2.push; then
  record F2.push PASS
else
  record F2.push FAIL "see ${ROOT}/f2.push.err"
fi

# ----- F3: zlib compression, pull -----------------------------------------
fixture F3 "zlib -z pull"
reset_client_dest f3
if run_client "${ROOT}/f3.pull.err" -avz "${URL_BASE}/f3/" "${CLIENT_DEST}/f3/" \
   && verify_diff "${DAEMON_SHARE}/f3" "${CLIENT_DEST}/f3" \
   && check_client_quiet F3.pull "${ROOT}/f3.pull.err" \
   && check_daemon_quiet F3.pull; then
  record F3.pull PASS
else
  record F3.pull FAIL "see ${ROOT}/f3.pull.err"
fi

# ----- F4: --checksum pull ------------------------------------------------
fixture F4 "--checksum pull"
rm -rf "${CLIENT_DEST}/f4"
cp -a "${CLIENT_DEST}/f4_seed" "${CLIENT_DEST}/f4"
if run_client "${ROOT}/f4.pull.err" -av --checksum \
     "${URL_BASE}/f4/" "${CLIENT_DEST}/f4/" \
   && verify_diff "${DAEMON_SHARE}/f4" "${CLIENT_DEST}/f4" \
   && check_client_quiet F4.pull "${ROOT}/f4.pull.err" \
   && check_daemon_quiet F4.pull; then
  record F4.pull PASS
else
  record F4.pull FAIL "see ${ROOT}/f4.pull.err"
fi

# ----- F5: --delete pull + push -------------------------------------------
fixture F5 "--delete pull+push"
rm -rf "${CLIENT_DEST}/f5_pull"
cp -a "${CLIENT_DEST}/f5_pull_seed" "${CLIENT_DEST}/f5_pull"
if run_client "${ROOT}/f5.pull.err" -av --delete \
     "${URL_BASE}/f5_pull/" "${CLIENT_DEST}/f5_pull/" \
   && verify_diff "${DAEMON_SHARE}/f5_pull" "${CLIENT_DEST}/f5_pull" \
   && check_client_quiet F5.pull "${ROOT}/f5.pull.err" \
   && check_daemon_quiet F5.pull; then
  record F5.pull PASS
else
  record F5.pull FAIL "see ${ROOT}/f5.pull.err"
fi

rm -rf "${DAEMON_SHARE}/f5_push"
cp -a "${DAEMON_SHARE}/f5_push_seed" "${DAEMON_SHARE}/f5_push"
if run_client "${ROOT}/f5.push.err" -av --delete \
     "${CLIENT_SRC}/f5_push/" "${URL_BASE}/f5_push/" \
   && verify_diff "${CLIENT_SRC}/f5_push" "${DAEMON_SHARE}/f5_push" \
   && check_client_quiet F5.push "${ROOT}/f5.push.err" \
   && check_daemon_quiet F5.push; then
  record F5.push PASS
else
  record F5.push FAIL "see ${ROOT}/f5.push.err"
fi

# ----- F6: 3-level directory tree push ------------------------------------
fixture F6 "3-level directory tree push"
reset_daemon_share_sub f6_push
if run_client "${ROOT}/f6.push.err" -av "${CLIENT_SRC}/f6/" "${URL_BASE}/f6_push/" \
   && verify_diff "${CLIENT_SRC}/f6" "${DAEMON_SHARE}/f6_push" \
   && check_client_quiet F6.push "${ROOT}/f6.push.err" \
   && check_daemon_quiet F6.push; then
  record F6.push PASS
else
  record F6.push FAIL "see ${ROOT}/f6.push.err"
fi

# ----- F7: extended-character filename pull + push ------------------------
fixture F7 "extended-char filename pull+push"
reset_client_dest f7
if run_client "${ROOT}/f7.pull.err" -av "${URL_BASE}/f7/" "${CLIENT_DEST}/f7/" \
   && verify_diff "${DAEMON_SHARE}/f7" "${CLIENT_DEST}/f7" \
   && check_client_quiet F7.pull "${ROOT}/f7.pull.err" \
   && check_daemon_quiet F7.pull; then
  record F7.pull PASS
else
  record F7.pull FAIL "see ${ROOT}/f7.pull.err"
fi
reset_daemon_share_sub f7_push
if run_client "${ROOT}/f7.push.err" -av "${CLIENT_SRC}/f7/" "${URL_BASE}/f7_push/" \
   && verify_diff "${CLIENT_SRC}/f7" "${DAEMON_SHARE}/f7_push" \
   && check_client_quiet F7.push "${ROOT}/f7.push.err" \
   && check_daemon_quiet F7.push; then
  record F7.push PASS
else
  record F7.push FAIL "see ${ROOT}/f7.push.err"
fi

# ----- F8: hardlink group pull + push -------------------------------------
fixture F8 "hardlink group pull+push"
reset_client_dest f8
if run_client "${ROOT}/f8.pull.err" -avH "${URL_BASE}/f8/" "${CLIENT_DEST}/f8/" \
   && verify_diff "${DAEMON_SHARE}/f8" "${CLIENT_DEST}/f8"; then
  ino_a=$(stat -c '%i' "${CLIENT_DEST}/f8/a.txt")
  ino_b=$(stat -c '%i' "${CLIENT_DEST}/f8/b.txt")
  if [[ "${ino_a}" == "${ino_b}" ]] \
     && check_client_quiet F8.pull "${ROOT}/f8.pull.err" \
     && check_daemon_quiet F8.pull; then
    record F8.pull PASS
  else
    record F8.pull FAIL "hardlink inodes differ (${ino_a} vs ${ino_b})"
  fi
else
  record F8.pull FAIL "see ${ROOT}/f8.pull.err"
fi

reset_daemon_share_sub f8_push
if run_client "${ROOT}/f8.push.err" -avH "${CLIENT_SRC}/f8/" "${URL_BASE}/f8_push/" \
   && verify_diff "${CLIENT_SRC}/f8" "${DAEMON_SHARE}/f8_push"; then
  ino_a=$(stat -c '%i' "${DAEMON_SHARE}/f8_push/a.txt")
  ino_b=$(stat -c '%i' "${DAEMON_SHARE}/f8_push/b.txt")
  if [[ "${ino_a}" == "${ino_b}" ]] \
     && check_client_quiet F8.push "${ROOT}/f8.push.err" \
     && check_daemon_quiet F8.push; then
    record F8.push PASS
  else
    record F8.push FAIL "hardlink inodes differ (${ino_a} vs ${ino_b})"
  fi
else
  record F8.push FAIL "see ${ROOT}/f8.push.err"
fi

# ----- F9: incremental update, pull twice ---------------------------------
fixture F9 "incremental update (pull x2)"
reset_client_dest f9
if run_client "${ROOT}/f9.pull1.err" -av "${URL_BASE}/f9/" "${CLIENT_DEST}/f9/" \
   && verify_diff "${DAEMON_SHARE}/f9" "${CLIENT_DEST}/f9" \
   && check_client_quiet F9.pull1 "${ROOT}/f9.pull1.err" \
   && check_daemon_quiet F9.pull1; then
  if run_client "${ROOT}/f9.pull2.err" -av --stats \
       "${URL_BASE}/f9/" "${CLIENT_DEST}/f9/" \
     && verify_diff "${DAEMON_SHARE}/f9" "${CLIENT_DEST}/f9" \
     && check_client_quiet F9.pull2 "${ROOT}/f9.pull2.err" \
     && check_daemon_quiet F9.pull2; then
    record F9.pull PASS
  else
    record F9.pull FAIL "second pull diverged - see ${ROOT}/f9.pull2.err"
  fi
else
  record F9.pull FAIL "first pull diverged - see ${ROOT}/f9.pull1.err"
fi

# ----- F10: 1 MiB file with delta after modification ----------------------
fixture F10 "1 MiB delta pull"
reset_client_dest f10
if run_client "${ROOT}/f10.pull1.err" -av "${URL_BASE}/f10/" "${CLIENT_DEST}/f10/" \
   && verify_diff "${DAEMON_SHARE}/f10" "${CLIENT_DEST}/f10" \
   && check_client_quiet F10.pull1 "${ROOT}/f10.pull1.err" \
   && check_daemon_quiet F10.pull1; then
  # Modify the daemon-side copy mid-stream to force the rolling+strong
  # checksum DECODE path at proto 28 on the next pull. Touch a few hundred
  # bytes in the middle so most of the file still matches block-for-block.
  printf 'F10 mutation marker A\n' \
    | dd of="${DAEMON_SHARE}/f10/payload.bin" \
         bs=1 seek=524288 conv=notrunc 2>/dev/null
  if run_client "${ROOT}/f10.pull2.err" -av "${URL_BASE}/f10/" "${CLIENT_DEST}/f10/" \
     && verify_diff "${DAEMON_SHARE}/f10" "${CLIENT_DEST}/f10" \
     && check_client_quiet F10.pull2 "${ROOT}/f10.pull2.err" \
     && check_daemon_quiet F10.pull2; then
    record F10.pull PASS
  else
    record F10.pull FAIL "delta pull diverged - see ${ROOT}/f10.pull2.err"
  fi
else
  record F10.pull FAIL "first pull diverged - see ${ROOT}/f10.pull1.err"
fi

# ----- F11: capability-string back-negotiation push -----------------------
# oc-rsync emits its modern capability string; the proto-28 daemon must
# accept it without rejecting the connection. Pass = transfer succeeds.
fixture F11 "capability back-negotiation push"
reset_daemon_share_sub f11_push
if run_client "${ROOT}/f11.push.err" -av "${CLIENT_SRC}/f11/" "${URL_BASE}/f11_push/" \
   && verify_diff "${CLIENT_SRC}/f11" "${DAEMON_SHARE}/f11_push" \
   && check_client_quiet F11.push "${ROOT}/f11.push.err" \
   && check_daemon_quiet F11.push; then
  record F11.push PASS
else
  record F11.push FAIL "see ${ROOT}/f11.push.err"
fi

# ----- F12: --exclude / --filter pull + push ------------------------------
fixture F12 "--exclude pull+push"
reset_client_dest f12
if run_client "${ROOT}/f12.pull.err" -av --exclude '*.log' \
     "${URL_BASE}/f12/" "${CLIENT_DEST}/f12/" \
   && [[ -f "${CLIENT_DEST}/f12/keep.txt" ]] \
   && [[ ! -f "${CLIENT_DEST}/f12/skip.log" ]] \
   && check_client_quiet F12.pull "${ROOT}/f12.pull.err" \
   && check_daemon_quiet F12.pull; then
  record F12.pull PASS
else
  record F12.pull FAIL "see ${ROOT}/f12.pull.err"
fi
reset_daemon_share_sub f12_push
if run_client "${ROOT}/f12.push.err" -av --exclude '*.log' \
     "${CLIENT_SRC}/f12/" "${URL_BASE}/f12_push/" \
   && [[ -f "${DAEMON_SHARE}/f12_push/keep.txt" ]] \
   && [[ ! -f "${DAEMON_SHARE}/f12_push/skip.log" ]] \
   && check_client_quiet F12.push "${ROOT}/f12.push.err" \
   && check_daemon_quiet F12.push; then
  record F12.push PASS
else
  record F12.push FAIL "see ${ROOT}/f12.push.err"
fi

echo
echo "RP28.f.2 summary (${PASS_COUNT} pass, ${FAIL_COUNT} fail):"
for line in "${RESULTS[@]}"; do
  echo "  ${line}"
done

if [[ ${FAIL_COUNT} -ne 0 ]]; then
  exit 1
fi
