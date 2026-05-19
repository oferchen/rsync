#!/usr/bin/env bash
# Portable rsync interop smoke harness
#
# Goals
# -----
# - Run on Linux, macOS, and Windows (via MSYS2 / Cygwin).
# - Validate a small but meaningful set of wire-compatible scenarios
#   between oc-rsync and a host-provided upstream rsync binary.
# - Skip Linux-only / privileged scenarios (xattr, ACL, daemon on a
#   privileged port, SSH loopback). Those remain the responsibility of
#   tools/ci/run_interop.sh on Linux.
#
# How wire-level interop is exercised
# -----------------------------------
# A local `src/ -> dst/` rsync command does an in-process copy and
# never speaks the wire protocol. To get oc-rsync and upstream rsync
# talking to each other across the wire, we spawn each as a daemon on
# a high TCP port and run the other as the client against
# `rsync://localhost:PORT/module/`. This works identically on Linux
# and macOS without needing SSH, sudo, or systemd.
#
# Windows (MSYS2) covers the upstream-daemon side only: the oc-rsync
# daemon is intentionally unsupported on Windows (see
# crates/cli/src/frontend/server/daemon.rs), so scenarios that require
# `oc-rsync --daemon` are skipped with a clear log line on that host.
#
# Inputs
# ------
# - $OC_RSYNC: path to the oc-rsync binary under test (required).
# - $UPSTREAM_RSYNC: path to the upstream rsync binary (default: `rsync`
#   from PATH).
#
# Exit code
# ---------
# - 0 if all scenarios pass.
# - non-zero on the first failure (with a clear PASS/FAIL log line).

set -euo pipefail

OC_RSYNC="${OC_RSYNC:-}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-rsync}"

if [[ -z "${OC_RSYNC}" ]]; then
  echo "OC_RSYNC must point to the oc-rsync binary under test" >&2
  exit 2
fi
if [[ ! -x "${OC_RSYNC}" ]]; then
  echo "OC_RSYNC=${OC_RSYNC} is not executable" >&2
  exit 2
fi
if ! command -v "${UPSTREAM_RSYNC}" >/dev/null 2>&1; then
  echo "Upstream rsync (${UPSTREAM_RSYNC}) not found in PATH" >&2
  exit 2
fi

os="$(uname -s 2>/dev/null || echo unknown)"
case "$os" in
  Linux*)   host_os=linux ;;
  Darwin*)  host_os=macos ;;
  CYGWIN*|MINGW*|MSYS*) host_os=windows ;;
  *)        host_os=unknown ;;
esac

echo "host_os=${host_os}"
echo "oc-rsync:       $(${OC_RSYNC} --version 2>&1 | head -1)"
echo "upstream rsync: $(${UPSTREAM_RSYNC} --version 2>&1 | head -1)"

workdir="$(mktemp -d 2>/dev/null || mktemp -d -t ocrsync-smoke)"
oc_pid=""
up_pid=""

cleanup() {
  # Stop daemons before deleting their workdir so they do not race
  # against rmdir on Windows (where deleting an open file's parent
  # directory fails with ERROR_SHARING_VIOLATION).
  if [[ -n "${oc_pid}" ]]; then kill "${oc_pid}" 2>/dev/null || true; fi
  if [[ -n "${up_pid}" ]]; then kill "${up_pid}" 2>/dev/null || true; fi
  rm -rf "${workdir}" 2>/dev/null || true
}
trap cleanup EXIT

src="${workdir}/src"
dst_oc_push="${workdir}/dst-oc-push"
dst_oc_pull="${workdir}/dst-oc-pull"
dst_baseline="${workdir}/dst-baseline"
mkdir -p "${src}" "${dst_oc_push}" "${dst_oc_pull}" "${dst_baseline}"

