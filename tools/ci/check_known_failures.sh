#!/usr/bin/env bash
# Known Failures Tracking Dashboard
# Runs each KNOWN_FAILURES entry from run_interop.sh individually and reports
# which ones still fail and which have been fixed. Always exits 0 (informational).
set -uo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
oc_bin="${workspace_root}/target/dist/oc-rsync"
interop_script="${workspace_root}/tools/ci/run_interop.sh"

# Build oc-rsync if not already built
if [[ ! -x "$oc_bin" ]]; then
  echo "Building oc-rsync..."
  cargo build --release --manifest-path "${workspace_root}/Cargo.toml"
  mkdir -p "$(dirname "$oc_bin")"
  cp "${workspace_root}/target/release/oc-rsync" "$oc_bin"
fi

# Ensure upstream rsync binaries are available
upstream_install="${workspace_root}/target/interop/upstream-install"
upstream_src="${workspace_root}/target/interop/upstream-src"

find_upstream_binary() {
  local version=$1
  local candidates=(
    "${upstream_install}/rsync-${version}/bin/rsync"
    "${upstream_src}/rsync-${version}/rsync"
    "$(command -v rsync 2>/dev/null || true)"
  )
  for c in "${candidates[@]}"; do
    if [[ -n "$c" && -x "$c" ]]; then
      echo "$c"
      return 0
    fi
  done
  return 1
}

# Build upstream rsync versions if needed
echo "Ensuring upstream rsync versions are available..."
bash "$interop_script" build-only 2>/dev/null || true

# Source shared known failure definitions.
# shellcheck source=tools/ci/known_failures.conf
source "$(dirname "${BASH_SOURCE[0]}")/known_failures.conf"
ENTRIES=("${DASHBOARD_ENTRIES[@]}")

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

# Port allocation
next_port=18730

alloc_port() {
  local p=$next_port
  next_port=$((next_port + 1))
  echo "$p"
}

hard_timeout=60
up_identity="rsync"

# Minimal source tree for daemon scenario tests
comp_src="${workdir}/src"
mkdir -p "${comp_src}/subdir"
echo "test file 1" > "${comp_src}/file1.txt"
echo "test file 2" > "${comp_src}/file2.txt"
echo "nested" > "${comp_src}/subdir/nested.txt"
dd if=/dev/urandom of="${comp_src}/binary.bin" bs=1024 count=4 2>/dev/null
ln -sf file1.txt "${comp_src}/link1" 2>/dev/null || true

# Daemon config helpers
write_daemon_conf() {
  local conf=$1 pidfile=$2 port=$3 dest=$4 module=$5 binary_name=$6
  mkdir -p "$dest"
  cat > "$conf" <<CONF
port = ${port}
pid file = ${pidfile}
log file = /dev/null
use chroot = no
read only = no

[interop]
  path = ${dest}
  comment = interop test
CONF
}

start_daemon() {
  local binary=$1 conf=$2 logfile=$3 pidfile=$4 port=$5
  "$binary" --daemon --config="$conf" --log-file="$logfile" --no-detach &
  local daemon_pid=$!
  echo "$daemon_pid" > "$pidfile"
  # Wait for port to become available
  for _ in $(seq 1 20); do
    if timeout 1 bash -c "echo >/dev/tcp/127.0.0.1/${port}" 2>/dev/null; then
      return 0
    fi
    sleep 0.25
  done
  return 1
}

