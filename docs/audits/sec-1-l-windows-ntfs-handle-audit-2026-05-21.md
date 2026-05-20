# SEC-1.l - Windows NTFS handle-based dispatch audit

**Date:** 2026-05-21
**Scope:** every receiver-reachable Windows code path that opens a file or directory, mutates the namespace, or applies metadata. Companion to [SEC-1.a](sec-1-a-path-syscall-surface-2026-05-20.md), which catalogues the Unix `*at` cutover sites.
**Goal:** determine whether the Windows write path is structurally safe from the TOCTOU class fixed upstream in rsync 3.4.3 (CVE-2026-29518, CVE-2026-43619), and produce a SECURITY.md-friendly posture statement.

This is read-only research. Test-only call sites (inside `#[cfg(test)]` modules, inside `#[test]` functions, inside files exclusively used as test fixtures) are excluded - they run in process-private tempdirs and carry no daemon-reachable TOCTOU. Sender-side path syscalls that run as the source-owning user are also out of scope per the SEC-1 charter.

## 1. Background: why Windows is structurally different

Linux/POSIX uses two distinct namespaces:

- **Path namespace** - paths re-resolve through the directory tree on every syscall. `open(path)` walks the path again even if you opened the parent five microseconds ago. An attacker who swaps a parent directory between calls can redirect the second call to a different file.
- **Descriptor namespace** - once `open()` returns an `int fd`, that descriptor is bound to a specific inode for its lifetime. Operations against an fd (`fchmod`, `futimens`, `write`, `fchown`, `fstatat(fd, "")`) cannot be re-routed by a namespace mutation.

The CVE-2026-29518 / CVE-2026-43619 class is "second access re-resolves the path through an attacker-controlled directory". The fix is to anchor the second access to an fd opened at the start of the operation (`openat`, `fstatat`, `unlinkat`, `renameat`, `linkat`).

Windows has only one namespace from the caller's point of view: kernel objects referenced by `HANDLE`. `CreateFileW(path, ...)` returns a `HANDLE` that, like an fd, is bound to a specific NTFS MFT record (inode-equivalent) for its lifetime. Subsequent `WriteFile`, `ReadFile`, `FlushFileBuffers`, `SetFileInformationByHandle`, and `DeviceIoControl` operations target that same MFT record even if every path component is renamed or replaced. The handle survives `MoveFileExW` of the file itself; it survives `RemoveDirectoryW` of intermediate components (the kernel keeps the chain alive via reference counts).

Two consequences relevant to SEC-1:

1. **Once we have a HANDLE, the inode is pinned.** There is no Windows analogue of the path-re-resolution attack against an open handle.
2. **Path-based Win32 calls re-resolve the path.** `MoveFileExW(src_path, dst_path)`, `DeleteFileW(path)`, `RemoveDirectoryW(path)`, `CreateFileW(path, ...)`, `SetNamedSecurityInfoW(path, ...)`, `SetFileAttributesW(path)`, and `set_file_times(path, ...)` walk the namespace from a drive root or current directory on every call, and are TOCTOU-attackable in exactly the same way `unlink(path)` is on Linux.

So the audit reduces to: **at the moments oc-rsync would otherwise re-walk a path on Windows, do we hold an open HANDLE to the destination or its parent, or do we re-walk via a path-based Win32 call?**

## 2. Inventory of Windows handle-creation sites

These are the call sites that take a `&Path` and return a kernel `HANDLE` (directly or wrapped in `std::fs::File`).

