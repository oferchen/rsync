# WIN-P.1: Linux-only `fast_io` stubs and Windows feed-forward priorities

Status as of 2026-06-11. Refresh of the Linux-only cfg-gated stub
surface in `crates/fast_io/src/`, focused on what WIN-P.2 (splice),
WIN-P.3 (vmsplice), WIN-P.4 (Landlock), and WIN-P.5 (SQPOLL) need to
know before scoping per-stub Windows-equivalent investigations.

Three earlier audits already cover much of this surface:

- `docs/audits/win-tier2-stub-inventory.md` (WIN-TIER2.1, 2026-06-10)
  inventoried the entry points reachable on the Windows transfer path
  and mapped each to its Cygwin behaviour and oc-rsync match status.
- `docs/design/win-s8-windows-stub-priority-matrix.md` (WIN-S.8)
  ranked stubs by correctness, throughput, frequency, and Windows
  equivalent.
- `docs/design/win-s2-sendfile-transmitfile-audit.md`,
  `docs/design/windows-splice-vmsplice-equivalents.md`,
  `docs/design/windows-landlock-equivalent.md`,
  `docs/design/wpg-8-send-zc-windows-equivalent.md`,
  `docs/design/wpg-9-registered-buffer-windows-equivalent.md` are the
  per-stub deep-dives that WIN-P.2..5 build on.

This audit adds three pieces those earlier passes did not produce:

1. A single full inventory table keyed by Linux arm + non-Linux arm
   that reflects current master (post WIN-S.LAND.1.d, post SQP-LAND.4,
   post WPC-8'.10, post SEC-MK).
2. A caller-graceful-degradation classification.
3. A WIN-P-specific priority list that takes WIN-S.8 and WIN-S.LAND.1
   as input.

## 1. Inventory table

Stubs are grouped by family. Every row is an entry point with a Linux
implementation and a non-Linux arm. Line numbers reference current
master.

### 1.1 Filesystem zero-copy and metadata fast paths

| Entry point | Linux arm | Non-Linux arm | Stub return | Notes |
|---|---|---|---|---|
| `splice::syscalls::try_splice_to_file` | `splice/syscalls.rs:40` (`splice(2)` via libc) | `splice/syscalls.rs:281` (`#[cfg(not(target_os = "linux"))]`) | `Err(ErrorKind::Unsupported)` | Already inventoried WIN-TIER2.1 row 1. |
| `splice::syscalls::try_vmsplice_to_file` | `splice/syscalls.rs:211` (`vmsplice(2)`) | `splice/syscalls.rs:272` | `Err(ErrorKind::Unsupported)` | WIN-TIER2.1 row 2. |
| `splice::syscalls::recv_fd_to_file` | `splice/syscalls.rs:331` | `splice/syscalls.rs:357` (`#[cfg(not(unix))]`) | `Err(ErrorKind::Unsupported)` | WIN-TIER2.1 row 3. The `cfg(not(unix))` rather than `cfg(not(target_os = "linux"))` means macOS/BSD also miss the helper but they fall through to standard read/write paths. |
| `splice::mod::SplicePipe::{new, with_capacity, splice_to_file, vmsplice_to_file, capacity}` | `splice/mod.rs:131-310` | `splice/mod.rs:336` (`#[cfg(not(target_os = "linux"))]`) | Constructors `Err(Unsupported)`; `capacity()` returns `0`. | WIN-TIER2.1 row 4. |
| `vmsplice_writer::VmspliceFileWriter` | `vmsplice_writer.rs:82` | Compiled out: `Writer::Vmsplice` variant is `#[cfg(all(target_os = "linux", feature = "vmsplice"))]`. | Variant absent on Windows; dispatcher falls through. | WIN-TIER2.1 row 5. |
| `sendfile::send_file_to_fd` | `sendfile/mod.rs:161` (Linux), `:177` (macOS) | `sendfile/mod.rs:194` (`#[cfg(not(unix))]`) | Calls `copy_via_fd_write(source, dest_fd, length)` since WIN-S.LAND.1.d (PR #5295). The earlier `io::sink()` silent-discard was removed; the non-unix arm now reuses the buffered loop. | WIN-TIER2.1 row 7; the noted P1 latent defect is closed. |
| `sendfile::send_file_to_fd_with_policy` | `sendfile/mod.rs:206-231` | `sendfile/mod.rs:236` | Buffered fallback via `copy_via_fd_write`. | WIN-TIER2.1 row 8; same correction. |
| `copy_file_range::try_copy_file_range` | `copy_file_range.rs:35-285` (`copy_file_range(2)` via `libc::syscall(SYS_copy_file_range)`) | `copy_file_range.rs:329` | `Err(ErrorKind::Unsupported)` | Caller in `copy_file_range::copy_file_contents` always falls back to a 256 KB buffered read/write loop. |
| `copy_basis_range::imp::copy_basis_range` | `copy_basis_range.rs:102-395` (Linux `copy_file_range`), `:325-395` (Windows `FSCTL_DUPLICATE_EXTENTS_TO_FILE`) | `copy_basis_range.rs:400` (`#[cfg(not(any(target_os = "linux", target_os = "windows")))]`) | `copy_basis_range` returns `Ok(0)`, `copy_file_range_supported()` returns `false`. | The Windows arm is **not a stub** - it uses the ReFS clone ioctl. Non-Linux, non-Windows (BSD, illumos) still get the no-op. |
| `platform_copy::dispatch::try_ficlone_impl` | `platform_copy/dispatch.rs:692` (Linux FICLONE via `rustix::fs::ioctl_ficlone`) | `platform_copy/dispatch.rs:707` (`#[cfg(not(target_os = "linux"))]`) | `Err(ErrorKind::Unsupported)` | Windows has its own `FSCTL_DUPLICATE_EXTENTS_TO_FILE` path on ReFS; macOS uses `clonefile`. The stub is intentionally Linux-only; Windows dispatch goes through a different arm. |
| `platform_copy::dispatch::platform_supports_reflink` / `platform_preferred_method` | `:773-792`, `:806-825` | `:797`, `:838` (`#[cfg(not(any(...)))]`) | `false` / `CopyMethod::StandardCopy` | Not a Windows stub - Windows is wired. The `not(any(...))` arm exists for BSD/illumos. |

### 1.2 io_uring surface (all of it)

| Entry point | Linux arm | Non-Linux arm | Stub return | Notes |
|---|---|---|---|---|
| `io_uring` module (file_reader, file_writer, socket_reader, socket_writer, disk_batch, linkat, statx, renameat2, send_zc, registered_buffers, buffer_ring, session_pool, cancel, linked_chain) | `io_uring/` | `io_uring_stub/` | Constructors `Err(ErrorKind::Unsupported)`; `is_io_uring_available()` returns `false`. | WIN-TIER2.1 row 6. After IUS-8.c, the stub module survives as `io_uring_stub/` for cross-crate uniform import. |
| `io_uring_ops::try_rename_via_io_uring` | `io_uring_ops.rs:39-71` | `io_uring_ops.rs:73-79` (`#[cfg(not(all(target_os = "linux", feature = "io_uring")))]`) | `None` (Option-encoded "try the fallback") | Caller `hard_link()` and `rename` consumers always fall through to `std::fs`. |
| `io_uring_ops::try_hard_link_via_io_uring` | `io_uring_ops.rs:99-130` | `:132-138` | `None` | Same Option-encoded fallback. |
| `io_uring_ops::try_statx_batch_via_io_uring` | `io_uring_ops.rs:167-176` | `:178-184` | `None` | Same. |
| `sqpoll_basis::WiredBasisWindow` | `sqpoll_basis.rs:42-247` (`mlock` against the basis-mmap window when SQPOLL is engaged) | `sqpoll_basis.rs:273-306` (`#[cfg(not(target_os = "linux"))]`) | `Ok(Self { len: len.min(MAX_WIRED_WINDOW_BYTES) })`; `as_ptr()` returns `std::ptr::null()`. **No-op success path.** | Not previously inventoried. Only meaningful on Linux+io_uring SQPOLL+mmap basis; on Windows the SQPOLL race the wrapper guards against does not exist. |

### 1.3 Sandbox and security

| Entry point | Linux arm | Non-Linux arm | Stub return | Notes |
|---|---|---|---|---|
| `landlock::is_supported` / `restrict_to_module_paths` | `landlock.rs:1-450` (real Landlock via the `landlock` Cargo dep) | `landlock_stub.rs:49-60` | `false` / `LandlockOutcome::Unavailable` | Stub structure is also compiled on Linux when the `landlock` feature is off. |
| `dir_sandbox::openat_dir` Linux fast path | `dir_sandbox/mod.rs:337` (`openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)`) | inline fallthrough at `:348` (`openat(O_NOFOLLOW \| O_DIRECTORY \| O_CLOEXEC)`) | `openat_nofollow` via rustix on non-Linux Unix. **No Windows arm at all** - the whole `dir_sandbox` module is `#![cfg(unix)]`. | Not a return-Ok stub; it is a "module absent on Windows" pattern. WIN-P.4 needs a fresh Windows-equivalent design rather than a stub replacement. |
| `lsm::read_active_lsms` | `lsm.rs:73` reads `/sys/kernel/security/lsm` | `lsm.rs:30` makes `LSM_LIST_PATH` an empty string; `read_active_lsms_from("")` early-returns `None`. | `None`; `has_mandatory_lsm()` always `false`. | Diagnostic-only; absence on Windows is correct. |
| `linux_capabilities::*` | `linux_capabilities.rs:42-110` (libcap-style capability surface) | `:114-172` | `Ok(())` / `false` no-op | LSM-CAP scope; Windows has no capability bits. Permanent gap. |
| `container::detect_rootless_container` | `container.rs:46` (cgroup + /proc inspection) | `container.rs:230` | `false` | Used to disable SQPOLL in rootless containers (SQP-LAND.4). Windows has no rootless-container concept; permanent gap. |
| `signal::install_signal_handler` | `signal/unix.rs` (real `sigaction` with `SA_RESTART`) | `signal/stub.rs:20` (`#[cfg(not(unix))]`) | `Ok(())` no-op | Windows uses `ctrlc` crate in higher layers; the stub exists so cross-platform callers compile. |

### 1.4 Anonymous temp files and reader stubs

| Entry point | Linux arm | Non-Linux arm | Stub return | Notes |
|---|---|---|---|---|
| `o_tmpfile::o_tmpfile_available` | `o_tmpfile/low_level.rs:30-225` | `:328-332` | `false` | Caller is `temp_file_strategy.rs`; Windows uses `win_tmpfile/` (`FILE_FLAG_DELETE_ON_CLOSE`) instead, wired since WIN-S.5. |
| `o_tmpfile::open_anonymous_tmpfile` | `:30-225` | `:335-344` | `Err(ErrorKind::Unsupported)` | Windows path uses `win_tmpfile::create_anonymous_tempfile`. Not reached on Windows. |
| `o_tmpfile::link_anonymous_tmpfile` | `:230-322` | `:347-353` | `Err(ErrorKind::Unsupported)` | Same. |
| `mmap_reader::MmapReader` | `mmap_reader.rs:1-340` (real `mmap2` mapping) | `mmap_reader_stub.rs:1-160` | Reads file into a `Vec<u8>` and returns the buffer. Doubles peak RSS for large basis. | WIN-S.LAND.1.b in flight: `WindowsChunkedReader` ships behind a feature flag and replaces this allocation pattern. Stub still exists for the unwired callers. |
| `kqueue_stub` (full module) | `kqueue/` (macOS) | `kqueue_stub.rs` (`#![cfg(not(target_os = "macos"))]`) | Constructors `Err(ErrorKind::Unsupported)`, `is_kqueue_available()` returns `false`. | Not a Linux-only stub but the same pattern. Not in WIN-P scope - tracked under DASYNC/macOS work. |

### 1.5 Miscellaneous status surface

| Entry point | Linux arm | Non-Linux arm | Stub return | Notes |
|---|---|---|---|---|
| `status::*` (multiple functions reading `/proc/self/status`, `/sys/kernel/io_uring`, etc.) | `status.rs:23-450` | `status.rs:112`, `:341`, `:389`, `:518`, `:534`, `:556-878` | Returns `None`, `false`, or default-constructed status struct. | Diagnostic-only output for `--io-uring=status` and `--lsm-status` CLI flags. Windows correctly reports "feature unavailable" rather than fabricating Linux-specific state. |
| `syscall_batch::stat_file` | `syscall_batch.rs:245-256` (statx fast path) | `:260-267` | Standard `std::fs::metadata` / `symlink_metadata` | Not a Windows stub - the non-Linux arm is a functioning fallback that uses portable std calls. |

## 2. Caller-graceful-degradation classification

The classification axis is whether a caller that reaches the non-Linux
arm gets:

- **A** - a correct buffered/portable fallback that produces the same
  observable outcome as the Linux arm (slower, but correct).
- **B** - a typed `Err(ErrorKind::Unsupported)` that the caller must
  handle, and where every existing caller in tree does handle it.
- **C** - an `Option::None` or `false` that the caller pattern-matches
  to fall through to a portable implementation.
- **D** - a no-op success (`Ok(())`, `Ok(0)`, `Ok(Self)`) that is
  correct only because the Linux-side concern (e.g. mlock against the
  basis window, capability drop) does not exist on Windows.
- **E** - **module absent on the platform**, no API present. Callers
  must already be cfg-gated; no degradation surface.

| Stub | Class | Rationale |
|---|---|---|
| `splice::try_splice_to_file` | B | `Err(Unsupported)`. No production Windows caller. |
| `splice::try_vmsplice_to_file` | B | Same. |
| `splice::recv_fd_to_file` | B | Same. |
| `splice::SplicePipe::*` | B | Constructor `Err(Unsupported)`; safety boundary at construction. |
| `vmsplice_writer::VmspliceFileWriter` | E | Enum variant absent on Windows; dispatcher selects `Writer::Iocp` or `Writer::Buffered`. |
| `sendfile::send_file_to_fd` | **A** | Post WIN-S.LAND.1.d the non-unix arm calls `copy_via_fd_write` and is correct. The prior P1 silent-discard risk is closed. |
| `sendfile::send_file_to_fd_with_policy` | A | Same. |
| `copy_file_range::try_copy_file_range` | B | Caller `copy_file_contents` always falls through to the buffered loop on `Err(Unsupported)`. |
| `copy_basis_range::imp::copy_basis_range` (non-Linux non-Windows arm) | C | Returns `Ok(0)`; caller treats zero-byte clone as fallback signal. **Windows is not a stub** - the ReFS arm runs. |
| `platform_copy::try_ficlone_impl` | B | Caller has Windows `FSCTL_DUPLICATE_EXTENTS_TO_FILE` and macOS `clonefile` alternatives selected by `platform_preferred_method`. |
| `io_uring/io_uring_stub` (full surface) | B+C | Constructors `Err(Unsupported)`; `is_io_uring_available()` `false`. Every caller checks the bool or matches the result. No Windows code path enters the stub for production work; IOCP is the structural replacement. |
| `io_uring_ops::try_*_via_io_uring` | C | All return `None` on non-Linux. Wrappers like `hard_link()` fall through to `std::fs::hard_link`. |
| `sqpoll_basis::WiredBasisWindow` (non-Linux) | **D** | Success no-op. **Correct precisely because the SQPOLL+mmap race the wrapper guards against does not exist on Windows.** Returns `null` pointer and clamped length; callers that dereference `as_ptr()` would crash, but no Windows caller does. |
| `landlock_stub` | C | `false` / `LandlockOutcome::Unavailable`. SEC-1 `*at` chain remains the active defense; daemon code matches on the outcome. |
| `dir_sandbox` (module) | **E** | Module `#![cfg(unix)]`. The receiver pipeline already cfg-gates around it; on Windows the path-based syscalls are unsandboxed and rely on NTFS handle-based APIs (per SEC-1.l audit). |
| `lsm::*` | C | `read_active_lsms` returns `None`; `has_mandatory_lsm` returns `false`. Diagnostic-only. |
| `linux_capabilities::*` | D | `Ok(())` no-op for capability drops; `false` for queries. Correct because Windows has no capability bits. |
| `container::detect_rootless_container` | D | `false` no-op. Correct because rootless containers are a Linux+cgroup concept. |
| `signal::install_signal_handler` | D | `Ok(())` no-op. Real Ctrl+C handling lives in `ctrlc` at the consumer layer. |
| `o_tmpfile::*` | B | All `Err(Unsupported)`. Windows callers route through `win_tmpfile/` instead. |
| `mmap_reader_stub::MmapReader` | **A (degraded)** | Returns a `Vec<u8>` with the file contents - functionally correct but doubles peak RSS for large files. WIN-S.LAND.1.b chunked reader is the replacement. |
| `kqueue_stub` | B | `Err(Unsupported)`. No production caller wires it. |
| `status::*` | C | `None` / default struct; consumed by diagnostic CLI flags. |

Risk summary by class:

- **Class D (no-op success)** is the surprising bucket. Three sites
  (`sqpoll_basis::WiredBasisWindow`, `linux_capabilities`,
  `container::detect_rootless_container`, plus `signal::install`)
  return success without doing anything. All four are correct because
  the Linux concern is absent on Windows. **The risk is regression by
  porting**: if someone moves Linux-only logic into a cross-platform
  caller and assumes the no-op stub is also a no-op semantically, a
  bug can hide. WIN-P should document these explicitly.
- **Class E (module absent)** is `dir_sandbox` and the Vmsplice writer
  variant. Callers must compile-time cfg-gate. WIN-P.4 must design a
  Windows sandbox primitive (covered separately under WIN-P.4 task).
- **Class A degraded** is `mmap_reader_stub` only. The fix is already
  in flight under WIN-S.LAND.1.b.

## 3. Priority list for WIN-P.2 / .3 / .4 / .5 follow-up

This list takes WIN-S.8's matrix as the throughput-impact baseline and
re-prioritises for the WIN-P series specifically, which is about
shipping Windows equivalents rather than auditing risk.

### WIN-P.2 - splice (#3683)

- **Audit verdict (carry from WIN-S.3 + windows-splice-vmsplice-equivalents.md):** No Win32 primitive matches splice's pipe-mediated kernel-to-kernel zero-copy. The closest analogues - WSARecv/WSASend with WSAIoctl SIO_TCP_INITIAL_RTO - operate on socket buffers only and do not avoid the user-space copy for socket-to-file paths.
- **Production reach today:** Zero. `splice` entry points are re-exported but no Windows-bound caller invokes them. IOCP is the production receive path.
- **WIN-P.2 recommendation:** **Document as permanent gap.** No equivalent worth shipping; IOCP is already faster than Cygwin's userspace `read`/`write` fallback. WIN-P.7 should be closed with "no implementation" and a pointer to WIN-S.3.

### WIN-P.3 - vmsplice (#3684)

- **Audit verdict (carry from WIN-S.4 + windows-splice-vmsplice-equivalents.md):** Even more restrictive than splice. `vmsplice` writes user pages directly into a kernel pipe; the closest Windows analogues are `WriteFileGather` (vectored writes) and Registered I/O `RIO_BUF` (already audited under WPG-9). Both target socket I/O, not file writes.
- **Production reach today:** Zero. `Writer::Vmsplice` enum variant is cfg-gated out on Windows entirely.
- **WIN-P.3 recommendation:** **Document as permanent gap.** WPG-9 already shipped the Registered I/O equivalent for socket sends. WIN-P.8 should be closed with no implementation.

### WIN-P.4 - Landlock (#3685)

- **Audit verdict (carry from WIN-S.7 + windows-landlock-equivalent.md):** Three candidate equivalents exist on Windows: restricted tokens (`CreateRestrictedToken`), AppContainer (`SECURITY_CAPABILITIES` + `CreateProcess` flags), and Job Objects with `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`. None are drop-in replacements - all require process-level setup, not per-thread runtime sandbox engagement.
- **Production reach today:** Zero on Windows. Landlock callers are gated by the `landlock` Cargo feature (Linux-only crate). The `dir_sandbox` module that backs the `*at` security chain is `#![cfg(unix)]`, so the entire SEC-1 chain is absent on Windows.
- **WIN-P.4 recommendation:** **Investigate restricted-token approach first.** Job Objects are the simpler win for daemon worker isolation; AppContainer carries heavier setup cost. The dependency is on **a Windows equivalent of `dir_sandbox`'s parent-dirfd model** - without that, even a restricted token does not give us the symlink-resistance the Linux path has.
- **Order of operations:** Design `dir_sandbox` Windows equivalent (handle-based path validation) **before** wiring Landlock equivalent. Otherwise the sandbox is incomplete.

### WIN-P.5 - SQPOLL (#3686)

- **Audit verdict (carry from WPG-1 + windows-transmitfile.md):** IOCP threadpool with `BindIoCompletionCallback` and Registered I/O `RIO_NOTIFY` queues are the structural equivalents. RIO completion polling without a syscall round-trip matches what SQPOLL gives io_uring on Linux.
- **Production reach today:** IOCP is already production-wired. SQPOLL itself is Linux-only inside the `sqpoll_basis` module and gated behind io_uring. No Windows caller reaches the `sqpoll_basis` no-op stub.
- **WIN-P.5 recommendation:** **Document as already-shipped under different name.** The Windows IOCP path is the structural answer; SQPOLL has no separate concept to port. WIN-P.10 should close with a pointer to WPG-7..9 and the existing IOCP work.

### WIN-P feed-forward priorities (synthesised)

Ranked by likely user-visible impact, taking the above audits as input:

1. **HIGH - sandbox seam (dependency for WIN-P.4).** Design a Windows
   equivalent of the `dir_sandbox` openat2/openat chain. Without it,
   Landlock-equivalent restricted tokens give partial protection only.
   Tracked separately under WPC-V verification; should become a fresh
   WIN-P.4-prereq task before any sandboxing equivalent ships.
2. **MEDIUM - finish `mmap_reader_stub` chunked replacement (WIN-S.LAND.1.b.2..7).** This is the only currently-degraded Class-A stub; cuts peak RSS on multi-GB checksum and signature builds. Already scoped, just needs the wire-up.
3. **MEDIUM - document Class D sites as permanent-gap-by-design.** `sqpoll_basis::WiredBasisWindow`, `linux_capabilities`, `container::detect_rootless_container`, and `signal::install_signal_handler` non-Linux arms are all no-op-correct. They should be explicitly documented in the user matrix as "Linux-only concern, not a Windows feature gap" so future contributors don't try to "fix" them.
4. **LOW - close WIN-P.7, WIN-P.8, WIN-P.10 as permanent-gap** with pointers to WIN-S.3 / WIN-S.4 / WPG-7 respectively. No work to do; just resolution.
5. **DEFER - WIN-P.9 (Landlock implementation)** until the dir_sandbox seam exists.

## 4. Permanent-gap candidates

Per the WIN-TIER2 series classification framework, these stubs have no
Windows equivalent worth shipping:

| Stub | Reason | Cross-reference |
|---|---|---|
| `splice::*` | No Win32 pipe-mediated kernel-to-kernel zero-copy. IOCP is faster than the upstream Cygwin userspace fallback. | WIN-S.3, `windows-splice-vmsplice-equivalents.md` |
| `vmsplice_writer::VmspliceFileWriter` | Same as splice; `WriteFileGather` and `RIO_BUF` cover the narrow socket-vectored case but not file writes. | WIN-S.4, WPG-9 |
| `sqpoll_basis::WiredBasisWindow` | Guards a Linux-specific SQPOLL+mmap race. IOCP threadpool is the Windows structural replacement and has no equivalent race to mitigate. | SQM series, WPG-3 |
| `linux_capabilities::*` | Windows has no `CAP_*` bits. Capability checks should be no-ops on Windows by design. | LSM-CAP series |
| `container::detect_rootless_container` | Linux+cgroup concept; Windows has no rootless-container model. | SQP-LAND series |
| `dir_sandbox::*` module | Module-level cfg-gate is correct; Windows needs handle-based equivalent designed separately. | SEC-1.l audit, WIN-P.4 prereq |
| `signal::install_signal_handler` | Real Ctrl+C handling delegated to `ctrlc` in higher layers on Windows. | SEC-1.l audit |
| `lsm::*` | Diagnostic-only; Windows uses ACL/SACL audit instead. | LSM-DETECT series |
| `o_tmpfile::*` | Replaced by `win_tmpfile/` via `FILE_FLAG_DELETE_ON_CLOSE`. | WIN-S.5, WIN-S.9 |
| `io_uring/io_uring_stub` | IOCP is the production Windows path. The stub mirrors the API surface only so cross-crate imports compile. | IUS-7, IUS-8 |
| `kqueue_stub` | macOS-only primitive; no Linux or Windows equivalent in scope. | DASYNC, ASY-4 |
| `copy_file_range::try_copy_file_range` non-Linux arm | Windows uses `CopyFileExW` via `platform_copy`; macOS uses `clonefile`. Stub is correct. | WIN-S.10, IUD-10 |
| `platform_copy::try_ficlone_impl` non-Linux arm | Windows uses ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`; macOS uses `clonefile`. Stub is correct. | WIN-S.10 |

The list above plus the existing WIN-TIER2 doc covers every Linux-only
cfg-gated entry point in `crates/fast_io/src/`.

## 5. Cross-reference index

- WIN-S.LAND.1 (2026-06-09, PR #5295): traced send_file_to_fd reachability and removed the io::sink() silent-discard. Closed the WIN-S.8 P1 latent defect; the row in this audit reflects the buffered fallback.
- WIN-S.LAND.1.d (2026-06-09): cfg-gated dead non-unix sendfile/recv_fd re-exports out of `lib.rs`.
- WIN-S.LAND.1.b series (in flight): `WindowsChunkedReader` replacing `mmap_reader_stub` Vec-per-file pattern.
- SQP-LAND.4: `container::detect_rootless_container` wired into io_uring config to skip SQPOLL when rootless.
- SEC-MK series (2026-06-04): `mknodat`/`mkfifoat` migrated through `dir_sandbox`; on Windows these are intentionally absent (NTFS does not support `mknod`).
- WPC-V series: production-code verification of WPC-3/4/8/9 status; informs the WIN-P.4 sandbox-seam dependency note.
- WIN-TIER2 series (2026-06-10): companion inventory keyed by Windows-transfer-path reachability and Cygwin parity.

## 6. Tracking

- Parent: **WIN-P** (#3681).
- This audit: **WIN-P.1** (#3682).
- Feed-forward: WIN-P.2 (#3683), WIN-P.3 (#3684), WIN-P.4 (#3685), WIN-P.5 (#3686), WIN-P.6 (#3687) decision matrix, WIN-P.7..10 implementations, WIN-P.11 regression tests, WIN-P.12 support-matrix update.
