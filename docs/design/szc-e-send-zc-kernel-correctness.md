# SZC.e - SEND_ZC per-kernel correctness validation spec

Date: 2026-05-26
Scope: correctness test matrix for `IORING_OP_SEND_ZC` across kernel
versions 5.16, 5.19, 6.0, 6.1, and 6.6.
Status: design spec; implementation is a follow-up PR.
Predecessors:
- IKV-1 (PR #4899): io_uring opcode inventory by minimum-kernel floor.
- IKV-3: runtime probe matrix spec - unified `ProbeMatrix` cache for
  per-opcode availability. The SEND_ZC probe at
  `crates/fast_io/src/io_uring/send_zc.rs::is_supported()` is a
  consolidation target.
- SZC.a: production-scale bench workload spec - defines the daemon pull
  scenarios, fixture shapes, and measurement methodology that the
  correctness tests here reuse in reduced form.
Successors:
- SZC.f: implementation of the kernel-correctness CI workflow.
- IKV-7/8/9: CI cells exercising specific LTS kernels against the
  runtime probe matrix (broader scope; SZC.e focuses on SEND_ZC only).

## 1. Motivation

`IORING_OP_SEND_ZC` (opcode 44) was introduced in Linux 6.0 but had
correctness fixes through subsequent stable releases. The opcode's
dual-CQE completion model - one transfer CQE with `IORING_CQE_F_MORE`,
one notification CQE with `IORING_CQE_F_NOTIF` - carries failure modes
that vary by kernel version:

- **Buffer pinning lifetime bugs.** Early 6.0.x releases had races where
  the notification CQE could fire before the NIC's DMA completed,
  allowing user-space to mutate pages still in flight. Fixed in 6.0.7+.
- **Notification ordering.** On kernels 6.0.0-6.0.2, the notification
  CQE could arrive before the transfer CQE under socket backpressure,
  violating the ordering assumption in `try_send_zc`'s drain loop (lines
  167-189 of `send_zc.rs`). Fixed in 6.0.3+.
- **Short-send on loopback.** Loopback TCP with SO_SNDBUF pressure could
  produce a short transfer CQE where `result < buf.len()` without
  setting `IORING_CQE_F_MORE`, causing the drain loop to terminate
  without a notification CQE. The `classify_cqe` function handles this
  case (lines 221-224) but early 6.0 kernels did not set the flags
  consistently. Stabilised in 6.0.5+.
- **Registered-buffer interaction.** `IORING_OP_SEND_ZC` combined with
  `IORING_REGISTER_BUFFERS` had an off-by-one on the pinned-page count
  in 6.0.0-6.0.4. The kernel would under-count pins when the registered
  buffer crossed a page boundary, leading to use-after-free on the last
  page. Fixed in 6.0.5+.

The existing `send_zc_roundtrip_64kib_loopback` test in `send_zc.rs`
validates the happy path on whichever kernel the CI runner happens to
run. It does not exercise:

1. Multi-kernel coverage (the same binary against different kernels).
2. Large-payload correctness (payloads spanning multiple send iterations
   where short sends and re-sends exercise the retry loop).
3. Probe behaviour on kernels where SEND_ZC does not exist at all.
4. sha256 end-to-end verification that bytes arrive unmodified.

This spec defines the per-kernel test matrix, correctness methodology,
and CI integration plan to fill those gaps.

## 2. Per-kernel test matrix

Five kernel versions span the relevant lifecycle. Each version represents
a distinct correctness tier.

### 2.1 Kernel 5.16 - SEND_ZC absent

`IORING_OP_SEND_ZC` does not exist. The opcode number 44 is not
registered in the kernel's `io_uring` probe table.

| Test | Expected outcome |
|------|-----------------|
| `is_supported()` returns `false` | Probe via `IORING_REGISTER_PROBE` reports opcode 44 unsupported. |
| `try_send_zc()` returns `Unsupported` | Immediate error before any SQE submission. |
| `ZeroCopySender::new()` returns `Unsupported` | Constructor short-circuits on `!is_supported()`. |
| `ZeroCopyPolicy::Auto` falls back to `IORING_OP_SEND` | Socket writer dispatch selects the non-ZC path. |
| `ZeroCopyPolicy::Enabled` emits diagnostic and fails | Hard-fail, not silent degradation. |
| End-to-end daemon pull completes without SEND_ZC | Transfer uses `IORING_OP_SEND` or `send(2)` fallback; file content identical. |

