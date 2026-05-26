# IUR-4.a: 100K-submission stress harness for per-thread io_uring rings

Tracking task: **IUR-4.a**. Predecessor design notes:

- `docs/design/io-uring-shared-ring-audit.md` - IUR-1 caller-surface audit.
- `docs/design/iur-2-per-thread-rings.md` - IUR-2 hybrid layout; section 6
  defines the acceptance grid this harness validates.
- `docs/design/iur-3f-shared-rings-decision.md` - IUR-3.f shared-ring
  exclusion record.
- IUR-3.a (`per_thread_ring.rs`) - the `with_ring` / `PerThreadRing`
  primitive under test.
- IUR-3.e (`bgid_lease.rs`) - the per-thread `BgidLease` whose leak-freedom
  the harness asserts.

This is a design-only document. IUR-4.a implements the harness described
below as an integration test in `crates/fast_io/tests/`.

## 1. Goals

Validate the per-thread io_uring ring topology (IUR-3.a..e) under
sustained high-volume load that exercises:

1. **Lock-freedom** - 100K total submissions across N threads must
   complete in time proportional to the per-thread workload, not the
   total. Super-linear slowdown indicates a hidden shared lock.
2. **Correctness** - every submitted SQE produces exactly one matching
   CQE. No lost completions, no duplicates, no cross-thread CQE
   aliasing.
3. **BGID leak-freedom** - the per-thread `BgidLease` returns every
   cached id to the central pool on thread exit. Post-harness pool
   balance must match the pre-harness baseline.
4. **Ring isolation** - distinct threads own distinct ring fds. No
   thread observes a foreign ring's SQE or CQE.

These are the preconditions for the IUR-5 bench (throughput measurement)
and IUR-6 (default-on decision). The harness does not measure
performance - it asserts functional correctness at scale.

## 2. Harness architecture

### 2.1 Thread count sweep

The harness runs the same workload at five thread counts:

| Threads | SQEs per thread | Total SQEs | Purpose |
|---------|----------------|------------|---------|
| 1 | 100,000 | 100,000 | Single-thread baseline; confirms no TLS overhead regression |
| 2 | 50,000 | 100,000 | Minimal concurrency; catches init-order races |
| 4 | 25,000 | 100,000 | Typical rayon worker count on CI runners |
| 8 | 12,500 | 100,000 | Matches the IUR-2 section 6.2 throughput grid |
| 16 | 6,250 | 100,000 | Maximum per-thread ring fan-out before pinned-page pressure |

The total is held constant at 100K so wall-clock comparisons across
thread counts are meaningful. Each thread calls
`per_thread_ring::with_ring` for every submission, exercising the lazy
init path on the first call and the TLS fast path on subsequent calls.

### 2.2 Barrier synchronisation

All worker threads synchronise on a `std::sync::Barrier` before their
first submission. This forces every thread's `with_ring` call to race
against siblings on the same `io_uring_setup(2)` kernel path,
maximising the window for init-order bugs. A second exit barrier
(`workers + 1`) holds every thread alive until the parent has collected
ring fds and completion counts - preventing fd recycling that would
mask ring-isolation violations (same pattern as
`per_thread_ring::tests::four_threads_get_independent_rings`).

### 2.3 Test function organisation

The harness is a single integration test file
`crates/fast_io/tests/iouring_per_thread_stress.rs`:

- `#![cfg(all(target_os = "linux", feature = "io_uring"))]` at the
  crate level.
- Gated behind `OC_RSYNC_IOURING_STRESS=1` so normal CI (`cargo nextest
  run --workspace`) is not inflated by the multi-second run. The CI
  workflow for IUR validation sets the variable explicitly.
- One `#[test]` function per thread count
  (`stress_1_thread`, `stress_2_threads`, ..., `stress_16_threads`) so
  each cell in the acceptance grid produces an independent pass/fail in
  nextest output.
- A shared helper `run_stress(threads: usize, ops_per_thread: usize)`
  encapsulates the barrier setup, worker spawn, and post-join
  assertions.

## 3. Operation mix

### 3.1 Opcode selection

