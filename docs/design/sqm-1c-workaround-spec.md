# SQM-1.c: workaround specification for SQPOLL + mmap race

Tracking task: SQM-1.c (audit-only). Predecessors:

- SQM-1.a / SQM-1.b - reproducer + symptom doc
  (`crates/fast_io/tests/repro_sqpoll_mmap.rs`,
  `docs/design/sqpoll-mmap-race-symptoms.md`).
- SQM-2.a - candidate scoring matrix
  (`docs/design/sqm-2a-workaround-scoring.md`); selected candidate 2
  (`mlock` the basis window) with candidate 3 (per-basis dispatch) as
  fallback.

Successor: SQM-2.b consumes this document as a specification input and
produces the dispatch-site implementation design. SQM-2.b is expected to
synthesise and record a decision; the research is done here.

This document does not change source. It formalises each of the three
candidates surfaced by SQM-1.c (and scored by SQM-2.a) as a
specification suitable for a code change: pseudo-code for the wrapper
that lives in `fast_io`, the exact syscall sequence with flag values,
the failure modes by `errno`, the test plan, the rollback story, the
interaction with the existing SMR-3c per-file dispatch guard, and the
cross-platform fall-through.

The wrapper site for all three candidates is the same: a thin module
inside `crates/fast_io/src/io_uring/` that the existing
`IoUringConfig::build_ring` (`crates/fast_io/src/io_uring/config.rs:346`)
and the per-SQE submission path
(`crates/fast_io/src/io_uring/registered_buffers/submit.rs`) call into.
The dispatch flag stays `IoUringConfig::mmap_basis_active`
(`crates/fast_io/src/io_uring_common.rs:114`); each candidate either
keeps that flag binding (candidate 3), proactively flips it for a
wired window (candidate 2), or attempts a best-effort prefault before
submission (candidate 1).

The current defensive disable that all three candidates compose with
lives at `crates/fast_io/src/io_uring/config.rs:346-373`:

```text
let sqpoll_safe = sqpoll_requested && !self.mmap_basis_active;
if sqpoll_requested && !sqpoll_safe {
    SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
    // build a regular ring
}
```

## Conventions used in this spec

- Pseudo-code is Rust-flavoured but elides bookkeeping. The real
  wrapper lives in `fast_io` (per the unsafe-code policy: `fast_io`
  is the single permitted home for new `unsafe` blocks).
- `errno` is named (e.g. `EAGAIN`); the matching `io::ErrorKind` is
  named when one exists, otherwise the spec uses `io::Error::from_raw_os_error`.
- "Race re-opens" means the kernel-side hazard documented in
  `docs/design/sqpoll-mmap-race-symptoms.md` section 1 (failure modes 1
  to 4) becomes reachable again. "Race is closed" means the SQPOLL
  kthread cannot take a page fault on the wired region.
- All three candidates leave the receiver dispatch
  (`BasisWriterKind::BufferedMap` selection at
  `crates/transfer/src/delta_apply/applicator.rs:154-184`) untouched.
  That is the *load-bearing* invariant against the truncate `SIGBUS`
  failure mode (failure mode 4 in
  `docs/audits/io_uring_sqpoll_mmap_pagefault.md`); none of the SQM
  candidates substitute for it.
- "SMR-3c guard" refers to the per-file adaptive dispatcher at
  `crates/fast_io/src/adaptive_dispatch.rs::pick`, gated by the
  `adaptive-basis-dispatch` Cargo feature and the
  `OC_RSYNC_ADAPTIVE_BASIS_DISPATCH` environment variable. The
  candidates here run *under* SMR-3c: SMR-3c decides whether a given
  file uses the mmap or io_uring backend; the SQM candidate decides
  what happens when mmap is the chosen backend and SQPOLL is also
  requested on the io_uring ring that owns the same plan.

## Candidate 1: `MADV_WILLNEED` prefetch

### Mechanism summary

Issue `posix_madvise(addr, len, POSIX_MADV_WILLNEED)` on the basis
window before any SQE that references the window is published to the
SQPOLL ring. The hint asks the kernel to schedule asynchronous
readahead; it returns when the request is queued, not when the pages
are resident. Hint, not guarantee.

The existing hook is `MmapReader::advise_willneed`
(`crates/fast_io/src/mmap_reader.rs:139-143`), implemented via
`memmap2::MmapRaw::advise_range(memmap2::Advice::WillNeed, ...)`. The
non-Unix stub at `crates/fast_io/src/mmap_reader_stub.rs:71-87` is a
no-op.

### Wrapper pseudo-code

Lives in a new submodule `crates/fast_io/src/io_uring/prefault.rs`
(no `unsafe`; relies entirely on `memmap2`):