stop_daemon() {
  local pidfile=$1
  if [[ -f "$pidfile" ]]; then
    local pid
    pid=$(cat "$pidfile" 2>/dev/null || true)
    if [[ -n "$pid" ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
    rm -f "$pidfile"
  fi
}

# Run a single daemon scenario test
# Returns 0 if the test passes, 1 if it fails
run_daemon_test() {
  local direction=$1 scenario_name=$2

  local version="3.4.1"
  # For protocol-31 with upstream 3.0.9, use that version
  if [[ "$scenario_name" == "protocol-31" && "$direction" == "up" ]]; then
    version="3.0.9"
  fi

  local upstream_binary
  upstream_binary=$(find_upstream_binary "$version") || {
    echo "SKIP:no-binary"
    return 2
  }

  local oc_port upstream_port
  oc_port=$(alloc_port)
  upstream_port=$(alloc_port)

  local tag="${direction}-${scenario_name}"
  local dest_oc="${workdir}/${tag}-oc-dest"
  local dest_up="${workdir}/${tag}-up-dest"
  local conf_oc="${workdir}/${tag}-oc.conf"
  local conf_up="${workdir}/${tag}-up.conf"
  local pid_oc="${workdir}/${tag}-oc.pid"
  local pid_up="${workdir}/${tag}-up.pid"
  local log_oc="${workdir}/${tag}-oc.log"
  local log_up="${workdir}/${tag}-up.log"

  mkdir -p "$dest_oc" "$dest_up"

  # Map scenario names to flags
  local flags=""
  case "$scenario_name" in
    acls)     flags="-avA" ;;
    xattrs)   flags="-avX" ;;
    itemize)  flags="-avi" ;;
    protocol-31) flags="-av --protocol=31" ;;
    *)        flags="-av" ;;
  esac

  local rc=0
  if [[ "$direction" == "up" ]]; then
    # upstream client -> oc-rsync daemon
    write_daemon_conf "$conf_oc" "$pid_oc" "$oc_port" "$dest_oc" "interop" "oc-rsync"
    if ! start_daemon "$oc_bin" "$conf_oc" "$log_oc" "$pid_oc" "$oc_port"; then
      stop_daemon "$pid_oc"
      return 1
    fi
    if ! timeout "$hard_timeout" "$upstream_binary" $flags --timeout=10 \
        "${comp_src}/" "rsync://127.0.0.1:${oc_port}/interop/" \
        >/dev/null 2>&1; then
      rc=1
    fi
    stop_daemon "$pid_oc"
  else
    # oc-rsync client -> upstream daemon
    write_daemon_conf "$conf_up" "$pid_up" "$upstream_port" "$dest_up" "interop" "rsync"
    if ! start_daemon "$upstream_binary" "$conf_up" "$log_up" "$pid_up" "$upstream_port"; then
      stop_daemon "$pid_up"
      return 1
    fi
    if ! timeout "$hard_timeout" "$oc_bin" $flags --timeout=10 \
        "${comp_src}/" "rsync://127.0.0.1:${upstream_port}/interop/" \
        >/dev/null 2>&1; then
      rc=1
    fi
    stop_daemon "$pid_up"
  fi

  return $rc
}

# Run a standalone test by name
run_standalone_test_by_name() {
  local name=$1
  local upstream_binary
  upstream_binary=$(find_upstream_binary "3.4.1") || {
    echo "SKIP:no-binary"
    return 2
  }

  local tag="standalone-${name}"
  local dest="${workdir}/${tag}"
  mkdir -p "$dest"

  case "$name" in
    write-batch-read-batch)
      local batch_dir="${dest}/batch"
      local dest1="${batch_dir}/dest1"
      local dest2="${batch_dir}/dest2"
      local batch_file="${batch_dir}/batch.rsync"
      mkdir -p "$dest1" "$dest2"
      # upstream writes batch
      if ! timeout "$hard_timeout" "$upstream_binary" -av \
          --write-batch="$batch_file" --timeout=10 \
          "${comp_src}/" "${dest1}/" >/dev/null 2>&1; then
        return 1
      fi
      # oc-rsync reads batch
      if ! timeout "$hard_timeout" "$oc_bin" -av \
          --read-batch="$batch_file" --timeout=10 \
          "${dest2}/" >/dev/null 2>&1; then
        return 1
      fi
      # Verify
      diff -rq "$comp_src" "$dest2" >/dev/null 2>&1 || return 1
      ;;
    info-progress2)
      local pdest="${dest}/progress"
      mkdir -p "$pdest"
      timeout "$hard_timeout" "$oc_bin" -av --info=progress2 --timeout=10 \
          "${comp_src}/" "${pdest}/" >/dev/null 2>&1 || return 1
      ;;
    large-file-2gb)
      # Skip in dashboard - too resource-intensive for a tracking check
      return 2
      ;;
    file-vanished)
      local vdest="${dest}/vanished"
      local vsrc="${dest}/vsrc"
      mkdir -p "$vdest" "$vsrc"
      cp -r "${comp_src}/"* "$vsrc/"
      # Create a file that will be removed during transfer
      echo "ephemeral" > "${vsrc}/vanish.txt"
      # Remove it to simulate vanishing
      rm -f "${vsrc}/vanish.txt"
      timeout "$hard_timeout" "$oc_bin" -av --timeout=10 \
          "${vsrc}/" "${vdest}/" >/dev/null 2>&1 || return 1
      ;;
    iconv)
      local idest="${dest}/iconv"
      mkdir -p "$idest"
      timeout "$hard_timeout" "$oc_bin" -av --iconv=utf8,latin1 --timeout=10 \
          "${comp_src}/" "${idest}/" >/dev/null 2>&1 || return 1
      ;;
    iconv-upstream)
      # Reproducer for #1916: --iconv UTF-8/LATIN1 round-trip vs upstream
      # rsync 3.4.1. Mirrors the standalone harness scenario but with a
      # minimal fixture so the dashboard can rerun quickly.
      local iu_src="${dest}/iconv-up-src"
      local iu_dest_oc="${dest}/iconv-up-dest-oc"
      local iu_conf="${dest}/iconv-up-oc.conf"
      local iu_pid="${dest}/iconv-up-oc.pid"
      local iu_log="${dest}/iconv-up-oc.log"
      local iu_port iu_rc=0
      iu_port=$(alloc_port)
      mkdir -p "$iu_src" "$iu_dest_oc"
      echo "ascii body" > "${iu_src}/plain.txt"
      echo "cafe body" > "${iu_src}/café.txt" 2>/dev/null || return 2
      [[ -f "${iu_src}/café.txt" ]] || return 2
      cat > "$iu_conf" <<CONF
