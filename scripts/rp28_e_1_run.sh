#!/usr/bin/env bash
# RP28.e.2 - run daemon-mode rsync 2.6.9 client interop harness
#
# Orchestrates the D1-D10 fixture matrix from
# `docs/design/rp28-e-1-daemon-2-6-9-client-harness.md`. The topology is
# inverted relative to RP28.c/RP28.d: oc-rsync runs as
# `--daemon --no-detach` and rsync 2.6.9 is the client.
#
# Usage:
#   scripts/rp28_e_1_run.sh \
#     [--oc-rsync target/release/oc-rsync] \
#     [--rsync-2-6-9 /usr/local/bin/rsync-2.6.9]
#
# Exit codes:
#   0  - all 10 fixtures passed
#   1  - one or more fixtures failed
#   77 - either required binary missing (treat as skip)
#
# Task: RP28.e.2 (#2964). Parent: RP28.e (#2730). Spec: RP28.e.1 (#2963).

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
      sed -n '2,18p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ ! -x "${OC_RSYNC}" ]]; then
  echo "RP28.e.2 skip: oc-rsync binary not found at ${OC_RSYNC}" >&2
  exit 77
fi
if [[ ! -x "${RSYNC_269}" ]]; then
  echo "RP28.e.2 skip: rsync 2.6.9 binary not found at ${RSYNC_269}" >&2
  exit 77
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${RP28_E_ROOT:-/tmp/rp28-e}"
export RP28_E_ROOT="${ROOT}"

DAEMON_SHARE="${ROOT}/daemon-share"
CLIENT_SRC="${ROOT}/client-src"
CLIENT_DEST="${ROOT}/client-dest"
DAEMON_CONF="${ROOT}/oc-rsyncd.conf"
DAEMON_PID_FILE="${ROOT}/oc-rsyncd.pid"
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
  if [[ ${rc} -ne 0 && -f "${DAEMON_LOG}" ]]; then
    echo "--- oc-rsync daemon log (tail) ---" >&2
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

echo "RP28.e.2: oc-rsync=$(${OC_RSYNC} --version 2>&1 | head -1)"
echo "RP28.e.2: rsync-2.6.9=$(${RSYNC_269} --version 2>&1 | head -1)"

bash "${SCRIPT_DIR}/rp28_e_1_setup.sh"

# Ephemeral port. Bind to verify availability then release immediately;
# brief reuse race vs. the daemon start is the same trade-off RP28.c/d
# accept and is tolerated by the bind-poll loop below.
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
echo "${PORT}" > "${PORT_FILE}"
echo "RP28.e.2: daemon port ${PORT}"

"${OC_RSYNC}" --daemon --no-detach --config "${DAEMON_CONF}" --port "${PORT}" \
  > "${DAEMON_LOG}" 2>&1 &
DAEMON_PID=$!

# Wait up to 10s for the daemon to bind.
for _ in $(seq 1 20); do
  if (echo >/dev/tcp/127.0.0.1/"${PORT}") 2>/dev/null; then
    break
  fi
  if ! kill -0 "${DAEMON_PID}" 2>/dev/null; then
    echo "FATAL: oc-rsync daemon exited before binding port ${PORT}" >&2
    exit 1
  fi
  sleep 0.5
done
if ! (echo >/dev/tcp/127.0.0.1/"${PORT}") 2>/dev/null; then
  echo "FATAL: oc-rsync daemon failed to bind port ${PORT} within 10s" >&2
  exit 1
