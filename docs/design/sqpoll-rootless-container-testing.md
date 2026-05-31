# SQPOLL rootless container testing design (SQP-2)

Tracking issue: SQP-2.

Companion artefacts:

- SQP-1 (audit) - capability requirements documented in
  `docs/audits/io_uring_sqpoll_mmap_pagefault.md`.
- SQP-3 - error message for capability failure (merged).
- SQP-4 - rootless-container detection at io_uring init (merged).
- SQP-5 - integration test for graceful fallback (merged).
- SQP-6 - deployment guide (merged).
- `tools/ci/test_iouring_unprivileged.sh` - existing end-to-end script
  exercising the fallback in unprivileged containers.
- `.github/workflows/iouring-kernel-compat.yml` - kernel version matrix CI.
- `crates/fast_io/src/io_uring/config.rs` - `build_ring()` SQPOLL fallback
  logic and `SQPOLL_FALLBACK` atomic.

## 1. Problem statement

io_uring SQPOLL mode (`IORING_SETUP_SQPOLL`) starts a kernel-side thread
that polls the submission queue without userspace entering the kernel on
each I/O. This requires `CAP_SYS_NICE` on kernels prior to 5.11 because
the kernel threads scheduler class and nice value need elevation. On
5.11+ the restriction was lifted for non-RT class threads.

Rootless Podman containers run without `CAP_SYS_NICE` by default. The
default seccomp profile may additionally block `io_uring_setup(2)` on some
runtimes. This means SQPOLL requests inside rootless containers will fail
with `EPERM`, and oc-rsync must gracefully fall back to a regular io_uring
ring (or standard buffered I/O if io_uring itself is blocked).

SQP-4 landed the detection logic; SQP-5 landed the integration test
confirming graceful fallback. SQP-2 defines the full test matrix - kernel
versions, container configurations, expected outcomes - and the CI
integration approach for ongoing regression coverage.

## 2. Test matrix

### 2.1. Kernel version tiers

| Tier | Kernel | SQPOLL without CAP_SYS_NICE | io_uring available | Notes |
|------|--------|-------------------------------|-------------------|-------|
| A | < 5.6 | N/A (no io_uring) | No | Entire fast_io subsystem disabled |
| B | 5.6 - 5.10 | Requires CAP_SYS_NICE | Yes | EPERM on SQPOLL without capability |
| C | 5.11 - 5.18 | Allowed unconditionally | Yes | Kernel commit `2a18b7e7e from 5.11` lifted restriction |
| D | 5.19+ | Allowed unconditionally | Depends on seccomp | Some distros block io_uring via seccomp in containers |
| E | 6.0+ | Allowed unconditionally | Yes (full tier) | PBUF_RING, SEND_ZC, all metadata ops |

### 2.2. Container runtime configurations

| Config | CAP_SYS_NICE | io_uring_setup(2) | SQPOLL expected | Notes |
|--------|--------------|-------------------|-----------------|-------|
| Rootless podman (default seccomp) | No | Depends on runtime version | Fallback to regular ring | Most common deployment |
| Rootless podman + `--security-opt seccomp=unconfined` | No | Yes | Fallback on tier B; succeeds on C+ | Relaxed seccomp, still no capability |
| Rootless podman + nested user namespace | No | Blocked | Fallback to buffered I/O | `unshare --user` inside container |
| Rootful podman (privileged) | Yes | Yes | Succeeds on all tiers | Baseline comparison |
| Rootless podman + `--cap-add CAP_SYS_NICE` | Yes | Yes | Succeeds on all tiers | Explicit capability grant |

### 2.3. Expected outcomes per cell

