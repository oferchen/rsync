#!/usr/bin/env bash
# Ubuntu/Debian-first rsync interop harness
# - Detects platform architecture and aligns Debian/Ubuntu package arch names
# - Tries real, validated package locations for:
#     3.0.9  -> old-releases.ubuntu.com
#     3.1.3  -> archive.ubuntu.com
#     3.4.1  -> deb.debian.org (3.4.1+ds1-6)
# - Falls back to source build if the exact .deb for this arch is missing
# - Starts oc-rsync --daemon on a non-privileged port by passing --port on the CLI
set -euo pipefail

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to fetch Ubuntu/Debian rsync packages" >&2
  exit 1
fi

if ! command -v tar >/dev/null 2>&1; then
  echo "tar is required to unpack upstream rsync releases" >&2
  exit 1
fi

export GIT_TERMINAL_PROMPT=0

# Retry helper for network operations with exponential backoff
retry_curl() {
  local url=$1
  local output=$2
  local max_attempts=${3:-3}
  local attempt=1

  while [ $attempt -le $max_attempts ]; do
    if curl -fsSL --connect-timeout 30 --max-time 120 "$url" -o "$output"; then
      return 0
    fi
    echo "Attempt $attempt/$max_attempts failed for $url" >&2
    if [ $attempt -lt $max_attempts ]; then
      local delay=$((attempt * 5))
      echo "Retrying in ${delay}s..." >&2
      sleep $delay
    fi
    attempt=$((attempt + 1))
  done
  return 1
}

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target_dir="${workspace_root}/target/dist"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_install_root="${workspace_root}/target/interop/upstream-install"
interop_log_dir="${workspace_root}/target/interop/logs"

# Versions we test against
versions=(3.0.9 3.1.3 3.4.1)
rsync_repo_url="https://github.com/RsyncProject/rsync.git"
rsync_tarball_base_url="${RSYNC_TARBALL_BASE_URL:-https://rsync.samba.org/ftp/rsync/src}"

# Mirrors (can be overridden in CI)
DEBIAN_MIRROR="${DEBIAN_MIRROR:-https://deb.debian.org/debian}"
UBUNTU_MIRROR="${UBUNTU_MIRROR:-http://archive.ubuntu.com/ubuntu}"
OLD_UBUNTU_MIRROR="${OLD_UBUNTU_MIRROR:-https://old-releases.ubuntu.com/ubuntu}"

oc_pid=""
up_pid=""
oc_pid_file_current=""
up_pid_file_current=""
workdir=""
hard_timeout=30

detect_deb_arch() {
  local u
  u=$(uname -m)
  case "$u" in
    x86_64)  echo "amd64" ;;
    aarch64) echo "arm64" ;;
    armv7l)  echo "armhf" ;;
    armv6l)  echo "armhf" ;;
    i386|i686) echo "i386" ;;
    ppc64le) echo "ppc64el" ;;
    riscv64) echo "riscv64" ;;
    *)
      echo "amd64"
      ;;
  esac
}

ensure_workspace_binaries() {
  if [[ -x "${target_dir}/oc-rsync" ]]; then
    return
  fi
  cargo build --profile dist --bin oc-rsync
}

build_jobs() {
  if command -v nproc >/dev/null 2>&1; then
    nproc
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.ncpu
  else
    echo 2
  fi
}

# Build the most realistic URL for this version+arch using actual distro naming
build_version_url() {
  local version=$1
  local arch=$2
  case "$version" in
    3.0.9)
      echo "${OLD_UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.0.9-1ubuntu1.3_${arch}.deb"
      ;;
    3.1.3)
      echo "${UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.1.3-8ubuntu0.9_${arch}.deb"
      ;;
    3.4.1)
      echo "${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_3.4.1+ds1-6_${arch}.deb"
      ;;
    *)
      echo "${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_${version}-1_${arch}.deb"
      ;;
  esac
}

