# SMR-4 regression strategy: SQPOLL stays enabled for large delta basis reads

Tracking: oc-rsync task #2291. Sibling documents already in tree:

- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` (SMR-2 #2287) - decision
  framework that selected the size-threshold heuristic (option 2) for the
  basis-read dispatch.
- `docs/design/basis-file-io-policy.md` (#1666) - selector rule that forbids
  `MmapStrategy` whenever an io_uring writer is active on the same plan.
- `docs/audits/io-uring-sqpoll-mmap-interaction.md` - re-verification of the
  SQPOLL + mmap hazard that motivates the dispatch redesign.
- `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs` (SMR-5) - bench
  scaffold whose results drove the SMR-2 recommendation.

## Why this document exists instead of the test

SMR-4 was scoped to land a regression test asserting that a future refactor
cannot silently disable SQPOLL on large-file basis reads. The test would:

1. Build a 4 MiB tempfile with deterministic bytes.
2. Open it through the basis-read dispatch the receiver wires for delta
   apply.
3. Inspect whether dispatch routed through the SQPOLL + `READ_FIXED` path
   or fell back to the mmap path.
4. Assert the SQPOLL path for the >= threshold case, and the mmap path
   for the < threshold case.

Step 3 needs a public observation surface: either a counter that increments
each time the dispatch picks the SQPOLL path, or an accessor returning the
chosen strategy for a given file. Neither exists on master today, and
SMR-3b (the dispatch implementation) is still in flight. Adding the
dispatch glue plus the observation surface inside SMR-4 would balloon the
change well past the 20-line budget the task set and would conflict with
SMR-3b's wiring.

Per the SMR-4 constraint, this document captures what the test needs and
which counters to add, so SMR-3b can land both the dispatch and the
counters together and SMR-4 can become a pure test-only follow-up.

## Required observation surfaces

The test cannot use `sqpoll_fell_back()` alone: that flag flips for ring
construction failures unrelated to the basis-read dispatch (e.g.
`CAP_SYS_NICE` denial during an unrelated ring). The test needs counters
that count only the basis-read dispatch decision.

### Counter 1: `iouring_basis_reads_count`

```rust
// crates/fast_io/src/io_uring/stats.rs (new file, or extend
// crates/fast_io/src/io_uring/registered_buffers/stats.rs)
use std::sync::atomic::{AtomicU64, Ordering};

static IOURING_BASIS_READS: AtomicU64 = AtomicU64::new(0);

#[doc(hidden)]
pub(crate) fn bump_iouring_basis_reads() {
    IOURING_BASIS_READS.fetch_add(1, Ordering::Relaxed);
}

/// Number of basis-file reads that dispatched through the io_uring +
/// SQPOLL + `READ_FIXED` path since process start.
///
/// Incremented once per basis-file open that picks the io_uring path,
/// not once per SQE. Used by `tests/sqpoll_stays_enabled_for_large_delta.rs`
/// to assert the size-threshold heuristic (SMR-2 #2287) routes large
/// files away from mmap.
pub fn iouring_basis_reads_count() -> u64 {
    IOURING_BASIS_READS.load(Ordering::Relaxed)
}
```

### Counter 2: `mmap_basis_reads_count`

Mirror counter for the mmap path. Lets the small-file scenario assert the
mmap counter increments and the io_uring counter does not (and the reverse
for the large-file scenario). Without both counters the test can only
prove one half of the dispatch decision.

```rust
static MMAP_BASIS_READS: AtomicU64 = AtomicU64::new(0);

#[doc(hidden)]
pub(crate) fn bump_mmap_basis_reads() {
    MMAP_BASIS_READS.fetch_add(1, Ordering::Relaxed);
}