fi
echo "RP28.e.2: daemon bound 127.0.0.1:${PORT}"

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
  # Runs rsync 2.6.9 against the daemon with a 60s wall-clock guard and
  # returns its exit status. Stderr is captured per-fixture into the
  # named file so the caller can inspect it.
  local stderr_path="$1"
  shift
  set +e
  timeout 60 "${RSYNC_269}" "$@" 2> "${stderr_path}"
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
  # Fail if the daemon emitted panics or unexpected `error:` / `WARNING`
  # lines. Protocol-downgrade WARNINGs at proto-28 negotiation are the
  # only allowlisted warnings per spec section 5.
  local label="$1"
  local quiet=1
  if grep -E "panicked at|thread .* panicked" "${DAEMON_LOG}" >/dev/null 2>&1; then
    echo "FAIL ${label}: daemon log contains panic" >&2
    quiet=0
  fi
  if grep -E "^error:" "${DAEMON_LOG}" >/dev/null 2>&1; then
    echo "FAIL ${label}: daemon log contains error: line" >&2
    quiet=0
  fi
  if grep -E "^WARNING" "${DAEMON_LOG}" \
       | grep -vE "protocol downgrade|protocol version 28|protocol 28" \
       >/dev/null 2>&1; then
    echo "FAIL ${label}: daemon log contains unexpected WARNING" >&2
    quiet=0
  fi
  [[ ${quiet} -eq 1 ]]
}

# Each fixture follows the same scaffold: prepare client side, invoke the
# 2.6.9 client, capture rc, run the verifier, then record the result.
fixture() {
  local name="$1"
  shift
  echo "=== ${name}: $* ==="
}

URL_BASE="rsync://127.0.0.1:${PORT}/test"

# ----- D1: empty dir, pull + push -----------------------------------------
fixture D1 "empty dir pull+push"
reset_client_dest d1
if run_client "${ROOT}/d1.pull.err" -av "${URL_BASE}/d1/" "${CLIENT_DEST}/d1/" \
   && verify_diff "${DAEMON_SHARE}/d1" "${CLIENT_DEST}/d1" \
   && check_daemon_quiet D1.pull; then
  record D1.pull PASS
else
  record D1.pull FAIL "see ${ROOT}/d1.pull.err"
fi
reset_daemon_share_sub d1_push
if run_client "${ROOT}/d1.push.err" -av "${CLIENT_SRC}/d1/" "${URL_BASE}/d1_push/" \
   && verify_diff "${CLIENT_SRC}/d1" "${DAEMON_SHARE}/d1_push" \
   && check_daemon_quiet D1.push; then
  record D1.push PASS
else
  record D1.push FAIL "see ${ROOT}/d1.push.err"
fi

# ----- D2: 100 small files, pull + push -----------------------------------
fixture D2 "100 small files pull+push"
reset_client_dest d2
if run_client "${ROOT}/d2.pull.err" -av "${URL_BASE}/d2/" "${CLIENT_DEST}/d2/" \
   && verify_diff "${DAEMON_SHARE}/d2" "${CLIENT_DEST}/d2" \
   && check_daemon_quiet D2.pull; then
  record D2.pull PASS
else
  record D2.pull FAIL "see ${ROOT}/d2.pull.err"
fi
reset_daemon_share_sub d2_push
if run_client "${ROOT}/d2.push.err" -av "${CLIENT_SRC}/d2/" "${URL_BASE}/d2_push/" \
   && verify_diff "${CLIENT_SRC}/d2" "${DAEMON_SHARE}/d2_push" \
   && check_daemon_quiet D2.push; then
  record D2.push PASS
else
  record D2.push FAIL "see ${ROOT}/d2.push.err"
fi

# ----- D3: zlib compression, pull -----------------------------------------
fixture D3 "zlib -z pull"
reset_client_dest d3
if run_client "${ROOT}/d3.pull.err" -avz "${URL_BASE}/d3/" "${CLIENT_DEST}/d3/" \
   && verify_diff "${DAEMON_SHARE}/d3" "${CLIENT_DEST}/d3" \
   && check_daemon_quiet D3.pull; then
  record D3.pull PASS
else
  record D3.pull FAIL "see ${ROOT}/d3.pull.err"
fi