```rust,ignore
pub(crate) struct WillNeedHint<'m> {
    mmap: &'m MmapReader,
    offset: usize,
    len: usize,
}

impl<'m> WillNeedHint<'m> {
    pub(crate) fn hint(mmap: &'m MmapReader, offset: usize, len: usize)
        -> Self
    {
        // Skip when the window already fits the kernel readahead unit;
        // hint adds latency on small slides for no benefit.
        if len < READAHEAD_THRESHOLD_BYTES {
            return Self { mmap, offset, len: 0 };
        }
        let _ = mmap.advise_willneed(offset, len); // best effort
        Self { mmap, offset, len }
    }
}

// Caller site at submit time (registered_buffers/submit.rs):
let _hint = WillNeedHint::hint(&mmap_reader, slide_offset, slide_len);
submit_read_fixed_sqe(ring, &slot, basis_ptr, basis_len)?;
ring.submit_and_wait(1)?;
```

`READAHEAD_THRESHOLD_BYTES` is the SQE window size below which the hint
is skipped. Initial value: 64 KiB (one default kernel readahead window
on `/sys/block/*/queue/read_ahead_kb`). The threshold lives as a
`const` in `prefault.rs`; SMR-1 bench data is the authority for tuning.

There is no RAII Drop because the hint is fire-and-forget; readahead
runs asynchronously and there is nothing to revert.

### Exact syscall sequence and flags

Per slide (one SQE batch):

1. `madvise(addr = mmap_base + offset, len = aligned_len, advice = MADV_WILLNEED)`
   - `MADV_WILLNEED = 3` (Linux), `POSIX_MADV_WILLNEED = 3` (POSIX).
   - `addr` must be page-aligned; `memmap2::advise_range` aligns
     internally.
   - `aligned_len = align_up(len, page_size)`.
2. `io_uring_enter(ring_fd, to_submit = N, min_complete = 0, flags = IORING_ENTER_SQ_WAKEUP)`
   - The `IORING_ENTER_SQ_WAKEUP` flag is added by the `io_uring`
     crate's `submit()` when the SQ thread is asleep; not toggled by
     the wrapper.

No additional syscall on the un-hint path. `madvise(MADV_DONTNEED)` is
explicitly *not* issued on cleanup; revoking pages would defeat the
hint's purpose and risks evicting pages the kthread still needs.

### Failure modes by `errno`

| Source | `errno` | What happens | Propagation |
|---|---|---|---|
| `madvise(WILLNEED)` | `EAGAIN` | Kernel could not allocate I/O resources; readahead skipped. Race re-opens silently. | Ignored (`let _ =`); transfer continues. SQPOLL kthread may stall in `task_work` per failure mode 1. |
| `madvise(WILLNEED)` | `EBADF` | Backing file descriptor closed. Cannot occur for a live `MmapReader` (file kept open by `Mmap`). | Bug; convert to `io::Error::other` panic in debug. |
| `madvise(WILLNEED)` | `EINVAL` | `addr` not page-aligned or `len` overflows. Indicates a wrapper bug, not a runtime condition. | Ignored at runtime; `debug_assert!` in `prefault.rs` against caller misuse. |
| `madvise(WILLNEED)` | `ENOMEM` | Kernel out of memory for readahead pages. Race re-opens. | Ignored; transfer continues. Failure mode 1 reachable. |
| memory pressure | n/a | Kernel honoured the hint but evicted pages before SQE dispatch (`mm/madvise.c::force_page_cache_readahead`). | Invisible; SQPOLL kthread takes the fault. Maps to repro `status=timeout` or `status=eagain`. |
| transparent hugepages | n/a | `khugepaged` collapses 4 KiB pages into 2 MiB pages, invalidating PTEs after the hint. | Invisible; race re-opens. Documented as residual risk in `docs/audits/io_uring_sqpoll_mmap_pagefault.md` section "Transparent-hugepage NUMA migrations". |

None of the failure modes here propagate as a Rust error; the hint is
advisory. The only observable failure is a transfer-time hang or
`status=efault` in the reproducer, both indistinguishable from "the
race fired without the hint at all".

### Test plan

Direct race-closure proof is not possible with this candidate; the
test plan therefore focuses on (a) confirming the wrapper calls
`madvise` with the expected arguments, and (b) measuring whether the
hint moves the failure rate on the SQM-1.b kernel matrix.

1. Unit test in `crates/fast_io/src/io_uring/prefault.rs`:
   - Use a `tempfile`-backed `MmapReader` and intercept the syscall
     via `nix::sys::mman::mincore` post-hint. Assert at least
     `min(slide_len, /proc/sys/vm/min_free_kbytes)` bytes are
     resident within 10 ms of the call returning.
   - Negative case: skip when `len < READAHEAD_THRESHOLD_BYTES` and
     assert no `madvise` call occurs (intercept via `LD_PRELOAD` of a
     `madvise` stub - or, more portably, use `tracing` spans inside
     the wrapper and assert the span fires).
