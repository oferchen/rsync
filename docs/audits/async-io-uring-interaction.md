# Async runtime impact on io_uring integration

Tracking issue: oc-rsync task #1595. Adjacent tasks: #1267 (SQPOLL /
DEFER_TASKRUN audit, completed), #1860 (splice for SSH stdio,
completed), #1872 (io_uring SEND blocking fix, completed), #1408 /
#1409 (session ring pool, completed), #1936 / #1937 (per-session pool
extension, pending), #1751 (`spawn_blocking` for rayon-bound CPU
work, pending). Companion audit:
`docs/audits/tokio-dependency-boundary-2026.md` (PR #3706).

## Summary

Today every io_uring submission in oc-rsync runs from synchronous
code on a dedicated rayon-managed thread or on the disk-commit
thread. No `pub async fn` in the workspace currently drives an SQE
through the kernel; the async surfaces in `engine`, `transfer`,
`bandwidth`, `protocol`, and `daemon` either use `tokio::fs` /
`tokio::net` directly or do not perform I/O at all. The question this
audit answers is: when a future tokio-async path eventually needs the
io_uring backend, what runtime model does it use?

The recommendation is option (a) - keep io_uring synchronous and
service it from `task::spawn_blocking` (#1751) for the foreseeable
future. The alternative runtimes (`tokio-uring`, custom poll-driven
SQE submission) each introduce a second runtime, fragment the
session-pool design (#1937), and require either pinning
`!Send` rings to one thread or rebuilding the lease/return contract
on top of `Pin<&mut>` futures. None of those costs is justified by
the current workload mix: the only async surfaces that touch disk
(`AsyncFileCopier`, the disk-commit thread) already amortise ring
lifecycle on synchronous boundaries.

## 1. Methodology - static

This audit is read-only. No `cargo` invocation, no benchmark, no
runtime probe. Inputs:

- Every file under `crates/fast_io/src/io_uring/`
  (`mod.rs`, `config.rs`, `file_reader.rs`, `file_writer.rs`,
  `disk_batch.rs`, `socket_reader.rs`, `socket_writer.rs`,
  `socket_factory.rs`, `file_factory.rs`, `batching.rs`,
  `shared_ring.rs`, `registered_buffers.rs`, `buffer_ring.rs`).
- Every call site that constructs or drives an io_uring object
  outside `fast_io`: `crates/transfer/src/disk_commit/thread.rs`,
  `crates/transfer/src/disk_commit/process.rs`,
  `crates/transfer/src/transfer_ops/response.rs`,
  `crates/transfer/src/generator/mod.rs`,
  `crates/transfer/src/parallel_io.rs`,
  `crates/engine/src/async_io/copier.rs`,
  `crates/engine/src/async_io/batch.rs`.
- Companion audits in `docs/audits/`:
  `shared-iouring-session-instance.md`,
  `iouring-socket-sqpoll-defer-taskrun.md`,
  `mmap-page-fault-iouring-sqpoll.md`,
  `mmap-iouring-co-usage.md`,
  `disk-commit-iouring-batching.md`,
  `iouring-pbuf-ring.md`, `tokio-dependency-boundary-2026.md`.
- Design note `docs/design/iouring-session-ring-pool.md` (#1409).
- The `io-uring` crate manifest pin in
  `crates/fast_io/Cargo.toml:36` (`io-uring = { version = "0.7" }`).

The `tokio-dependency-boundary-2026.md` table at lines 61-87 is the
ground truth for which crates currently link tokio. Every
async-driven io_uring call site this audit considers must pass
through one of the seven crates listed there. `fast_io` is
deliberately excluded from that list and must remain so.

## 2. Current usage points - sync rayon-driven submission today

Every existing SQE submission lives behind a synchronous
`Read` / `Write` call. The sites and their drivers:

- Receiver writer entry, per file:
  `crates/transfer/src/transfer_ops/response.rs:108` calls
  `fast_io::writer_from_file`. The function is sync; it runs on the
  disk-commit thread spawned at
  `crates/transfer/src/disk_commit/thread.rs:53-56`. No tokio task is
  involved.
- Generator reader entry, per file at >= 1 MiB:
  `crates/transfer/src/generator/mod.rs:726` calls
  `fast_io::reader_from_path`. The generator runs on rayon worker
  threads via the threshold-based dispatch in
  `crates/transfer/src/parallel_io.rs:107-125`.
- Disk-commit batch ring:
  `crates/transfer/src/disk_commit/thread.rs:71-83` constructs the
  long-lived `IoUringDiskBatch` (`fast_io/src/io_uring/disk_batch.rs:70-79`).
  Every SQE submission for the receive side flows through that
  single dedicated thread.
- Single-SQE writes / reads:
  `crates/fast_io/src/io_uring/file_writer.rs:190` (`write_at`),
  `:381` (`fsync` in the `FileWriter::sync` impl), and
  `crates/fast_io/src/io_uring/file_reader.rs:124`
  (`read_at`) call `submit_and_wait(1)` on the ring. All three are
  driven from `&mut self` and block the calling thread.
- Batched write submission:
  `crates/fast_io/src/io_uring/batching.rs:109`
  (`submit_write_batch`) calls `submit_and_wait(submitted)`. Used by
  `submit_write_fixed_batch` and the regular `submit_write_batch`
  paths in `file_writer.rs`.
- Batched send submission with PollOut gating:
  `crates/fast_io/src/io_uring/batching.rs:213` and `:336`
  (`poll_writable` and `submit_send_batch`), used by
  `crates/fast_io/src/io_uring/socket_writer.rs:55-62`.
- Shared ring `submit_and_wait`:
  `crates/fast_io/src/io_uring/shared_ring.rs:404-406` is the only
  external entry and is also synchronous.

Every call to `submit_and_wait` is therefore a parking syscall on a
thread the runtime knows nothing about. The async crates that exist
today (`engine/async_io`, `transfer/pipeline/async_pipeline`,
`bandwidth/async_limiter`, `protocol/multiplex/codec`, `daemon/
async_session`) do not reach any of the call sites above. Where they
need disk I/O, they use `tokio::fs` (`engine/async_io/copier.rs:124,
:127, :184` use `tokio::fs::File::open`, `OpenOptions::open`, and
`spawn_blocking` for metadata); where they need network I/O, they
use `tokio::net` types behind `tokio_util::codec`.

The only runtime-aware bridge in the workspace is the russh /
embedded-ssh path at
`crates/rsync_io/src/ssh/embedded/connect.rs:107-122`, which builds a
private `tokio::runtime::Builder::new_current_thread()` to run a
russh client. That runtime never touches `fast_io`; russh owns its
own poll-driven I/O against a TCP socket.

## 3. Why mixing tokio executor + io_uring is non-trivial

Three independent concerns make naive integration unsafe.

### 3.1 Blocking submit_and_wait

`submit_and_wait(n)` parks the calling thread until `n` CQEs are
ready. Inside an `async fn` running on a tokio worker, this stalls
the entire worker, and on a multi-thread runtime sized to
`num_cpus`, parks one core's-worth of cooperative scheduling for the
duration of the wait. The fix is `task::spawn_blocking`, but that
moves the ring access onto the blocking pool (default 512 threads)
and re-introduces the per-task synchronisation that the io_uring
SPSC submission queue was meant to avoid. Every site enumerated in
section 2 has this property: there is no non-blocking variant in
`fast_io` today, and the io-uring crate's `submission_shared` API
takes `&mut self`, which the `Mutex<RawIoUring>` design in
`docs/design/iouring-session-ring-pool.md:37-38` already accounts for.

A second hazard is reentrancy: if a future awakens inside an SQE's
completion handler, and the awakener holds the ring's mutex (because
`spawn_blocking` was used to submit), the next future to attempt a
lease deadlocks. The session-pool design avoids this by holding the
mutex only for the submit-and-wait cycle (see
`docs/design/iouring-session-ring-pool.md:55-59`), but composing this
with arbitrary futures is fragile.

### 3.2 SQPOLL semantics

`IORING_SETUP_SQPOLL` (config wired at
`crates/fast_io/src/io_uring/config.rs:381-390`) starts a kernel
thread that polls the SQ tail and submits without an
`io_uring_enter` syscall. Two interactions matter for any async
runtime:

- The kernel poller cannot service a major page fault from the
  submitter's address space; it falls back to task-work or stalls.
  See `docs/audits/mmap-page-fault-iouring-sqpoll.md:18-43` for the
  three failure modes. Tokio's blocking pool moves work between
  threads dynamically, so a future that pushes an SQE then awaits a
  CQE may find the SQE referencing memory not yet faulted on the
  current thread. The fix
  (`docs/audits/mmap-page-fault-iouring-sqpoll.md:92-104`) is to
  prefault on the submitter; under tokio that prefault must happen
  inside the same `spawn_blocking` closure that enqueues the SQE.
- `IORING_SETUP_DEFER_TASKRUN` (recommended in
  `docs/audits/iouring-socket-sqpoll-defer-taskrun.md:74-93`)
  pairs with `IORING_SETUP_SINGLE_ISSUER`, which requires the same
  task to submit and reap. Tokio rebalances futures across workers;
  unless the future is `!Send` or pinned to a specific worker, this
  invariant is violated. The russh path at
  `crates/rsync_io/src/ssh/embedded/connect.rs:112` already uses
  `new_current_thread()` for an unrelated reason; an io_uring
  integration would need the same constraint.

### 3.3 Runtime detection

`is_io_uring_available` (`crates/fast_io/src/io_uring/config.rs:167-180`)
caches the probe result in a process-wide `AtomicBool`. The probe
itself opens a 4-entry ring on first call
(`crates/fast_io/src/io_uring/config.rs:271-281`). Three consequences
for an async runtime:

- The probe is synchronous and fast (one `io_uring_setup` plus one
  `register_probe`). Running it inside a `tokio::spawn_blocking`
  closure is overkill, but running it on a tokio worker is also
  fine because it never sleeps for more than a syscall.
- The cache is process-global, not runtime-global. A library that
  uses two tokio runtimes (the workspace does not, but the
  embedded-ssh path constructs one for russh while the daemon may
  construct another) sees consistent results. Good.
- Fallback in the factories
  (`crates/fast_io/src/io_uring/file_factory.rs:115-128`,
  `:236-252`) is policy-driven and decided at construction time.
  Once an `IoUringOrStdReader::Std` variant is chosen, the type
  cannot upgrade to io_uring mid-transfer, regardless of which
  runtime is calling. This is the right contract; it just needs to
  be preserved across any async wrapper that may be built.

## 4. Three options

### Option (a): keep io_uring sync inside spawn_blocking (#1751)

Every async caller wraps the existing sync API in
`tokio::task::spawn_blocking`. Concretely:

```rust
// Conceptual sketch; not implemented.
let result = tokio::task::spawn_blocking(move || {
    let mut writer = fast_io::writer_from_file(file, cap, policy)?;
    writer.write_all(&data)?;
    writer.flush()?;
    Ok::<_, io::Error>(())
}).await??;
```

The pattern matches `engine/async_io/copier.rs:184-195` (already
uses `spawn_blocking` for permission/timestamp work). #1751 extends
the same idiom to the rayon-bound CPU work the receiver currently
runs synchronously.

### Option (b): tokio-uring crate

`tokio-uring` is a single-thread runtime that integrates io_uring
SQEs directly with the tokio reactor. Adopting it would mean:

- Adding a `tokio-uring` workspace dep (Linux-only, optional). The
  crate exposes `tokio_uring::start(async { ... })` rather than
  letting an existing tokio runtime drive futures, so it builds its
  own current-thread runtime per call.
- Replacing `fast_io::writer_from_file` callers with
  `tokio_uring::fs::File` on the async side. `tokio_uring::File`
  exposes `read_at(buf, offset).await` and `write_at(buf, offset).await`
  that submit SQEs and return `(io::Result<usize>, Buf)` so the
  buffer ownership crosses the await point.
- Maintaining a parallel implementation of session-pool, registered
  buffer groups, and SQPOLL fallback against `tokio-uring`'s ring
  abstraction.

### Option (c): custom poll-driven SQE submission via tokio's reactor

Build a `Future` whose `poll` method pushes an SQE on first poll,
registers a waker against the CQE's `user_data`, and returns
`Poll::Ready(...)` when a separate completion-pump task observes the
matching CQE. Concretely:

- One dedicated thread (or one tokio task pinned to a single worker)
  owns each ring's `CompletionQueue` and translates CQEs to waker
  notifications. `OpTag::decode`
  (`crates/fast_io/src/io_uring/shared_ring.rs:115-127`) is the
  in-tree prior art for `user_data` -> task demux.
- Each `SubmitFuture` holds the SQE inputs and a slot index allocated
  from the ring's per-ring slot table. On `Drop` (cancellation), the
  future must either wait for the kernel to finish or detach the
  buffer; this is the io_uring-crate `Cancel` opcode plus a poison
  flag.
- The submitter side is shared between async tasks; the SPSC SQ
  becomes MPSC and needs a `Mutex` exactly as the session pool
  proposes (`docs/design/iouring-session-ring-pool.md:54-59`).

This is the design `tokio-uring` itself implements internally; we
would be reimplementing it in-tree.

## 5. Per-option: feature surface gained / lost, code churn, risk

| Option | Gained | Lost / cost | Code churn (LoC, est.) | Risk |
|--------|--------|-------------|------------------------|------|
| (a) `spawn_blocking` | trivial path forward; reuses current API; pool design (`docs/design/iouring-session-ring-pool.md`) survives unchanged | one extra blocking-pool thread per concurrent SQE batch; no submission-side coalescing across futures | <200 (wrappers + tests) | low; identical syscall profile to today |
| (b) `tokio-uring` | true async io_uring with SQE-level cooperation; per-task back-pressure via `await` | second runtime in the workspace; current-thread only (every async caller pinned to one CPU); duplicates `IoUringConfig`, `RegisteredBufferGroup`, `IoUringDiskBatch`, session-pool inside `tokio-uring`'s type system; loses the `IoUringOrStdReader` fallback enum because `tokio-uring` types are not `!io_uring`-shaped | 1500-2500 (parallel `fast_io_async` module, factory rework, plumbing across `engine`/`transfer`) | high; ties non-trivial perf paths to a single-thread runtime; defeats the "tokio default-on" stance landed in #1732 |
| (c) custom poll-driven | full control; no extra runtime; integrates with the existing session-pool design | implementing what `tokio-uring` already provides; cancellation correctness (SQE in flight when future is dropped) is genuinely hard; CQE-pump task is a new long-lived component; SQPOLL + DEFER_TASKRUN constraints fall on the integrator | 2000-3500 (futures, pump, slot allocator, cancellation tests) | highest; one missed cancellation path leaks a kernel-pinned buffer for the lifetime of the ring |

Two cross-cutting losses are worth calling out for (b) and (c). Both
require pinning futures (or runtimes) so the SINGLE_ISSUER /
DEFER_TASKRUN flag set from
`docs/audits/iouring-socket-sqpoll-defer-taskrun.md:74-93` is honoured.
And both require parallel maintenance of the SQPOLL fallback path
(`crates/fast_io/src/io_uring/config.rs:381-396`); the existing path
returns `io::Result<RawIoUring>`, but a futures-based wrapper must
expose the same fallback as a `Future` outcome rather than a
synchronous error, doubling the surface that must stay in lockstep
with the kernel.

## 6. Interaction with shared session ring pool (#1937)

The pending session-pool work (#1937, building on #1408 / #1409 and
the design in `docs/design/iouring-session-ring-pool.md`) is the
load-bearing input for any async decision.

The pool's contract (sketched at
`docs/design/iouring-session-ring-pool.md:34-59`) is:

```text
RingPool::new(config, count) -> RingPool
RingPool::lease() -> RingLease<'_>     // blocks on Mutex<RawIoUring>
impl Drop for RingLease { /* return to pool */ }
```

`RingLease` holds `&mut RawIoUring` for the duration of the
submit-and-wait cycle (see notes at
`docs/design/iouring-session-ring-pool.md:55-59`). Three async-mode
considerations follow.

- Option (a) composes naturally. `tokio::task::spawn_blocking`
  acquires the lease inside the closure; the lease drops inside the
  closure; the `Future` only sees the result. No change to the pool
  API, no change to `RingPool::lease`.
- Option (b) cannot reuse the pool. `tokio-uring` rings are owned
  by the runtime and not exposed as `RawIoUring`. A second pool
  parallel to the sync pool would be required, doubling kernel
  ring count for any session that mixes sync and async paths
  (sender uses sync today; async daemon would use tokio-uring).
- Option (c) requires the lease guard to become a `Future` so that
  contention on the inner `Mutex` does not block a tokio worker.
  Either replace `Mutex` with `tokio::sync::Mutex` on the pool's
  slots (forcing every sync caller to also live in an async
  context), or keep `std::sync::Mutex` and run all leases inside
  `spawn_blocking` (which is just option (a) with extra
  indirection).

The recommendation in section 7 follows from this:
option (a) is the only one that lets the session pool stay a
synchronous, `fast_io`-internal type.

A separate consideration: `IoUringDiskBatch`
(`crates/fast_io/src/io_uring/disk_batch.rs:34-44`) is intentionally
`!Send` and `!Sync` and lives on a dedicated thread spawned in
`crates/transfer/src/disk_commit/thread.rs:53-56`. None of the three
options changes this; the disk-commit thread remains the canonical
synchronous endpoint for the receive side. The pool covers parallel
fan-out paths only.

## 7. Recommendation with explicit decision criteria

Adopt option (a): keep io_uring synchronous and route async callers
through `tokio::task::spawn_blocking`. Track the work under #1751.

Decision criteria, each treated as a hard gate:

1. **Tokio boundary stays out of `fast_io`.** The boundary policy
   re-verified in `docs/audits/tokio-dependency-boundary-2026.md` at
   lines 61-87 lists `fast_io` as `clean`. Options (b) and (c) both
   add tokio types to `fast_io`; only option (a) preserves the
   policy. This is non-negotiable per the "must never contain
   unsafe code" set in the design notes (paraphrased in
   `docs/audits/tokio-dependency-boundary-2026.md:142-152`).
2. **Session pool stays synchronous.** Adopting (b) or (c) forces a
   parallel async pool, doubling ring count and forking the
   lease/return semantics. The pool design at
   `docs/design/iouring-session-ring-pool.md` is the better
   investment to finish first; #1937 is already pending.
3. **Workload mix does not justify async io_uring.** No async caller
   today drives more than tens of concurrent file copies, and
   `engine/async_io/copier.rs` already uses `tokio::fs` plus
   `spawn_blocking` (line 184). The marginal latency gain from
   submitting SQEs per future is dominated by the existing
   blocking-pool dispatch cost.
4. **Cancellation semantics.** Async io_uring (option (c)) demands
   handling the case where a future drops with an SQE in flight.
   The kernel does not let us deallocate a registered buffer until
   its CQE is reaped (see `RegisteredBufferGroup` Drop ordering at
   `docs/audits/shared-iouring-session-instance.md:566-590`). The
   safe path is to wait for the CQE before drop completes, which
   amounts to the synchronous behaviour option (a) already has.
5. **No protocol change, no wire impact.** All three options are
   `fast_io`-internal. Confirmed by zero `io_uring` references in
   `crates/protocol/src/`
   (see `docs/audits/shared-iouring-session-instance.md:679-691`).

If a future workload demonstrates that `spawn_blocking` is the
bottleneck (criterion 3 reverses), revisit option (b) - but only
after the session pool has shipped, and only behind a feature gate
that pulls `tokio-uring` only when the user opts in.

## 8. Open questions

- **Is the `engine/async_io` path going to stay tokio-fs-based, or
  migrate to `fast_io`?** Today
  `crates/engine/src/async_io/copier.rs:124-135` uses
  `tokio::fs::File` and `BufReader`/`BufWriter` from `tokio::io`,
  bypassing `fast_io` entirely. If that migration happens
  (tracked separately), option (a) automatically gives that path
  io_uring on Linux without any further work; (b)/(c) require
  re-architecting the migration target.
- **Does the disk-commit thread remain SPSC for the foreseeable
  future?** The current design at
  `crates/transfer/src/disk_commit/thread.rs:170-225` is an explicit
  SPSC channel with a dedicated thread. The session-pool plan keeps
  this. If a future design parallelises disk commits (#1060), the
  io_uring side of that fan-out should reuse the session pool, which
  ties back to criterion 2 above.
- **How does this interact with #1860 (splice for SSH stdio)?**
  Splice for SSH stdio is unrelated to io_uring on the file side
  but does touch the same async / sync boundary on the network side.
  The SSH transport currently runs synchronously in
  `crates/rsync_io/src/ssh/`, with the embedded-ssh exception at
  `crates/rsync_io/src/ssh/embedded/connect.rs:107-122` using a
  current-thread tokio runtime. None of (a)/(b)/(c) changes this
  boundary, but option (b) would conflict with the russh runtime
  (two current-thread runtimes per process, both Linux-only).
- **What is the right fallback for `IoUringPolicy::Enabled` when
  the async wrapper is in use?** Today
  `crates/fast_io/src/io_uring/mod.rs:169-185` returns
  `io::ErrorKind::Unsupported` synchronously. Under option (a), the
  same error surfaces from inside the `spawn_blocking` closure and
  bubbles to the caller's `Result<(), io::Error>`. Under (b)/(c),
  the error must surface from a `Future`, which complicates the
  policy contract; option (a) preserves the existing contract
  exactly.
- **Bounding the blocking pool.** `tokio::runtime::Builder` defaults
  the blocking pool to 512 threads. If many concurrent
  `spawn_blocking` closures each lease an io_uring ring, the
  blocking pool can grow beyond the session pool's ring count.
  Mitigation is a `tokio::sync::Semaphore` sized to the pool count;
  this is plumbing, not architecture.

## References

- `crates/fast_io/src/io_uring/mod.rs:81-118` - public surface and
  fallback chain.
- `crates/fast_io/src/io_uring/config.rs:167-180` - process-wide
  availability cache.
- `crates/fast_io/src/io_uring/config.rs:381-396` - SQPOLL fallback
  path.
- `crates/fast_io/src/io_uring/file_writer.rs:170-204` - single-SQE
  blocking submit.
- `crates/fast_io/src/io_uring/batching.rs:53-143` - batched write
  submit-and-wait.
- `crates/fast_io/src/io_uring/batching.rs:270-379` - batched send
  with PollOut gating (#1872).
- `crates/fast_io/src/io_uring/shared_ring.rs:208-442` - shared ring
  reader+writer demux.
- `crates/fast_io/src/io_uring/disk_batch.rs:34-79` - dedicated
  long-lived ring on the disk-commit thread.
- `crates/transfer/src/disk_commit/thread.rs:47-83` - synchronous
  thread spawn and policy-driven batch construction.
- `crates/transfer/src/parallel_io.rs:107-125` - rayon dispatch for
  bounded-concurrency I/O.
- `crates/engine/src/async_io/copier.rs:124-195` - existing tokio-fs
  + `spawn_blocking` pattern that option (a) extends.
- `docs/audits/tokio-dependency-boundary-2026.md:61-87` - the
  authoritative tokio crate-membership table.
- `docs/audits/shared-iouring-session-instance.md` - constraint set
  for the session pool.
- `docs/audits/mmap-page-fault-iouring-sqpoll.md` - SQPOLL +
  page-fault interaction.
- `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` -
  DEFER_TASKRUN / SINGLE_ISSUER constraints.
- `docs/design/iouring-session-ring-pool.md` - pool API design and
  lease/return contract.
- `man 7 io_uring`, `man 2 io_uring_setup` -
  `IORING_SETUP_SQPOLL`, `IORING_SETUP_DEFER_TASKRUN`,
  `IORING_SETUP_SINGLE_ISSUER` semantics.
- Upstream rsync: `target/interop/upstream-src/rsync-3.4.1/io.c`
  uses blocking `read(2)` / `write(2)`; no async runtime. The
  io_uring integration is an oc-rsync-side optimisation with no
  wire-protocol implication.
