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
# Additionally, if the upstream rsync daemon fails to bind() on Windows
# (common on CI runners due to firewall/network restrictions), all
# daemon-dependent scenarios are skipped gracefully.
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
# Backdate the random file to a fixed old timestamp so its basis on the
# receiver has a clearly-old mtime. Scenario 7 modifies a same-size copy in
# place; without this, the modification and the basis can land in the same
# wall-clock second on a fast machine, and rsync's quick-check (equal size +
# same second-resolution mtime) skips the file - so the delta is never sent
# and the receiver keeps the stale content. This is standard rsync behaviour:
# upstream rsync 3.4.4 -> upstream 3.4.4 diverges identically in that case.
# Backdating makes the later in-place edit land in a strictly newer second,
# so the change is detected deterministically while still exercising the
# default mtime quick-check + delta path.
touch -t 202001010000 "${src}/random.bin"
# Empty file edge case.
: > "${src}/empty.txt"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# Run a wire-transfer client command with bounded retry on the daemon's
# "max connections ... try again later" reply.
#
# The rsync daemon caps concurrent clients with `max connections` and holds
# each slot via a POSIX record lock on its lock file (see upstream
# connection.c:claim_connection). That lock is released only when the child
# process serving the previous connection fully exits. This harness fires ~15
# client scenarios back-to-back against the same max=4 daemon within a
# sub-second window. On a fast host every child exits inside the inter-scenario
# gap, but on a slow CI runner a just-finished child can still be tearing down
# (releasing its lock) when the next scenario connects, so the daemon replies
# `@ERROR: max connections (4) reached -- try again later` and the client exits
# non-zero. This is inherent slot-release lag, not a client bug: an upstream
# rsync client contending for the same slots is rejected identically (verified
# by running 6 parallel clients against a max=4 daemon - upstream rsync and
# oc-rsync hit the limit an equal number of times). The "try again later"
# wording is an explicit invitation to retry, exactly what a real client does,
# so we retry a few times with a short backoff before failing.
#
# Combined stdout+stderr is streamed to this function's stdout so callers that
# capture output with `$(run_wire ...)` keep working. Returns the command's
# exit code (0 on eventual success, non-zero on non-retryable failure or after
# the retry budget is exhausted).
run_wire() {
  local attempt=1
  local max_attempts=5
  local out rc
  while true; do
    out="$("$@" 2>&1)"
    rc=$?
    if [[ ${rc} -eq 0 ]]; then
      printf '%s' "${out}"
      return 0
    fi
    if printf '%s' "${out}" | grep -q 'max connections' \
       && (( attempt < max_attempts )); then
      echo "retry ${attempt}/${max_attempts}: daemon slot busy (max connections), backing off" >&2
      sleep "$(awk "BEGIN{print 0.3 * ${attempt}}")"
      attempt=$(( attempt + 1 ))
      continue
    fi
    printf '%s' "${out}"
    return "${rc}"
  done
}

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
  munge symlinks = false
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
# On Windows CI runners the upstream rsync daemon may fail to bind() due
# to firewall/network restrictions. When that happens, mark the daemon
# as unavailable and skip every scenario that needs it rather than
# aborting the entire script. On Linux/macOS the failure remains fatal.
up_daemon_available=true
if up_port="$(start_daemon "${UPSTREAM_RSYNC}" up up_pid)"; then
  up_url="rsync://127.0.0.1:${up_port}/data"
else
  if [[ "${host_os}" == "windows" ]]; then
    echo "SKIP: upstream daemon failed to start (bind denied on Windows)"
    up_daemon_available=false
    up_url=""
  else
    fail "upstream daemon failed to start"
  fi
fi

if [[ "${up_daemon_available}" == "true" ]]; then
  run_wire "${OC_RSYNC}" -a "${src}/" "${up_url}/" \
    || fail "oc-rsync client / upstream daemon push failed"
  tree_diff "${src}" "${workdir}/data-up" \
    || fail "oc-rsync client / upstream daemon diverged (push)"
  pass "oc-rsync client -> upstream daemon (push)"