| Tier x Config | io_uring probe | SQPOLL attempt | End state | Diagnostic emitted |
|---------------|----------------|----------------|-----------|-------------------|
| B + rootless default | Available or blocked | EPERM | Regular ring or buffered | `sqpoll_fell_back() == true` or `io_uring: disabled` |
| B + rootless + seccomp=unconfined | Available | EPERM | Regular ring | `sqpoll_fell_back() == true` |
| C + rootless default | Available or blocked | Succeeds (if ring available) | SQPOLL ring or buffered | None or `io_uring: disabled` |
| C + rootless + seccomp=unconfined | Available | Succeeds | SQPOLL ring | None |
| D/E + rootless default | Available (usually) | Succeeds | SQPOLL ring | None |
| D/E + nested userns | Blocked | N/A | Buffered I/O | `io_uring: disabled` |
| Any + rootful privileged | Available | Succeeds | SQPOLL ring | None |

The invariant across all cells: the transfer completes successfully and
produces byte-identical output regardless of which I/O path is selected.

## 3. Test scenarios

### 3.1. Scenario S1: rootless podman - default seccomp

Validates the most common production deployment. The container runs with
no extra capabilities or security overrides.

```sh
podman run --rm --network=none \
  -v "${STAGE}:/stage:ro" \
  docker.io/library/alpine:3.21 \
  /stage/run.sh default
```

Expected: transfer succeeds. SQPOLL may or may not be available depending
on host kernel and runtime version. `sqpoll_fell_back()` reports the
outcome for diagnostic verification.

### 3.2. Scenario S2: rootless podman - nested user namespace

Creates a user namespace inside the already-unprivileged container,
reliably blocking `io_uring_setup(2)` on all kernel versions. This is the
strongest possible test of the full fallback chain.

```sh
podman run --rm --network=none \
  -v "${STAGE}:/stage:ro" \
  docker.io/library/alpine:3.21 \
  sh -c 'unshare --user --map-root-user --net /stage/run.sh userns-blocked'
```

Expected: `io_uring: disabled` in debug output. Transfer completes via
standard buffered I/O.

### 3.3. Scenario S3: rootless podman - explicit CAP_SYS_NICE

Proves that granting the capability makes SQPOLL succeed on kernels where
it would otherwise fail (tier B). Serves as the positive control.

```sh
podman run --rm --network=none --cap-add=SYS_NICE \
  -v "${STAGE}:/stage:ro" \
  docker.io/library/alpine:3.21 \
  /stage/run.sh cap-nice-granted
```

Expected: on tier B kernels, SQPOLL ring is created. On tier C+ this is
equivalent to the default case.

### 3.4. Scenario S4: rootless podman - io_uring explicitly disabled via env

Tests the `OC_RSYNC_DISABLE_IOURING=1` environment variable override
regardless of kernel or capability state.

```sh
podman run --rm --network=none \
  -e OC_RSYNC_DISABLE_IOURING=1 \
  -v "${STAGE}:/stage:ro" \
  docker.io/library/alpine:3.21 \
  /stage/run.sh env-disabled
```

Expected: `io_uring: disabled` in probe output. Transfer uses buffered
I/O. No SQPOLL attempt.

### 3.5. Scenario S5: seccomp profile blocking io_uring_setup

Simulates container runtimes that explicitly block `io_uring_setup` via a
custom seccomp profile. This covers the case where the syscall itself is
filtered, distinct from capability denial.

```sh
podman run --rm --network=none \
  --security-opt seccomp=/stage/deny-iouring.json \
  -v "${STAGE}:/stage:ro" \
  docker.io/library/alpine:3.21 \
  /stage/run.sh seccomp-blocked
```

Where `deny-iouring.json` is a seccomp profile that blocks syscall 425
(`io_uring_setup`), 426 (`io_uring_enter`), and 427 (`io_uring_register`).

Expected: `io_uring: disabled (syscall blocked)`. Transfer succeeds via
buffered I/O.

## 4. Verification criteria

Each scenario must satisfy all of the following:

1. **Transfer correctness**: every file in the source tree appears in the
   destination with identical content (verified via `cmp -s`).