pid file = ${iu_pid}
port = ${iu_port}
use chroot = false

[interop]
path = ${iu_dest_oc}
read only = false
numeric ids = yes
charset = ISO-8859-1
CONF
      if ! start_daemon "$oc_bin" "$iu_conf" "$iu_log" "$iu_pid" "$iu_port"; then
        stop_daemon "$iu_pid"
        return 1
      fi
      timeout "$hard_timeout" "$upstream_binary" -av \
          --iconv=UTF-8,ISO-8859-1 --timeout=10 \
          "${iu_src}/" "rsync://127.0.0.1:${iu_port}/interop" \
          >/dev/null 2>&1 || iu_rc=$?
      stop_daemon "$iu_pid"
      [[ $iu_rc -eq 0 ]] || return 1
      [[ -f "${iu_dest_oc}/café.txt" ]] || return 1
      cmp -s "${iu_src}/café.txt" "${iu_dest_oc}/café.txt" || return 1
      ;;
    upstream-compressed-batch-self-roundtrip)
      # upstream rsync 3.4.1 cannot read back its own compressed delta batch
      # files. The batch writer records raw compressed tokens (zlib DEFLATED_DATA)
      # but the batch reader's inflate context lacks the dictionary sync that
      # see_deflate_token() provides during live transfers, causing "inflate
      # returned -3" at token.c:608.
      #
      # This test verifies the upstream bug still exists by:
      # 1. Having upstream write a compressed delta batch
      # 2. Having upstream try to read its own batch (expected to fail)
      # 3. Having oc-rsync read the same batch (expected to succeed)
      #
      # upstream: token.c:608 - inflate fails without dictionary sync
      # upstream: compat.c:194-195 - batch read defaults to CPRES_ZLIB
      local bdir="${dest}/batch-delta"
      local bsrc="${bdir}/src"
      local bwrite="${bdir}/write-dest"
      local bread="${bdir}/read-dest"
      local oc_read="${bdir}/oc-read-dest"
      local bfile="${bdir}/batch.rsync"
      mkdir -p "$bsrc" "$bwrite" "$bread" "$oc_read"

      # Create basis + modified source for delta transfer
      dd if=/dev/zero bs=1K count=100 2>/dev/null | tr '\0' 'B' > "$bsrc/data.bin"
      printf 'CHANGED_DATA_HERE' | dd of="$bsrc/data.bin" bs=1 seek=40000 conv=notrunc 2>/dev/null
      dd if=/dev/zero bs=1K count=100 2>/dev/null | tr '\0' 'B' > "$bwrite/data.bin"
      cp "$bwrite/data.bin" "$bread/data.bin"
      cp "$bwrite/data.bin" "$oc_read/data.bin"

      # Step 1: upstream writes compressed delta batch
      if ! timeout "$hard_timeout" "$upstream_binary" -avI -z --no-whole-file \
          --compress-choice=zlib --write-batch="$bfile" --timeout=10 \
          "${bsrc}/" "${bwrite}/" >/dev/null 2>&1; then
        return 1
      fi

      # Step 2: upstream reads its own batch - expected to fail (upstream bug)
      local up_rc=0
      timeout "$hard_timeout" "$upstream_binary" -av \
          --read-batch="$bfile" --timeout=10 \
          "${bread}/" >/dev/null 2>&1 || up_rc=$?

      # Step 3: oc-rsync reads the same batch - expected to succeed
      local oc_rc=0
      timeout "$hard_timeout" "$oc_bin" -av \
          --read-batch="$bfile" --timeout=10 \
          "${oc_read}/" >/dev/null 2>&1 || oc_rc=$?

      if [[ $oc_rc -ne 0 ]]; then
        echo "    oc-rsync cannot read upstream compressed delta batch (regression)"
        return 1
      fi

      if ! cmp -s "${bsrc}/data.bin" "${oc_read}/data.bin"; then
        echo "    oc-rsync content mismatch after reading upstream compressed delta batch"
        return 1
      fi

      # The test "fails" if upstream still cannot read its own batch -
      # which is the expected upstream bug we are tracking.
      if [[ $up_rc -ne 0 ]]; then
        return 1
      fi

      # Verify upstream content match if it somehow succeeds
      cmp -s "${bsrc}/data.bin" "${bread}/data.bin" || return 1
      ;;
    *)
      return 2  # Unknown test, skip
      ;;
  esac
  return 0
}

