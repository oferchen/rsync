# fast_io

High-performance I/O abstractions with platform-specific optimizations and
safe fallback paths.

## Purpose

`fast_io` is the designated encapsulation layer for unsafe I/O optimizations.
Consumer crates (`engine`, `transfer`, `core`) depend on it through safe public
APIs only. Every unsafe optimization has a safe fallback so callers work on all
platforms and filesystem types (NFS, FUSE, etc.).

## Key Public Types and Modules

- `io_uring` - Linux 5.6+ batched syscalls (statx, rename, linkat, read, write)
- `iocp` - Windows I/O Completion Ports for overlapped async file I/O
- `platform_copy` - trait abstracting FICLONE/copy_file_range/clonefile/CopyFileExW
- `copy_file_range` - zero-copy file-to-file on Linux 4.5+
- `sendfile` - zero-copy file-to-socket
- `splice` / `vmsplice_writer` - zero-copy socket-to-file (Linux)
- `mmap_reader` - memory-mapped basis-file access
- `syscall_batch` - batched metadata ops with runtime threshold tuning
- `DirSandbox` - path-traversal-safe directory operations
- `landlock` - Landlock LSM allowlist (Linux 5.13+)
- `kqueue` - macOS readiness-driven event loop
- `adaptive_dispatch` - runtime I/O backend selection with throughput feedback
- `zero_detect` - 16-byte u128 zero-run detection for sparse writes

## Dependencies (upstream)

`logging` (only workspace crate dependency)

## Dependents (downstream)

`engine`, `transfer`, `daemon`, `core`, `cli`

## I/O Fallback Chain

1. FICLONE - instant CoW reflink (Linux Btrfs/XFS/bcachefs)
2. io_uring - batched syscalls (Linux 5.6+)
3. copy_file_range - in-kernel copy (Linux 4.5+)
4. sendfile - file-to-socket zero-copy
5. Standard read/write - universal fallback

## Platform Notes

- **Linux**: io_uring (5.6+), SQPOLL (5.13+), copy_file_range, splice,
  vmsplice, sendfile, Landlock LSM, FICLONE
- **macOS**: clonefile, fcopyfile, F_NOCACHE, writev, kqueue
- **Windows**: CopyFileExW, IOCP, ReFS reflink, TransmitFile