| file:line | API | Purpose | Lifetime of handle |
|---|---|---|---|
| `crates/fast_io/src/iocp/file_reader.rs:46` | `CreateFileW(GENERIC_READ + OPEN_EXISTING + FILE_FLAG_OVERLAPPED)` | Open source file for IOCP-batched read. | Lives for the entire read of the file; `IocpReader` owns it and closes it on `Drop`. |
| `crates/fast_io/src/iocp/file_writer.rs:84` | `CreateFileW(GENERIC_WRITE + CREATE_ALWAYS\|OPEN_EXISTING + FILE_FLAG_OVERLAPPED)` | Create or reopen the destination file for IOCP-batched write. | Lives for the entire write; `IocpWriter` owns it and closes it on `Drop`. |
| `crates/fast_io/src/iocp/disk_batch/writer.rs:54` | `ReOpenFile(GENERIC_WRITE + FILE_FLAG_OVERLAPPED)` | Convert a caller-owned `File` into an overlapped HANDLE without re-resolving the path. | Lives until `commit_file`. Bound to the same MFT record as the caller's `File`. |
| `crates/fast_io/src/iocp/file_factory.rs:380` | `GetFinalPathNameByHandleW` | Recover the canonical path from an existing HANDLE to feed `CreateFileW` for overlapped reopen. | Path is read from a live handle, eliminating any window in which a parent rename could redirect the reopen target (the kernel returns the path that resolves to the same MFT record). |
| `crates/fast_io/src/platform_copy/dispatch.rs:370,396,582,607` | `CreateFileW` (src + dst) | Open source and destination for ReFS reflink via `FSCTL_DUPLICATE_EXTENTS_TO_FILE`. | Both handles live for the duration of one `DeviceIoControl`; ioctl uses HANDLEs only, no further path walks. |
| `crates/fast_io/src/refs_detect.rs:156` | `CreateFileW(0 access + OPEN_EXISTING + FILE_FLAG_BACKUP_SEMANTICS)` | Open a volume root to probe its filesystem name via `GetVolumeInformationByHandleW`. | One ioctl per call; handle is closed immediately. Read-only metadata query, not in the write path. |
| `crates/metadata/src/xattr_windows.rs:268,312` | `CreateFileW` on `path:streamname:$DATA` | Open an NTFS alternate data stream (xattr backend). | One read or write per call; handle is closed via `File::Drop`. |
| `crates/metadata/src/xattr_windows.rs:348` | `DeleteFileW(path:streamname:$DATA)` | Remove an ADS xattr. | Path-based; no HANDLE held. See gap list. |

Every HANDLE in the production write path is created with `OPEN_EXISTING` or `CREATE_ALWAYS` plus a fixed access mask, and lives until the I/O completes and the file is committed. Once `IocpWriter::create` or `IocpDiskBatch::begin_file` returns, every subsequent operation on that file - `WriteFile`, `SetFileCompletionNotificationModes`, `SetFilePointerEx`, `SetEndOfFile`, `FlushFileBuffers`, `SetFileInformationByHandle` - takes a `HANDLE`, not a path. The kernel binds the HANDLE to the MFT record at open time; nothing the attacker does to the path namespace after `CreateFileW` can redirect that handle.

## 3. TOCTOU exposure analysis per site

### 3.1 IOCP write path (engine -> fast_io::iocp)

The disk-commit thread (`engine` -> `transfer::disk_commit` -> `fast_io::iocp::disk_batch::IocpDiskBatch`) accepts an already-opened `File` via `begin_file`. The batch never re-walks the destination path:

```text
caller opens File via DestinationWriteGuard ----> File (HANDLE) ----> begin_file()
                                                                          |
                                                                 ReOpenFile(handle, ...)
                                                                          |
                                                              WriteFile(handle, ...)
                                                              GetQueuedCompletionStatusEx
                                                              FlushFileBuffers(handle)
                                                              CloseHandle on commit
```

**TOCTOU verdict:** safe. From `begin_file` onward there is no path resolution. Even if a daemon client races a `MoveFileExW` against the destination's parent, the writes still land in the original MFT record the guard opened.

### 3.2 IOCP-or-Std reader/writer factory (`fast_io::iocp::file_factory`)

`IocpReaderFactory::open(path)` and `IocpWriterFactory::create(path)` perform a single `CreateFileW(path, ...)` and return a `HANDLE`-bearing wrapper. Subsequent `read`/`write`/`flush` calls go through the handle.

There is one micro-window worth naming: `IocpReaderFactory::open` calls `std::fs::metadata(path)?` (file_factory.rs:195) before `CreateFileW(path, ...)` to decide whether the file is large enough to merit IOCP. That is two path resolutions back-to-back. An attacker who swapped the destination's parent between those two calls would cause oc-rsync to read a different file than it stat'd. The damage is bounded - we read whatever the second resolution finds and return its bytes - and this site is in the *reader* factory, used for source-side file reads, which is out of scope for SEC-1's receiver-side TOCTOU charter. Still, recording it for completeness in the gap list.

