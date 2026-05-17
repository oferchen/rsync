# Async Daemon Impact on io_uring Integration (#1595)

Tracking issue: oc-rsync task #1595.

Companion design notes:

- `docs/design/async-migration-plan.md` (#4186) - workspace-wide async
  migration plan and phase ordering.
- `docs/design/spawn-blocking-bridge.md` (#4196) - contract for crossing
  the async/sync boundary via `tokio::task::spawn_blocking`.
- `docs/design/async-ssh-evaluation.md` (#4194) - async SSH transport
  evaluation that this document mirrors in shape.
- `docs/design/daemon-tokio-async-listener-impl.md` (#1935) - async
  daemon listener that this integration must compose with.
- `docs/design/iouring-daemon-tcp.md` (#1876) - io_uring socket I/O for
  daemon TCP, the only async-adjacent io_uring path that materially
  intersects this evaluation.
- `docs/design/async-io-uring-impact.md` - prior six-dimension
  evaluation of the same problem; this document is the focused
  decision-shaped successor.

## 1. Question

Phase 1 of #4186 migrates the daemon accept loop to tokio (#1935). The
synchronous transfer pipeline - including the `fast_io::io_uring`
submission paths - keeps running on dedicated `std::thread` and rayon
workers. Once a tokio worker accepts a connection and hands the session
to `core::session()`, every io_uring call sits behind some form of
boundary crossing.

This document evaluates the four candidate shapes for that crossing
and picks one:

1. `tokio-uring` as a parallel runtime co-resident with the multi-threaded
   tokio daemon runtime.
2. `tokio::task::spawn_blocking` wrapping the existing synchronous
   `submit_and_wait` paths.
3. Tokio's default mio/epoll reactor with the io_uring fd registered as
   an `AsyncFd`, driven by a third-party reactor adapter.
4. Keep io_uring strictly synchronous, called only from non-tokio
   threads, with `spawn_blocking` used solely as an inert handoff.

## 2. Current io_uring usage

The io_uring integration lives under `crates/fast_io/src/io_uring/`.
Every call site is synchronous and blocks the calling thread on a
`submit_and_wait`. Concrete entry points:

- **Disk batch ring (hottest path).** `IoUringDiskBatch::submit_fsync`
  at `crates/fast_io/src/io_uring/disk_batch.rs:250` calls
  `self.ring.submit_and_wait(1)`. The batch is created in
  `IoUringDiskBatch::try_new` at
  `crates/fast_io/src/io_uring/disk_batch.rs:65` via
  `config.build_ring()` and is owned by the disk-commit thread,
  spawned at `crates/transfer/src/disk_commit/thread.rs:53-56` with
  `thread::Builder::new().name("disk-commit".into())`. The batch is
  constructed inside `disk_thread_main` via `try_create_disk_batch`
  at `crates/transfer/src/disk_commit/thread.rs:74-92`.
- **Per-file writer ring.** `IoUringFileWriter` flushes via
  `self.ring.submit_and_wait(1)` at
  `crates/fast_io/src/io_uring/file_writer.rs:227` for single-SQE
  flushes and via `submit_write_batch` for batched writes
  (`crates/fast_io/src/io_uring/file_writer.rs:243-260`,
  `crates/fast_io/src/io_uring/batching.rs:108`,
  `crates/fast_io/src/io_uring/batching.rs:335`).
- **Per-file reader ring.** `IoUringFileReader` submits at
  `crates/fast_io/src/io_uring/file_reader.rs:138` and
  `crates/fast_io/src/io_uring/file_reader.rs:260`.
- **Registered buffer pools.** `RegisteredBufferGroup` drives bulk
  read/write batches at
  `crates/fast_io/src/io_uring/registered_buffers.rs:550` and
  `crates/fast_io/src/io_uring/registered_buffers.rs:670`, in both
  cases gating buffer lifetime on the synchronous return from
  `submit_and_wait`.
- **`linkat` / `renameat2`.** Atomic rename and link operations call
  `submit_and_wait(1)` at
  `crates/fast_io/src/io_uring/linkat.rs:190` and
  `crates/fast_io/src/io_uring/renameat2.rs:174`.
- **Shared session ring.** `SharedRing::submit_and_wait` at
  `crates/fast_io/src/io_uring/shared_ring.rs:308` co-locates a
  reader fd and writer fd on a single ring and is the foundation for
  the #1876 daemon socket I/O work.
- **Availability probe.** `is_io_uring_available()` at
  `crates/fast_io/src/io_uring/config.rs:155` is the gate every
  caller hits. The cached probe is sub-nanosecond after the first
  call (atomic `Relaxed` load,
  `crates/fast_io/src/io_uring/config.rs:156-167`).
- **Ring construction.** `IoUringConfig::build_ring` at
  `crates/fast_io/src/io_uring/config.rs:313` is the only place that
  enables SQPOLL (default off,
  `crates/fast_io/src/io_uring/config.rs:661-665`) and the
  `mmap_basis_active` interlock (`config.rs:315-329`).

None of these call sites runs on a tokio worker today. The disk-commit
thread is `std::thread`. The per-file readers and writers run on rayon
workers driven from the synchronous `core::session()` facade. The
shared session ring is currently a sync abstraction. No `async fn`
anywhere in the workspace calls `submit_and_wait` directly.

## 3. Crossing the async boundary

After phase 1 of #4186 lands, `AsyncDaemonListener::serve` at
`crates/daemon/src/daemon/async_session/listener.rs:180` runs the
accept loop on a multi-threaded tokio runtime. Each accepted
connection becomes a `tokio::spawn` task
(`listener.rs:212-243`). That task calls `handle_async_session` at
`crates/daemon/src/daemon/async_session/session.rs:119`, which in turn
calls into `core::session()`.

`core::session()` invokes the synchronous transfer pipeline. Every
rayon dispatch and every io_uring `submit_and_wait` reached from there
must not block a tokio worker, because tokio workers are
co-operatively scheduled and blocking one stalls the listener loop and
all other in-flight sessions on that worker. The bridge that fixes
this is `spawn_blocking`, fully specified in
`docs/design/spawn-blocking-bridge.md` (#4196). The same bridge that
guards rayon parallelism also guards io_uring `submit_and_wait` calls.

In other words: io_uring stays sync; the daemon goes async; the only
coordination between them is the `spawn_blocking` boundary inside
`core::session()`. The disk-commit thread keeps spawning as a plain
OS thread (`crates/transfer/src/disk_commit/thread.rs:53`), the rayon
pool keeps owning the per-file rings, and the tokio runtime sees none
of it. The only new requirement is that every entry into the sync
transfer pipeline from a tokio task goes through `spawn_blocking` or
`tokio::task::block_in_place` so the io_uring submissions reachable
beneath it never run on a tokio worker.

This split is identical to the rayon coordination story in #4196.
io_uring composes with the bridge for the same reason rayon does:
both work pools own their own threads, and the bridge only marks the
boundary.

## 4. Option A - `tokio-uring`

`tokio-uring` (crate 0.5.x) is an async io_uring driver that exposes
file and socket APIs through Rust futures. Adopting it inside the
async daemon would mean running two coexisting runtimes in the same
process: tokio's multi-threaded runtime for the listener and one
`tokio-uring` runtime per io_uring-driven session.

This costs more than it pays for in our specific setup:

- **Single-threaded only.** `tokio-uring::start` builds a
  `current_thread` tokio runtime that owns a single ring. Futures
  spawned inside it are `!Send` because they hold thread-local ring
  state. The multi-threaded daemon runtime spawns `Send` futures via
  `tokio::spawn`. Mixing them requires `LocalSet` and an OS thread per
  `tokio-uring` runtime, which recreates the `spawn_blocking` thread
  cost without sharing the blocking pool.
- **Two runtimes, two timer wheels, no shared work-stealing.** The
  multi-threaded runtime cannot steal work from the `tokio-uring`
  single-threaded runtime and vice versa. Connections that hand off to
  a `tokio-uring` driver lose access to the listener's worker pool for
  the duration of the transfer. Timer wakeups fire on whichever
  runtime registered them, so daemon-wide keepalive / shutdown logic
  must be plumbed across both. `docs/design/async-migration-plan.md`
  section "Runtime choice" explicitly forbids a second runtime in the
  workspace; adopting `tokio-uring` violates that rule.
- **Strict subset of features.** `tokio-uring` 0.5.x does not expose
  `IORING_REGISTER_BUFFERS`, `IORING_SETUP_SQPOLL`,
  `IORING_REGISTER_PBUF_RING`, `IORING_REGISTER_FILES`,
  `IORING_OP_LINKAT`, `IORING_OP_RENAMEAT`, or `IORING_OP_SEND_ZC`.
  Every one of those is in production use in `fast_io`
  (`registered_buffers.rs`, `config.rs:313`, `buffer_ring.rs`,
  `linkat.rs`, `renameat2.rs`, `iouring-send-zc.md`).
- **Buffer ownership rewrite.** `tokio-uring` uses the `BufResult<T,
  B>` pattern where the buffer is surrendered to the ring for the
  duration of every operation and returned alongside the result. Every
  existing call site in `fast_io::io_uring` would have to be rewritten
  to that idiom, including the registered-buffer slot pattern at
  `crates/fast_io/src/io_uring/registered_buffers.rs:525-550` that
  relies on `submit_and_wait` to bound buffer lifetime.

Net cost: a second runtime in-process, a strict feature regression,
and a rewrite of every io_uring consumer for no measurable throughput
gain. Reject.

## 5. Option B - `spawn_blocking` wrapper

Wrap each synchronous io_uring entry point in
`tokio::task::spawn_blocking`. The async daemon calls a thin async
shim, the shim hands the work to the blocking pool, the pool thread
performs `submit_and_wait`, the future resolves when the CQE arrives.

The objection is that this looks like it loses io_uring's main win.
The point of io_uring on the disk-commit path is to coalesce many
operations into one `io_uring_enter` syscall via `submit_and_wait(n)`
(`crates/fast_io/src/io_uring/batching.rs:108`,
`crates/fast_io/src/io_uring/batching.rs:335`), and to avoid a syscall
per operation entirely when SQPOLL is enabled
(`crates/fast_io/src/io_uring/config.rs:313-329`). If `spawn_blocking`
is invoked per io_uring operation, we add a tokio scheduler hop and a
blocking-pool thread-park around each `submit_and_wait` call, which
can dominate the cost of the syscall it is wrapping.

The objection is real but it answers itself. The right granularity
for `spawn_blocking` is not "per io_uring submission" but "per
session" - the same granularity the rayon bridge uses in #4196:

- Enter `spawn_blocking` once when the async session hands off to the
  sync transfer pipeline.
- Inside that single blocking task, the disk-commit thread is spawned
  (`crates/transfer/src/disk_commit/thread.rs:53`) and runs an
  arbitrary number of `submit_and_wait` calls without crossing back
  into tokio. The rayon workers and per-file rings stay entirely
  inside that blocking-task scope.
- The blocking task returns once the transfer completes. The async
  session awaits the `JoinHandle` and resumes.

With that granularity, the bridge cost is one `spawn_blocking` per
session, not one per syscall. The disk-commit batching at
`submit_write_batch` (`batching.rs:108`, up to `sq_entries` SQEs per
call) is preserved end-to-end. SQPOLL, registered buffers, fixed fds,
`linkat`, and `renameat2` keep working unchanged because no call site
in `fast_io::io_uring` is altered.

This is the design already implemented for the rayon side in #4196
and the design the existing `async-io-uring-impact.md` reached. It
preserves every io_uring optimization that the synchronous path
currently uses.

## 6. Option C - tokio default reactor + io_uring via `AsyncFd`

Tokio's default reactor is mio/epoll on Linux. The io_uring fd is
epoll-pollable, so a third-party driver could register the ring fd
with `tokio::io::unix::AsyncFd` and `await` CQ readiness instead of
calling `submit_and_wait`. The submission path would still be a
synchronous `ring.submission().push()` followed by `ring.submit()`,
but completion handling would be async.

This appears to give us "io_uring on tokio" without a second runtime.
It does not survive contact with the buffer ownership model:

- `ring.submit()` and `ring.submission()` take `&mut self`. The ring
  cannot be shared across tokio tasks without a mutex around every
  submission, which serialises the very parallelism that motivates
  io_uring on a multi-threaded runtime.
- Buffers handed to SQEs must remain valid until the corresponding
  CQE is reaped. A task cancellation at the `async_fd.readable().await`
  point drops the owning future, which drops the buffer slot
  (`RegisteredBufferSlot` returns to the free list in `Drop`,
  `crates/fast_io/src/io_uring/registered_buffers.rs`). The kernel
  still holds a pending operation that will write into freed or
  recycled memory: a use-after-free across the kernel boundary.
  `submit_and_wait` avoids this by definition because the thread does
  not unwind until the CQE has arrived.
- No measurable benefit for the disk-commit path: the disk-commit
  thread already exists, already submits batched SQEs
  (`batching.rs:108`), and is not contending with tokio for CPU. The
  epoll wakeup added by `AsyncFd` is a latency cost with no parallelism
  gained.
- The only place where async-driven completion would matter is daemon
  socket I/O (`docs/design/iouring-daemon-tcp.md`, #1876), and there
  the existing `IoUringSocketReader` / `IoUringSocketWriter` adapters
  already cover the in-process integration. Bringing tokio's reactor
  into the loop would split the ring between tokio's reactor and the
  socket adapters' own submission path with no clear ownership of the
  CQ ring.

No public third-party tokio reactor for io_uring is production-stable
at the time of writing. Building one ourselves would replicate
`tokio-uring`'s buffer ownership rewrite with extra cancellation-safety
risk. Reject.

## 7. Option D - keep io_uring strictly synchronous

Identical to Option B in mechanism but stricter in scope: prohibit
io_uring entry from any tokio context, full stop. Every io_uring call
site stays on `std::thread`-spawned threads (disk-commit) or rayon
workers, and the async daemon never directly references
`fast_io::io_uring`. Any need to drive io_uring from an async session
goes through the synchronous `core::session()` facade, which is in
turn invoked through `spawn_blocking` per #4196.

This is functionally what we already have. The only thing that changes
is the rule: we explicitly say io_uring will never be made async, even
for socket I/O in the daemon, so #1876's socket adapters keep their
own ring management and `AsyncFd` is never wired up.

The difference between D and B is whether `spawn_blocking` is the
sole boundary or one of several. We prefer to keep it explicit.

## 8. Recommendation

**Adopt Option B with Option D's scoping rule.**

- io_uring submissions stay synchronous in `fast_io::io_uring`. No call
  site under `crates/fast_io/src/io_uring/` becomes `async fn`. The
  cited submission points (`disk_batch.rs:250`, `file_writer.rs:227`,
  `file_writer.rs:243-260`, `file_reader.rs:138`, `file_reader.rs:260`,
  `registered_buffers.rs:550`, `registered_buffers.rs:670`,
  `linkat.rs:190`, `renameat2.rs:174`, `shared_ring.rs:308`,
  `batching.rs:108`, `batching.rs:335`) all keep their current
  signatures.
- The async daemon crosses into the sync transfer pipeline through
  `tokio::task::spawn_blocking` once per session, per #4196. Inside the
  blocking task, the disk-commit thread
  (`crates/transfer/src/disk_commit/thread.rs:53`) and the rayon
  workers run for the lifetime of the transfer without re-crossing the
  boundary.
- The shared session ring abstraction
  (`crates/fast_io/src/io_uring/shared_ring.rs:308`) keeps its sync
  `submit_and_wait` API. The #1876 daemon socket I/O work continues to
  use the existing `IoUringSocketReader` / `IoUringSocketWriter`
  adapters on dedicated threads, not on tokio workers.
- We never wire the io_uring fd into tokio's reactor and we never run
  a second async runtime. `tokio-uring` is forbidden in-process, in
  line with the "no second runtime" rule in
  `docs/design/async-migration-plan.md`.

Why this is the right shape for oc-rsync's workload:

1. **Workload is batch-oriented, not latency-bound.** Disk-commit
   submits up to `sq_entries` (default 64) SQEs per
   `submit_and_wait` call (`config.rs`, `batching.rs:108`). The cost
   of one `spawn_blocking` per session is amortised over hundreds to
   millions of SQEs in the same transfer.
2. **Buffer ownership stays trivially safe.** The synchronous
   `submit_and_wait` model is the only model under which the
   `RegisteredBufferGroup` checkout/return invariant
   (`registered_buffers.rs:550`) and the linkat / renameat2 stack-borrow
   pattern (`linkat.rs:190`, `renameat2.rs:174`) hold without
   reference counting or pinning.
3. **CLI stays runtime-free.** The CLI never spins up tokio; it calls
   the sync facade directly. Any async-only io_uring story would force
   tokio into the CLI for parity, which is a strict regression. Option
   B leaves CLI behaviour unchanged.
4. **Feature parity preserved.** SQPOLL (`config.rs:313`), registered
   buffers (`registered_buffers.rs`), fixed-fd registration,
   `IORING_OP_LINKAT`, `IORING_OP_RENAMEAT`, and PBUF_RING all stay
   wired. Options A and C would regress at least one of these.
5. **Predictable composition with rayon.** The same `spawn_blocking`
   boundary already documented for rayon in #4196 covers io_uring for
   free, because both pools live behind the same sync facade.

The migration cost is zero new code in `fast_io::io_uring`. The
contract is: the async daemon never reaches `submit_and_wait` without
crossing `spawn_blocking` first, and `spawn_blocking` is taken at
session granularity, not syscall granularity.

## 9. Cross-references

- #4186 - `docs/design/async-migration-plan.md`, async migration plan.
- #4196 - `docs/design/spawn-blocking-bridge.md`, `spawn_blocking`
  bridge for rayon work in the async daemon (the same bridge this
  evaluation reuses for io_uring).
- #4194 - `docs/design/async-ssh-evaluation.md`, async SSH transport
  evaluation; this document follows the same decision-shaped layout.
- #1935 - `docs/design/daemon-tokio-async-listener-impl.md`, async
  daemon listener implementation.
- #1876 - `docs/design/iouring-daemon-tcp.md`, io_uring socket I/O for
  daemon TCP.
- `docs/design/async-io-uring-impact.md` - prior multi-dimension
  evaluation; the recommendation here is the focused successor.
- `docs/design/iouring-session-ring-pool.md` - per-session ring pool
  shape this design composes with.
- `docs/design/io-uring-rayon-composition.md` - rayon-side composition
  policy for the native io_uring path.
