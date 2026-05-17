# splice/vmsplice zero-copy network-to-disk path

Tracking issue: oc-rsync task #1361.
Related primitives: `crates/fast_io/src/splice.rs` (already implemented as
low-level building blocks; not yet wired into the receiver).
Related design: `docs/design/basis-file-io-policy.md` (mmap vs buffered for
basis files), `docs/design/async-io-uring-impact.md`.

## Summary

This document evaluates whether oc-rsync's network-to-disk receive path
should be re-plumbed to use Linux's `splice(2)` and `vmsplice(2)` syscalls
for zero-copy ingest of literal delta data. The low-level primitives
(`try_splice_to_file`, `try_vmsplice_to_file`, `recv_fd_to_file`,
`SplicePipe`) already exist in `crates/fast_io/src/splice.rs` but no caller
in the workspace invokes them today.

**Recommendation: defer.** The multiplex framing layer and the per-token
literal/block-ref dispatch in the receiver mean splice cannot operate on
the wire stream directly; the userspace copy it would replace has already
been minimised by the existing `Vec<u8>`-recycling pipeline and io_uring
batched writes. The win is bounded and conditional. Concrete trigger
conditions for revisiting are listed below.

## Current network-to-disk data path

Source-of-truth references (all paths relative to repo root):

1. **Wire decoder.** The transport delivers raw bytes through
   `MultiplexReader` (`crates/transfer/src/reader/multiplex.rs:28-321`),
   which strips the 4-byte multiplex header and dispatches non-`MSG_DATA`
   frames (`Info`, `Warning`, `IoError`, `NoSend`, `Redo`, `ErrorExit`,
   `Log`, `Client`, `ErrorXfer`) before exposing the payload to readers
   (`multiplex.rs:166-223`).
2. **Token reader.** `TokenReader::read_token`
   (`crates/transfer/src/token_reader.rs:130-151`) reads a 4-byte
   little-endian header off the demuxed stream and yields either
   `DeltaToken::Literal(LiteralData::Pending(len))`,
   `DeltaToken::BlockRef(idx)`, or `DeltaToken::End`. In compressed mode
   the variant `LiteralData::Ready(Vec<u8>)` carries already-inflated
   bytes.
3. **Token loop.** `transfer_ops/token_loop.rs:96-200` drains tokens.
   For a literal, it copies the payload into a recycled `Vec<u8>` (via
   `literal_to_buf`) and forwards it as `FileMessage::Chunk(Vec<u8>)`
   over the SPSC channel
   (`crates/transfer/src/pipeline/spsc.rs`). Block matches are resolved
   against the basis file `MapFile` and sent as a freshly populated
   `Vec<u8>`.
4. **Pipeline channel.** `FileMessage`
   (`crates/transfer/src/pipeline/messages.rs:21-45`) carries `Begin`,
   per-chunk `Chunk(Vec<u8>)`, the coalesced `WholeFile { begin, data }`
   single-shot variant, `Commit`, `Abort`, `Shutdown`.
5. **Disk thread.** `disk_commit/thread.rs:209-223` and
   `disk_commit/process.rs:75` consume chunks. `make_writer`
   (`process.rs:277-313`) selects one of `Writer::IoUring { batch }`
   (Linux + `io_uring` feature, non-sparse, non-append),
   `Writer::Iocp { batch }` (Windows + `iocp`), `Writer::Macos`
   (macOS, `F_NOCACHE` above 1 MiB), or
   `Writer::Buffered(ReusableBufWriter)`. `ReusableBufWriter`
   (`writer.rs:71-122`) buffers below 8 KiB and falls through to
   `writev`/`write_all` above the threshold.

The wire bytes therefore cross userspace at least twice in the current
plain (uncompressed) path: once when `MultiplexReader` extracts a frame
payload, once when `literal_to_buf` copies the payload into the
chunk-channel `Vec`. The disk thread then writes that `Vec` via `write(2)`
or `IORING_OP_WRITE`/`IORING_OP_WRITE_FIXED`. Compressed mode adds an
inflate copy.

