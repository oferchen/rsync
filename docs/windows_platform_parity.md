# Windows Platform Parity Audit

Audit of all `#[cfg(unix)]`, `#[cfg(target_os = "linux")]`, and `#[cfg(not(windows))]` blocks
in `crates/` that represent functionality, documenting which have proper Windows implementations,
no-op stubs, or missing support.

Last updated: 2026-04-21

---

## 1. Implemented on Windows

Features with proper, functional Windows implementations.

### Signal Handling (`crates/platform/src/signal.rs`)

- Unix: `signal_hook` for SIGPIPE, SIGHUP, SIGTERM, SIGINT, SIGUSR1, SIGUSR2.
- Windows: `SetConsoleCtrlHandler` maps CTRL_C/CTRL_CLOSE to shutdown, CTRL_BREAK to graceful exit.
- Note: Config reload (SIGHUP equivalent) requires a named event - not yet implemented.

### Signal Handling - Core (`crates/core/src/signal/mod.rs`)

- Unix: Full handler in `unix` submodule with SIGINT/SIGTERM/SIGHUP/SIGPIPE.
- Windows: Stub in `stub.rs` provides the same API with Ctrl+C support. Full `ShutdownReason` enum, `SignalHandler`, `install_signal_handlers`, and `wait_for_signal` are all present.

### Windows Service Manager (`crates/platform/src/windows_service.rs`)

- Windows-only feature. Provides `run_service_dispatcher`, `ServiceStatusHandle`, `install_service`, `uninstall_service` via Win32 SCM APIs.
- Non-Windows: No-op stubs for `ServiceStatusHandle`.

### Windows User Impersonation (`crates/platform/src/privilege.rs`)

- Unix: `chroot`, `setuid`, `setgid`, `setgroups`.
- Windows: `drop_privileges_windows()` uses `LogonUserW` + `ImpersonateLoggedOnUser` for account impersonation.
- Both platforms have their own implementations, plus cross-platform no-op stubs.

### Windows Account Name Resolution (`crates/platform/src/name_resolution.rs`)

- Windows-only. Uses `LookupAccountNameW`, `GetSidSubAuthority`, `NetUserEnum` to convert between account names and RIDs.
- Non-Windows: Returns `None`.

### Daemon Name Converter (`crates/daemon/src/daemon/sections/name_converter.rs`)

- Unix: Subprocess-based converter using NSS in chroot context.
- Windows: `WindowsNameConverter` using `LookupAccountNameW` and `rid_to_account_name` from platform crate. No subprocess needed since Windows does not use chroot.

### Wire Mode Encoding (`crates/protocol/src/flist/wire_mode.rs`)

- Unix: Identity operations (`S_IFLNK` already matches wire format).
- Windows: Normalizes between Windows `_S_IFLNK` and canonical POSIX `0o120000` wire values. Both `to_wire_mode` and `from_wire_mode` are implemented.

### Platform File Copy (`crates/fast_io/src/platform_copy/dispatch.rs`)

