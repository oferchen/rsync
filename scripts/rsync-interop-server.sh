#!/usr/bin/env bash
# rsync-interop-server.sh
# ---------------------------------------------------------------------------
# WHAT WAS DONE / WHY:
# - Ensures workspace oc-rsync is built.
# - Ensures upstream rsync 3.0.9 / 3.1.3 / 3.4.1 exist (Debian/Ubuntu-first, source fallback).
# - Starts per-version daemons:
#     * oc-rsync --daemon on a unique port
#     * upstream rsync --daemon on the next port
# - Writes per-version env descriptors into target/interop/run/<version>/env
#   so a client can discover everything.
# - Distinct ports for amd64 (28000+); portable fallback for others.
# - Clean trap to stop all daemons.
# ---------------------------------------------------------------------------
set -euo pipefail

if ! command -v curl >/dev/null 2>&1; then
  printf 'curl is required to fetch Ubuntu/Debian rsync packages\n' >&2
  exit 1
fi

if ! command -v tar >/dev/null 2>&1; then
  printf 'tar is required to unpack upstream rsync releases\n' >&2
  exit 1
fi

export GIT_TERMINAL_PROMPT=0

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
readonly workspace_root

target_dir="${workspace_root}/target/dist"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_install_root="${workspace_root}/target/interop/upstream-install"
run_root="${workspace_root}/target/interop/run"
readonly target_dir upstream_src_root upstream_install_root run_root

versions=(3.0.9 3.1.3 3.4.1)
readonly versions

rsync_repo_url="https://github.com/RsyncProject/rsync.git"
rsync_tarball_base_url="${RSYNC_TARBALL_BASE_URL:-https://rsync.samba.org/ftp/rsync/src}"
readonly rsync_repo_url rsync_tarball_base_url

DEBIAN_MIRROR="${DEBIAN_MIRROR:-https://deb.debian.org/debian}"
UBUNTU_MIRROR="${UBUNTU_MIRROR:-http://archive.ubuntu.com/ubuntu}"
OLD_UBUNTU_MIRROR="${OLD_UBUNTU_MIRROR:-https://old-releases.ubuntu.com/ubuntu}"
readonly DEBIAN_MIRROR UBUNTU_MIRROR OLD_UBUNTU_MIRROR

declare -a oc_pid_list=()
declare -a up_pid_list=()

detect_deb_arch() {
  local u
  u=$(uname -m)
  case "${u}" in
    x86_64)  printf 'amd64\n' ;;
    aarch64) printf 'arm64\n' ;;
    armv7l|armv6l) printf 'armhf\n' ;;
    i386|i686) printf 'i386\n' ;;
    ppc64le) printf 'ppc64el\n' ;;
    riscv64) printf 'riscv64\n' ;;
    *) printf 'amd64\n' ;;
  esac
}

ensure_workspace_binaries() {
  if [[ -x "${target_dir}/oc-rsync" ]]; then
    return
  fi
  (cd "${workspace_root}" && cargo --locked build --profile dist --bin oc-rsync)
}

build_jobs() {
  if command -v nproc >/dev/null 2>&1; then
    nproc
    return
  fi
  if command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.ncpu
    return
  fi
  printf '2\n'
}

build_version_url() {
  local version=$1
  local arch=$2
  case "${version}" in
    3.0.9)
      printf '%s/pool/main/r/rsync/rsync_3.0.9-1ubuntu1.3_%s.deb\n' "${OLD_UBUNTU_MIRROR}" "${arch}"
      ;;
    3.1.3)
      printf '%s/pool/main/r/rsync/rsync_3.1.3-8ubuntu0.9_%s.deb\n' "${UBUNTU_MIRROR}" "${arch}"
      ;;
    3.4.1)
      printf '%s/pool/main/r/rsync/rsync_3.4.1+ds1-6_%s.deb\n' "${DEBIAN_MIRROR}" "${arch}"
      ;;
    *)
      printf '%s/pool/main/r/rsync/rsync_%s-1_%s.deb\n' "${DEBIAN_MIRROR}" "${version}" "${arch}"
      ;;
  esac
}

