#!/usr/bin/env bash
# Build upstream rsync 2.6.9 from source as a coexisting peer binary.
#
# rsync 2.6.9 is the oldest protocol-28 baseline we still test against.
# Its source predates many modern autoconf/gcc defaults, so this script
# coerces it onto current toolchains without disturbing newer rsync
# builds. The resulting binary is installed as `rsync-2.6.9` so it can
# live alongside upstream 3.x peers in CI containers.
#
# Task: RP28.b.1 (#2960). Parent: RP28.b (#2727). Grandparent: RP28 (#2725).
#
# Usage:
#   PREFIX=/usr/local WORKDIR=/tmp/rsync-2.6.9-build ./scripts/build_rsync_2_6_9.sh
#
# Idempotent: re-running with the binary already installed is a no-op.

set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
WORKDIR="${WORKDIR:-/tmp/rsync-2.6.9-build}"

VERSION="2.6.9"
TARBALL_URL="https://download.samba.org/pub/rsync/src/rsync-${VERSION}.tar.gz"
TARGET_BIN="${PREFIX}/bin/rsync-${VERSION}"

if [[ -x "${TARGET_BIN}" ]] && "${TARGET_BIN}" --version 2>/dev/null | grep -q "version ${VERSION}"; then
  echo "rsync ${VERSION} already installed at ${TARGET_BIN}"
  exit 0
fi

build_jobs() {
  if command -v nproc >/dev/null 2>&1; then
    nproc
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.ncpu
  else
    echo 2
  fi
}

mkdir -p "${WORKDIR}"
cd "${WORKDIR}"

if [[ ! -d "rsync-${VERSION}" ]]; then
  curl -fsSL "${TARBALL_URL}" | tar xz
fi

cd "rsync-${VERSION}"

# Refresh ancient autotools helper scripts when the system provides newer
# ones. rsync 2.6.9 ships a 2006-vintage config.guess/config.sub that does
# not recognise aarch64, riscv64, or modern Linux triples; Debian's
# autotools package keeps current copies in /usr/share/misc/.
for helper in config.guess config.sub; do
  if [[ -f "${helper}" && -f "/usr/share/misc/${helper}" ]]; then
    cp "/usr/share/misc/${helper}" "${helper}"
  fi
done

# gcc >= 14 makes implicit function declarations and implicit int errors by
# default, which breaks autoconf feature-probes in rsync 2.6.9 (every check
# silently reports "no", producing a build that calls gettimeofday with the
# wrong arity etc.). Restore the historical "warn, don't error" defaults.
CFLAGS="${CFLAGS:-} -O2 -Wno-error -Wno-implicit-function-declaration -Wno-implicit-int -Wno-int-conversion" \
  ./configure \
    --prefix="${WORKDIR}/install" \
    --disable-iconv \
    --disable-acl-support \
    --disable-xattr-support \
    --without-included-zlib \
    --with-included-popt

make -j"$(build_jobs)"

# Install to a staging prefix, then copy only the rsync binary as
# rsync-2.6.9 so it coexists with any system rsync 3.x.
make install

install -d "${PREFIX}/bin"
install -m 0755 "${WORKDIR}/install/bin/rsync" "${TARGET_BIN}"

if ! "${TARGET_BIN}" --version | grep -q "version ${VERSION}"; then
  echo "rsync ${VERSION} install verification failed: ${TARGET_BIN} --version did not report ${VERSION}" >&2
  exit 1
fi

echo "rsync ${VERSION} installed at ${TARGET_BIN}"