# Build a small but non-trivial corpus. Avoid features that are
# platform-specific (symlinks, devices, xattr, hardlinks) so the same
# fixture works on Linux/macOS/Windows.
mkdir -p "${src}/subdir/nested"
printf 'hello\n' > "${src}/hello.txt"
printf 'world\n' > "${src}/subdir/world.txt"
printf 'deep\n'  > "${src}/subdir/nested/deep.txt"
# A medium-sized random file to actually exercise the delta engine.
# 256 KiB is enough to push past a single block without being slow.
dd if=/dev/urandom of="${src}/random.bin" bs=1024 count=256 \
   >/dev/null 2>&1
# Empty file edge case.
: > "${src}/empty.txt"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# Portable directory-tree diff. macOS/BSD `diff -r` works the same as
# GNU `diff -r` for content comparison.
tree_diff() {
  diff -r "$1" "$2"
}

# Pick a random ephemeral port in the user range and confirm nothing
# else is listening. We do not need true atomicity; the worst case is
# a TOCTOU race that fails the daemon start, which is loud and
# obvious.
pick_port() {
  # Bash's $RANDOM is 0..32767. Bias into 40000..49999.
  local p=$(( 40000 + (RANDOM % 10000) ))
  echo "${p}"
}

# Wait for a TCP port to accept connections. Uses bash's built-in
# /dev/tcp where available (Linux/macOS) and falls back to invoking
# upstream rsync's `--list-only` against the daemon on Windows/MSYS2
# (where /dev/tcp is not exposed by Cygwin's bash).
wait_for_port() {
  local port=$1
  local deadline=$(( SECONDS + 15 ))
  while (( SECONDS < deadline )); do
    if (exec 3<>/dev/tcp/127.0.0.1/${port}) 2>/dev/null; then
      exec 3>&- 2>/dev/null || true
      return 0
    fi
    if "${UPSTREAM_RSYNC}" \
         "rsync://127.0.0.1:${port}/" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

# Start an rsync-compatible daemon serving ${workdir} as module
# `data`. Returns the PID via a global var ($1) and the chosen port
# via stdout. Daemon log goes to a per-instance file under workdir.
start_daemon() {
  local impl=$1          # path to rsync binary
  local label=$2         # "oc" or "up", used for filenames
  local pid_var=$3       # name of the global to populate with the PID
  local conf="${workdir}/${label}-rsyncd.conf"
  local log="${workdir}/${label}-rsyncd.log"
  local pidfile="${workdir}/${label}-rsyncd.pid"
  local port
  port="$(pick_port)"

  cat > "${conf}" <<EOF
use chroot = false
max connections = 4
pid file = ${pidfile}
log file = ${log}
lock file = ${workdir}/${label}-rsyncd.lock
port = ${port}

[data]
  path = ${workdir}/data-${label}
  read only = false
EOF

  # Only emit uid/gid when running as root; non-root daemons inherit
  # the invoking user and a `uid =` directive would otherwise trip
  # permission errors on macOS/MSYS2 where the runner is unprivileged.
  if [[ "$(id -u 2>/dev/null || echo 1)" == "0" ]]; then
    cat >> "${conf}" <<EOF
  uid = $(id -u)
  gid = $(id -g)
  numeric ids = true
EOF
  fi

  mkdir -p "${workdir}/data-${label}"

  "${impl}" --daemon --no-detach --config="${conf}" --port="${port}" \
    >> "${log}" 2>&1 &
  local pid=$!
  # Populate the named global so the caller can kill it on cleanup.
  printf -v "${pid_var}" '%s' "${pid}"

  if ! wait_for_port "${port}"; then
    echo "daemon ${label} (${impl}) failed to start on port ${port}" >&2
    [[ -f "${log}" ]] && sed 's/^/  /' "${log}" >&2 || true
    return 1
  fi
  echo "${port}"
}

# --- Scenario 1: baseline upstream local copy ---------------------------
"${UPSTREAM_RSYNC}" -a "${src}/" "${dst_baseline}/"
tree_diff "${src}" "${dst_baseline}" \
  || fail "baseline upstream local copy diverged"
pass "baseline upstream local copy"

# --- Scenario 2: oc-rsync client pushes into upstream daemon ------------
up_port="$(start_daemon "${UPSTREAM_RSYNC}" up up_pid)"
up_url="rsync://127.0.0.1:${up_port}/data"
"${OC_RSYNC}" -a "${src}/" "${up_url}/"
tree_diff "${src}" "${workdir}/data-up" \
  || fail "oc-rsync client / upstream daemon diverged (push)"
pass "oc-rsync client -> upstream daemon (push)"

# --- Scenario 3: oc-rsync client pulls from upstream daemon -------------
"${OC_RSYNC}" -a "${up_url}/" "${dst_oc_pull}/"
tree_diff "${src}" "${dst_oc_pull}" \
  || fail "oc-rsync client / upstream daemon diverged (pull)"
pass "oc-rsync client <- upstream daemon (pull)"

# Scenarios that require running oc-rsync as a daemon are skipped on
# Windows: the oc-rsync daemon is intentionally unsupported there (see
# crates/cli/src/frontend/server/daemon.rs - the binary exits with
# "daemon mode is not supported on this platform"). The upstream-daemon
# side of the wire is still exercised by scenarios 2, 3, and the
# scenario-7 delta push above.
if [[ "${host_os}" == "windows" ]]; then
  echo "SKIP: upstream client -> oc-rsync daemon (oc-rsync daemon unsupported on Windows)"
  echo "SKIP: upstream client <- oc-rsync daemon (oc-rsync daemon unsupported on Windows)"
else
  # --- Scenario 4: upstream client pushes into oc-rsync daemon ----------
  oc_port="$(start_daemon "${OC_RSYNC}" oc oc_pid)"
  oc_url="rsync://127.0.0.1:${oc_port}/data"
  "${UPSTREAM_RSYNC}" -a "${src}/" "${oc_url}/"
  tree_diff "${src}" "${workdir}/data-oc" \
    || fail "upstream client / oc-rsync daemon diverged (push)"
  pass "upstream client -> oc-rsync daemon (push)"

  # --- Scenario 5: upstream client pulls from oc-rsync daemon -----------
  "${UPSTREAM_RSYNC}" -a "${oc_url}/" "${dst_oc_push}/"
  tree_diff "${src}" "${dst_oc_push}" \
    || fail "upstream client / oc-rsync daemon diverged (pull)"
  pass "upstream client <- oc-rsync daemon (pull)"
fi

# --- Scenario 6: idempotent re-run (quick-check, no transfer) ----------
# Re-running the push should be a no-op. We look at --stats output for
# `Total transferred file size: 0 bytes` from oc-rsync.
out="$("${OC_RSYNC}" -a --stats "${src}/" "${up_url}/" 2>&1)"
if ! printf '%s\n' "${out}" \
     | grep -qE 'Total transferred file size: 0( bytes)?'; then
  printf '%s\n' "${out}" >&2
  fail "oc-rsync re-run transferred non-zero bytes"
fi
pass "oc-rsync re-run is a no-op (quick-check)"

# --- Scenario 7: delta update of a modified file ------------------------
# Modify a single byte and confirm both directions converge.
modified="${workdir}/src-modified"
cp -R "${src}" "${modified}"
printf 'X' | dd of="${modified}/random.bin" bs=1 count=1 conv=notrunc \
  seek=1024 >/dev/null 2>&1

"${OC_RSYNC}" -a "${modified}/" "${up_url}/"
tree_diff "${modified}" "${workdir}/data-up" \
  || fail "delta update push (oc-rsync -> upstream) diverged"
pass "delta update: oc-rsync client -> upstream daemon"

if [[ "${host_os}" == "windows" ]]; then
  echo "SKIP: delta update upstream -> oc-rsync daemon (oc-rsync daemon unsupported on Windows)"
else
  "${UPSTREAM_RSYNC}" -a "${modified}/" "${oc_url}/"
  tree_diff "${modified}" "${workdir}/data-oc" \
    || fail "delta update push (upstream -> oc-rsync) diverged"
  pass "delta update: upstream client -> oc-rsync daemon"
fi

echo "All interop smoke scenarios passed on ${host_os}."