`writer_from_file` (file_factory.rs:290) recovers the path from an existing HANDLE via `GetFinalPathNameByHandleW`, then drops the std `File` and reopens with `CreateFileW(... FILE_FLAG_OVERLAPPED ...)`. Because the path is read from a live HANDLE, the kernel returns the canonical path that currently resolves to the same MFT record. An attacker who replaces the file between `GetFinalPathNameByHandleW` and `CreateFileW` could redirect the reopen, but only into a file that lives at a path the attacker controls - this is a privilege boundary the daemon already lost when `use_chroot` was a no-op (see Section 5).

### 3.3 Path-based fs operations (engine, transfer)

The following `std::fs::*` paths are taken on Windows by daemon-reachable code. None of them hold a parent-directory HANDLE; each re-resolves the leaf through the namespace:

| file:line | std call | Underlying Windows API | Notes |
|---|---|---|---|
| `crates/engine/src/local_copy/executor/file/guard.rs:318,329` | `fs::rename(temp, final)` | `MoveFileExW` | Temp-file commit step. Both paths are re-resolved. |
| `crates/engine/src/local_copy/executor/file/guard.rs:344` | `fs::copy(temp, final)` then `fs::remove_file(temp)` | `CopyFileExW` + `DeleteFileW` | Cross-device fallback. Both paths re-resolved. |
| `crates/engine/src/local_copy/context_impl/state.rs:522,535` | `fs::rename(destination, backup_path)` | `MoveFileExW` | Backup rename. |
| `crates/engine/src/local_copy/context_impl/state.rs:526` | `fs::remove_file(backup_path)` | `DeleteFileW` | Clear pre-existing backup. |
| `crates/engine/src/local_copy/context_impl/state.rs:633,635` | `fs::remove_dir_all` / `fs::remove_file` | recursive `DeleteFileW`/`RemoveDirectoryW` | `force_remove_destination` branch. |
| `crates/engine/src/local_copy/context_impl/state.rs:471` | `fs::remove_dir(dir)` | `RemoveDirectoryW` | `--delay-updates` staging cleanup. |
| `crates/engine/src/local_copy/context_impl/state.rs:515` | `fs::create_dir_all(parent)` | repeated `CreateDirectoryW` | backup-path parent creation. |
| `crates/engine/src/delete/emitter/fs.rs:70-90` | `fs::remove_file`/`fs::remove_dir`/`fs::remove_dir_all` | `DeleteFileW` / `RemoveDirectoryW` | Receiver-side `--delete` dispatch. |
| `crates/engine/src/delete/extras.rs:107,115` | `fs::read_dir` + `fs::symlink_metadata` | `FindFirstFileExW` + `GetFileAttributesExW` | Scan and stat destination "extras". |
| `crates/engine/src/local_copy/executor/directory/recursive/mod.rs:128,131` | `fs::create_dir_all` / `fs::create_dir` | `CreateDirectoryW` | Directory creation. |
| `crates/engine/src/local_copy/executor/directory/recursive/deletion.rs:64` | `fs::remove_dir(destination)` | `RemoveDirectoryW` | Prune empty dir. |
| `crates/engine/src/local_copy/executor/directory/support.rs:44,50,78,90,108` | `fs::read_dir` + `fs::symlink_metadata` | `FindFirstFileExW` + `GetFileAttributesExW` | Directory listing for recursion. |
| `crates/engine/src/local_copy/executor/special/symlink.rs:474-480` | `os::windows::fs::symlink_file/dir` | `CreateSymbolicLinkW` | Symlink creation; target is recreated by leaf path. |
| `crates/engine/src/local_copy/overrides.rs:60` -> `fast_io::hard_link` -> `crates/fast_io/src/io_uring_ops.rs:212` | `std::fs::hard_link` | `CreateHardLinkW` | On Windows always falls through to `CreateHardLinkW(new_path, existing_path)`. |
| `crates/transfer/src/temp_cleanup.rs:95,137` | `fs::read_dir` + `fs::remove_file` | `FindFirstFileExW` + `DeleteFileW` | Stale-temp cleanup at startup. |
| `crates/metadata/src/apply/timestamps.rs:40,127,172` | `filetime::set_file_times` | `SetFileTime` after `CreateFileW(path, FILE_WRITE_ATTRIBUTES)` (internal to `filetime`) | Mtime/atime apply. Opens a fresh HANDLE per call by leaf path. |
| `crates/metadata/src/apply/permissions.rs:35,48,91` | `fs::set_permissions` | `SetFileAttributesW` | Readonly attribute apply. |
| `crates/metadata/src/xattr_windows.rs:348` | `DeleteFileW(path:stream:$DATA)` | `DeleteFileW` | Path-based ADS xattr removal. |
| `crates/metadata/src/acl_windows/dacl.rs:50,418` | `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW` | `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW` | DACL apply by path. |

