#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target_dir="${workspace_root}/target/dist"

if [[ ! -x "${target_dir}/oc-rsync" || ! -x "${target_dir}/oc-rsyncd" ]]; then
  cargo --locked build --profile dist --bin oc-rsync --bin oc-rsyncd
fi

upstream_dir="${workspace_root}/target/interop/upstream-src"
upstream_install="${workspace_root}/target/interop/upstream-install"
mkdir -p "${upstream_dir}" "${upstream_install}"

if [[ ! -x "${upstream_install}/bin/rsync" ]]; then
  rm -rf "${upstream_dir}"/*
  upstream_url="https://rsync.samba.org/ftp/rsync/src/rsync-3.4.1.tar.gz"
  curl -L --fail --silent --show-error --retry 5 --retry-delay 2 "$upstream_url" | tar -xz -C "${upstream_dir}"
  src_dir=$(find "${upstream_dir}" -maxdepth 1 -mindepth 1 -type d | head -n1)
  pushd "$src_dir" >/dev/null
  if [[ ! -x configure ]]; then
    echo "Upstream rsync source tree is missing a configure script" >&2
    exit 1
  fi
  ./configure --prefix="${upstream_install}" --disable-md2man --disable-xxhash --disable-lz4 >/dev/null
  make -j"$(nproc)" >/dev/null
  make install >/dev/null
  popd >/dev/null
fi

oc_client="${target_dir}/oc-rsync"
oc_daemon="${target_dir}/oc-rsyncd"
upstream_client="${upstream_install}/bin/rsync"

workdir=$(mktemp -d)
oc_pid_file="${workdir}/oc-rsyncd.pid"
up_pid_file="${workdir}/upstream-rsyncd.pid"
rm -f "$oc_pid_file" "$up_pid_file"
oc_delegate_pid=""

cleanup() {
  local exit_code=$1

  if [[ -n "${oc_pid:-}" ]]; then
    kill "${oc_pid}" >/dev/null 2>&1 || true
    wait "${oc_pid}" >/dev/null 2>&1 || true
  fi

  if [[ -n "${oc_delegate_pid:-}" && ${oc_delegate_pid} =~ ^[0-9]+$ ]]; then
    kill "${oc_delegate_pid}" >/dev/null 2>&1 || true
    for attempt in $(seq 1 50); do
      if ! kill -0 "${oc_delegate_pid}" >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
    rm -f "$oc_pid_file"
  fi

  if [[ -n "${up_pid:-}" ]]; then
    kill "${up_pid}" >/dev/null 2>&1 || true
    wait "${up_pid}" >/dev/null 2>&1 || true
    rm -f "$up_pid_file"
  fi

  rm -rf "$workdir"

  return "$exit_code"
}

trap 'status=$?; cleanup "$status"; exit "$status"' EXIT

src="${workdir}/source"
oc_dest="${workdir}/oc-destination"
up_dest="${workdir}/upstream-destination"
mkdir -p "$src" "$oc_dest" "$up_dest"

printf 'interop-test\n' >"${src}/payload.txt"

uid=$(id -u)
gid=$(id -g)

oc_identity=""
up_identity=""
# Upstream rsync attempts to adjust process credentials when `uid`/`gid` are
# present in the configuration file. Non-root users lack permission to call
# setgroups(2), so omit those directives unless the harness is executing with
# effective UID 0.
if [[ ${uid} -eq 0 ]]; then
  printf -v oc_identity 'uid = %s\ngid = %s\n' "${uid}" "${gid}"
  printf -v up_identity 'uid = %s\ngid = %s\n' "${uid}" "${gid}"
fi

oc_conf="${workdir}/oc-rsyncd.conf"
cat >"$oc_conf" <<OC_CONF
pid file = ${workdir}/oc-rsyncd.pid
port = 2873
use chroot = false
${oc_identity}
numeric ids = yes
[interop]
    path = ${oc_dest}
    comment = oc interop target
    read only = false
OC_CONF

up_conf="${workdir}/upstream-rsyncd.conf"
cat >"$up_conf" <<UP_CONF
pid file = ${workdir}/upstream-rsyncd.pid
port = 2874
use chroot = false
${up_identity}
numeric ids = yes
[interop]
    path = ${up_dest}
    comment = upstream interop target
    read only = false
UP_CONF

OC_RSYNC_DAEMON_FALLBACK="${upstream_client}" \
  OC_RSYNC_FALLBACK="${upstream_client}" \
  "$oc_daemon" --config "$oc_conf" --daemon --no-detach --log-file "${workdir}/oc-rsyncd.log" &
oc_pid=$!
sleep 1
if [[ -f "$oc_pid_file" ]]; then
  oc_delegate_pid=$(<"$oc_pid_file")
fi

"$upstream_client" -av --timeout=10 "${src}/" rsync://127.0.0.1:2873/interop >/dev/null

if [[ ! -f "${oc_dest}/payload.txt" ]]; then
  echo "Upstream client failed to transfer to oc-rsyncd" >&2
  exit 1
fi

kill "$oc_pid"
wait "$oc_pid" || true

if [[ -n "$oc_delegate_pid" && $oc_delegate_pid =~ ^[0-9]+$ ]]; then
  kill "$oc_delegate_pid" >/dev/null 2>&1 || true
  for attempt in $(seq 1 50); do
    if ! kill -0 "$oc_delegate_pid" >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done
  rm -f "$oc_pid_file"
fi

"$upstream_client" --daemon --config "$up_conf" --no-detach --log-file "${workdir}/upstream-rsyncd.log" &
up_pid=$!
sleep 1

"$oc_client" -av --timeout=10 "${src}/" rsync://127.0.0.1:2874/interop >/dev/null

if [[ ! -f "${up_dest}/payload.txt" ]]; then
  echo "oc-rsync client failed to transfer to upstream daemon" >&2
  exit 1
fi

kill "$up_pid"
wait "$up_pid" || true