try_fetch_deb() {
  local url=$1
  local install_dir=$2

  local tmp_deb
  tmp_deb=$(mktemp)
  if ! retry_curl "$url" "$tmp_deb"; then
    rm -f "$tmp_deb"
    return 1
  fi

  if ! command -v ar >/dev/null 2>&1; then
    echo "ar not available; cannot extract .deb from ${url}, will fall back to source" >&2
    rm -f "$tmp_deb"
    return 1
  fi

  mkdir -p "${install_dir}"
  if ! (
    cd "${install_dir}"
    if ! ar x "$tmp_deb" >/dev/null 2>&1; then
      echo "failed to extract archive payload from ${tmp_deb}" >&2
      exit 1
    fi
    if [[ -f data.tar.xz ]]; then
      tar -xf data.tar.xz
      rm -f data.tar.xz
    elif [[ -f data.tar.gz ]]; then
      tar -xzf data.tar.gz
      rm -f data.tar.gz
    else
      echo "unexpected .deb layout: missing data.tar archive" >&2
      exit 1
    fi
  ); then
    rm -f "$tmp_deb"
    return 1
  fi
  rm -f "$tmp_deb"

  if [[ -x "${install_dir}/usr/bin/rsync" ]]; then
    mkdir -p "${install_dir}/bin"
    cp "${install_dir}/usr/bin/rsync" "${install_dir}/bin/rsync"
    return 0
  fi

  return 1
}

try_fetch_deb_generic() {
  local version=$1
  local arch=$2
  local install_dir=$3
  local tmp_deb
  tmp_deb=$(mktemp)

  local candidates=()

  case "$version" in
    3.0.9)
      candidates+=(
        "${OLD_UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.0.9-1ubuntu1_${arch}.deb"
        "${OLD_UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.0.9-1ubuntu1.1_${arch}.deb"
        "${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_3.0.9-4_${arch}.deb"
      )
      ;;
    3.1.3)
      candidates+=(
        "${UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.1.3-8ubuntu0.8_${arch}.deb"
      )
      ;;
    3.4.1)
      candidates+=(
        "${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_3.4.1+ds1-5_${arch}.deb"
      )
      ;;
    *)
      candidates+=(
        "${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_${version}-1_${arch}.deb"
      )
      ;;
  esac

  for url in "${candidates[@]}"; do
    if retry_curl "$url" "$tmp_deb" 2>/dev/null; then
      if ! command -v ar >/dev/null 2>&1; then
        rm -f "$tmp_deb"
        return 1
      fi
      mkdir -p "${install_dir}"
      if ! (
        cd "${install_dir}"
        if ! ar x "$tmp_deb" >/dev/null 2>&1; then
          echo "failed to extract archive payload from ${tmp_deb}" >&2
          exit 1
        fi
        if [[ -f data.tar.xz ]]; then
          tar -xf data.tar.xz
          rm -f data.tar.xz
        elif [[ -f data.tar.gz ]]; then
          tar -xzf data.tar.gz
          rm -f data.tar.gz
        else
          echo "unexpected .deb layout: missing data.tar archive" >&2
          exit 1
        fi
      ); then
        rm -f "$tmp_deb"
        return 1
      fi
      rm -f "$tmp_deb"
      if [[ -x "${install_dir}/usr/bin/rsync" ]]; then
        mkdir -p "${install_dir}/bin"
        cp "${install_dir}/usr/bin/rsync" "${install_dir}/bin/rsync"
        return 0
      fi
      return 1
    fi
  done

  rm -f "$tmp_deb"
  return 1
}

clone_upstream_source() {
  local version=$1
  local destination=$2
  if ! command -v git >/dev/null 2>&1; then
    return 1
  fi
  local tag_candidates=("v${version}" "${version}")

  for tag in "${tag_candidates[@]}"; do
    if git clone --depth 1 --branch "$tag" "$rsync_repo_url" "$destination" >/dev/null 2>&1; then
      return 0
    fi
  done
  return 1
}

fetch_upstream_tarball() {
  local version=$1
  local destination=$2
  local tarball_url="${rsync_tarball_base_url}/rsync-${version}.tar.gz"
  local tmp_tar
  tmp_tar=$(mktemp)

  if ! retry_curl "$tarball_url" "$tmp_tar"; then
    rm -f "$tmp_tar"
    return 1
  fi

  mkdir -p "$upstream_src_root"
  rm -rf "$destination" "${upstream_src_root}/rsync-${version}"

  if ! tar -xzf "$tmp_tar" -C "$upstream_src_root" >/dev/null 2>&1; then
    rm -f "$tmp_tar"
    rm -rf "$destination"
    return 1
  fi

  rm -f "$tmp_tar"

  if [[ -d "$destination" ]]; then
    return 0
  fi

  rm -rf "$destination"
  return 1
}

