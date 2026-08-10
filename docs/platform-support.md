# Platform Feature Parity

Comprehensive audit of platform-specific behavior across Linux, macOS, and
Windows. Each feature is categorized by implementation status and whether
Windows support is theoretically possible.

## Feature Matrix

### Metadata Preservation

| Feature | Linux | macOS | Windows | Stub Behavior | Windows Feasible? |
|---------|:-----:|:-----:|:-------:|---------------|:-----------------:|
| Permissions (`-p`) - full mode | yes | yes | read-only bit only | Maps write bit to read-only flag | Partial |
| Permissions (`-p`) - suid/sgid/sticky | yes | yes | no | Silently ignored | No |
| Permissions - `--chmod` modifiers | yes | yes | no | Ignored (no mode bits) | No |
| Permissions - `--executability` (`-E`) | yes | yes | no | No executable bit on Windows | No |
| Permissions - `fchmod` (fd-based) | yes | yes | no | Falls back to path-based | N/A |
| Ownership (`-o`/`-g`) - chown | yes | yes | no | Identity passthrough | Partial (1) |
| Ownership - user/group name lookup | yes | yes | no | Returns `None` | Partial (1) |
| Ownership - `--usermap`/`--groupmap`/`--chown` | yes | yes | no | Returns `MappingParseError` | No |
| Ownership - `--numeric-ids` | yes | yes | no | Passthrough (no NSS) | N/A |
| Timestamps - mtime (nanosecond) | yes | yes | yes | Via `filetime` crate | - |
| Timestamps - atime | yes | yes | yes | Via `filetime` crate | - |
| Extended attributes (`-X`) | yes | yes (2) | no | Warning + `Ok(())` | Partial (3) |
| POSIX ACLs (`-A`) | yes | yes | no | Warning + `Ok(())` | No (4) |
| NFSv4 ACLs | yes (via xattr) | yes (via xattr) | no | `Ok(None)`/`Ok(())` | Partial (4) |
| `--fake-super` (xattr-based) | yes | yes | no | `ErrorKind::Unsupported` | No (5) |
| `--copy-as=USER[:GROUP]` (euid/egid) | yes | yes | no | No-op guard, `Ok(())` | No |

Notes:
1. Windows has SIDs (Security Identifiers) rather than numeric uid/gid. The
   Win32 `LookupAccountNameW`/`LookupAccountSidW` APIs could theoretically map
   names, but the POSIX uid/gid model does not translate meaningfully.
2. macOS uses a different xattr namespace; the `xattr` crate handles this.
3. NTFS Alternate Data Streams (ADS) serve a similar role but use a completely
   different API (`filename:streamname` syntax or `BackupRead`/`BackupWrite`).
   Upstream rsync does not support ADS either.
4. Windows uses DACLs via the Win32 Security API, structurally different from
   POSIX.1e ACLs. Windows ACLs are NFSv4-style (allow/deny per principal).
   Upstream rsync does not support Windows DACLs.
5. Requires xattr support to store `user.rsync.%stat` attributes.

### Special File Types

| Feature | Linux | macOS | Windows | Stub Behavior | Windows Feasible? |
|---------|:-----:|:-----:|:-------:|---------------|:-----------------:|
| Symlinks (`-l`) | yes | yes | yes (6) | `symlink_file` / `symlink_dir` | yes |
| Hard links (`-H`) | yes | yes | yes | `std::fs::hard_link` | - |
| FIFOs/named pipes (`--specials`) | yes (`mknodat`) | yes (`mkfifo`/`mknod`) | no | `Ok(())` | No |
| Unix domain sockets (`--specials`) | yes (`mknodat`) | yes (`mknod`) | no | `Ok(())` | No |
| Block devices (`-D`) | yes (`mknodat`) | yes (`mknod`) | no | `Ok(())` | No |
| Character devices (`-D`) | yes (`mknodat`) | yes (`mknod`) | no | `Ok(())` | No |
| Sparse file handling (`-S`) | yes | yes | yes | Zero-run detection | - |

