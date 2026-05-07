# Async runtime impact on io_uring integration

Tracking task: oc-rsync #1595. Companion audits:
`docs/audits/async-io-uring-interaction.md`,
`docs/audits/per-file-vs-shared-uring-ring.md`,
`docs/audits/shared-iouring-session-instance.md`. Adjacent tasks:
#1751 (rayon-bound CPU work via `spawn_blocking`), #1937 (per-session
ring pool extension), #1872 (io_uring SEND non-blocking fix,
completed), #1267 (SQPOLL / DEFER_TASKRUN audit, completed).

This is a focused successor that captures the current sync-over-fd
pattern, the two viable async-integration models, the coupling with
#1751, and the soundness risks across `.await` points.

## 1. Current sync-over-fd pattern

Every io_uring ring in the workspace is owned by exactly one thread
and driven synchronously:

- `fast_io::io_uring::file_writer::IoUringWriter` and
  `file_reader::IoUringReader` each hold an `io_uring::IoUring`
  inside a struct that is `!Send + !Sync` once the kernel fd is
  registered. Submission goes through `submit_and_wait(1)`; there is
  no SQPOLL kernel thread by default and no async waker.
- `fast_io::io_uring::socket_reader` / `socket_writer` follow the
  same pattern for daemon and SSH stdio fast paths. They expose a
  blocking `read` / `write` API and trust the caller to budget
  syscalls.
- The disk-commit pipeline (`transfer/src/disk_commit/thread.rs`)
  spins up one OS thread per receiver, owns one writer ring, drains
  the SPSC queue, calls `submit_and_wait`, and exits when the
  channel closes. The ring's lifetime is bounded by the thread's
  stack frame.
- Session-shared rings (#1408 / #1409, see
  `shared_iouring-session-instance.md`) are leased through a
  `Mutex<Vec<Lease>>` guard and returned synchronously in the
  lease's `Drop`. The lease is `!Send` for the duration of the
  borrow; concurrency is achieved by holding multiple leases in
  parallel rayon tasks, not by `.await`.

There is currently no `pub async fn` that submits an SQE. Async
surfaces in `engine::async_io`, `transfer`, `daemon`, `bandwidth`,
and `protocol` use `tokio::fs` / `tokio::net` (epoll-backed) or do
not touch disk at all. The seam between async and io_uring sits at
`AsyncFileCopier::copy_into`, which dispatches the synchronous
io_uring writer through `tokio::task::spawn_blocking`.

## 2. Tokio integration models

Two practical options exist if a future async surface needs the
io_uring backend directly.

### (a) `spawn_blocking` over the existing sync API

The async caller hands an owned `Vec<u8>` (or a `Bytes`) plus an
`Arc<SharedRing>` lease to `tokio::task::spawn_blocking`, which runs
the existing synchronous `IoUringWriter::write_all` /
`IoUringReader::read` on the blocking pool. The future yields when
the closure returns. No ring object crosses an `.await` point: the
lease is acquired and released entirely inside the blocking closure.

Pros: zero changes to `fast_io`, no second runtime, the lease
contract stays synchronous, `!Send` stays inside one thread, errors
keep their current shape, registered buffer lifetime is bounded by
the closure's stack frame.

Cons: each submission costs one blocking-pool hop. For small SQEs
this dominates the kernel work. The blocking-pool default is 512
threads; bursty fan-out can saturate it. Cancellation requires
JoinHandle abort plus interior cancel-safe state.

### (b) `tokio-uring` (or `monoio`) as a second runtime

`tokio-uring` gives a `tokio::runtime::Runtime`-shaped API where
buffers are owned by the runtime, SQEs are issued from a dedicated
LocalSet, and futures are `!Send`. `monoio` is similar with a
thread-per-core default.

Pros: no blocking-pool hop. Native cancellation via dropped futures.
Direct integration with tokio timers and `select!`.