- Linux: FICLONE reflink, then `copy_file_range`, then `std::fs::copy`.
- macOS: `clonefile`, then `fcopyfile`, then `std::fs::copy`.
- Windows: ReFS reflink via `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, then `CopyFileExW` (with `COPY_FILE_NO_BUFFERING` for large files), then `std::fs::copy`.

### CopyFileExW (`crates/fast_io/src/copy_file_ex.rs`)

- Windows-only. FFI wrapper for `CopyFileExW` with optional `COPY_FILE_NO_BUFFERING`.
- Non-Windows: `try_copy_file_ex` returns `Err(Unsupported)`.

### ReFS Reflink Detection (`crates/fast_io/src/refs_detect.rs`)

- Windows: Queries `GetVolumeInformationByHandleW` to detect ReFS volumes, with per-volume caching.
- Non-Windows: Always returns `Ok(false)`.

### Windows Optimized Copy Engine (`crates/engine/src/local_copy/win_copy.rs`)

- Cross-platform module providing `copy_file_optimized`. On Windows, uses `CopyFileExW` with no-buffering for large files. On other platforms, uses `std::fs::copy`.

### Daemon Xfer Exec (`crates/daemon/src/daemon/sections/xfer_exec.rs`)

- Unix: Runs pre/post-xfer exec scripts via `sh -c <command>`.
- Windows: Runs via `cmd /C <command>`. Same environment variables set on both platforms.

### File Permissions (`crates/fast_io/src/syscall_batch.rs`)

- Unix: `utimensat` for timestamps, `PermissionsExt::from_mode` for permissions.
- Windows: `filetime` crate for timestamps, `set_readonly` based on owner write bit for permissions.

### Metadata Cache (`crates/metadata/src/stat_cache.rs`)

- Linux: Uses `statx` with `AT_STATX_DONT_SYNC` for optimal performance.
- Unix (non-Linux): Falls back to `fs::metadata()` with mode/uid/gid fields.
- Windows: `CachedMetadata` stores `readonly` bool instead of mode/uid/gid. `fetch_metadata_optimized` uses `fs::metadata().permissions().readonly()`.

### Preallocate (`crates/engine/src/local_copy/executor/file/preallocate.rs`)

- Unix: Uses `posix_fallocate` (or `fallocate` on Linux).
- Windows/other: Uses `file.set_len(total_len)` as a portable fallback.

### Mmap Reader (`crates/fast_io/src/mmap_reader.rs` / `mmap_reader_stub.rs`)

- Unix: Real memory-mapped I/O with `mmap`, `madvise` (sequential/random/willneed).
- Non-Unix: Full buffered I/O fallback via `mmap_reader_stub.rs`. Reads entire file into memory. Same public API surface (`MmapReader`, `AdaptiveReader`, `AdaptiveReaderFactory`). The `advise_*` methods are no-ops.

### io_uring (`crates/fast_io/src/io_uring/` / `io_uring_stub.rs`)

- Linux with `io_uring` feature: Real io_uring with kernel probing, SQPOLL, registered buffers.
- All other platforms: Full stub module providing every public type and function. Factories return standard buffered I/O readers/writers. `is_io_uring_available()` returns `false`.

---

## 2. Stubbed on Windows (No-op / Returns Ok)

Features with no-op stubs that compile and run but do not perform the Unix operation.

### Ownership (chown) (`crates/metadata/src/apply/ownership.rs`)

- Unix: `chownat`/`fchown` with uid/gid resolution.
- Non-Unix: `set_owner_like()` is a no-op returning `Ok(())`. `apply_ownership_from_entry()` is a no-op returning `Ok(())`.

### Permissions (Full Mode) (`crates/metadata/src/apply/permissions.rs`)

- Unix: `PermissionsExt::from_mode()` preserves full 12-bit mode (suid/sgid/sticky + rwx).
- Non-Unix: Only preserves readonly flag from source metadata. No suid/sgid/sticky/execute bits.

### Copy-As (Effective UID/GID Switch) (`crates/metadata/src/copy_as.rs`)

- Unix: `seteuid`/`setegid` with RAII guard that restores on drop.
- Non-Unix: `CopyAsGuard` is a no-op struct. `switch_effective_ids()` returns `Ok(no-op guard)`. User/group name resolution returns `Err(Unsupported)`.

### Secrets File Permissions (`crates/platform/src/secrets.rs`)

- Unix: Validates mode bits (no other-access) and root ownership.
- Non-Unix: No-op, always returns `Ok(())`.

### Privilege Drop (`crates/platform/src/privilege.rs`)

- Unix: `chroot`, `setuid`, `setgid`, `setgroups`.
- Non-Unix: `drop_privileges()` returns `Ok(())` (no-op). `apply_chroot()` prints a warning and returns `Ok(())`.

### Extended Attributes (`crates/metadata/src/xattr.rs` / `xattr_stub.rs`)

- Unix with `xattr` feature: Full xattr read/write/sync using `xattr` crate. Linux-specific namespace filtering (`user.*`, `system.*`).
- Non-Unix or without feature: `sync_xattrs` and `apply_xattrs_from_list` emit a one-time warning and return `Ok(())`. `read_xattrs_for_wire` returns an empty `XattrList`.

### NFSv4 ACLs (`crates/metadata/src/nfsv4_acl_stub.rs`)

- Unix with `xattr` feature: Real NFSv4 ACL support.
- Non-Unix or without feature: No-op stub module aliased as `nfsv4_acl`.

### POSIX ACLs (`crates/metadata/src/acl_noop.rs`)

- Linux/macOS/FreeBSD with `acl` feature: Real ACL support via `exacl` crate.
- Windows and other platforms: `acl_noop` module provides no-op stubs for `apply_acls_from_cache`, `get_rsync_acl`, `sync_acls`.

### Fake-Super Metadata (`crates/metadata/src/fake_super.rs`)

- Unix: `FakeSuperStat::from_metadata()` reads mode/uid/gid/rdev from actual metadata.
- Non-Unix: Returns default values (`0o100644`, uid=0, gid=0, rdev=None).

### Ownership Helpers (`crates/metadata/src/ownership_stub.rs`)

- Unix: Real uid/gid type conversions via `nix`.
- Non-Unix: Identity functions (`uid_from_raw`, `gid_from_raw` return raw u32 unchanged).

### Mapping (User/Group Name Mapping) (`crates/metadata/src/mapping_win.rs`)

- Unix: `UserMapping`/`GroupMapping` with NSS-backed name resolution.
- Windows: `mapping_win.rs` provides same types with limited functionality.

### UID/GID Lookup (`crates/metadata/src/id_lookup/`)

- Unix: `cache.rs` + `nss.rs` with NSS-backed lookups, thread-local caching.
- Non-Unix: `nss_stub.rs` where `lookup_user_name`/`lookup_group_name` return `None`. `map_uid`/`map_gid` return `Some(id)` unchanged.

### Hard Link Tracking (`crates/engine/src/local_copy/hard_links.rs`)

- Unix: Tracks inode/dev pairs for hard link detection and creation.
- Non-Unix: `HardLinkTracker` is a no-op struct. `existing_target()` returns `None`. `record()` does nothing.

### Device Nodes and FIFOs (`crates/metadata/src/special.rs`, `crates/apple-fs/src/lib.rs`)

- Unix: `mkfifo` and `mknod` via `nix` crate.
- Non-Unix: `create_fifo_inner` and `create_device_node_inner` return `Ok(())` (silently skip). `apple_fs::mkfifo` and `apple_fs::mknod` return `Err(Unsupported)`.

### Symlink Safety (`crates/transfer/src/symlink_safety.rs`, `crates/flist/src/symlink_safety.rs`)

- Unix: Real symlink race-condition checks using `openat`, `fstatat`, `readlinkat`.
- Non-Unix: No-op safety checks (always allow).

### CLI User/Group Lookups (`crates/cli/src/platform.rs`)

- Unix: Uses `uzers` crate for `get_user_by_uid`, `get_group_by_gid`, etc.
- Non-Unix: All lookups return `None`. `supports_user_name_lookup()` and `supports_group_name_lookup()` return `false`.

### Batch Mode Shell Path (`crates/batch/src/replay.rs`, `crates/batch/src/script.rs`)

- Unix: Uses `/bin/sh` for batch replay execution.
- Windows: Uses `cmd.exe` or appropriate Windows shell.

### Quick Check (Receiver) (`crates/transfer/src/receiver/quick_check.rs`)

- Unix: Compares mode bits, uid, gid for change detection.
- Non-Unix: Skips mode/uid/gid comparisons, compares only size and mtime.

### Generator File List Entry (`crates/transfer/src/generator/file_list/entry.rs`)

- Unix: Reads mode, uid, gid, rdev from filesystem metadata.
- Non-Unix: Uses default mode values, zero uid/gid.

### Change Set Detection (`crates/engine/src/local_copy/plan/change_set/detection.rs`)

- Unix: Detects permission, ownership, and device changes using mode/uid/gid.
- Non-Unix: Skips permission/ownership/device change detection (always reports no change).

### Splice (socket-to-file) (`crates/fast_io/src/splice.rs`)

- Linux: Real `splice(2)` with pipe intermediary for zero-copy socket-to-file transfer.
- Non-Unix: `recv_fd_to_file` returns `Err(Unsupported)`. `try_splice_to_file` returns `Err(Unsupported)`.

### Sendfile (file-to-socket) (`crates/fast_io/src/sendfile.rs`)

- Linux: Real `sendfile(2)` syscall for zero-copy file-to-socket transfer.
- Non-Linux Unix: `libc::write` fallback.
- Non-Unix: `send_file_to_fd` writes to `io::sink()` (effectively a no-op).

### Deferred Fsync (`crates/engine/src/local_copy/deferred_sync.rs`)

- Linux: Uses `syncfs` for batch filesystem sync when more than 10 files pending.
- Non-Linux: Falls back to individual `File::sync_all()` per file.

---

## 3. Missing on Windows (Error / Unsupported)

Features that return explicit errors or are unavailable on Windows.

### Daemonization (`crates/platform/src/daemonize.rs`)

- Unix: `fork()`, `setsid()`, redirect stdio to `/dev/null`.
- Non-Unix: `become_daemon()` returns `Err(Unsupported)`. `redirect_stdio_to_devnull()` returns `Err(Unsupported)`.
- Impact: The daemon cannot detach from the terminal on Windows. Windows Service Manager is the intended alternative.

### Chroot (`crates/platform/src/privilege.rs`)

- Unix: Real `chroot(2)` call.
- Non-Unix: Prints warning to stderr and returns `Ok(())`. This is intentional - Windows daemon security uses different mechanisms.

### Socket I/O for io_uring (`crates/fast_io/src/lib.rs`)

- Unix: Exports `IoUringOrStdSocketReader`, `IoUringOrStdSocketWriter`, `socket_reader_from_fd`, `socket_writer_from_fd`.
- Non-Unix: These types are not exported. Code that uses raw fd-based socket I/O is Unix-only.
- Impact: Low - standard buffered I/O is used for network paths on Windows.

### O_TMPFILE Anonymous Temp Files (`crates/fast_io/src/o_tmpfile/`)

- Linux: Uses `O_TMPFILE` flag for anonymous temp file creation with `linkat` to atomically link into the filesystem.
- Non-Linux: Stub returns `OTmpfileSupport::Unavailable`. Falls back to named temp files.

### copy_file_range (`crates/fast_io/src/copy_file_range.rs`)

- Linux: Zero-copy file-to-file transfer in kernel space.
- Non-Linux: Not available (compile-time gated). Standard copy is used instead.

### Sparse File Hole Punching (`crates/engine/src/local_copy/executor/file/sparse/hole_punch.rs`)

- Linux: Uses `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE` for sparse file support.
- Non-Linux: Not available. Sparse writes use seek-past-zeros approach instead.

### SEEK_DATA/SEEK_HOLE Sparse Reader (`crates/engine/src/local_copy/executor/file/sparse/reader.rs`)

- Linux: Uses `SEEK_DATA`/`SEEK_HOLE` for efficient sparse file reading.
- Non-Linux: Uses sequential zero-detection reader.

### fd-based Metadata Application (`crates/metadata/src/lib.rs`)

- Unix: Exports `apply_file_metadata_with_fd` and `apply_file_metadata_with_fd_if_changed` that use `fchown`/`fchmod` on open file descriptors (avoids TOCTOU races).
- Non-Unix: Not exported. Path-based metadata application is used instead.

### Xattr Namespace Filtering (`crates/metadata/src/xattr.rs`)

- Linux: Filters xattrs by namespace (`user.*` for non-root, all except `system.*` for root).
- Non-Linux Unix: No namespace filtering (macOS/FreeBSD model).
- Non-Unix: Xattrs not supported at all (stub).

### Xattr Wire Protocol Prefix Compression (`crates/protocol/src/xattr/prefix.rs`)

- Linux: Supports Linux-specific xattr namespace prefix compression (`user.`, `security.`, `trusted.`, `system.`).
- Non-Linux: Different prefix tables or no compression.

### Daemon Accept Loop Unix Socket (`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`)

- Unix: Supports Unix domain socket listeners alongside TCP.
- Non-Unix: Unix socket paths return error.

### SSH Builder Raw Fd (`crates/rsync_io/src/ssh/builder.rs`)

- Unix: Sets raw file descriptors for SSH child process I/O.
- Non-Unix: Not available (the relevant functions are cfg-gated out).

### Batched Stat with statx (`crates/flist/src/batched_stat/types.rs`)

- Linux: Uses `statx` for batched metadata fetching with minimal mask.
- Non-Linux: Uses standard `fs::metadata`.

### File Guard Deferred Fsync (`crates/engine/src/local_copy/executor/file/guard.rs`)

- Linux: Uses `syncfs` for deferred filesystem-level sync, `fdatasync` via raw fd.
- Non-Linux: Standard `File::sync_all()`.

---

## 4. Fast Paths - Platform-Specific Optimizations

### File Copy Chain

| Priority | Linux | macOS | Windows | Fallback |
|----------|-------|-------|---------|----------|
| 1 | FICLONE (Btrfs/XFS/bcachefs) | `clonefile` (APFS) | ReFS reflink (`FSCTL_DUPLICATE_EXTENTS`) | - |
| 2 | io_uring batched read/write | - | - | - |
| 3 | `copy_file_range` (kernel 4.5+) | `fcopyfile` | `CopyFileExW` (+ `NO_BUFFERING` for >4MB) | - |
| 4 | `std::fs::copy` | `std::fs::copy` | `std::fs::copy` | All platforms |

### Network I/O Chain

| Priority | Linux | macOS | Windows | Fallback |
|----------|-------|-------|---------|----------|
| 1 | `sendfile` (file-to-socket) | - | - | - |
| 2 | `splice` (socket-to-file via pipe) | - | - | - |
| 3 | io_uring socket reader/writer | - | - | - |
| 4 | Buffered read/write | Buffered read/write | Buffered read/write | All platforms |

### Memory-Mapped I/O

| Platform | Implementation |
|----------|---------------|
| Unix | `mmap` with `madvise` hints (sequential, random, willneed) |
| Windows | Reads entire file into `Vec<u8>`. Same API, `advise_*` are no-ops. |

### Metadata Operations

| Operation | Linux | Unix (non-Linux) | Windows |
|-----------|-------|-------------------|---------|
| Stat | `statx` + `AT_STATX_DONT_SYNC` | `stat(2)` | `fs::metadata` (readonly only) |
| Timestamps | `utimensat` (nanosecond) | `utimensat` (nanosecond) | `filetime` crate |
| Permissions | Full 12-bit mode | Full 12-bit mode | Readonly flag only |
| Ownership | `chown`/`fchown` | `chown`/`fchown` | No-op (silently skipped) |
| Xattrs | Full with namespace filtering | Full without namespace filtering | Not supported (one-time warning) |
| ACLs | `exacl` crate | `exacl` crate (macOS/FreeBSD) | Not supported |

### Temp File Strategy

| Platform | Strategy |
|----------|----------|
| Linux | `O_TMPFILE` anonymous temp file + `linkat` for atomic rename |
| Other | Named temp file with `tempfile` crate |

### Sparse File Support

| Operation | Linux | Other |
|-----------|-------|-------|
| Reading | `SEEK_DATA`/`SEEK_HOLE` for efficient hole skipping | Sequential zero-detection |
| Writing | 16-byte `u128` zero-run detection + seek-past-zeros | Same zero-run detection |
| Hole punch | `FALLOC_FL_PUNCH_HOLE` | Not available |

### Sync/Fsync Strategy

| Platform | Batch Sync | Individual Sync |
|----------|------------|-----------------|
| Linux | `syncfs` when >10 files pending | `fdatasync` via raw fd |
| Other | Individual `File::sync_all()` per file | `File::sync_all()` |

---

## Summary

The codebase follows a consistent pattern for platform support:

1. **Core protocol logic** is fully cross-platform.
2. **Metadata operations** degrade gracefully: full POSIX semantics on Unix, readonly-only on Windows, with one-time warnings for unsupported features (xattrs, ACLs).
3. **I/O fast paths** have a complete fallback chain ending in portable `std::fs` operations. Windows gets its own optimized paths (CopyFileExW, ReFS reflink) that are not just stubs.
4. **Daemon infrastructure** has Windows-specific alternatives (SCM service, account impersonation, Win32 name resolution) rather than direct Unix ports.
5. **No compile errors on Windows** - every `#[cfg(unix)]` block has a corresponding `#[cfg(not(unix))]` or `#[cfg(windows)]` counterpart.

### Areas Where Windows Has Reduced Functionality

- **Ownership preservation** - silently skipped (no POSIX uid/gid on Windows)
- **Permission fidelity** - only readonly flag, no execute/suid/sgid/sticky bits
- **Extended attributes** - not supported (warning emitted)
- **ACLs** - not supported (Windows ACL model is fundamentally different from POSIX)
- **Hard link detection** - disabled (no inode tracking)
- **Device nodes and FIFOs** - silently skipped (not applicable to Windows)
- **Daemonization** - not supported (Windows Service Manager is the alternative)
- **Chroot** - not supported (warning emitted, security relies on other Windows mechanisms)
