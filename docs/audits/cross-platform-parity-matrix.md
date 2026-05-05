# Cross-Platform Parity Matrix

Tracking issue: cross-platform inventory across `crates/`. No code changes.

## 1. Scope and methodology

This audit produces the authoritative inventory of platform-specific code
paths in oc-rsync. Existing docs each cover a slice (`docs/windows_platform_parity.md`
catalogues Windows behaviour, `docs/platform-io-fast-paths.md` catalogues
the I/O fallback chain, `docs/platform-support.md` lists feature parity
including iOS/tvOS targets, `docs/platform-notes.md` summarises the
metadata surface, the focused per-feature audits under `docs/audits/`
cover individual subsystems). None of them aggregates the whole tree
into a single classification table. CI relies entirely on `#[cfg]`
gating to keep the workspace compiling on Linux/macOS/Windows; gaps that
slip past gating only surface as test-time failures on the Linux runner
or as silent stubs on macOS/Windows. This document is the canonical
inventory those other documents extend.

The audit was produced by:

1. Counting all `#[cfg(...)]` attributes under `crates/` and grouping by
   form. The current totals are 1466 `cfg(unix)`, 203
   `cfg(target_os = "...")` (linux/macos/windows only - no other tier-2
   targets are gated), 107 `cfg(windows)`, and 221 negated forms
   (`cfg(not(unix))`, `cfg(not(target_os = "linux"))`, etc.). Cargo
   feature gates contribute another layer (16 distinct feature names:
   `acl`, `xattr`, `iconv`, `io_uring`, `iocp`, `zstd`, `lz4`,
   `parallel`, `openssl`, `zlib-ng`, `embedded-ssh`, `serde`, `tracing`,
   `concurrent-sessions`, `incremental-flist`, `multi-producer`, plus
   `async`, `test-support`, `copy_file_range`).
2. Walking each major platform-aware crate (`fast_io`, `metadata`,
   `apple-fs`, `daemon`, `platform`, `cli`, `engine`, `transfer`,
   `protocol`, `rsync_io`, `checksums`) and recording each gated
   surface, the implementation tier on each platform, and the fallback.
3. Cross-checking the observed cfg layout against `.github/workflows/ci.yml`
   to identify which surfaces are actually exercised by macOS and
   Windows CI runners.
4. Reconciling the matrix against the existing per-feature audits in
   `docs/audits/` to avoid duplicating their findings.

The resulting classification uses four buckets:

- **Full parity (F)** - working implementation on all three platforms.
  Behavioural differences may remain, but every platform exercises a
  real code path, not a stub.
- **Partial parity (P)** - production implementation on Linux plus at
  least one secondary platform; the third platform either runs a
  best-effort fallback (e.g. portable `std::fs`) or routes through a
  stub that compiles and behaves predictably.
- **Linux-only with documented fallback (L+F)** - Linux gets the
  optimised path; macOS/Windows fall back to a portable, correct,
  documented alternative. No user-visible feature gap.
- **Linux-only without fallback (L)** - the feature is unavailable on
  macOS or Windows. Either silently no-ops, returns
  `ErrorKind::Unsupported`, or emits a one-time warning. These are the
  user-visible gaps tracked by the task list referenced in
  section 4.

## 2. Top-level matrix

The `Status` column uses the four-bucket classification above. The
`Code path` column lists the canonical entry point so reviewers can
locate the implementation and its stub partner. The `Fallback` column
describes what the non-Linux build actually does at runtime when the
optimised path is unavailable.

