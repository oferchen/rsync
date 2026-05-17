# io_uring Data Path Coverage Audit (IUD-1)

## Purpose

Catalogue every io_uring SQE construction site in `crates/fast_io/src/io_uring/`,
classify each by opcode category, and confirm whether real production callers
in `crates/transfer/`, `crates/engine/`, and `crates/rsync_io/` route file
data (not just metadata, fences, or fallbacks) through the ring.

The audit answers a single load-bearing question: is io_uring today
"metadata-only" or does it carry the bulk delta payload as well?

## Method

- Enumerated SQE sites via `grep -rn "opcode::" crates/fast_io/src/io_uring/`.
- For each site, recorded the opcode and the enclosing public function.
- Traced every public symbol upward through the workspace, excluding files
  under `crates/fast_io/tests/`, `crates/fast_io/benches/`, and inline
  `#[cfg(test)]` modules.
- Distinguished "production caller present" (non-test code in `engine`,
  `transfer`, `core`, `rsync_io`, or `daemon`) from "test/bench only".

## Per-opcode inventory

All file paths are relative to the workspace root.

### Metadata SQEs

| Site | Opcode | Submitter | Production caller |
| --- | --- | --- | --- |
| `crates/fast_io/src/io_uring/statx.rs:177` | `Statx` | `submit_statx_blocking` | None. `fast_io::try_statx_batch_via_io_uring` is exported but no `engine`, `transfer`, `core`, `daemon`, or `metadata` call site invokes it. Metadata reads go through `metadata/src/stat_cache.rs::try_statx_optimized`, which calls `rustix::fs::statx` (synchronous syscall, not io_uring). |
| `crates/fast_io/src/io_uring/statx.rs:346` | `Statx` (batch) | `submit_statx_batch_io_uring` | Same as above - exported, unused in production. |
| `crates/fast_io/src/io_uring/linkat.rs:152` | `LinkAt` | `submit_linkat_blocking` | Reached via `fast_io::hard_link` -> `try_hard_link_via_io_uring`. Used in production by `transfer/src/receiver/directory/links.rs:261`, `transfer/src/receiver/quick_check.rs:179`, `engine/src/local_copy/overrides.rs:60`, `engine/src/local_copy/executor/file/copy/links.rs:81`, `engine/src/local_copy/hard_links.rs:98,167`. |
| `crates/fast_io/src/io_uring/renameat2.rs:142` | `RenameAt` | `renameat2_blocking` | Reached via `fast_io::try_rename_via_io_uring`. Production callers: `transfer/src/disk_commit/process.rs:370`, `transfer/src/transfer_ops/response.rs:284`, `transfer/src/receiver/transfer.rs:447`, `engine/src/local_copy/executor/file/guard.rs:314,325`. |

Site count: 4 distinct opcodes, 2 wired into production (linkat, renameat2),
2 dormant (statx blocking, statx batch).

### File data read SQEs

| Site | Opcode | Submitter | Production caller |
| --- | --- | --- | --- |
| `crates/fast_io/src/io_uring/file_reader.rs:125` | `Read` | `IoUringReader::read_into` (BufReader fill path) | Reached via `fast_io::reader_from_path_with_depth`. Production caller: `transfer/src/generator/mod.rs:898` (`open_source_reader`) for source files >= 1 MiB. This is the sender-side basis/source read path. |
| `crates/fast_io/src/io_uring/file_reader.rs:244` | `Read` (batched) | `IoUringReader::read_batched` | Same `reader_from_path_with_depth` entry point as above. |
| `crates/fast_io/src/io_uring/registered_buffers/submit.rs:38` | `ReadFixed` | `submit_read_fixed_batch` | Used internally by `IoUringReader` registered-buffer fast path - same production caller (`generator::open_source_reader`). |
| `crates/fast_io/src/io_uring/shared_ring.rs:235` | `Read` | `SharedRing::submit_read` | None. `SharedRing` is exported via `fast_io::SharedRing` but no production crate constructs it. Tests in `crates/fast_io/tests/io_uring_shared_ring.rs` and `io_uring_mmap_pressure.rs` are the only callers. |
| `crates/fast_io/src/io_uring/linked_chain.rs:275,318` | `Read` | `LinkedChain` builder | None outside `crates/fast_io`. The chain helper is exposed but no `engine`/`transfer` site links a Read+Write chain today. |