Notes:
6. Windows symlinks require `SeCreateSymbolicLinkPrivilege` or Developer Mode.
   Batch replay uses `std::os::windows::fs::symlink_file` (defaults to file
   symlink; directory symlinks need type detection). The symlink safety analysis
   (`symlink_safety.rs`) uses `to_string_lossy` on Windows instead of
   `OsStrExt::as_bytes`.

### I/O Optimizations

| Feature | Linux | macOS | Windows | Stub Behavior | Windows Feasible? |
|---------|:-----:|:-----:|:-------:|---------------|:-----------------:|
| `io_uring` async I/O | yes (5.6+) | no | no | Std I/O fallback | No |
| `copy_file_range` | yes (4.5+) | no | no | `read`/`write` fallback | Partial (7) |
| `statx` syscall | yes (glibc) | no | no | `stat`/`lstat` fallback | N/A |
| `fallocate` / preallocation | yes | yes | yes (8) | Best-effort | - |
| Parallel stat (`rayon`) | yes | yes | yes | Cross-platform | - |
| SIMD checksums (AVX2/SSE2/NEON) | yes | yes (NEON) | yes (AVX2/SSE2) | Scalar fallback | - |

Notes:
7. Windows has `CopyFileEx` / `FSCTL_DUPLICATE_EXTENTS_TO_FILE` for reflink
   copies on ReFS, but the semantics differ from `copy_file_range`.
8. Preallocation works cross-platform through the `fast_io` trait abstraction.

### Daemon Mode

| Feature | Linux | macOS | Windows | Stub Behavior | Windows Feasible? |
|---------|:-----:|:-----:|:-------:|---------------|:-----------------:|
| TCP daemon (`--daemon`) | yes | yes | no | Error message + exit 1 | Partial (9) |
| Syslog logging | yes | yes | no | Not compiled | Partial (10) |
| `uid`/`gid` config directives | yes (NSS) | yes (NSS) | numeric only | `parse::<u32>` only | - |
| Group expansion (`auth users`) | yes | yes | no | No `getgrnam` | No |
| Secrets file permission check | yes | yes | no | No mode bits | Partial |
| `use chroot` | yes | yes | no | N/A | No |
| systemd `sd-notify` | yes | no | no | Feature-gated | No |
| PID file creation | yes | yes | yes (11) | Cross-platform | - |

Notes:
9. The daemon TCP listener and protocol negotiation are platform-independent.
   The Windows stub explicitly reports "daemon mode is not available on
   Windows" with a formatted error message including role trailer. The main
   blockers are: chroot support, privilege dropping (`setuid`/`setgid`), and
   syslog integration.
10. Windows has the Event Log (`ReportEventW`), not syslog. A Windows-native
    logging backend would need to use the `windows` crate or raw Win32 calls.
11. PID file I/O is cross-platform but permission enforcement is Unix-only.

### Signal Handling

| Feature | Linux | macOS | Windows | Stub Behavior |
|---------|:-----:|:-----:|:-------:|---------------|
| SIGINT (Ctrl+C) | yes | yes | Ctrl+C only | Polls atomic flag |
| SIGTERM | yes | yes | no | N/A |
| SIGHUP | yes | yes | no | N/A |
| SIGPIPE | yes | yes | no | N/A |
| Graceful shutdown (first signal) | yes | yes | yes (Ctrl+C) | Atomic flag check |
| Forced abort (second signal) | yes | yes | no | Manual `request_abort()` |
| Temp file cleanup on signal | yes | yes | yes | `CleanupManager` is cross-platform |

### SSH Transport

| Feature | Linux | macOS | Windows | Notes |
|---------|:-----:|:-----:|:-------:|-------|
| SSH command spawning | yes | yes | yes | `std::process::Command` |
| Default SSH program (`ssh`) | yes | yes | `ssh.exe` | Path lookup differs |
| IPv6 bracket detection | byte-level | byte-level | `to_string_lossy` | Functionally equivalent |
| SSH child reaping (`Drop`) | yes | yes | yes | Cross-platform |
| Exit code mapping | yes | yes | yes | Signal codes Unix-only |

