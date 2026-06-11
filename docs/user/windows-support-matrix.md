# Windows support matrix

User-facing reference for what oc-rsync supports on Windows today,
what is partially supported, and what is explicitly out of scope.
This document gives operators an unambiguous compatibility view
including per-subsystem maturity levels.

Tracks parent #2869 (Windows real-world parity series). The full
WPC audit series (WPC-1 through WPC-13) is complete. The WIN-G
series (WIN-G.c, WIN-G.d, WIN-G.g) covers EH ABI status and
release implications. The WPG series (IOCP hardware profiling)
is designed but deferred pending dedicated hardware.

## 1. Overview

oc-rsync runs on Windows via the MSVC toolchain
(`x86_64-pc-windows-msvc`) and uses I/O Completion Ports (IOCP) for
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
  limitations; see the linked WPC tracking task.
- **Unsupported** - not implemented; the call either no-ops, falls
  back to a portable path, or returns a typed unsupported error.

Maturity legend:

- **Production** - implemented, CI-validated, audited, no outstanding
  validation gaps.
- **CI-validated** - implemented and exercised in CI but not validated
  on real hardware or in production-like environments (e.g., IOCP not
  profiled on physical NTFS, DACL not tested in Active Directory).
- **Audited** - code-level audit completed with findings documented;
  implementation exists but validation coverage is limited.
- **Resolved** - concern investigated and closed with no remaining
  action (e.g., zero-cost no-op on shipped binaries).
- **N/A** - platform-independent; maturity is not a Windows-specific
  concern.

