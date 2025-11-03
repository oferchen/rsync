#!/usr/bin/env bash
# Changes made:
# 1. Repaired daemon lifecycle logic: stop_oc_daemon() and stop_upstream_daemon() are now simple, self-contained
#    functions using global state (oc_pid, up_pid, *_pid_file_current) instead of an incomplete subshell block.
# 2. Fixed config generation in run_interop_case(): it now uses the correct per-daemon identity variables
#    (oc_identity, up_identity) instead of the non-existent ${identity}.
# 3. Ensured a single global workdir with a single EXIT trap that cleans up both daemons and the temp directory.
# 4. Kept your original build flow (workspace bins → upstream rsync builds) and preserved feature-disable
#    probing during ./configure.
# 5. Kept everything POSIX-safe under bash with set -euo pipefail and explicit return paths.

set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target_dir="${workspace_root}/target/dist"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_install_root="${workspace_root}/target/interop/upstream-install"
versions=(3.0.9 3.1.3 3.4.1)

# Global daemon state
oc_pid=""
up_pid=""
oc_pid_file_current=""
up_pid_file_current=""
workdir=""

ensure_workspace_binaries() {
  if [[ -x "${target_dir}/oc-rsync" && -x "${target_dir}/oc-rsyncd" ]]; then
    return
  fi

  cargo --locked build --profile dist --bin oc-rsync --bin oc-rsyncd
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

ensure_upstream_build() {
  local version=$1
  local archive="rsync-${version}.tar.gz"
  local url="https://rsync.samba.org/ftp/rsync/src/${archive}"
  local src_dir="${upstream_src_root}/rsync-${version}"
  local install_dir="${upstream_install_root}/${version}"
  local binary="${install_dir}/bin/rsync"

  if [[ -x "$binary" ]]; then
    if "$binary" --version | head -n1 | grep -q "rsync\s\+version\s\+${version}\b"; then
      return
    fi
    rm -rf "$install_dir"
  fi

  rm -rf "$src_dir"
  mkdir -p "$upstream_src_root" "$upstream_install_root"

  echo "Building upstream rsync ${version}"
  curl -L --fail --silent --show-error --retry 5 --retry-delay 2 "$url" \
    | tar -xz -C "$upstream_src_root"

  if [[ ! -d "$src_dir" ]]; then
    echo "Failed to extract upstream rsync ${version}" >&2
    exit 1
  fi

  pushd "$src_dir" >/dev/null

  if [[ ! -x configure ]]; then
    echo "Upstream rsync ${version} source tree is missing a configure script" >&2
    exit 1
  fi

  local configure_help
  configure_help=$(./configure --help)
  local -a configure_args=("--prefix=${install_dir}")

  # Keep your feature probing so builds succeed on older tarballs
  if grep -q -- "--disable-xxhash" <<<"$configure_help"; then
    configure_args+=("--disable-xxhash")
  fi
  if grep -q -- "--disable-lz4" <<<"$configure_help"; then
    configure_args+=("--disable-lz4")
  fi
  if grep -q -- "--disable-md2man" <<<"$configure_help"; then
    configure_args+=("--disable-md2man")
  fi

  ./configure "${configure_args[@]}" >/dev/null
  make -j"$(build_jobs)" >/dev/null
  make install >/dev/null
  popd >/dev/null
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

start_oc_daemon() {
  local config=$1
  local log_file=$2
  local fallback_client=$3
  local pid_file=$4

  oc_pid_file_current="$pid_file"

  OC_RSYNC_DAEMON_FALLBACK="$fallback_client" \
    OC_RSYNC_FALLBACK="$fallback_client" \
    "$oc_daemon" --config "$config" --daemon --no-detach --log-file "$log_file" &
  oc_pid=$!
  # Give it a moment to write pid file / listen
  sleep 1
}

start_upstream_daemon() {
  local binary=$1
  local config=$2
  local log_file=$3
  local pid_file=$4

  up_pid_file_current="$pid_file"
  "$binary" --daemon --config "$config" --no-detach --log-file "$log_file" &
  up_pid=$!
  sleep 1
}

run_interop_case() {
  local version=$1
  local upstream_binary=$2
  local oc_port=$3
  local upstream_port=$4

  local version_tag=${version//./-}
  local oc_dest="${workdir}/oc-destination-${version_tag}"
  local up_dest="${workdir}/upstream-destination-${version_tag}"
  local oc_pid_file="${workdir}/oc-rsyncd-${version_tag}.pid"
  local up_pid_file="${workdir}/upstream-rsyncd-${version_tag}.pid"
  local oc_conf="${workdir}/oc-rsyncd-${version_tag}.conf"
  local up_conf="${workdir}/upstream-rsyncd-${version_tag}.conf"
  local oc_log="${workdir}/oc-rsyncd-${version_tag}.log"
  local up_log="${workdir}/upstream-rsyncd-${version_tag}.log"

  rm -rf "$oc_dest" "$up_dest"
  mkdir -p "$oc_dest" "$up_dest"

  cat >"$oc_conf" <<OC_CONF
pid file = ${oc_pid_file}
port = ${oc_port}
use chroot = false
${oc_identity}
numeric ids = yes
[interop]
    path = ${oc_dest}
    comment = oc interop target (${version})
    read only = false
OC_CONF

  cat >"$up_conf" <<UP_CONF
pid file = ${up_pid_file}
port = ${upstream_port}
use chroot = false
${up_identity}
numeric ids = yes
[interop]
    path = ${up_dest}
    comment = upstream interop target (${version})
    read only = false
UP_CONF

  echo "Testing upstream rsync ${version} client -> oc-rsyncd"
  start_oc_daemon "$oc_conf" "$oc_log" "$upstream_binary" "$oc_pid_file"
  "$upstream_binary" -av --timeout=10 "${src}/" "rsync://127.0.0.1:${oc_port}/interop" >/dev/null

  if [[ ! -f "${oc_dest}/payload.txt" ]]; then
    echo "Upstream rsync ${version} client failed to transfer to oc-rsyncd" >&2
    exit 1
  fi

  stop_oc_daemon

  echo "Testing oc-rsync client -> upstream rsync ${version} daemon"
  start_upstream_daemon "$upstream_binary" "$up_conf" "$up_log" "$up_pid_file"
  "$oc_client" -av --timeout=10 "${src}/" "rsync://127.0.0.1:${upstream_port}/interop" >/dev/null

  if [[ ! -f "${up_dest}/payload.txt" ]]; then
    echo "oc-rsync client failed to transfer to upstream rsync ${version} daemon" >&2
    exit 1
  fi

  stop_upstream_daemon
}

# ----- main flow -----

ensure_workspace_binaries

mkdir -p "$upstream_src_root" "$upstream_install_root"

for version in "${versions[@]}"; do
  ensure_upstream_build "$version"
done

oc_client="${target_dir}/oc-rsync"
oc_daemon="${target_dir}/oc-rsyncd"

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
  # only set uid/gid when running as root to match your original logic
  printf -v oc_identity 'uid = %s\ngid = %s\n' "${uid}" "${gid}"
  printf -v up_identity 'uid = %s\ngid = %s\n' "${uid}" "${gid}"
fi

port_base=2873

for version in "${versions[@]}"; do
  upstream_binary="${upstream_install_root}/${version}/bin/rsync"
  if [[ ! -x "$upstream_binary" ]]; then
    echo "Missing upstream rsync binary for version ${version}" >&2
    exit 1
  fi

  echo "Running interoperability checks against upstream rsync ${version}"
  run_interop_case "$version" "$upstream_binary" "$port_base" $((port_base + 1))
  port_base=$((port_base + 2))
done

echo "All interoperability checks succeeded."