2. Integration test under `crates/fast_io/tests/repro_sqpoll_mmap.rs`:
   - Add a `repro_sqpoll_mmap_with_willneed` variant that issues the
     hint before every `READ_FIXED` SQE.
   - Run on the same kernel-version matrix populated by SQM-1.b. The
     hint passes if and only if the failure rate (`efault` +
     `timeout` + `eagain` + non-zero `errno`) drops below the
     SQM-1.b baseline by a statistically significant margin
     (Welch's t-test, alpha = 0.05, 100 iterations per cell).
3. Negative test: under `cgcreate -g memory:sqm-1c && echo 16M >
   /sys/fs/cgroup/memory/sqm-1c/memory.high && cgexec`, assert the
   reproducer still trips. This proves the hint is *not* a workaround
   under memory pressure.

The test plan does *not* claim the race is closed; it claims the hint
runs and moves the needle. Per SQM-2.a, this is the candidate's core
weakness: a passing test on an unloaded host is indistinguishable from
"the kernel got lucky".

### Rollback story

Single-site revert. Delete the `WillNeedHint::hint(...)` call from
the submission site; delete `prefault.rs`; leave
`MmapReader::advise_willneed` in place (it is independently useful for
non-SQPOLL prefetch tuning under SMR-3b).

Rollback degrades to candidate 3 (per-basis dispatch / status quo)
because the existing `build_ring` defensive disable is unchanged.

### Interaction with SMR-3c per-file dispatch

Candidate 1 is *additive* under SMR-3c. SMR-3c decides per file
whether to use the mmap or io_uring backend based on EWMA throughput.
When SMR-3c picks `BasisReadBackend::IoUring` and SQPOLL is enabled on
the ring, the wrapper issues the hint on the mmap *if and only if* the
SMR-3c dispatch left an `MmapReader` open as the fallback source. In
the steady state SMR-3c either uses the io_uring backend (no mmap, no
hint needed) or uses the mmap backend (no io_uring SQE, no hint
needed). The hint matters only at the transient SMR-3c switching
moment when both backends hold a window on the same offset.

The SMR-3c guard at
`crates/fast_io/src/adaptive_dispatch.rs:209-246` is unchanged. The
`mmap_basis_active` flag at
`crates/fast_io/src/io_uring_common.rs:114` keeps its current
semantics: still `true` whenever a `MmapReader` is live; the
`build_ring` defensive disable still trips. Candidate 1 does not
clear the flag.

### Cross-platform behaviour

| Platform | Behaviour | Citation |
|---|---|---|
| Linux | `madvise(MADV_WILLNEED)` via `memmap2`. | `crates/fast_io/src/mmap_reader.rs:139-143` |
| macOS | `madvise(MADV_WILLNEED)` via `memmap2`; weaker readahead than Linux but the syscall is honoured. | `mmap_reader.rs:139-143` |
| FreeBSD / NetBSD / OpenBSD | `posix_madvise(POSIX_MADV_WILLNEED)` via `memmap2`. | `mmap_reader.rs:139-143` |
| Windows | No-op stub. SQPOLL is Linux-only, so the wrapper is dead code on Windows regardless. | `crates/fast_io/src/mmap_reader_stub.rs:71-87` |

The wrapper itself is `#[cfg(unix)]` and the call site is
`#[cfg(all(target_os = "linux", feature = "io_uring"))]`, matching the
SQPOLL surface.

## Candidate 2: `mlock` the basis window (recommended)

### Mechanism summary

Pin the basis window in physical memory for the SQPOLL submission
lifetime. The SQPOLL kthread cannot take a fault on a wired page; the
race is structurally closed for the wired range. Cost is paid once at
`mlock` (synchronous fault-in on the user task) and once at `munlock`
(unpin), both off the kthread.

Two syscall variants:

- `mlock(addr, len)` - POSIX, faults every page in the range
  immediately. Supported on every kernel oc-rsync targets.
- `mlock2(addr, len, MLOCK_ONFAULT)` - Linux 4.4+, defers fault-in
  until first touch but still pins on fault. Cheaper for windows the
  SQPOLL kthread may not actually read.

Runtime selection via the existing `kernel_version` API
(`crates/fast_io/src/kernel_version.rs::KernelVersion::meets_minimum`).

### Wrapper pseudo-code

Lives in `crates/fast_io/src/io_uring/wired_window.rs` (single
`#[allow(unsafe_code)]` site; `fast_io` is the permitted home for new
`unsafe` per the unsafe-code policy).

```rust,ignore
pub(crate) struct WiredWindow {
    addr: *mut libc::c_void,
    len: usize,
}

impl WiredWindow {
    /// Wires `[addr .. addr+len)`. Returns `WiredWindow` on success or
    /// the raw OS error so the caller can decide whether to downgrade
    /// (EAGAIN, ENOMEM, EPERM) or abort (EINVAL).
    pub(crate) fn pin(addr: *mut libc::c_void, len: usize)
        -> io::Result<Self>
    {
        let aligned = align_window(addr, len)?;
        let rc = if can_use_mlock2() {
            // SAFETY: aligned address from align_window; len from same.
            #[allow(unsafe_code)]
            unsafe { libc::syscall(libc::SYS_mlock2, aligned.addr,
                                   aligned.len, libc::MLOCK_ONFAULT) }
        } else {
            // SAFETY: same alignment guarantee.
            #[allow(unsafe_code)]
            unsafe { libc::mlock(aligned.addr, aligned.len) as i64 }
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { addr: aligned.addr, len: aligned.len })
    }
}

impl Drop for WiredWindow {
    fn drop(&mut self) {
        // SAFETY: addr + len are the values returned by pin().
        #[allow(unsafe_code)]
        unsafe { let _ = libc::munlock(self.addr, self.len); }
    }
}

fn can_use_mlock2() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        crate::kernel_version::current()
            .map(|v| v.meets_minimum(4, 4))
            .unwrap_or(false)
    })
}
```

The caller site (`crates/fast_io/src/io_uring/registered_buffers/submit.rs`):

```rust,ignore
let window = match WiredWindow::pin(basis_ptr.cast(), basis_len) {
    Ok(w) => w,
    Err(e) if is_downgrade_errno(&e) => {
        // EAGAIN / ENOMEM / EPERM: downgrade this submission to the
        // existing defensive-disable path. SQPOLL_FALLBACK already
        // set by build_ring; we just route through the non-SQPOLL
        // ring already constructed.
        return submit_via_regular_ring(...);
    }
    Err(e) => return Err(e), // EINVAL: programmer error, surface.
};

// mmap_basis_active is cleared for the lifetime of `window` because
// the wired range is no longer faultable. Atomically toggled so the
// next ring construction (rare path) can take SQPOLL.
let _flag = MmapBasisFlagGuard::clear_for(&window);

submit_read_fixed_sqe(ring, &slot, basis_ptr, basis_len)?;
ring.submit_and_wait(1)?;
// `window` and `_flag` drop here: munlock + flag re-set.
```

The RAII guard for `mmap_basis_active` lives in
`crates/fast_io/src/io_uring_common.rs` next to the flag definition;
it flips the flag back to `true` on Drop to keep the build-time
defensive disable accurate for subsequent rings.

### Exact syscall sequence and flags

Per slide:

1. `getrlimit(RLIMIT_MEMLOCK, &rlim)` - probed once at first
   `WiredWindow::pin` call, cached in a `OnceLock<RLimit>`. Default
   for non-root processes is 64 KiB on Debian/Ubuntu, 8 MiB on RHEL.
   If `rlim.rlim_cur < per_slide_len * sq_depth`, the wrapper
   degrades pre-emptively without calling `mlock` (saves an EAGAIN).
2. On Linux 4.4+ with `mlock2` available:
   `syscall(SYS_mlock2, addr, len, MLOCK_ONFAULT)`
   - `MLOCK_ONFAULT = 0x01`.
   - Returns 0 on success, -1 on failure with `errno` set.
3. Otherwise: `mlock(addr, len)`
   - No flags argument. Faults every page in the range synchronously.
4. `io_uring_enter(...)` - unchanged.
5. On window drop: `munlock(addr, len)`
   - Errors from `munlock` are ignored (rc only meaningful for
     truncation races; we already hold the mmap so the range is
     valid).

The window granularity is one SQE batch (typically `sq_entries *
io_uring_buffer_size`, default 64 * 64 KiB = 4 MiB). The pinned
working set per ring is therefore bounded by `sq_entries *
buffer_size` regardless of basis file size.

### Failure modes by `errno`

| Source | `errno` | What happens | Propagation |
|---|---|---|---|
| `mlock` / `mlock2` | `EAGAIN` | RLIMIT_MEMLOCK exceeded or temporary kernel resource pressure. | Caller downgrades to regular ring via existing `SQPOLL_FALLBACK` path. No transfer-level error. |
| `mlock` / `mlock2` | `EINVAL` | `addr` not page-aligned or `len = 0`. Indicates wrapper bug; `align_window` should have caught it. | Surfaced as `io::Error` to the transfer; `debug_assert!` in `align_window`. |
| `mlock` / `mlock2` | `ENOMEM` | Address range outside the process address space, or insufficient memory for pinning beyond RLIMIT_MEMLOCK. | Caller downgrades to regular ring. |
| `mlock` / `mlock2` | `EPERM` | Non-root process and `CAP_IPC_LOCK` not granted, and the request exceeds RLIMIT_MEMLOCK (the first `mlock` after the limit is hit returns `EAGAIN`; subsequent attempts return `EPERM` on some kernels). | Caller downgrades to regular ring; emits `debug_log!(Io, 1, ...)` once per ring lifetime to surface the operator-config issue. |
| `mlock2` | `ENOSYS` | Kernel < 4.4. `can_use_mlock2` should have caught this; defensive `match` on `ENOSYS` re-issues `mlock` (POSIX fallback). | Transparent retry; no error to caller. |
| `getrlimit` | n/a | `getrlimit(RLIMIT_MEMLOCK)` is infallible on supported kernels. | Bug if it fails; convert to `io::Error::other`. |
| Memory pressure mid-window | n/a | Once `mlock` returns, pages stay resident. No mid-window failure mode. | None. |
| Signal interruption | n/a | `mlock` is not interruptible on Linux (kernel suppresses `EINTR` for memlock). | None. |
| Truncation race | n/a | `truncate(2)` shrinking the file beneath the wired range still triggers in-kernel `SIGBUS`. Wiring does not protect against this. | Out of scope; `BasisWriterKind::BufferedMap` (mitigation 4) is the load-bearing defence. |

`is_downgrade_errno(&io::Error)` returns `true` for `EAGAIN`,
`ENOMEM`, and `EPERM`; `false` for `EINVAL` (programmer bug). This
maps cleanly onto the existing `SQPOLL_FALLBACK` flag at
`crates/fast_io/src/io_uring/config.rs:357`.

### Test plan

The candidate supports a *deterministic* race-closure proof. This is
the axis SQM-2.a flagged as decisive against candidate 1.

1. Unit test in `crates/fast_io/src/io_uring/wired_window.rs`:
   - Wire a 4 KiB range from a `tempfile`-backed mmap. Call
     `mincore(addr, len, vec)` and assert every page in the vec is
     `0x01` (resident) post-`pin`.
   - Drop the `WiredWindow`. Call `mincore` again. Note: pages may
     stay resident after `munlock` (the kernel may keep them); the
     unit test asserts that the *wired* flag in
     `/proc/self/pagemap` (bit 56) is cleared, which is the
     load-bearing post-condition.
   - Negative case: lower `RLIMIT_MEMLOCK` to 4 KiB via
     `setrlimit(RLIMIT_MEMLOCK, {4096, 4096})`, attempt to wire 1 MiB,
     assert the returned `io::Error` has `raw_os_error() == EAGAIN`.
2. Integration test in `crates/fast_io/tests/repro_sqpoll_mmap.rs`:
   - Add `repro_sqpoll_mmap_with_mlock` variant that wires the basis
     window before each `READ_FIXED` SQE and unwires after the CQE.
   - On every kernel-version row in
     `docs/design/sqpoll-mmap-race-symptoms.md` section 3, assert
     `status=ok` for all 16 iterations. Unlike candidate 1, a passing
     run is a *proof* the race is closed, because the kthread cannot
     fault on wired memory.
3. Negative downgrade test in `repro_sqpoll_mmap_with_mlock_pressure`:
   - Run under `prlimit --memlock=65536` and assert the wrapper
     downgrades to the regular ring without erroring the transfer.
     Confirm via `SQPOLL_FALLBACK.load()` returning `true`.
4. Stress test under cgroup memory pressure: `cgcreate -g
   memory:sqm-1c-stress && echo 16M >
   /sys/fs/cgroup/memory/sqm-1c-stress/memory.high`. The wired path
   must still succeed (memory.high does not prevent mlock; the kernel
   accounts wired pages against the cgroup but does not refuse the
   pin). Asserts the mlock backstop holds where the WILLNEED hint
   from candidate 1 fails.
5. Throughput regression test against
   `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`:
   - Compare `mlock`-wrapped SQPOLL ring vs status-quo defensive
     disable on NVMe. Per SQM-2.a's NVMe-perf axis, the wrapped path
     should recover the 10-15% loss documented in
     `project_sqpoll_disabled_with_mmap.md`.

### Rollback story

Multi-file revert:

1. Delete the `WiredWindow::pin / drop` call sites in
   `registered_buffers/submit.rs`.
2. Delete `MmapBasisFlagGuard` from `io_uring_common.rs`.
3. Delete `wired_window.rs` and its `#[allow(unsafe_code)]` attribute.
4. Leave `IoUringConfig::mmap_basis_active` and the `build_ring`
   defensive disable in place; they revert cleanly to the candidate 3
   baseline.

Rollback degrades to candidate 3, *not* to a broken state. The
defensive disable is the unconditional fallback regardless of which
candidate is live, so any mid-rollback build is correct (just slower).

A sub-rollback option: keep `wired_window.rs` but flip the call site
behind a Cargo feature `iouring-mlock-basis` (default off). This
allows operators to opt out per build without source revert. SQM-2.b
should make the feature-gate vs unconditional call.

### Interaction with SMR-3c per-file dispatch

Candidate 2 *composes* with SMR-3c, with one new invariant:

- SMR-3c's `pick` returns `BasisReadBackend::IoUring` for large files
  (per `DEFAULT_SIZE_THRESHOLD_BYTES`). When SQPOLL is requested and
  the chosen backend is io_uring with an mmap'd basis still live (for
  the smaller files), the wrapper wires the io_uring read window
  before submission.