# ----- D4: --checksum pull ------------------------------------------------
fixture D4 "--checksum pull"
rm -rf "${CLIENT_DEST}/d4"
cp -a "${CLIENT_DEST}/d4_seed" "${CLIENT_DEST}/d4"
if run_client "${ROOT}/d4.pull.err" -av --checksum \
     "${URL_BASE}/d4/" "${CLIENT_DEST}/d4/" \
   && verify_diff "${DAEMON_SHARE}/d4" "${CLIENT_DEST}/d4" \
   && check_daemon_quiet D4.pull; then
  record D4.pull PASS
else
  record D4.pull FAIL "see ${ROOT}/d4.pull.err"
fi

# ----- D5: --delete pull + push -------------------------------------------
fixture D5 "--delete pull+push"
rm -rf "${CLIENT_DEST}/d5_pull"
cp -a "${CLIENT_DEST}/d5_pull_seed" "${CLIENT_DEST}/d5_pull"
if run_client "${ROOT}/d5.pull.err" -av --delete \
     "${URL_BASE}/d5_pull/" "${CLIENT_DEST}/d5_pull/" \
   && verify_diff "${DAEMON_SHARE}/d5_pull" "${CLIENT_DEST}/d5_pull" \
   && check_daemon_quiet D5.pull; then
  record D5.pull PASS
else
  record D5.pull FAIL "see ${ROOT}/d5.pull.err"
fi

rm -rf "${DAEMON_SHARE}/d5_push"
cp -a "${DAEMON_SHARE}/d5_push_seed" "${DAEMON_SHARE}/d5_push"
if run_client "${ROOT}/d5.push.err" -av --delete \
     "${CLIENT_SRC}/d5_push/" "${URL_BASE}/d5_push/" \
   && verify_diff "${CLIENT_SRC}/d5_push" "${DAEMON_SHARE}/d5_push" \
   && check_daemon_quiet D5.push; then
  record D5.push PASS
else
  record D5.push FAIL "see ${ROOT}/d5.push.err"
fi

# ----- D6: 3-level directory tree push ------------------------------------
fixture D6 "3-level directory tree push"
reset_daemon_share_sub d6_push
if run_client "${ROOT}/d6.push.err" -av "${CLIENT_SRC}/d6/" "${URL_BASE}/d6_push/" \
   && verify_diff "${CLIENT_SRC}/d6" "${DAEMON_SHARE}/d6_push" \
   && check_daemon_quiet D6.push; then
  record D6.push PASS
else
  record D6.push FAIL "see ${ROOT}/d6.push.err"
fi

# ----- D7: extended-character filename pull + push ------------------------
fixture D7 "extended-char filename pull+push"
reset_client_dest d7
if run_client "${ROOT}/d7.pull.err" -av "${URL_BASE}/d7/" "${CLIENT_DEST}/d7/" \
   && verify_diff "${DAEMON_SHARE}/d7" "${CLIENT_DEST}/d7" \
   && check_daemon_quiet D7.pull; then
  record D7.pull PASS
else
  record D7.pull FAIL "see ${ROOT}/d7.pull.err"
fi
reset_daemon_share_sub d7_push
if run_client "${ROOT}/d7.push.err" -av "${CLIENT_SRC}/d7/" "${URL_BASE}/d7_push/" \
   && verify_diff "${CLIENT_SRC}/d7" "${DAEMON_SHARE}/d7_push" \
   && check_daemon_quiet D7.push; then
  record D7.push PASS
else
  record D7.push FAIL "see ${ROOT}/d7.push.err"
fi

# ----- D8: hardlink group pull + push -------------------------------------
fixture D8 "hardlink group pull+push"
reset_client_dest d8
if run_client "${ROOT}/d8.pull.err" -avH "${URL_BASE}/d8/" "${CLIENT_DEST}/d8/" \
   && verify_diff "${DAEMON_SHARE}/d8" "${CLIENT_DEST}/d8"; then
  ino_a=$(stat -c '%i' "${CLIENT_DEST}/d8/a.txt")
  ino_b=$(stat -c '%i' "${CLIENT_DEST}/d8/b.txt")
  if [[ "${ino_a}" == "${ino_b}" ]] && check_daemon_quiet D8.pull; then
    record D8.pull PASS
  else
    record D8.pull FAIL "hardlink inodes differ (${ino_a} vs ${ino_b})"
  fi