/// Number of basis-file reads that dispatched through the mmap path
/// since process start. See [`iouring_basis_reads_count`].
pub fn mmap_basis_reads_count() -> u64 {
    MMAP_BASIS_READS.load(Ordering::Relaxed)
}
```

### Call sites SMR-3b must instrument

- `crates/transfer/src/map_file/adaptive.rs` - call
  `bump_mmap_basis_reads()` from the `Mmap` arm of `open` /
  `open_with_threshold` and `bump_iouring_basis_reads()` from the new
  `IoUringReadFixed` arm SMR-3b adds.
- `crates/transfer/src/delta_apply/applicator.rs:161-176` - the existing
  `open_adaptive_buffered` branch must not bump either counter; it is the
  defensive disable path, not the SQPOLL path.

## Test outline (for the SMR-4 follow-up PR)

File: `crates/fast_io/tests/sqpoll_stays_enabled_for_large_delta.rs`

```rust
#![cfg(all(target_os = "linux", feature = "io_uring"))]

use fast_io::io_uring::is_io_uring_available;
use fast_io::io_uring::stats::{
    iouring_basis_reads_count, mmap_basis_reads_count,
};
// + the transfer-side entry point that runs the dispatch. SMR-3b decides
// whether it lives on AdaptiveMapStrategy or on a new BasisReader type.

#[test]
fn sqpoll_stays_enabled_for_large_delta_basis_reads() {
    if !is_io_uring_available() {
        eprintln!("skipped (io_uring unavailable)");
        return;
    }
    // 1. Snapshot both counters.
    // 2. Write a 4 MiB tempfile with deterministic bytes.
    // 3. Open via the SMR-3b dispatch (large branch).
    // 4. Assert iouring_basis_reads_count incremented by 1,
    //    mmap_basis_reads_count unchanged.
}

#[test]
fn small_basis_files_stay_on_mmap_path() {
    if !is_io_uring_available() {
        eprintln!("skipped (io_uring unavailable)");
        return;
    }
    // 1. Snapshot both counters.
    // 2. Write a 1 KiB tempfile.
    // 3. Open via the SMR-3b dispatch (small branch).
    // 4. Assert mmap_basis_reads_count incremented by 1,
    //    iouring_basis_reads_count unchanged.
}
```

Non-Linux / feature-off targets get a stub:

```rust
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn main() {}
```

Counter snapshots are needed because integration tests in the same binary
share process state; absolute equality would race against parallel tests.

## What this regression guards against

The risk the test must catch is a refactor that silently flips the
dispatch back to mmap for files at or above the SMR-2 threshold. Concrete
regression shapes:

- Future tuning swaps `>=` for `>` in `AdaptiveMapStrategy::open_with_threshold`
  and a 1 MiB file silently drops to mmap.
- A `cfg` cleanup gates the `IoUringReadFixed` variant behind a feature
  that no preset enables, and every basis read silently falls back to
  mmap.
- The `mmap_basis_active` flag is toggled true for a transfer plan that
  no longer carries an mmap, and `build_ring` defensively drops SQPOLL.
- A reorder of `AdaptiveMapStrategy::open*` constructors makes
  `open_buffered` the default for io_uring writers regardless of size.

Each regression manifests as a throughput cliff under benchmarks but is
silent under unit tests today. Counter-based assertions surface the
divergence on the first run after the refactor.

## Dependencies

- SMR-3b must land first. SMR-4 cannot land its test before the dispatch
  exists, because the dispatch is what the test inspects.
- The counters above belong in the SMR-3b PR, not in SMR-4, because they
  are produced by the dispatch code SMR-3b adds. SMR-4 only consumes
  them.

## Out of scope

- This document does not propose a wire-protocol change.
- This document does not propose flipping any preset's default
  `sqpoll` flag. That is SMR-5's job once SMR-3b plus this regression
  test are in place.
- This document does not propose removing the defensive disable in
  `crates/fast_io/src/io_uring/config.rs:343-370`. The disable remains
  load-bearing for non-basis mmap consumers (parallel checksum digest,
  `BufferRing` provided-buffer region).
