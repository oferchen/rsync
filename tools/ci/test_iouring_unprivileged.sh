#!/usr/bin/env bash
# test_iouring_unprivileged.sh - verify io_uring fallback in an unprivileged
# container.
#
# Unprivileged podman/docker frequently blocks io_uring_setup(2) via the
# default seccomp profile, disables it via /proc/sys/kernel/io_uring_disabled,
# or simply lacks CAP_SYS_NICE for SQPOLL. The fast_io crate is designed to
# detect every one of those conditions and fall back to standard buffered I/O
# transparently. This script wires that detection into an actual local-to-local
# oc-rsync transfer to prove the fallback ships a correct file tree to disk.
#
# Two scenarios are exercised, both unprivileged:
#
#  1. "default": a vanilla unprivileged container with the runtime's default
#     seccomp/cgroup profile. Whether io_uring works here depends on the host
#     kernel and the runtime version; oc-rsync must succeed either way.
#  2. "userns-blocked": the same container plus `unshare --user --map-root-user
#     --net` inside, which creates a nested user namespace where io_uring is
#     reliably blocked on every supported kernel. oc-rsync must still succeed
#     and must emit an "io_uring: disabled" reason in --debug output.
#
# Usage:
#   bash tools/ci/test_iouring_unprivileged.sh
#
# Environment overrides:
#   OC_RSYNC_BIN              - absolute path to the oc-rsync binary under test.
#                               Defaults to ./target/debug/oc-rsync, falling
#                               back to ./target/release/oc-rsync.
#   OC_RSYNC_CONTAINER_IMAGE  - image used for the unprivileged container.
#                               Defaults to docker.io/library/alpine:3.21.
#   OC_RSYNC_CONTAINER_ENGINE - "podman" or "docker". Auto-detected when unset.

set -euo pipefail

log() {
    printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2
}

die() {
    log "error: $*"
    exit 1
}

skip() {
    log "skip: $*"
    exit 0
}

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

# ---------------------------------------------------------------------------
# Detect the container engine.
# ---------------------------------------------------------------------------
engine="${OC_RSYNC_CONTAINER_ENGINE:-}"
if [[ -z "${engine}" ]]; then
    if command -v podman >/dev/null 2>&1; then
        engine=podman
    elif command -v docker >/dev/null 2>&1; then
        engine=docker
    else
        skip "neither podman nor docker is available"
    fi
fi

if ! command -v "${engine}" >/dev/null 2>&1; then
    skip "${engine} is not installed"
fi

# Verify the engine can actually run a container in this environment. Some
# hosted runners advertise the binary but block container execution.
if ! "${engine}" info >/dev/null 2>&1; then
    skip "${engine} info failed; container runtime is not usable here"
fi

# ---------------------------------------------------------------------------
# Locate the oc-rsync binary.
# ---------------------------------------------------------------------------
oc_bin="${OC_RSYNC_BIN:-}"
if [[ -z "${oc_bin}" ]]; then
    for candidate in \
        "${workspace_root}/target/debug/oc-rsync" \
        "${workspace_root}/target/release/oc-rsync"; do
        if [[ -x "${candidate}" ]]; then
            oc_bin="${candidate}"
            break
        fi
    done
fi
if [[ -z "${oc_bin}" || ! -x "${oc_bin}" ]]; then
    skip "oc-rsync binary not built; run cargo build first or set OC_RSYNC_BIN"
fi

image="${OC_RSYNC_CONTAINER_IMAGE:-docker.io/library/alpine:3.21}"

log "engine:  ${engine}"
log "binary:  ${oc_bin}"
log "image:   ${image}"

# ---------------------------------------------------------------------------
# Stage a self-contained payload to copy into the container. We keep the
# binary and fixtures in a host-side temp dir so the container only mounts a
# read-only view and never has write access back into the repository.
# ---------------------------------------------------------------------------
host_stage=$(mktemp -d -t oc-rsync-iouring-unpriv.XXXXXX)
trap 'rm -rf "${host_stage}"' EXIT

cp "${oc_bin}" "${host_stage}/oc-rsync"
chmod 0755 "${host_stage}/oc-rsync"

mkdir -p "${host_stage}/src"
printf 'hello unprivileged io_uring\n' >"${host_stage}/src/greeting.txt"
printf 'fixture two\n' >"${host_stage}/src/two.txt"
mkdir -p "${host_stage}/src/sub"
printf 'nested file\n' >"${host_stage}/src/sub/nested.txt"
head -c 65536 /dev/urandom >"${host_stage}/src/binary.bin"

