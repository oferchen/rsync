# SQM-2.b: implementation design for the SQPOLL + mmap mlock workaround

Tracking task: SQM-2.b (design-only). Predecessors:

- SQM-1.a / SQM-1.b - reproducer and symptom doc
  (`crates/fast_io/tests/repro_sqpoll_mmap.rs`,
  `docs/design/sqpoll-mmap-race-symptoms.md`).
- SQM-1.c - 791-line full spec of all three candidates
  (`docs/design/sqm-1c-workaround-spec.md`).
- SQM-2.a - candidate scoring + decision
  (`docs/design/sqm-2a-workaround-scoring.md`); selected
  Candidate 2 (`mlock` the basis window) with Candidate 3
  (per-basis dispatch) as fallback.

Successor: SQM-3 consumes this document and produces the
implementation PR. SQM-4 re-benches against the
SMR-1 baseline to confirm the ~10-15% NVMe throughput
recovery.

This document does not change source. It locks the implementation
shape SQM-3 must follow.

## 1. Implementation choice locked

**Candidate 2 (`mlock` the basis window) is the implementation.**
Candidate 3 (per-basis dispatch via the existing `SQPOLL_FALLBACK`
path at `crates/fast_io/src/io_uring/config.rs:346-373`) stays as
the unconditional downgrade target. Candidate 1
(`MADV_WILLNEED`) is not implemented under SQM-2; it remains
available as an additive tuning layer under SMR-3b if SQM-4
benches motivate it.

SQM-2.a's strict-dominance argument is the rationale:

> "Candidate 2 strictly dominates candidate 1 on the load-bearing
> axis (NVMe perf retained without a hidden-race regression) and
> strictly dominates candidate 3 on the only axis where they
> differ (NVMe perf retained vs ~10-15% loss)."
> -- `docs/design/sqm-2a-workaround-scoring.md`, "Is the scoring
> inconclusive?"

The strictness against Candidate 1 is the test-surface axis:
`MADV_WILLNEED` is a hint the kernel may ignore under memory
pressure (`mm/madvise.c::force_page_cache_readahead`), so a passing
`repro_sqpoll_mmap.rs` cell under Candidate 1 is indistinguishable
from "the kernel happened to readahead in time". `mlock` produces a
deterministic post-condition: post-`pin`, `mincore(2)` returns
`0x01` for every page in the range; the SQPOLL kthread cannot
fault. The race is structurally closed, not statistically masked.

The strictness against Candidate 3 is the NVMe-perf axis: Candidate
3's defensive disable costs ~10-15% NVMe throughput on large-basis
plans per `project_sqpoll_disabled_with_mmap.md`; Candidate 2's
per-slide overhead is `~5us mlock + ~5us munlock` against a
~500us slide service time, i.e. < 2% overhead. The recovered
throughput is the whole motivation for SQM-2.

## 2. Rollback criteria and threshold

SQM-2.a recommended "if `RLIMIT_MEMLOCK` breaks N% of users,
revert to Candidate 3". This section sets `N = 5%` and defines
the observability that triggers each escalation step.

### Threshold definition

`N` = percentage of `mlock`/`mlock2` calls on the `WiredWindow::pin`
path that return a downgrade-class `errno` (`EAGAIN`, `EPERM`,
`ENOMEM`), measured per ring lifetime and aggregated across all
production observations in a CI cycle.

**`N = 5%` is the code-revert trigger.** Below 5% the automatic
fallback handles the breakage transparently and Candidate 2 stays
in tree as the default. At or above 5%, the operator-config burden
(`RLIMIT_MEMLOCK` bump on every host) exceeds the perf-recovery
value, and SQM-3's PR is reverted, falling back to Candidate 3 as
the official path.