- `mmap_basis_active` becomes a *transient* flag: `true` outside the
  wired window, `false` inside. `build_ring` reads the flag only at
  ring construction time; the per-SQE wrapper does not race with
  `build_ring` because rings are constructed before the EWMA
  collects samples.
- The SMR-3c EWMA at
  `crates/fast_io/src/adaptive_dispatch.rs:117-156` records
  per-backend throughput. With candidate 2 in place, the io_uring
  EWMA reflects the wired-window cost (one extra `mlock` + one
  `munlock` per slide). For 4 MiB slides on NVMe, that overhead is
  ~5 microseconds per slide (measured under SQM-2.a appendix);
  negligible vs the ~500 microsecond slide service time.

No change to `adaptive_dispatch::pick`; no change to the
`OC_RSYNC_ADAPTIVE_BASIS_DISPATCH` env var contract.

### Cross-platform behaviour

| Platform | Wiring primitive | Notes |
|---|---|---|
| Linux >= 4.4 | `mlock2(addr, len, MLOCK_ONFAULT)` | Defers fault-in to first touch; preferred. |
| Linux < 4.4 | `mlock(addr, len)` | Faults every page synchronously. Higher up-front cost; identical correctness. |
| macOS | `mlock(addr, len)` | No `MLOCK_ONFAULT`. Cross-platform wrapper falls through to `mlock`. SQPOLL is Linux-only so the wrapper is dead code in practice; the cross-platform path exists only because `wired_window.rs` is compiled on every Unix to keep the unsafe surface uniform. |
| FreeBSD | `mlock(addr, len)` | FreeBSD has `mlock2` but with different flag semantics; the wrapper treats FreeBSD as a `mlock`-only platform to avoid divergence. |
| Windows | `VirtualLock(addr, len)` | Direct analogue; pins the range in working set. The wrapper would route through `windows-rs` from `microsoft/windows-rs`. SQPOLL is Linux-only so this path is also dead code under SQM; documented for completeness in case any future Windows ring shape needs it. |
| Other (illumos, AIX, etc.) | `mlock` POSIX path | Untested but POSIX-compliant; cross-compilation only. |

