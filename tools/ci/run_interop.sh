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
oc_port_current=""
up_port_current=""
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
  # Do NOT disable lz4 or zstd - needed for compression interop tests.
  # Both libraries are installed via APT (libzstd-dev, liblz4-dev).
  if grep -q -- "--disable-md2man" <<<"$configure_help"; then
    configure_args+=("--disable-md2man")
  fi
  # Enable ACL and xattr support so interop tests can exercise -A and -X flags.
  # The CI image installs libacl1-dev and libattr1-dev via APT.
  if grep -q -- "--enable-acl-support" <<<"$configure_help"; then
    configure_args+=("--enable-acl-support")
  fi
  if grep -q -- "--enable-xattr-support" <<<"$configure_help"; then
    configure_args+=("--enable-xattr-support")
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

[interop]
path = ${dest}
comment = ${comment}
read only = false
numeric ids = yes
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
munge symlinks = false
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
    # Wait up to 5 seconds for graceful shutdown, then SIGKILL
    local i=0
    while kill -0 "${oc_pid}" 2>/dev/null && [ $i -lt 10 ]; do
      sleep 0.5
      i=$((i + 1))
    done
    if kill -0 "${oc_pid}" 2>/dev/null; then
      kill -9 "${oc_pid}" >/dev/null 2>&1 || true
    fi
    wait "${oc_pid}" >/dev/null 2>&1 || true
    oc_pid=""
  fi
  if [[ -n "${oc_port_current}" ]]; then
    wait_for_port_free "${oc_port_current}" 10
    oc_port_current=""
  fi
  if [[ -n "${oc_pid_file_current:-}" ]]; then
    rm -f "${oc_pid_file_current}"
    oc_pid_file_current=""
  fi
}

stop_upstream_daemon() {
  if [[ -n "${up_pid}" ]]; then
    kill "${up_pid}" >/dev/null 2>&1 || true
    # Wait up to 5 seconds for graceful shutdown, then SIGKILL
    local i=0
    while kill -0 "${up_pid}" 2>/dev/null && [ $i -lt 10 ]; do
      sleep 0.5
      i=$((i + 1))
    done
    if kill -0 "${up_pid}" 2>/dev/null; then
      kill -9 "${up_pid}" >/dev/null 2>&1 || true
    fi
    wait "${up_pid}" >/dev/null 2>&1 || true
    up_pid=""
  fi
  if [[ -n "${up_port_current}" ]]; then
    wait_for_port_free "${up_port_current}" 10
    up_port_current=""
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

# Allocate an ephemeral port from the kernel.
# Binds port 0 on 127.0.0.1, records the assigned port, then releases it.
# The kernel picks from the ephemeral range (32768-60999 on Linux) and avoids
# recently used ports, eliminating TIME_WAIT collisions between test phases.
allocate_ephemeral_port() {
  python3 -c "
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', 0))
print(s.getsockname()[1])
s.close()
"
}

# Wait for a TCP port to become reachable, with timeout.
# Returns non-zero on failure - callers must handle this as a hard error.
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
  echo "ERROR: port $port not ready after ${max_wait}s" >&2
  return 1
}