extract_deb_payload() {
  local deb_path=$1
  local install_dir=$2
  if ! command -v ar >/dev/null 2>&1; then
    printf 'ar not available; cannot extract .deb (%s)\n' "${deb_path}" >&2
    return 1
  fi
  mkdir -p "${install_dir}"
  (
    cd "${install_dir}"
    ar x "${deb_path}" >/dev/null 2>&1
    if [[ -f data.tar.xz ]]; then
      tar -xf data.tar.xz
      rm -f data.tar.xz
    elif [[ -f data.tar.gz ]]; then
      tar -xzf data.tar.gz
      rm -f data.tar.gz
    else
      printf 'unexpected .deb layout: missing data.tar.* in %s\n' "${deb_path}" >&2
      exit 1
    fi
  )
  if [[ -x "${install_dir}/usr/bin/rsync" ]]; then
    mkdir -p "${install_dir}/bin"
    cp "${install_dir}/usr/bin/rsync" "${install_dir}/bin/rsync"
  else
    return 1
  fi
}

try_fetch_deb() {
  local url=$1
  local install_dir=$2
  local tmp_deb
  tmp_deb=$(mktemp)
  if ! curl -fsSL "${url}" -o "${tmp_deb}"; then
    rm -f "${tmp_deb}"
    return 1
  fi
  if ! extract_deb_payload "${tmp_deb}" "${install_dir}"; then
    rm -f "${tmp_deb}"
    return 1
  fi
  rm -f "${tmp_deb}"
  return 0
}

try_fetch_deb_generic() {
  local version=$1 arch=$2 install_dir=$3
  local tmp_deb
  tmp_deb=$(mktemp)
  local -a candidates=()

  case "${version}" in
    3.0.9)
      candidates=(
        "${OLD_UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.0.9-1ubuntu1_${arch}.deb"
        "${OLD_UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.0.9-1ubuntu1.1_${arch}.deb"
        "${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_3.0.9-4_${arch}.deb"
      )
      ;;
    3.1.3)
      candidates=("${UBUNTU_MIRROR}/pool/main/r/rsync/rsync_3.1.3-8ubuntu0.8_${arch}.deb")
      ;;
    3.4.1)
      candidates=("${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_3.4.1+ds1-5_${arch}.deb")
      ;;
    *)
      candidates=("${DEBIAN_MIRROR}/pool/main/r/rsync/rsync_${version}-1_${arch}.deb")
      ;;
  esac

  local url
  for url in "${candidates[@]}"; do
    if curl -fsSL "${url}" -o "${tmp_deb}" 2>/dev/null; then
      if extract_deb_payload "${tmp_deb}" "${install_dir}"; then
        rm -f "${tmp_deb}"
        return 0
      fi
    fi
  done
  rm -f "${tmp_deb}"
  return 1
}

fetch_upstream_tarball() {
  local version=$1 dest=$2
  local url="${rsync_tarball_base_url}/rsync-${version}.tar.gz"
  local tmp_tar
  tmp_tar=$(mktemp)
  if ! curl -fsSL "${url}" -o "${tmp_tar}"; then
    rm -f "${tmp_tar}"
    return 1
  fi
  mkdir -p "${upstream_src_root}"
  rm -rf "${dest}" "${upstream_src_root}/rsync-${version}"
  if ! tar -xzf "${tmp_tar}" -C "${upstream_src_root}" >/dev/null 2>&1; then
    rm -f "${tmp_tar}"
    rm -rf "${dest}"
    return 1
  fi
  rm -f "${tmp_tar}"
  [[ -d "${dest}" ]] || return 1
  return 0
}

