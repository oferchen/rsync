# Windows feature support matrix

Consolidated reference for all Windows-specific feature support in oc-rsync.
Covers I/O paths, filesystem features, security, protocol behaviour
differences, and performance optimizations.

This document extends the general Windows support overview in
`docs/user/windows-support-matrix.md` with per-feature implementation
details, source file references, and explicit comparison to the Linux
behaviour. Operators planning Windows deployments should read both
documents.

Tracks WSD-3 under parent #3325 (Windows support depth series).

## Status legend

- **Full** - implemented, CI-validated, no known gaps.
- **Partial** - works for common cases; documented limitations exist.
- **Stub** - compiles but returns `Ok(())`, `Ok(None)`, or
  `ErrorKind::Unsupported`. No real functionality.
- **N/A** - concept does not apply on Windows (no equivalent exists).

## 1. I/O path features

| Feature | Status | Notes | Source file(s) |
|---------|--------|-------|----------------|
| IOCP completion port | Full | Async overlapped file reads/writes via `CreateIoCompletionPort` + `GetQueuedCompletionStatusEx`. Auto-sized depth (4 ops/CPU, clamped 8-64). Min file size 64 KB; smaller files use synchronous I/O. Linux equivalent: io_uring. | `crates/fast_io/src/iocp/` |
| IOCP socket I/O | Full | `WSARecv`/`WSASend` with overlapped completion, shared pump thread with file I/O. Linux equivalent: io_uring socket_reader/socket_writer. | `crates/fast_io/src/iocp/socket.rs` |
| TransmitFile (zero-copy) | Full | Synchronous `TransmitFile()` for file-to-socket DMA. Feature-gated (`transmitfile`). Falls back to `WSASend` on SMB/DFS/encrypted volumes. Linux equivalent: `sendfile(2)`. | `crates/fast_io/src/iocp/transmit_file.rs` |
| CopyFileExW | Full | Kernel-level file copy with `COPY_FILE_NO_BUFFERING` for files > 4 MB. Bypasses system cache for large sequential transfers. Linux equivalent: `copy_file_range`. | `crates/fast_io/src/copy_file_ex.rs`, `crates/fast_io/src/platform_copy/dispatch.rs` |
| ReFS reflink (FSCTL_DUPLICATE_EXTENTS) | Full | O(1) copy-on-write block clone on ReFS volumes. Both whole-file and partial-range variants. Volume detection cached per-process. Linux equivalent: `FICLONE` ioctl. | `crates/fast_io/src/platform_copy/dispatch.rs`, `crates/fast_io/src/refs_detect.rs` |
| splice / vmsplice | N/A | Linux-only zero-copy pipe primitives. No Windows equivalent. Network-to-file path uses IOCP overlapped I/O instead. | `crates/fast_io/src/splice/` (Linux-only) |
| copy_file_range | N/A | Linux 4.5+ syscall. Windows uses `CopyFileExW` + ReFS reflink as the equivalent dispatch chain. | `crates/fast_io/src/copy_file_range.rs` (Linux-only) |
| sendfile | N/A | Linux/macOS zero-copy file-to-socket. Windows uses `TransmitFile()` as the direct equivalent. | `crates/fast_io/src/sendfile/` (Unix-only) |
| io_uring | N/A | Linux 5.6+ async I/O ring. IOCP is the permanent Windows replacement. The io_uring stub module provides compile-time API compatibility. | `crates/fast_io/src/io_uring_stub/` |
| IOCP disk batch writer | Full | Batched overlapped `WriteFile` with configurable chunk size (256 KB). Mirrors io_uring `disk_batch` calling convention. Single completion port reused across files. | `crates/fast_io/src/iocp/disk_batch/` |
| Page-aligned buffers | Full | `VirtualAlloc`-backed allocation for `FILE_FLAG_NO_BUFFERING`. Avoids kernel bounce-copy on unbuffered writes. Linux equivalent: page-aligned `alloc` for O_DIRECT. | `crates/fast_io/src/page_aligned.rs` |
| Delete-on-close temp files | Full | `FILE_FLAG_DELETE_ON_CLOSE` for crash-safe temp file lifecycle. Kernel removes file if process exits before commit. Commit via `SetFileInformationByHandle` to clear disposition + `MoveFileExW`. Linux equivalent: `O_TMPFILE` + `linkat`. | `crates/fast_io/src/win_tmpfile/` |

## 2. Filesystem features

