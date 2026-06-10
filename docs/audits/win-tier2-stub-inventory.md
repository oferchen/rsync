# Windows Tier 2 stub inventory and Cygwin parity

Status as of 2026-06-10. Bundled deliverable for WIN-TIER2.1 (Linux-only
stub inventory on the Windows transfer path), WIN-TIER2.2 (upstream Cygwin
rsync parity), WIN-TIER2.3 (known-failures closure mapping), and
WIN-TIER2.5 (Tier 2 disclosure). WIN-TIER2.4 (performance bench cells) is
deferred until Windows CI execution capacity is available.

Cross-references: the Windows support matrix at
`docs/user/windows-support-matrix.md`, the WIN-S series priority matrix
at `docs/design/win-s8-windows-stub-priority-matrix.md`, the
splice/vmsplice Windows-equivalent evaluation at
`docs/design/windows-splice-vmsplice-equivalents.md`, and the
sendfile/`TransmitFile` audit at
`docs/design/win-s2-sendfile-transmitfile-audit.md`.

## 1. Linux-only stub inventory (WIN-TIER2.1)

Each row names a `fast_io` entry point that has a Linux implementation
and a non-Linux stub, the production call site (if any) that reaches the
stub on Windows, and the stub's observable failure mode. Call sites were
located by `grep` for the entry point across `crates/` excluding
`crates/fast_io/` to identify external consumers.

| Entry point | Linux impl | Non-Linux stub | Stub failure mode | Production Windows call site |
|---|---|---|---|---|
| `splice::try_splice_to_file` | `crates/fast_io/src/splice/syscalls.rs:40` | `crates/fast_io/src/splice/syscalls.rs:281` | `Err(ErrorKind::Unsupported)` | None. Re-exported via `crates/fast_io/src/lib.rs:283`, no external caller. |
| `splice::try_vmsplice_to_file` | `crates/fast_io/src/splice/syscalls.rs:211` | `crates/fast_io/src/splice/syscalls.rs:272` | `Err(ErrorKind::Unsupported)` | None. Re-exported, no external caller. |
| `splice::recv_fd_to_file` | `crates/fast_io/src/splice/syscalls.rs:331` | `crates/fast_io/src/splice/syscalls.rs:357` (`#[cfg(not(unix))]`) | `Err(ErrorKind::Unsupported)` | None. The Windows receive path uses `Writer::Iocp` in `crates/transfer/src/disk_commit/writer.rs:151`. |
| `splice::SplicePipe::{new,with_capacity,splice_to_file,vmsplice_to_file}` | `crates/fast_io/src/splice/mod.rs:131` | `crates/fast_io/src/splice/mod.rs:336` | Constructor and methods return `Err(ErrorKind::Unsupported)`; `capacity()` returns `0`. | None. Constructor exit is the safety boundary. |
| `vmsplice_writer::VmspliceFileWriter` | `crates/fast_io/src/vmsplice_writer.rs:82` | Compiled out: `Writer::Vmsplice` variant is `#[cfg(all(target_os = "linux", feature = "vmsplice"))]` | Variant does not exist on Windows. | `crates/transfer/src/disk_commit/process.rs:452` only on Linux. On Windows the dispatch falls through to `Writer::Iocp` or `Writer::Buffered`. |
| `io_uring` module surface (`shared_ring`, `per_thread_ring`, `file_reader`, `file_writer`, `disk_batch`, `linkat`, `statx`, `renameat2`, `send_zc`, `registered_buffers`, `buffer_ring`, `session_pool`, `cancel`, `linked_chain`) | `crates/fast_io/src/io_uring/` | `crates/fast_io/src/io_uring_stub/` | `is_io_uring_available()` returns `false`; constructors return `Err(ErrorKind::Unsupported)`; opcode helpers return `Err(ErrorKind::Unsupported)`. | None on Windows. `Writer::IoUring` variant is `#[cfg(all(target_os = "linux", feature = "io_uring"))]`. Windows dispatches `Writer::Iocp` instead (`crates/transfer/src/disk_commit/writer.rs:147`). |
| `sendfile::send_file_to_fd` | `crates/fast_io/src/sendfile/mod.rs:161` (Linux), `:177` (macOS) | `crates/fast_io/src/sendfile/mod.rs:194` (`#[cfg(not(unix))]`) | Silent data-discard: `send_file_to_writer(source, &mut io::sink(), length)` returns `Ok(length)`. WIN-S.8 P1. | None. Function is re-exported at `crates/fast_io/src/lib.rs:278` but no external consumer wires it. Windows replacement `TransmitFile` lives at `crates/fast_io/src/iocp/transmit_file.rs` and is also not yet wired into the sender. |
| `sendfile::send_file_to_fd_with_policy` | `crates/fast_io/src/sendfile/mod.rs:206` | `crates/fast_io/src/sendfile/mod.rs:236` | Same `io::sink()` discard as above. | None. |

