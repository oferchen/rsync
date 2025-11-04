#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target_dir="${workspace_root}/target/dist"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_install_root="${workspace_root}/target/interop/upstream-install"

versions=(3.0.9 3.1.3 3.4.1)

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

# rust daemon is stricter: it wants directives in a module/section
write_rust_daemon_conf() {
  local path=$1
  local pid_file=$2
  local port=$3
  local dest=$4
  local comment=$5

  cat >"$path" <<CONF
[daemon]
path = ${dest}
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

# upstream classic rsyncd layout, still accepts top-level directives
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

start_oc_daemon() {
  local config=$1
  local log_file=$2
  local fallback_client=$3
  local pid_file=$4

  oc_pid_file_current="$pid_file"

  RUST_BACKTRACE=1 \
  OC_RSYNC_DAEMON_FALLBACK="$fallback_client" \
  OC_RSYNC_FALLBACK="$fallback_client" \
    "$oc_daemon" --config "$config" --daemon --no-detach --log-file "$log_file" &
  oc_pid=$!
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

  write_rust_daemon_conf "$oc_conf" "$oc_pid_file" "$oc_port" "$oc_dest" "oc interop target (${version})"
  write_upstream_conf "$up_conf" "$up_pid_file" "$upstream_port" "$up_dest" "upstream interop target (${version})" "$up_identity"

  echo "Testing upstream rsync ${version} client -> oc-rsyncd"
  start_oc_daemon "$oc_conf" "$oc_log" "$upstream_binary" "$oc_pid_file"

  if ! "$upstream_binary" -av --timeout=10 "${src}/" "rsync://127.0.0.1:${oc_port}/interop" >/dev/null 2>>"$oc_log"; then
    echo "FAIL: upstream rsync ${version} -> oc-rsyncd"
    echo "--- oc-rsyncd log (${oc_log}) ---"
    cat "$oc_log" || true
    stop_oc_daemon
    return 1
  fi

  if [[ ! -f "${oc_dest}/payload.txt" ]]; then
    echo "FAIL: upstream rsync ${version} reported success but file missing in oc dest"
    echo "--- oc-rsyncd log (${oc_log}) ---"
    cat "$oc_log" || true
    stop_oc_daemon
    return 1
  fi

  stop_oc_daemon

  echo "Testing oc-rsync client -> upstream rsync ${version} daemon"
  start_upstream_daemon "$upstream_binary" "$up_conf" "$up_log" "$up_pid_file"

  if ! "$oc_client" -av --timeout=10 "${src}/" "rsync://127.0.0.1:${upstream_port}/interop" >/dev/null 2>>"$up_log"; then
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

if (( ${#failed[@]} > 0 )); then
  echo "Interop failures: ${failed[*]}" >&2
  exit 1
fi

echo "All interoperability checks succeeded."