| Feature | Status | Notes | Source file(s) |
|---------|--------|-------|----------------|
| Long paths (`\\?\` prefix) | Full | Paths exceeding MAX_PATH (260 chars) automatically prefixed with `\\?\` by the path builder. Audited under WPC-5/WPC-6. No equivalent concern on Linux (PATH_MAX = 4096, rarely hit). | Audited in `docs/audit/windows-long-path-support.md` |
| Alternate data streams (ADS) | Partial | Surfaced via `FindFirstStreamW`/`FindNextStreamW` through the xattr pipeline. Requires `-X` to transfer; silently dropped otherwise with a one-shot warning. Linux has no equivalent (uses xattrs natively). | `crates/metadata/src/xattr_windows.rs` |
| Reparse points (junctions) | Partial | Classified by reparse tag. Junctions followed like directory symlinks. OneDrive/Cloud Files API placeholders detected but not transparently hydrated. Linux equivalent: bind mounts (not protocol-visible). | Audited in `docs/audit/windows-reparse-point-classification.md` |
| Case-insensitive FS handling | Full | Receiver detects source-side name collisions (`a.txt` vs `A.txt`) before applying writes. Linux filesystems are case-sensitive by default (no collision possible). | Audited in `docs/audit/windows-case-insensitive-conflict-detection.md` |
| Sparse files (`-S`) | Full | Zero-run detection is cross-platform via `fast_io::zero_detect` (SIMD-accelerated). Sparse holes created by seeking past zero runs. Uses the same `SparseWriteState` as Linux. NTFS supports sparse natively. | `crates/fast_io/src/zero_detect.rs`, `crates/transfer/src/delta_apply/sparse.rs` |
| ReFS filesystem detection | Full | Cached per-volume query via `GetVolumeInformationByHandleW`. Guards reflink attempts (NTFS does not support block cloning). | `crates/fast_io/src/refs_detect.rs` |
| Hard links | Full | `std::fs::hard_link` on NTFS. Same-volume constraint enforced by the OS. Identical behaviour to Linux. | Standard library |
| Symbolic links | Partial | `std::os::windows::fs::symlink_file`/`symlink_dir`. Requires `SeCreateSymbolicLinkPrivilege` or Developer Mode. Linux: unrestricted for non-root. | Standard library |
| O_TMPFILE | N/A | Linux-only anonymous temp file. Windows uses `FILE_FLAG_DELETE_ON_CLOSE` as equivalent (see I/O path section). | `crates/fast_io/src/o_tmpfile/` (Linux-only) |
| openat2 / dirfd sandbox | N/A | Unix-only strict-resolution open. Windows uses handle-based NTFS APIs that sidestep path TOCTOU naturally. | `crates/fast_io/src/secure_dir.rs` (Unix-only) |

## 3. Security features

| Feature | Status | Notes | Source file(s) |
|---------|--------|-------|----------------|
| DACL read/write (`-A`) | Partial | Maps POSIX ACL triples to NTFS DACL allow-ACEs via `GetNamedSecurityInfoW`/`SetNamedSecurityInfoW`. SACL intentionally skipped (requires `SE_SECURITY_NAME`). Inherited ACEs may be re-materialized as explicit ACEs. Not validated in Active Directory. Linux: POSIX ACLs via `exacl`. | `crates/metadata/src/acl_windows/` |
| DACL-to-POSIX mode mapping | Full | Bidirectional conversion between POSIX permission bits and DACL ACEs for interop with Unix endpoints. | `crates/metadata/src/acl_windows/posix_map.rs` |
| SDDL round-trip | Partial | Security descriptors serialized as SDDL strings through the xattr pipeline for cross-platform storage. | `crates/metadata/src/acl_windows/sddl.rs` |
| Owner/group (`-o`, `-g`) | Partial | NTFS uses SIDs, not uid/gid. Names round-trip via `LookupAccountNameW` when `-A` is present. Without `-A`, numeric IDs are passthrough only. No AD validation. Linux: direct `chown`/`lchown`. | `crates/metadata/src/acl_windows/common.rs` |
| Permission bits (`-p`) | Partial | Only the read-only flag is mapped from the POSIX write bit. SUID/SGID/sticky bits silently ignored (no NTFS equivalent). Linux: full 12-bit mode preservation. | `crates/metadata/src/apply/permissions.rs` |
| `--chmod` modifiers | Partial | Parsed and applied to wire-format POSIX mode bits only. Does not affect NTFS DACLs unless `-A` also present. | `crates/metadata/src/chmod/` |
| `--usermap`/`--groupmap`/`--chown` | Stub | Returns `MappingParseError`. No POSIX-to-SID mapping table exists. Linux: NSS-backed uid/gid remapping. | `crates/metadata/src/mapping_win.rs` |
| `--fake-super` | Stub | Returns `ErrorKind::Unsupported`. Requires xattr-backed `user.rsync.%stat` which depends on ADS semantics not matching POSIX xattr namespace. | `crates/metadata/src/fake_super.rs` |
| `--copy-as=USER[:GROUP]` | Stub | No-op guard returns `Ok(())`. No Windows `seteuid`/`setegid` equivalent. | `crates/metadata/src/copy_as.rs` |
| Restricted tokens / privilege dropping | N/A | No `setuid`/`setgid` on Windows. Daemon mode runs at the service account's privilege level. | `crates/platform/src/privilege.rs` |
| File secrets permission check | Stub | POSIX mode check (0600) not applicable on NTFS. Secrets file access is not validated on Windows. | `crates/platform/src/secrets.rs` |
| Windows service SCM integration | Full | `oc-rsync --daemon` installable as Windows service. SCM lifecycle events mapped to `SignalFlags` atomics. | `crates/platform/src/windows_service.rs` |

## 4. Protocol features with different Windows behaviour

| Feature | Status | Notes | Source file(s) |
|---------|--------|-------|----------------|
| Symlinks (`-l`) | Partial | Requires elevated privilege or Developer Mode. Wire format is identical; receiver must classify target as file or directory for correct Win32 API call. Linux: unprivileged, single `symlink(2)` call. | Standard library |
| Device files (`-D`, `--specials`) | Stub | No `mknod` on Windows. Returns `Ok(())` without creating the special file. Matches upstream rsync's Cygwin behaviour. | `crates/metadata/src/special.rs` |
| FIFOs / Unix sockets | Stub | Same as device files - no-op on Windows. | `crates/metadata/src/special.rs` |
| Ownership wire format | Partial | Numeric uid/gid round-trips on the wire unchanged. Name-to-SID resolution attempted only with `-A`. Without it, ownership is passthrough. Linux: full NSS-backed name/id resolution. | `crates/metadata/src/ownership_stub.rs`, `crates/metadata/src/id_lookup/nss_stub.rs` |
| Timestamps | Full | NTFS supports 100 ns resolution (better than ext4's 1 ns but stored differently). mtime and atime preserved via `filetime` crate. crtime not exposed on native Windows. | `crates/metadata/src/apply/timestamps.rs` |
| Extended attributes (`-X`) | Partial | Maps to NTFS ADS. Wire format is the same UTF-8 name+value pairs. Stream name syntax (`path:name:$DATA`) is internal. Without `-X`, ADS silently dropped with one-shot warning. | `crates/metadata/src/xattr_windows.rs` |
| NFSv4 ACLs | Partial | Stored via xattr backend. End-to-end NFSv4-to-DACL conversion not fully audited. | `crates/metadata/src/nfsv4_acl.rs` |
| `--numeric-ids` | Full | Passthrough; no NSS lookup attempted on Windows (same as having no NSS at all). | `crates/metadata/src/id_lookup/nss_stub.rs` |
| Daemon `use chroot` | N/A | No `chroot(2)` on Windows. Daemon relies on path-based access control and module root enforcement. | `crates/daemon/` |
| Symlink munging (daemon) | Full | Cross-platform `/rsyncd-munged/` prefix logic. Protects against symlink escape when `use chroot = no`. | `crates/metadata/src/symlink_munge.rs` |
| Signal handling | Partial | `SetConsoleCtrlHandler` maps CTRL_C/CTRL_CLOSE to shutdown. No SIGHUP-equivalent for config reload (named events not wired). Linux: full POSIX signal set. | `crates/fast_io/src/signal/`, `crates/platform/src/signal.rs` |
| Batch file format | Full | Platform-independent wire format. Symlink replay defaults to file symlinks (batch format does not encode target type). | `crates/core/` |

## 5. Performance features

| Feature | Status | Notes | Source file(s) |
|---------|--------|-------|----------------|
| Zero-copy file-to-socket (TransmitFile) | Full | Kernel DMA from file cache to socket send queue. 64 KB per-send granularity. Equivalent throughput to Linux `sendfile`. Falls back on SMB/DFS. | `crates/fast_io/src/iocp/transmit_file.rs` |
| Buffer alignment (VirtualAlloc) | Full | Page-aligned buffers for `FILE_FLAG_NO_BUFFERING`. Eliminates kernel bounce copies. Equivalent to Linux page-aligned `O_DIRECT` buffers. | `crates/fast_io/src/page_aligned.rs` |
| Async I/O (IOCP overlapped) | Full | Multiple overlapped ops in flight per completion port. Equivalent to io_uring SQ batching. Auto-scales with CPU count. | `crates/fast_io/src/iocp/config.rs` |
| Unbuffered large-file copy | Full | `COPY_FILE_NO_BUFFERING` flag on `CopyFileExW` for files > 4 MB. Avoids cache pollution on large sequential transfers. Linux equivalent: `O_DIRECT` or `copy_file_range`. | `crates/fast_io/src/copy_file_ex.rs` |
| ReFS block-clone (CoW) | Full | O(1) file duplication on ReFS volumes regardless of size. Linux equivalent: `FICLONE` on Btrfs/XFS. Not available on NTFS (most common FS). | `crates/fast_io/src/platform_copy/dispatch.rs` |
| SIMD checksums (AVX2/SSE2) | Full | Same SIMD fast paths as Linux x86_64. Runtime feature detection cached in `OnceLock`. | `crates/checksums/` |
| SIMD zero-detect for sparse | Full | AVX2 (32 bytes/cycle) and SSE2 (16 bytes/cycle) zero-run detection for sparse hole optimization. Same code path as Linux x86_64. | `crates/fast_io/src/zero_detect.rs` |
| Parallel stat (rayon) | Full | Cross-platform. Same threshold-based dual-path pattern. | `crates/engine/` |
| Rayon parallel file processing | Full | Cross-platform. No Windows-specific limitations. | Workspace-wide |
| SEND_ZC (zero-copy send) | N/A | Linux 6.0+ `IORING_OP_SEND_ZC`. Windows uses `TransmitFile` as the zero-copy send primitive instead. | `crates/fast_io/src/io_uring/send_zc.rs` (Linux-only) |
| SQPOLL (kernel-side submission) | N/A | Linux io_uring feature. No Windows equivalent needed - IOCP completion port handles are kernel-managed. | `crates/fast_io/src/sqpoll_basis.rs` (Linux-only) |

## 6. Summary of Linux-to-Windows equivalents

| Linux mechanism | Windows equivalent | Parity level |
|----------------|-------------------|--------------|
| io_uring | IOCP (completion ports) | Functional parity for file and socket I/O |
| sendfile(2) | TransmitFile() | Functional parity (zero-copy file-to-socket) |
| splice/vmsplice | No direct equivalent | IOCP overlapped I/O covers the use case |
| copy_file_range | CopyFileExW | Functional parity (kernel-level copy) |
| FICLONE (Btrfs/XFS) | FSCTL_DUPLICATE_EXTENTS (ReFS) | ReFS-only; NTFS has no equivalent |
| O_TMPFILE + linkat | FILE_FLAG_DELETE_ON_CLOSE + MoveFileExW | Functional parity |
| O_DIRECT | FILE_FLAG_NO_BUFFERING | Functional parity (requires aligned buffers) |
| openat2 RESOLVE_BENEATH | Handle-based NTFS APIs | Structural parity (different mechanism, same security) |
| POSIX ACLs | NTFS DACLs | Partial (lossy POSIX-to-DACL mapping) |
| chown/lchown | SID-based ownership | Partial (no uid/gid concept) |
| mknod | None | N/A (no special files on NTFS) |
| POSIX signals | Console control handler + named events | Partial (Ctrl+C only; no SIGHUP) |

## 7. Known gaps and limitations

### Structural limitations (will not be resolved)

- **No io_uring on Windows**: IOCP is the permanent replacement.
- **No special files**: FIFOs, sockets, block/character devices cannot
  exist on NTFS.
- **No SUID/SGID/sticky bits**: No NTFS equivalent. Silently ignored.
- **No chroot**: Daemon relies on path-based access control.
- **No uid/gid ownership model**: SIDs are fundamentally different.

### Validation gaps (implemented but not stress-tested)

- **IOCP not profiled on physical NTFS hardware** (CI uses virtualized
  storage on GitHub Actions runners).
- **DACL/ACL not validated in Active Directory** or cross-domain trust
  environments.
- **ARM64 Windows**: Not built or tested.
- **OneDrive/Cloud Files placeholders**: Detected but not hydrated.
  Transfers may produce zero-length copies.
- **NFSv4-to-DACL end-to-end conversion**: Not fully audited.
- **Windows Event Log**: Daemon logs to STDERR only.

## 8. Cross-references

- `docs/user/windows-support-matrix.md` - general Windows support overview
  with feature table and maturity levels.
- `docs/platform-support.md` - full cross-platform feature matrix
  (Linux/macOS/Windows side-by-side).
- `docs/user/xattr-acl-cross-platform.md` - xattr and ACL
  cross-platform details.
- `docs/audit/windows-ads-handling.md` - ADS audit (WPC-1).
- `docs/audit/windows-long-path-support.md` - long path audit (WPC-5/6).
- `docs/audit/windows-reparse-point-classification.md` - reparse points
  (WPC-8/9).
- `docs/audit/windows-dacl-ace-inheritance.md` - DACL round-trip
  (WPC-10).
- `docs/audit/windows-case-insensitive-conflict-detection.md` - case
  handling (WPC-11).
- `docs/audit/windows-perm-bits-posix-mapping.md` - permission mapping
  (WPC-12).
- `docs/audit/windows-copyfileex-platform-copy.md` - data-path dispatch
  chain.

Tracking: WSD-3 (#3327) under parent #3325.