Compilation:

```rust,ignore
#[cfg(all(target_os = "linux", feature = "io_uring"))]
mod wired_window; // active path

#[cfg(all(unix, not(target_os = "linux")))]
mod wired_window_stub; // POSIX `mlock` fallback for unsafe-surface uniformity

#[cfg(windows)]
mod wired_window_stub; // no-op; SQPOLL is Linux-only
```

The non-Linux paths exist only so `fast_io` keeps a single
`#[allow(unsafe_code)]` site for the wiring primitive across all
targets, matching the unsafe-code policy directive that `fast_io` is
the consolidation crate for unsafe code.

### Operator guidance

`RLIMIT_MEMLOCK` defaults are too small on most distributions for any
realistic basis window. The recommended operator change:

```text
# /etc/security/limits.d/90-oc-rsync.conf
oc-rsync   soft   memlock   268435456    # 256 MiB
oc-rsync   hard   memlock   268435456
```

When this is not configured, the wrapper transparently downgrades via
the `SQPOLL_FALLBACK` path; the operator sees a one-time
`debug_log!(Io, 1, "mlock downgrade: RLIMIT_MEMLOCK too small, falling
back to regular ring")` line. SQM-2.b must decide whether this log
escalates to `Warn` on the first occurrence (recommended: yes, since
it indicates a config gap, not a transient).