### 2.1 Async I/O backends

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| io_uring (file + socket) | yes (5.6+) | no | no | L+F | `crates/fast_io/src/io_uring/`, stub `io_uring_stub.rs` | std `BufReader`/`BufWriter` via `traits.rs:120-185` | Feature-gated by `io_uring`. Runtime probe in `io_uring/config.rs`. Capabilities: SQPOLL, fixed files, registered buffers, provided buffer ring, deferred-taskrun. |
| IOCP (file + socket) | no | no | yes (Vista+) | L+F | `crates/fast_io/src/iocp/`, stub `iocp_stub.rs` | std `BufReader`/`BufWriter` via `traits.rs:120-185` | Feature-gated by `iocp` (default-on for Windows). Runtime probe in `iocp/config.rs`. PRs #1717-#1721. |
| dispatch_io (file + socket) | no | no | no | L | not implemented | `BufReader`/`BufWriter` always | Audit `docs/audits/macos-dispatch-io.md` (#1653) recommends implementation; macOS receiver currently has no async fast path. |
| Buffer-pool reuse across backends | yes | yes | yes | F | `crates/engine/src/buffer_pool.rs` | not applicable | Cross-platform, RAII `PooledBuffer`. |

### 2.2 File copy fast paths

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| Reflink / CoW clone | FICLONE (Btrfs/XFS/bcachefs) | clonefile (APFS) | FSCTL_DUPLICATE_EXTENTS_TO_FILE (ReFS detection landed) | P | `crates/fast_io/src/platform_copy/dispatch.rs:62-211` | next stage of dispatch chain | Windows reflink probe in `crates/fast_io/src/refs_detect.rs`; full FSCTL wiring tracked by #1389. |
| Kernel-accelerated copy | copy_file_range (4.5+ same-fs, 5.3+ cross-fs) | fcopyfile | CopyFileExW (+ COPY_FILE_NO_BUFFERING for files > 4 MB) | F | `crates/fast_io/src/copy_file_range.rs`, `copy_file_ex.rs`, `platform_copy/dispatch.rs:103-294` | std `fs::copy` | Windows path tracked by #1414/#1749. CI runs `windows-iocp` job for IOCP coverage. |
| Portable copy | std::fs::copy | std::fs::copy | std::fs::copy | F | `std::fs::copy` (universal terminal) | not applicable | Final fallback on every platform. |
| O_TMPFILE anonymous temp + linkat | yes (3.11+) | no | no | L+F | `crates/fast_io/src/o_tmpfile/` | named temp via `tempfile::TempPath` | Probed once; fall through on NFS/FUSE. |
| Preallocate | fallocate / posix_fallocate | F_PREALLOCATE / posix_fallocate | NtSetInformationFile / set_len | F | `crates/engine/src/local_copy/executor/file/preallocate.rs` | best-effort `set_len` | Cross-platform trait `Preallocate`. |

### 2.3 Network and pipe fast paths

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| sendfile (file -> socket) | yes | no | no | L+F | `crates/fast_io/src/sendfile.rs` | `libc::write` on non-Linux Unix; `io::sink()` on Windows | Tracked by #1361. Used by daemon TCP path. |
| splice (socket -> file via pipe) | yes | no | no | L+F | `crates/fast_io/src/splice.rs` | `recv` + buffered `write` | Audit `docs/audits/splice-ssh-stdio.md`. |
| madvise MADV_WILLNEED | yes | yes | no | P | `crates/fast_io/src/mmap_reader.rs` | no-op on Windows (`mmap_reader_stub.rs`) | Tracked by #1662. Audit `docs/audits/madvise-willneed-prefault.md`. |
| Memory-mapped reader (mmap) | yes | yes | no | P | `crates/fast_io/src/mmap_reader.rs` vs `mmap_reader_stub.rs` | reads whole file into `Vec<u8>` | Same public API; `advise_*` calls are no-ops on Windows. |
| Sparse read SEEK_DATA / SEEK_HOLE | yes | no | no | L+F | `crates/engine/src/local_copy/executor/file/sparse/reader.rs` | sequential zero-detection reader | Same delta output regardless. |
| Sparse hole punch FALLOC_FL_PUNCH_HOLE | yes | no | no | L+F | `crates/engine/src/local_copy/executor/file/sparse/hole_punch.rs` | seek-past-zeros (`fast_io::zero_detect`) | 16-byte u128 zero-run detection is cross-platform. |
| Sparse write zero-run detection | yes | yes | yes | F | `crates/fast_io/src/zero_detect.rs` | not applicable | SIMD-friendly `u128` scan; same code on all OSes. |
| Sendfile/splice on macOS via dispatch_io | no | no | no | L | not implemented | buffered I/O | Listed in `docs/audits/macos-dispatch-io.md` phase 6. |

### 2.4 Event loops

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| epoll-style readiness | implicit via io_uring or std I/O | not used | not used | L+F | `crates/fast_io/src/io_uring/` | std blocking I/O on macOS/Windows | oc-rsync does not call `epoll_wait` directly; readiness is delegated to io_uring or std I/O. |
| kqueue-style readiness | not applicable | not used | not applicable | L | no consumer in `crates/` (no `EVFILT_*` references) | std blocking I/O | `dispatch_io` audit (#1653) is the proposed macOS replacement. |
| IOCP completion port | not applicable | not applicable | yes | F | `crates/fast_io/src/iocp/completion_port.rs` | not applicable | Windows-only, no fallback needed because std I/O on Linux/macOS is the equivalent. |
| sd_notify (systemd readiness) | yes (feature-gated) | no | no | L+F | `crates/daemon/src/systemd.rs` | inert helper struct on non-Linux | Cargo feature `tracing` orthogonal to this. |

### 2.5 Metadata: ownership, permissions, timestamps

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| chown / fchown | yes | yes | no | P | `crates/metadata/src/apply/ownership.rs` (Unix) vs `ownership_stub.rs` | `Ok(())` no-op on Windows | Windows has no POSIX uid/gid. SID model handled separately under ACLs. |
| 12-bit POSIX permission mode | yes | yes | partial (read-only bit only) | P | `crates/metadata/src/apply/permissions.rs` | maps owner write bit to read-only flag | suid/sgid/sticky/per-class rwx not representable on NTFS. |
| --chmod / -E modifiers | yes | yes | no | P | `crates/metadata/src/chmod/apply.rs` | ignored | Modifiers operate on POSIX mode bits which Windows does not store. |
| Timestamps mtime / atime (ns) | yes (utimensat) | yes (utimensat) | yes (filetime crate) | F | `crates/metadata/src/apply/timestamps.rs` | not applicable | Cross-platform `filetime`. |
| Birth time (crtime) | no | yes (st_birthtime) | yes (FILE_BASIC_INFO::CreationTime) | F | `crates/metadata/src/stat_cache.rs:686` | linux falls back to mtime where applicable | Documented in `docs/platform-support.md`. |
| User/group lookup | yes (getpwnam_r/getgrnam_r via uzers) | yes | yes (LookupAccountNameW) | P | Unix: `crates/metadata/src/id_lookup/nss.rs`; Windows: `crates/platform/src/name_resolution.rs`; CLI: `crates/cli/src/platform.rs` | `id_lookup_stub.rs` returns `None` | Windows lookup wraps RID via NetLocalGroupGetMembers. |
| --usermap / --groupmap / --chown | yes | yes | no | P | `crates/metadata/src/mapping/` (Unix) vs `mapping_win.rs` | `Err(MappingParseError::Unsupported)` | Windows lacks numeric uid/gid model. |
| Numeric IDs (`--numeric-ids`) | yes | yes | passthrough | F | `crates/metadata/src/options/setters.rs` | not applicable | No NSS round-trip needed. |
| --copy-as USER[:GROUP] (seteuid/setegid) | yes | yes | no | L+F | `crates/metadata/src/copy_as.rs` | no-op `CopyAsGuard`, returns `Ok(())` | Windows uses `ImpersonateLoggedOnUser` for daemon impersonation; not wired into `--copy-as` semantics. |
| --fake-super xattr storage | yes | yes | no | L+F | `crates/metadata/src/fake_super.rs` | `ErrorKind::Unsupported` (depends on xattr) | Windows ADS-based fake-super tracked by #1657 (see audit `fake-super-privilege.md`). |
| chroot | yes | yes | no | L+F | `crates/platform/src/privilege.rs` | warn + `Ok(())` on Windows | Daemon security on Windows uses token impersonation. |

### 2.6 Metadata: ACLs and xattrs

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| POSIX ACLs (`-A`) | yes (exacl) | yes (exacl) | yes (windows-rs DACL) | F | `crates/metadata/src/acl_exacl.rs`, `acl_windows.rs`, `acl_noop.rs` | warning + `Ok(())` on platforms without acl feature | Windows ACL synchronisation via `GetNamedSecurityInfoW`/`SetNamedSecurityInfoW` (lossy SID/account-name mapping). Tier 1C parity; SACL preservation deferred. |
| NFSv4 ACLs | yes (xattr-stored) | yes (xattr-stored) | not applicable | P | `crates/metadata/src/nfsv4_acl.rs` vs `nfsv4_acl_stub.rs` | no-op stub | Windows DACL is the structural equivalent and is handled by the ACL row. |
| Extended attributes (`-X`) | yes (xattr crate) | yes (xattr crate) | yes (NTFS ADS via windows-rs) | F | `crates/metadata/src/xattr_unix.rs`, `xattr_windows.rs`, `xattr_stub.rs` | warning + `Ok(())` when feature off | Windows ADS via FindFirstStreamW/FindNextStreamW; `xattr_windows.rs:441` warns when volume rejects ADS. |
| xattr namespace filtering | yes (`user.*` non-root, all-but-`system.*` root) | not applicable (flat namespace) | not applicable (flat namespace) | L+F | `crates/protocol/src/xattr/prefix.rs:23-237` | no-op | Linux-specific kernel restriction; macOS/Windows expose flat namespaces. |
| xattr wire prefix compression | yes (linux table) | partial | partial | P | `crates/protocol/src/xattr/prefix.rs` | non-Linux uses smaller prefix table | Compression preserves wire compatibility with upstream rsync. |

### 2.7 Special file types

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| Symlinks (`-l`) | yes | yes | yes (with privilege) | F | Unix: `std::os::unix::fs::symlink`; Windows: `crates/engine/src/local_copy/executor/special/symlink.rs:464` (`symlink_dir`/`symlink_file`) | not applicable | Windows requires `SeCreateSymbolicLinkPrivilege` or Developer Mode; surfaces as `PermissionDenied`. |
| Hard links (`-H`) | yes | yes | yes | F | `std::fs::hard_link` plus `crates/engine/src/local_copy/hard_links.rs` | not applicable | Windows tracker is no-op (no inode model); restore via `std::fs::hard_link` works. |
| FIFOs / named pipes | yes (mkfifo/mknodat) | yes (mkfifo/mknod) | no | L+F | `crates/metadata/src/special.rs`, `crates/apple-fs/src/lib.rs` | `Ok(())` (silent skip) on Windows | Windows named pipes are not file-system objects; no equivalent path. |
| Block / character devices (`-D`) | yes (mknodat) | yes (mknod) | no | L+F | `crates/metadata/src/special.rs` | `Ok(())` (silent skip) | Same rationale. |
| Unix domain sockets | yes | yes | no | L+F | `crates/metadata/src/special.rs` | `Ok(())` (silent skip) | Windows AF_UNIX exists in modern builds but is not used by oc-rsync. |
| AppleDouble + resource forks | yes (filter only) | yes (real implementation) | yes (filter only) | P | `crates/apple-fs/src/apple_double.rs`, `resource_fork.rs`; `crates/filters/src/apple_double.rs` | filter excludes `._*` sidecars on every platform | Resource fork code gated on `target_os = "macos"`. PR landed for AppleDouble + resource-fork support. |
| Symlink safety (race-free path resolution) | yes (openat/fstatat/readlinkat) | yes | partial (best-effort `to_string_lossy`) | P | `crates/transfer/src/symlink_safety.rs`, `crates/flist/src/symlink_safety.rs` | always-allow no-op on Windows | Windows lacks `*at` syscalls; TOCTOU window unavoidable. |

### 2.8 Path handling and Unicode

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| UTF-8 path handling | yes | yes | yes | F | `crates/cli/src/frontend/execution/file_list/parser.rs` | not applicable | rsync wire format is bytes; oc-rsync round-trips OsString. |
| Backslash separator | not applicable | not applicable | yes (normalised to `/` on the wire) | F | `crates/cli/src/frontend/execution/file_list/parser.rs:60-125` | not applicable | Audit `docs/audits/windows-path-separator-encoding.md`. |
| Drive letters (`C:\path`) | not applicable | not applicable | yes | F | `crates/cli/src/frontend/tests/operands.rs` (parser keeps `:` from being mistaken as `host:path`) | not applicable | Audit `docs/audits/windows-path-edge-cases.md`. |
| UNC paths (`\\server\share\path`) | not applicable | not applicable | yes | F | `crates/cli/src/frontend/execution/file_list/tests.rs:511` | not applicable | Audit `docs/audits/windows-path-normalization.md`. |
| Verbatim paths (`\\?\C:\...`) | not applicable | not applicable | yes | F | `crates/cli/src/frontend/tests/operands.rs:10` | not applicable | Required for paths > MAX_PATH. |
| iconv charset conversion | yes (feature `iconv`) | yes (feature `iconv`) | no by default | P | `crates/protocol/src/iconv/converter.rs` | identity converter when feature off | Audits `iconv-feature-design.md`, `iconv-pipeline.md`, `iconv-inert.md`. Windows builds typically omit the feature. |

### 2.9 SSH transport

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| External `ssh` spawn | yes | yes | yes (`ssh.exe`) | F | `crates/rsync_io/src/ssh/builder.rs` | not applicable | Cross-platform `Command::spawn`. |
| SSH child reaping | yes (Drop + waitpid) | yes | yes | F | `crates/rsync_io/src/ssh/connection.rs` (SshChildHandle) | not applicable | `SshChildHandle::Drop` reaps child to prevent zombies. |
| Raw fd setup for child stdio | yes | yes | no (uses pipes only) | L+F | `crates/rsync_io/src/ssh/builder.rs` | child stdio via `Stdio::piped()` on Windows | Audit `docs/audits/ssh-socketpair-vs-pipes.md`. |
| ControlMaster / multiplex | external (delegates to user's ssh client) | external | external | F | not implemented in oc-rsync | not applicable | oc-rsync does not implement SSH internally for the default path. |
| Identity files (`-i`) | yes | yes | yes | F | `crates/rsync_io/src/ssh/embedded/config.rs:67` (embedded ssh feature) | not applicable | Default identity files probed under `~/.ssh/`. |
| ssh-agent (`SSH_AUTH_SOCK`) | yes (delegated) | yes (delegated) | partial (delegated to OpenSSH for Windows) | F | external ssh handles agent | not applicable | Embedded SSH (feature `embedded-ssh`) reads agent socket via libssh2; tracked separately. |
| Cipher / compression negotiation | yes | yes | yes | F | external ssh handles negotiation | not applicable | Audit `docs/audits/ssh-cipher-compression.md`. |
| Capability string `-e.LsfxCIvu` | yes | yes | yes | F | `crates/core/src/client/remote/setup.rs::build_capability_string` | not applicable | Single source of truth for both directions. |

### 2.10 Daemon mode

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| TCP listener | yes | yes | yes | F | `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs` | not applicable | Cross-platform `TcpListener`. |
| `oc-rsyncd.conf` parsing | yes | yes | yes | F | `crates/daemon/src/config/` | not applicable | Cross-platform; `chroot` directive prints warning on Windows. |
| `@RSYNCD:` negotiation, auth | yes | yes | yes | F | `crates/daemon/src/auth.rs` | not applicable | Cross-platform protocol code. |
| Unix domain socket listener | yes | yes | no | L+F | `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs` | error on Windows | Windows AF_UNIX could be added but is not in scope; tracked alongside #1869. |
| Daemonize (fork+setsid+stdio redirect) | yes | yes | no | L+F | `crates/platform/src/daemonize.rs` | `ErrorKind::Unsupported` | Windows uses Service Control Manager instead. |
| Windows Service Control Manager | no | no | yes | F | `crates/platform/src/windows_service.rs` | not applicable | Real `run_service_dispatcher`, `install_service`, `uninstall_service` via Win32 SCM. |
| Privilege drop (setuid/setgid/setgroups) | yes | yes | partial (LogonUserW + ImpersonateLoggedOnUser) | P | `crates/platform/src/privilege.rs` | no-op on platforms without either model | Windows path uses token impersonation for `--copy-as`-like semantics. |
| Pre/post-xfer exec | yes (`sh -c`) | yes (`sh -c`) | yes (`cmd /C`) | F | `crates/daemon/src/daemon/sections/xfer_exec.rs` | not applicable | Same env vars on all platforms. |
| sd_notify (systemd) | yes (feature) | no | no | L+F | `crates/daemon/src/systemd.rs` | inert helper | Linux-only, default-on for Linux builds. |
| Syslog backend | yes | yes (BSD syslog) | no | L+F | `crates/logging/src/sinks/syslog.rs` | event-log not implemented; falls back to stderr | Audit `docs/audits/windows-acl-xattr-ci-matrix.md` notes the syslog gap. |
| Secrets-file permission check | yes | yes | partial | P | `crates/platform/src/secrets.rs` | no-op `Ok(())` on Windows (no mode bits) | Windows ACL-based check is a future task. |
| Name converter (NSS in chroot) | yes (subprocess via NSS) | yes | yes (LookupAccountNameW) | F | `crates/daemon/src/daemon/sections/name_converter.rs` | not applicable | Windows variant inline, no subprocess needed. |

### 2.11 Signal handling and shutdown

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| SIGINT / Ctrl-C | yes (signal_hook) | yes | yes (SetConsoleCtrlHandler) | F | `crates/platform/src/signal.rs`, `crates/core/src/signal/` | atomic-flag stub | Both platforms drive the same `ShutdownReason` flag. |
| SIGTERM | yes | yes | partial (CTRL_CLOSE_EVENT) | P | `crates/platform/src/signal.rs` | atomic-flag stub | Maps `CTRL_CLOSE` and `CTRL_LOGOFF` to graceful shutdown. |
| SIGHUP (config reload) | yes | yes | no (named-event reload not implemented) | L | `crates/core/src/signal/unix.rs` | no fallback | Audit `docs/audits/known-failures-eliminate.md`. Tracked as named-event reload in `windows_platform_parity.md` section 1. |
| SIGPIPE | yes | yes | not applicable | L+F | `crates/core/src/signal/unix.rs` | not needed (Windows uses ECONNRESET on broken sockets) | |
| Forced abort (second signal) | yes | yes | partial (Ctrl-C only, programmatic via `request_abort`) | P | `crates/core/tests/sigint_temp_cleanup.rs` | atomic flag `force_abort` | |
| Temp-file cleanup on signal | yes | yes | yes | F | `crates/core/src/cleanup.rs` (CleanupManager) | not applicable | Cross-platform RAII cleanup. |
| Exit-code mapping (rsync codes 20-30) | yes | yes | yes | F | `crates/core/src/exit_code.rs` | not applicable | Signal-death codes path is Unix-only; Windows maps Ctrl-C to ExitCode::Signal(20). |

### 2.12 Checksums and SIMD

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| Adler32 / rsum scalar | yes | yes | yes | F | `crates/checksums/src/rolling/checksum/scalar.rs` | not applicable | |
| AVX2 rolling checksum | yes (x86_64) | yes (x86_64) | yes (x86_64) | F | `crates/checksums/src/rolling/checksum/avx2.rs` | runtime probe -> SSE2 -> scalar | Cached via `OnceLock`. |
| SSE2 rolling checksum | yes (x86_64) | yes (x86_64) | yes (x86_64) | F | `crates/checksums/src/rolling/checksum/sse2.rs` | runtime probe -> scalar | |
| NEON rolling checksum | yes (aarch64) | yes (aarch64) | aarch64 cross-compile only | F | `crates/checksums/src/rolling/checksum/neon.rs` | runtime probe -> scalar | Windows aarch64 release build disabled; SIMD path still compiles. |
| MD4 / MD5 strong checksums | yes | yes | yes | F | `crates/checksums/src/strong/` | not applicable | Pure Rust. |
| XXH3 / XXH128 | yes | yes | yes | F | `crates/checksums/src/strong/xxh3.rs` | not applicable | xxhash-rust crate; SIMD via target features. |

### 2.13 Compression

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| zlib (deflate) | yes | yes | yes | F | `crates/compress/src/zlib.rs` | not applicable | flate2 crate. |
| zlib-ng (optional) | yes | yes | yes | F | feature `zlib-ng` | falls back to plain zlib | Cross-platform via build script. |
| zstd | yes | yes | yes | F | `crates/compress/src/zstd.rs` | feature gate; degrades to zlib if disabled | `--compress` selects codec. |
| lz4 | yes | yes | yes | F | `crates/compress/src/lz4.rs` | feature gate | |

### 2.14 Filesystem stat and batching

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| statx (DONT_SYNC, masked fields) | yes (glibc) | not applicable | not applicable | L+F | `crates/metadata/src/stat_cache.rs` | `fs::metadata()` on other Unix; `permissions().readonly()` on Windows | Linux musl falls through to plain `stat()`. |
| Parallel stat (rayon) | yes | yes | yes | F | `crates/transfer/src/receiver/quick_check.rs` (PARALLEL_STAT_THRESHOLD = 64) | not applicable | Threshold-based dual path (sequential below threshold, parallel above). |
| Batched fs::metadata for flist | yes | yes | yes | F | `crates/flist/src/batched_stat/` | not applicable | Linux uses statx underneath. |
| Reflink detection | yes (fs probe) | not needed (clonefile is filesystem-aware) | yes (GetVolumeInformationByHandleW) | F | `crates/fast_io/src/refs_detect.rs` | always-false stub on Linux/macOS | Windows ReFS detection landed; FSCTL wiring is the remaining gap (#1389). |

### 2.15 Build, packaging, distribution

| Feature | Linux | macOS | Windows | Status | Code path | Fallback | Notes |
|---|---|---|---|---|---|---|---|
| Workspace compile | x86_64, x86_64-musl, aarch64 | x86_64, aarch64 | x86_64-msvc, x86_64-gnu | F | `.github/workflows/ci.yml`, `release-cross.yml` | not applicable | aarch64-windows is disabled in `release-cross.yml`. |
| Static linking | yes (musl) | not applicable | not applicable | F | `.github/workflows/ci.yml:447` | not applicable | musl build verified with `ldd` check. |
| deb/rpm packaging | yes | not applicable | not applicable | F | `packaging/deb/`, `packaging/rpm/` | not applicable | Linux-only. |
| Homebrew formula | not applicable | yes | not applicable | F | `Formula/oc-rsync.rb` | not applicable | macOS-only. |
| MSI installer | not applicable | not applicable | not implemented | L | not applicable | none | Windows users install the standalone `.exe`. |
| Windows GNU exception handling | not applicable | not applicable | yes | F | `crates/windows-gnu-eh/` | not applicable | Required for the GNU MinGW target. |

## 3. Confirmed gaps with task IDs

The classification above flags these as gaps in user-visible behaviour
on at least one platform. Each row is a single-line impact statement
that points to the task ID and the bucket the row lives in.

- **#1361 - sendfile / splice / vmsplice on macOS/Windows.** L+F today.
  No data-correctness gap; daemon TCP path on macOS/Windows runs
  buffered I/O with measurable throughput cost on bulk transfers.
- **#1389 - reflink CoW on Windows.** P today (detection only). FSCTL
  wiring is missing, so ReFS volumes never see O(1) clones; falls back
  to `CopyFileExW`. No correctness gap; performance gap on ReFS.
- **#1414 / #1749 - copy_file_range / clonefile / CopyFileExW dispatch
  audit.** F. The dispatch chain is implemented end-to-end. Tracked as
  follow-up to verify that the chain ordering remains optimal as
  upstream rsync evolves.
- **#1653 - macOS dispatch_io backend.** L. macOS receiver has no async
  fast path. No correctness gap; macOS receiver throughput trails
  Linux io_uring and Windows IOCP. Audit
  `docs/audits/macos-dispatch-io.md` plans phases 1-6.
- **#1657 - Windows fake-super via NTFS ADS.** L+F today. xattr-stored
  fake-super metadata is unavailable on Windows builds without the
  `xattr` feature wired in to ADS; with the feature on it now works
  end-to-end via `xattr_windows.rs`. Audit
  `docs/audits/fake-super-privilege.md`.
- **#1662 - madvise MADV_WILLNEED prefault on macOS/Windows.** P. Today
  Windows mmap reader is a buffered fallback; macOS supports madvise
  but its WILLNEED hint is not driven from receiver-side basis-file
  reads. Audit `docs/audits/madvise-willneed-prefault.md`.
- **#1869 - Windows ACL/xattr CI coverage.** F (code) but P (CI). Real
  Windows ACL/ADS implementations exist (`acl_windows.rs`,
  `xattr_windows.rs`), exercised by the dedicated `windows-acl-xattr`
  job. The audit `docs/audits/windows-acl-xattr-ci-matrix.md`
  recommends extending the matrix with `--no-default-features` and
  stub-warning coverage.
- **kqueue / dispatch_io as macOS event loop.** L. Captured by the
  dispatch_io audit (#1653); kqueue is intentionally not used because
  it is the lower-level primitive.
- **SIGHUP-equivalent reload on Windows (named event).** L. Daemon
  config reload requires manual restart on Windows. Tracked in
  `docs/windows_platform_parity.md` section 1.
- **Windows MSI installer.** L. Not user-visible blocker; release
  artefacts ship the standalone `.exe`.

Each row above corresponds to a row in the matrix in section 2 with
the same status code, so the audit and the gap list stay in sync.

## 4. CI exercise per feature

Source: `.github/workflows/ci.yml`. The Linux `nextest` job runs
`cargo nextest run --workspace --all-features` on `ubuntu-latest`. The
Windows and macOS jobs run only `-p core -p engine -p cli
--all-features` plus targeted feature jobs. The Linux musl job runs
the workspace minus `xattr` differences and the iocp/io_uring features
do not exist on musl. The interop, benchmark, and coverage workflows
are Linux-only.

| Surface | Linux nextest | Linux musl | macOS | Windows | Notes |
|---|---|---|---|---|---|
| io_uring (file + socket) | yes | partial (default-features off; feature exists but not selected) | not applicable | not applicable | `_test-features.yml` linux_only entry. |
| IOCP | not applicable | not applicable | not applicable | yes (windows-iocp job) | `ci.yml:223-270`. |
| copy_file_range | yes | yes (explicit feature) | not applicable | not applicable | Linux only. |
| clonefile | not applicable | not applicable | implicit (covered by core/engine/cli copy tests) | not applicable | macOS clonefile fast path is exercised but not isolated. **Gap**: no dedicated macOS clonefile test job. |
| CopyFileExW | not applicable | not applicable | not applicable | yes (via core/engine/cli) | Tested implicitly. |
| ReFS reflink (FSCTL_DUPLICATE_EXTENTS) | not applicable | not applicable | not applicable | partial | Detection covered; FSCTL not yet wired. |
| O_TMPFILE | yes | yes | not applicable | not applicable | Linux fallback test in `o_tmpfile/tests`. |
| sendfile / splice | yes | yes | not applicable | not applicable | Linux only. |
| madvise | yes | yes | implicit | not applicable | Windows stub trivially exercised. |
| ACLs (`-A`) | yes (`metadata` is in workspace nextest) | yes (without `acl` feature) | partial (`metadata` not in macOS job - **gap**) | yes (`windows-acl-xattr` dedicated job) | macOS ACL via exacl is gated to its own runner pattern. |
| xattrs (`-X`) | yes | not applicable (`xattr` feature off in musl matrix) | partial (same gap) | yes (`windows-acl-xattr` dedicated job) | NTFS ADS coverage on Windows. |
| AppleDouble + resource forks | yes (filter logic) | yes (filter logic) | yes (`apple-fs` is in macOS workspace build) | yes (filter logic) | Resource fork code only exercised on macOS runner. |
| Unicode / Windows path normalization | yes | yes | yes | yes | `cli` test suite covers all platforms. |
| SSH transport spawn | yes (with loopback ssh) | partial (no ssh setup) | partial (no ssh setup - **gap**) | partial (no ssh setup - **gap**) | Audit `docs/audits/ssh-transport-timeout-coverage.md`. |
| Daemon mode | yes | yes | partial | partial (TCP only; no Windows Service test) | Audit `docs/audits/async-daemon-listener.md`. |
| SIGINT / Ctrl-C | yes | yes | yes | yes | `crates/core/tests/sigint_temp_cleanup.rs` runs on every platform. |
| User/group lookup | yes | yes | yes | yes (windows lookup tested via `windows-acl-xattr`) | |
| Hardlink / symlink restore | yes | yes | yes | yes (limited - directory symlinks not type-detected) | Audit `docs/audits/known-failures-eliminate.md`. |
| AVX2 / SSE2 rolling checksum | yes (x86_64) | yes (x86_64) | yes (x86_64) | yes (x86_64) | SIMD parity tests run on every runner. |
| NEON rolling checksum | yes (linux musl uses x86_64 only) | not applicable | yes (aarch64 macOS) | not applicable (no aarch64 windows runner) | |
| iconv | yes (feature on) | partial (feature on for `iconv` musl entry) | yes | partial (default off on Windows) | |

CI gaps surfaced by the table above:

1. **No isolated macOS clonefile job.** Failures fold into the macOS
   `core+engine+cli` tests. A dedicated job analogous to `windows-iocp`
   would tighten the loop.
2. **`metadata` crate not in the macOS or Windows main test list.** The
   `windows-acl-xattr` job covers Windows; macOS has no equivalent for
   exacl/xattr unit tests. Recommend extending `macos-test` with
   `-p metadata`.
3. **No SSH integration tests on macOS or Windows.** SSH loopback only
   runs on Linux (`ci.yml:127-140`). Audit
   `ssh-transport-timeout-coverage.md` flags this.
4. **No daemon-mode integration tests on Windows.** Daemon TCP listener
   compiles and runs, but no workflow exercises it end-to-end on a
   Windows runner.

## 5. Recommendations

Ordered by user-visible blast radius. Numbered to match the cross-cutting
issues in section 3.

1. **Wire FSCTL_DUPLICATE_EXTENTS_TO_FILE on Windows (#1389).** ReFS
   reflink detection landed; the dispatch chain is the remaining gap.
   Single-PR-sized change in `crates/fast_io/src/platform_copy/dispatch.rs`.
   Largest blast radius for Windows users on ReFS volumes - replaces
   minutes of `CopyFileExW` with O(1) clones.
2. **Land macOS dispatch_io backend phases 1-4 (#1653).** macOS
   receiver currently runs synchronous buffered I/O. The audit
   recommends a feature-gated implementation that mirrors the IOCP
   layout. This is the single largest macOS performance gap.
3. **Add `-p metadata` to the macOS CI test job and add a dedicated
   macOS clonefile job.** Mechanical YAML edits. Without these, every
   PR that touches metadata or clonefile silently passes on macOS.
4. **Drive MADV_WILLNEED on macOS receiver-side basis reads (#1662).**
   Memory-mapped reads in the delta-apply path can prefault basis
   blocks; macOS supports madvise but the hint is not invoked. Smaller
   blast radius but mechanically simple.
5. **Implement Windows daemon SCM integration test.** The SCM runner
   exists in `crates/platform/src/windows_service.rs`. A smoke test
   that registers and unregisters a service against a Windows runner
   would close the daemon-on-Windows coverage gap.
6. **Add SSH loopback tests on macOS and Windows.** Add the same
   ssh-keygen / loopback step that `ci.yml:127` runs on Linux. macOS
   ships OpenSSH; Windows ships OpenSSH server as an optional feature.
7. **Document and gate the AppleDouble + resource-fork tests under
   macOS.** Resource-fork code is `cfg(target_os = "macos")` but the
   audit table shows the macOS runner exercises it implicitly via
   `apple-fs`. A dedicated test invocation
   (`-p apple-fs --features apple-fs`) would make this explicit.
8. **Track Windows MSI packaging in a follow-up issue.** Not blocking
   today, but the absence is the largest packaging gap on the matrix.

## 6. References

- Existing platform docs:
  - `docs/windows_platform_parity.md` - Windows-specific cfg inventory.
  - `docs/platform-io-fast-paths.md` - I/O fallback chain narrative.
  - `docs/platform-support.md` - feature parity including iOS targets.
  - `docs/platform-notes.md` - metadata stub matrix.
- Per-feature audits:
  - `docs/audits/windows-acl-xattr-ci-matrix.md` (#1869)
  - `docs/audits/macos-dispatch-io.md` (#1653)
  - `docs/audits/madvise-willneed-prefault.md` (#1662)
  - `docs/audits/splice-ssh-stdio.md` (#1361)
  - `docs/audits/fake-super-privilege.md` (#1657)
  - `docs/audits/windows-path-normalization.md`
  - `docs/audits/windows-path-edge-cases.md`
  - `docs/audits/windows-path-separator-encoding.md`
  - `docs/audits/iocp-pbuf-ring.md`
  - `docs/audits/iouring-pbuf-ring.md`
  - `docs/audits/ssh-cipher-compression.md`
  - `docs/audits/ssh-transport-timeout-coverage.md`
  - `docs/audits/async-daemon-listener.md`
- CI surface: `.github/workflows/ci.yml` jobs `lint`, `test`,
  `feature-flags`, `windows-test`, `windows-iocp`, `windows-acl-xattr`,
  `windows-gnu-cross-check`, `macos-test`, `linux-musl`,
  `interop-upstream`.
- Code anchors: every `Code path` cell in section 2 points to the file
  or module that owns the platform-specific implementation; matching
  stub modules sit alongside under the same crate.