build_upstream_from_source() {
  local version=$1
  local src_dir="${upstream_src_root}/rsync-${version}"
  local install_dir="${upstream_install_root}/${version}"
  local build_log="${interop_log_dir}/rsync-${version}-build.log"

  rm -rf "$src_dir"
  mkdir -p "$upstream_src_root" "$upstream_install_root"
  mkdir -p "$interop_log_dir"
  rm -f "$build_log"

  echo "Fetching upstream rsync ${version} release tarball from ${rsync_tarball_base_url} (log: ${build_log})"
  if ! fetch_upstream_tarball "$version" "$src_dir"; then
    echo "Falling back to cloning upstream rsync ${version} from ${rsync_repo_url}" >&2
    if ! clone_upstream_source "$version" "$src_dir"; then
      echo "Failed to obtain upstream rsync ${version} sources" >&2
      exit 1
    fi
  fi

  pushd "$src_dir" >/dev/null

  if [[ ! -x configure ]]; then
    if [[ -x ./prepare-source ]]; then
      ./prepare-source >/dev/null
    fi
  fi

  if [[ ! -x configure ]]; then
    echo "Upstream rsync ${version} source tree is missing a configure script" >&2
    exit 1
  fi

  local configure_help
  configure_help=$(./configure --help)
  local -a configure_args=("--prefix=${install_dir}")

  if grep -q -- "--disable-xxhash" <<<"$configure_help"; then
    configure_args+=("--disable-xxhash")
  fi
  if grep -q -- "--disable-lz4" <<<"$configure_help"; then
    configure_args+=("--disable-lz4")
  fi
  if grep -q -- "--disable-md2man" <<<"$configure_help"; then
    configure_args+=("--disable-md2man")
  fi

  if ! ./configure "${configure_args[@]}" >"${build_log}" 2>&1; then
    echo "Upstream rsync ${version} configure failed; see ${build_log}" >&2
    tail -n 50 "${build_log}" >&2 || true
    exit 1
  fi

  if ! make -j"$(build_jobs)" >>"${build_log}" 2>&1; then
    echo "Upstream rsync ${version} build failed; see ${build_log}" >&2
    tail -n 50 "${build_log}" >&2 || true
    exit 1
  fi

  if ! make install >>"${build_log}" 2>&1; then
    echo "Upstream rsync ${version} install failed; see ${build_log}" >&2
    tail -n 50 "${build_log}" >&2 || true
    exit 1
  fi

  popd >/dev/null
}

ensure_upstream_build() {
  local version=$1
  local install_dir="${upstream_install_root}/${version}"
  local binary="${install_dir}/bin/rsync"
  local arch="${DEB_ARCH:-$(detect_deb_arch)}"

  if [[ -x "$binary" ]]; then
    if "$binary" --version | head -n1 | grep -q "rsync\s\+version\s\+${version}\b"; then
      return
    fi
    rm -rf "$install_dir"
  fi

  mkdir -p "$install_dir"

  local url
  url=$(build_version_url "$version" "$arch")
  echo "Trying ${url}"
  if try_fetch_deb "$url" "$install_dir"; then
    if "${install_dir}/bin/rsync" --version | head -n1 | grep -q "rsync\s\+version\s\+${version}\b"; then
      echo "Using rsync ${version} from ${url}"
      return
    else
      echo "Version mismatch for ${url}, discarding"
      rm -rf "$install_dir"
      mkdir -p "$install_dir"
    fi
  fi

  echo "Trying generic pool for ${version} (${arch}) ..."
  if try_fetch_deb_generic "$version" "$arch" "$install_dir"; then
    if "${install_dir}/bin/rsync" --version | head -n1 | grep -q "rsync\s\+version\s\+${version}\b"; then
      echo "Using rsync ${version} from generic pool"
      return
    else
      echo "Version mismatch for generic pool, discarding"
      rm -rf "$install_dir"
    fi
  fi

  echo "No suitable .deb found for rsync ${version} (${arch}); building from source ..."
  build_upstream_from_source "$version"
}

