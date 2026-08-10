# Per-file vs shared io_uring ring on small-file workloads

Tracking issue: oc-rsync task #1410. Branch: `docs/per-file-vs-shared-uring-1410`.

## Scope

Quantify the cost of the current per-file io_uring ring lifecycle when the
workload is dominated by small files (think 100K entries at 1 KB each), and
plan a head-to-head benchmark against a shared session ring. Extends the
session-instance audit (see `docs/audits/shared-iouring-session-instance.md`,
task #1408) with a concrete measurement plan that the implementation work
under #1937 / #1097 / #1872 can validate against.

This is a docs-only audit. No code changes, no `Cargo.toml` change. The
output is the benchmark plan that follow-up patches will execute.

## 1. Current ring lifecycle in `crates/fast_io/src/io_uring/`

Two distinct lifetimes exist today; both are documented and exercised in
production code.

### 1.1 Per-file rings (the hot path for small files)

The receiver and the sender both build a fresh `io_uring::IoUring` instance
for every file they touch.

- `crates/fast_io/src/io_uring/mod.rs:174-226` -
  `writer_from_file_with_depth` calls `IoUringConfig::default()` then
  `config.build_ring()` per invocation. Each receiver-side write commit
  (`crates/transfer/src/transfer_ops/response.rs:108-113`) routes through
  this entry point, so one ring is born and torn down per output file.
- `crates/fast_io/src/io_uring/mod.rs:250-288` -
  `reader_from_path_with_depth` does the same on the read side. The
  generator (`crates/transfer/src/generator/mod.rs:712-743`) gates this
  behind a 1 MB threshold (`IO_URING_READ_THRESHOLD`) precisely because the
  ring construction cost outweighs the batched-syscall benefit on small
  inputs. Files below 1 MB get a plain `BufReader`.
- `crates/fast_io/src/io_uring/file_factory.rs` and
  `file_writer.rs::IoUringWriter::with_ring` accept the ring by value, so
  the ring's `Drop` runs at the end of the per-file writer's scope.

### 1.2 Per-session rings (already in place for two paths)

Two long-lived rings already exist; the small-file path is not yet wired to
either.

- `crates/fast_io/src/io_uring/disk_batch.rs:65-92` - `IoUringDiskBatch::new`
  is invoked exactly once per disk-commit thread
  (`crates/transfer/src/disk_commit/thread.rs:179`). The same ring services
  every file the commit thread writes; `begin_file` re-registers the new fd
  via `try_register_fd` and reuses the buffer. This is the only existing
  multi-file shared ring.
- `crates/fast_io/src/io_uring/shared_ring.rs:208-286` - `SharedRing::try_new`
  registers one reader fd and one writer fd into a single ring with
  `IORING_OP_POLL_ADD` for write-readiness. It is currently used only by
  the experimental session merger and has not been promoted to the default
  for the receiver write path.

### 1.3 Net effect on a 100K-file workload

The receiver's hot path through `transfer_ops/response.rs` allocates one
ring per file. With `IoUringConfig::default()` (`config.rs:369-383`) that
is `sq_entries = 64`, `register_buffers = true`, `registered_buffer_count =
8`, `register_files = true`, no SQPOLL. The ring is dropped at function
exit, so 100K files mean 100K `io_uring_setup` calls plus their teardown.
The disk-commit thread's `IoUringDiskBatch` runs in parallel and is
unaffected by file count, but the writer in front of it is not.

## 2. Per-file ring cost

Three concrete kernel-side costs scale linearly with file count when the
ring is per-file.

### 2.1 `io_uring_setup(2)` syscall

`io_uring::IoUring::new(sq_entries)` (`config.rs:438-453`,
`disk_batch.rs:70-71`) ultimately invokes `io_uring_setup(2)`. The kernel
allocates an `io_ring_ctx`, sizes the SQ/CQ rings (`sq_entries=64` rounded
to a power of two, `cq_entries = 2 * sq_entries` by default), and grabs an
unused fd. On modern kernels the syscall itself is on the order of tens of
microseconds when the ring is small; the dominant cost is the page
allocations described next, not the syscall vector.

### 2.2 SQ / CQ / SQE array `mmap`

After `io_uring_setup(2)` the io-uring crate maps three regions into the
process address space (`io_uring/src/lib.rs` upstream): the submission
ring, the completion ring, and the SQE array. With `sq_entries = 64` and
`cq_entries = 128`, those are typically three 4 KiB pages plus a 4 KiB SQE
array, so 16 KiB of pinned-page mappings per ring. Per-file rings churn
those mappings 100K times, which is ~1.6 GiB of cumulative mmap/munmap
work. The kernel `vm_area_struct` insert / split / merge overhead and the
TLB shootdown traffic are the visible symptoms.

In addition, `register_buffers = true` triggers
`IORING_REGISTER_BUFFERS` (`registered_buffers.rs`), which pins each
registered buffer's pages and bumps their reference counts. Eight 64 KiB
buffers per ring means 512 KiB pinned per file, dropped when the ring
closes. This shows up as `mm->pinned_vm` traffic and as page-table
churn.

### 2.3 SQPOLL kernel thread

`IoUringConfig::sqpoll` is `false` in the defaults, but when a caller flips
it on (`config.rs:439-449`) the kernel spawns one kernel thread per ring
to poll the SQ. A per-file ring with SQPOLL enabled means one short-lived
kernel thread per file, which is catastrophic at 100K files: `kthread_create`
plus the scheduler enqueue, idle spin, and exit dwarfs the actual I/O.
SQPOLL only ever pays off on rings that live long enough to amortize that
thread; short-lived per-file rings must keep `sqpoll = false`. The
benchmark plan includes a SQPOLL-on row for the shared ring only, to
measure the upper bound.

### 2.4 Other per-file fixed costs

- `IORING_REGISTER_FILES` (`batching.rs::try_register_fd`) installs a
  fixed-file table per ring; on a per-file ring that table holds one slot
  and is freed immediately on drop.
- `Probe` setup via `register_probe` runs once per process and is cached,
  so it does not contribute per-file.
- Allocating the `RegisteredBufferGroup` user pages
  (`registered_buffers.rs`) goes through the global allocator on every
  ring; a per-file ring re-allocates 8 * 64 KiB = 512 KiB of `Vec<u8>`
  buffers each time.

## 3. Shared session ring trade-offs

Promoting the receiver hot path to the existing `IoUringDiskBatch` model
(or to a richer `SharedRing` with a single SQ for the whole session)
removes the per-file overhead but introduces three new constraints.

### 3.1 Submission contention

`RawIoUring` is `!Sync`. `IoUringDiskBatch` already documents this
(`disk_batch.rs:42-44`) and pins itself to the disk-commit thread. A
session-wide ring shared between the network receiver, the disk-commit
thread, and the generator would either (a) force all three into one
thread, losing the producer/consumer pipeline, or (b) require an internal
mutex around `submission()` / `submit_and_wait()`. Mutex serialization on
a hot path that previously ran lock-free is the primary risk.

The mitigation candidates are:
- **One ring per worker thread**, mirroring the existing per-thread
  `BufferPool` pattern. Eliminates contention but multiplies kernel
  resources by the worker count.
- **Lock-free SQ enqueue**, using `io_uring_setup(IORING_SETUP_SUBMIT_ALL)`
  plus a CAS on the ring tail. Doable but invasive; defer until the
  benchmark shows the mutex is the bottleneck.
- **Bounded session pool** - 4-8 rings drawn round-robin, sized to the
  number of CPU pipelines. Halfway between per-file and singleton, and
  the cleanest fallback when SQPOLL is not available.

### 3.2 Registered buffer reuse

`RegisteredBufferGroup::try_new` (`registered_buffers.rs`) registers a
fixed-size pool against a specific ring. The pool currently outlives a
single file because `IoUringDiskBatch` reuses one ring; it cannot
trivially span rings (the kernel binds the pages to the registering ring
fd). Promoting to a session ring lets the same pool service every file,
which removes the per-file 512 KiB allocate-pin-unpin churn entirely.

The tradeoff is buffer sizing. The default 8 buffers x 64 KiB suits one
hot file; under 100K concurrent inflight files we either need a larger
pool (32-64 buffers) or a smaller per-file slice. The benchmark plan
records the registered-buffer hit rate to size this empirically.

### 3.3 Fallback chain stays the same

The `Auto` policy in `mod.rs::writer_from_file` already chains
`is_io_uring_available()` -> `build_ring()` -> `BufWriter`. A shared ring
inherits that chain at session-construction time: build the session ring
once, fall back to per-file `BufWriter` for the entire session if it
fails. SQPOLL fallback (`SQPOLL_FALLBACK` in `config.rs`) becomes
session-scoped instead of per-file, which is strictly cheaper.

### 3.4 Lifetime hazards

`SharedRing` documents an explicit drop order (`shared_ring.rs:189-192`):
the kernel ring fd must close before `RegisteredBufferGroup` deallocates
its user pages. A session-scoped ring must enforce the same ordering at
session teardown; otherwise the kernel still holds pinned references to
freed user memory. This is an invariant audit, not a bug, but it is the
hazard the implementation must verify with miri / loom-style ordering
tests.

## 4. Proposed benchmark

### 4.1 Workload matrix

Three file-size shapes, each at a fixed total count. Counts chosen so
both extremes finish in seconds on a development host.

| Shape | Count | Per-file size | Total bytes | Aggregate inflight |
|-------|-------|---------------|-------------|--------------------|
| Tiny  | 100K  | 1 KiB         | ~100 MiB    | 1 ring x 64 SQ     |
| Mid   | 100K  | 64 KiB        | ~6.4 GiB    | 1 ring x 64 SQ     |
| Mixed | 100K  | 80% 1 KiB + 20% 64 KiB | ~1.3 GiB | 1 ring x 64 SQ |

Each shape runs through three io_uring topologies:

1. `per_file` - today's default, one ring per output file.
2. `disk_batch` - re-route receiver writes through `IoUringDiskBatch` so the
   single commit-thread ring services every file.
3. `session_pool` - 4 rings drawn round-robin from a session pool; SQPOLL on,
   with `CAP_SYS_NICE` granted via `setpriv` in the bench harness.

A baseline `std` row (`IoUringPolicy::Disabled`) anchors the absolute
floor. All four rows for all three shapes give twelve data points per
hardware target.

### 4.2 Harness

Reuse `crates/fast_io/benches/io_optimizations.rs` as the entry point
(criterion). Add a new bench group, `per_file_vs_shared_ring`, that:

1. Generates the workload into a `tempfile::TempDir` once per shape.
2. For each topology, spins up an `oc-rsync` local-copy session with
   `--no-whole-file=false` so the receiver's write path is exercised.
3. Records throughput (bytes / second), wall time, and `getrusage`
   `ru_nivcsw` and `ru_inblock` to capture syscall/scheduler load.
4. Captures per-call latency by wrapping the ring-construction path in a
   `tracing` span and exporting a histogram (p50 / p99) per topology.

Run on the canonical Linux benchmark container (`localhost/oc-rsync-bench`,
the podman image used by `scripts/benchmark.sh`). The container already
has the upstream rsync 3.4.1 binary built, so the bench harness can emit a
"vs upstream" column for context, even though upstream does not use
io_uring.

### 4.3 Metrics and acceptance

Primary:

- **Throughput (MiB/s)**, geo-mean across shapes. The shared-ring wins
  must clear `per_file` by at least 25% on the Tiny shape to justify the
  contention complexity; ties are unacceptable since the shared ring
  carries new lifetime hazards.
- **`io_uring_setup` syscall count** captured via `bpftrace
  -e 'tracepoint:syscalls:sys_enter_io_uring_setup { @[comm] = count(); }'`.
  Expected: `per_file` ~= file count, `disk_batch` and `session_pool` <= 8.

Secondary:

- **Per-call latency (ns)** for `writer_from_file_with_depth` (per-file)
  vs `IoUringDiskBatch::begin_file` (shared). p50 and p99 are both
  reported; the p99 is the indicator of mutex contention if it surfaces.
- **`mm->pinned_vm` peak** via `cat /proc/self/status` snapshots; the
  shared ring should plateau, the per-file ring should oscillate.
- **Scheduler invuluntary context switches (`ru_nivcsw`)** - SQPOLL
  topology should drop this near zero on the shared ring; the per-file
  ring must not enable SQPOLL (see 2.3) and should match the no-SQPOLL
  baseline.

The benchmark is documentation when it lands; the implementation patches
under #1937 / #1097 / #1872 cite this doc and replay the harness in CI.

## References

- Session-instance design: `docs/audits/shared-iouring-session-instance.md`
  (#1408).
- Existing pbuf-ring audit: `docs/audits/iouring-pbuf-ring.md`.
- Container harness: `scripts/benchmark.sh`,
  `scripts/benchmark_hyperfine.sh`.
- Upstream small-file behaviour: `target/interop/upstream-src/rsync-3.4.1/`
  (`fileio.c`, `receiver.c`).
