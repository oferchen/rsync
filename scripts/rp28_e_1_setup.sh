#!/usr/bin/env bash
# RP28.e.2 - setup harness for daemon-mode rsync 2.6.9 client interop
#
# Builds the deterministic fixture set described in
# `docs/design/rp28-e-1-daemon-2-6-9-client-harness.md` section 4 (D1-D10).
# Two trees are populated under `/tmp/rp28-e/`:
#
#   - `daemon-share/` - the share oc-rsync exposes via the `[test]` module;
#     pre-seeded with the per-fixture state the client expects on pulls.
#   - `client-src/`   - source trees the rsync 2.6.9 client pushes to the
#     daemon. Each fixture lives in its own sub-directory.
#
# The script also writes the daemon config to `/tmp/rp28-e/oc-rsyncd.conf`
# per spec section 3. All file contents use fixed bytes (no random data
# without a fixed seed) so re-runs produce identical hashes.
#
# Task: RP28.e.2 (#2964). Parent: RP28.e (#2730). Spec: RP28.e.1 (#2963).

set -euo pipefail

ROOT="${RP28_E_ROOT:-/tmp/rp28-e}"
DAEMON_SHARE="${ROOT}/daemon-share"
CLIENT_SRC="${ROOT}/client-src"
CLIENT_DEST="${ROOT}/client-dest"
DAEMON_CONF="${ROOT}/oc-rsyncd.conf"
DAEMON_PID_FILE="${ROOT}/oc-rsyncd.pid"
DAEMON_LOG="${ROOT}/daemon.log"

# Reset the tree so every invocation starts from a known state.
rm -rf "${ROOT}"
mkdir -p "${DAEMON_SHARE}" "${CLIENT_SRC}" "${CLIENT_DEST}"

# Deterministic 1 KiB block used wherever a non-trivial filler is needed.
det_block() {
  # 1 KiB of the printable byte sequence 0..255 repeated, prefixed with a
  # fixture-specific tag so different fixtures hash differently while still
  # being deterministic.
  local tag="$1"
  printf '%s\n' "${tag}"
  awk 'BEGIN { for (i = 0; i < 1024; i++) printf "%c", (i % 95) + 32 }'
}

# D1: empty directory (pull + push). The fixture's source directory exists
# but holds no files; verifies greeting + module list + empty flist.
mkdir -p "${DAEMON_SHARE}/d1"
mkdir -p "${CLIENT_SRC}/d1"

# D2: 100 small files (pull + push). Exercises proto-28 flist encoding.
mkdir -p "${DAEMON_SHARE}/d2" "${CLIENT_SRC}/d2"
for i in $(seq -w 1 100); do
  printf 'd2 daemon file %s\n' "${i}" > "${DAEMON_SHARE}/d2/file_${i}.txt"
  printf 'd2 client file %s\n' "${i}" > "${CLIENT_SRC}/d2/file_${i}.txt"
done

# D3: file with `-z` (pull). Compresses well so the proto-28 zlib codec
# path is exercised.
mkdir -p "${DAEMON_SHARE}/d3"
awk 'BEGIN { for (i = 0; i < 16384; i++) printf "compressible-d3-payload\n" }' \
  > "${DAEMON_SHARE}/d3/zlib_input.txt"

# D4: file with `--checksum` (pull). Two files: one identical to what we
# expect locally, one different, so the client's --checksum path picks up
# at least one transfer.
mkdir -p "${DAEMON_SHARE}/d4" "${CLIENT_DEST}/d4_seed"
printf 'd4 identical contents\n' > "${DAEMON_SHARE}/d4/same.txt"
printf 'd4 identical contents\n' > "${CLIENT_DEST}/d4_seed/same.txt"
printf 'd4 daemon-side updated contents\n' > "${DAEMON_SHARE}/d4/changed.txt"
printf 'd4 client-side older contents\n'   > "${CLIENT_DEST}/d4_seed/changed.txt"

# D5: file with `--delete` on dest (pull + push). The destination starts
# with an extra file that --delete must remove.
mkdir -p "${DAEMON_SHARE}/d5_pull" "${CLIENT_DEST}/d5_pull_seed"
printf 'd5 kept-pull\n' > "${DAEMON_SHARE}/d5_pull/keep.txt"
printf 'd5 kept-pull\n' > "${CLIENT_DEST}/d5_pull_seed/keep.txt"
printf 'd5 stale-pull (must be deleted)\n' > "${CLIENT_DEST}/d5_pull_seed/stale.txt"

