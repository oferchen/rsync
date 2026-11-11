#!/usr/bin/env bash
# build_openssh.sh - build the pinned OpenSSH `ssh` that the ssh_config
# differential tests use as their oracle.
#
# The tarball is downloaded to a file, checked against tools/ci/openssh.sha256
# and only then extracted, into target/interop/upstream-src/openssh-<ver>/, the
# tree this repository's OpenSSH citations name. Only `ssh` is built and nothing
# is installed: the binary is copied to target/interop/openssh/bin/ssh. A second
# mirror is tried only when a download fails; a digest mismatch is fatal.
#
# The configure flags match the build the differential cells were measured
# against. OpenSSL and zlib stay enabled because `ssh -G` prints the default
# cipher, kex and host-key lists, and a build without OpenSSL drops the RSA and
# ECDSA entries from them.
#
# Usage: build_openssh.sh
#   Prints the path of the built `ssh` on stdout. A cached binary is reused.
#
# Exit status: 0 ok, 1 download or build failed, 2 pin missing, 3 digest mismatch.

set -euo pipefail

repo_root="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="${repo_root}/tools/ci/openssh.sha256"
out_dir="${repo_root}/target/interop/openssh"
src_root="${repo_root}/target/interop/upstream-src"
mirrors=(https://cdn.openbsd.org/pub/OpenBSD/OpenSSH/portable https://ftp.openbsd.org/pub/OpenBSD/OpenSSH/portable)

die() {
    local code=$1
    shift
    printf 'build_openssh: ERROR: %s\n' "$*" >&2
    exit "$code"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

pin="$(grep -v '^#' "$manifest" | grep -v '^[[:space:]]*$' || true)"
[[ -n "$pin" ]] || die 2 "no pin in ${manifest}"
read -r want tarball <<<"$pin"
release="${tarball%.tar.gz}"
mkdir -p "$out_dir/bin" "$src_root"

if [[ ! -f "$out_dir/$tarball" ]] || [[ "$(sha256_of "$out_dir/$tarball")" != "$want" ]]; then
    rm -f "$out_dir/$tarball.part" "$out_dir/bin/ssh"
    fetched=
    for base in "${mirrors[@]}"; do
        if curl -sSfL -o "$out_dir/$tarball.part" "$base/$tarball"; then
            fetched=1
            break
        fi
        printf 'build_openssh: download from %s failed\n' "$base" >&2
    done
    [[ -n "$fetched" ]] || die 1 "could not download ${tarball}"
    got="$(sha256_of "$out_dir/$tarball.part")"
    [[ "$got" == "$want" ]] || die 3 "${tarball}: sha256 ${got}, pinned ${want}"
    mv "$out_dir/$tarball.part" "$out_dir/$tarball"
fi

rm -rf "${src_root:?}/${release}"
tar -xzf "$out_dir/$tarball" -C "$src_root"

if [[ ! -x "$out_dir/bin/ssh" ]]; then
    build_dir="$(mktemp -d)"
    trap 'rm -rf "$build_dir"' EXIT
    tar -xzf "$out_dir/$tarball" -C "$build_dir"
    if ! (
        cd "$build_dir/$release"
        ./configure --without-openssl-header-check --with-privsep-path="$build_dir/empty"
        make -j"$(getconf _NPROCESSORS_ONLN)" ssh
    ) >"$build_dir/build.log" 2>&1; then
        tail -n 50 "$build_dir/build.log" >&2
        die 1 "building ssh from ${tarball} failed"
    fi
    cp "$build_dir/$release/ssh" "$out_dir/bin/ssh"
fi

printf '%s\n' "$out_dir/bin/ssh"