else
  echo "SKIP: oc-rsync client -> upstream daemon (upstream daemon unavailable on Windows)"
fi

# --- Scenario 3: oc-rsync client pulls from upstream daemon -------------
if [[ "${up_daemon_available}" == "true" ]]; then
  run_wire "${OC_RSYNC}" -a "${up_url}/" "${dst_oc_pull}/" \
    || fail "oc-rsync client / upstream daemon pull failed"
  tree_diff "${src}" "${dst_oc_pull}" \
    || fail "oc-rsync client / upstream daemon diverged (pull)"
  pass "oc-rsync client <- upstream daemon (pull)"
else
  echo "SKIP: oc-rsync client <- upstream daemon (upstream daemon unavailable on Windows)"
fi

# Scenarios that require running oc-rsync as a daemon are skipped on
# Windows: the oc-rsync daemon is intentionally unsupported there (see
# crates/cli/src/frontend/server/daemon.rs - the binary exits with
# "daemon mode is not supported on this platform"). When the upstream
# daemon is available, its side of the wire is exercised by scenarios
# 2, 3, and the scenario-7 delta push above.
if [[ "${host_os}" == "windows" ]]; then
  echo "SKIP: upstream client -> oc-rsync daemon (oc-rsync daemon unsupported on Windows)"
  echo "SKIP: upstream client <- oc-rsync daemon (oc-rsync daemon unsupported on Windows)"
else
  # --- Scenario 4: upstream client pushes into oc-rsync daemon ----------
  oc_port="$(start_daemon "${OC_RSYNC}" oc oc_pid)"
  oc_url="rsync://127.0.0.1:${oc_port}/data"
  run_wire "${UPSTREAM_RSYNC}" -a "${src}/" "${oc_url}/" \
    || fail "upstream client / oc-rsync daemon push failed"
  tree_diff "${src}" "${workdir}/data-oc" \
    || fail "upstream client / oc-rsync daemon diverged (push)"
  pass "upstream client -> oc-rsync daemon (push)"

  # --- Scenario 5: upstream client pulls from oc-rsync daemon -----------
  run_wire "${UPSTREAM_RSYNC}" -a "${oc_url}/" "${dst_oc_push}/" \
    || fail "upstream client / oc-rsync daemon pull failed"
  tree_diff "${src}" "${dst_oc_push}" \
    || fail "upstream client / oc-rsync daemon diverged (pull)"
  pass "upstream client <- oc-rsync daemon (pull)"
fi

# --- Scenario 6: idempotent re-run (quick-check, no transfer) ----------
# Re-running the push should be a no-op. We look at --stats output for
# `Total transferred file size: 0 bytes` from oc-rsync.
if [[ "${up_daemon_available}" == "true" ]]; then
  out="$(run_wire "${OC_RSYNC}" -a --stats "${src}/" "${up_url}/")"
  if ! printf '%s\n' "${out}" \
       | grep -qE 'Total transferred file size: 0( bytes)?'; then
    printf '%s\n' "${out}" >&2
    fail "oc-rsync re-run transferred non-zero bytes"
  fi
  pass "oc-rsync re-run is a no-op (quick-check)"
else
  echo "SKIP: oc-rsync re-run quick-check (upstream daemon unavailable on Windows)"
fi

# --- Scenario 7: delta update of a modified file ------------------------
# Modify a single byte and confirm both directions converge.
modified="${workdir}/src-modified"
cp -R "${src}" "${modified}"
printf 'X' | dd of="${modified}/random.bin" bs=1 count=1 conv=notrunc \
  seek=1024 >/dev/null 2>&1

if [[ "${up_daemon_available}" == "true" ]]; then
  run_wire "${OC_RSYNC}" -a "${modified}/" "${up_url}/" \
    || fail "delta update push (oc-rsync -> upstream) failed"
  tree_diff "${modified}" "${workdir}/data-up" \
    || fail "delta update push (oc-rsync -> upstream) diverged"
  pass "delta update: oc-rsync client -> upstream daemon"