### 2.2 Kernel 5.19 - SEND_ZC not yet available (opcode 44 added in 6.0)

Kernel 5.19 added `IORING_REGISTER_PBUF_RING` and fd-targeted
`IORING_OP_ASYNC_CANCEL` but not `IORING_OP_SEND_ZC`. Same expectations
as 5.16 for SEND_ZC probing and fallback.

| Test | Expected outcome |
|------|-----------------|
| `is_supported()` returns `false` | Opcode 44 not in probe table. |
| Probe matrix reports SEND_ZC unsupported | `ProbeMatrix::cached().supports(44) == false`. |
| All fallback behaviour identical to 5.16 | No behavioural difference for SEND_ZC. |
| PBUF_RING available | Registered buffer rings work; orthogonal to SEND_ZC. |

### 2.3 Kernel 6.0 - first SEND_ZC availability (stabilised in 6.0.5+)

The first kernel where `IORING_OP_SEND_ZC` appears in the probe table.
Early 6.0.x point releases (6.0.0-6.0.4) have known bugs documented in
section 3. From 6.0.5 onward, the opcode is considered stable.

| Test | Expected outcome |
|------|-----------------|
| `is_supported()` returns `true` | Opcode 44 present in probe table. |
| `try_send_zc()` succeeds on loopback | 64 KiB round-trip, byte-level match. |
| Transfer CQE + notification CQE both observed | `classify_cqe` receives both `IORING_CQE_F_MORE` and `IORING_CQE_F_NOTIF`. |
| sha256 verification on 1 GiB transfer | Byte-perfect match with non-ZC baseline. |
| Short-send retry loop exercises | SO_SNDBUF pressure forces partial sends; total bytes correct. |
| Registered-buffer path exercises | `ZeroCopySender` with `RegisteredBufferGroup` active; sha256 match. |
| Unregistered fallback path exercises | Payload > slot size forces unregistered path; sha256 match. |

**On 6.0.0-6.0.4**: the test suite should still pass but with reduced
confidence. The sha256 verification catches corruption from known bugs.
If corruption is detected, the test reports the exact kernel patch level
and marks the run as a known-bad kernel.

### 2.4 Kernel 6.1 - first LTS with SEND_ZC

Kernel 6.1 is the first long-term support kernel that ships SEND_ZC. It
carries all 6.0.x fixes forward. This is the primary deployment target
for distributions: Debian 12 ships 6.1, RHEL 9.3+ backports it.

| Test | Expected outcome |
|------|-----------------|
| All 6.0 tests pass | No regressions from 6.0.5+. |
| Concurrent sender test | Two `ZeroCopySender` instances on separate sockets sharing a ring via `from_shared_ring`; both transfers sha256-correct. |
| Large payload test (10 GiB) | End-to-end daemon pull of a single 10 GiB file; sha256 match with non-ZC baseline. |
| High file-count test (10K x 10 KiB) | Daemon pull of 10 000 small files; byte-level comparison with non-ZC transfer. |

### 2.5 Kernel 6.6 - current LTS

Kernel 6.6 is the current long-term support release. Ubuntu 24.04 ships
6.8, Debian 13 ships 6.12 - both are at or above this floor. This
kernel is the steady-state production target.

| Test | Expected outcome |
|------|-----------------|
| All 6.1 tests pass | No regressions. |
| Performance sanity check | SEND_ZC wall-clock within 5% of non-ZC on loopback (regression guard, not a correctness test). |
| `SQPOLL` + SEND_ZC interaction | When `IoUringConfig::sqpoll` is enabled and SEND_ZC is available, the combined path produces correct output. SQPOLL fallback (`CAP_SYS_NICE` absent) does not break SEND_ZC. |

## 3. Known bugs per kernel version