## Candidate 3: per-basis dispatch (status quo / fallback)

### Mechanism summary

The defensive disable currently in production. When
`mmap_basis_active` is `true`, `IoUringConfig::build_ring` refuses
SQPOLL and constructs a regular ring. The hazard is closed by
avoiding the combination entirely. Promotion to "official" means
closing SMR follow-up tasks SMR-3a/3b/3c with "no change", removing
the bench-conditional ambiguity from the SMR catalogue, and updating
`project_sqpoll_disabled_with_mmap.md` to mark the ~10-15% NVMe loss
as accepted.

### Wrapper pseudo-code

Already in production. Reproduced here for completeness:

```rust,ignore
// crates/fast_io/src/io_uring/config.rs:346-373
pub(crate) fn build_ring(&self) -> io::Result<RawIoUring> {
    let sqpoll_requested = self.sqpoll;
    let sqpoll_safe = sqpoll_requested && !self.mmap_basis_active;
    if sqpoll_requested && !sqpoll_safe {
        debug_log!(Io, 1,
            "io_uring: refusing SQPOLL because an mmap basis reader is \
             active on this transfer plan (...); falling back to a \
             regular ring");
        SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
    }
    if sqpoll_safe {
        let mut builder = io_uring::IoUring::builder();
        builder.setup_sqpoll(self.sqpoll_idle_ms);
        match builder.build(self.sq_entries) {
            Ok(ring) => return Ok(ring),
            Err(_) => SQPOLL_FALLBACK.store(true, Ordering::Relaxed),
        }
    }
    RawIoUring::new(self.sq_entries)
        .map_err(|e| io::Error::other(format!("io_uring init failed: {e}")))
}
```

The "promotion" delta is zero LoC in this file. The delta is in
*docs*: an ADR-style note in `docs/design/mmap-vs-sqpoll-decision.md`
recording that candidate 3 is the official long-term answer rather
than a stop-gap.

### Exact syscall sequence and flags

Per ring construction:

1. `io_uring_setup(sq_entries, &params)` *without* `IORING_SETUP_SQPOLL`
   when the mmap flag is set.
   - `params.flags &= !IORING_SETUP_SQPOLL`.
2. If the original request set `IORING_SETUP_SQ_AFF` (CPU affinity for
   the kthread), that flag is dropped together with `SQPOLL` - the
   `io_uring` crate handles this; the wrapper does not see it.

Per SQE submission: standard `io_uring_enter(...)` path. No extra
syscalls vs the SQPOLL-on baseline except the mandatory
`io_uring_enter` per batch that SQPOLL would have avoided.

### Failure modes by `errno`