else
  echo "SKIP: delta update oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
fi

if [[ "${host_os}" == "windows" ]]; then
  echo "SKIP: delta update upstream -> oc-rsync daemon (oc-rsync daemon unsupported on Windows)"
else
  run_wire "${UPSTREAM_RSYNC}" -a "${modified}/" "${oc_url}/" \
    || fail "delta update push (upstream -> oc-rsync) failed"
  tree_diff "${modified}" "${workdir}/data-oc" \
    || fail "delta update push (upstream -> oc-rsync) diverged"
  pass "delta update: upstream client -> oc-rsync daemon"
fi

# ===================================================================
# Extended cross-platform scenarios
#
# These exercise wire-level flag negotiation paths that the baseline
# scenarios above do not cover. Each scenario uses the upstream daemon
# already running on $up_port (oc-rsync as client) and, where the
# oc-rsync daemon is available (non-Windows), also exercises the
# reverse direction. Every scenario creates a fresh destination inside
# the daemon's module directory to avoid interference.
# ===================================================================

# Helper: reset a daemon module's data directory and optionally
# populate it with a setup function.
reset_module_data() {
  local label=$1   # "up" or "oc"
  local data_dir="${workdir}/data-${label}"
  rm -rf "${data_dir}"
  mkdir -p "${data_dir}"
}

# Helper: run a single extended scenario in both directions.
# $1 = scenario name (for log lines)
# $2 = extra rsync flags (space-separated string, applied after -a)
# $3 = optional setup function name (called with src_dir as $1)
# $4 = optional comparison function name (default: tree_diff src data)
run_extended_scenario() {
  local name=$1
  local flags=$2
  local setup_fn="${3:-}"
  local compare_fn="${4:-}"

  # Prepare a per-scenario source if a setup function is provided;
  # otherwise reuse the common $src.
  local scenario_src="${src}"
  if [[ -n "${setup_fn}" ]]; then
    scenario_src="${workdir}/src-${name}"
    rm -rf "${scenario_src}"
    cp -R "${src}" "${scenario_src}"
    "${setup_fn}" "${scenario_src}"
  fi

  # Direction 1: oc-rsync client -> upstream daemon
  if [[ "${up_daemon_available}" == "true" ]]; then
    reset_module_data "up"
    # shellcheck disable=SC2086
    run_wire "${OC_RSYNC}" ${flags} "${scenario_src}/" "${up_url}/" || {
      fail "${name}: oc-rsync -> upstream daemon (exit $?)"
    }
    if [[ -n "${compare_fn}" ]]; then
      "${compare_fn}" "${scenario_src}" "${workdir}/data-up" \
        || fail "${name}: oc-rsync -> upstream daemon diverged"
    else
      tree_diff "${scenario_src}" "${workdir}/data-up" \
        || fail "${name}: oc-rsync -> upstream daemon diverged"
    fi
    pass "${name}: oc-rsync -> upstream daemon"
  else
    echo "SKIP: ${name}: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
  fi

  # Direction 2: upstream client -> oc-rsync daemon (non-Windows)
  if [[ "${host_os}" != "windows" ]]; then
    reset_module_data "oc"
    # shellcheck disable=SC2086
    run_wire "${UPSTREAM_RSYNC}" ${flags} "${scenario_src}/" "${oc_url}/" || {
      fail "${name}: upstream -> oc-rsync daemon (exit $?)"
    }
    if [[ -n "${compare_fn}" ]]; then
      "${compare_fn}" "${scenario_src}" "${workdir}/data-oc" \
        || fail "${name}: upstream -> oc-rsync daemon diverged"
    else
      tree_diff "${scenario_src}" "${workdir}/data-oc" \
        || fail "${name}: upstream -> oc-rsync daemon diverged"
    fi
    pass "${name}: upstream -> oc-rsync daemon"
  else
    echo "SKIP: ${name}: upstream -> oc-rsync daemon (unsupported on Windows)"
  fi
}