2. **Exit code**: oc-rsync exits 0.
3. **Diagnostic accuracy**: `--debug=io1` output correctly reflects the
   actual I/O path taken (SQPOLL ring, regular ring, or buffered).
4. **No panic or abort**: no SIGABRT, SIGSEGV, or Rust panic in stderr.
5. **Idempotency**: running the same transfer twice produces no changes on
   the second run (quick-check confirms files are up to date).

## 5. CI integration approach

### 5.1. Existing coverage

The existing `tools/ci/test_iouring_unprivileged.sh` script already
covers scenarios S1 and S2. The `iouring-kernel-compat.yml` workflow tests
io_uring availability on ubuntu-22.04 (~5.15) and ubuntu-24.04 (~6.8).

### 5.2. Proposed additions

#### 5.2.1. Dedicated workflow: `iouring-rootless-container.yml`

A new non-required workflow that exercises the full matrix when changes
touch `crates/fast_io/` or the test script itself.

```yaml
name: io_uring rootless container
on:
  push:
    branches: [master]
    paths:
      - 'crates/fast_io/**'
      - 'tools/ci/test_iouring_unprivileged.sh'
  pull_request:
    paths:
      - 'crates/fast_io/**'
      - 'tools/ci/test_iouring_unprivileged.sh'
  workflow_dispatch:
```

Matrix strategy:

| Runner | Kernel tier | Podman version | Scenarios |
|--------|-------------|---------------|-----------|
| ubuntu-22.04 | ~5.15 (tier C) | 3.4+ | S1, S2, S3, S4, S5 |
| ubuntu-24.04 | ~6.8 (tier E) | 4.9+ | S1, S2, S3, S4, S5 |

Both runners ship Podman pre-installed. The workflow builds oc-rsync for
x86_64-unknown-linux-gnu (statically linked via musl for container
portability), then runs each scenario.

#### 5.2.2. Container image selection

Use `docker.io/library/alpine:3.21` as the test image. Alpine is minimal
(~7 MB), ships `unshare(1)`, and starts fast. The oc-rsync binary is
statically linked so no glibc dependency exists.

#### 5.2.3. Seccomp profile for S5

A minimal JSON seccomp profile that denylists io_uring syscalls:

```json
{
  "defaultAction": "SCMP_ACT_ALLOW",
  "syscalls": [
    {
      "names": ["io_uring_setup", "io_uring_enter", "io_uring_register"],
      "action": "SCMP_ACT_ERRNO",
      "args": [],
      "errnoRet": 1
    }
  ]
}
```

Stored at `tools/ci/seccomp-deny-iouring.json` and mounted into the
container at `/stage/deny-iouring.json`.

#### 5.2.4. Step summary

The workflow emits a GitHub Actions step summary table showing each
scenario's result, the detected I/O path, and whether SQPOLL fell back.
This makes failures visible without digging through logs.

### 5.3. Why non-required

This workflow is informational. It validates graceful degradation, not
correctness of the primary transfer path. Failures here mean diagnostics
or capability detection regressed - they do not indicate data corruption.
Promoting to required would add flake surface from container runtime
variance across GitHub-hosted runners.

### 5.4. Local execution

Developers can run the test locally without CI:

```sh
# Build a static binary for container use
cargo build --target x86_64-unknown-linux-musl --release

# Run existing script (covers S1, S2)
OC_RSYNC_BIN=./target/x86_64-unknown-linux-musl/release/oc-rsync \
  bash tools/ci/test_iouring_unprivileged.sh

# Run individual scenario manually
podman run --rm --network=none --cap-add=SYS_NICE \
  -v /tmp/stage:/stage:ro \
  alpine:3.21 /stage/run.sh cap-nice-granted
```

## 6. Interaction with existing rsync-profile container

The long-running `rsync-profile` container (`podman run --name rsync-profile
-v /Users/ofer/devel/rsync:/workspace rust:latest sleep infinity`) is a
rootful Debian container with bind-mounted workspace. It is unsuitable for
SQPOLL rootless testing because:

1. It runs as root with full capabilities.
2. Its bind mount gives write access to the host workspace.
3. It uses a Debian-based image (heavier than needed for probe tests).

The SQPOLL rootless tests use ephemeral (`--rm`) Alpine containers with
read-only mounts specifically to avoid the pitfalls documented in
CLAUDE.md about `rm -rf` in bind-mounted containers. No destructive
commands run inside any test container.

## 7. Kernel version detection in test harness

The test harness must detect the host kernel version to set expectations:

```sh
KERNEL_RELEASE=$(uname -r)
MAJOR=$(echo "$KERNEL_RELEASE" | cut -d. -f1)
MINOR=$(echo "$KERNEL_RELEASE" | cut -d. -f2)

if [ "$MAJOR" -lt 5 ] || { [ "$MAJOR" -eq 5 ] && [ "$MINOR" -lt 11 ]; }; then
    # Tier B: SQPOLL requires CAP_SYS_NICE
    EXPECT_SQPOLL_DEFAULT=false
else
    # Tier C+: SQPOLL allowed without CAP_SYS_NICE
    EXPECT_SQPOLL_DEFAULT=true
fi
```

On tier B hosts, scenario S1 expects `sqpoll_fell_back() == true`; on
tier C+ it expects SQPOLL to succeed (unless the runtime blocks it via
seccomp).

## 8. Failure modes and diagnostics

| Failure mode | Detection | Remediation |
|---|---|---|
| EPERM on SQPOLL (expected on tier B) | `sqpoll_fell_back()` returns true | No action - this is correct behavior |
| ENOSYS on io_uring_setup (seccomp block) | Probe reports `io_uring: disabled` | No action - buffered fallback is correct |
| ENOMEM on ring creation | Build_ring returns error, falls back | Log warning, continue with buffered I/O |
| Transfer produces wrong bytes | `cmp -s` fails in verification | Bug - investigate delta engine, not I/O path |
| Panic in fallback path | Non-zero exit + backtrace in stderr | Bug - missing error handling in fallback chain |
| Silent data loss (SQPOLL + mmap race) | Not applicable in rootless (SQPOLL disabled) | Covered by SQM-1 mlock tests instead |

## 9. Relationship to other SQP tasks

```
SQP-1 (audit)                    - DONE: identified capability requirements
SQP-2 (this doc)                 - test design and CI integration plan
SQP-3 (error message)            - DONE: user-facing diagnostic on EPERM
SQP-4 (rootless detection)       - DONE: detect at init, set SQPOLL_FALLBACK
SQP-5 (integration test)         - DONE: graceful fallback verified in test
SQP-6 (deployment guide)         - DONE: documents --cap-add=SYS_NICE for operators
```

## 10. Implementation steps

1. Add `tools/ci/seccomp-deny-iouring.json` with the syscall denylist.
2. Extend `tools/ci/test_iouring_unprivileged.sh` with scenarios S3, S4,
   and S5 (S1 and S2 already exist).
3. Add `.github/workflows/iouring-rootless-container.yml` wiring the
   matrix across ubuntu-22.04 and ubuntu-24.04.
4. Add kernel-version-aware expectations to the test driver so it
   distinguishes tier B from tier C+ outcomes.
5. Verify locally on a Linux host with Podman installed.

## 11. Open questions

- **Self-hosted runner for tier B (5.6-5.10)?** GitHub Actions does not
  offer a runner with a kernel in this range. Container images use the
  host kernel. A self-hosted runner or QEMU-based VM would be needed
  for strict tier B coverage. For now, the nested-userns scenario (S2)
  reliably exercises the EPERM path regardless of host kernel version,
  which provides equivalent coverage of the fallback logic.

- **Podman version variance?** Podman 3.x vs 4.x have different default
  seccomp profiles. The test accepts either outcome (io_uring available
  or blocked) and only asserts transfer correctness. This makes the test
  resilient to runtime version changes.