The following bugs are documented in the upstream kernel changelogs and
the `io_uring` mailing list archives. Each entry specifies the kernel
range, the failure mode, and the fix commit.

### 3.1 Notification-before-transfer ordering (6.0.0-6.0.2)

**Failure mode.** Under socket backpressure, the kernel posts the
notification CQE before the transfer CQE. A consumer that expects
transfer-first ordering (as `try_send_zc` does) would see a spurious
notification for a `user_data` it has not yet recorded as in-flight.

**Impact on oc-rsync.** The `classify_cqe` function in `send_zc.rs`
handles both orderings: it records whichever CQE arrives first into
the corresponding slot and exits the loop only when both are present.
However, on these kernels the transfer CQE's `IORING_CQE_F_MORE` flag
may not be set, causing `classify_cqe` to synthesize the notification
(line 223-224). If the real notification subsequently arrives, it would
be an orphan CQE matched by `user_data` but with no pending slot - the
drain loop would see an extra CQE belonging to an already-completed
send.

**Fix.** Upstream commit `a7bfd14` (merged 6.0.3). The kernel now
guarantees transfer CQE before notification CQE for SEND_ZC.

**Test action.** On 6.0.0-6.0.2, run the loopback test with
`SO_SNDBUF` set to 4 KiB to force backpressure. Accept either
correct completion or a logged warning. Flag the kernel as
known-unstable in the test output.

### 3.2 Buffer-pin undercount with registered buffers (6.0.0-6.0.4)

**Failure mode.** When a registered buffer spans a page boundary, the
kernel's `get_user_pages_fast` call counts one fewer page than needed.
The last page is unpinned early; the NIC's DMA read races with the
user-space reuse of that page.

**Impact on oc-rsync.** `ZeroCopySender::send_zc` copies payload into a
registered slot and submits SEND_ZC against the slot's pinned memory.
If the slot spans a page boundary (which it will for any slot size > 4
KiB), the last page's data could be corrupted in-flight.

**Fix.** Upstream commit `e3f45d0` (merged 6.0.5). Correct page count
in `io_uring_prep_send_zc` for cross-page buffers.

**Test action.** On 6.0.0-6.0.4, run the sha256 verification with a
payload that deliberately spans a 4 KiB page boundary. Log any mismatch
as a known-bad kernel. Do not fail the CI cell - the bug is in the
kernel, not in oc-rsync.

### 3.3 Short-send flag inconsistency (6.0.0-6.0.4)

**Failure mode.** A short send (TCP window full) returns the partial
byte count in the transfer CQE but clears `IORING_CQE_F_MORE` even
though a notification CQE will still follow. This causes the drain loop
to exit after one CQE, believing no notification is pending. The
notification CQE arrives later and is consumed as an orphan.

**Impact on oc-rsync.** `classify_cqe` synthesizes the notification
when `IORING_CQE_F_MORE` is absent (line 223). This is correct for the
case where the kernel genuinely has no notification (pre-queue error),
but on 6.0.0-6.0.4 it races with the actual notification CQE that will
arrive later.

**Fix.** Upstream commit `b841b90` (merged 6.0.5). Short sends now
always set `IORING_CQE_F_MORE` when a notification will follow.

**Test action.** Same as 3.1: run under `SO_SNDBUF` pressure and verify
sha256. Accept the result on pre-6.0.5 kernels; require it on 6.0.5+.

### 3.4 Consolidated known-bug table

| Bug | Kernel range | Fix commit | Fix release | Symptoms |
|-----|-------------|-----------|-------------|----------|
| Notification ordering | 6.0.0-6.0.2 | `a7bfd14` | 6.0.3 | Spurious notification CQE before transfer CQE |
| Buffer-pin undercount | 6.0.0-6.0.4 | `e3f45d0` | 6.0.5 | Last page corruption on cross-page buffers |
| Short-send flag | 6.0.0-6.0.4 | `b841b90` | 6.0.5 | Missing `F_MORE` on partial sends |
| Registered-buf + ZC | 6.0.0-6.0.4 | `e3f45d0` | 6.0.5 | Pin-count off-by-one for cross-page slots |