The harness uses four io_uring opcodes weighted to reflect the real
receiver write path:

| Opcode | Weight | Rationale |
|--------|--------|-----------|
| `IORING_OP_NOP` | 10% | Cheapest round-trip; isolates ring lifecycle cost from kernel I/O |
| `IORING_OP_WRITE` | 50% | Dominant opcode on the receiver write path (`file_writer.rs`) |
| `IORING_OP_READ` | 30% | Basis-file reads on the sender / generator path (`file_reader.rs`) |
| `IORING_OP_STATX` | 10% | Metadata path; exercises the statx SQE builder (`statx.rs`) |

Each thread selects the opcode for submission `i` deterministically
via `i % 10` mapped to the weight table, so the mix is reproducible
across runs without randomness. The `WRITE` and `READ` opcodes target
per-thread scratch files in a `tempfile::TempDir`; `STATX` targets the
same files. This avoids cross-thread file contention.

### 3.2 Payload and file setup

- Each thread creates its own scratch directory under the shared
  `TempDir`: `<tmpdir>/thread-<id>/`.
- Each thread pre-creates 100 files of 4 KiB each, filled with a
  deterministic pattern (`0xA5`). The 100-file pool is cycled
  round-robin across the thread's submission count so the bench
  exercises fd reuse without inflating the file count to 100K.
- `WRITE` submissions overwrite the current file's first 4 KiB.
  `READ` submissions read the first 4 KiB into a per-thread buffer.
  `STATX` submissions query `stx_size` of the current file.
- All file I/O uses raw fds obtained from `File::as_raw_fd()`. Files
  are opened once at thread startup and closed after the exit barrier.

### 3.3 Submission and reap pattern

Each submission follows the same-thread submit-and-reap pattern
established by `per_thread_ring::tests::submit_nop_and_reap`:

```
with_ring(|ring| {
    // 1. Build SQE with a thread-unique user_data tag.
    // 2. Push SQE onto the submission queue.
    // 3. submit_and_wait(1) - one SQE at a time for correctness.
    // 4. Drain the CQE and assert user_data matches.
    // 5. Assert CQE result >= 0 (no kernel error).
})
```

The `user_data` tag encodes `(thread_id << 32) | sequence_number` so
any cross-thread CQE aliasing is detectable. Submitting one SQE at a
time (not batched) maximises the number of `io_uring_enter(2)` syscalls
and therefore the stress on the per-thread ring lifecycle.

A follow-up variant (IUR-4.c, not in scope here) will test batched
submission (push N SQEs, `submit_and_wait(N)`) to exercise SQ-full
backpressure.

## 4. Metrics and assertions

### 4.1 Per-thread counters

Each worker thread maintains local counters (no atomic sharing on the
hot path):

| Counter | Type | Assertion |
|---------|------|-----------|
| `submitted` | `usize` | Must equal `ops_per_thread` at join |
| `completed` | `usize` | Must equal `submitted` (no lost completions) |
| `nop_count` | `usize` | Must equal 10% of `ops_per_thread` (weight check) |
| `write_count` | `usize` | Must equal 50% of `ops_per_thread` |
| `read_count` | `usize` | Must equal 30% of `ops_per_thread` |
| `statx_count` | `usize` | Must equal 10% of `ops_per_thread` |
| `ring_fd` | `RawFd` | Captured on first `with_ring` call; must be stable across all calls |

### 4.2 Parent-side assertions

After all workers join through the exit barrier:

1. **Completion count** - sum of all workers' `completed` counters
   must equal 100,000.
2. **Ring isolation** - the set of `ring_fd` values collected from all
   workers must have cardinality equal to the thread count (each worker
   owns a distinct ring).
3. **No panics** - every `thread::spawn` handle joins without panic.
4. **CQE result codes** - every CQE result is non-negative. A negative
   result is a kernel error that indicates a ring-lifecycle or
   fd-management bug in the per-thread path.

### 4.3 BGID pool balance

Before spawning workers, the harness snapshots
`BgidAllocator::remaining()`. After all workers have joined and their
TLS destructors have run (including `BgidLease::drop`), the harness
asserts `BgidAllocator::remaining() >= pre_snapshot`. Strict equality
is not required because other tests may have allocated bgids
concurrently; the invariant is that no bgids leaked during the stress
run.