# Wait for a TCP port to stop accepting connections (released after daemon shutdown).
# Prevents the next daemon from racing against TIME_WAIT on the old socket.
wait_for_port_free() {
  local port=$1
  local max_wait=${2:-10}
  local elapsed=0

  while [ $elapsed -lt $max_wait ]; do
    if ! (echo >/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
      return 0
    fi
    sleep 0.5
    elapsed=$((elapsed + 1))
  done
  echo "Warning: port $port still in use after ${max_wait}s" >&2
  return 0
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

  stop_oc_daemon

  oc_pid_file_current="$pid_file"
  oc_port_current="$port"

  RUST_BACKTRACE=1 \
  OC_RSYNC_DAEMON_FALLBACK=0 \
    "$oc_binary" --daemon --no-detach --config "$config" --port "$port" --log-file "$log_file" </dev/null &
  oc_pid=$!
  if ! wait_for_port "$port" 10; then
    echo "FATAL: oc-rsync daemon failed to bind port $port" >&2
    stop_oc_daemon
    return 1
  fi
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
    up_port_current="$port"
    if ! wait_for_port "$port" 10; then
      echo "FATAL: upstream rsync daemon failed to bind port $port" >&2
      stop_upstream_daemon
      return 1
    fi
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
  echo "temp data" > "$dir/temp_one.tmp"
  echo "temp nested" > "$dir/subdir/temp_two.tmp"
  echo "notes content" > "$dir/notes.dat"
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
    existing)
      rm -rf "$ddir"/*; mkdir -p "$ddir/subdir"
      echo "old content" > "$ddir/hello.txt"
      echo "old nested" > "$ddir/subdir/file.txt"
      ;;
    backup)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      echo "old hello" > "$ddir/hello.txt"
      echo "old multiline" > "$ddir/multiline.txt"
      ;;
    update)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      cp -r "$sdir"/* "$ddir"/
      # Set dest file timestamps to future (newer than source)
      find "$ddir" -type f -exec touch -t 203001010000 {} +
      ;;
    checksum-skip)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      # Copy source files to dest so content is identical
      cp -a "$sdir"/* "$ddir"/
      ;;
    max-delete)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      echo "extra1" > "$ddir/extra_one.txt"
      echo "extra2" > "$ddir/extra_two.txt"
      echo "extra3" > "$ddir/extra_three.txt"
      ;;
    inplace)
      rm -rf "$ddir"/*
      mkdir -p "$ddir/subdir/nested"
      for f in hello.txt multiline.txt empty.txt subdir/file.txt subdir/nested/deep.txt; do
        [[ -f "$sdir/$f" ]] && cp "$sdir/$f" "$ddir/$f"
      done
      echo "old content for inplace" > "$ddir/hello.txt"
      ;;
    whole-file-replace)
      rm -rf "$ddir"/*
      mkdir -p "$ddir/subdir/nested"
      for f in hello.txt multiline.txt subdir/file.txt subdir/nested/deep.txt; do
        echo "stale data" > "$ddir/$f"
      done
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
    compare-dest)
      rm -rf "$ddir"/*; mkdir -p "$ddir/compare_ref"
      cp -a "$sdir/hello.txt" "$ddir/compare_ref/"
      ;;
    link-dest)
      rm -rf "$ddir"/*; mkdir -p "$ddir/link_ref"
      # Copy source files to reference dir so link-dest can hardlink them
      cp -a "$sdir"/* "$ddir/link_ref"/
      ;;
    files-from)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      # Create file list selecting only specific files (in dest dir so both sides can find it)
      printf 'hello.txt\nmultiline.txt\nsubdir/file.txt\n' > "$ddir/filelist.txt"
      ;;
    hardlinks-relative)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      ;;
    xattrs)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      ;;
    itemize)
      rm -rf "$ddir"/*
      mkdir -p "$ddir/subdir/nested"
      echo "old content" > "$ddir/hello.txt"
      echo "old nested" > "$ddir/subdir/file.txt"
      ;;
    include-exclude)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      ;;
    filter-rule)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      ;;
    merge-filter)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      ;;
    exclude-from)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      # Create exclude-from file listing patterns to exclude
      printf '*.log\n*.tmp\n' > "$ddir/exclude_patterns.txt"
      ;;
    *)
      rm -rf "$ddir"/*; mkdir -p "$ddir"
      ;;
  esac

  # Per-scenario source augmentation: add files that only specific scenarios need,
  # avoiding pollution of the shared source that breaks older rsync versions.
  case "$vtype" in
    safe-links)
      ln -sf /etc/hostname "$sdir/unsafe_link.txt" 2>/dev/null || true
      ;;
    sparse)
      dd if=/dev/zero bs=4096 count=16 2>/dev/null > "$sdir/sparse_test.bin"
      ;;
    merge-filter)
      # Place a .rsync-filter merge file in source that excludes *.dat files
      printf 'exclude *.dat\n' > "$sdir/.rsync-filter"
      ;;
  esac

  # shellcheck disable=SC2086
  local transfer_log="${log}.transfer"
  # Resolve --files-from path to absolute (file placed in dest dir during prep)
  local resolved_flags="$flags"
  if [[ "$resolved_flags" == *"--files-from=filelist.txt"* ]]; then
    resolved_flags="${resolved_flags/--files-from=filelist.txt/--files-from=${ddir}/filelist.txt}"
  fi
  if [[ "$resolved_flags" == *"--exclude-from=exclude_patterns.txt"* ]]; then
    resolved_flags="${resolved_flags/--exclude-from=exclude_patterns.txt/--exclude-from=${ddir}/exclude_patterns.txt}"
  fi
  # When -R (--relative) is active, use relative source paths so the
  # destination receives files as hello.txt instead of /full/path/hello.txt.
  # upstream: rsync -avR /abs/src/ dst/ preserves the full absolute path,
  # but rsync -avR . dst/ (from within src/) preserves only relative paths.
  local rc=0
  if [[ "$resolved_flags" == *"R"* ]]; then
    local abs_client
    abs_client=$(command -v "$client" 2>/dev/null || echo "$client")
    [[ "$abs_client" != /* ]] && abs_client="$(cd "$(dirname "$client")" && pwd)/$(basename "$client")"
    (cd "$sdir" && timeout "$hard_timeout" "$abs_client" $resolved_flags --timeout=10 \
        . "$url" >"$transfer_log.out" 2>"$transfer_log.err") || rc=$?
  else
    timeout "$hard_timeout" $client $resolved_flags --timeout=10 \
        "${sdir}/" "$url" >"$transfer_log.out" 2>"$transfer_log.err" || rc=$?
  fi
  cat "$transfer_log.err" >> "$log"

  # Clean up per-scenario source augmentation
  case "$vtype" in
    safe-links) rm -f "$sdir/unsafe_link.txt" ;;
    sparse) rm -f "$sdir/sparse_test.bin" ;;
    merge-filter) rm -f "$sdir/.rsync-filter" ;;
  esac

  # --max-delete exits 25 when limit reached; treat as success for verification
  if [[ $rc -ne 0 ]] && ! [[ "$vtype" == "max-delete" && $rc -eq 25 ]]; then
    echo "    FAIL (transfer error, exit=$rc)"
    echo "    stderr: $(head -5 "$transfer_log.err")"
    return 1
  fi

  # Verify per scenario type
  case "$vtype" in
    checksum-skip)
      local file_count
      file_count=$(find "$ddir" -type f | wc -l)
      if [ "$file_count" -lt 1 ]; then
        echo "FAIL: no files in destination after checksum transfer"
        return 1
      fi
      echo "  --checksum correctly handled pre-populated identical files"
      return 0
      ;;
    update)
      for f in $(find "$ddir" -type f); do
        local mod_epoch
        mod_epoch=$(stat -c %Y "$f" 2>/dev/null)
        # 1893456000 = 2030-01-01 00:00:00 UTC
        if [[ "$mod_epoch" -lt 1893456000 ]]; then
          echo "    --update: $f was overwritten despite newer dest timestamp"
          return 1
        fi
      done
      echo "  --update correctly skipped files with newer dest timestamps"
      return 0
      ;;
    basic|compress|whole-file|whole-file-replace|inplace|numeric-ids|recursive|size-only|inc-recursive|delta|sparse|partial|append|bwlimit)
      if ! comp_verify_transfer "$sdir" "$ddir"; then
        echo "    dest contents: $(find "$ddir" -type f | sort | head -20)"
        echo "    daemon log tail: $(tail -5 "$log" 2>/dev/null)"
        return 1
      fi
      return 0
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
    copy-links)
      comp_verify_transfer "$sdir" "$ddir" || return 1
      if [[ -L "$ddir/link.txt" ]]; then
        echo "    --copy-links: link.txt is still a symlink"
        return 1
      fi
      if [[ -f "$ddir/link.txt" ]]; then
        if ! cmp -s "$sdir/hello.txt" "$ddir/link.txt"; then
          echo "    --copy-links: link.txt content mismatch"
          return 1
        fi
      else
        echo "    --copy-links: link.txt missing"
        return 1
      fi
      return 0
      ;;
    safe-links)
      comp_verify_transfer "$sdir" "$ddir" || return 1
      if [[ -L "$ddir/unsafe_link.txt" ]]; then
        echo "    --safe-links: unsafe symlink was transferred"
        return 1
      fi
      if [[ -L "$sdir/link.txt" ]] && [[ ! -L "$ddir/link.txt" ]]; then
        echo "    --safe-links: safe symlink missing"
        return 1
      fi
      return 0
      ;;
    existing)
      if [[ ! -f "$ddir/hello.txt" ]]; then
        echo "    --existing: pre-existing hello.txt missing"
        return 1
      fi
      if ! cmp -s "$sdir/hello.txt" "$ddir/hello.txt"; then
        echo "    --existing: hello.txt not updated"
        return 1
      fi
      if ! cmp -s "$sdir/subdir/file.txt" "$ddir/subdir/file.txt"; then
        echo "    --existing: subdir/file.txt not updated"
        return 1
      fi
      if [[ -f "$ddir/multiline.txt" || -f "$ddir/binary.dat" ]]; then
        echo "    --existing: new files were created"
        return 1
      fi
      return 0
      ;;
    backup)
      comp_verify_transfer "$sdir" "$ddir" || return 1
      if [[ ! -f "$ddir/hello.txt~" ]]; then
        echo "    --backup: hello.txt~ backup not created"
        return 1
      fi
      if [[ ! -f "$ddir/multiline.txt~" ]]; then
        echo "    --backup: multiline.txt~ backup not created"
        return 1
      fi
      local expected_hello="old hello"
      local actual_hello
      actual_hello=$(cat "$ddir/hello.txt~")
      if [[ "$actual_hello" != "$expected_hello" ]]; then
        echo "    --backup: hello.txt~ content wrong"
        return 1
      fi
      return 0
      ;;
    dry-run)
      local count
      count=$(find "$ddir" -type f 2>/dev/null | wc -l)
      if [[ $count -gt 0 ]]; then
        echo "    --dry-run: files were created ($count found)"
        return 1
      fi
      return 0
      ;;
    max-delete)
      comp_verify_transfer "$sdir" "$ddir" || return 1
      local remaining=0
      [[ -f "$ddir/extra_one.txt" ]] && remaining=$((remaining + 1))
      [[ -f "$ddir/extra_two.txt" ]] && remaining=$((remaining + 1))
      [[ -f "$ddir/extra_three.txt" ]] && remaining=$((remaining + 1))
      if [[ $remaining -lt 2 ]]; then
        echo "    --max-delete=1: too many files deleted (${remaining} remaining, expected >= 2)"
        return 1
      fi
      return 0
      ;;
    acls)
      # ACLs transfer should not break the transfer itself
      comp_verify_transfer "$sdir" "$ddir" || return 1
      return 0
      ;;
    compare-dest)
      # Files matching compare_ref should be skipped; others should transfer
      if [[ ! -f "$ddir/multiline.txt" ]]; then
        echo "    --compare-dest: multiline.txt not transferred (should not match ref)"
        return 1
      fi
      if [[ -f "$ddir/hello.txt" ]]; then
        echo "    --compare-dest: hello.txt was transferred despite matching ref"
        return 1
      fi
      return 0
      ;;
    link-dest)
      # With --link-dest, unchanged files should be hardlinked to reference
      if [[ ! -f "$ddir/hello.txt" ]]; then
        echo "    --link-dest: hello.txt missing from dest"
        return 1
      fi
      # Check that hello.txt is a hardlink (link count > 1)
      local link_count
      link_count=$(stat -c %h "$ddir/hello.txt" 2>/dev/null || stat -f %l "$ddir/hello.txt" 2>/dev/null)
      if [[ "$link_count" -le 1 ]]; then
        echo "    --link-dest: hello.txt not hardlinked (count=$link_count)"
        return 1
      fi
      return 0
      ;;
    files-from)
      # Only the files listed in filelist.txt should be transferred
      for f in hello.txt multiline.txt subdir/file.txt; do
        if [[ ! -f "$ddir/$f" ]]; then
          echo "    --files-from: listed file $f missing"
          return 1
        fi
        if ! cmp -s "$sdir/$f" "$ddir/$f"; then
          echo "    --files-from: content mismatch for $f"
          return 1
        fi
      done
      # Files NOT in the list should not be transferred
      for f in binary.dat large.dat empty.txt subdir/nested/deep.txt; do
        if [[ -f "$ddir/$f" ]]; then
          echo "    --files-from: unlisted file $f was transferred"
          return 1
        fi
      done
      # Clean up the file list
      rm -f "$sdir/filelist.txt"
      return 0
      ;;
    hardlinks-relative)
      # With -H -R, hardlinks and relative paths should both work
      if [[ ! -f "$ddir/hello.txt" ]]; then
        echo "    -HR: hello.txt missing"
        return 1
      fi
      if ! cmp -s "$sdir/hello.txt" "$ddir/hello.txt"; then
        echo "    -HR: hello.txt content mismatch"
        return 1
      fi
      # Check hardlink preservation
      if [[ -f "$ddir/hardlink.txt" ]]; then
        local i1 i2
        i1=$(stat -c %i "$ddir/hello.txt" 2>/dev/null || stat -f %i "$ddir/hello.txt" 2>/dev/null)
        i2=$(stat -c %i "$ddir/hardlink.txt" 2>/dev/null || stat -f %i "$ddir/hardlink.txt" 2>/dev/null)
        if [[ "$i1" != "$i2" ]]; then
          echo "    -HR: hardlink not preserved (inodes $i1 vs $i2)"
          return 1
        fi
      fi
      return 0
      ;;
    xattrs)
      # -X transfer should not break the transfer itself
      comp_verify_transfer "$sdir" "$ddir" || return 1
      return 0
      ;;
    itemize)
      comp_verify_transfer "$sdir" "$ddir" || return 1
      local item_out="$transfer_log.out"
      if ! grep -qE '^[<>ch.][fdLDS]' "$item_out"; then
        echo "    --itemize-changes: no itemize output found"
        return 1
      fi
      if ! grep -qE '^\>f' "$item_out"; then
        echo "    --itemize-changes: no file transfer itemize lines"
        return 1
      fi
      return 0
      ;;
    include-exclude)
      # Only .txt files should be transferred (include *.txt, exclude *)
      for f in hello.txt multiline.txt empty.txt subdir/file.txt subdir/nested/deep.txt; do
        if [[ ! -f "$ddir/$f" ]]; then
          echo "    --include/--exclude: expected .txt file $f missing"
          return 1
        fi
        if ! cmp -s "$sdir/$f" "$ddir/$f"; then
          echo "    --include/--exclude: content mismatch for $f"
          return 1
        fi
      done
      # Non-.txt files should NOT be present
      for f in binary.dat large.dat notes.dat excluded.log temp_one.tmp script.sh; do
        if [[ -f "$ddir/$f" ]]; then
          echo "    --include/--exclude: non-.txt file $f was transferred"
          return 1
        fi
      done
      return 0
      ;;
    filter-rule)
      # --filter 'exclude *.tmp' should exclude .tmp files, transfer everything else
      for f in hello.txt multiline.txt binary.dat large.dat \
               subdir/file.txt subdir/nested/deep.txt empty.txt; do
        if [[ ! -f "$ddir/$f" ]]; then
          echo "    --filter exclude: expected file $f missing"
          return 1
        fi
        if ! cmp -s "$sdir/$f" "$ddir/$f"; then
          echo "    --filter exclude: content mismatch for $f"
          return 1
        fi
      done
      if [[ -f "$ddir/temp_one.tmp" || -f "$ddir/subdir/temp_two.tmp" ]]; then
        echo "    --filter exclude: .tmp files were transferred"
        return 1
      fi
      return 0
      ;;
    merge-filter)
      # .rsync-filter excludes *.dat, so .dat files should not transfer
      for f in hello.txt multiline.txt empty.txt \
               subdir/file.txt subdir/nested/deep.txt; do
        if [[ ! -f "$ddir/$f" ]]; then
          echo "    --filter merge: expected file $f missing"
          return 1
        fi
        if ! cmp -s "$sdir/$f" "$ddir/$f"; then
          echo "    --filter merge: content mismatch for $f"
          return 1
        fi
      done
      for f in binary.dat large.dat notes.dat; do
        if [[ -f "$ddir/$f" ]]; then
          echo "    --filter merge: .dat file $f was transferred despite .rsync-filter"
          return 1
        fi
      done
      return 0
      ;;
    exclude-from)
      # --exclude-from file lists *.log and *.tmp patterns
      for f in hello.txt multiline.txt binary.dat large.dat \
               subdir/file.txt subdir/nested/deep.txt empty.txt; do
        if [[ ! -f "$ddir/$f" ]]; then
          echo "    --exclude-from: expected file $f missing"
          return 1
        fi
        if ! cmp -s "$sdir/$f" "$ddir/$f"; then
          echo "    --exclude-from: content mismatch for $f"
          return 1
        fi
      done
      if [[ -f "$ddir/excluded.log" || -f "$ddir/subdir/debug.log" ]]; then
        echo "    --exclude-from: .log files were transferred"
        return 1
      fi
      if [[ -f "$ddir/temp_one.tmp" || -f "$ddir/subdir/temp_two.tmp" ]]; then
        echo "    --exclude-from: .tmp files were transferred"
        return 1
      fi
      return 0
      ;;
  esac
}

# Run all comprehensive scenarios for one upstream version, optionally forcing protocol.
# Known comprehensive test failures — pre-existing feature limitations.
# Format: "direction:name" where direction is "up" (upstream→oc) or "oc" (oc→upstream).
# These are tracked separately from unexpected failures so CI catches regressions
# while not blocking on unrelated missing features.
#
# Resolved since initial tracking:
# - up:checksum, oc:checksum (always-checksum mode implemented)
# - up:delete (apply_long_form_args now parses --delete/--delete-before)
# - up:symlinks, oc:symlinks (create_symlinks() in receiver)
# - oc:delete, oc:numeric-ids, oc:exclude (correct compact flag semantics + long-form args)
# - up:compress, oc:compress (TokenReader integration in run_sync path)
# - up:size-only (do_compression check matched 'z' in --size-only long-form arg)
#
# SSH interop test: oc-rsync client transfers to localhost via SSH.
run_ssh_interop_test() {
  local oc_bin=$1 src_dir=$2 work_dir=$3 log=$4

  local ssh_dest="${work_dir}/ssh_dest"
  rm -rf "$ssh_dest"
  mkdir -p "$ssh_dest"

  local transfer_log="${log}.ssh-transfer"
  if ! timeout "$hard_timeout" "$oc_bin" -av \
      -e "ssh -o StrictHostKeyChecking=no" \
      --timeout=10 \
      "${src_dir}/" "${ssh_dest}/" \
      >"$transfer_log.out" 2>"$transfer_log.err"; then
    local rc=$?
    cat "$transfer_log.err" >> "$log"
    echo "    FAIL (SSH transfer error, exit=$rc)"
    echo "    stderr: $(head -5 "$transfer_log.err")"
    return 1
  fi
  cat "$transfer_log.err" >> "$log"

  if ! comp_verify_transfer "$src_dir" "$ssh_dest"; then
    echo "    dest contents: $(find "$ssh_dest" -type f | sort | head -20)"
    return 1
  fi

  return 0
}

# Remaining known failures:
KNOWN_FAILURES=(
  # --- oc→upstream (client push) ---
  # ACLs/xattrs: wire format incompatibility with older upstream receivers.
  # oc-rsync sends ACL/xattr indices that older upstream (3.0.9) cannot parse.
  "oc:acls"
  "oc:xattrs"
  # Hardlinks: wire format divergence with older upstream receivers.
  "oc:hardlinks"
  "oc:hardlinks-relative"
  # Itemize: output format differences with upstream daemon mode.
  "oc:itemize"
  # merge-filter: per-directory merge filters (.rsync-filter) not yet wired
  # to generator walk_path - DirMerge infrastructure exists in engine but
  # push_local_filters/pop_local_filters not implemented for remote transfers.
  "oc:merge-filter"
  # hardlinks: receiver-side hardlink restoration not yet implemented for
  # daemon push transfers (wire encoding of abbreviated followers is correct,
  # but receiver does not create hardlinks from the index).
  "oc:hardlinks"
  "oc:hardlinks-relative"
  # --- upstream→oc (daemon receive) ---
  # Itemize: output format differences when upstream pushes to oc-rsync daemon.
  "up:itemize"
  # protocol-31: upstream 3.0.9 does not support protocol 31.
  "up:protocol-31"
  # ACLs/xattrs: upstream daemon builds may not have ACL/xattr support enabled.
  "up:acls"
  "up:xattrs"
  # Sparse: flaky under parallel CI load at protocol 30 forced mode.
  "up:sparse"
  # --- standalone ---
  "standalone:write-batch-read-batch"
  "standalone:large-file-2gb"
)

is_known_failure() {
  local direction=$1 name=$2 forced_proto=$3
  local key="${direction}:${name}"
  for kf in "${KNOWN_FAILURES[@]}"; do
    [[ "$kf" == "$key" ]] && return 0
  done
  return 1
}

# ============================================================================
# Standalone Interop Test Scenarios (#876-#884)
# These tests require custom daemon configs, special setup, or non-standard
# verification that does not fit the comp_run_scenario pattern.
# ============================================================================

# Helper: check if a standalone test is a known failure and report accordingly.
run_standalone_test() {
  local name=$1
  local test_func=$2
  shift 2

  echo "  [standalone] ${name}"
  if $test_func "$@"; then
    echo "    PASS"
    return 0
  else
    if is_known_failure "standalone" "$name" ""; then
      echo "    SKIP (known limitation)"
      return 2
    else
      echo "    UNEXPECTED FAIL: standalone:${name}"
      return 1
    fi
  fi
}

# #876: write-batch / read-batch roundtrip
# Tests batch file format compatibility in three ways:
# 1. Local: upstream writes batch, oc-rsync reads it (and vice versa)
# 2. Daemon: oc-rsync pushes to oc-rsync daemon with --write-batch, then
#    replays --read-batch to a fresh destination
test_write_batch_read_batch() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5 \
        oc_port=$6 upstream_port=$7

  local batch_dir="${work}/batch-test"
  local dest1="${batch_dir}/dest1"
  local dest2="${batch_dir}/dest2"
  local batch_file="${batch_dir}/batch.rsync"
  rm -rf "$batch_dir"
  mkdir -p "$dest1" "$dest2"

  # --- Local roundtrip ---

  # Step 1: upstream rsync writes a batch file
  if ! timeout "$hard_timeout" "$upstream_binary" -av \
      --write-batch="$batch_file" --timeout=10 \
      "${src_dir}/" "${dest1}/" \
      >"${log}.write-batch.out" 2>"${log}.write-batch.err"; then
    echo "    write-batch failed (upstream write, exit=$?)"
    return 1
  fi

  if [[ ! -f "$batch_file" ]]; then
    echo "    batch file not created"
    return 1
  fi

  # Step 2: oc-rsync reads the batch file to a fresh destination
  if ! timeout "$hard_timeout" "$oc_bin" -av \
      --read-batch="$batch_file" --timeout=10 \
      "${dest2}/" \
      >"${log}.read-batch.out" 2>"${log}.read-batch.err"; then
    echo "    read-batch failed (oc-rsync read, exit=$?)"
    return 1
  fi

  # Verify files match
  if ! comp_verify_transfer "$src_dir" "$dest2"; then
    echo "    content mismatch after read-batch"
    return 1
  fi

  # Step 3: reverse - oc-rsync writes batch, upstream reads
  local dest3="${batch_dir}/dest3"
  local dest4="${batch_dir}/dest4"
  local batch_file2="${batch_dir}/batch2.rsync"
  mkdir -p "$dest3" "$dest4"

  if ! timeout "$hard_timeout" "$oc_bin" -av \
      --write-batch="$batch_file2" --timeout=10 \
      "${src_dir}/" "${dest3}/" \
      >"${log}.write-batch2.out" 2>"${log}.write-batch2.err"; then
    echo "    write-batch failed (oc-rsync write, exit=$?)"
    return 1
  fi

  if [[ ! -f "$batch_file2" ]]; then
    echo "    batch file 2 not created"
    return 1
  fi

  if ! timeout "$hard_timeout" "$upstream_binary" -av \
      --read-batch="$batch_file2" --timeout=10 \
      "${dest4}/" \
      >"${log}.read-batch2.out" 2>"${log}.read-batch2.err"; then
    echo "    read-batch failed (upstream read, exit=$?)"
    return 1
  fi

  if ! comp_verify_transfer "$src_dir" "$dest4"; then
    echo "    content mismatch after reverse read-batch"
    return 1
  fi

  # --- Daemon roundtrip ---
  # Push files to oc-rsync daemon with --write-batch to capture a batch file,
  # then replay --read-batch to a fresh destination and verify.

  local daemon_dest="${batch_dir}/daemon-dest"
  local replay_dest="${batch_dir}/replay-dest"
  local batch_daemon="${batch_dir}/batch-daemon.rsync"
  mkdir -p "$daemon_dest" "$replay_dest"

  local daemon_conf="${batch_dir}/daemon-batch.conf"
  local daemon_pid="${batch_dir}/daemon-batch.pid"
  local daemon_log="${batch_dir}/daemon-batch.log"
  cat > "$daemon_conf" <<CONF
pid file = ${daemon_pid}
port = ${oc_port}
use chroot = false

[interop]
path = ${daemon_dest}
comment = batch daemon test
read only = false
numeric ids = yes
CONF

  start_oc_daemon "$daemon_conf" "$daemon_log" "$upstream_binary" "$daemon_pid" "$oc_port"

  # Step 4: oc-rsync pushes to daemon with --write-batch
  if ! timeout "$hard_timeout" "$oc_bin" -av \
      --write-batch="$batch_daemon" --timeout=10 \
      "${src_dir}/" "rsync://127.0.0.1:${oc_port}/interop" \
      >"${log}.write-batch-daemon.out" 2>"${log}.write-batch-daemon.err"; then
    echo "    write-batch failed (daemon push, exit=$?)"
    stop_oc_daemon
    return 1
  fi

  stop_oc_daemon

  if [[ ! -f "$batch_daemon" ]]; then
    echo "    daemon batch file not created"
    return 1
  fi

  # Verify daemon destination matches source
  if ! comp_verify_transfer "$src_dir" "$daemon_dest"; then
    echo "    content mismatch after daemon push"
    return 1
  fi

  # Step 5: replay the batch file to a fresh destination
  if ! timeout "$hard_timeout" "$oc_bin" -av \
      --read-batch="$batch_daemon" --timeout=10 \
      "${replay_dest}/" \
      >"${log}.read-batch-daemon.out" 2>"${log}.read-batch-daemon.err"; then
    echo "    read-batch failed (daemon batch replay, exit=$?)"
    return 1
  fi

  # Verify replayed destination matches source
  if ! comp_verify_transfer "$src_dir" "$replay_dest"; then
    echo "    content mismatch after daemon batch replay"
    return 1
  fi

  return 0
}

# #877: --info=progress2 output
# Verifies that --info=progress2 produces whole-transfer progress output.
test_info_progress2() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5

  local dest="${work}/progress2-dest"
  rm -rf "$dest"
  mkdir -p "$dest"

  # oc-rsync client with --info=progress2
  if ! timeout "$hard_timeout" "$oc_bin" -av --info=progress2 --timeout=10 \
      "${src_dir}/" "${dest}/" \
      >"${log}.progress2.out" 2>"${log}.progress2.err"; then
    echo "    transfer failed (exit=$?)"
    return 1
  fi

  # --info=progress2 should show percentage progress lines with xfr#N
  if ! grep -qE '[0-9]+%|xfr#|to-chk=' "${log}.progress2.out" "${log}.progress2.err" 2>/dev/null; then
    echo "    no progress2 output found in stdout/stderr"
    echo "    stdout: $(head -5 "${log}.progress2.out")"
    echo "    stderr: $(head -5 "${log}.progress2.err")"
    return 1
  fi

  if ! comp_verify_transfer "$src_dir" "$dest"; then
    echo "    content verification failed"
    return 1
  fi

  return 0
}

# #878: large file >2GB transfer
# Creates a sparse 2200MB file via truncate and transfers it.
test_large_file_2gb() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5 \
        oc_port=$6 upstream_port=$7

  local large_src="${work}/large-src"
  local large_dest="${work}/large-dest"
  rm -rf "$large_src" "$large_dest"
  mkdir -p "$large_src" "$large_dest"

  # Create a sparse 3GB file - exceeds 2GB to validate 64-bit size
  # handling in the wire protocol. Sparse creation uses no real disk.
  local expected_size=3221225472  # 3 * 1024^3
  if ! truncate -s 3G "${large_src}/bigfile.dat"; then
    echo "    truncate not available or failed"
    return 1
  fi

  # Write a small marker at the start so we can verify content
  echo "large-file-marker" > "${large_src}/marker.txt"

  # Compute source checksum before transfer
  local src_cksum
  if command -v md5sum >/dev/null 2>&1; then
    src_cksum=$(md5sum "${large_src}/bigfile.dat" | awk '{print $1}')
  elif command -v md5 >/dev/null 2>&1; then
    src_cksum=$(md5 -q "${large_src}/bigfile.dat")
  else
    echo "    no md5sum or md5 command available"
    return 1
  fi

  # Transfer via oc-rsync daemon to exercise 64-bit wire protocol sizes.
  # Start a dedicated daemon instance with large_dest as the module path.
  local large_conf="${work}/large-file.conf"
  local large_pid="${work}/large-file.pid"
  local large_log="${work}/large-file.log"
  cat > "$large_conf" <<CONF
pid file = ${large_pid}
port = ${oc_port}
use chroot = false

[largefile]
path = ${large_dest}
comment = large file 2gb test
read only = false
numeric ids = yes
CONF

  start_oc_daemon "$large_conf" "$large_log" "$upstream_binary" "$large_pid" "$oc_port"

  # Push large file to daemon via rsync:// protocol
  if ! timeout 180 "$upstream_binary" -avS --timeout=120 \
      "${large_src}/" "rsync://127.0.0.1:${oc_port}/largefile" \
      >"${log}.large.out" 2>"${log}.large.err"; then
    echo "    large file daemon transfer failed (exit=$?)"
    echo "    stderr: $(head -5 "${log}.large.err")"
    stop_oc_daemon
    rm -rf "$large_src" "$large_dest"
    return 1
  fi

  stop_oc_daemon

  if [[ ! -f "${large_dest}/bigfile.dat" ]]; then
    echo "    bigfile.dat missing from dest"
    rm -rf "$large_src" "$large_dest"
    return 1
  fi

  # Verify size matches - must be exactly 3GB (3221225472 bytes)
  local src_size dest_size
  src_size=$(stat -c %s "${large_src}/bigfile.dat" 2>/dev/null || stat -f %z "${large_src}/bigfile.dat" 2>/dev/null)
  dest_size=$(stat -c %s "${large_dest}/bigfile.dat" 2>/dev/null || stat -f %z "${large_dest}/bigfile.dat" 2>/dev/null)
  if [[ "$src_size" != "$dest_size" ]]; then
    echo "    size mismatch: src=${src_size} dest=${dest_size}"
    rm -rf "$large_src" "$large_dest"
    return 1
  fi

  if [[ "$dest_size" != "$expected_size" ]]; then
    echo "    unexpected dest size: ${dest_size} (expected ${expected_size})"
    rm -rf "$large_src" "$large_dest"
    return 1
  fi

  # Verify checksum matches to confirm data integrity
  local dest_cksum
  if command -v md5sum >/dev/null 2>&1; then
    dest_cksum=$(md5sum "${large_dest}/bigfile.dat" | awk '{print $1}')
  elif command -v md5 >/dev/null 2>&1; then
    dest_cksum=$(md5 -q "${large_dest}/bigfile.dat")
  fi

  if [[ "$src_cksum" != "$dest_cksum" ]]; then
    echo "    checksum mismatch: src=${src_cksum} dest=${dest_cksum}"
    rm -rf "$large_src" "$large_dest"
    return 1
  fi

  if ! cmp -s "${large_src}/marker.txt" "${large_dest}/marker.txt"; then
    echo "    marker.txt content mismatch"
    rm -rf "$large_src" "$large_dest"
    return 1
  fi

  # Clean up large files immediately to save disk
  rm -rf "$large_src" "$large_dest"
  return 0
}

# #879: file-vanished-mid-transfer
# Upstream rsync exits 24 (some files vanished) when a source file disappears
# during transfer. Uses --files-from to deterministically list a non-existent
# file alongside real files, avoiding race conditions. The sender discovers the
# listed file is gone and reports IOERR_VANISHED (exit 24).
test_file_vanished() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5

  local vanish_src="${work}/vanish-src"
  local vanish_dest="${work}/vanish-dest"
  rm -rf "$vanish_src" "$vanish_dest"
  mkdir -p "$vanish_src" "$vanish_dest"

  # Create several stable source files
  echo "stable content A" > "${vanish_src}/stable_a.txt"
  echo "stable content B" > "${vanish_src}/stable_b.txt"
  mkdir -p "${vanish_src}/subdir"
  echo "nested stable" > "${vanish_src}/subdir/nested.txt"

  # Create a --files-from list that includes a file that does not exist on disk.
  # rsync will stat this during transfer, find it missing, and set IOERR_VANISHED.
  local filelist="${work}/vanish-filelist.txt"
  printf 'stable_a.txt\nstable_b.txt\nvanished_file.dat\nsubdir/nested.txt\n' > "$filelist"

  timeout "$hard_timeout" "$oc_bin" -av --timeout=10 \
      --files-from="$filelist" \
      "${vanish_src}/" "${vanish_dest}/" \
      >"${log}.vanish.out" 2>"${log}.vanish.err"
  local rc=$?

  # Exit code 24 means "some files vanished before transfer" - expected
  # Exit code 23 means "partial transfer due to error" - also acceptable
  if [[ $rc -ne 24 && $rc -ne 23 ]]; then
    echo "    unexpected exit code $rc (expected 23 or 24)"
    echo "    stderr: $(head -5 "${log}.vanish.err")"
    return 1
  fi

  # All stable files should still be transferred successfully
  for f in stable_a.txt stable_b.txt subdir/nested.txt; do
    if [[ ! -f "${vanish_dest}/$f" ]]; then
      echo "    $f missing - partial transfer broke remaining files"
      return 1
    fi
    if ! cmp -s "${vanish_src}/$f" "${vanish_dest}/$f"; then
      echo "    $f content mismatch"
      return 1
    fi
  done

  # The vanished file should not be in the destination
  if [[ -f "${vanish_dest}/vanished_file.dat" ]]; then
    echo "    vanished_file.dat should not exist in destination"
    return 1
  fi

  rm -f "$filelist"
  return 0
}

# #880: --copy-unsafe-links + --safe-links interaction
# --copy-unsafe-links converts absolute symlinks to regular files while
# --safe-links drops symlinks pointing outside the transfer tree. When both
# are combined, unsafe links should be copied as files and truly unsafe
# (absolute) links should be transferred as file content.
test_copy_unsafe_safe_links() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5

  local link_src="${work}/unsafe-links-src"
  local link_dest="${work}/unsafe-links-dest"
  rm -rf "$link_src" "$link_dest"
  mkdir -p "$link_src/subdir" "$link_dest"

  # Create test files
  echo "target content" > "${link_src}/target.txt"
  echo "sub content" > "${link_src}/subdir/sub.txt"

  # Safe relative symlink (within transfer tree)
  ln -sf target.txt "${link_src}/safe_rel.txt"

  # Unsafe relative symlink (points outside tree via ..)
  echo "outside content" > "${work}/outside.txt"
  ln -sf "../../outside.txt" "${link_src}/subdir/unsafe_rel.txt"

  # Absolute symlink (always unsafe)
  ln -sf /etc/hostname "${link_src}/abs_link.txt" 2>/dev/null || true

  # Transfer with --copy-unsafe-links --safe-links
  timeout "$hard_timeout" "$oc_bin" -rlptv \
      --copy-unsafe-links --safe-links --timeout=10 \
      "${link_src}/" "${link_dest}/" \
      >"${log}.unsafe-safe.out" 2>"${log}.unsafe-safe.err"
  local rc=$?

  if [[ $rc -ne 0 ]]; then
    echo "    transfer failed (exit=$rc)"
    echo "    stderr: $(head -5 "${log}.unsafe-safe.err")"
    return 1
  fi

  # target.txt should be transferred normally
  if [[ ! -f "${link_dest}/target.txt" ]]; then
    echo "    target.txt missing"
    return 1
  fi

  # safe_rel.txt should be preserved as a symlink
  if [[ ! -L "${link_dest}/safe_rel.txt" ]]; then
    echo "    safe relative symlink not preserved"
    return 1
  fi

  # unsafe_rel.txt should have been copied as a regular file
  # (--copy-unsafe-links converts it)
  if [[ -L "${link_dest}/subdir/unsafe_rel.txt" ]]; then
    echo "    unsafe relative symlink was not dereferenced"
    return 1
  fi

  return 0
}

# #881: pre-xfer exec / post-xfer exec daemon hooks
# Upstream rsyncd.conf supports "pre-xfer exec" and "post-xfer exec" module
# parameters that run scripts before/after a transfer. Verify our daemon
# handles these hooks.
test_pre_post_xfer_exec() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5 \
        oc_port=$6

  local xfer_dest="${work}/xfer-exec-dest"
  local pre_marker="${work}/pre-xfer-ran.marker"
  local post_marker="${work}/post-xfer-ran.marker"
  rm -rf "$xfer_dest" "$pre_marker" "$post_marker"
  mkdir -p "$xfer_dest"

  # Create hook scripts
  local pre_script="${work}/pre-xfer.sh"
  local post_script="${work}/post-xfer.sh"
  cat > "$pre_script" <<'SCRIPT'
#!/bin/sh
touch "$RSYNC_MODULE_PATH/../pre-xfer-ran.marker"
exit 0
SCRIPT
  chmod 755 "$pre_script"

  cat > "$post_script" <<'SCRIPT'
#!/bin/sh
touch "$RSYNC_MODULE_PATH/../post-xfer-ran.marker"
exit 0
SCRIPT
  chmod 755 "$post_script"

  # Write custom daemon conf with pre/post-xfer exec
  local xfer_conf="${work}/xfer-exec.conf"
  local xfer_pid="${work}/xfer-exec.pid"
  local xfer_log="${work}/xfer-exec.log"
  cat > "$xfer_conf" <<CONF
pid file = ${xfer_pid}
port = ${oc_port}
use chroot = false

[interop]
path = ${xfer_dest}
comment = xfer exec test
read only = false
numeric ids = yes
pre-xfer exec = ${pre_script}
post-xfer exec = ${post_script}
CONF

  start_oc_daemon "$xfer_conf" "$xfer_log" "$upstream_binary" "$xfer_pid" "$oc_port"

  if ! timeout "$hard_timeout" "$upstream_binary" -av --timeout=10 \
      "${src_dir}/" "rsync://127.0.0.1:${oc_port}/interop" \
      >"${log}.xfer-exec.out" 2>"${log}.xfer-exec.err"; then
    echo "    transfer failed (exit=$?)"
    stop_oc_daemon
    return 1
  fi

  stop_oc_daemon

  # Verify the transfer completed
  if ! comp_verify_transfer "$src_dir" "$xfer_dest"; then
    echo "    content verification failed"
    return 1
  fi

  # Check that hooks ran
  if [[ ! -f "$pre_marker" ]]; then
    echo "    pre-xfer exec did not run"
    return 1
  fi

  if [[ ! -f "$post_marker" ]]; then
    echo "    post-xfer exec did not run"
    return 1
  fi

  return 0
}

# #882: read-only module rejection
# When a module is configured as "read only = true", push transfers should
# be rejected by the daemon with an appropriate error.
test_read_only_module() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5 \
        oc_port=$6 upstream_port=$7

  local ro_dest="${work}/readonly-dest"
  rm -rf "$ro_dest"
  mkdir -p "$ro_dest"

  # Test 1: upstream client -> oc-rsync daemon with read-only module
  local ro_conf="${work}/readonly-oc.conf"
  local ro_pid="${work}/readonly-oc.pid"
  local ro_log="${work}/readonly-oc.log"
  cat > "$ro_conf" <<CONF
pid file = ${ro_pid}
port = ${oc_port}
use chroot = false

[interop]
path = ${ro_dest}
comment = read-only module
read only = true
numeric ids = yes
CONF

  start_oc_daemon "$ro_conf" "$ro_log" "$upstream_binary" "$ro_pid" "$oc_port"

  # Push to a read-only module should fail
  timeout "$hard_timeout" "$upstream_binary" -av --timeout=10 \
      "${src_dir}/" "rsync://127.0.0.1:${oc_port}/interop" \
      >"${log}.readonly-oc.out" 2>"${log}.readonly-oc.err"
  local rc=$?

  stop_oc_daemon

  if [[ $rc -eq 0 ]]; then
    echo "    push to read-only oc module succeeded (should have been rejected)"
    return 1
  fi

  # Test 2: oc-rsync client -> upstream daemon with read-only module
  local ro_up_dest="${work}/readonly-up-dest"
  rm -rf "$ro_up_dest"
  mkdir -p "$ro_up_dest"

  local ro_up_conf="${work}/readonly-up.conf"
  local ro_up_pid="${work}/readonly-up.pid"
  local ro_up_log="${work}/readonly-up.log"
  cat > "$ro_up_conf" <<CONF
pid file = ${ro_up_pid}
port = ${upstream_port}
use chroot = false
munge symlinks = false
${up_identity}numeric ids = yes
[interop]
    path = ${ro_up_dest}
    comment = read-only upstream
    read only = true
CONF

  start_upstream_daemon "$upstream_binary" "$ro_up_conf" "$ro_up_log" "$ro_up_pid"

  timeout "$hard_timeout" "$oc_bin" -av --timeout=10 \
      "${src_dir}/" "rsync://127.0.0.1:${upstream_port}/interop" \
      >"${log}.readonly-up.out" 2>"${log}.readonly-up.err"
  rc=$?

  stop_upstream_daemon

  if [[ $rc -eq 0 ]]; then
    echo "    push to read-only upstream module succeeded (should have been rejected)"
    return 1
  fi

  return 0
}

# #883: wrong password authentication rejection
# When a module requires authentication and the wrong password is provided,
# the daemon should reject the connection.
test_wrong_password_auth() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5 \
        oc_port=$6 upstream_port=$7

  local auth_dest="${work}/auth-dest"
  rm -rf "$auth_dest"
  mkdir -p "$auth_dest"

  # Create secrets file for upstream daemon
  local secrets_file="${work}/rsyncd.secrets"
  echo "testuser:correctpassword" > "$secrets_file"
  chmod 600 "$secrets_file"

  # Create wrong password file for client
  local wrong_pass_file="${work}/wrong.pass"
  echo "wrongpassword" > "$wrong_pass_file"
  chmod 600 "$wrong_pass_file"

  # Test: oc-rsync client -> upstream daemon with wrong password
  local auth_conf="${work}/auth-up.conf"
  local auth_pid="${work}/auth-up.pid"
  local auth_log="${work}/auth-up.log"
  cat > "$auth_conf" <<CONF
pid file = ${auth_pid}
port = ${upstream_port}
use chroot = false
munge symlinks = false
${up_identity}numeric ids = yes
[interop]
    path = ${auth_dest}
    comment = auth test
    read only = false
    auth users = testuser
    secrets file = ${secrets_file}
CONF

  start_upstream_daemon "$upstream_binary" "$auth_conf" "$auth_log" "$auth_pid"

  # Try with wrong password
  RSYNC_PASSWORD="wrongpassword" \
  timeout "$hard_timeout" "$oc_bin" -av --timeout=10 \
      "${src_dir}/" "rsync://testuser@127.0.0.1:${upstream_port}/interop" \
      >"${log}.wrongpass.out" 2>"${log}.wrongpass.err"
  local rc=$?

  stop_upstream_daemon

  if [[ $rc -eq 0 ]]; then
    echo "    auth with wrong password succeeded (should have failed)"
    return 1
  fi

  # Verify error message mentions auth failure
  if ! grep -qiE 'auth|denied|unauthorized|password|refused' \
      "${log}.wrongpass.err" "${log}.wrongpass.out" 2>/dev/null; then
    echo "    no auth error message in output (exit=$rc)"
    echo "    stderr: $(cat "${log}.wrongpass.err" 2>/dev/null)"
    # Still pass if exit code is non-zero - the important thing is rejection
  fi

  # Verify no files were transferred
  local file_count
  file_count=$(find "$auth_dest" -type f 2>/dev/null | wc -l)
  if [[ "$file_count" -gt 0 ]]; then
    echo "    files were transferred despite wrong password"
    return 1
  fi

  return 0
}

# #884: --iconv charset conversion
# Upstream rsync supports --iconv=LOCAL,REMOTE for filename charset conversion.
# Verify that our implementation handles (or gracefully rejects) this option.
# Tests identity conversion (UTF-8,UTF-8), cross-charset conversion
# (UTF-8,ISO-8859-1), and filenames with accented chars, umlauts, and CJK.
test_iconv() {
  local upstream_binary=$1 oc_bin=$2 src_dir=$3 work=$4 log=$5

  local iconv_src="${work}/iconv-src"
  local iconv_dest="${work}/iconv-dest"
  local iconv_dest2="${work}/iconv-dest2"
  rm -rf "$iconv_src" "$iconv_dest" "$iconv_dest2"
  mkdir -p "$iconv_src" "$iconv_dest" "$iconv_dest2"

  # Create files with ASCII names (safe baseline)
  echo "ascii content" > "${iconv_src}/ascii.txt"
  echo "another file" > "${iconv_src}/plain.txt"

  # Create files with various non-ASCII UTF-8 filenames
  local has_utf8=true
  # Accented characters (Latin-1 compatible)
  echo "café content" > "${iconv_src}/café.txt" 2>/dev/null || has_utf8=false
  if $has_utf8; then
    # German umlauts
    echo "umlaut content" > "${iconv_src}/über-größe.txt" 2>/dev/null || true
    # Accented vowels
    echo "accented content" > "${iconv_src}/résumé.txt" 2>/dev/null || true
    # Nordic characters
    echo "nordic content" > "${iconv_src}/ångström.txt" 2>/dev/null || true
    # Create a subdirectory with non-ASCII name
    mkdir -p "${iconv_src}/données" 2>/dev/null && \
      echo "subdir content" > "${iconv_src}/données/fichier.txt" 2>/dev/null || true
  fi

  # --- Test 1: identity conversion (UTF-8 to UTF-8) ---
  timeout "$hard_timeout" "$oc_bin" -av --iconv=UTF-8,UTF-8 --timeout=10 \
      "${iconv_src}/" "${iconv_dest}/" \
      >"${log}.iconv.out" 2>"${log}.iconv.err"
  local rc=$?

  # Accept either success or graceful rejection (exit code 2 = syntax/usage)
  if [[ $rc -ne 0 ]]; then
    # Check if it was a graceful rejection vs crash
    if [[ $rc -eq 2 ]] || grep -qiE 'iconv|not supported|charset' \
        "${log}.iconv.err" 2>/dev/null; then
      echo "    --iconv gracefully rejected (not implemented)"
      return 1
    fi
    echo "    transfer failed with unexpected exit code $rc"
    echo "    stderr: $(head -5 "${log}.iconv.err")"
    return 1
  fi

  # Verify ASCII baseline files transferred correctly
  if [[ ! -f "${iconv_dest}/ascii.txt" ]]; then
    echo "    ascii.txt missing after iconv identity transfer"
    return 1
  fi

  if ! cmp -s "${iconv_src}/ascii.txt" "${iconv_dest}/ascii.txt"; then
    echo "    ascii.txt content mismatch after iconv identity transfer"
    return 1
  fi

  # Verify non-ASCII filenames survived the identity conversion
  if $has_utf8; then
    for fname in "café.txt" "résumé.txt" "über-größe.txt" "ångström.txt"; do
      if [[ -f "${iconv_src}/${fname}" && ! -f "${iconv_dest}/${fname}" ]]; then
        echo "    ${fname} missing after iconv identity transfer"
        return 1
      fi
    done
    # Check subdirectory with non-ASCII name
    if [[ -d "${iconv_src}/données" && ! -f "${iconv_dest}/données/fichier.txt" ]]; then
      echo "    données/fichier.txt missing after iconv identity transfer"
      return 1
    fi
  fi

  # --- Test 2: cross-charset conversion (UTF-8 local, ISO-8859-1 remote) ---
  # This tests actual charset transcoding. Latin-1 compatible characters
  # (accented chars, umlauts) should convert cleanly.
  timeout "$hard_timeout" "$oc_bin" -av --iconv=UTF-8,ISO-8859-1 --timeout=10 \
      "${iconv_src}/" "${iconv_dest2}/" \
      >"${log}.iconv-cross.out" 2>"${log}.iconv-cross.err"
  local rc2=$?

  if [[ $rc2 -ne 0 ]]; then
    if [[ $rc2 -eq 2 ]] || grep -qiE 'iconv|not supported|charset' \
        "${log}.iconv-cross.err" 2>/dev/null; then
      echo "    --iconv=UTF-8,ISO-8859-1 gracefully rejected"
      # Identity conversion passed, so partial success - still return 1
      # since cross-charset is not yet supported
      return 1
    fi
    echo "    cross-charset transfer failed with unexpected exit code $rc2"
    echo "    stderr: $(head -5 "${log}.iconv-cross.err")"
    return 1
  fi

  # If cross-charset transfer succeeded, verify ASCII files are intact
  if [[ ! -f "${iconv_dest2}/ascii.txt" ]]; then
    echo "    ascii.txt missing after cross-charset iconv transfer"
    return 1
  fi

  if ! cmp -s "${iconv_src}/ascii.txt" "${iconv_dest2}/ascii.txt"; then
    echo "    ascii.txt content mismatch after cross-charset iconv transfer"
    return 1
  fi

  return 0
}

# Run all standalone interop tests.
# Uses globals: $oc_client, $up_identity, $hard_timeout, $comp_src, $workdir.
run_standalone_interop_tests() {
  local upstream_binary=$1 oc_port=$2 upstream_port=$3

  local total=0 passed=0 known=0 unexpected=0
  local standalone_log="${workdir}/standalone"

  local test_names=(
    "write-batch-read-batch"
    "info-progress2"
    "large-file-2gb"
    "file-vanished"
    "copy-unsafe-safe-links"
    "pre-post-xfer-exec"
    "read-only-module"
    "wrong-password-auth"
    "iconv"
  )
  local test_funcs=(
    "test_write_batch_read_batch"
    "test_info_progress2"
    "test_large_file_2gb"
    "test_file_vanished"
    "test_copy_unsafe_safe_links"
    "test_pre_post_xfer_exec"
    "test_read_only_module"
    "test_wrong_password_auth"
    "test_iconv"
  )

  for i in "${!test_names[@]}"; do
    local name="${test_names[$i]}"
    local func="${test_funcs[$i]}"
    total=$((total + 1))

    local test_args=("$upstream_binary" "$oc_client" "$comp_src" "$workdir" "$standalone_log")

    # Some tests need ports
    case "$name" in
      write-batch-read-batch)
        test_args+=("$oc_port" "$upstream_port")
        ;;
      large-file-2gb)
        test_args+=("$oc_port" "$upstream_port")
        ;;
      pre-post-xfer-exec)
        test_args+=("$oc_port")
        ;;
      read-only-module)
        test_args+=("$oc_port" "$upstream_port")
        ;;
      wrong-password-auth)
        test_args+=("$oc_port" "$upstream_port")
        ;;
    esac

    local rc=0
    run_standalone_test "$name" "$func" "${test_args[@]}" || rc=$?

    if [[ $rc -eq 0 ]]; then
      passed=$((passed + 1))
    elif [[ $rc -eq 2 ]]; then
      known=$((known + 1))
    else
      unexpected=$((unexpected + 1))
    fi
  done

  echo "  === Standalone: ${passed}/${total} passed, ${known} known, ${unexpected} unexpected ==="
  return "$unexpected"
}

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

  # Core scenarios run against all versions. Extended scenarios only run
  # against 3.4.1 to keep CI within time limits (~45 scenarios x 3 versions
  # x 2 directions = 270 transfers at ~10s each = 45 min for native alone).
  local -a scenarios=(
    "archive|-av|basic"
    "relative|-avR|basic"
    "checksum|-avc|basic"
    "compress|-avz|compress"
    "whole-file|-avW|whole-file"
    "delta|-av --no-whole-file -I|delta"
    "inplace|-av --inplace|inplace"
    "numeric-ids|-av --numeric-ids|numeric-ids"
    "symlinks|-rlptv|symlinks"
    "hardlinks|-avH|hardlinks"
    "delete|-av --delete|delete"
    "exclude|-av --exclude=*.log|exclude"
    "permissions|-rlpv|perms"
    "itemize|-avi|itemize"
    "acls|-avA|acls"
    "xattrs|-avX|xattrs"
  )

  # Extended scenarios only for the newest upstream version (3.4.1).
  if [[ "${version}" == "3.4.1" ]]; then
    scenarios+=(
      "one-file-system|-avx|basic"
      "whole-file-replace|-avW|whole-file-replace"
      "delay-updates|-av --delay-updates|basic"
      "recursive-only|-rv|recursive"
      "delete-after|-av --delete-after|delete"
      "delete-during|-av --delete-during|delete"
      "include-exclude|-rv --include=*.txt --include=*/ --exclude=*|include-exclude"
      "filter-rule|-av --exclude=*.tmp|filter-rule"
      "merge-filter|-av -FF|merge-filter"
      "exclude-from|-av --exclude-from=exclude_patterns.txt|exclude-from"
      "size-only|-av --size-only|size-only"
      "ignore-times|-av --ignore-times|basic"
      "checksum-skip|-avc|checksum-skip"
      "copy-links|-avL|copy-links"
      "safe-links|-rlptv --safe-links|safe-links"
      "existing|-av --existing|existing"
      "backup|-av --backup|backup"
      "link-dest|-av --link-dest=link_ref|link-dest"
      "max-delete|-av --delete --max-delete=1|max-delete"
      "update|-av --update|update"
      "dry-run|-avn|dry-run"
      "sparse|-avS|sparse"
      "partial|-av --partial|partial"
      "append|-av --append|append"
      "bwlimit|-av --bwlimit=10000|bwlimit"
      "compress-level-1|-avz --compress-level=1|basic"
      "compress-level-9|-avz --compress-level=9|basic"
      "protocol-30|-av --protocol=30|basic"
      "protocol-31|-av --protocol=31|basic"
      "compress-delta|-avz --no-whole-file -I|delta"
      "devices|-avD|basic"
      "compare-dest|-av --compare-dest=compare_ref|compare-dest"
      "files-from|-av --files-from=filelist.txt|files-from"
      "hardlinks-relative|-avHR|hardlinks-relative"
    )
  fi

  # Protocol-31 requires upstream rsync >= 3.1.0 (protocol 31 support).
  # rsync 3.0.x only supports up to protocol 30, so --protocol=31 fails.
  if [[ "${version}" == 3.0.* ]]; then
    local -a filtered=()
    for s in "${scenarios[@]}"; do
      [[ "$s" != "protocol-31|"* ]] && filtered+=("$s")
    done
    scenarios=("${filtered[@]}")
  fi

  # Incremental recursion only supported on protocol 30+
  local fp=""; [[ -n "$protocol_flag" ]] && fp="${protocol_flag##*=}"
  if [[ -z "$fp" || "$fp" -ge 30 ]]; then
    scenarios+=("inc-recursive|-av --inc-recursive|inc-recursive")
  fi

  local total=0 passed=0 known=0 unexpected=0

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
      if is_known_failure "up" "$name" "$fp"; then
        echo "    SKIP (known limitation)"
        known=$((known + 1))
      else
        echo "    UNEXPECTED FAIL: up:${name} (fp=${fp:-native})"
        unexpected=$((unexpected + 1))
      fi
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
      if is_known_failure "oc" "$name" "$fp"; then
        echo "    SKIP (known limitation)"
        known=$((known + 1))
      else
        echo "    UNEXPECTED FAIL: oc:${name} (fp=${fp:-native})"
        unexpected=$((unexpected + 1))
      fi
    fi
  done

  stop_upstream_daemon

  # SSH interop (only if SSH is available)
  if command -v ssh >/dev/null 2>&1; then
    total=$((total + 1))
    echo "  [oc-rsync SSH] local SSH transfer${sfx}"
    local ssh_dir="${workdir}/ssh-${tag}"
    mkdir -p "$ssh_dir"
    if run_ssh_interop_test "$oc_client" "$comp_src" "$ssh_dir" "$olf"; then
      echo "    PASS"
      passed=$((passed + 1))
    else
      if is_known_failure "oc" "ssh-transfer" "$fp"; then
        echo "    SKIP (known limitation)"
        known=$((known + 1))
      else
        echo "    UNEXPECTED FAIL: oc:ssh-transfer (fp=${fp:-native})"
        unexpected=$((unexpected + 1))
      fi
    fi
  fi

  local fail=$((known + unexpected))
  echo "  === ${version}${sfx}: ${passed}/${total} passed, ${known} known, ${unexpected} unexpected ==="
  return "$unexpected"
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

