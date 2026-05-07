# io_uring fixed-buffer registration coverage

Tracking issue: oc-rsync #2118. Siblings:
[`io-uring-fixed-buffer-audit.md`](io-uring-fixed-buffer-audit.md) (call-site
map), [`iouring-fixed-buffer-registration.md`](iouring-fixed-buffer-registration.md)
(registration mechanics + failure surface),
[`iouring-registered-buffer-adaptive-sizing.md`](iouring-registered-buffer-adaptive-sizing.md)
(resize policy). Coverage view: which hot-path SQEs use `READ_FIXED` /
`WRITE_FIXED`, which still pay per-op page pinning, where the missing
wiring matters for the `>= 64 KiB` workload.

## 1. Mechanism

`IORING_REGISTER_BUFFERS` pins an `iovec` array in kernel memory at ring init.
`READ_FIXED` / `WRITE_FIXED` SQEs dereference a `buf_index` into the pinned
slot table and skip `get_user_pages_fast`. The unfixed `IORING_OP_READ` /
`IORING_OP_WRITE` opcodes walk the caller's address space and refcount each
page on submit, releasing on completion. Pinning charges against
`RLIMIT_MEMLOCK`. The only `register_buffers` call site is
`RegisteredBufferGroup::new` at
`crates/fast_io/src/io_uring/registered_buffers.rs:307`, wrapped by
`try_new` (`registered_buffers.rs:352`). Fixed-opcode helpers:
`submit_read_fixed_batch` (`registered_buffers.rs:498-610`) and
`submit_write_fixed_batch` (`registered_buffers.rs:617-701`).

## 2. Currently registered

Five owners construct a `RegisteredBufferGroup` via `try_new`:

- `IoUringReader::open` - `crates/fast_io/src/io_uring/file_reader.rs:73-81`.
- `IoUringWriter::create` - `crates/fast_io/src/io_uring/file_writer.rs:56-64`.
- `IoUringWriter::from_file` - `crates/fast_io/src/io_uring/file_writer.rs:83-91`.
- `IoUringWriter::with_ring` - `crates/fast_io/src/io_uring/file_writer.rs:118`
  (hard-codes `count = 8`, ignores `IoUringConfig::register_buffers`).
- `IoUringWriter::create_with_size` - `crates/fast_io/src/io_uring/file_writer.rs:143-151`.
- `SharedRing::new_inner` - `crates/fast_io/src/io_uring/shared_ring.rs:267-275`
  (registered but never checked out).

