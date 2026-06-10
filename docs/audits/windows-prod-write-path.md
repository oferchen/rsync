# Windows production write path trace (WIN-S.LAND.2)

This audit traces what actually runs on Windows when oc-rsync performs a regular
file copy through the local-copy executor (`engine::local_copy`). It answers a
single question: which `fast_io` primitive executes the bytes-to-disk step on
Windows, and how does it differ from the Linux/macOS equivalents?

Scope is the local-copy entry point at
`crates/engine/src/local_copy/executor/file/copy/`. The network-receive
disk-commit pipeline (`crates/transfer/src/disk_commit/`) is covered separately
at the end because, although it shares the same `fast_io` crate, its writer
selection is independent and IOCP only fires there.

Companion: WIN-S.LAND.1 catalogs which fast_io primitives are *stubbed* on
Windows. This document catalogs which one *runs*.

## ASCII call chain (local-copy executor, Windows, whole-file)

```
LocalCopyExecution -> ... -> file::copy::transfer::execute::execute_transfer
  crates/engine/src/local_copy/executor/file/copy/transfer/execute/mod.rs:55

  +-- (mac-only)  clonefile::try_clone         [#[cfg(target_os = "macos")]]
  +-- (linux-only) iouring::try_dispatch        [#[cfg(target_os="linux",
  |                                              feature="iouring-data-writes")]]
  |
  +-- open_destination_writer
  |     crates/engine/src/local_copy/executor/file/copy/transfer/
  |       write_strategy.rs:114
  |     => std::fs::OpenOptions::open(destination)
  |        => Windows: CreateFileW(GENERIC_WRITE, ..., FILE_ATTRIBUTE_NORMAL)
  |           (no FILE_FLAG_OVERLAPPED, no FILE_FLAG_NO_BUFFERING,
  |            no FILE_FLAG_SEQUENTIAL_SCAN)
  |
  +-- maybe_preallocate_destination  (no-op on Windows: posix_fallocate stub)
  |
  +-- CopyContext::copy_file_contents
        crates/engine/src/local_copy/context_impl/transfer.rs:276

      // sparse=false, compress=false, no bandwidth limiter, initial_bytes=0
      // is the common whole-file case below.
      |
      +-- fast_io::copy_file_range::copy_file_contents_buffered
            crates/fast_io/src/copy_file_range.rs:107

            +-- try_io_uring_copy
            |     #[cfg(not(all(target_os="linux", feature="io_uring")))]
            |     -> Err(ErrorKind::Unsupported)   [STUB on Windows]
            |
            +-- try_copy_file_range
            |     #[cfg(not(target_os="linux"))]
            |     -> Err(ErrorKind::Unsupported)   [STUB on Windows]
            |
            +-- copy_file_contents_readwrite_with_buffer
                  crates/fast_io/src/copy_file_range.rs:384

                  loop {
                    source.read(&mut buffer[..to_read])?;
                      // std::fs::File::read on Windows ->
                      //   handle_ms::Handle::read ->
                      //     kernel32::ReadFile(handle, buf, len, &mut read, NULL)
                    destination.write_all(&buffer[..n])?;
                      // std::fs::File::write_all -> File::write ->
                      //   handle_ms::Handle::write ->
                      //     kernel32::WriteFile(handle, buf, len, &mut wrote, NULL)
                  }
```

Terminal Win32 syscalls for the local-copy executor regular-file path:

- File open: `CreateFileW` (no async flags, no cache hints)
- Bulk write: `WriteFile` (synchronous, default cache, no overlapped)
- Bulk read: `ReadFile` (synchronous, default cache)

There is no `CopyFileExW`, no `TransmitFile`, no `WriteFileGather`, no
`FSCTL_DUPLICATE_EXTENTS_TO_FILE`, no overlapped IOCP, no `FILE_FLAG_NO_BUFFERING`
on this path. The buffer chunk is sourced from the engine `BufferPool` (size
controlled by `adaptive_buffer_size(file_size)`, typically 256 KiB) and is reused
across files.

## Per-layer notes

### Layer 1: `execute_transfer` (engine)

`execute_transfer` is the entry point reached from both the CLI and daemon for
every regular-file copy. The two whole-file fast-path branches it would attempt
before normal copy are gated out on Windows:

| Branch | Cfg gate | Windows result |
|---|---|---|
| `clonefile::try_clone` (APFS reflink) | `#[cfg(target_os = "macos")]` | not compiled |
| `iouring::try_dispatch` (registered buffers) | `#[cfg(all(target_os = "linux", feature = "iouring-data-writes"))]` | not compiled |

