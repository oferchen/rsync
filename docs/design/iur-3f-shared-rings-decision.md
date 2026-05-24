# IUR-3.f: keep one-shot probes and the disk-commit ring shared

Tracking task: **IUR-3.f**. This is a decision record. No `.rs` files
change. The companion design notes are:

- `docs/design/io-uring-shared-ring-audit.md` - IUR-1 inventory that
  identified the per-thread-vs-shared split.
- `docs/design/iur-2-per-thread-rings.md` - IUR-2 hybrid layout. Section
  1.1 itemises the rows this record formalises as "shared".
- IUR-3.a..e (PRs #4793, #4804, #4807, #4806, #4811) - the per-thread
  migration of `file_writer`, `file_reader`, `socket_writer`, and the
  per-thread `BgidLease`.

## 1. Decision

Two ring categories stay on a single process-wide / per-session ring and
are **explicitly excluded** from the per-thread migration:

| Category | Sites | Topology kept |
|----------|-------|---------------|
| One-shot capability probes | `crates/fast_io/src/io_uring/linkat.rs:113,181`, `crates/fast_io/src/io_uring/renameat2.rs:76,165`, `crates/fast_io/src/io_uring/statx.rs:138,223,305` | One `io_uring::IoUring::new(2)` per probe site, result cached in process-wide `OnceLock<bool>` |
| Disk-commit singleton | `crates/fast_io/src/io_uring/disk_batch.rs:45` (`IoUringDiskBatch`), spawned by `crates/transfer/src/disk_commit/thread.rs:77` | One `RawIoUring` for the life of the session, owned by the disk-commit thread |

The decision applies regardless of whether the cargo feature
`per-thread-rings` is on or off. Future readers should not attempt to
migrate these sites without first re-opening this record.

## 2. One-shot probes

### 2.1 What they are

Each probe builds a 2-entry throwaway `io_uring::IoUring`, registers an
`io_uring::Probe`, asks the kernel whether one opcode
(`IORING_OP_LINKAT`, `IORING_OP_RENAMEAT`, `IORING_OP_STATX`) is
supported, and stores the boolean in a static `OnceLock<bool>`.
`linkat.rs:49,92,109-122` is the canonical example. Adjacent caches
(`linux_capabilities::openat2_supported`,
`kernel_version::log_io_uring_probe_result`,
`buffer_ring::registration::check_kernel_version`) follow the same
once-and-cache pattern without even building a ring.

### 2.2 Why not per-thread

- **Called exactly once per process.** The `OnceLock<bool>` guarantees
  one probe builds a ring, ever; every subsequent caller reads the
  cached boolean. Per-thread storage would replace one ring build with
  N ring builds and N TLS lookups for the same cached boolean.
- **Zero contention to dissolve.** IUR-1 section 3.4 measured the probe
  acquire below the flame-graph noise floor: no SQ-tail ping-pong, no
  cross-thread shared mutex, never on any hot path.
- **fd cost.** Each probe ring consumes one `io_uring` fd plus SQ/CQ
  mmap pages while it lives. The ring is dropped right after
  `Probe::register`; per-thread duplication would scale that with
  worker count for no observable gain.
- **The boolean is the cache, not the ring.** The point of the
  `OnceLock<bool>` is that the live ring is not needed after the first
  call. Pushing the ring into TLS would invert that invariant.

## 3. Disk-commit singleton

### 3.1 What it is

`IoUringDiskBatch` (`disk_batch.rs:45`) owns a single `RawIoUring`
(`disk_batch.rs:46`) and is declared `!Send + !Sync` by construction
(`disk_batch.rs:42-44`). The disk-commit thread is spawned once per
session (`transfer/src/disk_commit/thread.rs:47-56`), takes ownership
of one `IoUringDiskBatch` via `IoUringDiskBatch::try_new`
(`thread.rs:77,84,89`), and drives every queued write through that
single ring for the full session.

The batch is the deferred-fsync / batched-write target for every
network-reader thread; chunks arrive on the existing
`crossbeam-channel` plumbing in `transfer/src/disk_commit/` and the
disk-commit thread serialises them onto its ring.

### 3.2 Why not per-thread

- **Per-thread defeats the batching invariant.** The whole point of
  the singleton is to amortise SQE setup across many small writes from
  many producer threads. Splitting the ring per producer would give
  every producer its own SQE batch but lose the cross-producer
  coalescing the disk-commit thread does today (`disk_batch.rs:11`:
  "reuses one ring across the entire commit phase").
- **Single-submitter by construction.** `IoUringDiskBatch` is
  `!Send + !Sync` and only the disk-commit thread submits to it. There
  is no cross-thread SQ-tail contention to remove; the SQ tail is
  written by exactly one thread for the whole session.
- **Bounded SQ pressure.** Each producer enqueues at most one fsync
  per file via the existing channel; the disk-commit thread translates
  those into batched SQEs against a single ring at its own pace. SQ
  contention is bounded by ring depth (default 64,
  `io_uring/config.rs:369-383`), not by producer count.
- **Making it thread-local would be a no-op rename.** The disk-commit
  thread already is the unique owner; moving its ring into a TLS slot
  in the same thread changes nothing observable while adding a
  `RefCell` borrow on every submit.

This matches the IUR-2 design call (`iur-2-per-thread-rings.md`
section 1.1, row "`IoUringDiskBatch`": "**shared** singleton (status
quo)").

## 4. Future-revisit triggers

Re-open this decision only if at least one of the following is
observed in production or a credible bench:

- **Disk-commit SQ contention > 5% of CPU.** A flame graph showing the
  disk-commit thread blocked on its own submission queue (or measurable
  starvation of producers waiting for channel space) would invalidate
  the "single-submitter, bounded SQ" assumption. The fix at that point
  is likely not per-thread rings but a deeper rework of the
  disk-commit channel topology (e.g., per-producer ring + fan-in
  reaper); the per-thread layout would still defeat batching.
- **Probe path becomes hot.** If a future code path forks worker
  processes (none today), spawns short-lived child interpreters, or
  otherwise resets the `OnceLock` cache on every iteration, the probe
  ring builds would scale with that frequency. The fix is to move the
  cache into a per-process structure that survives across the forking
  boundary, not to make probes per-thread.
- **Probe ring fd cost shows up under fd-pressure.** If a customer
  hits `EMFILE` and a probe ring is implicated, the fix is to drop the
  ring earlier in the probe (which the current code already does by
  scoping `io_uring::IoUring::new(2)` to the probe function body), not
  to migrate to per-thread.
- **Disk-commit thread itself becomes the bottleneck.** If the
  single-threaded disk-commit drain is the slowest stage, the answer
  is a broader fan-out rework tracked under IUR-7, not the IUR-3.b..e
  per-thread ring primitive.

The per-thread ring primitive (IUR-3.a) is a contention-relief tool for
high-frequency multi-producer factories. It is not the right lever for
either category above.

## 5. Cross-references to the per-thread vs shared map

| Site | Topology | Subtask |
|------|----------|---------|
| `file_writer` factory | per-thread | IUR-3.b (#4804) |
| `file_reader` factory | per-thread | IUR-3.c (#4807) |
| `socket_writer` factory | per-thread | IUR-3.d (#4806) |
| `BgidLease` (per-ring buffer ids) | per-thread (BGE-4 lease) | IUR-3.e (#4811) |
| `socket_reader` factory | shared (one reader per session) | IUR-2 1.1, not migrated |
| One-shot probes (linkat/renameat/statx) | shared, OnceLock-cached | **IUR-3.f (this doc)** |
| `IoUringDiskBatch` | shared singleton, disk-commit thread | **IUR-3.f (this doc)** |
| `ZeroCopySender::ring` | shared `Arc<Mutex<RawIoUring>>` | IUR-3.g (deferred behind IUS-8) |

A reader arriving at one of the sites in section 1 should see this
record cited from the IUR-2 design row and conclude: per-thread
migration was considered and declined. The next reopen trigger is one
of the four bullets in section 4, not a routine "everything per-thread"
sweep.