mkdir -p "${CLIENT_SRC}/d5_push" "${DAEMON_SHARE}/d5_push_seed"
printf 'd5 kept-push\n' > "${CLIENT_SRC}/d5_push/keep.txt"
printf 'd5 kept-push\n' > "${DAEMON_SHARE}/d5_push_seed/keep.txt"
printf 'd5 stale-push (must be deleted)\n' > "${DAEMON_SHARE}/d5_push_seed/stale.txt"

# D6: directory tree (3 levels deep) (push). Drives the non-INC_RECURSE
# legacy flist walk against the daemon.
mkdir -p "${CLIENT_SRC}/d6/lvl1/lvl2/lvl3"
printf 'd6 top\n'  > "${CLIENT_SRC}/d6/top.txt"
printf 'd6 lvl1\n' > "${CLIENT_SRC}/d6/lvl1/a.txt"
printf 'd6 lvl2\n' > "${CLIENT_SRC}/d6/lvl1/lvl2/b.txt"
printf 'd6 lvl3\n' > "${CLIENT_SRC}/d6/lvl1/lvl2/lvl3/c.txt"

# D7: file with extended-character name (pull + push). The 2.6.9 build
# disables iconv, so we stick to filename bytes only - no ACL/xattr work.
mkdir -p "${DAEMON_SHARE}/d7" "${CLIENT_SRC}/d7"
printf 'd7 daemon\n' > "${DAEMON_SHARE}/d7/café-naïve.txt"
printf 'd7 client\n' > "${CLIENT_SRC}/d7/café-naïve.txt"

# D8: hardlink group of two files (pull + push). The verifier asserts the
# destination preserves the inode-identity relationship.
mkdir -p "${DAEMON_SHARE}/d8" "${CLIENT_SRC}/d8"
printf 'd8 daemon-shared contents\n' > "${DAEMON_SHARE}/d8/a.txt"
ln "${DAEMON_SHARE}/d8/a.txt" "${DAEMON_SHARE}/d8/b.txt"
printf 'd8 client-shared contents\n' > "${CLIENT_SRC}/d8/a.txt"
ln "${CLIENT_SRC}/d8/a.txt" "${CLIENT_SRC}/d8/b.txt"

# D9: incremental update (pull twice). First pull copies the file fresh;
# second pull must hit the quick-check skip path with no transfer.
mkdir -p "${DAEMON_SHARE}/d9"
det_block d9-baseline > "${DAEMON_SHARE}/d9/incremental.bin"

# D10: 1 MiB file modified between two pulls (delta path). Seeded with a
# deterministic block so the rolling+strong checksum match phase has
# something to do on the second pull.
mkdir -p "${DAEMON_SHARE}/d10"
awk 'BEGIN { for (i = 0; i < 1024; i++) printf "%c", (i % 95) + 32 }' \
  > "${DAEMON_SHARE}/d10/payload.bin.chunk"
: > "${DAEMON_SHARE}/d10/payload.bin"
for _ in $(seq 1 1024); do
  cat "${DAEMON_SHARE}/d10/payload.bin.chunk" >> "${DAEMON_SHARE}/d10/payload.bin"
done
rm -f "${DAEMON_SHARE}/d10/payload.bin.chunk"

# Daemon config. The `[test]` module is read+write because the matrix
# exercises both directions through the same module. `use chroot = no`
# avoids needing CAP_SYS_CHROOT in CI.
cat > "${DAEMON_CONF}" <<EOF
use chroot = no
address = 127.0.0.1
pid file = ${DAEMON_PID_FILE}
log file = ${DAEMON_LOG}
[test]
    path = ${DAEMON_SHARE}
    read only = false
    list = yes
EOF

echo "RP28.e.2 setup complete:"
echo "  daemon share: ${DAEMON_SHARE}"
echo "  client src:   ${CLIENT_SRC}"
echo "  client dest:  ${CLIENT_DEST}"
echo "  daemon conf:  ${DAEMON_CONF}"