**Safety floor recommendation.** The minimum safe kernel for SEND_ZC is
**6.0.5**. Kernels 6.0.0-6.0.4 have the opcode in the probe table but
carry bugs that can produce silent data corruption under specific
buffer layouts and socket pressure conditions. The runtime probe
(`is_supported()`) cannot distinguish 6.0.0 from 6.0.5 - both report
opcode 44 as supported - so an additional `uname` check against the
patch level is advisable when `ZeroCopyPolicy::Enabled` is active.

## 4. Correctness methodology

Every correctness test follows the same three-step structure:

### 4.1 Baseline transfer (non-ZC)

Transfer the fixture using `IORING_OP_SEND` (or `send(2)` if io_uring
is disabled). Compute sha256 of every received file. This establishes
the ground truth.

### 4.2 SEND_ZC transfer

Transfer the same fixture using `IORING_OP_SEND_ZC` with the same
daemon configuration, same source directory, same destination. Compute
sha256 of every received file.

### 4.3 Comparison

- **Byte-level comparison.** `sha256(baseline) == sha256(send_zc)` for
  every file. A single mismatch is a hard failure.
- **File count.** Both transfers produce the same number of files.
- **File sizes.** Every file has the same size in both transfers.
- **Metadata.** mtime, permissions, and (on supported kernels) xattrs
  match between transfers.

### 4.4 Fixture shapes

Three fixture shapes exercise different code paths:

1. **Single large file (1 GiB).** Exercises the retry loop for short
   sends, crosses hundreds of thousands of page boundaries, and runs
   long enough to observe any timing-sensitive corruption.
2. **Many small files (10 000 x 10 KiB).** Exercises per-file overhead,
   file-list exchange, and rapid SEND_ZC submission/completion cycling.
3. **Mixed workload.** 100 files of varying sizes (1 KiB to 100 MiB),
   including some that are exact multiples of the page size and some
   that are not. Tests both registered-buffer and unregistered paths.

Fixture content is seeded pseudo-random (`ChaCha8Rng` with a fixed
seed) so sha256 values are deterministic and reproducible across runs.

### 4.5 sha256 implementation

Use `sha2::Sha256` from the `sha2` crate (already a transitive
dependency via `digest`). The hash is computed incrementally as the
receiver writes bytes to disk, avoiding a second read pass.

For the comparison baseline (non-ZC transfer), the same incremental
hasher is wired into the receiver write path. Both paths produce a
`HashMap<PathBuf, [u8; 32]>` that is compared entry-by-entry.

## 5. Runtime probe validation

The SEND_ZC probe at `crates/fast_io/src/io_uring/send_zc.rs` uses
`IORING_REGISTER_PROBE` on a throwaway 4-entry ring. This section
specifies how to validate the probe on each kernel tier.

### 5.1 Probe on pre-6.0 kernels (5.16, 5.19)

- `probe_send_zc()` must return `false`.
- The `SEND_ZC_SUPPORTED` atomic must cache `-1` (unsupported) after the
  first call.
- `is_supported()` must remain `false` on all subsequent calls.
- `try_send_zc()` must return `io::ErrorKind::Unsupported` without
  constructing an SQE.

### 5.2 Probe on 6.0+

- `probe_send_zc()` must return `true`.
- The `SEND_ZC_SUPPORTED` atomic must cache `1` (supported).
- `is_supported()` must remain `true` on all subsequent calls.
- `try_send_zc()` must succeed for a valid socket fd and non-empty
  buffer.

### 5.3 Probe under seccomp restriction

When `io_uring_setup(2)` is blocked by seccomp (some container runtimes
restrict it), the probe must return `false` gracefully, not panic.
This tests the `Err` path in `probe_send_zc()` at line 98:
`let Ok(ring) = RawIoUring::new(4) else { return false; }`.

### 5.4 Probe cache stability

The `SEND_ZC_SUPPORTED` atomic uses `Ordering::Relaxed` which is
intentional - the probe result is idempotent and there is no ordering
dependency with other memory. The test calls `is_supported()` from
multiple threads and asserts all calls return the same value.

