#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target_dir="${workspace_root}/target/dist"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_install_root="${workspace_root}/target/interop/upstream-install"

versions=(3.0.9 3.1.3 3.4.1)

main() {
  ensure_workspace_binaries

  for version in "${versions[@]}"; do
    ensure_upstream "${version}"
  done

  local index=0
  for version in "${versions[@]}"; do
    local oc_port=$((2873 + index * 4))
    local up_port=$((oc_port + 1))
    run_interop "${version}" "${oc_port}" "${up_port}"
    index=$((index + 1))
  done
}

ensure_workspace_binaries() {
  if [[ ! -x "${target_dir}/oc-rsync" || ! -x "${target_dir}/oc-rsyncd" ]]; then
    echo "Building oc-rsync workspace binaries with cargo (dist profile)"
    cargo --locked build --profile dist --bin oc-rsync --bin oc-rsyncd
  fi
}

ensure_upstream() {
  local version=$1
  local install_dir="${upstream_install_root}/${version}"
  local binary="${install_dir}/bin/rsync"

  if [[ -x "${binary}" ]]; then
    echo "Reusing cached upstream rsync ${version} at ${binary}"
    return
  fi

  mkdir -p "${upstream_src_root}" "${install_dir}"
  rm -rf "${install_dir}"/*

  local url="https://rsync.samba.org/ftp/rsync/src/rsync-${version}.tar.gz"
  echo "Fetching upstream rsync ${version} from ${url}"

  rm -rf "${upstream_src_root}/rsync-${version}"
  curl -L --fail --silent --show-error "${url}" | tar -xz -C "${upstream_src_root}"

  local source_dir="${upstream_src_root}/rsync-${version}"
  if [[ ! -d "${source_dir}" ]]; then
    echo "Failed to unpack upstream rsync ${version}" >&2
    exit 1
  fi

  pushd "${source_dir}" >/dev/null
  echo "Configuring upstream rsync ${version}"
  ./configure --prefix="${install_dir}" >/dev/null
  echo "Building upstream rsync ${version}"
  make -j"$(cpu_count)" >/dev/null
  echo "Installing upstream rsync ${version}"
  make install >/dev/null
  popd >/dev/null
}

cpu_count() {
  if command -v nproc >/dev/null 2>&1; then
    nproc
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.ncpu
  else
    echo 4
  fi
}

run_interop() {
  local version=$1
  local oc_port=$2
  local up_port=$3
  local install_dir="${upstream_install_root}/${version}"
  local upstream_client="${install_dir}/bin/rsync"

  if [[ ! -x "${upstream_client}" ]]; then
    echo "Upstream rsync ${version} missing at ${upstream_client}" >&2
    exit 1
  fi

  echo "Running interoperability checks against upstream rsync ${version}"

  (
    set -euo pipefail

    local workdir
    workdir=$(mktemp -d)
    local oc_pid_file="${workdir}/oc-rsyncd.pid"
    local up_pid_file="${workdir}/upstream-rsyncd.pid"
    local oc_delegate_pid=""
    local oc_pid=""
    local up_pid=""

    cleanup() {
      local status=$?

      if [[ -n "${oc_pid}" ]]; then
        kill "${oc_pid}" >/dev/null 2>&1 || true
        wait "${oc_pid}" >/dev/null 2>&1 || true
      fi

      if [[ -n "${oc_delegate_pid}" ]]; then
        kill "${oc_delegate_pid}" >/dev/null 2>&1 || true
        for _ in $(seq 1 50); do
          if ! kill -0 "${oc_delegate_pid}" >/dev/null 2>&1; then
            break
          fi
          sleep 0.1
        done
        rm -f "${oc_pid_file}"
      fi

      if [[ -n "${up_pid}" ]]; then
        kill "${up_pid}" >/dev/null 2>&1 || true
        wait "${up_pid}" >/dev/null 2>&1 || true
        rm -f "${up_pid_file}"
      fi

      rm -rf "${workdir}"

      exit "${status}"
    }

    trap cleanup EXIT

    local src="${workdir}/source"
    local oc_dest="${workdir}/oc-destination"
    local up_dest="${workdir}/upstream-destination"
    mkdir -p "${src}" "${oc_dest}" "${up_dest}"

    printf 'interop-test\n' >"${src}/payload.txt"

    local uid
    local gid
    uid=$(id -u)
    gid=$(id -g)

    local identity=""
    if [[ ${uid} -eq 0 ]]; then
      identity=$(printf 'uid = %s\ngid = %s\n' "${uid}" "${gid}")
    fi

    local oc_conf="${workdir}/oc-rsyncd.conf"
    cat >"${oc_conf}" <<OC_CONF
pid file = ${oc_pid_file}
port = ${oc_port}
use chroot = false
${identity}
numeric ids = yes
[interop]
    path = ${oc_dest}
    comment = oc interop target
    read only = false
OC_CONF

    local up_conf="${workdir}/upstream-rsyncd.conf"
    cat >"${up_conf}" <<UP_CONF
pid file = ${up_pid_file}
port = ${up_port}
use chroot = false
${identity}
numeric ids = yes
[interop]
    path = ${up_dest}
    comment = upstream interop target
    read only = false
UP_CONF

    OC_RSYNC_DAEMON_FALLBACK="${upstream_client}" \
      OC_RSYNC_FALLBACK="${upstream_client}" \
      "${target_dir}/oc-rsyncd" --config "${oc_conf}" --daemon --no-detach \
      --log-file "${workdir}/oc-rsyncd.log" &
    oc_pid=$!
    sleep 1

    if [[ -f "${oc_pid_file}" ]]; then
      oc_delegate_pid=$(<"${oc_pid_file}")
    fi

    "${upstream_client}" -av --timeout=10 "${src}/" rsync://127.0.0.1:${oc_port}/interop >/dev/null

    if [[ ! -f "${oc_dest}/payload.txt" ]]; then
      echo "Upstream rsync ${version} failed to transfer to oc-rsyncd" >&2
      exit 1
    fi

    kill "${oc_pid}"
    wait "${oc_pid}" || true
    oc_pid=""

    if [[ -n "${oc_delegate_pid}" ]]; then
      kill "${oc_delegate_pid}" >/dev/null 2>&1 || true
      for _ in $(seq 1 50); do
        if ! kill -0 "${oc_delegate_pid}" >/dev/null 2>&1; then
          break
        fi
        sleep 0.1
      done
      oc_delegate_pid=""
      rm -f "${oc_pid_file}"
    fi

    "${upstream_client}" --daemon --config "${up_conf}" --no-detach \
      --log-file "${workdir}/upstream-rsyncd.log" &
    up_pid=$!
    sleep 1

    OC_RSYNC_FALLBACK="${upstream_client}" \
      "${target_dir}/oc-rsync" -av --timeout=10 "${src}/" \
      rsync://127.0.0.1:${up_port}/interop >/dev/null

    if [[ ! -f "${up_dest}/payload.txt" ]]; then
      echo "oc-rsync failed to transfer to upstream rsync ${version}" >&2
      exit 1
    fi

    kill "${up_pid}"
    wait "${up_pid}" || true
    up_pid=""
  )
}

main "$@"
