# Per-thread ring stress outcome template (IUR-4.c)

Tracking task: **IUR-4.c**. Predecessor design notes:

- `docs/design/iur-4a-100k-submission-stress-harness.md` - IUR-4.a harness
  spec. Defines the 100K-submission workload, thread-count sweep, operation
  mix, and functional correctness assertions.
- `docs/design/iur-2-per-thread-rings.md` - IUR-2 hybrid layout. Section 6
  defines the acceptance grid and section 8 defines the default-on criteria
  this outcome gates.
- `docs/design/shared-ring-removal-plan.md` - IUR-6.b removal plan. Gated
  on IUR-5 bench sign-off, which in turn requires IUR-4 stress pass.
- `docs/design/iur-3f-shared-rings-decision.md` - IUR-3.f shared-ring
  exclusion record.

This document pre-defines the success criteria, expected failure modes,
thresholds, and outcome documentation format for the IUR-4.b stress
validation (tsan + miri on Linux hardware). It is written before IUR-4.b
runs so the pass/fail criteria are locked down before observing results.

## 1. Harness overview

The IUR-4.a stress harness (`crates/fast_io/tests/iouring_per_thread_stress.rs`)
submits 100K SQEs across N per-thread io_uring rings (N = 1, 2, 4, 8, 16).
Each thread owns an independent ring via `per_thread_ring::with_ring`,
submits a deterministic mix of NOP/WRITE/READ/STATX operations against
per-thread scratch files, and reaps every CQE on the same thread.

IUR-4.b runs this harness under two instrumentation tools:

| Tool | Target | What it detects |
|------|--------|-----------------|
| Thread Sanitizer (tsan) | All concurrency surfaces | Data races, use-after-free on shared state, lock-order inversions |
| Miri | Pure-Rust logic only (no syscalls) | Undefined behavior in `BgidLease` cache, slot management, TLS drop ordering |

Both tools run the same 100K-submission workload to maximize coverage of
the concurrent init, submission, reap, and teardown paths.

## 2. Success criteria - Thread Sanitizer

### 2.1 Zero data races

tsan must report **zero** data race warnings across all five thread-count
cells (1/2/4/8/16). Any tsan-reported race is a blocking failure regardless
of whether it manifests as a user-visible bug.

Concurrency surfaces under tsan instrumentation:

| Surface | Location | Expected access pattern |
|---------|----------|------------------------|
| `BgidAllocator` free-list | `buffer_ring/allocator.rs:99-101` | Protected by `Mutex<Vec<u16>>`; no raw access |
| `NEXT_POOL_ID` atomic | `session_pool.rs:308` | `Relaxed` ordering; no dependent loads |
| `THREAD_RING_FALLBACK_COUNT` | per IUR-2 section 5.2 | `Relaxed` increment; no synchronizes-with requirement |
| Per-thread `RefCell<Option<RawIoUring>>` | `session_pool.rs:285-300` | Thread-local; never shared; tsan confirms no cross-thread access |
| `BgidLease` fields (base, used, free_within) | `bgid_lease.rs` | Thread-local; never shared |

### 2.2 No use-after-free on CQE buffers

tsan must detect no use-after-free when:

- A CQE is reaped and its `user_data` is read after the ring might be in
  a logically-invalid state.
- `BgidLease::drop` returns ids to the central pool while another thread
  is allocating from the same pool.
- Thread-local ring storage is accessed during TLS destructor execution
  on thread exit.

### 2.3 No lock-order inversions

tsan's deadlock detector must report no lock-order cycles. The only mutex
in the per-thread ring path is `BgidAllocator::bgid_free_list()`. If
future changes introduce a second lock, tsan will catch AB/BA inversions.

## 3. Success criteria - Miri

### 3.1 No undefined behavior in unsafe ring interaction

Miri cannot execute `io_uring_setup(2)`. The harness bails out via the
availability check, but a dedicated miri test module exercises the
pure-Rust logic:

| Component | What miri validates |
|-----------|---------------------|
| `BgidLease::allocate` | No out-of-bounds on the `free_within` Vec; no aliasing violations |
| `BgidLease::deallocate` | Returned offset `(id - base) as u8` does not overflow `u8` bounds |
| `BgidLease::drop` | Batch return to `bgid_free_list()` does not trigger UB in the Mutex<Vec> path |
| `ThreadLocalRingPool` slot management | Vec indexing, Option take/replace, pool-id lookup |
| `PerThreadRing::new` error path | Returns `Err` cleanly; no dangling state in the TLS slot |