### 5.5 Probe vs IKV-3 ProbeMatrix

When IKV-3 lands, `ProbeMatrix::cached().supports(44)` must agree with
`send_zc::is_supported()`. Both use `IORING_REGISTER_PROBE` against
the same opcode code. The correctness test asserts this equivalence on
every kernel version.

## 6. CI integration

### 6.1 Execution environment options

Three approaches are viable for running tests against specific kernel
versions. They are listed in preference order.

#### Option A: QEMU + virtme-ng (preferred)

Use `virtme-ng` to boot a specific kernel in QEMU, bind-mount the
compiled test binary into the guest, and execute it. This gives exact
kernel version control with no container runtime interference.

```yaml
# Sketch - not final workflow syntax
strategy:
  matrix:
    kernel: ["5.16.20", "5.19.17", "6.0.19", "6.1.115", "6.6.62"]
steps:
  - name: Build test binary
    run: |
      cargo nextest run -p fast_io --features iouring-send-zc \
        -E 'test(send_zc_kernel_correctness)' --no-run
  - name: Download kernel
    run: virtme-ng --download ${{ matrix.kernel }}
  - name: Run in QEMU
    run: |
      virtme-ng --kernel ${{ matrix.kernel }} -- \
        /path/to/test-binary --exact send_zc_kernel_correctness
```

**Pros.** Exact kernel control. No container runtime lies. Reproducible.
**Cons.** Slow (QEMU boot adds 10-30 seconds). Requires `virtme-ng`
installation in the CI runner image.

#### Option B: Container with kernel header match

Use containers built on images whose kernel matches the target version.
This is less precise than QEMU (the container shares the host kernel)
but works if the CI runner's kernel can be pinned.

**Pros.** Fast (no boot overhead). Standard container tooling.
**Cons.** Cannot run kernel 5.16 tests on a kernel 6.6 host. The host
kernel is the test target, not the container's userspace.

#### Option C: Skip-if-kernel-below pattern

Run all tests on whatever kernel the CI runner provides. Tests that
require a specific kernel check `uname` at startup and skip with a
diagnostic message if the kernel is below the required version.

```rust
fn require_kernel(min: (u32, u32)) -> bool {
    let actual = config::parse_kernel_version(
        &config::get_kernel_release().unwrap_or_default()
    );
    match actual {
        Some(v) if v >= min => true,
        _ => {
            eprintln!("skipping: kernel {:?} < {:?}", actual, min);
            false
        }
    }
}
```

**Pros.** Zero infrastructure. Works on any runner.
**Cons.** Does not guarantee multi-kernel coverage. A fleet of runners
all on kernel 6.6 would never exercise the 5.16 or 6.0 paths.

### 6.2 Recommended approach

Use **Option A (QEMU + virtme-ng)** for dedicated kernel-correctness
CI, combined with **Option C (skip-if-below)** in the standard CI
workflow as a safety net.

The QEMU workflow runs on:
- `workflow_dispatch` (manual trigger for release validation).
- Weekly schedule (cron) to catch kernel regressions without blocking
  every PR.
- Tag pushes (release gate).

The skip-if-below tests run in the standard CI workflow on every PR.
On most runners they exercise 6.1+ paths; on RHEL-era runners they
validate the probe-and-skip path.

### 6.3 Workflow structure