Cons: a second runtime in the same process. The main tokio runtime
cannot poll a `tokio-uring` future directly; bridging requires
spawn-on-LocalSet plus a oneshot. All ring buffers must use
`tokio_uring::buf::IoBuf` instead of `Vec<u8>`, fragmenting the
buffer-pool design. Session ring pool (#1937) has to be rebuilt on
top of `Pin<&mut>` futures and a `LocalSet`, because the current
lease type is `Send`-bounded by `Arc<SharedRing>` but
`tokio-uring`'s ring is `!Send`. The blast radius touches every
crate that currently borrows a lease.

A third option (raw `io-uring` crate driven by a custom waker
plugged into tokio's reactor through `AsyncFd`) was considered and
rejected: it produces the worst of both worlds - a hand-rolled
runtime with the registered-buffer constraints of `tokio-uring`.

## 3. Compatibility with #1751

#1751 moves rayon-bound CPU work (rolling checksum, MD5, MD4, zstd,
delta search) onto `tokio::task::spawn_blocking` so the async path
no longer ties up reactor threads on `par_iter` storms. The same
mechanism naturally services io_uring submissions:

- The blocking pool is already sized to absorb both rayon-bound CPU
  bursts and synchronous I/O dispatch. Adding io_uring writers does
  not change the pool's worst-case shape, because each disk-commit
  thread today is a permanent OS thread that #1751 will replace
  with on-demand blocking tasks of bounded lifetime.
- The lease contract stays synchronous. A blocking task acquires a
  lease, submits, waits, releases; the async caller awaits the
  `JoinHandle`. The lease never crosses `.await`.
- Cancellation semantics align: `JoinHandle::abort` propagates
  through the blocking task once the kernel returns, matching the
  cancel-on-drop expectation of the broader async surface.

The only friction is buffer ownership. #1751's CPU tasks return
owned `Vec<u8>`. io_uring submissions need the buffer pinned for
the kernel's view; with the synchronous API this is automatic
(stack-bound). With registered buffer rings (#1937) the buffer is
already pinned in the ring's slot, so the closure passes a slot
index rather than a slice, which is `Send`-clean.

Recommendation for #1751: budget one extra integration point - the
disk-commit thread becomes a blocking task that loops on the SPSC
channel and returns when the channel closes. No ring crosses an
`.await`; the future that wraps the disk-commit lives at the same
level as the receiver future.

## 4. Risks

### Kernel ring fd ownership across `.await`

The io_uring fd is a process-wide resource, but the in-memory ring
state (head/tail cursors, registered buffer table, completion
queue) is wait-free single-producer / single-consumer. Holding a
ring across `.await` is sound only if the future is `!Send` and
pinned to one thread. tokio's default executor moves futures
between worker threads on every poll, so a `Send` future cannot
hold a ring across an await without UB on the cursor and CQE
dequeue paths. `tokio-uring`'s `LocalSet` exists precisely to
prevent this. Option (a) sidesteps the issue: the ring lives
inside a blocking closure that does not yield.

### Registered buffer lifetime

Registered buffers (`io_uring::register_buffers`) hand the kernel a
borrow that must outlive every in-flight SQE referencing it. With
synchronous submission the borrow is the ring's lifetime; the ring
guarantees no SQE outlives its CQE drain. Across `.await` this
invariant becomes the future's responsibility: dropping the future
before the CQE arrives must not free the buffer. `tokio-uring`
solves this by transferring buffer ownership into the runtime and
returning it on completion. Option (a) keeps the invariant inside
the blocking closure (the closure does not return until the CQE is
drained), so the future is free to be cancelled at any await
boundary above the closure.

A future hazard is registered fixed files (`IORING_REGISTER_FILES`):
the registered fd table outlives a single submission and currently
lives inside the ring's `Drop`. Bridging this through `tokio-uring`
forces the registration into the LocalSet, which fragments the
session pool. Option (a) keeps registration synchronous.

## 5. Recommendation

Adopt option (a): keep io_uring synchronous behind the existing
`fast_io::io_uring` API and dispatch from async code via
`tokio::task::spawn_blocking`. This keeps the ring object `!Send`
inside one thread, preserves the synchronous lease contract for
the session ring pool (#1937), avoids a second runtime, and fits
naturally into #1751's blocking-pool budget. Registered buffer
lifetime stays bounded by the closure stack frame, and the kernel
ring fd never crosses an `.await` point.

Revisit when one of these triggers: (1) the blocking pool becomes a
bottleneck under measured load (per-submission hop dominates ring
work); (2) a hot path needs `select!` semantics with timer
cancellation that cannot be expressed as `JoinHandle::abort`; (3) a
third-party crate forces `tokio-uring` on us. None of those is
imminent; the present audit closes #1595 with no code change.