### 3.2 No stacked-borrows violations

Miri's stacked-borrows model must pass for all `BgidLease` and
`ThreadLocalRingPool` operations. A stacked-borrows violation indicates
that a reference was invalidated while still live - typically a `RefCell`
aliasing bug or an interior-mutability misuse.

### 3.3 No memory leaks (miri leak check)

Miri's leak checker (`-Zmiri-leak-check`) must confirm no leaked
allocations in the `BgidLease` / `ThreadLocalRingPool` code paths after
all test threads complete.

## 4. Expected failure modes

These are the failure patterns most likely to surface under instrumentation.
Each maps to a specific component and investigation path.

### 4.1 BGID collision

**Symptom:** tsan reports a data race on `Vec<u16>` in `bgid_free_list()`,
or miri reports an out-of-bounds access in `BgidLease::allocate`.

**Root cause:** Two threads attempt to allocate from the global free-list
simultaneously without holding the mutex, or the `BgidLease` local cache
hands out a bgid that was already returned to the global pool by a dying
thread.

**Investigation:** Check whether `BgidLease::drop` and
`BgidAllocator::allocate` overlap without the free-list mutex. Verify the
`Mutex::lock()` call is not bypassed in any code path.

### 4.2 CQE buffer lifetime issues

**Symptom:** tsan reports use-after-free on memory within the CQE ring
buffer, or kernel returns `-EFAULT` on a submission that references a
registered buffer whose backing memory was freed.

**Root cause:** A registered buffer group was de-registered or its backing
memory freed while a CQE referencing that buffer was still in-flight.
This can happen if TLS destructors run in the wrong order (ring drops
before buffer group, releasing the kernel-side pin before the CQE is
reaped).

**Investigation:** Verify the TLS slot layout places the ring and buffer
group in the same tuple (IUR-2 section 2.3 / IUR-3.e). Confirm
`Drop` ordering within the tuple is field-declaration order (ring drops
last, releasing kernel pin after buffer group memory is freed). If the
issue is inter-TLS-slot ordering, colocate into a single
`thread_local!` cell.

### 4.3 Ring overflow (SQ full)

**Symptom:** `submit_and_wait(1)` returns `-EBUSY` or the SQE push
returns an SQ-full error. This is not a tsan/miri finding but a
functional failure that may only manifest under instrumentation slowdown.

**Root cause:** tsan instrumentation slows CQE reaping relative to SQE
submission. If the harness submits faster than the instrumented reap path
can drain, the ring's SQ fills up.

**Investigation:** Not a correctness bug - this is a harness tuning
issue. Fix by either increasing ring depth
(`OC_RSYNC_IOURING_SQ_ENTRIES=256`) for instrumented runs, or switching
to submit-and-drain-all-before-next-push (already the harness's pattern:
one SQE at a time with immediate reap).

### 4.4 TLS destructor ordering race

**Symptom:** tsan reports a race between a thread's TLS destructor
(dropping the ring) and the parent thread's post-join assertions
(reading the ring-fd set). Or: the ring fd is recycled before the parent
reads it.

**Root cause:** The exit barrier pattern (IUR-4.a section 2.2) should
prevent this. If it surfaces, the barrier is not correctly sequencing
the parent's read before the worker's TLS drop.

**Investigation:** Confirm the exit barrier fires before `JoinHandle::join`
returns. The barrier count must be `workers + 1` (parent participates).
Workers must pass the barrier before exiting the closure (which triggers
TLS drop on thread exit).

### 4.5 Atomic ordering violation (theoretical)

**Symptom:** tsan reports a data race on a load/store of
`NEXT_POOL_ID` or `THREAD_RING_FALLBACK_COUNT` under `Relaxed` ordering.

**Root cause:** `Relaxed` is correct for these counters because they have
no dependent loads (they are identity assignment and metrics counters
respectively). A tsan report here would indicate the counter is being
used for synchronization elsewhere (a bug in consuming code, not in the
counter itself).

**Investigation:** Search for code that reads these counters and uses the
value to guard a memory access. If found, upgrade the ordering to
`Acquire`/`Release` on those paths.

## 5. Thresholds

### 5.1 Submission latency (p99)

| Thread count | p99 submission latency (single SQE round-trip) | Threshold |
|--------------|-----------------------------------------------|-----------|
| 1 | Baseline (uninstrumented) | N/A |
| 1 (tsan) | <= 10x baseline | Acceptable tsan overhead |
| 16 (tsan) | <= 15x baseline | Acceptable tsan overhead under contention |
| 1 (uninstrumented) | <= 50 us | Absolute ceiling for NOP round-trip |
| 16 (uninstrumented) | <= 100 us | Absolute ceiling under 16-thread fan-out |