# --- Main ---

echo "=========================================="
echo " Known Failures Tracking Dashboard"
echo " Date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "=========================================="
echo ""

declare -a results_key=()
declare -a results_desc=()
declare -a results_status=()

for entry in "${ENTRIES[@]}"; do
  IFS='|' read -r key description method <<< "$entry"
  IFS=':' read -r direction name <<< "$key"

  echo -n "Testing ${key}... "

  rc=0
  if [[ "$method" == "daemon" ]]; then
    run_daemon_test "$direction" "$name" || rc=$?
  else
    run_standalone_test_by_name "$name" || rc=$?
  fi

  if [[ $rc -eq 0 ]]; then
    status="FIXED"
    echo "FIXED (now passing)"
  elif [[ $rc -eq 2 ]]; then
    status="SKIPPED"
    echo "SKIPPED"
  else
    status="FAILING"
    echo "still failing"
  fi

  results_key+=("$key")
  results_desc+=("$description")
  results_status+=("$status")
done

echo ""
echo "=========================================="
echo " Summary"
echo "=========================================="

fixed=0
failing=0
skipped=0
for s in "${results_status[@]}"; do
  case "$s" in
    FIXED) fixed=$((fixed + 1)) ;;
    FAILING) failing=$((failing + 1)) ;;
    SKIPPED) skipped=$((skipped + 1)) ;;
  esac
done

total=${#results_status[@]}
echo "Total: ${total} | Fixed: ${fixed} | Still failing: ${failing} | Skipped: ${skipped}"
echo ""

# Output markdown summary for GitHub Actions job summary
output_markdown() {
  echo "## Known Failures Dashboard"
  echo ""
  echo "**Date:** $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo ""
  echo "| Status | Key | Description |"
  echo "|--------|-----|-------------|"
  for i in "${!results_key[@]}"; do
    local icon
    case "${results_status[$i]}" in
      FIXED)   icon="✅ Fixed" ;;
      FAILING) icon="❌ Failing" ;;
      SKIPPED) icon="⏭️ Skipped" ;;
    esac
    echo "| ${icon} | \`${results_key[$i]}\` | ${results_desc[$i]} |"
  done
  echo ""
  echo "**Summary:** ${fixed} fixed, ${failing} still failing, ${skipped} skipped out of ${total} tracked failures"
  echo ""
  if [[ $fixed -gt 0 ]]; then
    echo "> **Action needed:** ${fixed} known failure(s) are now passing. Consider removing them from \`tools/ci/known_failures.conf\`."
  fi
}

# Write markdown to file for GitHub Actions
markdown_output="${GITHUB_STEP_SUMMARY:-${workdir}/summary.md}"
output_markdown > "$markdown_output"

# Also print the markdown to stdout
echo "--- Markdown Summary ---"
output_markdown

# Always exit 0 - this is informational only
exit 0