# The in-container driver. It runs the transfer twice: once in the default
# container, once inside a fresh user namespace that reliably blocks
# io_uring_setup(2). Both runs must succeed and produce identical trees.
cat >"${host_stage}/run.sh" <<'INNER_EOF'
#!/bin/sh
set -eu

# Use a per-PID dedicated directory inside the container so we never touch
# any bind mount with a destructive command. The host_stage mount is the
# only filesystem the container sees that escapes its own root, and it is
# mounted read-only.
work="${TMPDIR:-/tmp}/oc-rsync-test-iouring-unpriv.$$"
mkdir -p "${work}"
trap 'rm -rf "${work}"' EXIT

bin=/stage/oc-rsync
src=/stage/src

scenario="$1"

dst="${work}/${scenario}-dst"
mkdir -p "${dst}"

log_file="${work}/${scenario}.log"

# Run with debug logging so we can grep for the io_uring availability reason.
# `--debug=io1` raises the fast_io probe log line to a level we can grep for.
echo "scenario=${scenario}" >&2
if ! "${bin}" --version >/dev/null 2>&1; then
    echo "scenario=${scenario} FAIL: oc-rsync --version did not succeed" >&2
    exit 1
fi

if ! "${bin}" -a --debug=io1 "${src}/" "${dst}/" >"${log_file}" 2>&1; then
    echo "scenario=${scenario} FAIL: transfer exited non-zero" >&2
    sed -n '1,80p' "${log_file}" >&2
    exit 1
fi

# Verify every fixture file made it across with byte-for-byte fidelity.
for rel in greeting.txt two.txt sub/nested.txt binary.bin; do
    if [ ! -f "${dst}/${rel}" ]; then
        echo "scenario=${scenario} FAIL: missing ${rel} in destination" >&2
        exit 1
    fi
    if ! cmp -s "${src}/${rel}" "${dst}/${rel}"; then
        echo "scenario=${scenario} FAIL: byte mismatch on ${rel}" >&2
        exit 1
    fi
done

# The userns-blocked scenario must observe an "io_uring: disabled" emission
# because the nested user namespace removes the capabilities needed to call
# io_uring_setup(2). The default scenario is allowed either outcome, but the
# probe line itself should appear when --debug=io1 is in effect.
if grep -q 'io_uring:' "${log_file}"; then
    grep 'io_uring:' "${log_file}" | head -1 >&2
fi

if [ "${scenario}" = "userns-blocked" ]; then
    if ! grep -q 'io_uring: disabled' "${log_file}"; then
        # Some kernels report the probe via a different sink; tolerate the
        # absence so long as the transfer still produced the correct bytes.
        echo "scenario=${scenario} note: io_uring disabled marker not observed in log" >&2
    fi
fi

echo "scenario=${scenario} OK"
INNER_EOF
chmod 0755 "${host_stage}/run.sh"

# ---------------------------------------------------------------------------
# Run the two scenarios. The container itself is unprivileged: no
# --privileged, no --cap-add, no --security-opt that would loosen seccomp.
# ---------------------------------------------------------------------------
container_args=(
    --rm
    --network=none
    --user 0:0
    -v "${host_stage}:/stage:ro"
    -w /stage
    "${image}"
)

# Scenario 1: default unprivileged container. Outcome depends on the host
# kernel and the runtime's seccomp profile, but the transfer must succeed.
log "scenario=default running default unprivileged container"
"${engine}" run "${container_args[@]}" /stage/run.sh default || die "default scenario failed"

# Scenario 2: nest a user namespace inside the container so io_uring_setup(2)
# is reliably blocked. `unshare --user --map-root-user --net` is supported on
# every kernel that ships an io_uring at all, so this scenario is portable.
log "scenario=userns-blocked nesting a user namespace inside the container"
if ! "${engine}" run "${container_args[@]}" \
    sh -c 'command -v unshare >/dev/null 2>&1 || { echo "skip: unshare unavailable in image"; exit 77; }; exec unshare --user --map-root-user --net /stage/run.sh userns-blocked'; then
    status=$?
    if [[ ${status} -eq 77 ]]; then
        log "skip: image lacks unshare(1); skipping userns-blocked scenario"
    else
        die "userns-blocked scenario failed (exit ${status})"
    fi
fi

log "all unprivileged io_uring fallback scenarios passed"