write_rust_daemon_conf() {
  local path=$1
  local pid_file=$2
  local port=$3
  local dest=$4
  local comment=$5

  cat >"$path" <<CONF
pid file = ${pid_file}
port = ${port}
use chroot = false
numeric ids = yes

[interop]
path = ${dest}
comment = ${comment}
read only = false
CONF
}

write_upstream_conf() {
  local path=$1
  local pid_file=$2
  local port=$3
  local dest=$4
  local comment=$5
  local identity=$6

  cat >"$path" <<CONF
pid file = ${pid_file}
port = ${port}
use chroot = false
${identity}numeric ids = yes
[interop]
    path = ${dest}
    comment = ${comment}
    read only = false
CONF
}

stop_oc_daemon() {
  if [[ -n "${oc_pid}" ]]; then
    kill "${oc_pid}" >/dev/null 2>&1 || true
    wait "${oc_pid}" >/dev/null 2>&1 || true
    oc_pid=""
  fi
  if [[ -n "${oc_pid_file_current:-}" ]]; then
    rm -f "${oc_pid_file_current}"
    oc_pid_file_current=""
  fi
}

stop_upstream_daemon() {
  if [[ -n "${up_pid}" ]]; then
    kill "${up_pid}" >/dev/null 2>&1 || true
    wait "${up_pid}" >/dev/null 2>&1 || true
    up_pid=""
  fi
  if [[ -n "${up_pid_file_current:-}" ]]; then
    rm -f "${up_pid_file_current}"
    up_pid_file_current=""
  fi
}

cleanup() {
  local exit_code=$?
  stop_oc_daemon
  stop_upstream_daemon
  if [[ -n "${workdir:-}" && -d "${workdir:-}" ]]; then
    rm -rf "${workdir}"
  fi
  exit "$exit_code"
}