So on Windows control unconditionally falls through to
`open_destination_writer` + `copy_file_contents`.

### Layer 2: `open_destination_writer`

Uses `std::fs::OpenOptions::{create_new,truncate,write}.open(destination)` for
all five write strategies (`Direct`, `Inplace`, `Append`, `TempFileRename`,
`AnonymousTempFile`). Rust's standard library maps this on Windows to:

```
CreateFileW(path, GENERIC_WRITE | (read?GENERIC_READ:0),
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            NULL, <disposition>, FILE_ATTRIBUTE_NORMAL, NULL)
```

There is no opportunity here to pass `FILE_FLAG_OVERLAPPED`,
`FILE_FLAG_SEQUENTIAL_SCAN`, or `FILE_FLAG_NO_BUFFERING`; the engine never
constructs the handle via the `windows-sys` `CreateFileW` wrapper that
`fast_io::iocp::file_writer` uses.

### Layer 3: `CopyContext::copy_file_contents`

Selects between three sub-paths based on flags:

| Condition | Sub-path |
|---|---|
| has delta signature | `copy_file_contents_with_delta` (delta apply loop) |
| `sparse` | `copy_file_contents_sparse` (SparseWriter decorator + std write_all) |
| else (the **hot path** for whole-file copies) | `fast_io::copy_file_range::copy_file_contents_buffered` |

The non-delta, non-sparse branch is the only one that touches `fast_io` at all.

### Layer 4: `fast_io::copy_file_range::copy_file_contents_buffered`

Despite the module name, this function is a tiered dispatcher with two stubbed
tiers on Windows:

```rust
if length >= IO_URING_COPY_THRESHOLD {            // 256 KiB
    if let Ok(n) = try_io_uring_copy(...) { return Ok(n); }
}
if length >= COPY_FILE_RANGE_THRESHOLD {          // 64 KiB
    if let Ok(n) = try_copy_file_range(...) { return Ok(n); }
}
copy_file_contents_readwrite_with_buffer(...)
```

On Windows:

- `try_io_uring_copy` is the stub at `copy_file_range.rs:247-253`
  (`#[cfg(not(all(target_os = "linux", feature = "io_uring")))]`) returning
  `ErrorKind::Unsupported`.
- `try_copy_file_range` is the stub at `copy_file_range.rs:329-335`
  (`#[cfg(not(target_os = "linux"))]`) returning `ErrorKind::Unsupported`.
- Control unconditionally reaches `copy_file_contents_readwrite_with_buffer`
  for every file size.

The size thresholds are dead code on Windows: both branches return immediately
without entering the inner loop.

### Layer 5: `copy_file_contents_readwrite_with_buffer`

Plain `Read::read` / `Write::write_all` loop on `&std::fs::File`. Rust's
`std::fs` on Windows implements these via `sys::pal::windows::handle::Handle::{read,write}`,
which directly call `ReadFile` and `WriteFile` with the synchronous
(non-`OVERLAPPED`) form. No `WriteFileEx`, `WriteFileGather`, `TransmitFile`,
or `CopyFileExW` is involved.

## Cross-platform equivalence table

Primitive selected for a non-sparse, non-compressed, non-delta, whole-file copy
of size `N`:

| N (bytes) | Linux (io_uring feature on) | Linux (default) | macOS | Windows |
|---|---|---|---|---|
| < 64 KiB | `read`/`write` (`copy_file_contents_readwrite_with_buffer`) | `read`/`write` | `clonefile(2)` if APFS + eligible, else `read`/`write` | `ReadFile`/`WriteFile` |
| 64 KiB - 256 KiB | `copy_file_range(2)` | `copy_file_range(2)` | `clonefile(2)` if APFS + eligible, else `read`/`write` | `ReadFile`/`WriteFile` |
| >= 256 KiB | io_uring `Read`+`Write` opcodes (`try_io_uring_copy`) | `copy_file_range(2)` | `clonefile(2)` if APFS + eligible, else `read`/`write` | `ReadFile`/`WriteFile` |