Mmap, Landlock, `O_TMPFILE`, and `copy_file_range` stubs are inventoried
in the WIN-S.8 priority matrix and are excluded here because they are not
Linux-only in the same sense - Windows has equivalents already wired
(`platform_copy::CopyFileExW`, ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`,
`FILE_FLAG_DELETE_ON_CLOSE`) or the Linux primitive itself is not the
right reference point.

### Cross-reference to WIN-S series

The WIN-S series tracks the per-stub replacement workstream:

- **WIN-S.1** (#3242) - stub inventory parent; this audit consolidates
  it for the Linux-only subset.
- **WIN-S.2** (#3243) - sendfile -> `TransmitFile`. Backend shipped at
  `crates/fast_io/src/iocp/transmit_file.rs`; sender wire-up tracked
  separately (no production caller of `send_file_to_fd` yet).
- **WIN-S.3** (#3244) - splice equivalent. Decision: permanent stub. See
  `docs/design/windows-splice-vmsplice-equivalents.md`.
- **WIN-S.4** (#3245) - vmsplice equivalent. Decision: permanent stub
  (variant gated out on Windows).
- **WIN-S.5** (#3246) - mmap_reader. `memmap2` upgrade tracked under
  WIN-S.8.2.
- **WIN-S.6** (#3247) - `O_TMPFILE`. `FILE_FLAG_DELETE_ON_CLOSE` already
  shipped at `crates/fast_io/src/win_tmpfile/`.
- **WIN-S.7** (#3248) - Landlock alternatives on Windows.
- **WIN-S.8** (#3249) - priority matrix
  (`docs/design/win-s8-windows-stub-priority-matrix.md`).
- **WIN-S.9..S.11** (#3250-#3252) - send_zc, registered buffers,
  follow-up audits.

## 2. Upstream Cygwin rsync parity (WIN-TIER2.2)

Upstream rsync's Windows story is Cygwin-only. The Cygwin port relies on
the Cygwin runtime to translate POSIX syscalls into Win32 calls, which
shapes what oc-rsync should match. For each Linux-only primitive above,
this table documents Cygwin's behavior and oc-rsync's match status.

| Primitive | Upstream Cygwin behavior | oc-rsync Windows behavior | Match status |
|---|---|---|---|
| `splice(2)` | Cygwin exposes `splice` as a glibc-like wrapper. The Cygwin implementation does not use kernel-side zero-copy because Windows has no equivalent; it falls back to `read()` + `write()` internally. Upstream rsync code that calls `splice` therefore runs as a buffered copy with zero observable wire effect. | `try_splice_to_file` returns `Err(ErrorKind::Unsupported)`. No production caller wires it on Windows. The receive path uses `Writer::Iocp` (IOCP-batched `WriteFile`) which is strictly faster than Cygwin's userspace `read`/`write`. | **Match (oc-rsync ahead).** Upstream falls back to userspace copy; oc-rsync uses IOCP. |
| `vmsplice(2)` | Cygwin does not expose `vmsplice`. Code that calls it returns `ENOSYS`. Upstream rsync does not call `vmsplice` on the receive path; the `vmsplice` Cargo feature exists only in oc-rsync as a Linux-only optimization. | `VmspliceFileWriter` is compiled out on Windows (`#[cfg(all(target_os = "linux", feature = "vmsplice"))]`). The `Writer::Vmsplice` enum variant does not exist on Windows. | **Match.** Both upstream Cygwin and oc-rsync skip vmsplice on Windows. |
| `io_uring` | Cygwin does not implement io_uring (no equivalent kernel interface). Upstream rsync has no io_uring code path at all. | `io_uring_stub::is_io_uring_available()` returns `false`. `Writer::IoUring` variant is gated out on Windows. IOCP is the production backend. | **Match (oc-rsync ahead).** Upstream has no async I/O acceleration on Windows; oc-rsync uses IOCP. |
| `sendfile(2)` | Cygwin exposes `sendfile` and delegates internally to `TransmitFile`. Upstream rsync's sender uses buffered `read` + `write` rather than `sendfile`, so this is moot on the sender. | `send_file_to_fd` on Windows is a stub that discards data via `io::sink()` (WIN-S.8 P1 latent defect). The Windows-native `TransmitFile` wrapper exists at `crates/fast_io/src/iocp/transmit_file.rs` but is not wired into the sender. | **Match in practice** (neither implementation reaches the stub today). **Latent defect**: if a future change wires `send_file_to_fd`, oc-rsync silently discards data while upstream Cygwin would succeed via `TransmitFile`. WIN-S.8.1 fixes by returning `Err(ErrorKind::Unsupported)`. |
| Pipe creation for `splice` chain (`SplicePipe`) | Cygwin provides POSIX pipes through `pipe(2)`. Upstream does not chain splice through pipes on Windows because the underlying `splice` wrapper degrades to read/write. | `SplicePipe::new` returns `Err(ErrorKind::Unsupported)`. | **Match.** Neither side establishes the pipe pair on Windows. |
| `recv_fd_to_file` (high-level wrapper) | No upstream analog; this is an oc-rsync abstraction over `splice` + `read/write` fallback. Upstream's read loop is `read(socket)` -> `write(file)` regardless of platform. | `recv_fd_to_file` returns `Err(ErrorKind::Unsupported)` on `#[cfg(not(unix))]`. The Windows receive path bypasses this entirely via `Writer::Iocp`. | **Match.** Neither side uses a unified fd-to-fd primitive on Windows. |

### Deliberate divergences from Cygwin (advantages)

- **IOCP file writes.** Upstream Cygwin rsync writes files through
  buffered `write(2)` (translated to `WriteFile`). oc-rsync writes
  through `IocpDiskBatch`, which submits overlapped writes with a single
  completion port wait per batch. This is faster than the Cygwin path
  and has no upstream analog.
- **IOCP socket I/O.** Daemon and SSH transports use `WSARecv` / `WSASend`
  with overlapped completion. Upstream Cygwin uses blocking `recv` /
  `send`.
- **`CopyFileExW` data-path dispatch.** Local copies route through
  `platform_copy::DefaultPlatformCopy`, which preferentially uses
  `CopyFileExW` with `COPY_FILE_NO_BUFFERING` for files > 4 MB. Upstream
  Cygwin uses the standard read/write loop.
- **ReFS reflink.** When the destination volume is ReFS, oc-rsync issues
  `FSCTL_DUPLICATE_EXTENTS_TO_FILE` for zero-copy clones. Upstream Cygwin
  has no reflink support on Windows.

These are documented as deliberate Tier 2+ advantages, not parity gaps.

## 3. Known-failures closure mapping (WIN-TIER2.3)

`tools/ci/known_failures.conf` was inspected for Windows-specific
entries. **The configuration contains no Windows-specific known
failures.** All current entries are either:

- Upstream bugs (`standalone:upstream-compressed-batch-self-roundtrip`).
- Protocol-version downgrades (`up:acls`, `up:xattrs`,
  `up:compress-zstd`, `up:compress-lz4`, `up:merge-filter`).

None of these depend on Linux-only stubs. The classification framework
the task asked for - (a) closable with effort, (b) permanent gap on
Linux-only feature, (c) different root cause - applies to potential
future entries rather than current ones. For the WIN-P series planning
horizon, candidate failure types and their classifications are:

| Candidate failure category | Classification | Rationale |
|---|---|---|
| ADS-bearing files via `--xattrs` on Windows | (c) different root cause - CLI preflight blocks `--xattrs` at `crates/cli/src/frontend/execution/drive/workflow/preflight.rs:176`; backend exists at `crates/metadata/src/xattr_windows.rs`. Tracked: PR #5564. | Not a stub issue; preflight gate. |
| Long-path (>260 char) corner cases | (c) different root cause - missing `\\?\` prefix helper. Tracked: PR #5575 (`to_extended_path`). | Not a stub issue; helper missing. |
| OneDrive placeholder hydration | (c) different root cause - reparse-point classifier not wired. Tracked: PR #5579. | Not a stub issue; classifier missing. |
| Case-insensitive NTFS collision detection | (c) different root cause - no collision-detection layer. | Not a stub issue; layer missing. |
| Symlink transfer on Windows without admin/Developer Mode | (b) permanent gap - Windows requires elevation for symlink creation. | Not a stub issue; OS-policy. |
| POSIX device nodes / FIFOs on NTFS | (b) permanent gap - NTFS does not support `mknod`. Matches upstream Cygwin behavior. | Not a stub issue; FS limitation. |
| Sender zero-copy (`sendfile`) on Windows | (a) closable - `TransmitFile` backend exists at `crates/fast_io/src/iocp/transmit_file.rs`; wire-up tracked under WIN-S.2. WIN-S.8.1 first hardens the stub to `Err(Unsupported)`. | Wirable with effort. |
| Receiver `splice` zero-copy on Windows | (b) permanent gap - no Win32 primitive matches splice pipe-mediated semantics. See `docs/design/windows-splice-vmsplice-equivalents.md`. IOCP is already faster than the Cygwin fallback. | Permanent stub. |
| Receiver `vmsplice` on Windows | (b) permanent gap - same as splice. | Permanent stub. |

Priority list for the WIN-P series (#3681+) to attack:

1. **High value, closable.** Sender `TransmitFile` wire-up (after
   WIN-S.8.1 stub fix lands) - closes the only latent data-discard risk.
2. **Medium value, closable.** `MmapReader` upgrade via `memmap2` to
   eliminate the doubled memory cost on checksums for large files
   (WIN-S.8.2).
3. **Medium value, closable.** `FILE_FLAG_DELETE_ON_CLOSE` anonymous
   temp file wiring to replace the named-tempfile crash-debris pattern
   (WIN-S.8.3).
4. **Low value, design-only.** Landlock alternative selection between
   restricted tokens and AppContainer (WIN-S.8.4).
5. **No action.** splice/vmsplice/io_uring stubs are correct and
   permanent.

## 4. Tier 2 maintenance plan

The decision to mark Windows as Tier 2 (not Tier 1) is deliberate and
based on three structural constraints:

1. **No CI runners under contributor control execute the full nextest
   workspace on Windows.** Required CI cells for Windows test only the
   `core`, `engine`, and `cli` crates per the macOS/Windows scoping note.
2. **No physical hardware coverage.** WIN-G series (#3042+) tracks the
   gap; real-hardware NTFS testing is currently inferred from CI VM
   results.
3. **Permanent Linux-only primitives.** `splice`, `vmsplice`, and
   `io_uring` have no Win32 equivalents. IOCP is the structural
   replacement and is faster than the upstream Cygwin fallback, but the
   public-facing claim of "full parity" would be misleading.

### What would advance Windows to Tier 1?

| Requirement | Current status | Owner |
|---|---|---|
| Full nextest workspace executes on Windows CI cells | Only `core`, `engine`, `cli` (and on some matrices `metadata`, `apple-fs`) | WCI series (#3694-3705) |
| Physical NTFS hardware coverage (vs CI VM) | Not in matrix | WIN-G series (#3042+) |
| Sender `TransmitFile` wired (closes WIN-S.8.1 latent risk) | Backend shipped, wire-up pending | WIN-S.2 follow-up |
| ARM64 Windows build | Not in CI matrix | tracked separately |
| `--xattrs` preflight gate widened | CLI rejects on Windows; backend exists | PR #5564 |
| Long-path `\\?\` helper | Missing | PR #5575 |
| Reparse-point classifier | Missing | PR #5579 |
| Case-insensitive collision detection | Missing | tracked under WPC-11 |
| Active Directory DACL fidelity | Audited, follow-up open | WPC-10 |
| OneDrive placeholder hydration | Blocked on classifier | PR #5579 |

Each of the above is independently shippable. Tier 1 promotion is
gated on the CI and hardware coverage rows first - the missing features
are then individually mergeable as PRs.

## 5. Decision log

- **Splice and vmsplice stubs are permanent.** See
  `docs/design/windows-splice-vmsplice-equivalents.md`. The receive path
  uses IOCP, which is strictly faster than the splice/vmsplice path
  would be on Windows even if the primitives existed.
- **io_uring stub is permanent.** IOCP is the production Windows
  backend. The 73 KB stub mirrors the cross-platform API surface so
  call sites compile uniformly; see `project_io_uring_stub_size.md` for
  the size note.
- **`send_file_to_fd` `io::sink()` discard is a latent defect (P1).**
  WIN-S.8.1 hardens the stub to return `Err(ErrorKind::Unsupported)`
  before any production wire-up. `TransmitFile` will be the Windows
  backend when the sender zero-copy path is wired.
- **Cygwin compatibility is an explicit non-goal.** Users on Cygwin
  should run upstream Cygwin rsync. See
  `docs/user/windows-support-matrix.md` section 7.

## 6. Tracking issues

- Parent: **#2869** (Windows real-world parity series).
- WIN-TIER2 bundle (this doc): WIN-TIER2.1 (#4014), WIN-TIER2.2 (#4015),
  WIN-TIER2.3 (#4016), WIN-TIER2.5 (#4018).
- WIN-TIER2.4 (Windows perf bench cells, #4017): deferred until Windows
  CI execution capacity is available.
- WIN-S series (#3242-#3252): per-stub replacement workstream.
- WIN-P series (#3681+): Windows-only platform features.
- WCI series (#3694-3705): Windows nightly CI cell coverage.
- WIN-G series (#3042+): real-hardware validation.