## splice/vmsplice API constraints

`splice(2)` (`man 2 splice`, Linux 2.6.17+) moves bytes between two file
descriptors without copying through userspace - but **one of the two
descriptors must be a pipe**. The valid permutations relevant here are:

- `socket -> pipe` (source-side splice)
- `pipe -> regular_file` (sink-side splice)
- `socket -> file` requires a pipe pair as intermediary: the standard
  "pipe trick" is `splice(socket, pipe_write); splice(pipe_read, file)`,
  encoded in `splice_fd_to_file_via_pipe`
  (`crates/fast_io/src/splice.rs:219-281`).

`vmsplice(2)` moves user pages into a pipe by reference (no copy). It only
supports `user_buf -> pipe`. The complement (`pipe -> user_buf`) exists
but copies, so it gives no zero-copy benefit. The kernel holds a
reference to the user pages until the consumer drains them; the caller
must not mutate the buffer before the matching `splice(pipe, fd)` returns.
This invariant is documented on `try_vmsplice_to_file`
(`splice.rs:361-420`) and `SplicePipe::vmsplice_to_file`
(`splice.rs:748-797`).

Practical limits:

- Per-call ceiling is the pipe buffer capacity, typically 64 KiB by
  default. `SplicePipe::with_capacity` raises this via `F_SETPIPE_SZ`
  (`splice.rs:665-682`); 1 MiB is the documented default
  (`DEFAULT_PIPE_CAPACITY`, `splice.rs:97`).
- Splice cannot target every filesystem; tmpfs, FUSE, NFS, and some
  network filesystems either reject the call or fall back to a kernel
  copy that is no faster than `write(2)`.
- A bytes-in-pipe count must be fully drained before the next phase-1
  splice, otherwise the pipe deadlocks. `drain_pipe_to_fd`
  (`splice.rs:289-331`) handles this.
- splice does not see TLS-encrypted or SSH-tunneled data. The source fd
  must deliver plaintext rsync bytes; that excludes `transport::ssh`
  (data is decrypted in userspace by libssh2/`ssh` subprocess) and any
  future TLS daemon path.

## Where this would integrate

Two integration points are conceivable:

### A. Socket-direct splice on the demultiplexed payload (rejected)

Splicing directly from the rsync TCP socket into the destination file
**will not work**. The rsync protocol multiplexes control frames inline
with data (`MSG_INFO`, `MSG_WARNING`, `MSG_ERROR`, `MSG_IO_ERROR`,
`MSG_NO_SEND`, `MSG_REDO`, `MSG_ERROR_EXIT`, `MSG_LOG`, `MSG_DATA`).
Every 4-byte multiplex header must be parsed in userspace to know whether
the next N bytes are payload or a control message that must be handled
synchronously (e.g. `MSG_ERROR_EXIT` aborts the loop;
`MultiplexReader::dispatch_message` at `multiplex.rs:166-223`). On top of
that, each literal token is preceded by a 4-byte length and may be
followed by an arbitrary token boundary. The kernel splice path has no
hook to inspect or split the byte stream. Routing the raw socket into a
pipe would mix control frames into the on-disk file.

### B. vmsplice on already-demuxed literal payloads (the only viable shape)

The integration point that respects the protocol layering is the literal
branch of `token_loop.rs:133-151`. Today that branch:

1. Knows the literal length up front (`LiteralData::Pending(len)`).
2. Reads `len` bytes from the demuxed reader into a recycled `Vec<u8>`.
3. Sends the `Vec` over an SPSC channel to the disk thread, which writes
   it via the selected `Writer` backend.

A splice-flavoured variant would:

1. Read `len` bytes off the demuxed reader into a recycled `Vec<u8>`
   (unavoidable - the bytes still have to pass through `MultiplexReader`
   so non-DATA frames are dispatched).