**TOCTOU verdict per row:** each row re-resolves the destination through the full path on every call. Two adjacent calls against the same logical file (e.g. the `fs::rename` + retry pair, or `set_file_times` followed by `set_permissions`) are TOCTOU-attackable in exactly the way `rename(path)` + `chmod(path)` are on Linux. An attacker who can swap an intermediate directory between two calls can redirect the second to a different MFT record.

The good news: the *write* of file data itself (the bytes the client sent) is no longer in this list. Once the IOCP path takes over via the open HANDLE, the byte content lands in the MFT record the guard chose. Only the surrounding namespace operations (rename, delete, link, set attributes) remain path-based.

### 3.4 ADS xattr backend (`metadata::xattr_windows`)

`read_attribute`, `write_attribute`, and `remove_attribute` each open `path:streamname:$DATA` via `CreateFileW` or `DeleteFileW`. The leading path resolves to the parent file's MFT record on every call. If the parent file is renamed or replaced between two adjacent xattr calls, the second call targets the new MFT record. This is the NTFS analogue of the Unix per-entry lstat+open race.

### 3.5 Windows ACL backend (`metadata::acl_windows::dacl`)

`GetNamedSecurityInfoW` and `SetNamedSecurityInfoW` are both path-based. Each call re-resolves the path through the namespace. On NTFS these set the DACL on the resolved MFT record, so an attacker who can swap an intermediate directory between `GetNamedSecurityInfo` and `SetNamedSecurityInfo` can read one file's DACL and write it to another.

## 4. Gap list - path-based syscalls that should migrate to handle-based on Windows

Ordered by daemon-reachable severity. "Handle-based replacement" lists the Win32 API that takes a `HANDLE` instead of a path.