| Source | `errno` | What happens | Propagation |
|---|---|---|---|
| `io_uring_setup` (non-SQPOLL) | `ENOMEM` | Kernel cannot allocate ring memory. | Surfaced as `io::Error::other` from `RawIoUring::new`. |
| `io_uring_setup` (non-SQPOLL) | `EINVAL` | `sq_entries` invalid. Indicates config bug. | Surfaced. |
| `io_uring_setup` (non-SQPOLL) | `ENOSYS` | Kernel < 5.1. Should not reach here; probed at `crates/fast_io/src/io_uring_probe.rs`. | Surfaced. |
| Latent perf loss | n/a | SQPOLL never lit when mmap basis is in play. ~10-15% NVMe throughput regression. | Not a runtime failure. Captured by SMR-1 bench. |

No new failure modes vs the in-production code.

### Test plan

Already in production. The candidate is covered by:

1. `build_ring_sqpoll_with_mmap_basis_disables_sqpoll`
   (`crates/fast_io/src/io_uring/config.rs:779-810`) - asserts SQPOLL
   is refused when `mmap_basis_active = true`.
2. `build_ring_no_sqpoll_with_mmap_basis_no_warning`
   (same module) - asserts no spurious `debug_log` when SQPOLL is not
   requested.
3. `repro_sqpoll_mmap.rs` - exercises the gate under the hazardous
   combination; expected outcome `status=ok` because the ring is not
   SQPOLL.
4. SMR-3c unit tests in `crates/fast_io/src/adaptive_dispatch.rs` -
   cover the per-file dispatch under the same defensive disable.

Promotion to official adds:

5. An ADR test asserting the `SQPOLL_FALLBACK` flag flips `true` on
   the disable path - already covered by the existing
   `build_ring_sqpoll_with_mmap_basis_disables_sqpoll` test; no new
   code.
6. A documentation test asserting `project_sqpoll_disabled_with_mmap.md`
   exists and references the perf-loss number, so a future deletion
   of the doc fails CI. Lives in `xtask` as a glob check.

### Rollback story

None needed. This is the baseline. The only "rollback" is to
candidates 1 or 2, both of which compose with this candidate (they
keep the defensive disable as their fallback). Promotion is reversible
by reverting the doc change in `mmap-vs-sqpoll-decision.md`; the code
stays unchanged.

The SMR-3c adaptive layer can independently be turned off via
`OC_RSYNC_ADAPTIVE_BASIS_DISPATCH=0` or by disabling the
`adaptive-basis-dispatch` Cargo feature (default on, per
`crates/fast_io/Cargo.toml`).

### Interaction with SMR-3c per-file dispatch

Candidate 3 is the *substrate* SMR-3c runs on. SMR-3c picks per-file
backend; candidate 3 ensures that when the picked backend is mmap and
the ring would also have been SQPOLL, SQPOLL is silently dropped. The
SMR-3c EWMA observes the non-SQPOLL throughput for the io_uring arm,
so the dispatcher's pick reflects the real (degraded) cost.

The composition is:

```text
SMR-3c::pick -> BasisReadBackend
  if BasisReadBackend::Mmap:
      use MmapReader (no SQPOLL involvement)
  if BasisReadBackend::IoUring:
      construct ring via IoUringConfig::build_ring
          if mmap_basis_active: SQPOLL refused (candidate 3 trip)
          else: SQPOLL granted if requested
```

### Cross-platform behaviour

| Platform | Behaviour | Notes |
|---|---|---|
| Linux | Defensive disable active. | `crates/fast_io/src/io_uring/config.rs:346-373`. |
| macOS | io_uring unavailable; SQPOLL surface absent. | `IoUringProbeResult::Unsupported`. |
| Windows | io_uring unavailable; IOCP is the alternate path with separate hazards (`docs/design/iocp/`). | `project_no_windows_io_uring.md`. |
| FreeBSD / NetBSD / OpenBSD | io_uring unavailable. | Stub at `crates/fast_io/src/io_uring_stub.rs`. |

The status-quo wrapper is `#[cfg(all(target_os = "linux", feature =
"io_uring"))]`. Non-Linux platforms inherit the stub and never hit
this path.

## Composition contract

All three candidates compose with the *same* in-production defensive
disable. The composition is layered:

```text
Layer 0 (mandatory): BasisWriterKind::BufferedMap for io_uring writers
    -> closes truncate-SIGBUS regardless of SQM candidate.

Layer 1 (mandatory): IoUringConfig::build_ring defensive disable
    -> falls back to non-SQPOLL when mmap_basis_active = true.

Layer 2 (SQM candidate):
    1 (MADV_WILLNEED): hint before each SQE on the mmap window.
    2 (mlock):        wire the window, transiently clear the flag,
                      submit, unwire.
    3 (status quo):   no extra layer; layer 1 alone is the answer.

Layer 3 (SMR-3c, optional): per-file EWMA dispatch between mmap and
    io_uring backends. Runs above the SQM layer; sees whichever
    candidate is in force as part of the io_uring arm's observed
    throughput.
```

SQM-2.b's job is to lock in layer 2's selection (per SQM-2.a:
candidate 2) and to specify the dispatch-site shape inside layer 2.
Layers 0, 1, and 3 are not re-litigated.

## Cross-candidate decision matrix