The 5% bound mirrors the SMR-2 decision framework's bench-cell
significance threshold
(`docs/design/mmap-vs-sqpoll-decision.md`, "Tie-breaker margin")
and is the smallest band where the SMR-1 NVMe bench can still
detect a regression against the recovered baseline with the
default 100-iteration sample (Welch's t-test, alpha = 0.05).

### Two-tier signal

The wrapper exposes two counters via the existing telemetry seam
(`crates/fast_io/src/telemetry.rs` or its successor named in the
SQM-3 PR if telemetry has been restructured by then):

| Counter | Semantics |
|---|---|
| `sqm2_mlock_attempts` | Increment on every `WiredWindow::pin` entry. |
| `sqm2_mlock_downgrades` | Increment on every `is_downgrade_errno`-classified return from `mlock`/`mlock2`. |

The ratio `sqm2_mlock_downgrades / sqm2_mlock_attempts` is the `N`
above.

**Tier 1 (automatic fallback, no human action).** A single
downgrade-class `errno` from `mlock` flips the existing
`SQPOLL_FALLBACK` flag for that ring and routes the SQE through
the regular ring. This is the per-call mitigation; it is not a
revert trigger.

**Tier 2 (code revert, human action).** When
`sqm2_mlock_downgrades / sqm2_mlock_attempts >= 0.05` across a
representative observation window (defined below), the on-call
engineer files an issue referencing `project_sqpoll_disabled_with_mmap.md`
and reverts SQM-3's PR. The default reverts to Candidate 3 with no
code change, only flipping the Cargo feature `sqpoll-mlock-basis`
to off in the default profile. A subsequent PR removes the
wrapper module if the revert holds for two CI cycles.

### Observation window

The 5% ratio is measured over:

- **CI cycle:** every cell of the nightly SMR-1 bench harness
  (`crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`) that
  exercises the SQPOLL path.
- **Production telemetry:** when production telemetry exists (it
  does not today; tracked under `project_telemetry_*` follow-ups),
  the ratio is aggregated per-host per-day. Until then the CI
  cycle is the sole authority and the threshold is conservative
  (a CI cell is one host on one kernel; production diversity is
  not yet observed).

### Trigger summary

| Signal | Source | Action | Reversibility |
|---|---|---|---|
| Single `EAGAIN` / `EPERM` / `ENOMEM` from `mlock` | Per-call | Downgrade this submission via `SQPOLL_FALLBACK` | Automatic, transparent |
| `sqm2_mlock_downgrades / sqm2_mlock_attempts >= 0.05` over one CI cycle | Bench harness | File P1 issue, no immediate revert | Investigate first |
| Same `>= 0.05` ratio over two consecutive CI cycles | Bench harness | Revert SQM-3 PR; flip `sqpoll-mlock-basis` default to off | Feature-gate flip |
| Same `>= 0.05` ratio over four consecutive CI cycles | Bench harness | Delete `wired_window.rs`; document Candidate 3 as terminal | Source delete (multi-PR) |
| Truncation-race `SIGBUS` in production (failure mode 4) | Out-of-band crash | Not an mlock failure; verify `BasisWriterKind::BufferedMap` is still selected at `crates/transfer/src/delta_apply/applicator.rs:154-184` | Bug in mitigation 4, not in SQM-2 |

## 3. Implementation surface

### Module location

**New file: `crates/fast_io/src/sqpoll_basis.rs`.**

Rationale for a new file over extending an existing module:

- `crates/fast_io/src/io_uring/config.rs` already owns
  `build_ring` and is the substrate for the defensive disable;
  adding the wiring primitive there couples ring construction with
  per-SQE wiring, violating single-responsibility.
- `crates/fast_io/src/io_uring/registered_buffers/submit.rs` is
  the natural *caller* but should not own the primitive.
- `crates/fast_io/src/mmap_reader.rs` owns the `MmapReader`
  abstraction; wiring is conceptually about the SQPOLL submission
  window, not about reading.
- A dedicated `sqpoll_basis.rs` keeps the
  `#[allow(unsafe_code)]` attribute scoped to one file, matches
  the unsafe-code policy directive that `fast_io` is the
  consolidation crate for unsafe code, and is the canonical name
  SQM-3 reviewers will expect.

Submodule layout inside `sqpoll_basis.rs`:

```text
sqpoll_basis.rs
  pub(crate) struct WiredBasisWindow { ... }     // RAII guard
  pub(crate) fn align_window(...) -> AlignedWindow
  fn can_use_mlock2() -> bool                    // OnceLock cache
  fn rlimit_memlock() -> Option<RLimit>          // OnceLock cache
  fn is_downgrade_errno(e: &io::Error) -> bool   // EAGAIN/EPERM/ENOMEM
  pub(crate) struct MmapBasisFlagGuard           // re-export from io_uring_common
```

The `MmapBasisFlagGuard` lives next to the
`mmap_basis_active` flag definition at
`crates/fast_io/src/io_uring_common.rs:106-114` (per SQM-1.c
section "Wrapper pseudo-code"); `sqpoll_basis.rs` only re-exports
the type so callers see one import path.

### Public API

```rust,ignore
/// RAII wrapper that pins a basis-file window in physical memory
/// for the duration of an SQPOLL submission.
///
/// On `pin`, every page in `[basis_fd_window.addr ..
/// basis_fd_window.addr + basis_fd_window.len)` is wired
/// (`VM_LOCKED`); the SQPOLL kthread cannot take a fault on the
/// wired range. On `Drop`, the range is unwired via `munlock`.
///
/// Returns `IoUringError::Downgrade` when `mlock`/`mlock2` returns
/// `EAGAIN`, `EPERM`, or `ENOMEM`; the caller is expected to
/// route the submission through the regular (non-SQPOLL) ring.
/// Returns `IoUringError::Fatal` for `EINVAL` (programmer error)
/// or any other unexpected `errno`.
pub(crate) struct WiredBasisWindow {
    addr: *mut libc::c_void,
    len: usize,
}

impl WiredBasisWindow {
    pub(crate) fn new(
        basis_fd: BorrowedFd<'_>,
        range: Range<u64>,
    ) -> Result<Self, IoUringError>;

    pub(crate) fn len(&self) -> usize;
    pub(crate) fn as_ptr(&self) -> *const u8;
}

impl Drop for WiredBasisWindow {
    fn drop(&mut self);
}
```

Notes:

- `basis_fd: BorrowedFd<'_>` is the basis-file file descriptor
  (already held open by `MmapReader`); the wrapper does not own
  the fd. `range: Range<u64>` is the basis-file byte range
  matching the SQE. Internally, `new` maps the range through the
  existing `MmapReader` to get the user-virtual address, then
  calls `align_window` and the wiring syscall.
- `IoUringError` is the existing error type at
  `crates/fast_io/src/io_uring_common.rs::IoUringError`. SQM-3
  must add the `Downgrade` and `Fatal` variants if they do not
  exist; they map cleanly onto the `is_downgrade_errno`
  classification.
- `len()` and `as_ptr()` are convenience accessors for the
  caller to pass into `submit_read_fixed_sqe`.

### Integration into SMR-3c per-file dispatch site

The SMR-3c dispatcher at
`crates/fast_io/src/adaptive_dispatch.rs::pick` returns
`BasisReadBackend::IoUring` or `BasisReadBackend::Mmap`. SQM-2's
hook is inside the io_uring arm of the consumer (currently at
`crates/fast_io/src/io_uring/registered_buffers/submit.rs`):

```rust,ignore
// Existing call, unchanged:
let backend = adaptive_dispatch::pick(file_size_hint, &ewma);

if backend == BasisReadBackend::IoUring && ring_is_sqpoll(&ring) {
    let window = match WiredBasisWindow::new(basis_fd, slide_range) {
        Ok(w) => w,
        Err(IoUringError::Downgrade) => {
            telemetry::incr("sqm2_mlock_downgrades", 1);
            return submit_via_regular_ring(ring_fallback, sqe);
        }
        Err(e) => return Err(e), // Fatal: surface to transfer.
    };
    telemetry::incr("sqm2_mlock_attempts", 1);

    let _flag = MmapBasisFlagGuard::clear_for(&window);
    submit_read_fixed_sqe(&ring, &slot, window.as_ptr(), window.len())?;
    ring.submit_and_wait(1)?;
    // `window` drops here: munlock + flag re-set.
} else {
    // Non-SQPOLL or non-io_uring path: unchanged.
    submit_read_fixed_sqe(&ring, &slot, basis_ptr, basis_len)?;
    ring.submit_and_wait(1)?;
}
```

The `MmapBasisFlagGuard::clear_for(&window)` call flips
`mmap_basis_active` to `false` for the wired window's lifetime,
so any *new* ring construction inside the guard sees the cleared
flag and is allowed to take SQPOLL. The guard's `Drop` re-asserts
the flag. Per SQM-1.c "Composition with `mmap_basis_active`": the
flag is `true` outside the wired window, `false` inside.

`submit_via_regular_ring` reuses the existing regular ring already
constructed by `build_ring` when `SQPOLL_FALLBACK` is set; no new
ring is built per submission.

### Cargo feature gate

**Feature name: `sqpoll-mlock-basis`.** Default-on for Linux,
no-op elsewhere.

```toml
# crates/fast_io/Cargo.toml
[features]
default = [..., "sqpoll-mlock-basis"]
sqpoll-mlock-basis = []
```

```rust,ignore
// crates/fast_io/src/lib.rs
#[cfg(all(target_os = "linux", feature = "io_uring",
          feature = "sqpoll-mlock-basis"))]
mod sqpoll_basis;

#[cfg(not(all(target_os = "linux", feature = "io_uring",
              feature = "sqpoll-mlock-basis")))]
mod sqpoll_basis_stub;

#[cfg(all(target_os = "linux", feature = "io_uring",
          feature = "sqpoll-mlock-basis"))]
pub(crate) use sqpoll_basis::WiredBasisWindow;

#[cfg(not(all(target_os = "linux", feature = "io_uring",
              feature = "sqpoll-mlock-basis")))]
pub(crate) use sqpoll_basis_stub::WiredBasisWindow;
```

The stub `WiredBasisWindow` is zero-cost: `new` returns
`IoUringError::Downgrade` unconditionally, so the caller routes
through the regular ring exactly as it does in production today.
This keeps the call-site API uniform across all targets and
removes the need for `#[cfg]` at the call site.

The feature is default-on so users get the perf recovery without
opting in. Per section 2's rollback criteria, operators can flip
the feature off without a source revert if the `5%` threshold
trips on their kernel/distro.

### Wiring granularity

**Per-SQE-batch (one wired window per submission cycle).** SQM-1.c
section "Exact syscall sequence and flags" sets the window
granularity at `sq_entries * io_uring_buffer_size`, default
`64 * 64 KiB = 4 MiB`. The pinned working set per ring is
bounded by `sq_entries * buffer_size` regardless of basis-file
size; at depth 32 and 1 MiB windows the per-ring pin is 32 MiB,
well inside any `RLIMIT_MEMLOCK` after the operator bump in
section 2.

Per-file wiring would pin the whole basis (multi-GiB possible)
and exceed `RLIMIT_MEMLOCK` immediately on real workloads; per-SQE
(individual) wiring would issue `mlock` + `munlock` thousands of
times per second and dominate the perf recovery the wrapper is
meant to deliver. Per-batch is the SQM-2.a-blessed granularity.

## 4. Test plan

The plan formalises SQM-1.c's pre-classified test items into three
test files, each with a clear pass condition.

### Test 1: regression guard against the race

**File: `crates/fast_io/tests/repro_sqpoll_mmap.rs` (existing,
modified).**

- **Pre-condition:** the SQM-1.a reproducer.
- **With `sqpoll-mlock-basis` feature on:** every iteration must
  report `status=ok`. The race is structurally closed.
- **With `sqpoll-mlock-basis` feature off (regression guard):**
  at least one iteration must report `status=efault`, `timeout`,
  `eagain`, or `errno=N` on a kernel known to exhibit the race
  (per SQM-1.b matrix). This guarantees the test would *catch* a
  regression if a future change disabled mlock by accident.

SQM-3 implementation must add a `#[test]` (not `#[ignore]`) variant
that asserts the feature-on path passes on every CI runner; the
feature-off variant stays `#[ignore]` and runs in the SMR
bench-cell harness only.

### Test 2: fault injection

**File: `crates/fast_io/tests/sqpoll_mlock_fault_injection.rs`
(new).**

Asserts the fallback path lights up correctly for every
downgrade-class `errno`. The test uses `setrlimit(RLIMIT_MEMLOCK,
...)` and `prlimit` to drive the failure modes:

| Scenario | Trigger | Assertion |
|---|---|---|
| `EAGAIN` on lock-count exceeded | `setrlimit(RLIMIT_MEMLOCK, { 4096, 4096 })`; attempt 1 MiB pin | `WiredBasisWindow::new` returns `IoUringError::Downgrade`; `SQPOLL_FALLBACK.load() == true`; transfer succeeds via regular ring |
| `EPERM` (non-root, no `CAP_IPC_LOCK`, request > limit) | `setrlimit(RLIMIT_MEMLOCK, { 0, 0 })` then attempt pin | Same downgrade path; one `debug_log!(Io, 1, ...)` line emitted |
| `ENOMEM` (kernel out of memory for pinning) | Difficult to deterministically trigger; mocked via `LD_PRELOAD` shim that swaps `mlock` for a stub returning -1 with `errno = ENOMEM` | Same downgrade path |
| Windows `ERROR_WORKING_SET_QUOTA` analogue | Stub path on Windows; `WiredBasisWindow::new` returns `IoUringError::Downgrade` unconditionally | Caller routes via regular ring; no SQPOLL on Windows in the first place |
| `EINVAL` (programmer bug) | Pass a misaligned address through a test-only `pin_unaligned` constructor | `WiredBasisWindow::new` returns `IoUringError::Fatal`; transfer surfaces the error |

The `LD_PRELOAD` shim is acceptable as a unit-test technique
because the test runs only on Linux CI with shell-spawn capability
(`crates/fast_io/tests/`); cross-platform CI runners skip the
`ENOMEM` row.

### Test 3: throughput recovery bench

**File: `crates/fast_io/benches/mlock_vs_sqpoll_disabled_basis_throughput.rs`
(new, criterion-based).**

Three cells:

| Cell | Setup | Expected |
|---|---|---|
| `baseline_sqpoll_off` | `sqpoll-mlock-basis = off`, mmap basis active -> Candidate 3 path (defensive disable, no SQPOLL) | Throughput `T_baseline` |
| `mlock_sqpoll_on` | `sqpoll-mlock-basis = on`, mmap basis active -> Candidate 2 path (mlock + SQPOLL) | Throughput `T_mlock >= T_baseline * 1.10` (i.e. >= 10% recovery) |
| `sqpoll_off_no_mmap` | `sqpoll-mlock-basis = off`, no mmap (the SQPOLL-clean ceiling) | Throughput `T_ceiling`; `T_mlock` should be within 5% of `T_ceiling` |

Per SMR-1's published 10-15% NVMe loss the recovery target is the
midpoint; the bench passes if `(T_mlock - T_baseline) /
T_baseline >= 0.10`. Below 10%, SQM-4 escalates to the SQM-2.a
re-evaluation path (downgrade to Candidate 3 as primary).

The bench is `#[ignore]` by default and runs in the nightly
SMR bench cell; it does not gate PR CI.

## 5. Fault-injection paths

Each scenario below has a corresponding fault-injection test in
section 4 Test 2. The runtime behaviour table is the *contract*
the wrapper must satisfy:

| Source | `errno` | Wrapper action | User-visible effect |
|---|---|---|---|
| `mlock` / `mlock2` | `EAGAIN` | Return `IoUringError::Downgrade`; caller routes via regular ring; `sqm2_mlock_downgrades` increment | None; transfer continues at Candidate 3 throughput |
| `mlock` / `mlock2` | `EPERM` | Return `IoUringError::Downgrade`; caller routes via regular ring; `sqm2_mlock_downgrades` increment; one `debug_log!(Io, 1, "mlock downgrade: RLIMIT_MEMLOCK insufficient and CAP_IPC_LOCK not granted; falling back to regular ring")` per ring lifetime | None at the transfer level; operator sees one log line per `oc-rsync` invocation |
| `mlock` / `mlock2` | `ENOMEM` | Return `IoUringError::Downgrade`; caller routes via regular ring; `sqm2_mlock_downgrades` increment | None; transfer continues at Candidate 3 throughput |
| `mlock` / `mlock2` | `EINVAL` | Return `IoUringError::Fatal` | `io::Error` surfaces to the transfer; treat as programmer bug, file a panic ticket |
| `mlock` / `mlock2` | `ENOSYS` (kernel < 4.4 hit on `mlock2`) | Catch in `can_use_mlock2`; transparently retry with plain `mlock` | None |
| `munlock` during `Drop` | any | Log via `debug_log!(Io, 1, "munlock failed during WiredBasisWindow drop: {errno}; page stays wired until process exit")`; do *not* panic | Process accumulates one extra wired page per failed `munlock` until exit; bounded by total `mlock` calls per process |
| `getrlimit(RLIMIT_MEMLOCK)` | any | Treat as `RLIMIT_MEMLOCK = 0` and route via regular ring | Same as `EPERM` path |
| Truncation race (file shrunk under wired window) | `SIGBUS` in kernel context | Not catchable from the wrapper; the load-bearing defence is `BasisWriterKind::BufferedMap` at `crates/transfer/src/delta_apply/applicator.rs:154-184` | Crash if mitigation 4 is bypassed; otherwise no exposure |

The `Drop`-time `munlock` failure case is intentional: the
wrapper does not panic. A failed `munlock` leaves the page wired
until process exit; the kernel reclaims it at exit. This is
strictly safer than panicking inside `Drop` (which would abort the
process and lose in-flight writes).

## 6. Cross-platform behaviour

### macOS

The kqueue-based fast path
(`crates/fast_io/src/kqueue_path.rs` or its successor; see
`docs/design/xpl-2-kqueue-cross-platform-audit.md` for the current
status) does not use SQPOLL semantics. The SQM-2 wrapper is a
no-op via `sqpoll_basis_stub::WiredBasisWindow`. macOS has `mlock`
but no `MLOCK_ONFAULT`; the stub is preferred over compiling the
POSIX path because there is no caller that could benefit.

### Windows

IOCP
(`crates/fast_io/src/iocp/` and `docs/design/iocp/`) does not have
SQPOLL semantics. The Windows analogue of the working-set hazard
is `ERROR_WORKING_SET_QUOTA`, which surfaces when
`VirtualLock(2)` exceeds the process working-set limit. The SQM-2
wrapper is a no-op via `sqpoll_basis_stub::WiredBasisWindow`; if
a future IOCP enhancement needs working-set pinning, the wrapper
can grow a Windows-active path that calls `VirtualLock`/`VirtualUnlock`
via `windows-rs`. SQM-2 does not commit to that path.

### Linux: kernel-version probe

SQM-1.b's 5-row kernel coverage matrix (Linux 5.10 LTS, 5.15 LTS,
6.1 LTS, 6.6 LTS, 6.12) is the support surface. The race is
specific to the kernel range where:

1. `IORING_SETUP_SQPOLL` exists (Linux 5.1+, present on all five
   matrix rows).
2. `IORING_REGISTER_BUFFERS` is the registered-buffer path
   (Linux 5.1+).
3. The SQPOLL kthread does *not* punt registered-mmap faults to
   `io-wq` reliably (the bug surface; SQM-1.b matrix is the source
   of truth on which rows trip).

The wrapper does not gate on kernel version; it gates on:

- `kernel_version::is_at_least(4, 4)` for `mlock2(MLOCK_ONFAULT)`
  selection (`crates/fast_io/src/kernel_version.rs`); below 4.4
  the wrapper falls back to plain `mlock`.
- `IoUringProbeResult::Supported` for the io_uring surface at
  ring-construction time (`crates/fast_io/src/io_uring_probe.rs`);
  below the probe threshold the SQPOLL path never lights up.

There is no `if kernel_version <= 6.X: skip mlock` branch. The
wrapper runs on every supported kernel that grants SQPOLL; on
kernels where the race did not exist in the first place the
wrapper is harmless (mlock + munlock with bounded overhead).

The SQM-1.b matrix populates over time; once the data shows the
race is absent on, say, all Linux >= 6.12 builds, a future SQM-5
task can add a kernel-version gate that turns the wrapper off on
those kernels and accepts the small per-slide overhead saving.
SQM-2 does not do that gating because the matrix is currently
TBD-populated.

## 7. Rollout plan

### SQM-3: implementation PR

Single PR landing:

- New file `crates/fast_io/src/sqpoll_basis.rs` with the
  `WiredBasisWindow` RAII guard and the
  `is_downgrade_errno` / `can_use_mlock2` / `rlimit_memlock`
  helpers, gated by `#[cfg(all(target_os = "linux", feature =
  "io_uring", feature = "sqpoll-mlock-basis"))]`.
- Stub file `crates/fast_io/src/sqpoll_basis_stub.rs` for every
  other target combination.
- New `MmapBasisFlagGuard` in
  `crates/fast_io/src/io_uring_common.rs`.
- Cargo feature `sqpoll-mlock-basis` added to
  `crates/fast_io/Cargo.toml`, default-on.
- Caller integration in
  `crates/fast_io/src/io_uring/registered_buffers/submit.rs`.
- Telemetry counters `sqm2_mlock_attempts` /
  `sqm2_mlock_downgrades`.
- The two test files from section 4 (the bench arrives in SQM-4).
- Operator-guidance addition to the existing
  `docs/operations/` tree (or `docs/design/sqpoll-mmap-race-symptoms.md`
  if no operations tree exists) covering the `RLIMIT_MEMLOCK`
  recommendation from SQM-1.c section "Operator guidance".

PR title: `feat(fast_io): wire SQPOLL+mmap basis windows via mlock (SQM-3)`.
Branch: `feat/sqm-3-mlock-wire-basis`. Reviewers: same set as the
SMR series.

### SQM-4: re-bench

Run the SMR-1 NVMe bench harness
(`crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`) with the
SQM-3 PR landed, plus the new
`mlock_vs_sqpoll_disabled_basis_throughput.rs` bench. Pass
criterion: `T_mlock >= T_baseline * 1.10` per section 4 Test 3.

If the recovery is below 10% but above 5%, document as a partial
win and keep the wrapper; the perf is still net-positive. If the
recovery is below 5%, downgrade SQM-3 to "experimental, default-off"
and re-open SQM-2.a to re-evaluate against Candidate 3.

### Post-bake

After SQM-4 confirms the perf recovery and the SQM-3 PR has been
in tree for **N = 4 CI cycles** with the
`sqm2_mlock_downgrades / sqm2_mlock_attempts < 0.05` ratio
holding, Candidate 3 (the defensive disable at
`crates/fast_io/src/io_uring/config.rs:346-373`) becomes pure
fallback. The flag, the disable, and the `SQPOLL_FALLBACK`
counter all stay in tree as the structural backstop for the
fault-injection path; the *primary* path is now Candidate 2.

No code is deleted post-bake. Candidate 3 is the safety net; SQM-2
explicitly composes both layers and the layered model in SQM-1.c's
"Composition contract" section is the long-term shape.

## 8. Open questions for SQM-3 implementation

The questions below are *not* decisions; they are inputs SQM-3
must resolve during implementation. Each has a recommended default
and a fallback if the default fails.

### Q1: Lazy vs eager wiring

Should `WiredBasisWindow::new` be:

- **Lazy:** mlock on first SQPOLL submission per basis window.
  Saves the mlock cost when SMR-3c's EWMA later picks the mmap
  backend (no io_uring SQE, no wiring needed).
- **Eager:** mlock at basis open. Front-loads the cost; subsequent
  submissions are mlock-free; cleaner Drop semantics.

**Recommended default: lazy.** SMR-3c can switch backends mid-file
based on EWMA throughput, and eager wiring would pay the cost on
files SMR-3c eventually routes through the mmap backend (zero
SQPOLL involvement, wiring wasted). Lazy wiring pays the cost only
on SQEs that actually submit through SQPOLL.

**Fallback if lazy hurts NVMe perf in SQM-4 bench:** switch to
eager wiring, capped at the first `min(file_size, sq_entries *
buffer_size)` bytes of each basis. The cap keeps the
`RLIMIT_MEMLOCK` budget bounded.

### Q2: Per-basis vs pooled wired windows

Should wired windows be:

- **Per-basis:** one `WiredBasisWindow` per `MmapReader`; the
  guard owns its mlock for that basis only.
- **Pooled:** a process-wide `WiredWindowPool` that recycles
  wired ranges across basis instances. Saves mlock churn when
  many small files come through.

**Recommended default: per-basis.** The pooled design adds
complexity (LRU eviction, interaction with the `mmap_basis_active`
flag, lifetime tracking across `MmapReader` drops) for unclear
benefit until SQM-4 measures the mlock cost on small-file
workloads. Per-basis matches the SQM-1.c pseudo-code one-to-one
and lets the SQM-3 PR stay small.

**Fallback if SQM-4 shows per-basis mlock churn dominates the
recovery on small-file workloads:** add a process-wide pool in a
follow-up SQM-5 task; do not block SQM-3 on it.

### Q3: SMR-3c throughput-feedback integration

SMR-3c's EWMA at
`crates/fast_io/src/adaptive_dispatch.rs:117-156` records
per-backend throughput. Two integration models:

- **Independent:** SQM-2's wiring runs invisibly to SMR-3c; the
  EWMA sees the *wrapped* io_uring throughput (including mlock
  overhead) and dispatches accordingly. The dispatch decision is
  "io_uring (with wiring) vs mmap", as a single combined cell.
- **Coupled:** the EWMA tracks io_uring-wired and io_uring-unwired
  separately; SMR-3c picks the cheaper of the two when mmap basis
  is active.

**Recommended default: independent.** Per SQM-1.c section
"Interaction with SMR-3c per-file dispatch": the wiring overhead
on 4 MiB slides is ~5 microseconds against a ~500 microsecond
slide service time (< 1% overhead), well below the EWMA's
sensitivity. The dispatch sees io_uring throughput as one cell;
the wiring is transparent.

**Fallback if SQM-4 shows the wiring overhead exceeds 5% on any
real workload:** SQM-5 adds a separate EWMA cell for the wired
path; the dispatcher then makes a three-way choice (mmap,
io_uring-unwired, io_uring-wired). This is strictly additive to
SMR-3c's existing two-way pick API and does not break the
`OC_RSYNC_ADAPTIVE_BASIS_DISPATCH` env-var contract.

## References

- `docs/design/sqpoll-mmap-race-symptoms.md` - SQM-1.b symptom
  doc and kernel-version coverage matrix (the input).
- `docs/design/sqm-2a-workaround-scoring.md` - SQM-2.a scoring
  + strict-dominance rationale (the input).
- `docs/design/sqm-1c-workaround-spec.md` - SQM-1.c full spec of
  all three candidates with pseudo-code; this doc reuses the
  `WiredWindow` pseudo-code, errno-to-downgrade mapping, and test
  classifications verbatim.
- `crates/fast_io/src/io_uring/config.rs:346-373` - the existing
  defensive disable; the long-term fallback that Candidate 2
  composes with.
- `crates/fast_io/src/io_uring_common.rs:106-114` -
  `mmap_basis_active` flag and the new `MmapBasisFlagGuard`
  location.
- `crates/fast_io/src/adaptive_dispatch.rs` - SMR-3c per-file
  dispatcher; the integration site.
- `crates/fast_io/src/kernel_version.rs` - runtime kernel-version
  probe used by `can_use_mlock2`.
- `crates/fast_io/src/io_uring_probe.rs` - io_uring availability
  probe; gates whether the SQPOLL surface is reachable.
- `crates/fast_io/tests/repro_sqpoll_mmap.rs` - SQM-1.a
  reproducer; the regression guard for Candidate 2.
- `crates/transfer/src/delta_apply/applicator.rs:154-184` -
  `BasisWriterKind::BufferedMap` selector; the load-bearing
  Layer-0 defence against the truncate `SIGBUS` failure mode.
- `docs/design/basis-file-io-policy.md` - Layer-0 invariant doc.
- `docs/design/mmap-vs-sqpoll-decision.md` - SMR-2 decision
  framework that picked SMR Option 3 as the substrate.
- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` - SMR
  resolution catalogue (Options 1/2/3 for basis-read dispatch).
- `docs/audits/io_uring_sqpoll_mmap_pagefault.md` - long-form
  audit of the page-fault hazard.
- `docs/audits/madvise-willneed-prefault.md` - Candidate 1's
  underlying audit (best-effort; not implemented under SQM-2).
- `project_sqpoll_disabled_with_mmap.md` - the ~10-15% NVMe loss
  motivating SQM-2's perf-recovery goal.