| # | Current site | Current call | Handle-based replacement | Notes |
|---|---|---|---|---|
| 1 | `engine::local_copy::executor::file::guard:318,329` | `fs::rename(temp, final)` | `SetFileInformationByHandle(temp_handle, FileRenameInfoEx, FILE_RENAME_FLAG_POSIX_SEMANTICS \| FILE_RENAME_FLAG_REPLACE_IF_EXISTS)` against a temp-file HANDLE opened with `DELETE` access | Windows 10 1607+; replaces both `fs::rename` and the AlreadyExists retry in a single atomic step. The final-path leaf is still resolved through the destination directory, but only once and against a kernel-tracked rename target. |
| 2 | `engine::local_copy::context_impl::state:522,535` | `fs::rename(destination, backup_path)` | Same as #1, plus open the destination HANDLE up-front via `CreateFileW(destination, DELETE \| GENERIC_READ, ...)` before the rename | Backup commit. |
| 3 | `engine::delete::emitter::fs:70-90` (`unlink_file`, `unlink_symlink`, `unlink_device`, `unlink_special`) | `fs::remove_file(path)` -> `DeleteFileW` | Open via `CreateFileW(path, DELETE, FILE_SHARE_DELETE, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT \| FILE_FLAG_BACKUP_SEMANTICS)`, then `SetFileInformationByHandle(handle, FileDispositionInfoEx, FILE_DISPOSITION_FLAG_DELETE \| FILE_DISPOSITION_FLAG_POSIX_SEMANTICS)` | Resolves the leaf once at open time; the unlink itself is then handle-based. `FILE_FLAG_OPEN_REPARSE_POINT` ensures symlinks are not traversed (matches `AT_SYMLINK_NOFOLLOW`). |
| 4 | `engine::delete::emitter::fs:74` (`rmdir`) | `fs::remove_dir(path)` -> `RemoveDirectoryW` | `CreateFileW(path, DELETE, ..., FILE_FLAG_BACKUP_SEMANTICS)` + `SetFileInformationByHandle(handle, FileDispositionInfoEx, FILE_DISPOSITION_FLAG_DELETE \| FILE_DISPOSITION_FLAG_POSIX_SEMANTICS)` | `FILE_FLAG_BACKUP_SEMANTICS` is required to open a directory HANDLE. |
| 5 | `engine::delete::emitter::fs:90` (`remove_dir_all`) | `fs::remove_dir_all(path)` | Recursive walk via opened parent HANDLE + per-child `DeleteFileW` against handles. Mirror upstream's `delete_dir_contents` model but with NTFS HANDLEs instead of dirfds. | No single Windows API equivalent; this is a multi-step pattern. |
| 6 | `engine::delete::extras:107,115` | `fs::read_dir(dir)` + `fs::symlink_metadata(entry)` | `CreateFileW(dir, GENERIC_READ, FILE_SHARE_*, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS)` then `GetFileInformationByHandleEx(handle, FileIdBothDirectoryInfo, ...)` for batched stat | Single directory HANDLE anchors every per-entry stat; the entry stats become handle-relative and cannot be redirected by a parent swap. |
| 7 | `engine::local_copy::executor::directory::support:44,50,78,90,108` | `fs::read_dir(path)` + `fs::symlink_metadata(entry)` | Same as #6 | Recursive directory listing for receiver descent. |
| 8 | `engine::local_copy::executor::directory::recursive::deletion:64` | `fs::remove_dir(destination)` | Same as #4 | Prune-empty-directory branch. |
| 9 | `engine::local_copy::executor::directory::recursive::mod:128,131` | `fs::create_dir_all(destination)` / `fs::create_dir(destination)` | `CreateFileW(destination, GENERIC_WRITE, ..., CREATE_NEW, FILE_FLAG_BACKUP_SEMANTICS \| FILE_ATTRIBUTE_DIRECTORY)` per component, anchored to the previous component's HANDLE via `NtCreateFile` with a `RootDirectory` `OBJECT_ATTRIBUTES` field | Each component is resolved relative to the parent HANDLE, eliminating the multi-component path race. Requires dropping into `windows::Wdk::Storage::FileSystem::NtCreateFile`; the `windows` crate ships safe wrappers. |
| 10 | `engine::local_copy::context_impl::state:515` | `fs::create_dir_all(parent)` (backup-path parent) | Same as #9 | |
| 11 | `engine::local_copy::context_impl::state:633,635` | `fs::remove_dir_all` / `fs::remove_file` (`force_remove_destination`) | Same as #3 / #5 | |
| 12 | `engine::local_copy::context_impl::state:471` | `fs::remove_dir(dir)` (`--delay-updates` staging) | Same as #4 | |
| 13 | `engine::local_copy::executor::special::symlink:474-480` | `os::windows::fs::symlink_file` / `symlink_dir` -> `CreateSymbolicLinkW` | `CreateSymbolicLinkW` against a path resolved relative to a parent-directory HANDLE; or equivalently, drop into `NtCreateFile` with `FILE_CREATE` and the `FileSymbolicLinkInformation` reparse data | The leaf is still resolved on creation, but the parent must not be swappable mid-operation. |
| 14 | `fast_io::io_uring_ops:212` (`std::fs::hard_link` Windows fallback -> `CreateHardLinkW`) | `CreateHardLinkW(new_path, existing_path)` | Open the existing file via `CreateFileW(existing_path, MAXIMUM_ALLOWED, ..., FILE_FLAG_BACKUP_SEMANTICS)`, then `SetFileInformationByHandle(handle, FileLinkInfo, ...)` with the new leaf | Hardlink creation by HANDLE; the destination leaf is resolved against the link's parent HANDLE. |
| 15 | `metadata::apply::timestamps:40,127,172` | `filetime::set_file_times(path, ...)` | `CreateFileW(path, FILE_WRITE_ATTRIBUTES, ..., FILE_FLAG_OPEN_REPARSE_POINT)` + `SetFileTime(handle, ...)` directly, bypassing `filetime` for the Windows path | Mtime/atime apply opens a fresh HANDLE per call inside `filetime`; calling `SetFileTime` ourselves on an already-open destination HANDLE removes that re-resolution. Where the local-copy guard still holds the destination HANDLE post-commit, reuse it. |
| 16 | `metadata::apply::permissions:35,48,91` | `fs::set_permissions(path, ...)` -> `SetFileAttributesW` | `CreateFileW(path, FILE_WRITE_ATTRIBUTES, ...)` + `SetFileInformationByHandle(handle, FileBasicInfo, ...)` to set `FileAttributes` | Readonly-attribute apply. |
| 17 | `metadata::xattr_windows:268,312,348` | `CreateFileW(path:stream:$DATA, ...)` / `DeleteFileW(path:stream:$DATA)` | Open the parent file via `CreateFileW(path, FILE_GENERIC_READ \| FILE_WRITE_DATA, ..., FILE_FLAG_BACKUP_SEMANTICS)` once per metadata batch, then open the stream via `NtCreateFile` with the parent HANDLE as `RootDirectory` and `:stream:$DATA` as the relative name | Single parent open anchors every ADS read/write/delete. |
| 18 | `metadata::acl_windows::dacl:50,418` | `GetNamedSecurityInfoW(path, SE_FILE_OBJECT, ...)` / `SetNamedSecurityInfoW(path, SE_FILE_OBJECT, ...)` | `CreateFileW(path, READ_CONTROL \| WRITE_DAC, ..., FILE_FLAG_OPEN_REPARSE_POINT)` once, then `GetSecurityInfo(handle, SE_KERNEL_OBJECT, ...)` / `SetSecurityInfo(handle, SE_KERNEL_OBJECT, ...)` | DACL get/set against a HANDLE. The pair of calls then operates on the same MFT record regardless of any concurrent namespace mutation. |
| 19 | `transfer::temp_cleanup:95,137` | `fs::read_dir(dest)` + `fs::remove_file(path)` | Same as #6 + #3 | Stale-temp cleanup at startup. Lower severity because it runs once before any client request. |
| 20 | `fast_io::iocp::file_factory:195` (`IocpReaderFactory::open`) | `std::fs::metadata(path)` + `CreateFileW(path, ...)` | `CreateFileW(path, ..., FILE_FLAG_BACKUP_SEMANTICS)` first, then `GetFileSizeEx(handle)` to decide whether to keep IOCP or fall back | Reader side, source-owning user, low severity. Recorded for completeness. |