Throughput implication: on Windows, regardless of file size, every byte traverses
userspace twice (read into buffer, write out of buffer). Linux at the same size
class either keeps the bytes inside the kernel (`copy_file_range`) or batches
the syscalls through a single ring submission. The closest semantic peer to
Linux's `copy_file_range` on NTFS is `CopyFileExW` (kernel-mode block transfer,
optionally cache-bypassing). On ReFS the peer is `FSCTL_DUPLICATE_EXTENTS_TO_FILE`
(O(1) reflink). Both exist as wrappers in `fast_io` but the local-copy executor
never calls them.

## Is `FILE_FLAG_NO_BUFFERING` engaged on the production path?

No. The flag is wrapped in two places in `fast_io`:

| Wrapper | Threshold | Local-copy executor calls it? |
|---|---|---|
| `fast_io::copy_file_ex::try_copy_file_ex` | 4 MiB (`NO_BUFFERING_THRESHOLD`) | no - only `engine::local_copy::win_copy::copy_file_optimized`, which has no production callers |
| `fast_io::iocp::IocpConfig.unbuffered` | runtime-configured | no - only the network-receive disk-commit thread |

Searching for `copy_file_optimized` outside its own module and tests returns
zero hits. Searching for `clone_or_copy` (the macOS clonefile entry that also
funnels through `DefaultPlatformCopy::copy_file`) returns zero hits. The
`DefaultPlatformCopy` Windows branch in `crates/fast_io/src/platform_copy/dispatch.rs:92-119`
(ReFS reflink, then `CopyFileExW`, then `std::fs::copy`) is unreachable from
`execute_transfer` on Windows.

The 4 MiB threshold for no-buffering is therefore not exercised by the local-copy
write path. It is exercised by `dispatch::platform_copy_impl` if anything ever
calls `DefaultPlatformCopy::copy_file` on Windows, which currently nothing does
in production code.

## Gap summary

Per-file regular-file copies on Windows degrade to a portable `read`/`write`
loop with a stack-shaped fallback chain whose first two tiers are unconditional
stubs. The Windows-native fast paths exist as code (`copy_file_ex`,
`try_refs_reflink`, `IocpDiskBatch`) but the local-copy executor never reaches
them. Three measurable gaps follow:

1. **No CoW reflink for ReFS local copies.** `try_refs_reflink_impl` is wired
   only via `DefaultPlatformCopy::copy_file`; the local-copy executor calls
   neither. Linux Btrfs/XFS gets `FICLONE` via the same dispatcher (also unused
   here), but the Linux path has a second mechanism `copy_file_range` that the
   executor *does* use directly, so Linux still wins kernel-side bytes movement.
   Windows has no equivalent direct call.
2. **No cache-bypass for large local copies.** `COPY_FILE_NO_BUFFERING` is only
   reachable via `try_copy_file_ex`, which the executor does not call. Large
   sequential writes pollute the Windows file cache.
3. **No async / overlapped I/O for local copies.** `IocpWriter` and
   `IocpDiskBatch` exist and are wired for the receiver pipeline
   (`crates/transfer/src/disk_commit/writer.rs:151`), but the local-copy
   executor opens its destinations with `std::fs::OpenOptions` (no
   `FILE_FLAG_OVERLAPPED`) and cannot retrofit IOCP. Linux's analogous
   `IoUringDiskBatch` is wired into local copy via the
   `iouring-data-writes` feature; there is no Windows counterpart for that
   entry point.

## Aside: network-receive disk-commit path

For completeness, the receiver-side pipeline at
`crates/transfer/src/disk_commit/writer.rs:144-163` does select the IOCP writer
when the `iocp` feature is enabled, `use_sparse=false`, and `append_offset=0`:

```
Writer::Iocp { batch: &mut fast_io::IocpDiskBatch }
  -> IocpDiskBatch::write_data
     -> IocpWriter (crates/fast_io/src/iocp/file_writer.rs)
        => CreateFileW(..., FILE_FLAG_OVERLAPPED | FILE_GENERIC_WRITE)
        => WriteFile(handle, buf, len, NULL, &mut OVERLAPPED)
        => GetQueuedCompletionStatus(port, ...)
```

`IocpConfig::unbuffered` defaults to `false`, so the receiver path does *not*
engage `FILE_FLAG_NO_BUFFERING` by default. `for_large_files()` and
`for_small_files()` presets also leave `unbuffered = false`. The receiver
pipeline therefore runs overlapped, cached, asynchronous `WriteFile` -
distinct from the local-copy executor's synchronous, cached, blocking
`WriteFile`.

This split is the source of the Windows-side perf asymmetry: a network receive
benefits from IOCP batching; an explicit local copy with identical bytes does
not.