# --- Scenario 8: compressed transfer (-avz) ----------------------------
run_extended_scenario "compress" "-avz"

# --- Scenario 9: checksum mode (-avc) ----------------------------------
run_extended_scenario "checksum" "-avc"

# --- Scenario 10: delete (--delete) ------------------------------------
# For --delete, we need to pre-populate the destination before the
# transfer so that the delete phase actually runs.
if [[ "${up_daemon_available}" == "true" ]]; then
  reset_module_data "up"
  printf 'stale\n' > "${workdir}/data-up/stale-file.txt"
  # shellcheck disable=SC2086
  run_wire "${OC_RSYNC}" -av --delete "${src}/" "${up_url}/" \
    || fail "delete: oc-rsync -> upstream daemon failed"
  if [[ -f "${workdir}/data-up/stale-file.txt" ]]; then
    fail "delete: stale file not removed by oc-rsync -> upstream daemon"
  fi
  tree_diff "${src}" "${workdir}/data-up" \
    || fail "delete: oc-rsync -> upstream daemon diverged"
  pass "delete: oc-rsync -> upstream daemon"
else
  echo "SKIP: delete: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
fi

if [[ "${host_os}" != "windows" ]]; then
  reset_module_data "oc"
  printf 'stale\n' > "${workdir}/data-oc/stale-file.txt"
  run_wire "${UPSTREAM_RSYNC}" -av --delete "${src}/" "${oc_url}/" \
    || fail "delete: upstream -> oc-rsync daemon failed"
  if [[ -f "${workdir}/data-oc/stale-file.txt" ]]; then
    fail "delete: stale file not removed by upstream -> oc-rsync daemon"
  fi
  tree_diff "${src}" "${workdir}/data-oc" \
    || fail "delete: upstream -> oc-rsync daemon diverged"
  pass "delete: upstream -> oc-rsync daemon"
else
  echo "SKIP: delete: upstream -> oc-rsync daemon (unsupported on Windows)"
fi

# --- Scenario 11: dry run (-avn) ---------------------------------------
# Dry run should not create any files in the destination.
if [[ "${up_daemon_available}" == "true" ]]; then
  reset_module_data "up"
  run_wire "${OC_RSYNC}" -avn "${src}/" "${up_url}/" || true
  count="$(find "${workdir}/data-up" -type f 2>/dev/null | wc -l | tr -d ' ')"
  if [[ "${count}" -ne 0 ]]; then
    fail "dry-run: oc-rsync -> upstream daemon created files"
  fi
  pass "dry-run: oc-rsync -> upstream daemon (no files created)"
else
  echo "SKIP: dry-run: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
fi

# --- Scenario 12: exclude filter (--exclude) ---------------------------
setup_exclude_src() {
  local d=$1
  printf 'should be excluded\n' > "${d}/debug.log"
  printf 'should be excluded too\n' > "${d}/subdir/access.log"
}
exclude_src="${workdir}/src-exclude"
rm -rf "${exclude_src}"
cp -R "${src}" "${exclude_src}"
setup_exclude_src "${exclude_src}"

if [[ "${up_daemon_available}" == "true" ]]; then
  reset_module_data "up"
  run_wire "${OC_RSYNC}" -av --exclude='*.log' "${exclude_src}/" "${up_url}/" \
    || fail "exclude: oc-rsync -> upstream daemon failed"
  if [[ -f "${workdir}/data-up/debug.log" ]] || \
     [[ -f "${workdir}/data-up/subdir/access.log" ]]; then
    fail "exclude: *.log files were transferred by oc-rsync -> upstream"
  fi
  # Verify non-excluded files arrived.
  [[ -f "${workdir}/data-up/hello.txt" ]] \
    || fail "exclude: hello.txt missing after oc-rsync -> upstream"
  pass "exclude: oc-rsync -> upstream daemon"
else
  echo "SKIP: exclude: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
fi