clone_upstream_source() {
  local version=$1 dest=$2
  if ! command -v git >/dev/null 2>&1; then
    return 1
  fi
  local -a tags=("v${version}" "${version}")
  local tag
  for tag in "${tags[@]}"; do
    if git clone --depth 1 --branch "${tag}" "${rsync_repo_url}" "${dest}" >/dev/null 2>&1; then
      return 0
    fi
  done
  return 1
}

build_upstream_from_source() {
  local version=$1
  local src_dir="${upstream_src_root}/rsync-${version}"
  local install_dir="${upstream_install_root}/${version}"

  rm -rf "${src_dir}"
  mkdir -p "${upstream_src_root}" "${upstream_install_root}"

  printf 'Fetching upstream rsync %s sources\n' "${version}"
  if ! fetch_upstream_tarball "${version}" "${src_dir}"; then
    printf 'Falling back to git clone for %s\n' "${version}" >&2
    if ! clone_upstream_source "${version}" "${src_dir}"; then
      printf 'Failed to obtain upstream rsync %s sources\n' "${version}" >&2
      exit 1
    fi
  fi

  pushd "${src_dir}" >/dev/null

  if [[ ! -x configure && -x ./prepare-source ]]; then
    ./prepare-source >/dev/null
  fi
  if [[ ! -x configure ]]; then
    printf 'Upstream rsync %s is missing configure\n' "${version}" >&2
    exit 1
  fi

  local -a cfg=("--prefix=${install_dir}")
  local help
  help=$(./configure --help)
  if grep -q -- '--disable-xxhash' <<<"${help}"; then
    cfg+=("--disable-xxhash")
  fi
  if grep -q -- '--disable-lz4' <<<"${help}"; then
    cfg+=("--disable-lz4")
  fi
  if grep -q -- '--disable-md2man' <<<"${help}"; then
    cfg+=("--disable-md2man")
  fi

  ./configure "${cfg[@]}" >/dev/null
  make -j"$(build_jobs)" >/dev/null
  make install >/dev/null

  popd >/dev/null
}

ensure_upstream_build() {
  local version=$1
  local install_dir="${upstream_install_root}/${version}"
  local bin="${install_dir}/bin/rsync"
  local arch="${DEB_ARCH:-$(detect_deb_arch)}"

  if [[ -x "${bin}" ]] && "${bin}" --version | head -n1 | grep -q "rsync\s\+version\s\+${version}\b"; then
    return
  fi

  rm -rf "${install_dir}"
  mkdir -p "${install_dir}"

  local url
  url=$(build_version_url "${version}" "${arch}")
  printf 'Trying %s\n' "${url}"
  if try_fetch_deb "${url}" "${install_dir}"; then
    if "${install_dir}/bin/rsync" --version | head -n1 | grep -q "rsync\s\+version\s\+${version}\b"; then
      printf 'Using rsync %s from %s\n' "${version}" "${url}"
      return
    fi
    rm -rf "${install_dir}"
    mkdir -p "${install_dir}"
  fi

  printf 'Trying generic pool for %s (%s)\n' "${version}" "${arch}"
  if try_fetch_deb_generic "${version}" "${arch}" "${install_dir}"; then
    if "${install_dir}/bin/rsync" --version | head -n1 | grep -q "rsync\s\+version\s\+${version}\b"; then
      printf 'Using rsync %s from generic pool\n' "${version}"
      return
    fi
    rm -rf "${install_dir}"
  fi

  printf 'Building rsync %s from source\n' "${version}"
  build_upstream_from_source "${version}"
}

