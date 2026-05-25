# Windows support matrix

User-facing reference for what oc-rsync supports on Windows today,
what is partially supported, and what is explicitly out of scope.
This document gives operators an unambiguous compatibility view
before the deeper Windows audit work in the WPC series (#2905 -
#2914) lands.

Tracks parent #2869 (Windows real-world parity series) and follows
WPC-1 (#2903) and WPC-2 (#2904). Forthcoming tasks WPC-3..WPC-12
fill in the rows marked Partial below; this matrix will be revised
in place as each task ships.

## 1. Overview

oc-rsync runs on Windows via the GNU toolchain
(`x86_64-pc-windows-gnu`) and uses I/O Completion Ports (IOCP) for
the I/O dispatch fast path. io_uring is Linux-only and has no
Windows port; the IOCP path is the permanent Windows replacement and
covers metadata calls today, with the data path going through the
`std::fs::copy` / `CopyFileExW` / `FSCTL_DUPLICATE_EXTENTS_TO_FILE`
dispatch chain. See `docs/audit/windows-copyfileex-platform-copy.md`
and the memory note on Windows io_uring scope for the structural
reason.

Most rsync-level behaviour - the wire protocol, delta engine,
checksum negotiation, compression, bandwidth limiter, daemon mode,
SSH transport, filter rules - works identically on Windows because
those subsystems are platform-independent. The matrix below covers
the platform-specific surface: NTFS metadata, security descriptors,
reparse points, and Windows file-system quirks.

## 2. Feature matrix

Status legend:

- **Full** - implemented, exercised in CI, no known gaps versus the
  documented behaviour on POSIX hosts.
- **Partial** - works for the common case but has documented
  limitations or pending audits; see the linked WPC tracking task.
- **Unsupported** - not implemented; the call either no-ops, falls
  back to a portable path, or returns a typed unsupported error.

| Feature | Status | Notes |
|---------|--------|-------|
| Basic file transfer (`-r`, `-a`) | Full | IOCP-backed I/O dispatch; full data path through `CopyFileExW` and ReFS reflink where available. |
| Permissions (`-p`) | Partial | POSIX bits are stored and round-tripped on the wire; the receiver maps the write bit to the NTFS read-only flag. SUID/SGID/sticky bits are silently ignored. Bidirectional fidelity audit pending under WPC-12. |
| `--chmod` modifiers | Partial | Parsed and applied to the wire-format POSIX mode bits. NTFS DACLs are not affected unless `-A` is also passed. Audited end-to-end under WPC-12. |
| Owner / group (`-o`, `-g`) | Partial | NTFS uses SIDs, not POSIX uid/gid. Names round-trip via `LookupAccountNameW` when `-A` is present; without `-A` the numeric ids are passthrough only and do not bind to a Windows principal. |
| `--usermap` / `--groupmap` / `--chown` | Unsupported | Returns `MappingParseError` on Windows. No POSIX-to-SID mapping table. |
| `--numeric-ids` | Full | Pass-through; no NSS lookup is attempted on Windows. |
| Symbolic links (`-l`) | Partial | Created via `std::os::windows::fs::symlink_file` / `symlink_dir`. Requires `SeCreateSymbolicLinkPrivilege` or Developer Mode on the receiver; without the privilege the call fails with `ERROR_PRIVILEGE_NOT_HELD`. Directory vs file symlink type detection is pending under WPC-7. |
| Hard links (`-H`) | Full | Uses `std::fs::hard_link`; NTFS hardlinks honored within the same volume. |
| Timestamps (`-t`) | Full | NTFS supports 100 ns resolution. mtime and atime are preserved via the `filetime` crate. crtime support is currently Cygwin-only on the upstream side and is not exposed on the native Windows build. |
| Extended attributes (`-X`) | Partial | NTFS alternate data streams (ADS) are surfaced through the cross-platform xattr pipeline via `FindFirstStreamW` / `FindNextStreamW`. Without `-X`, ADS are silently dropped, matching upstream rsync's Cygwin default. See `docs/design/windows-ads-strategy.md` (WPC-2) for the binding strategy and `docs/audit/windows-ads-handling.md` (WPC-1) for the audit; the one-shot warning when ADS is detected without `-X` ships under WPC-3. |
| POSIX ACLs (`-A`) | Partial | Maps to NTFS DACLs via the `windows-acl`-backed implementation in `crates/metadata/src/acl_windows/`. POSIX `user::` / `group::` / `other::` triples translate to allow ACEs on the corresponding SIDs. Inherited-ACE round-trip fidelity is pending under WPC-10. |
| NFSv4 ACLs | Partial | Stored via the xattr backend on Windows. End-to-end NFSv4-to-DACL conversion audit pending. |
| `--fake-super` | Unsupported | Requires xattr support for `user.rsync.%stat`. Returns `ErrorKind::Unsupported` on Windows. |
| `--copy-as=USER[:GROUP]` | Unsupported | No-op guard returns `Ok(())`; no Windows equivalent shipped. |
| FIFOs / sockets / devices (`--specials`, `-D`) | Unsupported | No `mknod` on Windows; the call returns `Ok(())` without creating the special file. Same behaviour upstream rsync exhibits on Cygwin. |
| Sparse files (`-S`) | Full | Zero-run detection via the cross-platform `fast_io` path; `FSCTL_SET_ZERO_DATA` used for sparse hole creation on NTFS. |
| Compression (`-z`, `--zc=...`) | Full | Wire codec is platform-independent. zlib and zstd both available. |
| Checksum negotiation (`--checksum-choice`) | Full | XXH3 / XXH128 / MD5 / MD4 paths all platform-independent; SIMD fast paths use AVX2 and SSE2 on Windows x86_64. |
| Bandwidth limits (`--bwlimit`) | Full | Userspace rate limiter; no platform dependency. |
| Filter rules (`--filter`, `--include`, `--exclude`, `.rsync-filter`) | Full | Cross-platform; path-component matching uses the platform's native separator. |
| `--delete` and variants | Full | Receiver-side deletion uses the sandboxed deletion path landed under SEC-1.q2. |
| `--inplace` | Full | NTFS in-place writes honored; no temp-file rename. |
| `--partial` / `--partial-dir` | Full | NTFS rename semantics honored on commit. |
| `--copy-links` (`-L`) | Full | Symlink target resolution is platform-aware; reparse-point classification follows the symlink chain. |
| Daemon mode (`oc-rsync --daemon`) | Full | Listens via Winsock; `oc-rsyncd.conf` parsing, module configuration, `@RSYNCD:` handshake, and admission gating via `--max-connections` are all functional. Logging to Event Log is not wired; logs go to STDERR. |
| SSH transport (russh) | Full | Compatible with OpenSSH for Windows, PuTTY (`plink`), and any RFC 4253 server. SSH compression detection from `~/.ssh/config` is incomplete cross-platform; see the memory note on SSH compression detection. |
| Batch mode (`--write-batch`, `--read-batch`, `--only-write-batch`) | Full | Batch file format is platform-independent; replay honors the platform's native metadata application path. |
| Windows service mode | Full | `oc-rsync --daemon` can be installed as a Windows service via the Win32 Service Control Manager (`crates/platform/src/windows_service.rs`). |
| Signal handling (Ctrl+C, Ctrl+Break, console close) | Full | `SetConsoleCtrlHandler` maps CTRL_C / CTRL_CLOSE to shutdown and CTRL_BREAK to graceful exit. SIGHUP-equivalent config reload via named events is not wired. |

## 3. Known limitations

Concrete Windows-specific gaps still open:

- **Alternate data streams (ADS)**: silently dropped unless `-X` is
  enabled, matching upstream rsync on Cygwin. The audit lives at
  `docs/audit/windows-ads-handling.md` (WPC-1) and the binding
  strategy at `docs/design/windows-ads-strategy.md` (WPC-2). The
  one-shot warning when ADS is detected without `-X` ships under
  **WPC-3 (#2905)**; the regression test ships under **WPC-4
  (#2906)**.
- **Long paths (> 260 chars)**: support is unaudited. The `\\?\`
  prefix is not currently applied by the path builder, so
  `MAX_PATH`-bound APIs may fail before the IOCP path is reached.
  Tracking under **WPC-5 (#2907)** for the audit and **WPC-6
  (#2908)** for the long-path runtime fix.
- **Reparse points (junctions, mount points, OneDrive placeholders,
  Cloud Files API placeholders)**: today classified uniformly as
  symlinks. Finer classification and behavioural fidelity for
  non-symlink reparse points is pending under **WPC-7 (#2909)**,
  **WPC-8 (#2910)**, and **WPC-9 (#2911)**.
- **DACL inherited ACEs vs explicit ACEs**: explicit ACEs round-trip
  via the `windows-acl` crate. Inherited ACEs may be re-materialised
  as explicit ACEs on the receiver, losing the inheritance flag.
  Round-trip fidelity audit pending under **WPC-10 (#2912)**.
- **Case-insensitive filesystem conflict detection**: NTFS preserves
  case but matches case-insensitively by default. The receiver does
  not currently detect a source-side `a.txt` vs `A.txt` collision
  before applying the second write. Audit pending under **WPC-11
  (#2913)**.
- **Windows attribute bits (read-only, hidden, system, archive)**:
  only the read-only bit is mapped today, derived from the POSIX
  write bit. Bidirectional mapping for hidden / system / archive
  bits to xattrs is pending under **WPC-12 (#2914)**.
- **No io_uring on Windows**: I/O dispatch uses IOCP exclusively.
  This is a structural decision, not a gap. See the memory notes
  on Windows io_uring scope and the IOCP wiring history.
- **SUID / SGID / sticky bits**: silently ignored on the receiver.
  No NTFS equivalent.
- **POSIX special files**: FIFOs, sockets, block and character
  devices cannot be created on NTFS. `--specials` and `-D` no-op,
  matching upstream rsync's Cygwin behaviour.

## 4. Build and packaging requirements

The Windows builds shipped in the release pipeline use the GNU
toolchain:

- **Toolchain triple**: `x86_64-pc-windows-gnu`.
- **C cross-compiler**: GCC 13 or later via the `x86_64-w64-mingw32`
  toolchain, or the project `Cross.toml` consumed by `cargo-cross`.
- **ABI bridge**: the `windows-gnu-eh` crate supplies the SEH /
  Itanium-ABI exception-handling bridge required for the
  `windows-rs` crate family.
- **Pre-built artifacts**: GitHub Releases ship a
  `x86_64-pc-windows-gnu` `.zip` per tag containing `oc-rsync.exe`
  and the daemon configuration template.

Building from source on a Windows host is supported via the same
GNU toolchain. The MSVC toolchain is not audited (see the next
section). For the rationale behind picking GNU over MSVC, see
`docs/audit/windows-gnu-vs-msvc-evaluation.md`.

## 5. What is NOT supported

Explicit non-goals to set operator expectations:

- **`x86_64-pc-windows-msvc` builds**. The MSVC toolchain compiles
  but is not exercised in CI and is not audited for ABI parity
  with the GNU build. Use `x86_64-pc-windows-gnu`. See the GNU vs
  MSVC evaluation referenced above.
- **WSL bridging**. Running `oc-rsync` inside WSL is treated as a
  Linux build (the binary is `x86_64-unknown-linux-gnu`, not a
  Windows binary). WSL filesystem quirks (DrvFs case sensitivity,
  metadata translation, `\\wsl$` UNC paths) are outside the scope
  of this document.
- **Cygwin compatibility mode**. Upstream rsync ships a Cygwin
  port; oc-rsync does not. Users running on Cygwin should run the
  upstream Cygwin rsync, not the oc-rsync Windows GNU binary.
- **NTFS object IDs, USN journal entries, and EFS-encrypted file
  re-encryption**. None of these are part of the rsync protocol or
  the upstream feature set, and oc-rsync does not preserve them.
- **Native Windows Event Log integration**. Daemon mode logs to
  STDERR; Event Log routing is not wired.

## 6. Cross-references

Upstream and internal documentation:

- `docs/audit/windows-ads-handling.md` - ADS handling audit
  (WPC-1, #2903).
- `docs/design/windows-ads-strategy.md` - ADS strategy decision
  (WPC-2, #2904).
- `docs/audit/windows-ntfs-acl-support.md` - NTFS ACL backend audit
  consumed by the WAS-1..WAS-8 implementation series.
- `docs/audit/windows-acl-xattr-ci-matrix.md` - CI coverage matrix
  for ACL and xattr round-trip on Windows.
- `docs/audit/windows-copyfileex-platform-copy.md` - data-path
  dispatch chain (`CopyFileExW`, ReFS reflink).
- `docs/audit/windows-gnu-vs-msvc-evaluation.md` - toolchain choice
  rationale.
- `docs/platform-support.md` - full cross-platform feature matrix
  (Linux / macOS / Windows side-by-side).
- `docs/windows_platform_parity.md` - per-`cfg(unix)`-block audit
  used to derive the rows above.
- `docs/user/daemon-concurrency-limits.md` - daemon admission and
  connection-cap sizing.

Tracking tasks:

- Parent: **#2869** (Windows real-world parity series).
- Sibling completed: **WPC-1 (#2903)**, **WPC-2 (#2904)**.
- Sibling pending: **WPC-3..WPC-12 (#2905-#2914)** - each row in
  the matrix marked Partial is owned by one of these tasks.
- This document: **WPC-13 (#2915)**.

Memory cross-links (internal): `[[project_windows_real_world_parity_unclear]]`,
`[[project_no_windows_io_uring]]`, `[[project_windows_parity_wip]]`,
`[[project_xattr_acl_cross_platform_parity_gap]]`,
`[[project_iocp_not_wired]]`.