if [[ "${host_os}" != "windows" ]]; then
  reset_module_data "oc"
  run_wire "${UPSTREAM_RSYNC}" -av --exclude='*.log' "${exclude_src}/" "${oc_url}/" \
    || fail "exclude: upstream -> oc-rsync daemon failed"
  if [[ -f "${workdir}/data-oc/debug.log" ]] || \
     [[ -f "${workdir}/data-oc/subdir/access.log" ]]; then
    fail "exclude: *.log files were transferred by upstream -> oc-rsync"
  fi
  [[ -f "${workdir}/data-oc/hello.txt" ]] \
    || fail "exclude: hello.txt missing after upstream -> oc-rsync"
  pass "exclude: upstream -> oc-rsync daemon"
else
  echo "SKIP: exclude: upstream -> oc-rsync daemon (unsupported on Windows)"
fi

# --- Scenario 13: relative paths (-avR) --------------------------------
# Skipped entirely on Windows: MSYS2 path translation mangles the /./
# separator that rsync -R mode requires.
if [[ "${host_os}" == "windows" ]]; then
  echo "SKIP: relative (MSYS2 path translation breaks /./ separator)"
else
  if [[ "${up_daemon_available}" == "true" ]]; then
    reset_module_data "up"
    run_wire "${OC_RSYNC}" -avR "${src}/./subdir/world.txt" "${up_url}/" \
      || fail "relative: oc-rsync -> upstream daemon failed"
    [[ -f "${workdir}/data-up/subdir/world.txt" ]] \
      || fail "relative: subdir/world.txt missing in upstream module"
    pass "relative: oc-rsync -> upstream daemon"
  else
    echo "SKIP: relative: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
  fi

  reset_module_data "oc"
  run_wire "${UPSTREAM_RSYNC}" -avR "${src}/./subdir/world.txt" "${oc_url}/" \
    || fail "relative: upstream -> oc-rsync daemon failed"
  [[ -f "${workdir}/data-oc/subdir/world.txt" ]] \
    || fail "relative: subdir/world.txt missing in oc-rsync module"
  pass "relative: upstream -> oc-rsync daemon"
fi

# --- Scenario 14: whole-file mode (-avW) --------------------------------
run_extended_scenario "whole-file" "-avW"

# --- Scenario 15: inplace mode (--inplace) ------------------------------
# KNOWN FAILURE (upstream rsync, NOT oc-rsync): a non-chroot daemon on any
# platform without openat2 / RESOLVE_BENEATH - including Cygwin/Windows, the
# *BSDs and Solaris - takes upstream's portable secure_relative_open() fallback,
# which re-walks every path component with openat(O_RDONLY|O_DIRECTORY|O_NOFOLLOW)
# and only rescues ENOTDIR. A NEW destination file returns ENOENT from that
# probe, which the fallback does not handle, so upstream's --inplace receiver
# cannot create new files and fails with `open "<f>" (in data) failed: No such
# file or directory`, leaving the module empty. Proven on the Windows runner by
# an upstream-client -> upstream-daemon --inplace push diverging identically.
# oc-rsync's sender has no --inplace code (it forwards the flag verbatim) and
# cannot influence the upstream server; oc-rsync's own secure-open opens the leaf
# directly with O_CREAT and is unaffected (--inplace passes on Linux and macOS).
# On Windows, exercise the same transfer without --inplace so content parity is
# still validated.
if [[ "${host_os}" == "windows" ]]; then
  echo "KNOWN-FAILURE: inplace: --inplace not exercised on Windows - upstream daemon's portable secure_relative_open() cannot create new files (receiver.c); running -av for content parity"
  run_extended_scenario "inplace" "-av"
else
  run_extended_scenario "inplace" "-av --inplace"
fi

# --- Scenario 16: numeric-ids (--numeric-ids) --------------------------
run_extended_scenario "numeric-ids" "-av --numeric-ids"

# --- Scenario 17: itemize (-avi) ----------------------------------------
# Verify the transfer succeeds; exact itemize output format is not
# compared cross-implementation, only content parity.
run_extended_scenario "itemize" "-avi"

