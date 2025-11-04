#!/usr/bin/env bash
# Ubuntu/Debian-first rsync interop harness
# - Detects platform architecture and aligns Debian/Ubuntu package arch names
# - Tries real, validated package locations for:
#     3.0.9  -> old-releases.ubuntu.com
#     3.1.3  -> archive.ubuntu.com
#     3.4.1  -> deb.debian.org (3.4.1+ds1-6)
# - Falls back to source build if the exact .deb for this arch is missing
# - Starts oc-rsyncd on a non-privileged port by passing --port on the CLI
set -euo pipefail

if ! command -v git >/dev/null 2>&1; then
  echo "git is required to build upstream rsync releases for interop tests" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to fetch Ubuntu/Debian rsync packages" >&2
  exit 1
fi

export GIT_TERMINAL_PROMPT=0

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target_dir="${workspace_root}/target/dist"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_install_root="${workspace_root}/target/interop/upstream-install"

# Versions we test against
versions=(3.0.9 3.1.3 3.4.1)
rsync_repo_url="https://github.com/RsyncProject/rsync.git"

# Mirrors (can be overridden in CI)
DEBIAN_MIRROR="${DEBIAN_MIRROR:-https://deb.debian.org/debian}"
UBUNTU_MIRROR="${UBUNTU_MIRROR:-http://archive.ubuntu.com/ubuntu}"
OLD_UBUNTU_MIRROR="${OLD_UBUNTU_MIRROR:-https://old-releases.ubuntu.com/ubuntu}"

oc_pid=""
up_pid=""
oc_pid_file_current=""
up_pid_file_current=""
workdir=""

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
  if ! curl -fsSL "$url" -o "$tmp_deb"; then
    rm -f "$tmp_deb"
    return 1
  fi

  if ! command -v ar >/dev/null 2>&1; then
    echo "ar not available; cannot extract .deb from ${url}, will fall back to source" >&2
    rm -f "$tmp_deb"
    return 1
  fi

  mkdir -p "${install_dir}"
  (
    cd "${install_dir}"
    ar x "$tmp_deb" >/dev/null 2>&1 || true
    if [[ -f data.tar.xz ]]; then
      tar -xf data.tar.xz
      rm -f data.tar.xz
    elif [[ -f data.tar.gz ]]; then
      tar -xzf data.tar.gz
      rm -f data.tar.gz
    fi
  )
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
    if curl -fsSL "$url" -o "$tmp_deb" 2>/dev/null; then
      if ! command -v ar >/dev/null 2>&1; then
        rm -f "$tmp_deb"
        return 1
      fi
      mkdir -p "${install_dir}"
      (
        cd "${install_dir}"
        ar x "$tmp_deb" >/dev/null 2>&1 || true
        if [[ -f data.tar.xz ]]; then
          tar -xf data.tar.xz
          rm -f data.tar.xz
        elif [[ -f data.tar.gz ]]; then
          tar -xzf data.tar.gz
          rm -f data.tar.gz
        fi
      )
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
  local tag_candidates=("v${version}" "${version}")

  for tag in "${tag_candidates[@]}"; do
    if git clone --depth 1 --branch "$tag" "$rsync_repo_url" "$destination" >/dev/null 2>&1; then
      return 0
    fi
  done
  return 1
}

build_upstream_from_source() {
  local version=$1
  local src_dir="${upstream_src_root}/rsync-${version}"
  local install_dir="${upstream_install_root}/${version}"

  rm -rf "$src_dir"
  mkdir -p "$upstream_src_root" "$upstream_install_root"

  echo "Cloning upstream rsync ${version} from ${rsync_repo_url}"
  if ! clone_upstream_source "$version" "$src_dir"; then
    echo "Failed to clone upstream rsync ${version} from ${rsync_repo_url}" >&2
    exit 1
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

  ./configure "${configure_args[@]}" >/dev/null
  make -j"$(build_jobs)" >/dev/null
  make install >/dev/null

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

# IMPORTANT: oc-rsyncd needs the port on CLI, otherwise it binds to 873 (privileged)
start_oc_daemon() {
  local config=$1
  local log_file=$2
  local fallback_client=$3
  local pid_file=$4
  local port=$5

  oc_pid_file_current="$pid_file"

  RUST_BACKTRACE=1 \
  OC_RSYNC_DAEMON_FALLBACK="$fallback_client" \
  OC_RSYNC_FALLBACK="$fallback_client" \
    "$oc_daemon" --config "$config" --port "$port" --log-file "$log_file" &
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
  start_oc_daemon "$oc_conf" "$oc_log" "$upstream_binary" "$oc_pid_file" "$oc_port"

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

# ------------------ main ------------------

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