Site count: 5. Two paths (`file_reader::read`, `read_fixed`) are wired into
production through `generator::open_source_reader`. Three paths
(`shared_ring::submit_read`, two `linked_chain` Read constructors) are
unused outside tests.

### File data write SQEs

| Site | Opcode | Submitter | Production caller |
| --- | --- | --- | --- |
| `crates/fast_io/src/io_uring/file_writer.rs:214` | `Write` | `IoUringWriter::write_via_ring` | Reached via `fast_io::writer_from_file_with_depth`. Production caller: `transfer/src/transfer_ops/response.rs:108` (`process_file_response`) - the synchronous receiver write path for delta literals and copy-token output. |
| `crates/fast_io/src/io_uring/file_writer.rs:407` | `Fsync` | `IoUringWriter::sync` | Same call chain as above. |
| `crates/fast_io/src/io_uring/registered_buffers/submit.rs:168` | `WriteFixed` | `submit_write_fixed_batch` | Used internally by `IoUringDiskBatch::write_chunk` and `IoUringWriter`, both reached from production. |
| `crates/fast_io/src/io_uring/disk_batch.rs:238` | `Fsync` | `IoUringDiskBatch::submit_fsync` | Reached via `fast_io::IoUringDiskBatch`. Production caller: `transfer/src/disk_commit/thread.rs:84,89` constructs the batch, `transfer/src/disk_commit/process.rs:289` enrols files via `begin_file`. This is the pipelined receiver write path used when neither sparse mode nor append mode is active. |
| `crates/fast_io/src/io_uring/batching.rs:90` | `Write` | `submit_write_batch` | Internal helper used by `IoUringWriter`, `IoUringDiskBatch`, both with production callers above. |
| `crates/fast_io/src/io_uring/linked_chain.rs:285,323` | `Write` | `LinkedChain` builder | None outside `crates/fast_io` - chain helper is dormant. |

Site count: 6 SQE-constructing sites for write/fsync. Four are reached in
production (`file_writer::write`, `disk_batch::Fsync`,
`batching::submit_write_batch`, `registered_buffers::submit_write_fixed_batch`).
Two `linked_chain` Write constructors are unused outside tests.

### Socket I/O SQEs

| Site | Opcode | Submitter | Production caller |
| --- | --- | --- | --- |
| `crates/fast_io/src/io_uring/shared_ring.rs:289` | `Send` | `SharedRing::submit_send` | None outside `crates/fast_io/tests/`. |
| `crates/fast_io/src/io_uring/batching.rs:317` | `Send` | `submit_send_batch` | Used internally by `IoUringSocketWriter::write`. The writer is exposed via `fast_io::socket_writer_from_fd`, but the only callers live in `crates/fast_io/tests/io_uring_shared_ring.rs` and the internal `fast_io::io_uring::tests` module. No `rsync_io`, `daemon`, or `core` site constructs an io_uring socket writer. |
| `crates/fast_io/src/io_uring/send_zc.rs:129` | `SendZc` | `try_send_zc_blocking` | Same socket writer wiring as above - only test consumers today. |
| `crates/fast_io/src/io_uring/socket_reader.rs:49,90` | `Recv` | `IoUringSocketReader::fill_buf`, `read_into` | Reached via `fast_io::socket_reader_from_fd`. No production crate calls it - `rsync_io` builds sockets with `std::net::TcpStream` and `ssh::stdio` pipes only. |

Site count: 5 socket SQE sites. Zero production callers; the entire socket
path is benchmark and integration-test material.

### Polling / cancellation SQEs

