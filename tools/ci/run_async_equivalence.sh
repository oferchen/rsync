#!/usr/bin/env bash
# run_async_equivalence.sh - async (tokio-transfer) vs default equivalence gate.
#
# Runs the SAME real oc-rsync transfers twice - once with the default binary
# and once built with `--features tokio-transfer` - and asserts byte-identical
# results across three observables:
#
#   1. destination tree content (find | sort | sha256sum of every file),
#   2. `--stats` output (normalized to drop the timing-only bytes/sec rate),
#   3. process exit code.
#
# The feature-on daemon module receiver is routed through the tokio driver, so
# the daemon PUSH / PULL legs exercise the async foundation on a real transfer.
# An rsync:// pull leg is included as a second wire path. Any divergence on any
# leg is a hard failure (exit 1), so the atomic async receiver fork is CI-
# verifiable: the moment the async path diverges from the threaded path in dest
# bytes, stats, or exit code, this gate goes red.
#
# The gate builds each feature state once, uses a deterministic corpus and a
# pinned `--checksum-seed`, and cleans every temp dir it creates.
#
# Environment:
#   OC_RSYNC_OFF_BIN  path to a pre-built default binary (skips its build)
#   OC_RSYNC_ON_BIN   path to a pre-built --features tokio-transfer binary
#   CARGO             cargo binary to use (default: cargo)

set -euo pipefail

workspace_root=$(cd "$(dirname "$0")/../.." && pwd)
cargo_bin="${CARGO:-cargo}"

log() { printf '[async-equivalence] %s\n' "$*"; }
fail() { printf '[async-equivalence] FAIL: %s\n' "$*" >&2; exit 1; }