2. Hand the `Vec` to a path that calls `try_vmsplice_to_file` against
   the destination fd, bypassing the SPSC handoff entirely.

That collapses the disk-side `write(2)` into a `vmsplice + splice` pair.
It does not eliminate any userspace copy upstream of the literal buffer,
because `MultiplexReader` must own its read into a staging buffer to
preserve frame boundaries.

## Performance hypothesis

| Path stage                              | Current (plain) | With B (vmsplice) |
|-----------------------------------------|-----------------|-------------------|
| socket -> kernel sk_buff (TCP)          | unavoidable     | unavoidable       |
| sk_buff -> userspace via `recv(2)`      | 1 copy          | 1 copy            |
| userspace stage buffer -> chunk `Vec`   | 1 copy          | 1 copy            |
| chunk `Vec` -> disk fd                  | `write(2)` / io_uring | `vmsplice + splice` (0 copies) |
| Page-cache landing                      | yes             | yes               |

The hypothesised win is **one userspace-to-kernel copy on the disk-write
side, for literal tokens >= the pipe-buffer threshold (64 KiB on a default
kernel, 1 MiB with `F_SETPIPE_SZ`)**. Concretely, a 32 KiB literal token
saves nothing - it falls below the 64 KiB `SPLICE_THRESHOLD`
(`splice.rs:88`) and `recv_fd_to_file` already short-circuits to
`read/write` below that threshold (`splice.rs:482-495`).

### When this loses

- **Small tokens.** Multiplex frame size caps at 64 KiB
  (`MULTIPLEX_READER_BUFFER_CAPACITY`, `multiplex.rs:54`). The amortised
  cost of building a pipe pair (`pipe2` + two `fcntl`) plus two `splice`
  syscalls per chunk exceeds the cost of a single buffered `write` for
  literals under 64 KiB. Reusing one `SplicePipe` per file mitigates the
  setup cost but not the per-chunk syscall count.
- **Compressed transfers (`-z`).** `CompressedTokenDecoder` returns
  already-inflated payload as `Vec<u8>` (`token_reader.rs:145-148`); the
  source bytes never were on the wire as literal-sized chunks. vmsplice
  applies here too, but the page-cache benefit is identical to a plain
  `write` because the buffer is already in userland.
- **io_uring is already wired.** `IoUringDiskBatch`
  (`crates/fast_io/src/io_uring/disk_batch.rs:45`) batches writes across
  files and uses `IORING_OP_WRITE_FIXED` when registered buffers are
  available, which itself avoids the per-SQE `get_user_pages` cost.
  splice and io_uring are not complementary on the same byte: vmsplice
  references userspace pages exactly like a registered buffer, but with
  pipe-buffer-bounded chunk size and a mandatory two-syscall split.
- **Non-Linux platforms.** macOS, Windows, and BSD do not have splice.
  The fallback in `recv_fd_to_file` is plain `read/write`, identical to
  the current path. Maintenance of two code paths buys nothing on those
  targets.
- **SSH transport.** SSH and TLS see only ciphertext on the socket. The
  receive path lives entirely in userspace because the cipher stream is
  decrypted there. vmsplice on the plaintext literal `Vec` is still
  possible but the per-byte syscall count goes up while the copy count
  stays flat compared to a buffered `write`.
- **Sparse mode.** Zero-run detection requires byte inspection
  (`crates/transfer/src/delta_apply/sparse.rs`). splice has no equivalent
  hook; sparse files would force the buffered path regardless.

## Comparison with the existing io_uring path

`IoUringDiskBatch` already provides kernel-side queueing for the
disk-write side: many SQEs submitted in one `io_uring_enter`, optional
SQPOLL kernel-thread polling, and `WRITE_FIXED` with pre-registered
buffer slots so the kernel does not need to pin user pages per call.
On the same buffer, `WRITE_FIXED` and `vmsplice + splice` both avoid a
data copy. The differences:

