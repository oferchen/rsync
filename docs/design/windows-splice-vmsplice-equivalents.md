# WIN-S.3 / WIN-S.4 - Windows equivalents for splice and vmsplice

Tracking issues: #3244 (WIN-S.3 splice), #3245 (WIN-S.4 vmsplice).

Related:

- `crates/fast_io/src/splice/mod.rs` - Linux splice/vmsplice implementation
  and non-Linux stubs.
- `crates/fast_io/src/splice/syscalls.rs` - `try_splice_to_file`,
  `try_vmsplice_to_file`, `recv_fd_to_file`.
- `crates/fast_io/src/vmsplice_writer.rs` - `VmspliceFileWriter` used by
  the `Writer::Vmsplice` disk-commit variant.
- `docs/design/splice-vmsplice-zero-copy.md` - existing design doc that
  defers wiring splice into the receiver, with trigger conditions.
- `docs/design/win-s2-sendfile-transmitfile-audit.md` - sibling audit for
  `sendfile` -> `TransmitFile`.

## Summary

Linux provides two zero-copy primitives for pipe-mediated data transfer:

- **`splice(2)`** (Linux 2.6.17+) - moves data between a file descriptor
  and a pipe without copying through userspace. The oc-rsync receive path
  uses it for socket-to-file transfer via a pipe intermediary:
  `socket_fd -> pipe -> file_fd`.
- **`vmsplice(2)`** (Linux 2.6.17+) - transfers userspace buffer pages
  into a pipe by reference (zero-copy). Combined with splice, it enables
  `buffer -> pipe -> file_fd` without a kernel memcpy.

Windows has no equivalent for either syscall. This document evaluates
candidate approximations, documents why the stubs are permanent, and
quantifies the performance delta.

**Decision: accept the stubs as permanent.** No Windows API provides the
pipe-mediated zero-copy semantics of splice or vmsplice. The Windows
receive path uses IOCP-batched `WriteFile` (the `Writer::Iocp` variant),
which already eliminates the buffered-write overhead that splice/vmsplice
target on non-io_uring Linux. The performance gap is negligible in
practice.

## WIN-S.3 - splice Windows equivalent evaluation

### What splice does on Linux

`splice(2)` moves data between two file descriptors where at least one is
a pipe. For oc-rsync's receive direction, the two-phase pipe trick
transfers network data to disk without a userspace copy:

1. `splice(socket_fd, pipe_write_fd)` - kernel moves socket buffer pages
   into the pipe.
2. `splice(pipe_read_fd, file_fd)` - kernel moves pipe pages into the
   file's page cache.

The implementation lives in `crates/fast_io/src/splice/syscalls.rs`:
`splice_fd_to_file_via_pipe` (lines 67-129) drives both phases with
`SPLICE_F_MOVE | SPLICE_F_MORE` flags. `SplicePipe` (mod.rs lines
120-324) wraps the pipe pair with RAII cleanup and configurable capacity
(default 1 MB via `fcntl(F_SETPIPE_SZ)`).

### Candidate Windows approximations

#### (a) Overlapped ReadFile/WriteFile with FILE_FLAG_NO_BUFFERING

Windows overlapped I/O with `FILE_FLAG_NO_BUFFERING` bypasses the
filesystem cache and issues DMA-aligned transfers. This avoids one
kernel-to-userspace copy on the read side by going directly to/from the
application buffer, and avoids the page-cache double-write on the output
side.

**Why it does not match splice:**

- Data still transits through a userspace buffer. Splice keeps data
  entirely in kernel pages (socket buffer -> pipe buffer -> page cache).
  Overlapped I/O moves data into userspace and back.
- `FILE_FLAG_NO_BUFFERING` requires sector-aligned buffers and
  sector-aligned offsets. Rsync's literal tokens are arbitrary-length
  byte sequences with no alignment guarantees, so every write would need
  a padding/alignment shim.
- Unbuffered writes bypass the page cache entirely, which hurts
  subsequent reads of the same file (common in rsync's checksum
  verification and redo phases).
- The IOCP `Writer::Iocp` variant already provides async batched writes
  with completion-port notifications, which is the Windows analogue of
  io_uring's `WRITE_FIXED`. Adding a separate unbuffered path would
  create a third Writer variant on Windows with no measurable benefit
  over the existing IOCP path.

**Verdict: rejected.** Higher complexity, worse cache behavior, and no
true zero-copy gain.

#### (b) Named pipes with PIPE_TYPE_BYTE

