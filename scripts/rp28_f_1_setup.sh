#!/usr/bin/env bash
# RP28.f.2 - setup harness for client-mode rsync 2.6.9 daemon interop
#
# Builds the deterministic fixture set described in
# `docs/design/rp28-f-1-client-2-6-9-daemon-harness.md` section 4 (F1-F12).
# The topology is the inverse of RP28.e: rsync 2.6.9 runs as the daemon and
# oc-rsync drives the conversation as the client. Three trees are populated
# under `/tmp/rp28-f/`:
#
#   - `daemon-share/` - the share rsync 2.6.9 exposes via the `[legacy]`
#     module; pre-seeded with the per-fixture state the oc-rsync client
#     expects on pulls.
#   - `client-src/`   - source trees the oc-rsync client pushes to the
#     legacy daemon. Each fixture lives in its own sub-directory.
#   - `client-dest/`  - destination for pulls; reset per-fixture by the
#     runner before each transfer.
#
# The script also writes the daemon config to
# `/tmp/rp28-f/rsyncd-2-6-9.conf` per spec section 3. All file contents
# use fixed bytes (no random data without a fixed seed) so re-runs produce
# identical hashes.
#
# Task: RP28.f.2 (#2967). Parent: RP28.f (#2731). Spec: RP28.f.1 (#2966).

set -euo pipefail

ROOT="${RP28_F_ROOT:-/tmp/rp28-f}"
DAEMON_SHARE="${ROOT}/daemon-share"
CLIENT_SRC="${ROOT}/client-src"
CLIENT_DEST="${ROOT}/client-dest"
DAEMON_CONF="${ROOT}/rsyncd-2-6-9.conf"
DAEMON_PID_FILE="/tmp/rsyncd-2-6-9.pid"
DAEMON_LOG="${ROOT}/daemon.log"

# Reset the tree so every invocation starts from a known state.
rm -rf "${ROOT}"
rm -f "${DAEMON_PID_FILE}"
mkdir -p "${DAEMON_SHARE}" "${CLIENT_SRC}" "${CLIENT_DEST}"

# Deterministic 1 KiB block used wherever a non-trivial filler is needed.
det_block() {
  local tag="$1"
  printf '%s\n' "${tag}"
  awk 'BEGIN { for (i = 0; i < 1024; i++) printf "%c", (i % 95) + 32 }'
}

# F1: empty directory (pull + push). Smoke test: daemon greeting, module
# list, empty flist negotiation.
mkdir -p "${DAEMON_SHARE}/f1"
mkdir -p "${CLIENT_SRC}/f1"

# F2: 100 small files (pull + push). Exercises proto-28 flist
# encoding/decoding on both directions.
mkdir -p "${DAEMON_SHARE}/f2" "${CLIENT_SRC}/f2"
for i in $(seq -w 1 100); do
  printf 'f2 daemon file %s\n' "${i}" > "${DAEMON_SHARE}/f2/file_${i}.txt"
  printf 'f2 client file %s\n' "${i}" > "${CLIENT_SRC}/f2/file_${i}.txt"
done

# F3: file with `-z` (pull). Compresses well so the proto-28 zlib DECODE
# path is exercised without cursor-advance assumption.
mkdir -p "${DAEMON_SHARE}/f3"
awk 'BEGIN { for (i = 0; i < 16384; i++) printf "compressible-f3-payload\n" }' \
  > "${DAEMON_SHARE}/f3/zlib_input.txt"

# F4: file with `--checksum` (pull). Two files: one identical to what we
# expect locally, one different, so the client's --checksum path picks up
# at least one transfer and exercises MD5 fallback (no CSUM negotiation).
mkdir -p "${DAEMON_SHARE}/f4" "${CLIENT_DEST}/f4_seed"
printf 'f4 identical contents\n' > "${DAEMON_SHARE}/f4/same.txt"
printf 'f4 identical contents\n' > "${CLIENT_DEST}/f4_seed/same.txt"
printf 'f4 daemon-side updated contents\n' > "${DAEMON_SHARE}/f4/changed.txt"
printf 'f4 client-side older contents\n'   > "${CLIENT_DEST}/f4_seed/changed.txt"

# F5: file with `--delete` on local (pull + push). The destination starts
# with an extra file that --delete must remove. Exercises delete-stats
# absence handling at proto < 31 (no NDX_DEL_STATS in goodbye phase).
mkdir -p "${DAEMON_SHARE}/f5_pull" "${CLIENT_DEST}/f5_pull_seed"
printf 'f5 kept-pull\n' > "${DAEMON_SHARE}/f5_pull/keep.txt"
printf 'f5 kept-pull\n' > "${CLIENT_DEST}/f5_pull_seed/keep.txt"
printf 'f5 stale-pull (must be deleted)\n' > "${CLIENT_DEST}/f5_pull_seed/stale.txt"