# --- Scenario 18: symlinks (macOS / Linux only) -------------------------
if [[ "${host_os}" != "windows" ]]; then
  symlink_src="${workdir}/src-symlinks"
  rm -rf "${symlink_src}"
  cp -R "${src}" "${symlink_src}"
  printf 'link target\n' > "${symlink_src}/real-file.txt"
  ln -s real-file.txt "${symlink_src}/link.txt"

  if [[ "${up_daemon_available}" == "true" ]]; then
    reset_module_data "up"
    run_wire "${OC_RSYNC}" -av "${symlink_src}/" "${up_url}/" \
      || fail "symlinks: oc-rsync -> upstream daemon failed"
    if [[ ! -L "${workdir}/data-up/link.txt" ]]; then
      fail "symlinks: link.txt not a symlink in upstream module"
    fi
    target="$(readlink "${workdir}/data-up/link.txt")"
    [[ "${target}" == "real-file.txt" ]] \
      || fail "symlinks: link.txt points to '${target}', expected 'real-file.txt'"
    pass "symlinks: oc-rsync -> upstream daemon"
  else
    echo "SKIP: symlinks: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
  fi

  reset_module_data "oc"
  run_wire "${UPSTREAM_RSYNC}" -av "${symlink_src}/" "${oc_url}/" \
    || fail "symlinks: upstream -> oc-rsync daemon failed"
  if [[ ! -L "${workdir}/data-oc/link.txt" ]]; then
    fail "symlinks: link.txt not a symlink in oc-rsync module"
  fi
  target="$(readlink "${workdir}/data-oc/link.txt")"
  [[ "${target}" == "real-file.txt" ]] \
    || fail "symlinks: link.txt points to '${target}', expected 'real-file.txt'"
  pass "symlinks: upstream -> oc-rsync daemon"
else
  echo "SKIP: symlinks (unsupported on Windows)"
fi

# --- Scenario 19: hardlinks (macOS / Linux only) -----------------------
if [[ "${host_os}" != "windows" ]]; then
  hardlink_src="${workdir}/src-hardlinks"
  rm -rf "${hardlink_src}"
  mkdir -p "${hardlink_src}"
  printf 'shared content\n' > "${hardlink_src}/original.txt"
  ln "${hardlink_src}/original.txt" "${hardlink_src}/hardlink.txt"

  if [[ "${up_daemon_available}" == "true" ]]; then
    reset_module_data "up"
    run_wire "${OC_RSYNC}" -avH "${hardlink_src}/" "${up_url}/" \
      || fail "hardlinks: oc-rsync -> upstream daemon failed"
    # Verify both files exist with identical content.
    [[ -f "${workdir}/data-up/original.txt" ]] \
      || fail "hardlinks: original.txt missing in upstream module"
    [[ -f "${workdir}/data-up/hardlink.txt" ]] \
      || fail "hardlinks: hardlink.txt missing in upstream module"
    # Verify they are actually hardlinked (same inode).
    ino1="$(stat -f '%i' "${workdir}/data-up/original.txt" 2>/dev/null \
      || stat -c '%i' "${workdir}/data-up/original.txt" 2>/dev/null)"
    ino2="$(stat -f '%i' "${workdir}/data-up/hardlink.txt" 2>/dev/null \
      || stat -c '%i' "${workdir}/data-up/hardlink.txt" 2>/dev/null)"
    [[ "${ino1}" == "${ino2}" ]] \
      || fail "hardlinks: inodes differ (${ino1} vs ${ino2}) in upstream module"
    pass "hardlinks: oc-rsync -> upstream daemon"
  else
    echo "SKIP: hardlinks: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
  fi

  reset_module_data "oc"
  run_wire "${UPSTREAM_RSYNC}" -avH "${hardlink_src}/" "${oc_url}/" \
    || fail "hardlinks: upstream -> oc-rsync daemon failed"
  ino1="$(stat -f '%i' "${workdir}/data-oc/original.txt" 2>/dev/null \
    || stat -c '%i' "${workdir}/data-oc/original.txt" 2>/dev/null)"
  ino2="$(stat -f '%i' "${workdir}/data-oc/hardlink.txt" 2>/dev/null \
    || stat -c '%i' "${workdir}/data-oc/hardlink.txt" 2>/dev/null)"
  [[ "${ino1}" == "${ino2}" ]] \
    || fail "hardlinks: inodes differ (${ino1} vs ${ino2}) in oc-rsync module"
  pass "hardlinks: upstream -> oc-rsync daemon"