| Feature | Status | Maturity | Notes |
|---------|--------|----------|-------|
| Basic file transfer (`-r`, `-a`) | Full | CI-validated | IOCP-backed I/O dispatch; full data path through `CopyFileExW` and ReFS reflink where available. IOCP not hardware-profiled on physical NTFS. |
| Permissions (`-p`) | Partial | Audited | POSIX bits round-trip on the wire; the receiver maps the write bit to the NTFS read-only flag. SUID/SGID/sticky bits silently ignored. Bidirectional fidelity audited under WPC-12; see `docs/audit/windows-perm-bits-posix-mapping.md`. |
| `--chmod` modifiers | Partial | Audited | Parsed and applied to wire-format POSIX mode bits. NTFS DACLs not affected unless `-A` is also passed. Audited end-to-end under WPC-12. |
| Owner / group (`-o`, `-g`) | Partial | CI-validated | NTFS uses SIDs, not POSIX uid/gid. Names round-trip via `LookupAccountNameW` when `-A` is present; without `-A` the numeric ids are passthrough only. Not tested in Active Directory environments. |
| `--usermap` / `--groupmap` / `--chown` | Unsupported | N/A | Returns `MappingParseError` on Windows. No POSIX-to-SID mapping table. |
| `--numeric-ids` | Full | Production | Pass-through; no NSS lookup is attempted on Windows. |
| Symbolic links (`-l`) | Partial | CI-validated | Created via `std::os::windows::fs::symlink_file` / `symlink_dir`. Requires `SeCreateSymbolicLinkPrivilege` or Developer Mode on the receiver. |
| Hard links (`-H`) | Full | Production | Uses `std::fs::hard_link`; NTFS hardlinks honored within the same volume. |
| Timestamps (`-t`) | Full | Production | NTFS supports 100 ns resolution. mtime and atime preserved via the `filetime` crate. crtime is Cygwin-only upstream and not exposed on native Windows. |
| Extended attributes (`-X`) | Partial | Audited | ADS surfaced through the cross-platform xattr pipeline via `FindFirstStreamW` / `FindNextStreamW`. Without `-X`, ADS silently dropped, matching upstream. One-shot ADS warning implemented per WPC-3. See `docs/audit/windows-ads-handling.md`. |
| POSIX ACLs (`-A`) | Partial | CI-validated | Maps to NTFS DACLs via the `windows-acl`-backed implementation. POSIX triples translate to allow ACEs on corresponding SIDs. Inherited-ACE round-trip audited under WPC-10; see `docs/audit/windows-dacl-ace-inheritance.md`. Not validated in Active Directory. |
| NFSv4 ACLs | Partial | Audited | Stored via the xattr backend on Windows. End-to-end NFSv4-to-DACL conversion audit pending. |
| `--fake-super` | Unsupported | N/A | Requires xattr support for `user.rsync.%stat`. Returns `ErrorKind::Unsupported` on Windows. |
| `--copy-as=USER[:GROUP]` | Unsupported | N/A | No-op guard returns `Ok(())`; no Windows equivalent shipped. |
| FIFOs / sockets / devices (`--specials`, `-D`) | Unsupported | N/A | No `mknod` on Windows; the call returns `Ok(())` without creating the special file. Same behaviour upstream rsync exhibits on Cygwin. |
| Sparse files (`-S`) | Full | Production | Zero-run detection via the cross-platform `fast_io` path; `FSCTL_SET_ZERO_DATA` used for sparse hole creation on NTFS. |
| Compression (`-z`, `--zc=...`) | Full | N/A | Wire codec is platform-independent. zlib and zstd both available. |
| Checksum negotiation (`--checksum-choice`) | Full | N/A | XXH3 / XXH128 / MD5 / MD4 paths all platform-independent; SIMD fast paths use AVX2 and SSE2 on Windows x86_64. |
| Bandwidth limits (`--bwlimit`) | Full | N/A | Userspace rate limiter; no platform dependency. |
| Filter rules (`--filter`, `--include`, `--exclude`, `.rsync-filter`) | Full | N/A | Cross-platform; path-component matching uses the platform's native separator. |
| `--delete` and variants | Full | N/A | Receiver-side deletion uses the sandboxed deletion path landed under SEC-1.q2. |
| `--inplace` | Full | Production | NTFS in-place writes honored; no temp-file rename. |
| `--partial` / `--partial-dir` | Full | Production | NTFS rename semantics honored on commit. |
| `--copy-links` (`-L`) | Full | Audited | Symlink target resolution is platform-aware; reparse-point classification follows the symlink chain. Reparse points classified per WPC-8/WPC-9; see `docs/audit/windows-reparse-point-classification.md`. |
| Daemon mode (`oc-rsync --daemon`) | Full | CI-validated | Listens via Winsock; `oc-rsyncd.conf` parsing, module configuration, `@RSYNCD:` handshake, and admission gating via `--max-connections` are all functional. Logging to Event Log is not wired; logs go to STDERR. |
| SSH transport (russh) | Full | Production | Compatible with OpenSSH for Windows, PuTTY (`plink`), and any RFC 4253 server. SSH compression detection from `~/.ssh/config` is incomplete cross-platform. |
| Batch mode (`--write-batch`, `--read-batch`, `--only-write-batch`) | Full | N/A | Batch file format is platform-independent; replay honors the platform's native metadata application path. |
| Windows service mode | Full | CI-validated | `oc-rsync --daemon` can be installed as a Windows service via the Win32 Service Control Manager (`crates/platform/src/windows_service.rs`). |
| Signal handling (Ctrl+C, Ctrl+Break, console close) | Full | Production | `SetConsoleCtrlHandler` maps CTRL_C / CTRL_CLOSE to shutdown and CTRL_BREAK to graceful exit. SIGHUP-equivalent config reload via named events is not wired. |
| EH ABI (`windows-gnu-eh`) | Full | Resolved | Zero-cost no-op on MSVC release binaries. Exists only for `x86_64-pc-windows-gnu` cross-compilation via `cargo-zigbuild`. See `docs/audit/win-gc-gnu-eh-abi-status.md` and `docs/audit/win-gd-gnu-eh-release-implications.md`. |
| Long paths (`\\?\` prefix) | Full | Audited | `\\?\` prefix applied by the path builder for paths exceeding `MAX_PATH`. Audited under WPC-5/WPC-6; see `docs/audit/windows-long-path-support.md`. |
| Reparse points (junctions, mount points) | Partial | Shipped, end-to-end wired | Reparse-point classification (WPC-8/WPC-9) is consumed by the transfer-side flist generator: every `FILE_ATTRIBUTE_REPARSE_POINT` entry routes through `metadata::windows::classify_path` and serialises as a SYMLINK-class `FileEntry` for Cygwin parity. Junctions and symbolic links carry their on-disk target; OneDrive / Cloud Files placeholders, WSL `AF_UNIX` sockets, and opaque vendor tags emit an empty target stub. Transparent OneDrive hydration is still out of scope. See `docs/audit/windows-reparse-point-classification.md` (WPC-8/WPC-9). |
| Case-insensitive FS conflict detection | Full | Audited | Receiver detects source-side name collisions (e.g., `a.txt` vs `A.txt`) before applying the second write. Regression test per WPC-11; see `docs/audit/windows-case-insensitive-conflict-detection.md`. |

## 3. Known limitations

Structural Windows-specific limitations:

- **No io_uring on Windows**: I/O dispatch uses IOCP exclusively.
  This is a structural decision, not a gap. See the memory notes
  on Windows io_uring scope and the IOCP wiring history.
- **SUID / SGID / sticky bits**: silently ignored on the receiver.
  No NTFS equivalent.
- **POSIX special files**: FIFOs, sockets, block and character
  devices cannot be created on NTFS. `--specials` and `-D` no-op,
  matching upstream rsync's Cygwin behaviour.
- **Alternate data streams (ADS)**: silently dropped unless `-X` is
  enabled, matching upstream rsync on Cygwin. One-shot warning when
  ADS detected without `-X` is implemented per WPC-3. See
  `docs/audit/windows-ads-handling.md`.
- **Reparse points**: non-symlink reparse points (junctions, mount
  points) are classified by tag per WPC-8/WPC-9. OneDrive and Cloud
  Files API placeholders are detected but not transparently hydrated.
  See `docs/audit/windows-reparse-point-classification.md`.
- **DACL inherited ACEs vs explicit ACEs**: inherited ACEs may be
  re-materialised as explicit ACEs on the receiver, losing the
  inheritance flag. Round-trip fidelity audited under WPC-10; see
  `docs/audit/windows-dacl-ace-inheritance.md`.
- **Windows attribute bits (read-only, hidden, system, archive)**:
  only the read-only bit is mapped today, derived from the POSIX
  write bit. Bidirectional mapping for hidden/system/archive bits
  audited under WPC-12; see
  `docs/audit/windows-perm-bits-posix-mapping.md`.

## 3a. Known gaps - not yet validated

The following areas have been implemented and audited at the code
level but lack real-world validation in specific environments:

- **IOCP hardware profiling**: the IOCP I/O dispatch path is
  CI-validated on GitHub Actions runners (virtualized storage) but
  has not been profiled on physical NTFS with real disk hardware.
  WPG-1..10 designed the profiling methodology; hardware validation
  is deferred until dedicated Windows hardware is available.
- **Active Directory environments**: DACL round-trip (`-A`) and
  owner/group (`-o`, `-g`) are tested with local Windows accounts
  only. Domain-joined machines with AD-backed SIDs, group policies,
  and cross-domain trust relationships have not been validated.
- **Low-memory / resource-constrained conditions**: no stress testing
  has been performed under memory pressure, high file-descriptor
  counts, or near-full NTFS volumes on Windows.
- **OneDrive / Cloud Files placeholder hydration**: reparse-point
  classification detects Cloud Files API placeholders (WPC-8/WPC-9)
  but does not trigger on-demand hydration. Transferring
  placeholder-only files may produce zero-length copies.
- **NFSv4-to-DACL conversion**: NFSv4 ACLs are stored via xattr but
  end-to-end conversion to DACL ACEs has not been audited.
- **ARM64 Windows**: not built or tested. If added, the IOCP path
  and `windows-gnu-eh` crate would need review (see WIN-G.c).
- **Windows Event Log**: daemon mode logs to STDERR only; Event Log
  integration is not wired.

## 4. Build and packaging requirements

The Windows builds shipped in the release pipeline use the MSVC
toolchain:

- **Toolchain triple**: `x86_64-pc-windows-msvc`.
- **Exception handling**: SEH (Structured Exception Handling), the
  native Windows mechanism. Panic unwinding across FFI boundaries
  is safe. Since Rust 1.71, panics reaching `extern "system"` frames
  abort rather than invoke undefined behavior.
- **Runtime dependencies**: `vcruntime140.dll` and `ucrtbase.dll`,
  both shipped with every supported Windows version since Windows 10.
  No additional DLLs are required.
- **Pre-built artifacts**: GitHub Releases ship a
  `x86_64-pc-windows-msvc` `.zip` per tag containing `oc-rsync.exe`
  and the daemon configuration template.
- **EH ABI status**: the `windows-gnu-eh` crate in the workspace is
  a no-op on MSVC targets - it compiles to a single zero-cost
  function that is optimized away entirely. It exists to support
  cross-compilation from Linux to `x86_64-pc-windows-gnu` via
  `cargo-zigbuild`. See `docs/audit/win-gd-gnu-eh-release-implications.md`
  for the full analysis.

Building from source on a Windows host is supported via the default
`rustup` MSVC toolchain. For the toolchain choice rationale, see
`docs/audits/windows-gnu-vs-msvc.md`.

## 5. What is NOT supported

Explicit non-goals to set operator expectations:

- **`x86_64-pc-windows-gnu` release builds**. The GNU toolchain is
  compile-checked in CI (`cargo check` only, no runtime tests) but
  is not shipped in release artifacts. Cross-compilation from Linux
  to `x86_64-pc-windows-gnu` works via `cargo-zigbuild` with the
  `windows-gnu-eh` shim crate, but the resulting binary is not
  CI-tested at runtime. See the GNU vs MSVC evaluation at
  `docs/audits/windows-gnu-vs-msvc.md`.
- **WSL bridging**. Running `oc-rsync` inside WSL is treated as a
  Linux build (the binary is `x86_64-unknown-linux-gnu`, not a
  Windows binary). WSL filesystem quirks (DrvFs case sensitivity,
  metadata translation, `\\wsl$` UNC paths) are outside the scope
  of this document.
- **Cygwin compatibility mode**. Upstream rsync ships a Cygwin
  port; oc-rsync does not. Users running on Cygwin should run the
  upstream Cygwin rsync, not the oc-rsync Windows binary.
- **NTFS object IDs, USN journal entries, and EFS-encrypted file
  re-encryption**. None of these are part of the rsync protocol or
  the upstream feature set, and oc-rsync does not preserve them.
- **Native Windows Event Log integration**. Daemon mode logs to
  STDERR; Event Log routing is not wired.

## 6. Release-notes scaffold - Windows support

Use this section as the basis for release-note entries when
shipping Windows-related changes. Copy the relevant bullet points
into `.github/RELEASE_TEMPLATE.md` under the appropriate heading.

### Windows subsystem maturity summary

- **IOCP I/O dispatch**: implemented and CI-validated on GitHub
  Actions runners. Hardware profiling on physical NTFS deferred
  (WPG-1..10 designed).
- **DACL/ACL round-trip (`-A`)**: implemented and tested with local
  Windows accounts. Active Directory environments not yet validated.
- **Alternate data streams (`-X`)**: implemented per WPC-3. One-shot
  warning emitted when ADS detected without `-X`.
- **Long paths (`\\?\`)**: implemented per WPC-5/WPC-6. Paths
  exceeding `MAX_PATH` are prefixed automatically.
- **Reparse points**: classified by tag per WPC-8/WPC-9 and wired
  end-to-end into transfer-side flist generation. Junctions and
  symbolic links serialise as SYMLINK-class entries carrying their
  on-disk target; OneDrive/Cloud Files placeholders, WSL `AF_UNIX`
  sockets, and opaque vendor tags emit empty-target SYMLINK stubs
  rather than slipping through as regular files. Transparent
  on-demand hydration is still out of scope.
- **EH ABI**: resolved per WIN-G.c/WIN-G.d. The `windows-gnu-eh`
  crate is a zero-cost no-op on MSVC release binaries.
- **Case-insensitive FS**: collision detection implemented per
  WPC-11. Regression test covers `a.txt` vs `A.txt` scenarios.
- **Permission bits**: POSIX-to-NTFS round-trip audited per WPC-12.
  Read-only flag mapped; SUID/SGID/sticky silently ignored.

### Known gaps for release notes

When authoring release notes, include these caveats in a "Known
Limitations" or "Windows Notes" section:

- IOCP path not profiled on physical NTFS hardware.
- DACL/ACL not validated in Active Directory or cross-domain
  trust environments.
- OneDrive/Cloud Files placeholders detected but not hydrated -
  placeholder-only transfers may produce zero-length files.
- NFSv4-to-DACL end-to-end conversion not audited.
- ARM64 Windows not built or tested.
- No Windows Event Log integration; daemon logs to STDERR.

## 7. Cross-references

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
- `docs/audit/windows-long-path-support.md` - long-path audit
  (WPC-5/WPC-6).
- `docs/audit/windows-reparse-point-classification.md` - reparse-point
  classification (WPC-8/WPC-9).
- `docs/audit/windows-dacl-ace-inheritance.md` - DACL inherited-ACE
  round-trip audit (WPC-10).
- `docs/audit/windows-case-insensitive-conflict-detection.md` -
  case-insensitive FS collision detection (WPC-11).
- `docs/audit/windows-perm-bits-posix-mapping.md` - POSIX permission
  bits round-trip audit (WPC-12).
- `docs/audit/win-gc-gnu-eh-abi-status.md` - EH ABI compatibility
  status (WIN-G.c).
- `docs/audit/win-gd-gnu-eh-release-implications.md` - EH release
  binary implications (WIN-G.d).
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
- Sibling completed: **WPC-1 (#2903)** through **WPC-13 (#2915)**.
- WIN-G series: **WIN-G.c** (EH ABI status), **WIN-G.d** (release
  implications), **WIN-G.g** (this maturity update).
- WPG series (IOCP profiling): **WPG-1..10** designed; hardware
  validation deferred.
- This document: **WPC-13 (#2915)**, updated by **WIN-G.g**.

Memory cross-links (internal): `[[project_windows_real_world_parity_unclear]]`,
`[[project_no_windows_io_uring]]`, `[[project_windows_parity_wip]]`,
`[[project_xattr_acl_cross_platform_parity_gap]]`,
`[[project_iocp_not_wired]]`.