else
  record D8.pull FAIL "see ${ROOT}/d8.pull.err"
fi

reset_daemon_share_sub d8_push
if run_client "${ROOT}/d8.push.err" -avH "${CLIENT_SRC}/d8/" "${URL_BASE}/d8_push/" \
   && verify_diff "${CLIENT_SRC}/d8" "${DAEMON_SHARE}/d8_push"; then
  ino_a=$(stat -c '%i' "${DAEMON_SHARE}/d8_push/a.txt")
  ino_b=$(stat -c '%i' "${DAEMON_SHARE}/d8_push/b.txt")
  if [[ "${ino_a}" == "${ino_b}" ]] && check_daemon_quiet D8.push; then
    record D8.push PASS
  else
    record D8.push FAIL "hardlink inodes differ (${ino_a} vs ${ino_b})"
  fi
else
  record D8.push FAIL "see ${ROOT}/d8.push.err"
fi

# ----- D9: incremental update, pull twice ---------------------------------
fixture D9 "incremental update (pull x2)"
reset_client_dest d9
if run_client "${ROOT}/d9.pull1.err" -av "${URL_BASE}/d9/" "${CLIENT_DEST}/d9/" \
   && verify_diff "${DAEMON_SHARE}/d9" "${CLIENT_DEST}/d9" \
   && check_daemon_quiet D9.pull1; then
  if run_client "${ROOT}/d9.pull2.err" -av --stats \
       "${URL_BASE}/d9/" "${CLIENT_DEST}/d9/" \
     && verify_diff "${DAEMON_SHARE}/d9" "${CLIENT_DEST}/d9" \
     && check_daemon_quiet D9.pull2; then
    record D9.pull PASS
  else
    record D9.pull FAIL "second pull diverged - see ${ROOT}/d9.pull2.err"
  fi
else
  record D9.pull FAIL "first pull diverged - see ${ROOT}/d9.pull1.err"
fi

# ----- D10: 1 MiB file with delta after modification ----------------------
fixture D10 "1 MiB delta pull"
reset_client_dest d10
if run_client "${ROOT}/d10.pull1.err" -av "${URL_BASE}/d10/" "${CLIENT_DEST}/d10/" \
   && verify_diff "${DAEMON_SHARE}/d10" "${CLIENT_DEST}/d10" \
   && check_daemon_quiet D10.pull1; then
  # Modify the daemon-side copy mid-stream to force the rolling+strong
  # checksum match phase on the next pull. Touch a few hundred bytes in
  # the middle so most of the file still matches block-for-block.
  printf 'D10 mutation marker A\n' \
    | dd of="${DAEMON_SHARE}/d10/payload.bin" \
         bs=1 seek=524288 conv=notrunc 2>/dev/null
  if run_client "${ROOT}/d10.pull2.err" -av "${URL_BASE}/d10/" "${CLIENT_DEST}/d10/" \
     && verify_diff "${DAEMON_SHARE}/d10" "${CLIENT_DEST}/d10" \
     && check_daemon_quiet D10.pull2; then
    record D10.pull PASS
  else
    record D10.pull FAIL "delta pull diverged - see ${ROOT}/d10.pull2.err"
  fi
else
  record D10.pull FAIL "first pull diverged - see ${ROOT}/d10.pull1.err"
fi

echo
echo "RP28.e.2 summary (${PASS_COUNT} pass, ${FAIL_COUNT} fail):"
for line in "${RESULTS[@]}"; do
  echo "  ${line}"
done

if [[ ${FAIL_COUNT} -ne 0 ]]; then
  exit 1
fi