These thresholds are not pass/fail criteria for IUR-4.b (which is a
correctness check, not a performance bench). They serve as
**regression flags** for the IUR-5 bench that follows. If uninstrumented
p99 exceeds the absolute ceilings above, investigate before proceeding
to IUR-5.

### 5.2 Ring utilization bounds

| Metric | Acceptable range | Red flag |
|--------|-----------------|----------|
| SQ occupancy at submission | 0-1 entries (one-at-a-time pattern) | > 1 indicates batching (not the harness's intent) |
| CQ drain lag | 0 entries after each reap | > 0 indicates lost completions |
| Ring fd count during stress | Exactly N (thread count) | < N means ring sharing; > N means ring churn |
| BGID lease allocations | <= 16 per thread per slice | > 16 indicates slice exhaustion / fallback to global pool |
| Global `bgid_free_list` mutex acquisitions | <= 2 per thread (1 lease, 1 return) | >> 2*N indicates slice too small or lease logic regression |

### 5.3 Wall-clock bounds

| Test cell | Maximum wall-clock (uninstrumented) | Maximum wall-clock (tsan) |
|-----------|-------------------------------------|---------------------------|
| 1 thread, 100K ops | 30 seconds | 300 seconds |
| 16 threads, 6250 ops each | 15 seconds | 150 seconds |

Exceeding these bounds is not an IUR-4.b failure (correctness is the
gate), but it indicates a performance issue that blocks IUR-5.

## 6. Outcome documentation template

After IUR-4.b completes, fill in the following template and commit it as
`docs/design/iur-4b-stress-outcome.md`.

```markdown
# IUR-4.b stress outcome

## Run environment

| Property | Value |
|----------|-------|
| Kernel | (e.g., 6.1.38-generic) |
| CPU | (e.g., AMD EPYC 7R13, 8 cores) |
| RAM | (e.g., 16 GB) |
| Rust toolchain | (e.g., nightly-2026-06-01) |
| io_uring ring depth | (e.g., 64) |
| BGID slice size | (e.g., 16) |

## Thread Sanitizer results

| Thread count | Pass/Fail | Races found | Notes |
|--------------|-----------|-------------|-------|
| 1 | | | |
| 2 | | | |
| 4 | | | |
| 8 | | | |
| 16 | | | |

**tsan command:**
(paste exact command used)

**tsan output (if failures):**
(paste relevant tsan report sections)

## Miri results

| Component | Pass/Fail | Violations found | Notes |
|-----------|-----------|-----------------|-------|
| BgidLease::allocate | | | |
| BgidLease::deallocate | | | |
| BgidLease::drop | | | |
| ThreadLocalRingPool slots | | | |
| PerThreadRing error path | | | |
| Leak check | | | |

**miri command:**
(paste exact command used)

**miri output (if failures):**
(paste relevant miri report sections)

## Latency observations (informational, not gating)

| Thread count | p50 (us) | p99 (us) | Threshold met |
|--------------|----------|----------|---------------|
| 1 | | | |
| 16 | | | |

## Ring utilization observations

| Metric | Observed | Within bounds |
|--------|----------|--------------|
| Ring fd count (16 threads) | | |
| CQ drain lag (max) | | |
| BGID mutex acquisitions total | | |

## Overall verdict

- [ ] tsan: zero races across all thread counts
- [ ] miri: zero UB, zero leaks
- [ ] Wall-clock within bounds
- [ ] BGID pool balance maintained

**Verdict:** PASS / FAIL

**Blocking issues (if FAIL):**
(list each issue with its failure-mode classification from section 4)
```

## 7. Regression report format

If IUR-4.b surfaces a failure after a previously-passing run (e.g., a
code change introduces a race), document the regression as follows:

```markdown
# IUR-4 regression: <short description>

## Classification

Failure mode: (one of: BGID collision, CQE buffer lifetime, ring overflow,
               TLS destructor ordering, atomic ordering violation, other)

## Reproduction

- Commit: <sha>
- Command: <exact tsan/miri invocation>
- Thread count at failure: <N>
- Frequency: (always / intermittent with rate)

## tsan/miri output

(paste full report)

## Root cause

(analysis)

## Fix

(PR link or description)

## Verification

(confirmation that the fix resolves the report under the same invocation)
```

## 8. Follow-up actions on failure

Each failure class maps to a specific component owner and investigation
path:

| Failure class | Component | First action | Escalation |
|---------------|-----------|--------------|------------|
| BGID collision | `buffer_ring/allocator.rs`, `bgid_lease.rs` | Audit mutex acquisition in `allocate`/`deallocate`; check `BgidLease::drop` | If lease logic is correct, the issue is in the global allocator's `Vec` access |
| CQE buffer lifetime | `per_thread_ring.rs`, TLS slot layout | Verify field-declaration order in the TLS tuple; confirm ring outlives buffer group | If ordering is correct, check kernel-side pin release timing via `strace` |
| Ring overflow | Harness tuning (not a product bug) | Increase `sq_entries` for instrumented runs; verify one-at-a-time submit pattern | If overflow persists at depth 256, the reap path has a latent bug |
| TLS destructor ordering | `session_pool.rs` TLS cell, exit barrier | Verify barrier count is `workers + 1`; confirm barrier fires before closure exit | If barrier is correct, the issue is platform-specific TLS destructor ordering |
| Atomic ordering violation | `session_pool.rs`, metrics counters | Search for dependent loads on the counter value; upgrade ordering if found | If no dependent loads exist, the tsan report is a false positive (document) |

### 8.1 Blocking vs non-blocking failures

- **Blocking:** BGID collision, CQE buffer lifetime, atomic ordering
  violation. These indicate correctness bugs that must be fixed before
  IUR-5 (bench) or IUR-6 (shared-ring removal) proceed.
- **Non-blocking:** Ring overflow under tsan (harness tuning), wall-clock
  threshold exceeded (performance, not correctness). Document and proceed
  to IUR-5 which will measure the performance dimension independently.
- **Informational:** tsan false positives on thread-local access (document
  with suppression file if needed).

## 9. Relationship to downstream tasks

### 9.1 IUR-5 (bench)

IUR-5 measures throughput and latency of the per-thread ring topology.
It is gated on IUR-4 passing:

- **IUR-4 pass** -> IUR-5 proceeds with the per-thread topology as the
  candidate under bench.
- **IUR-4 fail (blocking)** -> IUR-5 is blocked until the failure is
  resolved and IUR-4 re-passes.
- **IUR-4 fail (non-blocking)** -> IUR-5 proceeds but documents the
  known non-blocking issue in its bench report.

IUR-5 uses the latency thresholds from section 5.1 as a baseline
sanity check: if uninstrumented p99 exceeds the absolute ceilings,
the bench investigates the cause before reporting throughput numbers.

### 9.2 IUR-6 (shared_ring removal gate)

IUR-6 (`docs/design/shared-ring-removal-plan.md`) removes the dormant
`SharedRing` abstraction. Its gate criteria (IUR-6.b section 6.6)
require:

1. IUR-4 stress test passes (this document's success criteria).
2. IUR-5 bench shows >= 25% throughput uplift on tiny-file workload.
3. IUR-5 bench shows single-thread parity within +/- 5%.
4. Two consecutive nightly interop runs green with `per-thread-rings`
   enabled.

If IUR-4 fails with a blocking issue, IUR-6 cannot proceed. The
shared-ring removal stays gated until the per-thread topology is
proven correct under instrumentation.

### 9.3 Default-on decision (IUR-2 section 8.2)

The cargo feature `per-thread-rings` flips to default-on only after all
four criteria in IUR-2 section 8.2 are met. IUR-4 is criterion 1. A
blocking failure here keeps the feature default-off; the existing
per-file ring path remains the production default.

## 10. Cross-references

- `crates/fast_io/tests/iouring_per_thread_stress.rs` - the IUR-4.a
  harness implementation this outcome documents.
- `crates/fast_io/src/io_uring/per_thread_ring.rs` - the `with_ring`
  primitive under test (IUR-3.a).
- `crates/fast_io/src/io_uring/bgid_lease.rs` - the `BgidLease` /
  `with_thread_lease` primitive (IUR-3.e).
- `crates/fast_io/src/io_uring/session_pool.rs` - `ThreadLocalRingPool`,
  TLS slot layout.
- `docs/design/iur-2-per-thread-rings.md` section 6 - acceptance grid.
- `docs/design/iur-2-per-thread-rings.md` section 8.2 - default-on
  criteria.
- `docs/design/shared-ring-removal-plan.md` section 6.6 - removal timing
  gate.
- `docs/design/io-uring-bgid-namespace.md` - BGE-4, the process-global
  bgid free-list under test.
