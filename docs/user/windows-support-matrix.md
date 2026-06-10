# oc-rsync Windows support matrix

Status as of 2026-06-10. Each row is a feature x support-level cell
verified against production code at the named source location. This
document supersedes the previous matrix, which marked several WPC
tasks "completed" based on design or audit docs rather than shipped
code.

Tracks parent #2869 (Windows real-world parity series). The WPC-V.1-5
reality-check audits surfaced status-vs-reality misalignments for
WPC-3, WPC-4, WPC-8, and WPC-9 cells: the design and audit documents
exist, but the production code path was not yet wired at the time of
the previous matrix revision. The cells below cite a verified source
file for every "Shipped" claim; cells whose implementation is still in
flight are marked "Design only" or "Audit only" with a tracking link.

## 1. Filesystem features

| Feature | Status | Source |
|---|---|---|
| Basic file transfer (`-r`, `-a`) | Shipped | `crates/fast_io/src/iocp/file_reader.rs`, `crates/fast_io/src/iocp/file_writer.rs` |
| Hard links (`-H`) | Shipped | `crates/transfer/src/receiver/directory/links.rs:367` (`std::fs::hard_link`) |
| Symbolic links (`-l`) | Shipped | `std::os::windows::fs::symlink_file` / `symlink_dir` (Windows test coverage at `crates/engine/src/local_copy/tests/executor_file_operations.rs:128`) |
| Sparse files (`-S`) | Shipped (cross-platform zero-run detection) | `crates/fast_io/src/policy.rs:144` (no `FSCTL_SET_ZERO_DATA` call site found in tree) |
| Timestamps (`-t`) | Shipped | `filetime` crate; NTFS 100 ns resolution |
| NTFS Alternate Data Streams (ADS) via `--xattrs` | Design + backend shipped, preflight rejects flag | `crates/metadata/src/xattr_windows.rs:57` (FindFirstStreamW/FindNextStreamW backend exists); `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:176-180` still rejects `--xattrs` on Windows. Follow-up: PR #5564. |
| Long paths (>260 chars) via `\\?\` prefix | Audit only | `crates/fast_io/src/iocp/file_reader.rs:315` (`to_wide_path` does NOT prepend `\\?\`). Long-path test at `crates/fast_io/tests/ntfs_edge_cases.rs:128` exercises the kernel acceptance but not a helper. Follow-up: PR #5575. |
| Reparse-point classification (junctions, mount points, OneDrive) | Design only | No `FSCTL_GET_REPARSE_POINT` or `classify_reparse_point` call site exists under `crates/metadata/src/`. Audit doc: `docs/audit/windows-reparse-point-classification.md`. Follow-up: PR #5579. |
| Junctions | Followed as directory symlinks (default kernel behaviour) | No classifier wired; transfer-time handling unverified. |
| Mount points | Followed as directory symlinks (default kernel behaviour) | No classifier wired. |
| OneDrive / Cloud Files placeholders | Not classified | Placeholder-only files may transfer as zero-length until #5579 lands. |
| Case-insensitive FS conflict detection | Audit only | No `case_insensitive` collision-detection call site found under `crates/transfer/src/` or `crates/engine/src/`. Audit doc: `docs/audit/windows-case-insensitive-conflict-detection.md`. |

## 2. Permissions and ACLs

| Feature | Status | Source |
|---|---|---|
| POSIX permission bits (`-p`) | Shipped (POSIX bits on the wire; read-only flag mapped on NTFS) | Audit: `docs/audit/windows-perm-bits-posix-mapping.md` (WPC-12) |
| `--chmod` modifiers | Shipped | Applied to wire-format POSIX mode bits; DACL untouched unless `-A` set |
| Owner / group (`-o`, `-g`) | Shipped (SID round-trip via `LookupAccountNameW` when `-A` set) | `crates/metadata/src/acl_windows/sync.rs` |
| `--numeric-ids` | Shipped (pass-through) | No NSS lookup attempted on Windows |
| `--usermap` / `--groupmap` / `--chown` | Not supported | Returns `MappingParseError` on Windows |
| `--fake-super` | Not supported | Requires `user.rsync.%stat` xattr backend |
| `--copy-as=USER[:GROUP]` | Not supported | Guarded no-op |
| NTFS DACL via `--acls` | Shipped | `crates/metadata/src/acl_windows/dacl.rs` |
| DACL <-> POSIX mode-bits | Shipped | `crates/metadata/src/acl_windows/posix_map.rs` |
| SDDL round-trip | Shipped | `crates/metadata/src/acl_windows/sddl.rs` |
| Owner / group SID round-trip | Shipped | `crates/metadata/src/acl_windows/sync.rs` |
| DACL inherited-ACE round-trip | Audited (round-trip fidelity caveats) | `docs/audit/windows-dacl-ace-inheritance.md` (WPC-10) |
| NFSv4 ACLs | Stored via xattr backend; end-to-end NFSv4-to-DACL conversion not audited | `crates/metadata/src/nfsv4_acl.rs` |

## 3. I/O backends

| Feature | Status | Source |
|---|---|---|
| IOCP file I/O | Shipped | `crates/fast_io/src/iocp/` (file_reader, file_writer, completion_port, overlapped) |
| `TransmitFile` (sendfile equivalent) | Shipped | `crates/fast_io/src/iocp/transmit_file.rs` |
| `FILE_FLAG_DELETE_ON_CLOSE` (O_TMPFILE equivalent) | Shipped | `crates/fast_io/src/win_tmpfile/` |
| ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (reflink) | Shipped | `crates/fast_io/src/platform_copy/dispatch.rs:283,499` |
| `CopyFileExW` data-path dispatch | Shipped | `crates/fast_io/src/copy_file_ex.rs` |
| io_uring | Not applicable (Linux-only) | Structural decision; IOCP is the permanent Windows replacement |
| `splice` | Not implemented | No Windows pipe-zero-copy equivalent. Tracked: WIN-P series |
| `vmsplice` | Not implemented | Tracked: WIN-P series |
| Landlock | Not applicable | Use AppArmor/SELinux on Linux, restricted tokens on Windows (tracked: WIN-P.4) |
| SEND_ZC (zero-copy SEND) | Not applicable (Linux-only) | Windows equivalent design at `docs/design/wpg-8-send-zc-windows-equivalent.md` |
| Registered buffers | Not applicable (Linux-only) | Windows equivalent design at `docs/design/wpg-9-registered-buffer-windows-equivalent.md` |

## 4. CLI surface

| Feature | Status | Source / notes |
|---|---|---|
| `--xattrs` on Windows | Backend shipped, CLI preflight blocks | `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:176-180`. Follow-up PR #5564 widens the preflight cfg gate |
| `--acls` on Windows | Shipped | Per WAS-1..WAS-8 series; `crates/metadata/src/acl_windows/` |
| `--crtimes` | Not supported (`crtimes` is Cygwin-only upstream) | Daemon version banner advertises "no crtimes" |
| `--specials`, `-D` | No-op | Matches upstream rsync on Cygwin (no `mknod` on NTFS) |
| Compression (`-z`, `--zc=...`) | Shipped (platform-independent) | zlib and zstd both available |
| Checksum negotiation (`--checksum-choice`) | Shipped | XXH3 / XXH128 / MD5 / MD4 with AVX2 / SSE2 fast paths on Windows x86_64 |
| Bandwidth limits (`--bwlimit`) | Shipped | Userspace rate limiter |
| Filter rules (`--filter`, `.rsync-filter`) | Shipped | Path-component matching uses platform native separator |
| `--delete` and variants | Shipped | Sandboxed deletion path (SEC-1.q2) |
| `--inplace` | Shipped | NTFS in-place writes honored |
| `--partial` / `--partial-dir` | Shipped | NTFS rename semantics honored on commit |
| `--copy-links` (`-L`) | Shipped (symlink chain followed); reparse-point chain depends on classifier landing | See `crates/metadata/src/xattr_windows.rs:190` comment on reparse-point traversal |
| Batch mode (`--write-batch`, `--read-batch`, `--only-write-batch`) | Shipped (platform-independent) | |
| Daemon mode (`oc-rsync --daemon`) | Shipped | Winsock listener; `@RSYNCD:` handshake; `--max-connections` admission cap |
| Windows service mode | Shipped | `crates/platform/src/windows_service.rs` |
| SSH transport (russh) | Shipped | Compatible with OpenSSH for Windows, PuTTY (`plink`), any RFC 4253 server |
| Signal handling (Ctrl+C, Ctrl+Break, console close) | Shipped | `crates/platform/src/signal.rs` (SetConsoleCtrlHandler) |

## 5. Gaps explicitly tracked

- Windows nightly CI cell coverage: WCI series (#3694-3705).
- Windows-only stub call sites: WIN-S series (#3241+).
- Windows-only platform features: WIN-P series (#3681+).
- Real-hardware validation (vs CI VM): WIN-G series (#3042+).
- IOCP hardware profiling on physical NTFS: WPG-1..10 designed; hardware deferred.
- Active Directory DACL / owner-group validation: tracked under WPC-10 follow-up.
- OneDrive placeholder hydration: blocked on PR #5579 landing the classifier.
- NFSv4-to-DACL conversion audit: tracked under XAP series.
- ARM64 Windows build and test: not in CI matrix.
- Windows Event Log integration: not wired; daemon logs to STDERR.

## 6. Build and packaging requirements

> **Packaging note (platform-feature gates).** Every CLI-facing platform
> feature (`--xattrs`, `--acls`) is gated at two layers: a Cargo feature in
> `crates/core/Cargo.toml` that propagates to the backend crate, and a
> preflight `#[cfg]` gate at the CLI boundary. Both layers must list every
> platform that ships a backend, or the flag is silently rejected even with
> the feature compiled in. The convention is documented in
> `docs/contributing/ONBOARDING.md` ("Platform-feature gates in preflight")
> and locked in by `crates/cli/tests/feature_propagation.rs`. WPC-3 (PR
> #5564) was exactly this defect class on Windows `--xattrs`.

- **Toolchain triple**: `x86_64-pc-windows-msvc`.
- **Exception handling**: SEH (Structured Exception Handling). Panics
  reaching `extern "system"` frames abort rather than invoke undefined
  behavior (Rust 1.71+).
- **Runtime dependencies**: `vcruntime140.dll` and `ucrtbase.dll`, both
  shipped with every supported Windows release since Windows 10.
- **Pre-built artifacts**: GitHub Releases ship an
  `x86_64-pc-windows-msvc` `.zip` per tag containing `oc-rsync.exe` and
  the daemon configuration template.
- **EH ABI**: the `windows-gnu-eh` crate is a zero-cost no-op on MSVC
  release binaries. It supports cross-compilation from Linux to
  `x86_64-pc-windows-gnu` via `cargo-zigbuild`. See
  `docs/audit/win-gd-gnu-eh-release-implications.md`.

Building from source on a Windows host is supported via the default
`rustup` MSVC toolchain. Toolchain rationale: `docs/audits/windows-gnu-vs-msvc.md`.

## 7. Explicit non-goals

- `x86_64-pc-windows-gnu` release binaries (compile-checked only).
- WSL bridging - WSL builds are Linux binaries; quirks of `DrvFs` and
  `\\wsl$` are out of scope.
- Cygwin compatibility mode - users on Cygwin should run upstream
  Cygwin rsync.
- NTFS object IDs, USN journal entries, EFS-encrypted re-encryption.
- Native Windows Event Log integration.

## 8. Audit methodology

Status table verified 2026-06-10 by per-row source-file grep plus
integration-test inventory. Each "Shipped" cell cites a file:line
that contains the production call site or its module entry point.

Anti-pattern lesson surfaced by WPC-V.1-5: WPC-3, WPC-4, WPC-8, and
WPC-9 were previously marked completed based on the existence of
design and audit documents. The cells above are verified against
PRODUCTION CODE at the named file:line; cells whose implementation is
still in flight are marked "Design only" or "Audit only" with a
follow-up PR or tracker. Future updates must apply the same standard:
no row may be promoted past "Audit only" without a verified call site.

## 9. Cross-references

- `docs/audit/windows-ads-handling.md` - ADS handling audit (WPC-1).
- `docs/design/windows-ads-strategy.md` - ADS strategy (WPC-2).
- `docs/audit/windows-long-path-support.md` - long-path audit (WPC-5/6).
- `docs/audit/windows-reparse-point-classification.md` - reparse-point
  classifier audit (WPC-8/9).
- `docs/audit/windows-dacl-ace-inheritance.md` - inherited-ACE audit (WPC-10).
- `docs/audit/windows-case-insensitive-conflict-detection.md` - case-collision audit (WPC-11).
- `docs/audit/windows-perm-bits-posix-mapping.md` - permission-bits audit (WPC-12).
- `docs/audit/windows-copyfileex-platform-copy.md` - data-path dispatch chain.
- `docs/audit/win-gc-gnu-eh-abi-status.md` - EH ABI status (WIN-G.c).
- `docs/audit/win-gd-gnu-eh-release-implications.md` - EH release implications (WIN-G.d).
- `docs/audits/windows-gnu-vs-msvc.md` - toolchain rationale.
- `docs/platform-support.md` - full Linux/macOS/Windows feature matrix.
- `docs/windows_platform_parity.md` - per-`cfg(unix)`-block audit.
- `docs/user/daemon-concurrency-limits.md` - daemon admission sizing.
- `docs/user/windows-feature-matrix.md` - companion narrative document.

Tracking tasks:

- Parent: **#2869** (Windows real-world parity series).
- WPC-V.1-5 audits: this revision.
- WPC-3 / WPC-4 follow-up: PR #5564 (`--xattrs` preflight gate).
- WPC-5 / WPC-6 follow-up: PR #5575 (`to_extended_path` helper).
- WPC-8 / WPC-9 follow-up: PR #5579 (reparse-point classifier).
- WIN-G series: EH ABI status (.c), release implications (.d), this
  matrix update (.g).
- WPG series (IOCP profiling): WPG-1..10 designed; hardware deferred.

Memory cross-links (internal): `[[project_windows_real_world_parity_unclear]]`,
`[[project_no_windows_io_uring]]`, `[[project_windows_parity_wip]]`,
`[[project_xattr_acl_cross_platform_parity_gap]]`,
`[[project_iocp_not_wired]]`.