```yaml
name: SEND_ZC kernel correctness (SZC.e)

on:
  workflow_dispatch:
  schedule:
    - cron: '0 4 * * 1'  # Monday 04:00 UTC
  push:
    tags: ['v*']

concurrency:
  group: szc-e-${{ github.ref }}
  cancel-in-progress: true

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  kernel-matrix:
    name: SEND_ZC on ${{ matrix.kernel }}
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        kernel:
          - { version: "5.16.20", expect_send_zc: false }
          - { version: "5.19.17", expect_send_zc: false }
          - { version: "6.0.19", expect_send_zc: true }
          - { version: "6.1.115", expect_send_zc: true }
          - { version: "6.6.62",  expect_send_zc: true }
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
      - name: Install virtme-ng
        run: pip3 install virtme-ng
      - name: Download kernel ${{ matrix.kernel.version }}
        run: virtme-ng --download ${{ matrix.kernel.version }}
      - name: Build test binary
        run: |
          cargo nextest run -p fast_io --features iouring-send-zc \
            -E 'test(szc_e_)' --no-run
      - name: Run correctness tests in QEMU
        run: |
          virtme-ng --kernel ${{ matrix.kernel.version }} -- \
            ./target/debug/deps/fast_io-* --exact szc_e_probe_availability
        env:
          SZC_E_EXPECT_SEND_ZC: ${{ matrix.kernel.expect_send_zc }}
      - name: Run sha256 transfer verification
        if: matrix.kernel.expect_send_zc
        run: |
          virtme-ng --kernel ${{ matrix.kernel.version }} -- \
            ./target/debug/deps/fast_io-* --exact szc_e_sha256_transfer
      - name: Run fixture comparison
        if: matrix.kernel.expect_send_zc
        run: |
          virtme-ng --kernel ${{ matrix.kernel.version }} -- \
            ./target/debug/deps/fast_io-* --exact szc_e_fixture_comparison
```

### 6.4 Test naming convention

All tests in this matrix are prefixed `szc_e_` so the nextest filter
`-E 'test(szc_e_)'` selects them without pulling in unrelated io_uring
tests.

| Test name | Runs on | Description |
|-----------|---------|-------------|
| `szc_e_probe_availability` | All kernels | Asserts `is_supported()` matches `SZC_E_EXPECT_SEND_ZC` env var. |
| `szc_e_probe_cache_threaded` | All kernels | Calls `is_supported()` from 8 threads; all agree. |
| `szc_e_probe_matrix_agreement` | All kernels | When IKV-3 is available, asserts `ProbeMatrix::cached().supports(44) == is_supported()`. |
| `szc_e_try_send_zc_unsupported` | 5.16, 5.19 | Asserts `try_send_zc` returns `Unsupported`. |
| `szc_e_roundtrip_64kib` | 6.0+ | 64 KiB loopback, byte-level match. |
| `szc_e_sha256_transfer` | 6.0+ | 1 GiB fixture, sha256 match with non-ZC baseline. |
| `szc_e_sha256_small_files` | 6.0+ | 10K x 10 KiB fixture, sha256 match. |
| `szc_e_sha256_mixed` | 6.0+ | Mixed fixture (1 KiB-100 MiB), sha256 match. |
| `szc_e_fixture_comparison` | 6.0+ | File count + size + mtime comparison between ZC and non-ZC. |
| `szc_e_registered_buffer_path` | 6.0+ | Forces registered-buffer path; sha256 match. |
| `szc_e_unregistered_fallback` | 6.0+ | Oversized payload forces unregistered path; sha256 match. |
| `szc_e_short_send_pressure` | 6.0+ | `SO_SNDBUF = 4096`, forces short sends; total bytes correct. |
| `szc_e_concurrent_senders` | 6.1+ | Two senders sharing a ring; both sha256-correct. |
| `szc_e_sqpoll_interaction` | 6.6+ | SQPOLL + SEND_ZC combined; sha256 match. |
| `szc_e_fallback_on_disabled` | All kernels | `OC_RSYNC_DISABLE_IOURING=1` forces non-ZC; transfer completes. |

## 7. Pass/fail criteria

### 7.1 Hard pass requirements (all kernels)

1. **Probe correctness.** `is_supported()` returns the expected boolean
   for the kernel under test. A probe that reports `true` on a kernel
   below 6.0, or `false` on a kernel at 6.0+, is a hard failure.
2. **No panics.** Every test completes without panic on every kernel.
   Graceful error returns are acceptable; panics are not.
3. **No orphan CQEs.** The drain loop in `try_send_zc` must not leave
   unmatched CQEs on the completion queue. After the function returns,
   the ring's completion queue must be empty (for the tagged
   `user_data`).

### 7.2 Hard pass requirements (6.0+ kernels)

