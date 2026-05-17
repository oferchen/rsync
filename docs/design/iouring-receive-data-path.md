# io_uring receive data path: routing chunk writes through registered buffers

Tracking task: IUD-2 (oc-rsync follow-up #2362). Implementation phases
are keyed to IUD-5 (buffer-pool wiring) and IUD-8 (telemetry + rollout).

Companion docs already in tree:

- `docs/design/iouring-registered-buffer-adaptive-sizing.md` - registered
  buffer group sizing and lifecycle.
- `docs/design/iouring-adaptive-buffer-pool.md` - cross-thread buffer
  pool that supplies the SPSC chunks today.
- `docs/design/iouring-borrowed-slice-consumer.md` (#4218) - pin-counted
  pool that this work must compose with.
- `docs/design/iouring-per-thread-rings.md` (#1929) - per-thread ring
  topology this writer will plug into when promoted.
- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` - the same READ_FIXED
  primitive on the basis-file side; this design must not contradict the
  SMR ruling that mmap pointers never enter io_uring SQEs.
- `docs/design/basis-file-io-policy.md` - selector that downgrades the
  basis to `BufferedMap` whenever an io_uring writer is active.

This document does not change any wired dispatch. It specifies the next
step: routing the bytes that the network thread hands to the disk-commit
thread through `IORING_OP_WRITE_FIXED` against a registered-buffer group
shared with the basis-file reader. The actual switch is gated by the new
opt-in feature `iouring-data-writes` (default off) and lands as the
patches enumerated in section 6.

## 1. Current receive writer

The receive path is two threads connected by a lock-free SPSC channel.

### 1.1 Network thread (producer)

- `crates/transfer/src/receiver/transfer.rs:55-70` runs the receiver
  role. The pipelined path
  (`crates/transfer/src/receiver/transfer/pipeline.rs`) parses delta
  tokens via `TokenReader` (`crates/transfer/src/token_reader.rs`) and
  resolves block-copy references against a `MapFile`
  (`crates/transfer/src/map_file/mod.rs:55`).
- For literal tokens the network thread pulls a recycled
  `Vec<u8>` from the buffer pool
  (`crates/transfer/src/pipeline/buffer_pool.rs`), copies the demuxed
  bytes in, and sends it as `FileMessage::Chunk(Vec<u8>)`
  (`crates/transfer/src/pipeline/messages.rs:21-45`).
- For block-copy tokens the network thread reads from the basis
  `MapFile` and emits the resulting slice via the same `Chunk`
  message. The basis source is `BufferedMap` whenever io_uring is
  active on the writer side
  (`crates/transfer/src/delta_apply/applicator.rs:161-176`).

### 1.2 Disk commit thread (consumer)

- `crates/transfer/src/disk_commit/process.rs:32-118` drains the SPSC
  channel one message at a time. `Chunk(Vec<u8>)` is forwarded to
  `Writer::write_chunk` (`crates/transfer/src/disk_commit/writer.rs:199-211`),
  which dispatches to one of:
  - `Writer::Buffered` - `ReusableBufWriter` over `std::fs::File` with
    a 256 KB reusable buffer
    (`crates/transfer/src/disk_commit/writer.rs:71-122`).
  - `Writer::IoUring` (Linux + `io_uring` feature) - delegates to
    `IoUringDiskBatch::write_data`
    (`crates/fast_io/src/io_uring/disk_batch.rs:124-149`), which
    internally calls `submit_write_batch`
    (`crates/fast_io/src/io_uring/batching.rs`) over its private 256 KB
    staging buffer.
  - `Writer::Iocp` (Windows + `iocp` feature) - peer of the above on
    `IORING_OP_WRITEV` analogue.
  - `Writer::Macos` - `MacosWriter` with `F_NOCACHE` + `writev(2)`.
  - `Writer::Vmsplice` (Linux + `vmsplice` feature) - zero-copy splice
    that fires only when neither io_uring nor IOCP claimed the file.

- `make_writer` (`crates/transfer/src/disk_commit/process.rs:277-323`)
  picks the variant. Sparse mode and non-zero `append_offset` force
  `Buffered` because none of the batched backends implement `Seek`.
- After draining a file, `Writer::flush_and_sync` and `Writer::finish`
  (`crates/transfer/src/disk_commit/writer.rs:218-299`) run flush +
  optional fsync + rename in the order matching upstream
  `fileio.c:write_file()` and `receiver.c:finish_recv_file()`.

### 1.3 The two memcpy hops we want to eliminate

For literal tokens today, a byte travels:

1. Socket -> demux -> token reader staging
   (`crates/transfer/src/token_reader.rs`). Copy 1: kernel page cache to
   user buffer via `read(2)`.
2. Token reader -> recycled `Vec<u8>` from the buffer pool. Copy 2: the
   `TokenBuffer::pull_literal` memcpy
   (`crates/transfer/src/token_buffer.rs`).
3. Network thread sends `FileMessage::Chunk(Vec<u8>)` -> SPSC; the buffer
   itself is moved, no copy.
4. Disk thread writes via `Writer::write_chunk`. For
   `Writer::IoUring`, this is `IoUringDiskBatch::write_data` which
   copies the chunk into its private 256 KB staging buffer before
   submitting `IORING_OP_WRITE` SQEs
   (`crates/fast_io/src/io_uring/disk_batch.rs:124-149`,
   `crates/fast_io/src/io_uring/batching.rs::submit_write_batch`).
   Copy 3: user buffer to kernel-bounce staging buffer.

The kernel performs an additional copy inside `IORING_OP_WRITE` because
the staging buffer is not registered: `get_user_pages_fast` pins the
pages each submission, and the SQE carries an unfixed `iovec`. This
write design replaces hop 4's user-side memcpy and removes the
per-submission `get_user_pages` cost by handing the kernel a registered
buffer index that points at the same memory the network thread filled.

### 1.4 Coalesced `WholeFile` path

The single-message `FileMessage::WholeFile { begin, data }` path
(`crates/transfer/src/pipeline/messages.rs:32-37`) is taken when the
entire file fits in one literal token. It folds Begin + one Chunk +
Commit into one SPSC send. The data-path proposal must keep this fast
path intact - it stays on `Writer::Buffered` because the file is
typically small enough that registering a buffer is pure overhead. See
section 4.4 for the threshold.

## 2. Proposed: WRITE_FIXED-backed disk writer

### 2.1 Topology

Reuse the existing `RegisteredBufferGroup`
(`crates/fast_io/src/io_uring/registered_buffers/registry.rs`). The
buffer-pool layer already owns a fixed-size set of `Vec<u8>` slots; we
add a parallel "registered-pool" that wraps those same allocations as
io_uring fixed buffers, keyed by index. Each slot has:

- A registered-buffer index (`buf_index: u16`) usable as the third
  argument to `IORING_OP_WRITE_FIXED`.
- A raw pointer + length stored in a `RegisteredBufferSlotInfo`
  (`crates/fast_io/src/io_uring/registered_buffers/submit.rs:253-260`).
- A pin reference count, mirroring the borrowed-slice consumer design
  (#4218) so that the disk thread cannot recycle a slot while a
  notification CQE is still outstanding.

The disk-commit thread keeps the same `IoUringDiskBatch` instance it
holds today, plus a new sibling `IoUringWriteFixedBatch` that takes
ownership of pre-registered slots instead of allocating a private
staging buffer.

### 2.2 Submission flow per chunk

When `feature = "iouring-data-writes"` is on and the chunk arrives via
`FileMessage::Chunk(slot)` (a new variant carrying a registered-buffer
handle rather than a bare `Vec<u8>`):

1. The disk thread maps the slot's index to the file offset
   (cumulative `bytes_written` for the active `ActiveFile`).
2. It calls `submit_write_fixed_batch`
   (`crates/fast_io/src/io_uring/registered_buffers/submit.rs:159-243`)
   with the slot's `RegisteredBufferSlotInfo` and the current file fd
   (already registered via `try_register_fd`).
3. The submission helper pushes one `IORING_OP_WRITE_FIXED` SQE per
   slot. It uses `submit_and_wait(submitted)` (synchronous) for the
   first cut to match the current `submit_write_batch` semantics; the
   batched / non-blocking path is enumerated in section 6.
4. On CQE, the slot is returned to the pool. Pin count is decremented
   so the network thread can reclaim it via the buffer-return channel
   (`buf_return_tx` in `process.rs:32-118`).

### 2.3 Backward compatibility: dispatch surface

`Writer::IoUring` keeps its current variant. A new `Writer::IoUringFixed`
variant is added:

```text
pub(super) enum Writer<'a> {
    Buffered(ReusableBufWriter<'a>),
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    IoUring { batch: &'a mut fast_io::IoUringDiskBatch },
    #[cfg(all(target_os = "linux", feature = "io_uring", feature = "iouring-data-writes"))]
    IoUringFixed { batch: &'a mut fast_io::IoUringWriteFixedBatch },
    // ... existing variants
}
```

`make_writer` picks `IoUringFixed` when:

- the registered-pool is available (the buffer pool succeeded in
  registering its backing slots with the active ring), AND
- `use_sparse` is false AND `append_offset == 0` AND
  `begin.target_size >= IOURING_DATA_WRITES_MIN_BYTES` (default 64 KiB,
  tunable via env, see section 4.4).

Otherwise dispatch is unchanged, including the unregistered
`Writer::IoUring` path. There is no scenario in which the new variant
silently degrades a file that previously used `Writer::Buffered`.

## 3. Ordering

The write order is unchanged: `Begin -> Chunk* -> Commit | Abort`. The
disk thread still processes one file at a time. The `WRITE_FIXED` SQEs
within a single `Chunk` message complete out-of-order, but the helper
already reassembles bytes-written by user-data index
(`submit_write_fixed_batch:217-237`) and reports `batch_written` only
after every CQE is drained. The next `Chunk` is not submitted until the
previous batch has fully completed; sequential file offset is therefore
preserved.

Across files, `commit_file` (`disk_batch.rs:168-187`) drains the active
file before calling `submit_fsync`. The new `IoUringWriteFixedBatch`
follows the same shape: `commit_file` performs `flush_current` (drain),
then `submit_fsync` (only when `do_fsync`), then detaches the fd.

## 4. fsync placement, partial writes, and fallback

### 4.1 fsync

Identical to today. `commit_file(do_fsync)` issues an `IORING_OP_FSYNC`
SQE against the same fd
(`crates/fast_io/src/io_uring/disk_batch.rs:236-263`). The change does
not move fsync earlier or later; durability semantics match upstream's
`writefd` + `fsync()` pair in `receiver.c:finish_recv_file()`.

### 4.2 Short writes

`IORING_OP_WRITE_FIXED` may complete short on the same surfaces as
`pwrite(2)` short writes: ENOSPC partial flushes, signal-interrupted
NFS writes, FUSE backends that honour `direct_io`. The submission
helper detects shorts by comparing each SQE's `requested_per_sqe` to
its `actual_per_sqe` and advances `total_written` only through the
contiguous prefix of fully-written SQEs
(`submit.rs:217-237`). The outer loop resubmits the remaining bytes
starting at the new offset. This is the same algorithm
`submit_read_fixed_batch` (`submit.rs:29-152`) already uses on the read
side.

If a CQE returns a negative result, the helper converts it to
`io::Error::from_raw_os_error(-result)` and the disk thread aborts the
file via the existing `FileMessage::Abort` pathway. The partially
written temp file is removed by `TempFileGuard::drop`
(`crates/transfer/src/temp_guard.rs`).

### 4.3 Fallback hierarchy

The new path falls back in order:

1. **No `iouring-data-writes` feature** -> existing `Writer::IoUring`
   (unfixed) path, unchanged.
2. **Feature on, but registration failed at ring construction**
   (kernel < 5.6, `register_buffers` rejected, MEMLOCK rlimit too low)
   -> `Writer::IoUring` unfixed path. The disk thread logs a one-line
   `debug_log!(Io, 1, "registered buffer pool unavailable, falling
   back to unfixed io_uring writer")` at startup, mirroring the
   `RegisteredBufferStatus` provenance pattern from
   `crates/fast_io/src/io_uring/file_writer.rs:42-47`.
3. **Per-file ineligible** (sparse, append, file too small) ->
   `Writer::Buffered`, unchanged.
4. **Kernel error mid-transfer** (`ENOMEM`, `EAGAIN`) -> abort the
   file; the receiver records a hard error. Same as today's io_uring
   submission errors.

### 4.4 Per-file size threshold

`IOURING_DATA_WRITES_MIN_BYTES = 64 * 1024` by default. The selection
follows two pressures: small files (under one wire chunk, ~32 KiB) ride
the `WholeFile` coalesced path and never reach the chunk loop;
medium files just above that threshold incur ring-side scheduling that
costs more than the kernel-side `get_user_pages_fast` it saves.
Telemetry from IUD-8 (section 6) will tighten this number after a real
run.

## 5. Feature flag and configuration

`iouring-data-writes` (default off) lives on the `fast_io` crate and is
re-exported by `transfer` through the same pass-through pattern as
`io_uring`, `iocp`, `vmsplice`
(`crates/fast_io/Cargo.toml:39-83`,
`crates/transfer/Cargo.toml:88-101`):

```text
# fast_io/Cargo.toml
iouring-data-writes = ["io_uring"]

# transfer/Cargo.toml
iouring-data-writes = ["io_uring", "fast_io/iouring-data-writes"]
```

The runtime selector additionally consults
`OC_RSYNC_IOURING_DATA_WRITES` (`auto` / `force` / `off`). `auto` is
the documented production value once the rollout completes; `force` is
for benchmarks; `off` disables the path even when the feature is
compiled in. The env var follows the same dispatch convention as
`OC_RSYNC_BENCH_IOURING_RING`
(`crates/fast_io/Cargo.toml:155`).

## 6. Implementation plan

The work is split into five PR-sized steps, keyed to IUD-5 (buffer pool
wiring) and IUD-8 (telemetry + rollout).

1. **IUD-5a: `IoUringWriteFixedBatch` skeleton.** Introduce the new
   batch type in `crates/fast_io/src/io_uring/` next to
   `IoUringDiskBatch`. API mirror: `new`, `try_new`, `begin_file`,
   `write_data_fixed(slot: RegisteredBufferSlotInfo, len: usize)`,
   `flush`, `commit_file`, `bytes_written*`. Tests: extend
   `crates/fast_io/src/io_uring/tests.rs` with `commit`, multi-file,
   drop-flush parity tests under the new feature. No production
   wiring.

2. **IUD-5b: Buffer-pool registration.** Teach the existing
   buffer-pool allocator in `crates/transfer/src/pipeline/buffer_pool.rs`
   to allocate slots that are page-aligned and large enough to satisfy
   `io_uring_register_buffers(2)`. Add a `try_register_with_ring`
   method that walks the slots, registers them, and stores the
   per-slot `RegisteredBufferSlotInfo` alongside the existing
   `Vec<u8>` payload. Provenance is captured in
   `RegisteredBufferStatus` so operators can diagnose
   `unsupported / rejected / disabled-by-config / disabled-by-rlimit`.

3. **IUD-5c: New `FileMessage::FixedChunk` variant.** Add the variant
   to `crates/transfer/src/pipeline/messages.rs:21-45`. The network
   thread writes literal-token bytes directly into a registered slot
   (no second memcpy) and sends `FixedChunk(slot_handle)`. The disk
   thread dispatches `FixedChunk` to `Writer::IoUringFixed::write_chunk`
   and returns the handle through `buf_return_tx`. The pre-existing
   `Chunk(Vec<u8>)` variant is kept for the buffered, vmsplice, IOCP,
   and macOS variants.

4. **IUD-5d: `Writer::IoUringFixed` and selector update.** Extend
   `Writer` in `crates/transfer/src/disk_commit/writer.rs:144-163` and
   `make_writer` in `process.rs:277-323`. Add the eligibility checks
   from section 2.3 and the size threshold from section 4.4. Default
   the env-var selector to `auto`.

5. **IUD-8: Telemetry + rollout gating.** Wire counters into the
   existing `TransferStats`
   (`crates/transfer/src/receiver/stats.rs`) for: chunks routed via
   `WRITE_FIXED`, chunks routed via unfixed io_uring, chunks routed
   via buffered fallback, registered-buffer slot-exhaustion fallbacks,
   short-write retries. Wire a single-line `debug_log!(Io, 1, ..)`
   on the first per-process degradation transition. Flip the
   `OC_RSYNC_IOURING_DATA_WRITES` default to `auto` once benchmarks
   from `crates/fast_io/benches/iouring_data_writes.rs` (added in
   IUD-8) demonstrate non-regression on the workloads called out in
   `crates/fast_io/Cargo.toml:111-160`.

## 7. Interaction with the basis-file read primitive (SMR-3a)

The basis-file reader (SMR-3a, `mmap-vs-sqpoll-conflict-resolution.md`
section "Replacing mmap with READ_FIXED") plans to issue
`IORING_OP_READ_FIXED` against the same registered-buffer pool. The two
designs deliberately share the pool, which means:

- **One pool, two consumers.** Per-ring registration limits
  (`IORING_REGISTER_BUFFERS` caps at 1024 slots; the kernel-side
  iov_iter cost grows with slot count) are spent once; both the
  reader and the writer draw from the same pool.
- **Strict ownership ping-pong.** A slot in flight on a READ_FIXED SQE
  cannot be reused by a WRITE_FIXED SQE until its read CQE has been
  drained and its pin-count returned to zero. The borrowed-slice
  consumer (#4218) already encodes this invariant.
- **SQPOLL is forbidden for both paths.** The
  `mmap-vs-sqpoll-conflict-resolution.md` rule applies symmetrically:
  registered buffers backed by file-backed VMAs cannot be referenced
  by an SQPOLL kthread without exposing `SIGBUS`. Both the read and
  write paths therefore continue to construct rings without
  `IORING_SETUP_SQPOLL` when the registered-buffer pool is active.

The selector in section 2.3 must therefore also check that the basis-
file strategy is not `MmapStrategy`. The check is already enforced
indirectly today via `basis-file-io-policy.md` (writer-io_uring forces
`BufferedMap`), but the new feature flag promotes the rule to a hard
precondition on `Writer::IoUringFixed` construction.