| Site | Opcode | Submitter | Production caller |
| --- | --- | --- | --- |
| `crates/fast_io/src/io_uring/shared_ring.rs:262` | `PollAdd` (POLLOUT) | `SharedRing::submit_poll_write` | None outside tests (SharedRing has no production callers). |
| `crates/fast_io/src/io_uring/batching.rs:189` | `PollAdd` (POLLOUT) | `submit_send_batch` internal readiness probe | Indirectly through `IoUringSocketWriter` - no production callers (see Socket I/O row). |
| `crates/fast_io/src/io_uring/batching.rs:194` | `LinkTimeout` | `submit_send_batch` | Same as above - test-only. |
| `crates/fast_io/src/io_uring/cancel.rs:160` | `AsyncCancel` | `submit_async_cancel_blocking` | None - exposed via `fast_io::cancel_op_blocking` but no production caller invokes it. |
| `crates/fast_io/src/io_uring/cancel.rs:205` | `AsyncCancel2` | `submit_async_cancel2_blocking` | None. |
| `crates/fast_io/src/io_uring/cancel.rs:392,454` | `PollAdd` (test fixtures) | inline `#[cfg(test)]` | Tests only. |

Site count: 6. Zero production callers.

## Aggregate counts

| Category | SQE sites | Production-wired | Production-wired ratio |
| --- | --- | --- | --- |
| Metadata (statx / linkat / renameat2) | 4 | 2 (linkat, renameat2) | 50% |
| File data read | 5 | 2 (`file_reader::Read`, `ReadFixed`) | 40% |
| File data write | 6 | 4 (`file_writer::Write`, `Fsync`, `disk_batch::Fsync`, batched `submit_write_batch`, `WriteFixed`) | 67% |
| Socket I/O (Send / SendZc / Recv) | 5 | 0 | 0% |
| Polling / cancel (PollAdd / LinkTimeout / AsyncCancel*) | 6 | 0 | 0% |
| **Totals** | **26** | **8** | **31%** |

## "Metadata-only" claim: refuted

The often-repeated framing - "io_uring in oc-rsync today is metadata-only" -
does not match the wire-up. Two of the four metadata opcodes (`Statx` both
variants) are dormant. The metadata code paths actually wired through io_uring
are `LinkAt` and `RenameAt`, and they sit on the cold path (hardlink
creation, temp-file commit).

Real file-data traffic does flow through io_uring today:

- **Receiver write path.** `transfer/src/transfer_ops/response.rs:108`
  wraps every received file in `fast_io::writer_from_file_with_depth`. On
  Linux 5.6+ with the feature enabled this becomes an `IoUringWriter` whose
  every `write` call submits an `IORING_OP_WRITE` (or `WRITE_FIXED` when
  registered buffers are in use), followed by a final `IORING_OP_FSYNC`.
- **Pipelined receiver write path.** `transfer/src/disk_commit/thread.rs`
  creates one `IoUringDiskBatch` per disk-commit thread and routes
  `BeginMessage` -> `WriteChunkMessage` -> `EndMessage` through it. The chunk
  payload (delta data) lands in `submit_write_batch` and the commit fence in
  `IoUringDiskBatch::submit_fsync`.
- **Sender source-file read path.** `transfer/src/generator/mod.rs:898`
  (`open_source_reader`) routes source files >= 1 MiB through
  `fast_io::reader_from_path_with_depth`, which submits `IORING_OP_READ` /
  `READ_FIXED` SQEs for each fill of the rolling-hash buffer.

What is **not** wired through io_uring today:

- All socket I/O. `rsync_io` (daemon TCP, SSH stdio passthrough) uses
  `std::net::TcpStream`, `std::io::Read`/`Write`, and synchronous
  `read_exact` / `write_all`. None of `socket_writer_from_fd`,
  `socket_reader_from_fd`, `SharedRing::submit_send`, or `try_send_zc_blocking`
  has a production caller.
- All polling / cancellation primitives. The `cancel.rs` and `LinkTimeout`
  helpers exist for future use but no caller routes file-data SQEs through
  them.
- Source-file reads below 1 MiB and any source read taken with `--open-noatime`
  fall back to `std::io::BufReader` via `open_source::open_source_with_noatime`.
- Receiver writes with sparse mode or non-zero `append_offset` fall back to
  the buffered std-I/O writer (`disk_commit/process.rs:288-322`).