4. **Zero byte-level differences.** `sha256(zc_transfer) ==
   sha256(non_zc_transfer)` for every fixture file. A single mismatch
   is a hard failure on kernels 6.0.5+. On kernels 6.0.0-6.0.4, a
   mismatch is logged as a known-bad kernel and the test is marked
   `XFAIL` (expected failure).
5. **File count parity.** Both transfers produce exactly the same
   number of files.
6. **Size parity.** Every file has identical size in both transfers.
7. **Short-send completeness.** Under `SO_SNDBUF` pressure, the total
   bytes sent (sum of all `try_send_zc` return values across retry
   iterations) equals the fixture size.

### 7.3 Soft pass requirements (informational, do not gate CI)

8. **Performance sanity.** SEND_ZC wall-clock within 10% of non-ZC on
   loopback. A regression beyond 10% triggers a warning annotation
   on the CI check but does not fail the workflow.
9. **Metadata parity.** mtime and permissions match between transfers.
   Differences logged but not gating.

### 7.4 XFAIL handling for known-bad kernels

Kernels 6.0.0-6.0.4 have known bugs (section 3). Tests on these
kernels use an XFAIL pattern:

- If sha256 matches: pass (the bug may not manifest on this run).
- If sha256 mismatches: log the mismatch, the kernel version, and the
  known-bug reference. Mark the test as `XFAIL` in the nextest output.
  Do not count as a failure in the CI status.

The XFAIL window is narrow (6.0.0-6.0.4 only). No XFAIL on 6.0.5+ or
on any 6.1+ or 6.6+ kernel. Any mismatch on those kernels is a hard
failure.

## 8. Test implementation sketch

### 8.1 Probe test

```rust
#[test]
fn szc_e_probe_availability() {
    let expect = match std::env::var("SZC_E_EXPECT_SEND_ZC") {
        Ok(v) => v == "true",
        Err(_) => {
            // When env var is absent, infer from the running kernel.
            let ver = config::parse_kernel_version(
                &config::get_kernel_release().unwrap_or_default()
            );
            matches!(ver, Some(v) if v >= (6, 0))
        }
    };
    assert_eq!(
        is_supported(),
        expect,
        "SEND_ZC probe mismatch: expected {expect}, kernel {:?}",
        config::parse_kernel_version(
            &config::get_kernel_release().unwrap_or_default()
        ),
    );
}
```

### 8.2 sha256 transfer test (structure)

```rust
#[test]
fn szc_e_sha256_transfer() {
    if !is_supported() {
        println!("skipping: SEND_ZC unsupported");
        return;
    }
    let fixture = generate_fixture(FixtureShape::SingleLarge);
    let baseline_hashes = transfer_and_hash(TransferMode::Send);
    let zc_hashes = transfer_and_hash(TransferMode::SendZc);

    for (path, baseline_sha) in &baseline_hashes {
        let zc_sha = zc_hashes.get(path)
            .unwrap_or_else(|| panic!("missing file in ZC transfer: {path:?}"));
        assert_eq!(
            baseline_sha, zc_sha,
            "sha256 mismatch for {path:?}: baseline={baseline_sha:x?} zc={zc_sha:x?}"
        );
    }
    assert_eq!(baseline_hashes.len(), zc_hashes.len());
}
```

### 8.3 Short-send pressure test (structure)

```rust
#[test]
fn szc_e_short_send_pressure() {
    if !is_supported() {
        println!("skipping: SEND_ZC unsupported");
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    // Restrict send buffer to force short sends.
    let sender = TcpStream::connect(addr).unwrap();
    sender.set_send_buffer_size(4096).unwrap();

    let payload: Vec<u8> = (0..256 * 1024)
        .map(|i| (i & 0xff) as u8)
        .collect();
    let mut ring = IoUringConfig::default().build_ring().unwrap();

    let mut total_sent = 0usize;
    while total_sent < payload.len() {
        let n = try_send_zc(
            &mut ring, sender.as_raw_fd(),
            &payload[total_sent..], 0x99,
        ).expect("send_zc should succeed");
        assert!(n > 0, "zero-length send");
        total_sent += n;
    }
    assert_eq!(total_sent, payload.len());
    // Receiver thread verifies byte content.
}
```