# Pick one SHA-256 tool up front so every hash in a run uses the same one.
if command -v sha256sum >/dev/null 2>&1; then
    sha256_of() { sha256sum "$1" | awk '{print $1}'; }
    sha256_stdin() { sha256sum | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
    sha256_stdin() { shasum -a 256 | awk '{print $1}'; }
else
    fail "no sha256sum or shasum on PATH"
fi

# ---------------------------------------------------------------------------
# Build (or accept pre-built) both feature states.
# ---------------------------------------------------------------------------
build_dir=$(mktemp -d "${TMPDIR:-/tmp}/async-equiv.XXXXXX")
cleanup() {
    if [ -n "${daemon_off_pid:-}" ]; then kill "${daemon_off_pid}" 2>/dev/null || true; fi
    if [ -n "${daemon_on_pid:-}" ]; then kill "${daemon_on_pid}" 2>/dev/null || true; fi
    rm -rf "${build_dir}"
}
trap cleanup EXIT INT TERM

if [ -n "${OC_RSYNC_OFF_BIN:-}" ]; then
    off_bin="${OC_RSYNC_OFF_BIN}"
    log "using pre-built default binary: ${off_bin}"
else
    log "building default (feature-off) binary"
    ( cd "${workspace_root}" && "${cargo_bin}" build --release --bin oc-rsync )
    off_bin="${workspace_root}/target/release/oc-rsync"
    cp "${off_bin}" "${build_dir}/oc-rsync-off"
    off_bin="${build_dir}/oc-rsync-off"
fi

if [ -n "${OC_RSYNC_ON_BIN:-}" ]; then
    on_bin="${OC_RSYNC_ON_BIN}"
    log "using pre-built tokio-transfer binary: ${on_bin}"
else
    log "building tokio-transfer (feature-on) binary"
    ( cd "${workspace_root}" && "${cargo_bin}" build --release --bin oc-rsync --features tokio-transfer )
    on_bin="${workspace_root}/target/release/oc-rsync"
    cp "${on_bin}" "${build_dir}/oc-rsync-on"
    on_bin="${build_dir}/oc-rsync-on"
fi

[ -x "${off_bin}" ] || fail "default binary not executable: ${off_bin}"
[ -x "${on_bin}" ] || fail "tokio-transfer binary not executable: ${on_bin}"

# ---------------------------------------------------------------------------
# Deterministic mixed corpus: whole-file + delta-candidate + compressible +
# multi-file + nested + empty. Seeded so both feature states see identical data.
# ---------------------------------------------------------------------------
corpus="${build_dir}/src"
mkdir -p "${corpus}/nested/a/b"
# Deterministic pseudo-random bytes without relying on /dev/urandom entropy:
# a fixed byte pattern is sufficient - the point is identical input to both runs.
seed_bytes() { # size path
    head -c "$1" /dev/zero | LC_ALL=C tr '\0' "$2" > "$3"
}
seed_bytes 200000 'Q' "${corpus}/whole.bin"
seed_bytes 500000 'A' "${corpus}/compressible.txt"
seed_bytes 30000  'Z' "${corpus}/nested/a/file1.dat"
seed_bytes 100000 'D' "${corpus}/nested/delta.bin"
printf 'hello world\n' > "${corpus}/nested/a/b/small.txt"
: > "${corpus}/empty.file"
for i in 1 2 3 4 5; do printf 'line %d\n' "$i" > "${corpus}/multi_${i}.txt"; done

# ---------------------------------------------------------------------------
# Daemon config (no chroot, current user, ephemeral module dir).
# ---------------------------------------------------------------------------
port_off=48873
port_on=48874
mod_off="${build_dir}/mod_off"
mod_on="${build_dir}/mod_on"
mkdir -p "${mod_off}" "${mod_on}"

write_conf() { # path module_dir port
    cat > "$1" <<EOF
use chroot = no
port = $3
[mod]
    path = $2
    read only = false
EOF
}
write_conf "${build_dir}/rsyncd_off.conf" "${mod_off}" "${port_off}"
write_conf "${build_dir}/rsyncd_on.conf" "${mod_on}" "${port_on}"

wait_for_daemon() { # bin port
    local bin="$1" port="$2" tries=50
    while [ "${tries}" -gt 0 ]; do
        if "${bin}" "rsync://localhost:${port}/" >/dev/null 2>&1; then return 0; fi
        tries=$((tries - 1))
        sleep 0.2
    done
    return 1
}

log "starting default daemon on ${port_off}"
"${off_bin}" --daemon --no-detach --config="${build_dir}/rsyncd_off.conf" --port="${port_off}" \
    >"${build_dir}/daemon_off.log" 2>&1 &
daemon_off_pid=$!
log "starting tokio-transfer daemon on ${port_on}"
"${on_bin}" --daemon --no-detach --config="${build_dir}/rsyncd_on.conf" --port="${port_on}" \
    >"${build_dir}/daemon_on.log" 2>&1 &
daemon_on_pid=$!

wait_for_daemon "${off_bin}" "${port_off}" || fail "default daemon did not come up"
wait_for_daemon "${on_bin}" "${port_on}" || fail "tokio-transfer daemon did not come up"

# ---------------------------------------------------------------------------
# Observables.
# ---------------------------------------------------------------------------
tree_hash() { # dir -> single content hash of every file (path-sorted)
    (
        cd "$1" || exit 1
        # Emit "<relpath> <content-hash>" per file, path-sorted, then fold the
        # whole listing into one hash. Path + content both feed the final digest,
        # so a renamed, added, dropped, or byte-changed file all diverge.
        find . -type f | LC_ALL=C sort | while IFS= read -r f; do
            printf '%s %s\n' "$f" "$(sha256_of "$f")"
        done
    ) | sha256_stdin
}

# Drop only the timing-dependent rate line ("... bytes/sec") from --stats; every
# other byte must match exactly.
normalize_stats() {
    grep -v 'bytes/sec' || true
}

# Run one leg with both binaries and diff all three observables.
#   $1 label  $2 direction (push|pull)  $3.. rsync args
run_leg() {
    local label="$1" direction="$2"; shift 2
    local -a args=("$@")
    local off_stats on_stats off_exit on_exit off_tree on_tree
    local off_out on_out off_dst on_dst

    case "${direction}" in
        push)
            # PUSH into each daemon's module (feature-on module receiver = tokio driver).
            off_out=$("${off_bin}" "${args[@]}" --checksum-seed=1234 --stats \
                "${corpus}/" "rsync://localhost:${port_off}/mod/${label}/" 2>&1) && off_exit=0 || off_exit=$?
            on_out=$("${on_bin}" "${args[@]}" --checksum-seed=1234 --stats \
                "${corpus}/" "rsync://localhost:${port_on}/mod/${label}/" 2>&1) && on_exit=0 || on_exit=$?
            off_stats=$(printf '%s\n' "${off_out}" | normalize_stats)
            on_stats=$(printf '%s\n' "${on_out}" | normalize_stats)
            off_tree=$(tree_hash "${mod_off}/${label}")
            on_tree=$(tree_hash "${mod_on}/${label}")
            ;;
        pull)
            off_dst="${build_dir}/pull_off_${label}"
            on_dst="${build_dir}/pull_on_${label}"
            rm -rf "${off_dst}" "${on_dst}"; mkdir -p "${off_dst}" "${on_dst}"
            off_out=$("${off_bin}" "${args[@]}" --checksum-seed=1234 --stats \
                "rsync://localhost:${port_off}/mod/pullsrc/" "${off_dst}/" 2>&1) && off_exit=0 || off_exit=$?
            on_out=$("${on_bin}" "${args[@]}" --checksum-seed=1234 --stats \
                "rsync://localhost:${port_on}/mod/pullsrc/" "${on_dst}/" 2>&1) && on_exit=0 || on_exit=$?
            off_stats=$(printf '%s\n' "${off_out}" | normalize_stats)
            on_stats=$(printf '%s\n' "${on_out}" | normalize_stats)
            off_tree=$(tree_hash "${off_dst}")
            on_tree=$(tree_hash "${on_dst}")
            ;;
        *) fail "unknown direction: ${direction}" ;;
    esac

    if [ "${off_exit}" != "${on_exit}" ]; then
        fail "${label}: exit code diverged (off=${off_exit} on=${on_exit})"
    fi
    if [ "${off_tree}" != "${on_tree}" ]; then
        fail "${label}: dest tree hash diverged (off=${off_tree} on=${on_tree})"
    fi
    if [ "${off_stats}" != "${on_stats}" ]; then
        printf '=== off stats ===\n%s\n=== on stats ===\n%s\n' "${off_stats}" "${on_stats}" >&2
        fail "${label}: --stats output diverged (see diff above)"
    fi
    log "OK ${label} (${direction}): exit=${off_exit}, dest tree + --stats byte-identical (tree ${off_tree:0:12})"
}

# Seed the pull source into both modules so PULL has identical source content.
"${off_bin}" -a "${corpus}/" "rsync://localhost:${port_off}/mod/pullsrc/" >/dev/null 2>&1 \
    || fail "seeding default daemon pull source failed"
"${on_bin}" -a "${corpus}/" "rsync://localhost:${port_on}/mod/pullsrc/" >/dev/null 2>&1 \
    || fail "seeding tokio-transfer daemon pull source failed"

# Legs: exercise delta + whole-file + compressed across push and pull.
run_leg push_archive_z  push "-az"    # archive + compression (delta-candidate path)
run_leg push_whole_z    push "-azW"   # whole-file + compression
run_leg push_archive    push "-a"     # archive, no compression
run_leg pull_archive_z  pull "-az"    # daemon pull, compressed
run_leg pull_archive    pull "-a"     # daemon pull, uncompressed

log "PASS: default and tokio-transfer transfers are byte-identical on all legs"