### Batch Mode

| Feature | Linux | macOS | Windows | Notes |
|---------|:-----:|:-----:|:-------:|-------|
| Batch file read/write | yes | yes | yes | Cross-platform |
| Batch symlink replay | yes | yes | yes (12) | Type detection limited |
| Batch permission replay | yes | yes | read-only only | Same as `-p` |

Notes:
12. Windows batch symlink replay defaults to file symlinks. Directory symlinks
    may not be created correctly since the batch format does not encode the
    target type.

### Build & Packaging

| Feature | Linux | macOS | Windows | Notes |
|---------|:-----:|:-----:|:-------:|-------|
| Binary compilation | yes | yes | yes | CI-tested |
| Cross-compilation targets | x86_64, aarch64 | x86_64, aarch64 | x86_64 | aarch64 Windows disabled |
| Windows GNU exception handling | N/A | N/A | yes | `windows-gnu-eh` crate |
| deb/rpm packaging | yes | N/A | N/A | Linux-only |
| Homebrew formula | N/A | yes | N/A | macOS-only |

## Stub Architecture

The codebase uses a consistent pattern for platform abstraction:

```
Feature ──→ #[cfg] gate ──┬── Unix implementation (full functionality)
                          └── Non-Unix stub (no-op, warning, or error)
```

### Metadata Module (`crates/metadata/src/`)

| Module | Unix | Non-Unix |
|--------|------|----------|
| `acl_exacl.rs` | Full POSIX/NFSv4 ACL support | `acl_noop.rs` - warning + `Ok(())` |
| `xattr.rs` | Full xattr read/write/sync | `xattr_stub.rs` - warning + `Ok(())` |
| `nfsv4_acl.rs` | NFSv4 ACE types and operations | `nfsv4_acl_stub.rs` - type stubs only |
| `ownership.rs` | `uid_from_raw`/`gid_from_raw` via rustix | `ownership_stub.rs` - identity functions |
| `id_lookup.rs` | `getpwnam_r`/`getgrnam_r` lookups | `id_lookup_stub.rs` - returns `None` |
| `mapping.rs` | `UserMapping`/`GroupMapping` parsing | `mapping_win.rs` - always errors |
| `copy_as.rs` | `seteuid`/`setegid` with RAII guard | No-op `CopyAsGuard` |
| `special.rs` | `mknodat`/`mkfifo`/`mknod` | `Ok(())` - silently succeeds |
| `fake_super.rs` | Full xattr-based metadata storage | `Unsupported` error or defaults |
| `chmod/apply.rs` | Full 12-bit mode manipulation | Not compiled on non-Unix |
| `stat_cache.rs` | `statx` on Linux, `stat` on other Unix | Basic `fs::metadata` |

### Permission Handling Detail

On Unix, `apply/permissions.rs` operates on the full 12-bit POSIX mode
(suid, sgid, sticky, rwx for user/group/other). On Windows, the only
permission bit available through `std::fs::Permissions` is the read-only
flag. The Windows code path maps the source write bit (`0o200`) to the
read-only flag:

- Source has write permission (`mode & 0o200 != 0`) -> destination is not
  read-only
- Source lacks write permission -> destination is read-only

### Signal Handling Detail

Unix signal handling (`core/src/signal/unix.rs`) installs handlers for
SIGINT, SIGTERM, SIGHUP, and SIGPIPE using libc signal APIs. The stub
(`core/src/signal/stub.rs`) provides the same `SignalHandler` interface
but only responds to programmatic shutdown requests via atomic flags.
Windows Ctrl+C handling would need `SetConsoleCtrlHandler` for proper
integration.

## Linux-Specific Features

These features are exclusive to Linux and have no equivalent on other platforms:

| Feature | Kernel Requirement | Fallback |
|---------|-------------------|----------|
| `io_uring` batched async I/O | 5.6+ | Standard `read`/`write` |
| `copy_file_range` zero-copy | 4.5+ (same-fs), 5.3+ (cross-fs) | `read`/`write` loop |
| `statx` extended stat | glibc (non-musl) | `stat`/`lstat` |
| systemd `sd-notify` | systemd | Feature-gated out |