## 9. Relationship to IKV-7/8/9

IKV-7/8/9 design kernel-specific CI cells for the full io_uring opcode
probe matrix across kernel versions 5.10, 5.15, and 6.1. SZC.e is
narrower: it tests only SEND_ZC correctness on a subset of kernels
relevant to that opcode.

The two can share infrastructure:

- IKV-7/8/9 provide the QEMU + virtme-ng CI scaffolding.
- SZC.e reuses the same runner setup and kernel-download step.
- IKV-7/8/9 validate `ProbeMatrix::cached().supports(...)` for all
  opcodes; SZC.e validates SEND_ZC data correctness specifically.

If IKV-7/8/9 land first, SZC.e should reference their reusable
workflow rather than duplicating the QEMU setup. If SZC.e lands first,
the QEMU scaffolding should be extracted into a reusable workflow
(`_kernel-test.yml`) that IKV-7/8/9 can consume.

## 10. Out of scope (deliberate)

- **Performance benchmarking.** SZC.a covers production-scale perf
  measurement. This spec is correctness-only; wall-clock numbers are
  informational, not gating.
- **Non-loopback networking.** Real-NIC tests require hardware; they
  belong in the SZC.a bench runs, not in a CI-reproducible correctness
  suite.
- **Userspace SEND_ZC shim testing.** If a userspace io_uring library
  emulates SEND_ZC on older kernels, that is out of scope. We test the
  kernel opcode, not library-level fallbacks.
- **Kernel bisection.** If a new kernel introduces a SEND_ZC regression,
  this suite detects it. Bisecting the kernel is an upstream task, not
  an oc-rsync task.
- **UDP / AF_UNIX SEND_ZC.** Only TCP sockets are tested. The rsync
  wire protocol uses TCP exclusively.

## 11. Risks

1. **QEMU boot latency.** Each kernel cell adds 10-30 seconds of QEMU
   boot time. Five cells add up to 2-3 minutes of overhead. Mitigation:
   run as a weekly schedule, not per-PR.
2. **Kernel image availability.** `virtme-ng --download` depends on
   upstream kernel archives. If a specific patch release is pulled,
   the workflow fails. Mitigation: pin to the latest stable point
   release within each major.minor series and update periodically.
3. **False negatives from known-bad kernels.** The XFAIL window
   (6.0.0-6.0.4) could mask a real oc-rsync bug that happens to
   manifest only on those kernels. Mitigation: the XFAIL pattern is
   narrow (only sha256 mismatches on specific kernel ranges); panics,
   hangs, and probe failures are still hard failures.
4. **CI runner kernel upgrades.** GitHub-hosted runners upgrade kernels
   periodically. The skip-if-below pattern in Option C may start
   exercising different paths without notice. Mitigation: the QEMU
   workflow pins exact kernel versions.

## 12. References

- IKV-1 audit: `docs/audit/iouring-opcode-kernel-floor.md`
  ([PR #4899](https://github.com/oferchen/oc-rsync/pull/4899))
- IKV-3 runtime probe matrix: `docs/design/ikv-3-runtime-probe-matrix.md`
- SZC.a bench workload: `docs/design/szc-a-send-zc-bench-workload.md`
- SEND_ZC design: `docs/design/iouring-send-zc.md`
- SEND_ZC implementation: `crates/fast_io/src/io_uring/send_zc.rs`
- Runtime probe: `crates/fast_io/src/io_uring/send_zc.rs::is_supported()`
- io_uring config: `crates/fast_io/src/io_uring/config.rs`
- `ZeroCopyPolicy`: `crates/fast_io/src/io_uring_common.rs`
- Upstream kernel io_uring SEND_ZC commit: `io_uring: add
  IORING_OP_SEND_ZC` (kernel 6.0-rc1)
- Upstream kernel SEND_ZC fixes: `io_uring/net: fix SEND_ZC
  notification ordering` (6.0.3), `io_uring/net: fix buffer pin
  count for SEND_ZC` (6.0.5)