Defaults from `IoUringConfig` (`config.rs:350-379`):
`register_buffers = true`, `registered_buffer_count = 8`,
`buffer_size = 64 KiB` (page-aligned). `for_large_files` lifts to
`16 x 256 KiB = 4 MiB`; `for_small_files` to `8 x 16 KiB = 128 KiB`.
Hard cap `MAX_REGISTERED_BUFFERS = 1024` (`registered_buffers.rs:80`).
The PBUF_RING / `IORING_OP_PROVIDE_BUFFERS` path in `buffer_ring.rs` is a
disjoint namespace (kernel 5.19+), wired but with no production caller;
see [`io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md).
`READ_FIXED` is reached only from `IoUringReader::read_all_batched`
(`file_reader.rs:158-184`); `WRITE_FIXED` only from
`IoUringWriter::write_all_batched` (`file_writer.rs:215-246`) and
`flush_buffer` (`file_writer.rs:282-309`). All three guard on
`available() > 0` and fall through to the unfixed path on miss.

## 3. Coverage gap: ad-hoc unfixed READ / WRITE

Every other read / write SQE in the module uses unfixed opcodes:

| Path | Opcode | Why unfixed |
|---|---|---|
| `IoUringReader::read_at` (`file_reader.rs:111`) | `Read` | One-shot read into caller `&mut [u8]`. |
| `read_all_batched` fallback (`file_reader.rs:229`) | `Read` | All slots in flight. |
| `IoUringWriter::write_at` (`file_writer.rs:177`) | `Write` | One-shot positioned write. |
| `IoUringWriter` fallbacks (`batching.rs:91`) | `Write` | Slot starvation. |
| `SharedRing::submit_read` / `submit_send` (`shared_ring.rs:331`) | `Read` / `Send` | Group registered but reserved for a future batched submitter. |
| `IoUringDiskBatch::flush_buffer` (`disk_batch.rs`) | `Write` | No `RegisteredBufferGroup` field; every flush hits unfixed `submit_write_batch`. |
| `IoUringSocketReader` / `IoUringSocketWriter` | `Recv` / `Send` | No group; `ZeroCopyPolicy::Auto` keeps `SEND` over `SEND_ZC`. |

**Delta-apply uses unfixed reads.** The applicator at
`crates/transfer/src/delta_apply/applicator.rs:163-172` forces
`BasisAccess::BufferedMap` whenever the writer is io_uring-backed - mmap
pointers must never enter an SQE on Linux. `apply_delta` in
`crates/match/src/script.rs:105` is generic over `R: Read, W: Write` and
reaches io_uring only through `IoUringWriter::flush_buffer`. The basis
read therefore never hits `READ_FIXED`, and the disk-batch commit path
has no fixed-buffer plumbing at all - the two largest uncovered slabs of
hot-path I/O.

## 4. Bench plan: fixed vs unfixed at 64 KiB / 1 MiB / 16 MiB

Run on the `rsync-profile` podman container (Linux 6.x, NVMe scratch),
via a microbench in `crates/fast_io/benches/io_uring_fixed.rs` driving a
sequential read or write loop against a pre-allocated 1 GiB file. Each
cell reports steady-state throughput (GiB/s) and CPU% (`perf stat -e
task-clock` divided by wall time).

| Read size | Variant | Submission shape | Expected delta vs unfixed |
|---|---|---|---|
| 64 KiB | `Read` | `read_at` loop, no batching | baseline |
| 64 KiB | `READ_FIXED` | `read_all_batched`, `count = 8`, `sq_entries = 64` | -10..-20% CPU, throughput within +/-2% |
| 1 MiB | `Read` | 16 chunks per submit batch | baseline |
| 1 MiB | `READ_FIXED` | `count = 16` (`for_large_files` preset) | -25..-35% CPU, throughput +5..+10% on slow CPUs |
| 16 MiB | `Read` | 256 chunks per submit batch | baseline |
| 16 MiB | `READ_FIXED` | `count = 16`, `buffer_size = 256 KiB` | -30..-45% CPU, throughput +10..+20% |

Repeat for `Write` vs `WRITE_FIXED`; expect the same shape modulo the
per-write `memcpy` into the slot (`registered_buffers.rs:638-650`), which
narrows the small-size win. Capture `RegisteredBufferStats::miss_rate()`
each run to confirm pool sizing. Numbers above are projections from
`get_user_pages_fast` cost models; populate from `cargo bench -p fast_io
--bench io_uring_fixed` once landed.

## 5. Recommendation

Target every IO `>= 64 KiB` on the hot path with `IORING_REGISTER_BUFFERS`:

1. **Wire `IoUringDiskBatch` to `WRITE_FIXED`.** Add
   `Option<RegisteredBufferGroup>` to the struct and mirror the
   `flush_buffer` branch in `file_writer.rs:282-309`. Largest single
   coverage gap on the disk commit path.
2. **Plumb `register_buffers` through `with_ring`.** One-line fix in
   `writer_from_file_with_depth` (`mod.rs:166-218`); currently the flag
   is silently ignored at that entry point.
3. **Hold the line below 64 KiB.** `read_at` / `write_at` and the
   `for_small_files` preset stay unfixed; the slot-copy cost dominates
   the `get_user_pages_fast` saving for sub-page-cluster IO.
4. **Cap pinned memory.** Default footprint is `512 KiB` per owner; a
   200-connection daemon in `for_large_files` would pin `800 MiB`. The
   adaptive resizer in
   [`iouring-registered-buffer-adaptive-sizing.md`](iouring-registered-buffer-adaptive-sizing.md)
   is the long-term answer; until it lands, document the
   `RLIMIT_MEMLOCK` trade-off in release notes and surface a
   `RegisteredBufferStatus` enum so failure is distinguishable from
   "disabled by config".

Trade-off: each fixed buffer pins `next_multiple_of(buffer_size,
page_size)` bytes for the ring's lifetime against `RLIMIT_MEMLOCK`, and
write paths pay one `memcpy` per submit. In return, every SQE skips an
MMU walk and refcount cycle that scales linearly with `buffer_size /
page_size`. The break-even crosses at roughly 64 KiB on commodity
x86_64; below that, leave the unfixed opcode in place.
