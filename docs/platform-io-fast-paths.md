# Platform I/O Fast Paths

oc-rsync selects the best available I/O mechanism at runtime on each platform, falling back through increasingly portable options. All fast paths are implemented in the `fast_io` crate and exposed via safe public APIs. Consumer crates (`engine`, `transfer`, `core`) never contain unsafe I/O code.

## Fallback Chain Overview

| Priority | Linux | macOS | Windows |
|----------|-------|-------|---------|
| 1 | FICLONE (CoW reflink) | clonefile (CoW) | ReFS FSCTL_DUPLICATE_EXTENTS (planned) |
| 2 | io_uring (batched I/O) | fcopyfile | CopyFileExW (no-buffering) |
| 3 | copy_file_range (zero-copy) | std::fs::copy | std::fs::copy |
| 4 | sendfile (file-to-socket) | - | - |
| 5 | Standard buffered I/O | Standard buffered I/O | Standard buffered I/O |

Each mechanism independently falls back on failure (e.g., NFS/FUSE mounts, old kernels, seccomp restrictions, unsupported filesystem).

---

## Linux

### FICLONE (reflink)

**What it does.** The `FICLONE` ioctl creates an instant copy-on-write clone where source and destination share storage blocks until either is modified. O(1) regardless of file size.

**When oc-rsync uses it.** First choice for all local file copies via `PlatformCopy::copy_file`. Requires Linux 4.5+ and a CoW-capable filesystem - Btrfs, XFS (with reflink enabled), or bcachefs. Called through `rustix::fs::ioctl_ficlone` (fully safe, no raw FFI).

**Expected perf impact.** Near-instant for any file size. Eliminates all data transfer for same-filesystem copies on supported filesystems.

**Fallback.** Returns `EOPNOTSUPP` on ext4, tmpfs, NFS, FUSE, or cross-device copies. The dispatch chain then tries `copy_file_range`.

### io_uring

**What it does.** Batches multiple read/write syscalls into a single `io_uring_enter` call, reducing kernel transitions. Supports file I/O and socket I/O.

**When oc-rsync uses it.** File copies >= 256 KB and socket I/O when the `io_uring` cargo feature is enabled. Runtime detection checks kernel >= 5.6, probes `io_uring_setup(2)` for seccomp compatibility, and caches the result in a process-wide atomic. Supports SQPOLL mode (needs `CAP_SYS_NICE`), fixed-file descriptors, and registered buffer groups.

**Expected perf impact.** Reduces per-file syscall overhead for large transfers. Most beneficial for high-throughput bulk copies where kernel transition cost dominates.

**Fallback.** Returns `Unsupported` on non-Linux, kernels < 5.6, or when seccomp blocks the syscalls. Falls through to `copy_file_range` or standard I/O.

### copy_file_range

**What it does.** Zero-copy file-to-file transfer in kernel space. Data stays in the page cache without round-tripping through userspace buffers.

**When oc-rsync uses it.** File copies >= 64 KB on Linux 4.5+ (same-filesystem) or 5.3+ (cross-filesystem). Used as the second tier in `copy_file_contents()` after io_uring.

**Expected perf impact.** Eliminates one memcpy per chunk compared to read/write. Most visible on large sequential transfers where the saved copy saturates memory bandwidth.

**Fallback.** Returns an error on non-Linux, old kernels, or cross-device on kernel < 5.3. Falls through to standard buffered read/write with a 256 KB buffer.

### sendfile

**What it does.** Zero-copy file-to-socket transfer. Data moves directly from the page cache to the network stack without passing through userspace.

**When oc-rsync uses it.** File-to-socket transfers >= 64 KB on Linux. Used for daemon-mode transfers where file data is sent over a TCP socket.

**Expected perf impact.** Eliminates one userspace buffer copy per chunk. Sends data in chunks up to ~2 GB to avoid signal interruption.

**Fallback.** Returns `Unsupported` on non-Linux. Falls through to buffered read/write with a 256 KB buffer.

### O_TMPFILE

**What it does.** Creates an anonymous file with no directory entry. The file is materialized atomically via `linkat` when complete. If dropped before linking, the kernel reclaims the inode - no cleanup needed.

**When oc-rsync uses it.** Receiver-side temp file creation on Linux 3.11+ with a supporting filesystem (ext4, xfs, btrfs, tmpfs). The `open_temp_file` function probes support once and returns a typed result for fallback.

**Expected perf impact.** Eliminates the risk of orphaned temp files on crash. Reduces directory entry operations by one create + one unlink compared to named temp files.

**Fallback.** Returns `Unavailable` on non-Linux, old kernels, or unsupported filesystems (NFS, FUSE). Callers fall back to named temporary files.

### Memory-mapped I/O