write_rust_daemon_conf() {
  local path=$1 pid_file=$2 port=$3 dest=$4 comment=$5
  cat >"${path}" <<CONF
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

write_upstream_conf() {
  local path=$1 pid_file=$2 port=$3 dest=$4 comment=$5 identity=$6
  cat >"${path}" <<CONF
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

start_oc_daemon() {
  local bin=$1 conf=$2 log=$3 pid_file=$4 port=$5 fallback=$6
  OC_RSYNC_DAEMON_FALLBACK="${fallback}" \
  OC_RSYNC_FALLBACK="${fallback}" \
    "${bin}" --daemon --config "${conf}" --port "${port}" --log-file "${log}" &
  oc_pid_list+=("$!")
  sleep 1
}

start_upstream_daemon() {
  local bin=$1 conf=$2 log=$3
  "${bin}" --daemon --config "${conf}" --no-detach --log-file "${log}" &
  up_pid_list+=("$!")
  sleep 1
}

cleanup() {
  local p
  for p in "${oc_pid_list[@]:-}"; do
    kill "${p}" >/dev/null 2>&1 || true
    wait "${p}" >/dev/null 2>&1 || true
  done
  for p in "${up_pid_list[@]:-}"; do
    kill "${p}" >/dev/null 2>&1 || true
    wait "${p}" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

ensure_workspace_binaries
mkdir -p "${upstream_src_root}" "${upstream_install_root}" "${run_root}"

local_arch=$(detect_deb_arch)
readonly local_arch

for v in "${versions[@]}"; do
  ensure_upstream_build "${v}"
done

oc_client="${target_dir}/oc-rsync"
readonly oc_client

if [[ "${local_arch}" == "amd64" ]]; then
  port_base=28000
else
  port_base=2873
fi
readonly port_base

uid=$(id -u)
gid=$(id -g)
up_identity=""
if [[ ${uid} -eq 0 ]]; then
  printf -v up_identity 'uid = %s\ngid = %s\n' "${uid}" "${gid}"
fi
readonly up_identity

idx=0
for version in "${versions[@]}"; do
  version_tag=${version//./-}
  version_run_dir="${run_root}/${version_tag}"
  mkdir -p "${version_run_dir}"

  upstream_binary="${upstream_install_root}/${version}/bin/rsync"
  if [[ ! -x "${upstream_binary}" ]]; then
    printf 'Missing upstream rsync %s\n' "${version}" >&2
    continue
  fi

  oc_port=$((port_base + idx*2))
  up_port=$((oc_port + 1))

  oc_dest="${version_run_dir}/oc-destination"
  up_dest="${version_run_dir}/upstream-destination"
  mkdir -p "${oc_dest}" "${up_dest}"

  oc_pid_file="${version_run_dir}/oc.pid"
  up_pid_file="${version_run_dir}/upstream.pid"
  oc_conf="${version_run_dir}/oc.conf"
  up_conf="${version_run_dir}/upstream.conf"
  oc_log="${version_run_dir}/oc.log"
  up_log="${version_run_dir}/upstream.log"

  write_rust_daemon_conf "${oc_conf}" "${oc_pid_file}" "${oc_port}" "${oc_dest}" "oc interop target (${version})"
  write_upstream_conf "${up_conf}" "${up_pid_file}" "${up_port}" "${up_dest}" "upstream interop target (${version})" "${up_identity}"

  start_oc_daemon "${oc_client}" "${oc_conf}" "${oc_log}" "${oc_pid_file}" "${oc_port}" "${upstream_binary}"
  start_upstream_daemon "${upstream_binary}" "${up_conf}" "${up_log}"

  cat > "${version_run_dir}/env" <<ENV
VERSION=${version}
OC_PORT=${oc_port}
UPSTREAM_PORT=${up_port}
OC_DEST=${oc_dest}
UP_DEST=${up_dest}
OC_CONF=${oc_conf}
UP_CONF=${up_conf}
OC_LOG=${oc_log}
UP_LOG=${up_log}
UPSTREAM_BIN=${upstream_binary}
OC_BIN=${oc_client}
ENV

  idx=$((idx + 1))
  printf 'Started daemons for rsync %s (oc: %s, upstream: %s)\n' "${version}" "${oc_port}" "${up_port}"
done

printf 'All daemons started. Keep this running for the client.\n'
wait