Items 1-18 are receiver-side and in scope for SEC-1. Items 19-20 are noted for completeness but are not on the SEC-1 critical path.

## 5. Daemon-mode cross-reference

`crates/platform/src/privilege.rs:63` shows the Windows `apply_chroot` implementation:

```rust
#[cfg(not(unix))]
pub fn apply_chroot(_path: &Path) -> io::Result<()> {
    eprintln!("WARNING: chroot is not supported on this platform - skipping");
    Ok(())
}
```

So when the operator sets `use chroot = yes` in `oc-rsyncd.conf` on Windows, oc-rsync **logs a warning and continues without any sandboxing**. There is no NTFS analogue to chroot in standard Win32; the closest equivalents (Job-object filesystem redirection, silos, integrity-level downgrades) are not wired.

Consequences for the SEC-1 charter:

- **The receiver process retains full namespace access regardless of the module root.** A daemon serving `/srv/data` on Windows still has `\\?\C:\` reachable. An attacker who can rename or replace any directory above the module root affects every path-based call in Section 3.3.
- **The Win32 path-based calls listed above are TOCTOU-attackable to the full extent of the daemon's process privileges**, not just those of the module's intended scope.
- The `use chroot` directive on Windows is therefore documentation-grade only; it does not contain a daemon module to its declared path. Operators relying on it for isolation are mistaken.

This does not change whether the handle-based IOCP write path is TOCTOU-safe (it is). It does mean the path-based residue in Section 3.3 is more dangerous on Windows than on Unix when `use_chroot` is requested but silently ignored, because there is no module-level sandbox to contain the redirection.

## 6. Conclusions

1. **File-data writes are structurally safe.** The IOCP path, the IOCP-or-Std factory wrappers, and the temp-file write guard all hold an open HANDLE for the duration of the write. The byte stream lands in the MFT record the guard chose. No path swap can redirect file contents to a different inode.
2. **Namespace operations remain path-based and are TOCTOU-attackable.** Rename, delete, rmdir, mkdir, hardlink, symlink, set-attributes, set-times, set-DACL, and ADS xattr operations all re-resolve the destination through the namespace on every call. These are the Windows analogues of the upstream CVE-2026-29518 sites and need the same kind of cutover - in NTFS terms, opening a HANDLE up-front and dispatching subsequent operations via `SetFileInformationByHandle`, `SetSecurityInfo(handle)`, `SetFileTime(handle)`, etc.
3. **The receiver-side metadata-apply pipeline is the second-largest exposure.** `set_file_times`, `set_permissions`, `SetNamedSecurityInfoW`, and ADS xattr writes all run as a sequence of independent path-based calls. A coherent handle-based migration would group them: open the destination once with `FILE_WRITE_ATTRIBUTES \| WRITE_DAC \| FILE_WRITE_DATA` after the data is committed, run every metadata setter through that HANDLE, then close.
4. **Windows daemon mode lacks any module-confinement mechanism.** `use_chroot` silently no-ops. This magnifies the path-based exposure - a path-swap attack is not even contained by the module root.
5. **No path-based fallback hides behind the IOCP code path.** When `is_iocp_available()` returns `false` the writer drops to `StdFileWriter`, which is a thin wrapper around `std::fs::File`. The destination file is still opened via a single `CreateFileW(path, ...)` and the resulting HANDLE owns every subsequent operation - the same handle-based safety property as the IOCP path. There is no Windows code path in which file-data writes re-resolve the destination per write.

## 7. Recommended SECURITY.md text

```markdown
### Windows posture for CVE-2026-29518 / CVE-2026-43619 (TOCTOU)