On Linux musl targets, `statx` is not available and falls back to standard
`stat`/`lstat` calls.

## macOS-Specific Features

| Feature | Notes |
|---------|-------|
| `mknod`/`mkfifo` via `apple-fs` crate | Apple platforms use different `mode_t` width (u16) |
| Creation time (crtime/birthtime) | Available via `stat.st_birthtime` |
| POSIX ACLs via `exacl` | Supported on macOS, iOS/tvOS/watchOS use warning stubs |

## Known Windows Limitations

### Cannot Be Implemented (Fundamental Platform Differences)

- **POSIX permission bits** (suid, sgid, sticky, per-user/group/other rwx) -
  Windows NTFS uses DACLs, not mode bits
- **Unix domain sockets** - Windows has named pipes but different semantics
- **Device nodes** (block/character) - No Windows equivalent
- **FIFOs/named pipes via `mkfifo`** - Windows named pipes use a different API
  (`CreateNamedPipe`)
- **`seteuid`/`setegid` privilege switching** - Windows uses token
  impersonation (`ImpersonateLoggedOnUser`)
- **`chroot`** - No direct equivalent; Windows has app containers but they
  are architecturally different
- **POSIX signal handlers** (SIGTERM, SIGHUP, SIGPIPE) - Windows uses
  Structured Exception Handling and console control handlers

### Could Be Implemented (With Significant Effort)

- **NTFS Alternate Data Streams** as an xattr equivalent - requires Win32
  `BackupRead`/`BackupWrite` or stream path syntax
- **Windows DACLs** as an ACL equivalent - requires `GetNamedSecurityInfoW` /
  `SetNamedSecurityInfoW`
- **Daemon mode** - TCP listener is cross-platform; would need Event Log
  instead of syslog, and a Windows Service wrapper instead of systemd
- **Ctrl+C / Ctrl+Break handling** - `SetConsoleCtrlHandler` for proper
  Windows signal handling beyond the current polling stub
- **SID-based ownership** - `GetNamedSecurityInfoW` can retrieve file owner
  SID; mapping to rsync's uid/gid model is non-trivial

### Works Today (With Reduced Functionality)

- **File transfers** - core delta algorithm is cross-platform
- **Checksums** - all algorithms (MD4, MD5, XXH3, Adler32) work, including
  SIMD (AVX2/SSE2 on x86_64)
- **Compression** - zlib, zstd, lz4 all cross-platform
- **Filters** - include/exclude rules work on all platforms
- **Hard links** - `std::fs::hard_link` is cross-platform
- **Symlinks** - require Developer Mode or `SeCreateSymbolicLinkPrivilege`
- **Timestamps** - nanosecond mtime/atime via `filetime` crate
- **Sparse files** - zero-run detection is cross-platform
- **SSH transport** - `ssh.exe` spawning works
- **Batch mode** - read/write with limited symlink support
- **Socket options** - TCP socket configuration with Windows-specific constants
  (different `SOL_*`/`IPPROTO_*` values via `cfg(target_family = "windows")`)

## CI Coverage

| Platform | CI Workflow | Compiler | Status |
|----------|------------|----------|--------|
| Linux x86_64 | `ci.yml` (nextest) | stable | Required |
| Linux x86_64 musl | `ci.yml` | stable | Required |
| macOS (latest) | `ci.yml` | stable | Required |
| Windows (latest) | `ci.yml` | stable | Required |
| Linux x86_64 | `cross-compile.yml` | stable | Release builds |
| Linux aarch64 | `cross-compile.yml` | stable | Release builds |
| macOS x86_64 | `cross-compile.yml` | stable | Release builds |
| macOS aarch64 | `cross-compile.yml` | stable | Release builds |
| Windows x86_64 | `cross-compile.yml` | stable | Release builds |
| Windows aarch64 | `cross-compile.yml` | stable | Disabled |