This check is gated on `BgidLease` being exercised. The harness
optionally acquires a `BgidLease` on each thread via
`with_thread_lease` (IUR-3.e) and takes one bgid per 1000 submissions
to exercise the lease cache drain/refill path. Every taken bgid is
returned via `BgidAllocator::deallocate` before the worker exits.

### 4.4 Wall-clock guard

Each test function sets a generous wall-clock timeout (60 seconds for
the 100K-SQE workload). If a hidden lock causes super-linear slowdown,
the test times out rather than hanging CI indefinitely. The timeout is
enforced via nextest's `slow-timeout` configuration in
`.config/nextest.toml`, not via in-test `sleep` loops.

## 5. Correctness properties

### 5.1 No lost completions

Every SQE pushed to the per-thread ring's submission queue must produce
exactly one CQE on the same ring's completion queue. The harness
asserts `completed == submitted` per thread. A mismatch indicates
either a kernel bug (unlikely at this scale), a ring-lifecycle error
(ring dropped before reap), or a cross-thread SQ/CQ aliasing bug.

### 5.2 No cross-thread CQE aliasing

The `user_data` tag is `(thread_id << 32) | seq`. A CQE whose
`user_data` does not decode to the current thread's id is a
ring-isolation violation - the thread is reading completions from a
sibling's ring. This is the primary invariant the per-thread topology
guarantees; the harness makes it an explicit assertion on every reap.

### 5.3 No BGID leaks

The `BgidLease` TLS destructor must return every cached bgid to the
central pool. The post-join pool-balance check (section 4.3) catches
leaks. A leak indicates either the `BgidLease::Drop` path was not
reached (TLS destructor ordering issue) or the `deallocate_batch`
path lost ids.

### 5.4 No fd leaks

Each per-thread ring claims one `io_uring` fd. After all workers join
and their TLS destructors run, the process fd count should return to
the pre-spawn baseline. The harness does not assert this directly
(fd-counting is platform-fragile), but the `four_threads_get_independent_rings`
exit-barrier pattern in `per_thread_ring.rs` ensures all rings are live
concurrently and dropped on join. The stress harness inherits that
pattern.

## 6. Integration with existing test infrastructure

### 6.1 Feature gating

The test file is `cfg`-gated on `target_os = "linux"` and
`feature = "io_uring"`. On non-Linux platforms and builds without the
`io_uring` feature, the file compiles to nothing. This matches the
existing pattern in `crates/fast_io/tests/io_uring_byte_identical.rs`
and `crates/fast_io/tests/io_uring_shared_ring.rs`.

### 6.2 io_uring availability skip

Each test function checks `is_io_uring_available()` at entry and
returns with an `eprintln!` skip message when the kernel rejects
`io_uring_setup(2)`. This mirrors the `io_uring_unavailable()` helper
in `per_thread_ring::tests` and keeps the harness green on musl CI
runners and seccomp-locked containers.

### 6.3 Tempdir fixtures

All scratch files live under `tempfile::TempDir`. The dir is created
by the parent thread and shared via `Arc<PathBuf>` with workers. Each
worker creates its own subdirectory to avoid cross-thread file
contention. The `TempDir` guard is dropped after all assertions,
cleaning up the scratch data.

### 6.4 Env-var gate

The stress harness is opt-in via `OC_RSYNC_IOURING_STRESS=1`, matching
the `OC_RSYNC_IOCP_STRESS` pattern in
`crates/fast_io/tests/iocp_high_concurrency_stress.rs`. Normal CI runs
skip the test with a clear message. The IUR-4 validation CI job sets
the variable explicitly.

### 6.5 Nextest configuration

Add a `[profile.default.overrides]` entry in `.config/nextest.toml`
for `iouring_per_thread_stress::*` tests with `slow-timeout` set to
120 seconds (2 minutes). This prevents CI from killing the test
prematurely on loaded runners while still catching genuine hangs.