**What it does.** Maps a file directly into the process address space via `mmap`, avoiding explicit read syscalls.

**When oc-rsync uses it.** Read-only access for files >= 64 KB on Unix. Used for basis file reads during delta transfer where random access patterns benefit from demand paging.

**Expected perf impact.** Reduces syscall overhead for random access. Most beneficial for large files with non-sequential read patterns (e.g., delta matching against a basis file).

**Fallback.** Falls back to standard buffered I/O if mapping fails (NFS, FUSE, procfs) or on non-Unix platforms.

### Batched metadata (statx)

**What it does.** Groups metadata operations by type and processes them together. Uses Linux's `statx()` for more efficient metadata retrieval.

**When oc-rsync uses it.** File list building and quick-check comparisons when processing >= 8 files. Operations are reordered for cache locality but results match original input order.

**Expected perf impact.** Better cache locality and reduced context switches for large batches. Below the threshold of 8 operations, individual calls avoid grouping overhead.

**Fallback.** Non-Linux Unix uses standard library calls with grouping. Windows uses `filetime` crate for timestamps and readonly-attribute mapping for permissions.

---

## macOS

### clonefile

**What it does.** Creates an instant copy-on-write clone on APFS where source and destination share storage blocks until either is modified. O(1) regardless of file size.

**When oc-rsync uses it.** First choice for all local file copies via `PlatformCopy::copy_file`. Destination must not already exist. Source and destination must be on the same APFS volume.

**Expected perf impact.** Near-instant for any file size on APFS. Eliminates all data transfer.

**Fallback.** Fails on HFS+, cross-volume copies, or when the destination already exists. Falls through to `std::fs::copy` (which uses Darwin `copyfile()` internally, handling metadata and resource forks).

### fcopyfile

**What it does.** Kernel-accelerated data copy between open file descriptors using the Darwin `copyfile` API with `COPYFILE_DATA`. The kernel may use server-side copy, CoW, or optimized buffer transfer depending on the filesystem.

**When oc-rsync uses it.** Available as an explicit call via `try_fcopyfile`. Unlike `clonefile`, it works across different filesystems and on non-APFS volumes (HFS+, NFS, SMB).

**Expected perf impact.** Avoids userspace buffer copies when the kernel can optimize the transfer internally. Performance depends on the filesystem.

**Fallback.** Returns `Unsupported` on non-macOS. Callers use `std::fs::copy` as the portable path.

---

## Windows

### CopyFileExW

**What it does.** Windows kernel file copy API. For files > 4 MB, oc-rsync sets the `COPY_FILE_NO_BUFFERING` flag to bypass the system cache, reducing memory pressure and improving throughput for large sequential copies.

**When oc-rsync uses it.** All local file copies on Windows via `PlatformCopy::copy_file`. The no-buffering threshold is 4 MB.

**Expected perf impact.** Unbuffered mode avoids polluting the filesystem cache with transient transfer data. Most beneficial for large file workloads.

**Fallback.** On failure, cleans up the partial destination and falls back to `std::fs::copy`.

### ReFS reflink (FSCTL_DUPLICATE_EXTENTS)

**What it does.** Block-level copy-on-write clone on ReFS volumes, similar to Linux FICLONE. O(1) regardless of file size.

**When oc-rsync uses it.** Detection infrastructure is implemented - `is_refs_filesystem()` queries volume type via `GetVolumeInformationByHandleW` and caches results per volume root. The actual `FSCTL_DUPLICATE_EXTENTS_TO_FILE` call is not yet wired into the dispatch chain. Requires Windows Server 2016+ or Windows 10+ with ReFS-formatted volumes.

**Expected perf impact.** When implemented, will provide instant copies on ReFS - the same benefit as FICLONE on Linux or clonefile on macOS.

**Fallback.** Currently always falls through to `CopyFileExW`. Once implemented, will fall through on non-ReFS volumes or cross-volume copies.

---

## Cross-Platform Abstractions

### PlatformCopy trait

The `PlatformCopy` trait (Strategy Pattern) abstracts all platform-specific copy logic behind a unified interface. `DefaultPlatformCopy` auto-selects the best mechanism per platform. The engine injects this via dependency inversion, making copy strategy testable and swappable.

### FileReader / FileWriter traits

The `FileReader` and `FileWriter` traits abstract I/O backends (standard buffered, mmap, io_uring) behind a common interface. Factory traits (`FileReaderFactory`, `FileWriterFactory`) allow runtime selection without changing application code.

### Standard buffered I/O

The universal fallback on all platforms. Uses `BufReader`/`BufWriter` with 64 KB default buffers, or a 256 KB buffer for bulk file copy operations. The `copy_file_contents_buffered` variant accepts a caller-provided buffer from the engine's buffer pool to eliminate per-file heap allocation.