- Local-copy data writes. `engine/src/local_copy/executor/file/copy/` uses
  `copy_file_range`, `clonefile`, `CopyFileExW`, or `std::io::copy` - none of
  which submits io_uring SQEs for the payload itself.

Upstream rsync entry points that still drive std-I/O in our build:

- `io.c:perform_io()` - upstream waits on `select()` and drains buffers with
  `read`/`write`. Our equivalent (`rsync_io::channel_adapter`,
  `daemon::handler::tcp_io`) uses synchronous std reads / writes; no socket
  SQE is constructed.
- `receiver.c:855 do_open` for write target - opened with `OpenOptions`, then
  wrapped only conditionally in an io_uring writer; the open syscall itself
  is synchronous (no `IORING_OP_OPENAT`).
- `generator.c:generate_files` - file enumeration goes through std `metadata()`
  and `read_link()` calls, not `submit_statx_batch`.

## Top three production sites for io_uring data-path migration

Ordered by expected throughput payoff under the workloads we benchmark.

1. **Socket read/write in `rsync_io`** (`crates/rsync_io/src/channel_adapter.rs`,
   `crates/rsync_io/src/binary/negotiate.rs:101`, `crates/rsync_io/src/ssh/`).
   Every TCP daemon and SSH session today exchanges multiplex frames through
   synchronous `read_exact` / `write_all` over `std::net::TcpStream` /
   `std::process::ChildStdin/Out`. Switching to `IoUringSocketReader` /
   `IoUringSocketWriter` (the wiring already exists in
   `crates/fast_io/src/io_uring/socket_reader.rs` and `socket_writer.rs`)
   would let the receiver overlap network drain with disk commit on a single
   ring, removing two syscall round-trips per multiplex frame. IUD-2 is
   intended to scope this.

2. **Sender source reads below the 1 MiB threshold and the
   `--open-noatime` fallback** (`crates/transfer/src/generator/mod.rs:881`).
   Today every source file under 1 MiB and every `--open-noatime` transfer
   bypasses io_uring entirely (std `BufReader`). For 100 k small-file
   workloads this is the dominant read path. Lifting the threshold (or
   teaching `IoUringReader::open` to accept custom open flags so the
   noatime variant qualifies) routes that traffic through `READ_FIXED`
   SQEs and lets the rolling-hash scan amortise ring overhead via
   pipelined submissions. IUD-3 should cover this.

3. **Receiver fallback paths in sparse and append modes**
   (`crates/transfer/src/disk_commit/process.rs:288-322`). When either
   `use_sparse` or `append_offset > 0` we drop out of `IoUringDiskBatch`
   into `ReusableBufWriter`. Sparse-mode writes are common on backup
   workloads (mostly-zero VM images, log files). Wiring sparse-aware
   submissions through io_uring - either via per-extent `Write` SQEs with
   explicit offsets or via the linked `Write` + `FALLOC_FL_PUNCH_HOLE`
   chain - would close the largest production gap left after IUD-2.

## Cross-references

- IUD-2 (socket-path migration): design entry point will use
  `IoUringSocketReader` / `IoUringSocketWriter` from
  `crates/fast_io/src/io_uring/socket_reader.rs` and `socket_writer.rs`.
  The existing `docs/design/io-uring-shared-ring-bench.md` and
  `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` already discuss
  ring-mode trade-offs; IUD-2 should fold those findings into the call-site
  list above (`rsync_io::channel_adapter`, `rsync_io::binary::negotiate`,
  `rsync_io::ssh`).
- IUD-3 (small-file and noatime data-path migration): build on the existing
  threshold logic in `transfer/src/generator/mod.rs::open_source_reader`
  and on `docs/audits/io-uring-adaptive-buffer-sizing.md`,
  `docs/design/iouring-adaptive-buffer-pool.md` for buffer pool sizing.
- Related prior audits: `docs/audits/io-uring-fixed-buffer-audit.md`,
  `docs/audits/per-file-vs-shared-uring-ring.md`,
  `docs/audits/disk-commit-iouring-batching.md`.