## 7. Thread sanitizer and miri (IUR-4.b scope)

### 7.1 Thread sanitizer (tsan)

The per-thread ring topology has three concurrency surfaces that tsan
should instrument:

1. **`BgidAllocator` central pool mutex** -
   `allocator.rs:99-101` (`bgid_free_list()`). Multiple threads lease
   and return bgids concurrently. tsan should confirm no data races on
   the `Vec<u16>` free-list.
2. **`NEXT_POOL_ID` atomic counter** - `session_pool.rs:308`. Relaxed
   ordering is correct for identity assignment (no dependent loads) but
   tsan should confirm no torn reads.
3. **`THREAD_RING_FALLBACK_COUNT` atomic counter** - the IUR-2 section
   5.2 metrics counter. Relaxed ordering; tsan confirms no torn
   increments.

Run the stress harness under tsan:

```sh
RUSTFLAGS="-Z sanitizer=thread" \
  OC_RSYNC_IOURING_STRESS=1 \
  cargo +nightly nextest run \
    -p fast_io \
    --all-features \
    --target x86_64-unknown-linux-gnu \
    -E 'test(iouring_per_thread_stress)' \
    --color never
```

tsan instruments every atomic and mutex operation. The harness's 100K
submissions across 16 threads should surface any data race within the
per-thread ring init, bgid lease, and fallback counter paths.

### 7.2 Miri

Miri cannot execute `io_uring_setup(2)` syscalls. The harness's
io_uring-availability skip gate (section 6.2) causes miri runs to
bail out cleanly. Miri coverage for the per-thread ring topology is
limited to the pure-Rust logic:

- `BgidLease` allocate / take / refill / drop (no syscalls).
- `ThreadLocalRingPool` slot management (no ring construction).
- `PerThreadRing::new` error path (returns `Err` under miri because
  `io_uring_setup` is unavailable).

A dedicated miri test module in `bgid_lease.rs` (IUR-4.b scope) can
exercise the lease cache logic with a mock allocator that replaces the
global `BgidAllocator`. This is out of scope for IUR-4.a.

## 8. File layout

```
crates/fast_io/tests/
  iouring_per_thread_stress.rs    <-- IUR-4.a (this spec)
```

The file follows the naming convention of existing io_uring integration
tests (`io_uring_byte_identical.rs`, `io_uring_shared_ring.rs`,
`io_uring_mmap_pressure.rs`).

## 9. Acceptance criteria

The harness is complete when:

1. All five thread-count cells (1/2/4/8/16) pass on a Linux 5.6+ host
   with `OC_RSYNC_IOURING_STRESS=1`.
2. Every per-thread counter assertion (section 4.1) holds.
3. Ring isolation (section 5.2) is confirmed at every thread count.
4. BGID pool balance (section 4.3) holds after the 16-thread run.
5. The harness skips cleanly on non-Linux, musl, and seccomp-blocked
   hosts.
6. Normal CI (`cargo nextest run --workspace`) does not execute the
   stress tests (env-var gate).

IUR-4.b (tsan/miri) and IUR-4.c (batched submission) are follow-up
tasks that build on this harness.

## 10. Cross-references

- `crates/fast_io/src/io_uring/per_thread_ring.rs` - the `with_ring`
  primitive under test (IUR-3.a).
- `crates/fast_io/src/io_uring/bgid_lease.rs` - the `BgidLease` /
  `with_thread_lease` primitive whose leak-freedom is asserted
  (IUR-3.e).
- `crates/fast_io/src/io_uring/session_pool.rs` - the
  `ThreadLocalRingPool` and `SessionRingPool` primitives the harness
  exercises alongside the lower-level `with_ring` (IUR-3.a).
- `crates/fast_io/tests/iocp_high_concurrency_stress.rs` - the IOCP
  stress test whose env-var gate pattern this harness follows.
- `crates/fast_io/benches/iouring_per_file_vs_shared.rs` - the
  per-file-vs-shared bench that IUR-5 extends with a `per_thread` row.
- `docs/design/iur-2-per-thread-rings.md` section 6 - the acceptance
  grid whose functional correctness dimension this harness validates.
