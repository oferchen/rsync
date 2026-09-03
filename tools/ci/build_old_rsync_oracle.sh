#!/usr/bin/env bash
# build_old_rsync_oracle.sh - build a historical upstream rsync release and
# install it into an upstream tree's old_versions/ directory, where the 3.5.0
# testsuite looks for a legacy-behaviour oracle.
#
# WHY THIS EXISTS
#
# rsync 3.5.0 ships tests that pin a behaviour against a REAL old daemon when
# one is on disk and fall back to a hand-written static prediction when it is
# not. testsuite/daemon-symlink-escape-matrix_test.py is the current consumer:
#
#     elif (_repo / 'old_versions' / 'rsync_3.2.7').is_file():
#         ORACLE_BIN = str(_repo / 'old_versions' / 'rsync_3.2.7')
#     ...
#     want, src = static_followed(...), 'contract'
#
# upstream: rsync-3.5.0/old_versions/README.md documents the directory and
# ships build_static.sh to populate it, but the RELEASE TARBALL carries no
# binaries - MEASURED on a pristine 3.5.0 extraction, old_versions/ holds
# exactly README.md and build_static.sh. So on a stock CI runner the oracle is
# always absent, every oracle-backed cell quietly asserts the static prediction
# instead, and the test still reports PASS: runtests.py only shows a test's
# stdout when it does NOT pass (3.5.0 runtests.py:621), so the one line that
# names the degradation is never printed on the green path.
#
# This script closes that gap by BUILDING the oracle, so the contract the test
# says it enforces is the contract it actually enforces.
#
# Not upstream's build_static.sh: that one checks a git TAG out of a local
# rsync worktree (RSYNC_REPO, default ../rsync.4), links statically, and exists
# to produce an architecture-portable archive binary. CI has no such worktree
# and needs no portability - the binary is built and consumed on one machine -
# so this builds the release TARBALL dynamically. The one flag that is not
# optional is _FORTIFY_SOURCE=0; see below.
#
# Usage:
#   tools/ci/build_old_rsync_oracle.sh <version> <old_versions_dir> [workdir]
#
# Idempotent: a target binary that already reports <version> is left alone.

set -euo pipefail

VERSION="${1:?usage: build_old_rsync_oracle.sh <version> <old_versions_dir> [workdir]}"
DEST_DIR="${2:?usage: build_old_rsync_oracle.sh <version> <old_versions_dir> [workdir]}"
WORKDIR="${3:-${DEST_DIR}/.build}"

# Upstream's own naming convention for the archive - the testsuite stats this
# exact path. upstream: rsync-3.5.0/old_versions/README.md ("named
# `rsync_<version>`").
TARGET_BIN="${DEST_DIR}/rsync_${VERSION}"
TARBALL_BASE_URL="${RSYNC_TARBALL_BASE_URL:-https://rsync.samba.org/ftp/rsync/src}"

# `--version` is the acceptance test, not `-x`: a binary that cannot run on
# this host (wrong arch, missing shared library) is still present and
# executable, and would be handed to the testsuite as a working oracle. The
# consuming test runs the same probe for the same reason.
oracle_is_usable() {
    [[ -x "$TARGET_BIN" ]] || return 1
    "$TARGET_BIN" --version 2>/dev/null | head -1 | grep -q "version ${VERSION}"
}

if oracle_is_usable; then
    echo "==> rsync ${VERSION} oracle already present: ${TARGET_BIN}" >&2
    "$TARGET_BIN" --version | head -1 >&2
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

mkdir -p "$DEST_DIR" "$WORKDIR"
tarball="${WORKDIR}/rsync-${VERSION}.tar.gz"
srcdir="${WORKDIR}/rsync-${VERSION}"

if [[ ! -f "$tarball" ]]; then
    echo "==> Fetching rsync ${VERSION} source..." >&2
    curl -fsSL --connect-timeout 30 --max-time 300 \
        "${TARBALL_BASE_URL}/rsync-${VERSION}.tar.gz" -o "${tarball}.part"
    mv "${tarball}.part" "$tarball"
fi

rm -rf "$srcdir"
tar xzf "$tarball" -C "$WORKDIR"

# _FORTIFY_SOURCE=0 is LOAD-BEARING, not tidiness. Modern distributions default
# it to =3, whose object-size checks turn latent (historically benign)
# over-reads in old rsync into a hard "*** buffer overflow detected ***" abort
# when the binary runs as a server/daemon - which is exactly how this oracle is
# used. upstream: rsync-3.5.0/old_versions/README.md note 6 names 3.1.3 and
# 3.2.7 specifically as "unusable as peers" without it. A fortified oracle
# would not fail loudly here; it would abort mid-session, and the consuming
# test would read the aborted transfer as "did not follow" - a WRONG oracle
# answer rather than a missing one.
#
# -Wno-error and the implicit-declaration downgrades keep a release from an
# older toolchain era compiling under a current one; upstream's build_static.sh
# carries the same set for the same reason.
oracle_cflags="-O2 -g -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0 -Wno-error"
oracle_cflags+=" -Wno-implicit-function-declaration -Wno-int-conversion"
oracle_cflags+=" -Wno-incompatible-pointer-types"

echo "==> Configuring rsync ${VERSION} oracle..." >&2
(
    cd "$srcdir"
    # The --disable list is DETERMINISM, not taste. 3.2.7's configure ABORTS
    # ("Aborting configure run") when it finds a feature's header missing
    # instead of quietly proceeding, so which of openssl/xxhash/zstd/lz4 happen
    # to be installed on the runner image decides whether the oracle builds at
    # all - and, if it does, which digests and compressors it advertises. Naming
    # them makes the oracle identical on every host.
    #
    # Safe for what the oracle is ASKED: it is consulted about symlink and path
    # resolution in the daemon (send_directory / change_dir / basis_link_stat),
    # which no compression or checksum backend touches. rsync implements MD4/MD5
    # natively, so --disable-openssl only drops an alternative implementation of
    # digests it still has - upstream's own old_versions/build_static.sh drops
    # openssl for the same reason. --disable-md2man drops manpage generation
    # (it wants python3-commonmark).
    CFLAGS="$oracle_cflags" ./configure --disable-md2man \
        --disable-openssl --disable-xxhash --disable-zstd --disable-lz4 \
        >configure.log 2>&1 || { tail -40 configure.log >&2; exit 1; }
    echo "==> Building rsync ${VERSION} oracle..." >&2
    make -j"$(build_jobs)" >make.log 2>&1 || {
        grep -E 'error:|\*\*\*' make.log | head -20 >&2
        exit 1
    }
)

install -m 0755 "${srcdir}/rsync" "$TARGET_BIN"

if ! oracle_is_usable; then
    echo "ERROR: built oracle at ${TARGET_BIN} does not report version ${VERSION}." >&2
    "$TARGET_BIN" --version 2>&1 | head -1 >&2 || true
    rm -f "$TARGET_BIN"
    exit 1
fi

echo "==> Installed rsync ${VERSION} oracle: ${TARGET_BIN}" >&2
"$TARGET_BIN" --version | head -1 >&2