On Windows, oc-rsync writes file contents through NTFS HANDLEs (`CreateFileW` +
`ReOpenFile` with `FILE_FLAG_OVERLAPPED`) rather than path-based syscalls. Once
the destination is opened by the write guard, every subsequent `WriteFile`,
`FlushFileBuffers`, and `SetFileCompletionNotificationModes` call targets the
HANDLE - and the HANDLE is bound to the underlying MFT record for its lifetime.
A concurrent rename, replace, or removal of any parent directory cannot
redirect those writes to a different inode. The file-data write path is
therefore structurally safe from the CVE-2026-29518 / CVE-2026-43619 TOCTOU
class without requiring a separate `*at`-style cutover.

Namespace and metadata operations on Windows (`fs::rename`, `fs::remove_file`,
`fs::remove_dir`, `fs::create_dir`, `fs::hard_link`, symlink creation,
`set_file_times`, `fs::set_permissions`, `SetNamedSecurityInfoW`, and ADS
xattr read/write/delete) still re-resolve the destination through the path
namespace on every call. These sites are tracked in
`docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md` and will migrate
to HANDLE-based equivalents (`SetFileInformationByHandle` with
`FileRenameInfoEx` / `FileDispositionInfoEx` / `FileLinkInfo`,
`SetFileTime(handle)`, `SetSecurityInfo(handle, SE_KERNEL_OBJECT, ...)`,
parent-HANDLE-anchored ADS open) under the SEC-1.l follow-up tasks.

The Windows daemon does not implement `use chroot`; the directive logs a
warning and continues without sandboxing because NTFS has no `chroot(2)`
analogue. Operators who require module-level filesystem confinement on
Windows must run oc-rsyncd inside a constrained Job object or container.
```