failed=()

# =====================================================================
# Parallel version testing: basic + comprehensive tests run concurrently
# for each upstream version. Each version runs in a subshell to isolate
# mutable daemon PID globals. Results are collected via temp files.
# =====================================================================

comp_src="${workdir}/comp-source"
setup_comprehensive_src "$comp_src"

# Directory for per-version failure results
result_dir="${workdir}/parallel-results"
mkdir -p "$result_dir"

# Run all version tests (basic + comprehensive) in parallel subshells.
# Each subshell gets its own copy of daemon PID globals, unique ports,
# and unique temp directories. The read-only globals (comp_src, src,
# oc_client, oc_binary, up_identity, hard_timeout) are inherited safely.
version_pids=()
for version in "${versions[@]}"; do
  (
    # Subshell: isolated daemon PID state
    oc_pid=""
    up_pid=""
    oc_pid_file_current=""
    up_pid_file_current=""
    oc_port_current=""
    up_port_current=""

    version_failed=()
    upstream_binary="${upstream_install_root}/${version}/bin/rsync"
    if [[ ! -x "$upstream_binary" ]]; then
      echo "Missing upstream rsync binary for version ${version}" >&2
      version_failed+=("${version} (missing)")
    else
      # Comprehensive interop test (includes archive scenario that covers basic push/pull)
      oc_port=$(allocate_ephemeral_port)
      up_port=$(allocate_ephemeral_port)
      echo ""
      echo "=== Comprehensive: upstream ${version} (native protocol) (ports: oc=${oc_port} up=${up_port}) ==="
      if ! run_comprehensive_interop_case "$version" "$upstream_binary" \
          "$oc_port" "$up_port"; then
        version_failed+=("${version}")
      fi
    fi

    # Clean up any daemons started in this subshell
    stop_oc_daemon
    stop_upstream_daemon

    # Write failures to a version-specific result file
    if (( ${#version_failed[@]} > 0 )); then
      printf '%s\n' "${version_failed[@]}" > "${result_dir}/${version}.failures"
    fi
  ) &
  version_pids+=("$!")
  echo "Launched version ${version} tests (PID: ${version_pids[-1]})"
done

echo ""
echo "=== Waiting for ${#version_pids[@]} parallel version tests ==="

# Wait for all version subshells and track which ones failed
version_exit_failures=()
for i in "${!versions[@]}"; do
  version="${versions[$i]}"
  pid="${version_pids[$i]}"
  if ! wait "$pid"; then
    # Subshell exited non-zero (e.g., set -e triggered)
    version_exit_failures+=("${version}-subshell-error")
  fi
done

# Collect failures from result files
for version in "${versions[@]}"; do
  if [[ -f "${result_dir}/${version}.failures" ]]; then
    while IFS= read -r failure; do
      failed+=("$failure")
    done < "${result_dir}/${version}.failures"
  fi
done
for f in "${version_exit_failures[@]}"; do
  failed+=("$f")
done

echo "=== Parallel version tests complete ==="

# =====================================================================
# Protocol version forcing tests: all 5 protocols via upstream 3.4.1
# Run in parallel - each protocol uses unique ports and temp dirs.
# =====================================================================
newest_binary="${upstream_install_root}/3.4.1/bin/rsync"
if [[ -x "$newest_binary" ]]; then
  proto_pids=()
  protos=(28 29 30 31 32)
  for proto in "${protos[@]}"; do
    (
      oc_pid=""
      up_pid=""
      oc_pid_file_current=""
      up_pid_file_current=""
      oc_port_current=""
      up_port_current=""

      oc_port=$(allocate_ephemeral_port)
      up_port=$(allocate_ephemeral_port)
      echo ""
      echo "=== Protocol ${proto} (forced via --protocol=${proto}) (ports: oc=${oc_port} up=${up_port}) ==="
      proto_failed=false
      if ! run_comprehensive_interop_case "3.4.1" "$newest_binary" \
          "$oc_port" "$up_port" "--protocol=${proto}"; then
        proto_failed=true
      fi

      stop_oc_daemon
      stop_upstream_daemon

      if [[ "$proto_failed" == "true" ]]; then
        echo "proto${proto}" > "${result_dir}/proto${proto}.failures"
      fi
    ) &
    proto_pids+=("$!")
    echo "Launched protocol ${proto} tests (PID: ${proto_pids[-1]})"
  done

  echo ""
  echo "=== Waiting for ${#proto_pids[@]} parallel protocol tests ==="

  for i in "${!protos[@]}"; do
    proto="${protos[$i]}"
    pid="${proto_pids[$i]}"
    if ! wait "$pid"; then
      failed+=("proto${proto}-subshell-error")
    fi
  done

  for proto in "${protos[@]}"; do
    if [[ -f "${result_dir}/proto${proto}.failures" ]]; then
      while IFS= read -r failure; do
        failed+=("$failure")
      done < "${result_dir}/proto${proto}.failures"
    fi
  done

  echo "=== Parallel protocol tests complete ==="
else
  echo "Skipping protocol forcing tests (3.4.1 binary unavailable)"
fi

# =====================================================================
# Standalone interop tests (#876-#884): batch, progress2, large files,
# vanished files, link interactions, daemon hooks, auth, iconv
# =====================================================================
echo ""
echo "=== Standalone Interop Tests ==="

# Use the newest available upstream binary for standalone tests
standalone_binary="${upstream_install_root}/3.4.1/bin/rsync"
if [[ ! -x "$standalone_binary" ]]; then
  # Fall back to any available version
  for v in "${versions[@]}"; do
    if [[ -x "${upstream_install_root}/${v}/bin/rsync" ]]; then
      standalone_binary="${upstream_install_root}/${v}/bin/rsync"
      break
    fi
  done
fi

if [[ -x "$standalone_binary" ]]; then
  oc_port=$(allocate_ephemeral_port)
  up_port=$(allocate_ephemeral_port)
  if ! run_standalone_interop_tests "$standalone_binary" \
      "$oc_port" "$up_port"; then
    failed+=("standalone")
  fi
else
  echo "Skipping standalone tests (no upstream binary available)"
fi

# Final report
if (( ${#failed[@]} > 0 )); then
  echo ""
  echo "Interop failures: ${failed[*]}" >&2
  exit 1
fi

echo ""
echo "All interoperability checks succeeded (basic + comprehensive + protocols 28-32 + standalone)."
