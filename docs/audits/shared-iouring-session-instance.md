# Shared io_uring instance across the transfer session

Tracking issue: oc-rsync task #1408. Branch: `docs/shared-iouring-session-1408`.

## Scope

Design how a single io_uring ring (or a small bounded pool of rings) should be
shared across the lifetime of an oc-rsync transfer session, instead of the
current mix of per-file rings (sender / generator read path) and a
single-thread-owned ring (disk-commit thread). The audit answers four
questions:

1. How is `io_uring::IoUring` constructed today across `fast_io`, and where
   does each instance's lifetime begin and end relative to the transfer
   session?
2. What is the cost of constructing rings per file vs reusing one ring across
   the whole session, both in setup overhead and in kernel resource
   pressure?
3. What design alternatives exist - one global ring, per-thread rings, a
   bounded session pool, or merging reader+writer onto a single ring with
   `IORING_OP_POLL_ADD` - and what are the contention and fallback
   trade-offs of each?
4. What constraints does the existing implementation impose on any sharing
   scheme: registered-buffer-group `bgid` namespacing, `RegisteredBufferGroup`
   `Drop` ordering, the SQPOLL fallback path, and the standard-I/O fallback
   chain?

This is a docs-only audit. No code, no `Cargo.toml` change. The output
informs the implementation that will land under follow-up tasks
(#1409, #1937, #1097, #1874, #1872, #1060, #1410).

## Source files inspected

All paths are repository-relative.

- `crates/fast_io/src/io_uring/mod.rs` (public surface, fallback wiring).
- `crates/fast_io/src/io_uring/config.rs` (`IoUringConfig`, `build_ring`,
  SQPOLL setup, kernel probe).
- `crates/fast_io/src/io_uring/file_reader.rs` (`IoUringReader::open`).
- `crates/fast_io/src/io_uring/file_writer.rs` (`IoUringWriter::create`,
  `from_file`, `with_ring`, `create_with_size`).
- `crates/fast_io/src/io_uring/disk_batch.rs` (`IoUringDiskBatch`, the only
  existing session-level ring reuse).
- `crates/fast_io/src/io_uring/registered_buffers.rs`
  (`RegisteredBufferGroup`, the `Drop` ordering invariant, kernel
  `IORING_REGISTER_BUFFERS` cleanup).
- `crates/fast_io/src/io_uring/buffer_ring.rs` (`BufferRing` /
  `BufferRingConfig`, `bgid` namespace, PBUF_RING).
- `crates/fast_io/src/io_uring/socket_factory.rs` /
  `socket_reader.rs` / `socket_writer.rs` (per-socket ring construction).
- `crates/fast_io/src/io_uring/file_factory.rs`
  (`IoUringReaderFactory`, `IoUringWriterFactory`).
- `crates/fast_io/src/lib.rs` (`IoUringPolicy` enum and probe re-exports).
- `crates/transfer/src/disk_commit/thread.rs` (the only place a ring outlives
  a single file today).
- `crates/transfer/src/transfer_ops/response.rs` (per-file `writer_from_file`
  call site on the receiver write path).
- `crates/transfer/src/generator/mod.rs` (per-file `reader_from_path` call
  site on the generator / source-read path).
- `crates/protocol/src/` (referenced to confirm the protocol crate has zero
  io_uring dependency).

## TL;DR

- Today the codebase has **one** session-scoped ring -
  `IoUringDiskBatch` on the disk-commit thread - and **N** per-file rings,
  one created and destroyed for every file the receiver writes
  (`writer_from_file`) or the generator reads (`reader_from_path`).
- Per-file ring construction does an `mmap` of the SQ/CQ rings, an
  `io_uring_setup(2)`, an `IORING_REGISTER_FILES` (one fd) and an
  `IORING_REGISTER_BUFFERS` (eight 64 KiB pinned buffers by default). On a
  100 K-small-file transfer this is roughly a million syscalls and
  ~50 GiB of pinned-buffer churn, almost all of which can be avoided by
  reusing a single ring.
- The right design for #1408 is a **bounded session pool of long-lived
  rings**, leased per task and returned on drop, with the disk-commit ring
  staying as the canonical writer endpoint. This is the same shape as
  `BufferPool` / `PooledBuffer` (`crates/engine/src/local_copy/...`),
  reused for SQE-owning state.
- Sharing is purely a `fast_io` internal concern. The wire protocol does
  not see io_uring at all (`crates/protocol/src/` has zero `io_uring`
  references), so #1408 introduces no protocol-visible behaviour change.

## 1. Status quo: how io_uring is instantiated today

Every `IoUring` construction site in the workspace lives under
`crates/fast_io/`. There are three categories.

### 1.1 Per-file rings on the receiver write path

`fast_io::writer_from_file` is called once per inbound file from
`crates/transfer/src/transfer_ops/response.rs:108`:

```rust
// transfer_ops/response.rs:107-108
let writer_capacity = adaptive_writer_capacity(target_size);
let mut output = fast_io::writer_from_file(file, writer_capacity, ctx.config.io_uring_policy)?;
```

The factory then builds a fresh ring inside the function
(`crates/fast_io/src/io_uring/mod.rs:140-188`):

```rust
// io_uring/mod.rs:144-161
pub fn writer_from_file(
    file: File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdWriter> {
    let config = IoUringConfig::default();

    match policy {
        crate::IoUringPolicy::Auto => {
            if is_io_uring_available() {
                if let Ok(ring) = config.build_ring() {
                    let fixed_fd_slot =
                        try_register_fd(&ring, file.as_raw_fd(), config.register_files);
                    return Ok(IoUringOrStdWriter::IoUring(IoUringWriter::with_ring(
                        file,
                        ring,
                        buffer_capacity,
                        config.sq_entries,
                        fixed_fd_slot,
                    )));
                }
            }
            ...
```

`IoUringWriter::with_ring` further allocates and registers a private
`RegisteredBufferGroup` of eight buffers
(`crates/fast_io/src/io_uring/file_writer.rs:110-131`):

```rust
// file_writer.rs:110-131
pub(super) fn with_ring(
    file: File,
    ring: RawIoUring,
    buffer_capacity: usize,
    sq_entries: u32,
    fixed_fd_slot: i32,
) -> Self {
    // Attempt buffer registration with default count.
    let registered_buffers = RegisteredBufferGroup::try_new(&ring, buffer_capacity, 8);

    Self {
        ring,
        file,
        ...
    }
}
```

The same shape repeats for explicit constructors at
`crates/fast_io/src/io_uring/file_writer.rs:52` (`IoUringWriter::create`),
`:80` (`from_file`), and `:141` (`create_with_size`). Each constructor calls
`config.build_ring()` and a fresh `RegisteredBufferGroup::try_new`.

### 1.2 Per-file rings on the generator / source-read path

`reader_from_path` is invoked from
`crates/transfer/src/generator/mod.rs:728` for every source file at or
above the 1 MiB threshold:

```rust
// generator/mod.rs:721-734
const IO_URING_READ_THRESHOLD: u64 = 1024 * 1024;

if file_size >= IO_URING_READ_THRESHOLD
    && self.config.write.io_uring_policy != fast_io::IoUringPolicy::Disabled
{
    match fast_io::reader_from_path(path, self.config.write.io_uring_policy) {
        Ok(r) => return Ok(Box::new(r)),
        Err(_) => {
            // Fall through to standard BufReader on io_uring failure
        }
    }
}
```

`reader_from_path` -> `IoUringReader::open` -> `config.build_ring()`
(`crates/fast_io/src/io_uring/file_reader.rs:60`) and another
`RegisteredBufferGroup::try_new` (`file_reader.rs:73-81`).

So on the receive path a single transfer of N files creates 2 N rings
when both reader and writer cross their respective io_uring thresholds:
one writer ring per file plus one reader ring per file >= 1 MiB.

### 1.3 The session-scoped disk-commit ring

The only existing ring whose lifetime spans the whole transfer is
`IoUringDiskBatch`, owned by the disk-commit thread spawned in
`crates/transfer/src/disk_commit/thread.rs:124-133`:

```rust
// disk_commit/thread.rs:124-133
fn disk_thread_main(
    file_rx: spsc::Receiver<FileMessage>,
    result_tx: spsc::Sender<io::Result<CommitResult>>,
    buf_return_tx: spsc::Sender<Vec<u8>>,
    config: DiskCommitConfig,
) {
    let mut write_buf = Vec::with_capacity(WRITE_BUF_SIZE);
    let mut disk_batch = try_create_disk_batch(config.io_uring_policy);

    log_io_uring_status(config.io_uring_policy, disk_batch.is_some());
```

`IoUringDiskBatch::new` (`crates/fast_io/src/io_uring/disk_batch.rs:65-79`)
calls `config.build_ring()` once and reuses it across every
`begin_file -> write_data -> commit_file` cycle:

```rust
// disk_batch.rs:65-79
pub fn new(config: &IoUringConfig) -> io::Result<Self> {
    let ring = config.build_ring()?;
    Ok(Self {
        ring,
        config: config.clone(),
        current_file: None,
        buffer: vec![0u8; config.buffer_size.max(DEFAULT_BUFFER_CAPACITY)],
        buffer_pos: 0,
    })
}
```

`begin_file` re-registers the new fd into the ring's fixed-file table
(`disk_batch.rs:108-114`); `unregister_fd` releases it on commit
(`disk_batch.rs:269-275`). `Drop` flushes and finalises the active file
(`disk_batch.rs:297-303`). This is the prior art - and the foundation -
for the design proposed below: completed under #1409.

### 1.4 Probe rings

`config::check_io_uring_reason` (`crates/fast_io/src/io_uring/config.rs:271`)
allocates a 4-entry ring once at startup to detect whether
`io_uring_setup(2)` is available, and tears it down immediately.
`registered_buffers.rs:729-1301` and `buffer_ring.rs:723-787` each open
short-lived rings inside `#[cfg(test)]` blocks. Neither contributes to
the per-transfer ring count.

### 1.5 Construction-cost summary

Default ring config (`crates/fast_io/src/io_uring/config.rs:329-342`):

```rust
// config.rs:329-342
impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            sq_entries: 64,
            buffer_size: 64 * 1024, // 64 KB
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
            register_buffers: true,
            registered_buffer_count: 8,
        }
    }
}
```

Per ring, `config.build_ring()` (`config.rs:381-396`) issues
`io_uring_setup(2)` (which allocates two kernel-mapped ring buffers and
gives the process an mmap'd SQ/CQ region), and the typical
`with_ring`/`open` path then issues `IORING_REGISTER_FILES` (1 fd) and
`IORING_REGISTER_BUFFERS` (8 buffers x 64 KiB = 512 KiB pinned per ring).
On `Drop`, the ring fd close releases the kernel pinning and the
SQ/CQ mmaps go away.

So one open-write-close cycle of a 4 KiB file pays:

- 1 x `io_uring_setup`, 1 x `IORING_REGISTER_FILES`, 1 x
  `IORING_REGISTER_BUFFERS` on construction.
- 1 SQE + 1 CQE on the actual write (or 2 SQE/CQE pairs when fsync is
  requested via the disk-commit chain).
- 1 x ring fd close on drop (releases kernel state).

That is at least 4 syscalls of pure ring lifecycle for every 4-KiB file,
plus 512 KiB of pinned-page alloc/dealloc churn. Sharing eliminates
all four of the lifecycle syscalls and the page churn for every file
after the first.

## 2. The case for sharing

### 2.1 Why per-file rings are expensive

`man 2 io_uring_setup` and `liburing` documentation make the costs explicit:

- `io_uring_setup(2)` allocates two kernel pages (SQ and CQ) per ring
  plus an `io_sq_data` struct, then `mmap`s them into the user
  process. On modern kernels each setup is dominated by `kmalloc` of
  the per-ring `io_ring_ctx` (`fs/io_uring.c:io_ring_ctx_alloc`).
- `IORING_REGISTER_BUFFERS` calls `get_user_pages_fast` on every
  buffer in the iovec - 8 buffers x 64 KiB = 128 4-KiB pages pinned
  per ring at default config. The pages are not released until the
  ring fd is closed (see the doc comment in
  `crates/fast_io/src/io_uring/registered_buffers.rs:1-67`).
- `IORING_REGISTER_FILES` allocates a kernel `fixed_file_table` and
  copies the fd in.
- Every ring also consumes one process file descriptor and one entry in
  the kernel's file table, so per-file rings linearly grow `fd_table`
  pressure during a transfer.

The doc block at the top of `registered_buffers.rs` is explicit about
the kernel-side cleanup story (`registered_buffers.rs:30-37`):

```rust
// registered_buffers.rs:30-37
//! 1. `RawIoUring::Drop` closes the ring fd first, releasing the kernel's
//!    pinning of the registered buffer pages.
//! 2. `RegisteredBufferGroup::Drop` then deallocates the user-side memory
//!    backing those buffers.
```

That cleanup happens for every per-file ring. On a 100 K small-file
transfer (the canonical workload behind #1410), this is 100 K x
{`io_uring_setup`, `register_files`, `register_buffers`,
`unregister_buffers`(implicit), `close`} = ~500 K lifecycle syscalls
that produce no actual I/O.

### 2.2 What workloads pay off the most

#1410 (Benchmark per-file vs shared io_uring ring on 100K small files)
identifies the canonical workload: 100 K files of a few KiB each. With
default config, per-file ring construction dominates total runtime
because the actual data write is a single `WRITE_FIXED` SQE returning
in one `submit_and_wait` cycle. Sharing one ring across the whole
batch reduces ring lifecycle to O(1).

Less obvious wins:

- 1 GiB single-file transfers gain little from sharing (the ring cost
  amortises naturally over many SQEs), but they are the workload
  most exposed to **contention** on a single shared SQ-tail under
  parallel writers (see 3.4 below).
- Mixed workloads (build trees, source repos) sit between the two and
  benefit from sharing roughly proportional to the ratio of small
  files to total data.

#1410 will produce the curve that decides where the sharing threshold
lives. The benchmark scripts already in tree -
`scripts/benchmark.sh`, `scripts/benchmark_hyperfine.sh`,
`scripts/benchmark_100k.sh`, and the container-based
`scripts/run_arch_benchmark.py` - should be reused with
`--no-io-uring` and `--io-uring=enabled` toggles on identical
fixtures.

### 2.3 When sharing becomes a contention bottleneck

A shared ring has a **single SQ tail** and a **single CQ head**.
io_uring's submission queue is a SPSC ring, and the
`io_uring::IoUring::submission_shared()` API takes `&mut self` (or a
`Mutex` in the only `Send`-safe case), so multi-producer submission
needs an explicit mutex. Under high producer parallelism this becomes a
serialisation point.

Three mitigations exist:

1. **Bound the producer count.** A pool of N rings, each owned by one
   submitter, gives N x SQE_DEPTH parallel SQEs without any
   cross-thread synchronisation. This is the design proposed in #1937
   (per-session ring pool).
2. **Defer-taskrun + SQPOLL.** With `IORING_SETUP_DEFER_TASKRUN`, CQE
   reaping moves off the submission thread, eliminating the single-
   reader bottleneck on the CQ side. This interacts with the SQPOLL
   evaluation in `docs/audits/iouring-socket-sqpoll-defer-taskrun.md`.
3. **Per-thread rings.** Cheapest design model: each rayon worker
   owns its ring. Eliminates contention entirely at the cost of N rings
   (where N = `rayon::current_num_threads()`).

## 3. Design space

Four shapes are on the table.

### 3.1 Single global ring per process

One `IoUring`, statically initialised behind a `Mutex` or
`OnceLock<Mutex<IoUring>>`. Pros: simplest mental model. Cons:

- Every submit and every CQE drain needs the mutex, defeating the
  point of an async I/O API.
- Registered buffers and registered files become a single global
  resource pool, so file rotation (`begin_file` / `unregister_files`)
  serialises across the entire transfer.
- The single SQ depth caps total in-flight I/O at
  `IoUringConfig::sq_entries` (default 64) regardless of available
  parallelism.

This is acceptable for the disk-commit thread (which already runs
single-threaded by construction - see
`crates/fast_io/src/io_uring/disk_batch.rs:42-44`) but unacceptable for
the receiver write path or the generator read path that wants to
parallelise across rayon workers.

### 3.2 Per-thread ring

Each rayon worker (or each io_uring "owner thread") holds a
`RefCell<IoUring>` and uses it directly. No locking, no contention. Used
by io_uring servers like `tokio-uring` and `glommio`.

Pros: zero contention, natural ergonomics, predictable resource use.
Cons: ring count = `rayon::current_num_threads()`. On a 64-core box that
is 64 SQ/CQ rings, 64 fixed-file tables, 64 `RegisteredBufferGroup`s
(8 x 64 KiB each = 32 MiB total of pinned pages). Acceptable; in
practice this is what the kernel was tuned for (per-thread submission
is the io_uring "happy path").

The rayon thread pool is already centralised
(`crates/fast_io/src/parallel.rs:159` and `:217`), so a thread-local
`OnceCell<IoUring>` initialised on first use is a small change.

### 3.3 Bounded session pool of N rings (recommended)

A pool object lives at the transfer-session level (the same lifetime
boundary as `CoreConfig` and `TransferConfig`). It owns up to N rings,
each constructed lazily on first lease. Submitters acquire a
`PooledIoUring` lease for the duration of one logical task (one file,
one chunk batch, one fsync), and the lease returns the ring to the
pool on drop.

This is the same shape as the existing `BufferPool` / `PooledBuffer`
pattern in `crates/engine/src/buffer_pool/` and matches the prior art
of `IoUringDiskBatch` (one ring + a sequence of `begin_file` /
`commit_file` cycles - see
`crates/fast_io/src/io_uring/disk_batch.rs:103-189`).

Pros:

- N is bounded and tunable. Default
  `min(rayon::current_num_threads(), 8)` keeps the pinned-page budget
  predictable.
- Ring lifecycle cost is paid once per ring, not once per file.
- Each ring still has a single owner during a lease, so the SQE
  push path stays lock-free inside the lease.
- Pool exhaustion falls back to the standard-I/O path - no caller
  ever blocks on ring availability.

Cons:

- `IORING_REGISTER_FILES` and `IORING_REGISTER_BUFFERS` state is
  per-ring, so `begin_file` semantics from `IoUringDiskBatch` need to
  be generalised: the pool entry must re-register the new fd on
  every lease and unregister it on return.
- Cross-ring telemetry needs aggregation.

The lease/return contract:

```text
// Conceptual API; not yet implemented. Naming will be finalised in #1408.
pub struct IoUringPool { /* up to N rings */ }

impl IoUringPool {
    /// Reserves a ring for a single task. Returns `None` when the pool is
    /// exhausted; the caller must fall back to standard I/O.
    pub fn lease(&self) -> Option<PooledIoUring<'_>>;
}

pub struct PooledIoUring<'a> { /* lease */ }

impl Drop for PooledIoUring<'_> {
    /// Unregisters any per-task fd and returns the ring to the pool.
}
```

The disk-commit thread keeps its dedicated `IoUringDiskBatch` (it is
single-threaded by design). The pool serves the multi-threaded paths
on the receiver write fan-out and the generator read fan-out.

### 3.4 Reader + writer on a shared ring (#1874)

#1874 (Merge io_uring reader+writer onto shared ring with poll_add)
proposes mixing `IORING_OP_READ_FIXED` and `IORING_OP_WRITE_FIXED` on a
single ring. The current code keeps reader and writer rings separate
(`crates/fast_io/src/io_uring/file_reader.rs:30-41` vs
`file_writer.rs:24-42`). Sharing them on one ring requires:

- Distinguishing read CQEs from write CQEs by `user_data` payload (the
  io_uring crate already supports a 64-bit user_data per SQE).
- Using `IORING_OP_POLL_ADD` to multiplex socket readiness alongside
  file I/O so the network thread can park on the same CQ.
- Careful `bgid` allocation: `READ_FIXED` consumes from one buffer
  group while `WRITE_FIXED` references a separately registered group.
  Both groups live in the same ring's bgid namespace
  (`crates/fast_io/src/io_uring/buffer_ring.rs:147-152`):

```rust
// buffer_ring.rs:147-152
/// Buffer group ID for this ring.
///
/// SQEs reference this group ID to select buffers from this ring.
/// Multiple rings can coexist with different group IDs.
pub bgid: u16,
```

Sharing is desirable but orthogonal to #1408. The session pool design
should not preclude #1874; specifically, the pool should permit a ring
to host both a reader-side `RegisteredBufferGroup` and a writer-side
`RegisteredBufferGroup` simultaneously, as long as their `bgid` values
do not collide. See section 4.1 below.

### 3.5 SQPOLL implications

`IORING_SETUP_SQPOLL` spawns one kernel thread per ring whose only job
is to poll the SQ tail
(`crates/fast_io/src/io_uring/config.rs:381-396`):

```rust
// config.rs:381-396
pub(crate) fn build_ring(&self) -> io::Result<RawIoUring> {
    if self.sqpoll {
        let mut builder = io_uring::IoUring::builder();
        builder.setup_sqpoll(self.sqpoll_idle_ms);
        match builder.build(self.sq_entries) {
            Ok(ring) => return Ok(ring),
            Err(_) => {
                // SQPOLL requires CAP_SYS_NICE on most kernels. Record
                // the fallback so callers can surface it in diagnostics.
                SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
            }
        }
    }
    RawIoUring::new(self.sq_entries)
        .map_err(|e| io::Error::other(format!("io_uring init failed: {e}")))
}
```

Per-file SQPOLL would mean spawning a kernel thread per file. That is
absurd; it is the strongest argument for sharing. Per-thread SQPOLL is
acceptable. The session-pool design pairs naturally with SQPOLL: at
most N kernel poller threads for N pool rings, regardless of file
count.

The companion audits cover the privilege requirements:
`#1621` (CAP_SYS_NICE check) and `#1622` (SQPOLL fallback log).

## 4. Constraints from the existing implementation

### 4.1 Registered-buffer-group bgid namespace

`bgid` is a `u16` per ring (`crates/fast_io/src/io_uring/buffer_ring.rs:151`,
quoted in 3.4 above). The namespace is per-ring, not per-process. So:

- A single ring can host up to ~64 K distinct buffer groups, but the
  practical cap is far lower because each group registers buffers via
  `IORING_REGISTER_BUFFERS` and the kernel default cap is 1024
  registered buffers per ring (`crates/fast_io/src/io_uring/registered_buffers.rs:78-80`):

```rust
// registered_buffers.rs:76-80
/// Maximum number of buffers that can be registered with io_uring.
///
/// The kernel typically allows up to 1024 registered buffers. We cap at this
/// limit to avoid kernel rejections.
const MAX_REGISTERED_BUFFERS: usize = 1024;
```

- A shared ring serving M concurrent leases needs M distinct
  `bgid` values to keep buffer groups disjoint (one group per
  reader-style task, plus one per writer-style task). The pool entry
  must allocate `bgid` slots from a per-ring `u16` allocator, returned
  on lease drop. #2044 (bgid exhaustion bound) tracks the audit of
  this allocator.

### 4.2 RegisteredBufferGroup Drop ordering

`RegisteredBufferGroup` does not hold a reference to the ring it was
registered against. The doc comment at the top of `registered_buffers.rs`
documents the invariant explicitly
(`crates/fast_io/src/io_uring/registered_buffers.rs:18-49`):

```rust
// registered_buffers.rs:18-49
//! # Drop ordering and the ring fd
//!
//! [`RegisteredBufferGroup`] does not hold a reference to the [`RawIoUring`]
//! instance it was registered with. This is intentional: the kernel
//! automatically releases the pinned user pages when the ring fd is closed
//! ...
//! Owners of both a `RawIoUring` and a `RegisteredBufferGroup` (such as
//! `IoUringReader` and `IoUringWriter`) MUST declare the ring field BEFORE
//! the `RegisteredBufferGroup` field. Rust drops fields in declaration
//! order, so this ensures:
//!
//! 1. `RawIoUring::Drop` closes the ring fd first, releasing the kernel's
//!    pinning of the registered buffer pages.
//! 2. `RegisteredBufferGroup::Drop` then deallocates the user-side memory
//!    backing those buffers.
```

For a session pool the situation inverts: the ring outlives many
buffer groups, so each `RegisteredBufferGroup` must be **explicitly
unregistered** when its lease is dropped, otherwise the kernel keeps
the pinning across leases. The existing
`RegisteredBufferGroup::unregister` helper (referenced in the same doc
block, lines 47-49) is the right hook. The pool's `Drop` then closes
the ring fd, which the kernel will reject if any buffers are still
registered - so unregistration must happen on lease return, not on
ring drop.

### 4.3 Fallback chain

The fallback story is documented in `crates/fast_io/src/io_uring/mod.rs:72-80`:

```rust
// io_uring/mod.rs:72-80
//! # Fallback chain
//!
//! Each layer degrades independently so that io_uring features are best-effort:
//!
//! - **Ring creation**: SQPOLL ring -> regular io_uring ring -> standard buffered I/O.
//!   Factory types handle the final fallback to `BufReader`/`BufWriter`.
//! - **Buffer registration**: registered (`READ_FIXED`/`WRITE_FIXED`) -> regular
//!   (`Read`/`Write`) opcodes. Silent fallback on registration failure.
```

Plus the integration test in `crates/fast_io/tests/io_uring_probe_fallback.rs:23-30`:

```rust
// io_uring_probe_fallback.rs:23-30
#[test]
fn is_io_uring_available_is_idempotent() {
    let first = is_io_uring_available();
    let second = is_io_uring_available();
    let third = is_io_uring_available();
    assert_eq!(first, second);
    assert_eq!(second, third);
}
```

For a session pool, the fallback chain becomes:

1. Try to allocate a pool entry (lazy `build_ring` on first use).
2. If `build_ring` fails, mark the slot as "permanently unavailable"
   to avoid retry storms, and let the caller fall back to `BufReader`
   / `BufWriter` via `IoUringOrStdReader::Std` /
   `IoUringOrStdWriter::Std`.
3. If the pool is exhausted (all leases held), return `None` to the
   caller. The caller falls back to standard I/O for that one task,
   not the whole transfer.

This preserves the "io_uring is best-effort" contract that
`IoUringPolicy::Auto` already promises in `crates/fast_io/src/lib.rs:398-417`.

### 4.4 The disk-commit ring stays as-is

`IoUringDiskBatch` is intentionally `!Send` and `!Sync`
(`crates/fast_io/src/io_uring/disk_batch.rs:42-44`):

```rust
// disk_batch.rs:42-44
/// # Thread Safety
///
/// This type is not `Send` or `Sync` - it is designed for single-threaded use
/// on the dedicated disk commit thread.
```

It must not be merged into the multi-threaded session pool. Instead,
the pool covers the *parallel* paths (sender reads, generator
metadata I/O, future delta-apply readers), and the disk-commit thread
keeps its private long-lived ring.

### 4.5 IoUringPolicy must not change

`IoUringPolicy` is the user-facing contract
(`crates/fast_io/src/lib.rs:397-417`), wired through
`crates/transfer/src/config/mod.rs:57-68` and surfaced on the CLI. The
session-pool design must behave correctly under all three values:

- `Auto`: pool initialises lazily; entries that fail `build_ring`
  fall back to `Std`.
- `Enabled`: pool initialises eagerly with at least one entry; failure
  to build any ring is a hard error (mirroring
  `crates/fast_io/src/io_uring/mod.rs:167-173`).
- `Disabled`: pool is never constructed; all callers go through the
  `Std` path.

## 5. Wire-protocol neutrality

io_uring is a private kernel API. Sharing or not sharing a ring has no
observable effect on the bytes that go on the wire. The protocol crate
confirms this: a workspace search for `io_uring` or `IoUring` in
`crates/protocol/src/` returns no matches. The protocol layer does not
import `fast_io` at all - it depends only on `checksums`, `filters`,
`compress`, and `bandwidth` (see the dependency graph in `CLAUDE.md`).

This is consistent with `feedback_no_wire_protocol_features.md`:
sharing rings is a `fast_io`-internal optimisation, not a wire feature.
No protocol-version bump, no capability flag, no `MSG_*` extension.

## 6. Test plan

Three layers, mapping to the trackers below.

### 6.1 Microbenchmarks (#1410)

Per-file vs shared ring on:

- `100K_smallfiles`: 100 K x 4 KiB files in a flat directory. Reuses
  fixture from `scripts/benchmark_100k.sh`.
- `1GB_singlefile`: one 1 GiB file. Reuses `scripts/benchmark_1gb.sh`.
- `mixed_buildtree`: oc-rsync's own `target/` directory after a
  release build (~10 K files, mixed sizes).

Run each fixture three ways: `--no-io-uring`, default policy with
per-file rings (current `master`), default policy with the new pool.
Compare wall-clock and `getrusage` syscall counts. The expected curve:
sharing wins at 100 K small, ties at 1 GiB, wins moderately on
mixed.

Existing scripts to extend:

- `scripts/benchmark.sh` and `scripts/benchmark_hyperfine.sh` for
  baseline / pool comparison.
- `scripts/benchmark_remote.sh` for SSH transport variant (where the
  network-side cost dominates and ring lifecycle is less load-bearing).
- `scripts/run_arch_benchmark.py` for the canonical Arch container
  run that produces the chart attached to release notes.

### 6.2 Functional tests

Per `crates/fast_io/tests/io_uring_probe_fallback.rs:1-8`:

```rust
// io_uring_probe_fallback.rs:1-8
//! Integration tests for the io_uring runtime probe and fallback chain.
//!
//! These tests verify that the public probe API and the policy-driven
//! reader/writer factories degrade gracefully on systems that do not
//! support io_uring - either because the kernel is older than 5.6, the
//! syscall is blocked by seccomp/container policy, or the platform is
//! not Linux at all.
```

Add tests for:

- **Pool lifecycle.** All N entries can be leased and returned;
  Drop order matches the documented invariant in
  `registered_buffers.rs:18-49`; no buffer pinning leaks across leases.
- **Pool exhaustion.** When all N slots are leased, the
  `(N+1)`-th lease attempt returns `None`, and the caller falls back
  to `BufReader`/`BufWriter` without error.
- **Build_ring failure.** An entry whose first `build_ring` returns
  `Err` is marked permanently unavailable, and subsequent
  `lease` calls do not retry the failing setup.
- **SQPOLL fallback inside the pool.** `sqpoll_fell_back()`
  (`crates/fast_io/src/io_uring/config.rs:44-47`) reports correctly
  even when fallback happens inside a pooled ring.
- **bgid recycling.** A ring that has hosted M leases over the
  transfer ends with bgid 0 still allocatable (no leak).

Tests should reuse the `EnvGuard` and `setup_test_dirs` patterns
already used elsewhere in the workspace; the io_uring tests must
pre-check `is_io_uring_available()` and skip cleanly on non-Linux or
container hosts.

### 6.3 Interop and stress

- The full `tools/ci/run_interop.sh` against rsync 3.0.9 / 3.1.3 /
  3.4.1 must continue to pass on Linux with the pool active. The
  audit confirms there is no protocol-visible change, so no new
  interop variants are needed.
- The `rsync-profile` container is the right place to run a
  stress test that opens many concurrent file rotations and checks
  for fd leaks via `/proc/self/fd` count (see
  `feedback_container_debug_endpoint.md`).

## 7. Recommendation

Stage the work in four steps:

1. **Foundation (done): #1409.** `IoUringDiskBatch` is the proof-of-
   concept that one ring can outlive many files. Keep it as the
   disk-commit endpoint; do not generalise it.
2. **Session pool (#1408 / this audit / #1937).** Introduce
   `fast_io::IoUringPool` with the lease/return contract sketched in
   3.3. Fold `writer_from_file` and `reader_from_path` to lease from
   the pool when present, fall back to per-file rings only when no
   pool was supplied (covers the test-only path and the warm-up
   probe). The pool ownership lives on the
   `transfer::TransferConfig` so it crosses both directions of the
   session.
3. **Concurrent-files extension (#1060).** Once the pool exists and
   benchmarks confirm it is at least neutral on `1GB_singlefile`,
   extend it to the parallel file fan-out the receiver and generator
   already perform. This is a follow-up; do not bundle it with #1408.
4. **Reader+writer merge (#1874).** Independent of the pool. With
   per-ring `bgid` allocation in place
   (`crates/fast_io/src/io_uring/buffer_ring.rs:147-152`), a single
   leased ring can host both a `READ_FIXED` group and a
   `WRITE_FIXED` group. This unlocks the
   `IORING_OP_POLL_ADD` work in #1874 and the `submit_and_wait` fix
   in #1872 without further infrastructure.

Constraints that must hold across all four steps:

- **No wire-protocol changes.** This is mandated by
  `feedback_no_wire_protocol_features.md` and confirmed by the empty
  search for `io_uring` in `crates/protocol/src/`.
- **All unsafe code stays in `fast_io`.** Per
  `feedback_unsafe_code_policy.md` and the unsafe-code policy in
  `CLAUDE.md`. The pool is a `fast_io` concern; `transfer` and
  `core` only see `Send`-safe handles.
- **Fallback chain unchanged from the user's perspective.**
  `IoUringPolicy::Auto` continues to mean "best-effort"; the only
  behaviour change is faster best-effort.
- **No silent regressions on non-Linux.** The io_uring stub at
  `crates/fast_io/src/io_uring_stub.rs` must mirror the new pool API
  with a no-op pool that always returns `None` from `lease`, so
  callers degrade to standard I/O without `cfg` plumbing leaking
  out of `fast_io`.

#1410 is the gate: if the 100K-small-files benchmark does not show a
clear win with the pool active, the implementation is not ready to
land. Until then the work stays staged on a feature branch.

## References

- `man 2 io_uring_setup` - ring lifecycle, SQ/CQ mmap regions.
- `man 2 io_uring_register` - `IORING_REGISTER_FILES`,
  `IORING_REGISTER_BUFFERS`, `IORING_REGISTER_PBUF_RING`.
- `man 7 io_uring` - opcode reference and fallback semantics.
- `liburing` examples under `examples/` in the upstream tree -
  reference for the lease/return idiom and SQPOLL setup.
- Linux kernel: `fs/io_uring.c:io_ring_ctx_alloc`,
  `io_uring/kbuf.c:io_register_pbuf_ring`,
  `fs/io_uring.c:io_sqe_buffers_unregister`.
- oc-rsync: `crates/fast_io/src/io_uring/disk_batch.rs`
  (in-tree prior art for session-level ring reuse, completed under
  #1409).
- oc-rsync audits already in `docs/audits/`:
  `disk-commit-iouring-batching.md`, `iouring-pbuf-ring.md`,
  `iouring-socket-sqpoll-defer-taskrun.md`,
  `mmap-iouring-co-usage.md`, `io-uring-bgid-namespace.md`,
  `io-uring-adaptive-buffer-sizing.md`. The session-pool design is
  the cross-cutting consumer of the constraints those audits
  enumerate.