mkdir -p "${CLIENT_SRC}/f5_push" "${DAEMON_SHARE}/f5_push_seed"
printf 'f5 kept-push\n' > "${CLIENT_SRC}/f5_push/keep.txt"
printf 'f5 kept-push\n' > "${DAEMON_SHARE}/f5_push_seed/keep.txt"
printf 'f5 stale-push (must be deleted)\n' > "${DAEMON_SHARE}/f5_push_seed/stale.txt"

# F6: directory tree (3 levels) (push). Drives the non-INC_RECURSE sender
# path against the legacy receiver - 2.6.9 has no INC_RECURSE.
mkdir -p "${CLIENT_SRC}/f6/lvl1/lvl2/lvl3"
printf 'f6 top\n'  > "${CLIENT_SRC}/f6/top.txt"
printf 'f6 lvl1\n' > "${CLIENT_SRC}/f6/lvl1/a.txt"
printf 'f6 lvl2\n' > "${CLIENT_SRC}/f6/lvl1/lvl2/b.txt"
printf 'f6 lvl3\n' > "${CLIENT_SRC}/f6/lvl1/lvl2/lvl3/c.txt"

# F7: file with extended-character name (pull + push). The 2.6.9 build
# disables iconv, so we stick to filename bytes only - no UTF-8 hint frame
# expected at proto 28.
mkdir -p "${DAEMON_SHARE}/f7" "${CLIENT_SRC}/f7"
printf 'f7 daemon\n' > "${DAEMON_SHARE}/f7/café-naïve.txt"
printf 'f7 client\n' > "${CLIENT_SRC}/f7/café-naïve.txt"

# F8: hardlink group of two files (pull + push). The verifier asserts the
# destination preserves the inode-identity relationship.
mkdir -p "${DAEMON_SHARE}/f8" "${CLIENT_SRC}/f8"
printf 'f8 daemon-shared contents\n' > "${DAEMON_SHARE}/f8/a.txt"
ln "${DAEMON_SHARE}/f8/a.txt" "${DAEMON_SHARE}/f8/b.txt"
printf 'f8 client-shared contents\n' > "${CLIENT_SRC}/f8/a.txt"
ln "${CLIENT_SRC}/f8/a.txt" "${CLIENT_SRC}/f8/b.txt"

# F9: incremental update (pull twice). First pull copies the file fresh;
# second pull must hit the quick-check skip path with no transfer.
mkdir -p "${DAEMON_SHARE}/f9"
det_block f9-baseline > "${DAEMON_SHARE}/f9/incremental.bin"

# F10: 1 MiB file modified between two pulls (delta path). Seeded with a
# deterministic block so the rolling+strong checksum match phase has
# something to do on the second pull.
mkdir -p "${DAEMON_SHARE}/f10"
awk 'BEGIN { for (i = 0; i < 1024; i++) printf "%c", (i % 95) + 32 }' \
  > "${DAEMON_SHARE}/f10/payload.bin.chunk"
: > "${DAEMON_SHARE}/f10/payload.bin"
for _ in $(seq 1 1024); do
  cat "${DAEMON_SHARE}/f10/payload.bin.chunk" >> "${DAEMON_SHARE}/f10/payload.bin"
done
rm -f "${DAEMON_SHARE}/f10/payload.bin.chunk"

# F11: capability-string back-negotiation (push). Single small file is
# enough; the assertion lives in the runner (client must succeed against a
# proto-28 daemon despite advertising its modern capability string).
mkdir -p "${CLIENT_SRC}/f11"
printf 'f11 capability back-negotiation marker\n' > "${CLIENT_SRC}/f11/marker.txt"

# F12: `--exclude` / `--filter` (pull + push). Two files per side; only
# the non-excluded file is expected at the destination.
mkdir -p "${DAEMON_SHARE}/f12" "${CLIENT_SRC}/f12"
printf 'f12 daemon keep\n'    > "${DAEMON_SHARE}/f12/keep.txt"
printf 'f12 daemon dropped\n' > "${DAEMON_SHARE}/f12/skip.log"
printf 'f12 client keep\n'    > "${CLIENT_SRC}/f12/keep.txt"
printf 'f12 client dropped\n' > "${CLIENT_SRC}/f12/skip.log"

# Daemon config. The `[legacy]` module is read+write because the matrix
# exercises both directions through the same module. `use chroot = no`
# avoids needing CAP_SYS_CHROOT in CI. Pid file lives at
# `/tmp/rsyncd-2-6-9.pid` per spec section 3 so the runner can locate it
# from the EXIT trap even if ROOT is overridden.
cat > "${DAEMON_CONF}" <<EOF
use chroot = no
address = 127.0.0.1
pid file = ${DAEMON_PID_FILE}
log file = ${DAEMON_LOG}
[legacy]
    path = ${DAEMON_SHARE}
    read only = false
    list = yes
EOF

echo "RP28.f.2 setup complete:"
echo "  daemon share: ${DAEMON_SHARE}"
echo "  client src:   ${CLIENT_SRC}"
echo "  client dest:  ${CLIENT_DEST}"
echo "  daemon conf:  ${DAEMON_CONF}"