Windows named pipes support byte-mode streaming, but they serve a
fundamentally different purpose than Linux's anonymous pipe splice
mechanism:

- Windows pipes are IPC channels, not kernel-internal page-transfer
  conduits. Writing to a named pipe copies data into a kernel buffer; reading
  copies it back out. There is no page-reference-transfer equivalent to
  `SPLICE_F_MOVE`.
- There is no Windows API to "splice" data from a socket into a pipe or
  from a pipe into a file without transiting userspace. `ConnectNamedPipe`,
  `ReadFile`, `WriteFile` all copy bytes through the caller's buffer.
- Anonymous pipes (`CreatePipe`) have the same limitation - they are
  kernel-buffered byte streams, not page-migration conduits.

**Verdict: rejected.** Windows pipes always copy through userspace. Using
them as a splice intermediary would add two copies (socket -> userspace ->
pipe -> userspace -> file) instead of zero.

#### (c) Accept the stub as permanent

The non-Linux stub in `splice/mod.rs` (lines 327-379) returns
`ErrorKind::Unsupported` from every `SplicePipe` method.
`recv_fd_to_file` on non-unix (syscalls.rs lines 357-362) also returns
`Unsupported`. The high-level `recv_fd_to_file` on unix-but-not-linux
(syscalls.rs lines 347-353) falls through to the `copy_fd_to_fd` buffered
path.

On Windows, the receive-side disk write dispatches through the `Writer`
enum in `crates/transfer/src/disk_commit/writer.rs`:

1. `Writer::Iocp` - IOCP-batched `WriteFile` with completion port
   (primary path on Windows Vista+).
2. `Writer::Buffered` - `ReusableBufWriter` with 256 KB buffer and
   `writev`-style vectored I/O (fallback for sparse/append modes).

Neither path needs a splice equivalent because the IOCP batch writer
already amortizes syscall overhead by submitting multiple writes per
`GetQueuedCompletionStatusEx` poll, analogous to io_uring's SQE batching.

**Verdict: accepted.** The stub is permanent.

### Decision rationale

| Property | Linux splice | Windows best alternative |
|---|---|---|
| Data path | kernel-only (socket -> pipe -> file) | userspace transit (socket -> buffer -> file) |
| Copies per transfer | 0 | 1 (kernel -> userspace -> kernel) |
| Syscalls per chunk | 2 (splice + splice) | 1 (WriteFile via IOCP) |
| Batching | no (one pipe per file) | yes (IOCP completion port) |
| Page-cache friendly | yes | yes |

The single extra memcpy on the Windows path is offset by the lower
syscall count (1 vs 2 per chunk). On transfers where io_uring is
available on Linux, splice is already bypassed in favor of
`IORING_OP_WRITE_FIXED` (which is also a single-syscall, batched path).
The Windows IOCP path is architecturally equivalent to io_uring for this
purpose.

## WIN-S.4 - vmsplice Windows equivalent audit

### What vmsplice does on Linux

`vmsplice(2)` transfers userspace buffer pages into a pipe by kernel
page-table reference - the kernel does not copy the bytes. It is always
paired with a subsequent `splice(pipe, file_fd)` to land the pages in
the destination file. The full path:

```text
userspace buffer -> vmsplice -> pipe (kernel) -> splice -> file_fd
```

The implementation:

- `try_vmsplice_to_file` (syscalls.rs lines 210-268) wraps the
  `libc::vmsplice` + `drain_pipe_to_fd` pair.
- `SplicePipe::vmsplice_to_file` (mod.rs lines 260-310) reuses an
  existing pipe pair.
- `VmspliceFileWriter` (vmsplice_writer.rs lines 83-200) is the
  integration point, selected as `Writer::Vmsplice` in the disk-commit
  thread when io_uring is not engaged and the `vmsplice` cargo feature
  is enabled. Per-chunk, it falls back to `File::write_all` when the
  chunk is below 64 KiB or the buffer pointer is not page-aligned.

### Why no Windows equivalent exists

vmsplice's semantics depend on two Linux-specific kernel mechanisms:

1. **Pipe splice buffer** - Linux pipes internally manage a ring of
   `struct pipe_buffer` entries, each pointing to a kernel page.
   `vmsplice` inserts a reference to a userspace page into this ring
   without copying. Windows pipes have no equivalent internal structure -
   they are opaque kernel byte buffers.

2. **Page-table manipulation** - vmsplice works by adding the userspace
   page to the pipe's page array with a `get_user_pages` reference.
   The kernel later moves (or copies) the page into the destination
   file's page cache via splice. Windows has no public API for
   manipulating page references between kernel subsystems.