else
  echo "SKIP: hardlinks (unsupported on Windows)"
fi

# --- Scenario 20: --files-from -----------------------------------------
files_list="${workdir}/filelist.txt"
printf 'hello.txt\nsubdir/world.txt\n' > "${files_list}"

if [[ "${up_daemon_available}" == "true" ]]; then
  reset_module_data "up"
  run_wire "${OC_RSYNC}" -av --files-from="${files_list}" "${src}/" "${up_url}/" \
    || fail "files-from: oc-rsync -> upstream daemon failed"
  [[ -f "${workdir}/data-up/hello.txt" ]] \
    || fail "files-from: hello.txt missing in upstream module"
  [[ -f "${workdir}/data-up/subdir/world.txt" ]] \
    || fail "files-from: subdir/world.txt missing in upstream module"
  # random.bin and empty.txt should NOT be transferred.
  if [[ -f "${workdir}/data-up/random.bin" ]]; then
    fail "files-from: random.bin should not have been transferred"
  fi
  pass "files-from: oc-rsync -> upstream daemon"
else
  echo "SKIP: files-from: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
fi

if [[ "${host_os}" != "windows" ]]; then
  reset_module_data "oc"
  run_wire "${UPSTREAM_RSYNC}" -av --files-from="${files_list}" "${src}/" "${oc_url}/" \
    || fail "files-from: upstream -> oc-rsync daemon failed"
  [[ -f "${workdir}/data-oc/hello.txt" ]] \
    || fail "files-from: hello.txt missing in oc-rsync module"
  [[ -f "${workdir}/data-oc/subdir/world.txt" ]] \
    || fail "files-from: subdir/world.txt missing in oc-rsync module"
  if [[ -f "${workdir}/data-oc/random.bin" ]]; then
    fail "files-from: random.bin should not have been transferred"
  fi
  pass "files-from: upstream -> oc-rsync daemon"
else
  echo "SKIP: files-from: upstream -> oc-rsync daemon (unsupported on Windows)"
fi

# --- Scenario 21: --size-only ------------------------------------------
size_only_src="${workdir}/src-size-only"
rm -rf "${size_only_src}"
cp -R "${src}" "${size_only_src}"

if [[ "${up_daemon_available}" == "true" ]]; then
  reset_module_data "up"
  # First sync to populate destination.
  run_wire "${OC_RSYNC}" -av "${size_only_src}/" "${up_url}/" \
    || fail "size-only: oc-rsync -> upstream daemon initial sync failed"
  # Modify content but keep the same size.
  printf 'XXXXX\n' > "${size_only_src}/hello.txt"
  # Re-sync with --size-only: since the size matches (6 bytes both ways),
  # the file should NOT be transferred.
  out="$(run_wire "${OC_RSYNC}" -av --size-only --stats "${size_only_src}/" "${up_url}/")"
  if ! printf '%s\n' "${out}" \
       | grep -qE 'Total transferred file size: 0( bytes)?'; then
    printf '%s\n' "${out}" >&2
    fail "size-only: oc-rsync transferred data when sizes matched"
  fi
  pass "size-only: oc-rsync -> upstream daemon"
else
  echo "SKIP: size-only: oc-rsync -> upstream daemon (upstream daemon unavailable on Windows)"
fi

# --- Scenario 22: compressed delta transfer (-avz --no-whole-file -I) ---
run_extended_scenario "compress-delta" "-avz --no-whole-file -I"

echo "All interop smoke scenarios passed on ${host_os}."