| Property                          | `WRITE_FIXED` (io_uring) | `vmsplice + splice`  |
|-----------------------------------|--------------------------|----------------------|
| Per-write syscalls                | 0 (batched in `io_uring_enter`) | 2 (`vmsplice`, `splice`) |
| Chunk size ceiling                | buffer-registration size (typically MiB) | pipe capacity (64 KiB default, 1 MiB max) |
| Cross-file batching               | yes                      | no (one pipe per file) |
| Kernel requirement                | 5.6+ (`io_uring`)        | 2.6.17+              |
| Works on tmpfs/FUSE/NFS           | yes (falls back to `WRITE`) | partially (kernel copies) |
| Pairs with sparse-write           | no                       | no                   |
| Already wired in receiver         | yes (`process.rs:277-313`) | no                 |

The conclusion is that on Linux 5.6+, the io_uring path is strictly
broader than the splice path: it supports larger chunks, batches across
files, and matches the existing `Writer` enum dispatch. On older
kernels (Linux 2.6.17 to 5.5) splice is the only zero-copy option
available, but the chunk-size and per-chunk-syscall asymmetry remain.

## Interaction with multiplex framing

The multiplex layer is the load-bearing reason splice cannot front the
socket directly. Every `MSG_*` frame other than `MSG_DATA` requires a
userspace action:

- `MSG_INFO` / `MSG_CLIENT` -> printed to stdout
  (`multiplex.rs:169-175`).
- `MSG_WARNING` / `MSG_LOG` -> printed to stderr (`multiplex.rs:176-181`).
- `MSG_ERROR*` -> printed to stderr (`multiplex.rs:182-190`).
- `MSG_ERROR_EXIT` -> sets `error_exit_code`, aborts the read loop
  (`multiplex.rs:191-207`).
- `MSG_IO_ERROR` -> OR'd into the receiver's `io_error` accumulator,
  forwarded via `take_io_error()` (`multiplex.rs:114-128`).
- `MSG_NO_SEND` -> enqueued for the receiver's `take_no_send_indices()`
  consumer (`multiplex.rs:146-160`).
- `MSG_REDO` -> enqueued for `take_redo_indices()` (`multiplex.rs:130-144`).

Any zero-copy ingest must therefore start *after* demultiplexing.
`SplicePipe` could in principle be fed by a separate "DATA-only" pipe
that the demuxer writes into, but that re-introduces the userspace copy
the splice was meant to eliminate. The only realistic win is on the
disk-write tail of an already-demuxed `Vec<u8>` payload, which is the
shape described in section B above.

## Recommendation: defer

Defer wiring the existing splice/vmsplice primitives into the receiver.

Rationale:

- The chunk-size ceiling (64 KiB default pipe, 1 MiB max with privilege)
  is below the typical io_uring `WRITE_FIXED` chunk size, so any splice
  hot path would issue more syscalls per byte.
- On Linux 5.6+ (the platform where splice would matter most),
  `IoUringDiskBatch` already provides batched, zero-page-pin writes and
  is the default for non-sparse non-append commits.
- Multiplex framing forces a userspace stage buffer in every realistic
  shape, so the maximum theoretical saving is one `memcpy` per literal
  on the disk-write side, not a full zero-copy pipeline.
- Upstream rsync 3.4.1 ships no splice or vmsplice usage in `io.c`,
  `fileio.c`, or `receiver.c`. Adding it on the oc-rsync side does not
  improve interop and is not a feature parity gap.
- The `splice.rs` primitives are kept for callers that need them outside
  the receiver hot path (e.g. ad-hoc fd-to-file plumbing) and to keep
  the option available without re-implementing from scratch.

### Trigger conditions for revisiting

Implement integration B only when **all** of the following are true:

1. A benchmark shows the disk-write `memcpy` (from chunk `Vec` into the
   page cache via `write(2)`) accounts for >= 5% of receiver wall time
   on a Linux box where io_uring is **unavailable** (kernel < 5.6,
   seccomp blocks io_uring, or `--no-io-uring`).
2. The typical literal-token size in the targeted workload is >= 256 KiB
   (so it clears the pipe-buffer ceiling and amortises the two-syscall
   cost).
3. The transport is not SSH or TLS (so plaintext payload reaches the
   receiver).
4. Sparse mode is off (`--sparse` cannot use splice).
5. The destination filesystem is one where `splice` returns true
   zero-copy, not the kernel-fallback copy path (ext4/xfs/btrfs on a
   local block device).

If conditions 1-5 hold, integration B becomes a net win on the order of
one `memcpy` per literal token. Below that threshold the implementation
and test surface cost (cross-platform CI, fallback paths, pipe-fd budget
accounting) exceeds the benefit.

## Implementation sequencing (if revisited)

If a future benchmark satisfies the trigger conditions, implement in this
order:

1. **Add a `Writer::Splice { pipe: SplicePipe, file: File }` variant** to
   `crates/transfer/src/disk_commit/writer.rs`, gated on
   `cfg(target_os = "linux")`. Implement `Write::write` as
   `pipe.vmsplice_to_file(data, file.as_raw_fd())`. Mirror the
   `commit_file` path used by `IoUring`/`Iocp` variants. Keep the
   `Buffered` fallback for sparse/append paths.
2. **Extend `make_writer`** (`disk_commit/process.rs:277-313`) with a
   policy: select `Splice` only when (a) `IoUring` is not selected and
   (b) the file's target size (`size_hint`) is above the pipe-buffer
   ceiling. Add a `ZeroCopyPolicy` consultation so `--no-zero-copy`
   disables it, mirroring `is_splice_enabled`
   (`fast_io/src/splice.rs:121-124`).
3. **Per-file pipe reuse.** Construct one `SplicePipe::with_capacity`
   per file in the disk thread and reuse it across all chunks of that
   file. The pipe lifetime matches `ActiveFile`; do not rebuild it per
   chunk.
4. **Fd budget guard.** Each `SplicePipe` consumes two file descriptors.
   The disk thread is single-threaded today (`IoUringDiskBatch` is
   `!Send`, `process.rs:42-44`), so the budget is one pipe pair plus
   the open file - inside any reasonable `RLIMIT_NOFILE`. Document this
   invariant alongside the new variant.
5. **Test coverage.** Add integration tests under
   `crates/transfer/tests/` that round-trip a literal-heavy file
   through the splice path and assert (a) byte equality, (b) the
   selected `Writer` variant via a debug accessor, (c) the
   `ZeroCopyPolicy::Disabled` opt-out, and (d) graceful fallback when
   the destination filesystem rejects splice (force `EINVAL` via a
   tmpfs mount in CI). Mirror the parity-test pattern already used in
   `crates/fast_io/tests/splice_integration.rs`.

Throughout, keep the implementation behind feature parity with the
existing `BackendPolicy` plumbing so a single CLI flag can select or
disable splice independently of io_uring and IOCP.

## Open questions

- Whether `vmsplice`'s page-pinning semantics interact badly with
  `BufferPool`'s `Vec<u8>` recycling: a recycled buffer must not be
  returned to the pool until the pipe drain completes. The current
  pipeline returns buffers from the disk thread via a `buf_return_rx`
  channel after `write` completes (`token_loop.rs:185`); the splice
  variant must wait on `drain_pipe_to_fd` before recycling, otherwise
  the kernel reads from a freed allocation. This is identical to the
  invariant io_uring already enforces, so the same machinery applies.
- Whether `tee(2)` could route an in-pipe payload to both the disk fd
  and the in-memory checksum verifier without an extra copy. This is a
  follow-on, not part of the initial scope.