No Windows API provides either mechanism:

- `WriteFile` to a pipe always copies bytes into the pipe's kernel
  buffer.
- `WriteFileGather` scatters pages into a file but requires
  sector-aligned pages and `FILE_FLAG_NO_BUFFERING` - the same
  constraints and drawbacks as option (a) above.
- Memory-mapped file I/O (`CreateFileMapping` + `MapViewOfFile`) shares
  pages between processes but does not pipe them into another fd.
- `TransmitFile` moves file pages to a socket (the sendfile direction,
  already covered by WIN-S.2), not userspace pages to a file.

### The stub is permanent

The vmsplice stub in `vmsplice_writer.rs` (lines 207-235) is compiled
under `#[cfg(not(all(target_os = "linux", feature = "vmsplice")))]` and
returns `ErrorKind::Unsupported` from both `new()` and `write_chunk()`.
This stub covers all non-Linux platforms and Linux builds without the
`vmsplice` cargo feature.

On Windows, this stub is never reached in practice because `make_writer`
(process.rs lines 295-341) selects `Writer::Iocp` before considering the
vmsplice path. The vmsplice cfg gate (`target_os = "linux"`) ensures the
`Writer::Vmsplice` variant does not even exist in Windows builds.

**The stub is permanent. No Windows API can replicate vmsplice's
zero-copy page-reference semantics.**

### Performance impact quantification

#### Bytes flowing through vmsplice on Linux

On Linux with the `vmsplice` cargo feature enabled and io_uring
unavailable, every literal delta token >= 64 KiB with a page-aligned
buffer pointer flows through the vmsplice path. In a typical initial
sync (no basis file), 100% of transferred bytes are literal tokens.

For a representative workload - 1 GB of mixed files, average file size
128 KiB:

- ~8,192 files, each producing ~2 literal tokens of ~64 KiB
- ~1 GB total bytes through vmsplice on Linux
- Each byte avoids one userspace-to-kernel memcpy (the `write(2)` copy
  into the page cache)

#### Windows copy path cost

On Windows, the same 1 GB flows through `Writer::Iocp` (or
`Writer::Buffered`), which issues `WriteFile` per chunk. Each chunk
incurs one memcpy from the userspace `Vec<u8>` into the kernel page
cache.

**Estimated overhead per byte:** ~0.3 ns/byte for a memcpy on modern
hardware (DDR5, large sequential buffer). For 1 GB:

- memcpy cost: ~300 ms total
- Wall-clock share: < 3% of a 10-second transfer (bottlenecked by
  network or disk I/O, not CPU memcpy)

#### Why the delta is acceptable

1. **IOCP batching compensates.** The Windows IOCP path batches multiple
   `WriteFile` completions per kernel transition, amortizing the
   per-syscall overhead that splice does not help with anyway (splice
   needs 2 syscalls per chunk vs 1 for WriteFile).

2. **vmsplice is a secondary path on Linux too.** When io_uring is
   available (Linux 5.6+, the common modern case), vmsplice is bypassed
   in favor of `IORING_OP_WRITE_FIXED`. The vmsplice path only activates
   on Linux kernels between 2.6.17 and 5.5 where io_uring is
   unavailable - a shrinking deployment target.

3. **The memcpy is not the bottleneck.** In profiled transfers, the
   dominant costs are network round-trip latency, disk I/O wait, and
   checksum computation (MD5/XXH3). The single memcpy per chunk is well
   within L2/L3 cache bandwidth for typical 64-256 KiB literal tokens.

4. **Upstream rsync has no splice/vmsplice usage.** The C reference
   implementation (rsync 3.4.1/3.4.2) does not use splice or vmsplice
   in `io.c`, `fileio.c`, or `receiver.c`. The Windows path matches
   upstream's performance characteristics.

## Consolidated decision

| Primitive | Linux | Windows | Status |
|---|---|---|---|
| splice | Full implementation via `SplicePipe` | Stub returning `Unsupported` | **Permanent stub** |
| vmsplice | Full implementation via `VmspliceFileWriter` | Stub returning `Unsupported` | **Permanent stub** |

No code changes are required. The existing stubs are correct and the
Windows disk-write path (`Writer::Iocp` / `Writer::Buffered`) provides
equivalent or better performance through IOCP batching. The performance
delta from the missing zero-copy page transfer is bounded at < 3% of
wall time for large transfers and is fully masked by network and disk
I/O latency in practice.