# Wait for a TCP port to become reachable, with timeout.
wait_for_port() {
  local port=$1
  local max_wait=${2:-10}
  local elapsed=0

  while [ $elapsed -lt $max_wait ]; do
    if (echo >/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
      return 0
    fi
    sleep 0.5
    elapsed=$((elapsed + 1))
  done
  echo "Warning: port $port not ready after ${max_wait}s" >&2
  return 1
}

# IMPORTANT: oc-rsync --daemon needs the port on CLI, otherwise it binds to 873 (privileged)
# NOTE: Daemon defaults to delegating to system rsync. Set OC_RSYNC_DAEMON_FALLBACK=0
# to force native handling (required for interop testing).
start_oc_daemon() {
  local config=$1
  local log_file=$2
  local fallback_client=$3
  local pid_file=$4
  local port=$5

  oc_pid_file_current="$pid_file"

  RUST_BACKTRACE=1 \
  OC_RSYNC_DAEMON_FALLBACK=0 \
    "$oc_binary" --daemon --config "$config" --port "$port" --log-file "$log_file" &
  oc_pid=$!
  wait_for_port "$port" 10 || true
}

start_upstream_daemon() {
  local binary=$1
  local config=$2
  local log_file=$3
  local pid_file=$4

  up_pid_file_current="$pid_file"
  # Close stdin to prevent SIGPIPE when daemon writes to closed pipe
  "$binary" --daemon --config "$config" --no-detach --log-file "$log_file" </dev/null &
  up_pid=$!

  # Extract port from config for wait_for_port
  local port
  port=$(grep -oP 'port\s*=\s*\K\d+' "$config" 2>/dev/null || echo "")
  if [[ -n "$port" ]]; then
    wait_for_port "$port" 10 || true
  else
    sleep 1
  fi
}

run_interop_case() {
  local version=$1
  local upstream_binary=$2
  local oc_port=$3
  local upstream_port=$4

  local version_tag=${version//./-}
  local oc_dest="${workdir}/oc-destination-${version_tag}"
  local up_dest="${workdir}/upstream-destination-${version_tag}"
  local oc_pid_file="${workdir}/oc-daemon-${version_tag}.pid"
  local up_pid_file="${workdir}/upstream-rsyncd-${version_tag}.pid"
  local oc_conf="${workdir}/oc-daemon-${version_tag}.conf"
  local up_conf="${workdir}/upstream-rsyncd-${version_tag}.conf"
  local oc_log="${workdir}/oc-daemon-${version_tag}.log"
  local up_log="${workdir}/upstream-rsyncd-${version_tag}.log"

  rm -rf "$oc_dest" "$up_dest"
  mkdir -p "$oc_dest" "$up_dest"

  write_rust_daemon_conf "$oc_conf" "$oc_pid_file" "$oc_port" "$oc_dest" "oc interop target (${version})"
  write_upstream_conf "$up_conf" "$up_pid_file" "$upstream_port" "$up_dest" "upstream interop target (${version})" "$up_identity"

  echo "Testing upstream rsync ${version} client -> oc-rsync --daemon"
  start_oc_daemon "$oc_conf" "$oc_log" "$upstream_binary" "$oc_pid_file" "$oc_port"

  if ! timeout "$hard_timeout" "$upstream_binary" -av --timeout=10 "${src}/" "rsync://127.0.0.1:${oc_port}/interop" >/dev/null 2>>"$oc_log"; then
    echo "FAIL: upstream rsync ${version} -> oc-rsync --daemon"
    echo "--- oc-rsync --daemon log (${oc_log}) ---"
    cat "$oc_log" || true
    stop_oc_daemon
    return 1
  fi

  if [[ ! -f "${oc_dest}/payload.txt" ]]; then
    echo "FAIL: upstream rsync ${version} reported success but file missing in oc dest"
    echo "--- oc-rsync --daemon log (${oc_log}) ---"
    cat "$oc_log" || true
    stop_oc_daemon
    return 1
  fi

  stop_oc_daemon

  echo "Testing oc-rsync client -> upstream rsync ${version} daemon"
  start_upstream_daemon "$upstream_binary" "$up_conf" "$up_log" "$up_pid_file"

  if ! timeout "$hard_timeout" "$oc_client" -av --timeout=10 "${src}/" "rsync://127.0.0.1:${upstream_port}/interop" >/dev/null 2>>"$up_log"; then
    echo "FAIL: oc-rsync -> upstream rsync ${version} daemon"
    echo "--- upstream rsyncd log (${up_log}) ---"
    cat "$up_log" || true
    stop_upstream_daemon
    return 1
  fi

  if [[ ! -f "${up_dest}/payload.txt" ]]; then
    echo "FAIL: oc-rsync client -> upstream rsync ${version} daemon: file missing"
    echo "--- upstream rsyncd log (${up_log}) ---"
    cat "$up_log" || true
    stop_upstream_daemon
    return 1
  fi

  stop_upstream_daemon
  return 0
}

# ============================================================================
# Comprehensive Interop Test Framework
# Covers all protocols (28-32), major rsync options, bidirectional transfers.
# ============================================================================

# Rich test data: multiple file types, sizes, metadata, symlinks, hardlinks
setup_comprehensive_src() {
  local dir=$1
  rm -rf "$dir"
  mkdir -p "$dir/subdir/nested" "$dir/empty_dir"
  echo "hello world" > "$dir/hello.txt"
  printf 'line1\nline2\nline3\n' > "$dir/multiline.txt"
  dd if=/dev/urandom of="$dir/binary.dat" bs=1K count=50 2>/dev/null
  dd if=/dev/urandom of="$dir/large.dat" bs=1K count=200 2>/dev/null
  echo "nested content" > "$dir/subdir/file.txt"
  echo "deep content" > "$dir/subdir/nested/deep.txt"
  touch "$dir/empty.txt"
  ln -sf hello.txt "$dir/link.txt"
  ln "$dir/hello.txt" "$dir/hardlink.txt"
  printf '#!/bin/sh\necho test\n' > "$dir/script.sh"
  chmod 755 "$dir/script.sh"
  echo "should be excluded" > "$dir/excluded.log"
  echo "also excluded" > "$dir/subdir/debug.log"
}

# Verify core files transferred with correct content
comp_verify_transfer() {
  local s=$1 d=$2
  for f in hello.txt multiline.txt binary.dat large.dat \
           subdir/file.txt subdir/nested/deep.txt empty.txt; do
    if [[ ! -f "$d/$f" ]]; then
      echo "    Missing: $f"
      return 1
    fi
    if ! cmp -s "$s/$f" "$d/$f"; then
      echo "    Content mismatch: $f"
      return 1
    fi
  done
  return 0
}

# Verify symlink target preserved
comp_verify_symlink() {
  local s=$1 d=$2
  if [[ ! -L "$d/link.txt" ]]; then
    echo "    Symlink not preserved"
    return 1
  fi
  local st dt
  st=$(readlink "$s/link.txt")
  dt=$(readlink "$d/link.txt")
  if [[ "$st" != "$dt" ]]; then
    echo "    Symlink target: $st vs $dt"
    return 1
  fi
  return 0
}

# Verify hard links share inode
comp_verify_hardlink() {
  local d=$1
  if [[ ! -f "$d/hello.txt" || ! -f "$d/hardlink.txt" ]]; then
    echo "    Hardlink files missing"
    return 1
  fi
  local i1 i2
  if stat --version >/dev/null 2>&1; then
    i1=$(stat -c %i "$d/hello.txt")
    i2=$(stat -c %i "$d/hardlink.txt")
  else
    i1=$(stat -f %i "$d/hello.txt")
    i2=$(stat -f %i "$d/hardlink.txt")
  fi
  if [[ "$i1" != "$i2" ]]; then
    echo "    Hardlinks not preserved ($i1 vs $i2)"
    return 1
  fi
  return 0
}

# Verify file permissions match between src and dest
comp_verify_perms() {
  local s=$1 d=$2
  for f in script.sh hello.txt; do
    if [[ -f "$d/$f" ]]; then
      local sp dp
      if stat --version >/dev/null 2>&1; then
        sp=$(stat -c %a "$s/$f"); dp=$(stat -c %a "$d/$f")
      else
        sp=$(stat -f %Lp "$s/$f"); dp=$(stat -f %Lp "$d/$f")
      fi
      if [[ "$sp" != "$dp" ]]; then
        echo "    Perms mismatch $f: $sp vs $dp"
        return 1
      fi
    fi
  done
  return 0
}

# Run a single test scenario: prepare dest, transfer, verify.
# Flags are word-split intentionally (no glob-expandable patterns at word start).
comp_run_scenario() {
  local label=$1 client=$2 flags=$3 sdir=$4 url=$5 ddir=$6 log=$7 vtype=$8

  # Prepare destination per scenario requirements
  case "$vtype" in
    delete)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      echo "extra" > "$ddir/extra_file.txt"
      ;;
    delta)
      rm -rf "$ddir"/*
      mkdir -p "$ddir/subdir/nested"
      for f in hello.txt multiline.txt empty.txt subdir/file.txt subdir/nested/deep.txt; do
        [[ -f "$sdir/$f" ]] && cp "$sdir/$f" "$ddir/$f"
      done
      # Replace binary data so delta has work to do
      dd if=/dev/zero of="$ddir/binary.dat" bs=1K count=50 2>/dev/null
      dd if=/dev/zero of="$ddir/large.dat" bs=1K count=200 2>/dev/null
      ;;
    *)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      ;;
  esac

  # shellcheck disable=SC2086
  if ! timeout "$hard_timeout" $client $flags --timeout=10 \
      "${sdir}/" "$url" >/dev/null 2>>"$log"; then
    echo "    FAIL (transfer error)"
    return 1
  fi

  # Verify per scenario type
  case "$vtype" in
    basic|compress|whole-file|inplace|numeric-ids|recursive|size-only|inc-recursive|delta)
      comp_verify_transfer "$sdir" "$ddir"
      ;;
    symlinks)
      comp_verify_transfer "$sdir" "$ddir" && comp_verify_symlink "$sdir" "$ddir"
      ;;
    hardlinks)
      comp_verify_transfer "$sdir" "$ddir" && comp_verify_hardlink "$ddir"
      ;;
    delete)
      comp_verify_transfer "$sdir" "$ddir" || return 1
      if [[ -f "$ddir/extra_file.txt" ]]; then
        echo "    --delete: extra file not removed"
        return 1
      fi
      return 0
      ;;
    exclude)
      for f in hello.txt multiline.txt binary.dat large.dat; do
        if [[ ! -f "$ddir/$f" ]]; then
          echo "    Missing: $f"
          return 1
        fi
        if ! cmp -s "$sdir/$f" "$ddir/$f"; then
          echo "    Mismatch: $f"
          return 1
        fi
      done
      if [[ -f "$ddir/excluded.log" || -f "$ddir/subdir/debug.log" ]]; then
        echo "    Excluded files transferred"
        return 1
      fi
      return 0
      ;;
    perms)
      comp_verify_transfer "$sdir" "$ddir" && comp_verify_perms "$sdir" "$ddir"
      ;;
  esac
}

# Run all comprehensive scenarios for one upstream version, optionally forcing protocol.
# Uses global $comp_src, $oc_client, $up_identity, $hard_timeout.
run_comprehensive_interop_case() {
  local version=$1 upstream_binary=$2 oc_port=$3 upstream_port=$4
  local protocol_flag="${5:-}"
  local vtag=${version//./-}
  local ptag=""; [[ -n "$protocol_flag" ]] && ptag="_p${protocol_flag##*=}"
  local tag="${vtag}${ptag}"
  local sfx=""; [[ -n "$protocol_flag" ]] && sfx=" (${protocol_flag})"

  local od="${workdir}/co-${tag}" ud="${workdir}/cu-${tag}"
  local opf="${workdir}/co-${tag}.pid" upf="${workdir}/cu-${tag}.pid"
  local ocf="${workdir}/co-${tag}.conf" ucf="${workdir}/cu-${tag}.conf"
  local olf="${workdir}/co-${tag}.log" ulf="${workdir}/cu-${tag}.log"

  rm -rf "$od" "$ud"; mkdir -p "$od" "$ud"

  write_rust_daemon_conf "$ocf" "$opf" "$oc_port" "$od" "c-${tag}"
  write_upstream_conf "$ucf" "$upf" "$upstream_port" "$ud" "c-${tag}" "$up_identity"

  # Scenario table: name|flags|verify_type
  local -a scenarios=(
    "archive|-av|basic"
    "checksum|-avc|basic"
    "compress|-avz|compress"
    "whole-file|-avW|whole-file"
    "delta|-av --no-whole-file -I|delta"
    "inplace|-av --inplace|inplace"
    "numeric-ids|-av --numeric-ids|numeric-ids"
    "recursive-only|-rv|recursive"
    "symlinks|-rlptv|symlinks"
    "hardlinks|-avH|hardlinks"
    "delete|-av --delete|delete"
    "exclude|-av --exclude=*.log|exclude"
    "permissions|-rlpv|perms"
    "size-only|-av --size-only|size-only"
  )

  # Incremental recursion only supported on protocol 30+
  local fp=""; [[ -n "$protocol_flag" ]] && fp="${protocol_flag##*=}"
  if [[ -z "$fp" || "$fp" -ge 30 ]]; then
    scenarios+=("inc-recursive|-av --inc-recursive|inc-recursive")
  fi

  local total=0 passed=0 fail=0

  # Direction 1: upstream client -> oc-rsync daemon
  start_oc_daemon "$ocf" "$olf" "$upstream_binary" "$opf" "$oc_port"

  for spec in "${scenarios[@]}"; do
    IFS='|' read -r name flags vtype <<< "$spec"
    [[ -n "$protocol_flag" ]] && flags="$flags $protocol_flag"
    total=$((total + 1))
    echo "  [upstream ${version}→oc] ${name}${sfx}"
    if comp_run_scenario "$name" "$upstream_binary" "$flags" "$comp_src" \
        "rsync://127.0.0.1:${oc_port}/interop" "$od" "$olf" "$vtype"; then
      echo "    PASS"
      passed=$((passed + 1))
    else
      fail=$((fail + 1))
    fi
  done

  stop_oc_daemon

  # Direction 2: oc-rsync client -> upstream daemon
  start_upstream_daemon "$upstream_binary" "$ucf" "$ulf" "$upf"

  for spec in "${scenarios[@]}"; do
    IFS='|' read -r name flags vtype <<< "$spec"
    [[ -n "$protocol_flag" ]] && flags="$flags $protocol_flag"
    total=$((total + 1))
    echo "  [oc→upstream ${version}] ${name}${sfx}"
    if comp_run_scenario "$name" "$oc_client" "$flags" "$comp_src" \
        "rsync://127.0.0.1:${upstream_port}/interop" "$ud" "$ulf" "$vtype"; then
      echo "    PASS"
      passed=$((passed + 1))
    else
      fail=$((fail + 1))
    fi
  done

  stop_upstream_daemon

  echo "  === ${version}${sfx}: ${passed}/${total} passed, ${fail} failed ==="
  return $fail
}

# ------------------ main ------------------

# Parse command line arguments
build_only=false
for arg in "$@"; do
  case "$arg" in
    build-only)
      build_only=true
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      echo "Usage: $0 [build-only]" >&2
      exit 1
      ;;
  esac
done

# Build upstream binaries first (always needed)
mkdir -p "$upstream_src_root" "$upstream_install_root"
for version in "${versions[@]}"; do
  ensure_upstream_build "$version"
done

# If build-only mode, exit after building upstream binaries
if [[ "$build_only" == "true" ]]; then
  echo "Built upstream rsync binaries (build-only mode)"
  for version in "${versions[@]}"; do
    binary="${upstream_install_root}/${version}/bin/rsync"
    if [[ -x "$binary" ]]; then
      echo "  - ${version}: $binary"
    fi
  done
  exit 0
fi

# For full interop tests, build oc-rsync
ensure_workspace_binaries

oc_client="${target_dir}/oc-rsync"
oc_binary="${target_dir}/oc-rsync"

workdir=$(mktemp -d)
trap cleanup EXIT

src="${workdir}/source"
mkdir -p "$src"
printf 'interop-test\n' >"${src}/payload.txt"

uid=$(id -u)
gid=$(id -g)

oc_identity=""
up_identity=""
if [[ ${uid} -eq 0 ]]; then
  printf -v up_identity 'uid = %s\ngid = %s\n' "${uid}" "${gid}"
fi

port_base=2873
failed=()

for version in "${versions[@]}"; do
  upstream_binary="${upstream_install_root}/${version}/bin/rsync"
  if [[ ! -x "$upstream_binary" ]]; then
    echo "Missing upstream rsync binary for version ${version}" >&2
    failed+=("$version (missing binary)")
    continue
  fi

  echo "Running interoperability checks against upstream rsync ${version}"
  if ! run_interop_case "$version" "$upstream_binary" "$port_base" $((port_base + 1)); then
    failed+=("$version")
  fi
  port_base=$((port_base + 2))
done

# =====================================================================
# Comprehensive interop tests: all protocols (28-32), all major options
# =====================================================================
echo ""
echo "=== Comprehensive Interop Tests ==="

comp_src="${workdir}/comp-source"
setup_comprehensive_src "$comp_src"

# Test each version at its native protocol with all scenarios
for version in "${versions[@]}"; do
  upstream_binary="${upstream_install_root}/${version}/bin/rsync"
  if [[ ! -x "$upstream_binary" ]]; then
    failed+=("${version}-comprehensive (missing)")
    continue
  fi

  echo ""
  echo "=== Comprehensive: upstream ${version} (native protocol) ==="
  if ! run_comprehensive_interop_case "$version" "$upstream_binary" \
      "$port_base" $((port_base + 1)); then
    failed+=("${version}-comprehensive")
  fi
  port_base=$((port_base + 2))
done

# Protocol version forcing tests: all 5 protocols via upstream 3.4.1
newest_binary="${upstream_install_root}/3.4.1/bin/rsync"
if [[ -x "$newest_binary" ]]; then
  for proto in 28 29 30 31 32; do
    echo ""
    echo "=== Protocol ${proto} (forced via --protocol=${proto}) ==="
    if ! run_comprehensive_interop_case "3.4.1" "$newest_binary" \
        "$port_base" $((port_base + 1)) "--protocol=${proto}"; then
      failed+=("proto${proto}")
    fi
    port_base=$((port_base + 2))
  done
else
  echo "Skipping protocol forcing tests (3.4.1 binary unavailable)"
fi

# Final report
if (( ${#failed[@]} > 0 )); then
  echo ""
  echo "Interop failures: ${failed[*]}" >&2
  exit 1
fi

echo ""
echo "All interoperability checks succeeded (basic + comprehensive + protocols 28-32)."