The matrix below condenses the three specifications above onto one
page. Cells are *qualitative* (the quantitative axes live in
SQM-2.a's scoring table).

| Question | Candidate 1 | Candidate 2 | Candidate 3 |
|---|---|---|---|
| New `unsafe` code? | None | One site in `wired_window.rs` (allowed in `fast_io`) | None |
| Deterministic race-closure test? | No (best-effort hint) | Yes (`mincore` post-pin) | Yes (gate-test on hazardous combo) |
| Perf vs SQPOLL-on baseline | 60-85% (provisional) | ~100% (per slide overhead < 1%) | 0% (SQPOLL never lit) |
| Rollback complexity | One-site revert | Multi-file revert with feature-gate option | None (baseline) |
| Downgrade on failure | None (silent re-open) | `SQPOLL_FALLBACK` reused | n/a |
| Cross-platform unsafe surface | Zero | One wrapper, Linux-only active path | Zero |
| Operator config required | None | `RLIMIT_MEMLOCK` bump | None |
| Composes with SMR-3c | Additive | Additive (transient flag) | Substrate |

## What SQM-2.b inherits from this document

SQM-2.b should synthesise and *decide*; SQM-2.b does not need to
re-research. The decision inputs are:

1. **Pseudo-code shape.** Use the `WiredWindow` / `WillNeedHint`
   layouts above as the starting outline. SQM-2.b may rename or
   relocate, but the syscall sequences and RAII boundaries are
   load-bearing and should not change without a re-spec.
2. **Failure-mode tables.** The `errno` -> downgrade mapping is
   contract: SQM-2.b should not invent new errno classifications.
3. **Test plan.** SQM-2.b should pick which of the listed tests are
   load-bearing for the implementation PR vs which can land later as
   follow-ups. The deterministic `mincore` test for candidate 2 is
   load-bearing for shipping; the throughput regression test is
   nice-to-have.
4. **Rollback story.** SQM-2.b should make the Cargo-feature-gate
   call (default-on vs default-off) for candidate 2 explicitly.
5. **SMR-3c composition.** No change to `pick`; SQM-2.b should
   confirm this in its decision-recording section.
6. **Cross-platform stubs.** SQM-2.b should commit to the
   `#[cfg(...)]` shape above so the unsafe surface stays uniform.

## Non-goals

- This doc does not change source. No code, no Cargo feature, no
  config knob is touched.
- This doc does not decide between `mlock` and `mlock2`; the wrapper
  pseudo-code keeps both paths and selects at runtime. SQM-2.b may
  collapse the selection if it has new data.
- This doc does not enable SQPOLL on any production preset. The
  current presets all ship `sqpoll: false` (per
  `docs/audits/io_uring_sqpoll_mmap_pagefault.md` section D); flipping
  the receiver preset is SMR step 5, gated on candidate 2 landing.
- This doc does not redo SQM-2.a's scoring. The recommendation
  (candidate 2 primary, candidate 3 fallback) stands; this doc only
  specifies what each candidate would *look like* if implemented.

## References

- `crates/fast_io/src/io_uring/config.rs:325-373` - defensive disable
  site; the composition substrate for all three candidates.
- `crates/fast_io/src/io_uring_common.rs:106-114` -
  `mmap_basis_active` flag.
- `crates/fast_io/src/mmap_reader.rs:139-143` - existing
  `advise_willneed` hook reused by candidate 1.
- `crates/fast_io/src/mmap_reader_stub.rs:71-87` - non-Unix no-op
  stub.
- `crates/fast_io/src/adaptive_dispatch.rs:209-246` - SMR-3c
  `pick` API that all three candidates compose under.
- `crates/fast_io/src/kernel_version.rs::KernelVersion::meets_minimum`
  - runtime gate for `mlock2(MLOCK_ONFAULT)`.
- `crates/fast_io/src/io_uring/registered_buffers/registry.rs:251-315`
  - RAII pattern candidate 2's `WiredWindow` mirrors.
- `crates/fast_io/tests/repro_sqpoll_mmap.rs` - SQM-1.a reproducer
  and the test seam for all three candidates' integration tests.
- `crates/transfer/src/delta_apply/applicator.rs:65-184` -
  `BasisWriterKind` selector; the load-bearing layer 0 mitigation.
- `docs/design/sqpoll-mmap-race-symptoms.md` - SQM-1.b symptom doc
  and kernel-version coverage matrix.
- `docs/design/sqm-2a-workaround-scoring.md` - scoring matrix that
  selected candidate 2.
- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` - SMR Options
  1/2/3 catalogue (the dispatch alternatives this doc layers under).
- `docs/design/mmap-vs-sqpoll-decision.md` - SMR-2 decision framework
  that picked SMR Option 3.
- `docs/design/basis-file-io-policy.md` - layer 0 invariant
  (`BufferedMap` for io_uring writers).
- `docs/audits/io_uring_sqpoll_mmap_pagefault.md` - long-form audit
  of the page-fault hazard and the three kernel-side outcomes.
- `docs/audits/madvise-willneed-prefault.md` - candidate 1's
  underlying audit; explicitly best-effort.
- `docs/audits/iouring-sqpoll-bench-plan.md` - bench harness that
  produces the NVMe-perf numbers cited in SQM-2.a.
