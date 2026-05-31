# ASY-9.b: io_uring async coexistence implementation plan

Status: Design.
Tracking: ASY-9.b.
Predecessor: ASY-9.a (`docs/design/iouring-async-dispatch.md`, PR #5252) -
concluded KEEP io_uring SYNCHRONOUS; tokio-uring rejected.

## 1. Purpose

ASY-9.a settled the strategic question: the per-thread io_uring topology
(IUR-3) remains synchronous. This document specifies the implementation
details of the **coexistence model** - how the synchronous io_uring path
lives inside `spawn_blocking` islands when the `tokio-transfer` async
pipeline is active, covering thread pool interaction, ring lifecycle,
BGID leases, testing, performance budget, and feature flag composition.

## 2. Architecture overview

When both `tokio-transfer` and `io_uring` features are enabled, the call
chain is:

```
tokio multi_thread runtime (daemon or embedded)
    |
    | tokio::spawn(async transfer task)
    |     |
    |     |  .await on wire reads/writes (boundaries 1,2,4,5)
    |     |  .await on mpsc channels (boundaries 6,7)
    |     |
    |     +-- spawn_blocking (boundary 9: disk-commit task, long-lived)
    |              |
    |              |  block_on(async { recv from mpsc, dispatch to ring })
    |              |
    |              +-- per-thread io_uring ring (IUR-3.a)
    |              |       submit_and_wait(n) - synchronous
    |              |       file_writer / file_reader / disk_batch
    |              |
    |              +-- BgidLease (BGE-4, thread-local)
    |                      PBUF_RING buffer groups
    |
    +-- spawn_blocking (boundary 8: rayon basis-file batch)
    |        |
    |        +-- rayon par_iter -> per-worker with_ring()
    |                  per-thread ring on each rayon worker
    |
    +-- spawn_blocking (boundary 3: basis-file read)
             |
             +-- mmap / read_exact (no io_uring on this path)
```

Key invariants:

- **One ring per OS thread.** The `thread_local!` `THREAD_RING` in
  `per_thread_ring.rs` guarantees each OS thread that calls `with_ring`
  gets exactly one `IoUring` instance. This holds regardless of whether
  the thread is a rayon worker, a dedicated `std::thread`, or a tokio
  blocking-pool thread.

- **No ring crosses an `.await` point.** The ring reference (`&mut
  IoUring`) is borrowed inside `with_ring`'s closure. The closure is
  synchronous; `submit_and_wait` blocks the OS thread until CQEs arrive.
  The async runtime never sees the ring.

- **spawn_blocking is the sole async/sync bridge.** Tokio tasks never
  call io_uring directly. All disk I/O funnels through `spawn_blocking`
  islands that own the calling thread for the duration of the ring
  interaction.

## 3. Thread pool interaction

### 3.1 Tokio blocking pool threads as io_uring hosts

Tokio's blocking pool (`tokio::task::spawn_blocking`) creates OS threads
on demand up to a configurable ceiling (default 512, controlled by
`max_blocking_threads`). These threads are recycled: after completing a
blocking task, a thread waits for the next task up to a keep-alive
timeout (default 10s in tokio 1.x) before exiting.

When a blocking-pool thread first calls `with_ring`, the `thread_local!`
`THREAD_RING` cell is populated via `PerThreadRing::new()`. Subsequent
blocking tasks dispatched to the same OS thread reuse the ring without
`io_uring_setup(2)`. When the thread exits (keep-alive timeout or
runtime shutdown), the TLS destructor drops the ring, calling `close(2)`
on the ring fd and unmapping SQ/CQ pages.

This recycling model is **compatible with per-thread rings** because:

1. `RefCell<Option<PerThreadRing>>` guarantees exclusive access - only
   one blocking task runs on a given OS thread at a time (tokio's
   blocking pool is work-queue-per-thread, not work-stealing).
2. No ring state leaks between tasks because `with_ring` borrows the
   ring for the closure's scope and the ring's internal SQ/CQ state is
   fully drained by `submit_and_wait` before the closure returns.

### 3.2 Pool sizing for io_uring workloads

The disk-commit task (boundary 9) is long-lived - one per active
connection for the connection's lifetime. Each holds a blocking-pool
slot. The basis-file batch (boundary 8) holds a slot transiently per
batch. Sizing guidance from ASY-3:

```
TOKIO_TRANSFER_BLOCKING_THREADS >= max_connections * 4
```

The factor of 4 covers: 1 long-lived disk-commit + 1 transient basis
batch + 2 headroom for CLI invocations and rayon spillover. The io_uring
ring count at steady state equals the number of OS threads in the pool
that have called `with_ring` at least once - bounded by pool size, not
by connection count (multiple connections may time-share pool threads).

### 3.3 Rayon workers under spawn_blocking

Boundary 8's pattern (`spawn_blocking { par_iter { with_ring() } }`)
creates a nested threading model: one blocking-pool thread enters rayon,
which fans out across rayon's own thread pool. Each rayon worker gets its
own per-thread ring via `THREAD_RING`. These rings are disjoint from the
disk-commit ring on the blocking-pool thread. The rayon pool is
process-global and outlives individual connections, so its per-thread
rings persist across connection boundaries - this is the desired
amortization behavior.

## 4. Ring lifecycle

### 4.1 Creation

A ring is created on the first `with_ring` call from a given OS thread:

1. `THREAD_RING.with(|cell| ...)` - access TLS slot.
2. `cell.try_borrow_mut()` - exclusive access (fails with `WouldBlock`
   if re-entrant).
3. `guard.is_none()` check - only first call proceeds.
4. `PerThreadRing::new()` - `IoUringConfig::build_ring()` issues
   `io_uring_setup(2)`, maps SQ/CQ, optionally sets SQPOLL if
   `mmap_basis_active` is false.
5. Store in `Option<PerThreadRing>`.

Cost: one `io_uring_setup(2)` syscall + two `mmap(2)` calls (SQ + CQ).
Amortized to zero on subsequent calls from the same thread.

### 4.2 Reuse across blocking tasks

When tokio recycles a blocking-pool thread, the `THREAD_RING` cell
retains its value. The next blocking task dispatched to that thread
finds `guard.is_some()` and skips setup. The ring's SQ and CQ are empty
(invariant: `submit_and_wait` drains all CQEs before returning) so no
stale completions leak between tasks.

### 4.3 Destruction

The ring is destroyed when the OS thread exits:

- **Tokio keep-alive expiry:** blocking-pool threads exit after 10s
  idle. TLS destructor runs, dropping `PerThreadRing`, closing the ring
  fd. The kernel cancels any in-flight SQEs (there are none - see
  4.2 drain invariant) and reclaims kernel memory.
- **Runtime shutdown:** `runtime.shutdown_timeout(duration)` joins all
  blocking threads. Each thread's TLS destructor runs on join.
- **Rayon thread exit:** rayon workers exit on pool drop (process exit).
  Same TLS destructor path.

No explicit ring teardown API is required. The existing `thread_local!`
destructor semantics handle all cases.

### 4.4 Failure mode: ring creation fails

If `io_uring_setup(2)` returns `ENOSYS` (kernel too old), `ENOMEM`, or
`EPERM` (seccomp), `PerThreadRing::new()` returns `Err`. The `with_ring`
caller receives the error and falls back to standard I/O per IUR-2
section 5.2. The TLS cell remains `None`; subsequent calls on the same
thread will retry creation (allowing transient `ENOMEM` to recover).

## 5. BGID lease implications

### 5.1 Lease lifecycle on blocking-pool threads

`BgidLease` lives in `thread_local!` storage alongside the ring. On a
blocking-pool thread:

1. **First use:** `with_thread_lease` constructs a `BgidLease`,
   batch-allocating 16 bgids from the central `BgidAllocator`.
2. **Reuse:** recycled blocking tasks on the same thread reuse the
   lease. `take()` pops from the local free-list without touching the
   central mutex.
3. **Thread exit:** the lease's `Drop` impl returns all cached bgids to
   the central pool via `deallocate_batch`.

### 5.2 Thread recycling and lease churn

Tokio's blocking pool recycles threads aggressively (keep-alive 10s).
Under bursty workloads:

- A spike creates N blocking threads, each acquiring a lease of 16
  bgids (total: N * 16 bgids allocated from central pool).
- After the spike, idle threads exit within 10s, returning their leases.
- A subsequent spike may create new threads that re-allocate from the
  central pool.

This churn is acceptable because:

- `BgidAllocator` is a monotonic counter + free-list. Returned bgids go
  to the free-list; re-allocation pops from the list without syscalls.
- The central mutex is touched only on lease creation/destruction (thread
  birth/death), not on per-file I/O. Under steady state (long-lived
  disk-commit threads), the mutex is never contested.
- The 16-bgid batch size means at most `ceil(N/16)` central mutex
  acquisitions per spike, where N is the total bgid demand.

### 5.3 BGID exhaustion under high thread churn

If blocking-pool thread count * 16 exceeds the process-wide BGID space
(65535 usable bgids), lease allocation fails. Mitigations:

- The blocking pool ceiling (512 threads * 16 = 8192 bgids) is well
  within budget.
- The `BGID_WARNING_THRESHOLD` (process-wide) logs when 80% of bgid
  space is consumed.
- Lease failure surfaces as an `io::Error` from `with_thread_lease`,
  causing the caller to fall back to non-registered I/O.

### 5.4 No lease migration needed

Because `spawn_blocking` tasks are pinned to a single OS thread for
their lifetime (tokio never migrates a blocking task between threads),
the thread-local lease model is inherently correct. No `Send`/`Sync`
adaptation is needed.

## 6. Testing

### 6.1 Unit test: io_uring from spawn_blocking context

Verify that `with_ring` works correctly when called from inside
`tokio::task::spawn_blocking`:

```rust
#[tokio::test]
async fn io_uring_works_from_spawn_blocking() {
    let result = tokio::task::spawn_blocking(|| {
        with_ring(|ring| {
            // Submit a NOP and verify completion
            let nop = io_uring::opcode::Nop::new().build();
            unsafe { ring.submission().push(&nop).unwrap() };
            ring.submit_and_wait(1)?;
            let cqe = ring.completion().next().unwrap();
            assert_eq!(cqe.result(), 0);
            Ok(())
        })
    })
    .await
    .unwrap();
    assert!(result.is_ok());
}
```

### 6.2 Integration test: ring reuse across sequential blocking tasks

Verify that the same blocking-pool thread reuses its ring (no
`io_uring_setup` per task):

```rust
#[tokio::test]
async fn ring_reused_across_blocking_tasks() {
    // Force a single blocking thread by serializing tasks
    let fd1 = tokio::task::spawn_blocking(|| {
        with_ring(|ring| Ok(ring.as_raw_fd()))
    }).await.unwrap().unwrap();

    // Same thread is recycled (within keep-alive)
    let fd2 = tokio::task::spawn_blocking(|| {
        with_ring(|ring| Ok(ring.as_raw_fd()))
    }).await.unwrap().unwrap();

    // Ring fd should be identical (same ring reused)
    assert_eq!(fd1, fd2);
}
```

### 6.3 Stress test: concurrent spawn_blocking with io_uring

Verify N concurrent blocking tasks each get isolated rings:

```rust
#[tokio::test]
async fn concurrent_blocking_tasks_get_isolated_rings() {
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let b = barrier.clone();
            tokio::task::spawn_blocking(move || {
                // Synchronize to maximize thread concurrency
                tokio::runtime::Handle::current()
                    .block_on(b.wait());
                with_ring(|ring| Ok(ring.as_raw_fd()))
            })
        })
        .collect();

    let fds: Vec<i32> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap().unwrap())
        .collect();

    // All fds should be distinct (per-thread isolation)
    let unique: HashSet<i32> = fds.iter().copied().collect();
    assert_eq!(unique.len(), fds.len());
}
```

### 6.4 BGID lease test from blocking context

Verify lease allocation and return work correctly through the
spawn_blocking lifecycle:

```rust
#[tokio::test]
async fn bgid_lease_works_from_spawn_blocking() {
    let bgid = tokio::task::spawn_blocking(|| {
        with_thread_lease(|lease| lease.take())
    }).await.unwrap();
    assert!(bgid.is_ok());
    // bgid returned to central pool when thread exits
}
```

### 6.5 Wire-byte parity test

ASY-11.a (`docs/design/asy-11a-wire-parity-test.md`) covers end-to-end
verification that the async transfer path produces identical wire bytes
to the synchronous path. The io_uring coexistence model does not alter
wire output because the ring sits below the protocol layer - it
accelerates the same write/fsync operations. ASY-11.a's capture-replay
harness validates this transitively.

## 7. Performance budget

### 7.1 spawn_blocking overhead

Each `spawn_blocking` call incurs:

| Component | Cost | Notes |
|-----------|------|-------|
| Task allocation | ~200 ns | `Box<dyn FnOnce>` + waker |
| Thread wake (condvar) | ~1-3 us | Only if thread is parked |
| Thread reuse (hot) | ~50 ns | Task already waiting |
| Join handle poll | ~100 ns | Single atomic load |

Total per-call overhead: **1-4 us** when the pool thread is parked,
**~350 ns** when hot (thread already waiting for work).

### 7.2 Amortization strategy

The overhead is amortized by the granularity of blocking islands:

- **Boundary 9 (disk-commit):** One `spawn_blocking` per connection
  lifetime. Overhead: ~3 us total, amortized across thousands of files.
  Negligible.
- **Boundary 8 (basis batch):** One `spawn_blocking` per signature batch
  (default 64 files). Overhead: ~3 us per 64 files = ~50 ns/file.
  Negligible vs the ~500 us/file disk I/O.
- **Boundary 3 (basis read):** One `spawn_blocking` per basis load.
  Overhead: ~3 us per file. Basis reads are ~100 us (4 KB page) to ~10
  ms (large file mmap fault). Overhead is 0.03-3% of payload.

### 7.3 Comparison to direct thread dispatch

The synchronous (non-tokio) path uses `std::thread::spawn` for the
disk-commit thread - a one-time cost of ~20-50 us (thread creation +
stack allocation). Under `tokio-transfer`, `spawn_blocking` replaces
this with ~3 us (thread already exists in pool). The async path is
**cheaper** for connection setup because it avoids per-connection thread
creation.

### 7.4 io_uring batch efficiency unchanged

`submit_and_wait(n)` batches up to 64 SQEs per kernel entry regardless
of whether the calling thread is a dedicated `std::thread` or a tokio
blocking-pool thread. The kernel does not distinguish - it sees an
`io_uring_enter(2)` syscall from an OS thread. Batch efficiency is
determined by SQ depth and submission patterns, not by the thread's
provenance.

### 7.5 Budget conclusion

The `spawn_blocking` bridge adds < 5 us per blocking island. Given that
each island performs 10-1000 ms of I/O work, the overhead is < 0.05% of
wall time. No optimization is needed at this layer.

## 8. Feature flag interaction

### 8.1 Flag matrix

| `tokio-transfer` | `io_uring` | Behavior |
|-----------------|------------|----------|
| off | off | Pure sync, standard I/O (default on non-Linux) |
| off | on | Pure sync, io_uring via std::thread (current production) |
| on | off | Async pipeline, standard I/O in spawn_blocking |
| on | on | **Coexistence model** (this document) |

### 8.2 Conditional compilation

The coexistence model requires no new `#[cfg]` gates in `fast_io`. The
per-thread ring API (`with_ring`, `with_thread_lease`) is
thread-agnostic - it does not know or care whether its calling thread is
a rayon worker, a std::thread, or a tokio blocking-pool thread. The
existing `#[cfg(all(target_os = "linux", feature = "io_uring"))]` gate
on the io_uring module is sufficient.

The `tokio-transfer` feature affects only `crates/transfer` and
`crates/core` (channel swaps, async function signatures, spawn_blocking
sites). `crates/fast_io` is untouched.

### 8.3 No feature flag conflicts

The two features are orthogonal:

- `tokio-transfer` changes **how threads are scheduled** (tokio pool vs
  explicit std::thread).
- `io_uring` changes **how I/O is submitted** (ring vs syscall per op).

Enabling both simultaneously is the intended production configuration on
Linux. The only interaction point is that `spawn_blocking` threads host
io_uring rings - which works by the thread-local design documented
above.

### 8.4 Feature-gate testing in CI

CI tests all four flag combinations:

```yaml
strategy:
  matrix:
    features:
      - ""                    # off/off
      - "io_uring"           # off/on
      - "tokio-transfer"     # on/off
      - "tokio-transfer,io_uring"  # on/on (coexistence)
```

The coexistence cell (`tokio-transfer,io_uring`) runs the tests from
section 6 in addition to the standard nextest suite.

## 9. Failure modes and fallbacks

### 9.1 io_uring unavailable at runtime

If the kernel lacks io_uring support (< 5.6, seccomp blocked), the ring
creation in `with_ring` fails and the caller falls back to standard I/O.
This is unchanged from the non-tokio path - the fallback is inside
`fast_io`, below the spawn_blocking boundary.

### 9.2 Blocking pool exhaustion

If `max_blocking_threads` is reached and a new `spawn_blocking` is
attempted, tokio queues the task until a slot frees. Under extreme
connection load this causes backpressure on the async transfer task
(it awaits the join handle). The io_uring ring is not involved in this
failure - it is a scheduling concern handled by tokio's pool and the
`--max-connections` admission gate.

### 9.3 Ring fd leak on thread panic

If a blocking task panics after `with_ring` constructs the ring but
before the closure returns, the panic unwinds through the TLS
destructor, which drops the ring normally. Rust's panic = unwind
guarantees the TLS `Drop` impl runs. No ring fd leak occurs.

## 10. Migration path

### 10.1 No changes to fast_io

The `fast_io` crate requires zero modifications for ASY-9.b. Its
public API (`with_ring`, `with_thread_lease`, `write_file_with_io_uring`,
`read_file_with_io_uring`) is already compatible with any calling
context.

### 10.2 Changes in transfer/core (under tokio-transfer)

When `tokio-transfer` lands (ASY-7/8/12), the disk-commit thread
transitions from `std::thread::spawn` to `tokio::task::spawn_blocking`.
Inside the blocking task, the existing synchronous io_uring calls
(`with_ring`, `IoUringDiskBatch::submit_and_wait`) execute unchanged.
The only new code is the `Handle::block_on(async { recv.await })` loop
documented in ASY-3 boundary 9.

### 10.3 Verification checklist

Before marking ASY-9.b complete:

- [ ] Tests from section 6 pass on Linux with both features enabled.
- [ ] Wire-byte parity confirmed via ASY-11.a harness (sync vs async
      path produce identical output).
- [ ] Blocking pool sizing guidance documented in operator manual.
- [ ] No `#[cfg]` changes in `crates/fast_io`.
- [ ] `cargo clippy --features tokio-transfer,io_uring` clean.

## 11. Cross-references

- ASY-9.a (`docs/design/iouring-async-dispatch.md`) - strategic
  decision: keep io_uring synchronous, reject tokio-uring.
- ASY-3 boundaries 9, 10 (`docs/design/asy-3-async-boundary-spec.md`) -
  spawn_blocking island contracts this document implements.
- IUR-3.a (`crates/fast_io/src/io_uring/per_thread_ring.rs`) - the
  thread-local ring primitive.
- BGE-4 (`crates/fast_io/src/io_uring/bgid_lease.rs`) - the thread-local
  BGID lease primitive.
- ASY-11.a (`docs/design/asy-11a-wire-parity-test.md`) - wire-byte
  parity verification harness.
- ASY-8.a (`docs/design/sender-tokio-prototype.md`) - sender-side tokio
  migration that uses boundary 8.
- `docs/design/daemon-async-runtime-choice.md` - tokio multi_thread for
  daemon accept loop.
