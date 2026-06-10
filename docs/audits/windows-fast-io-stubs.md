# Windows splice / vmsplice / copy_file_range stub audit

Tracking issue: WIN-S.LAND.1 (oc-rsync task #3639). Branch:
`docs/win-s-land-1-stub-audit` (forked from `origin/master`).
Static, source-grounded audit; no benchmarks were collected.

## 1. Scope

Catalog every site in `crates/fast_io/src/` where Windows takes a
documented stub or Linux-only fallback in place of a kernel-assisted
copy primitive (`splice(2)`, `vmsplice(2)`, `copy_file_range(2)`,
`sendfile(2)`, `O_TMPFILE`, io_uring `RENAMEAT2 / LINKAT / STATX /
SEND_ZC`, FICLONE). For each entry, name the current Windows
behavior, whether the IOCP path (or an equivalent Windows primitive)
already replaces the stub, and whether the gap matters for transfer
throughput.

Companion documents:

- `docs/audits/windows-iocp-file-write-status.md` - IOCP wiring
  verdict for the disk-commit hot path.
- `docs/audits/windows-iocp-benchmark.md` - measurement plan.

The user surfaced (2026-06-10) that the IOCP path "has documented
stubs in place of splice/vmsplice equivalents". `crates/fast_io/src/
vmsplice_writer.rs:202` was confirmed as one such site. The full
catalog below is the answer to "are there others, and which ones
affect transfer throughput?".

## 2. Verdict

Windows has **two** classes of stub:

1. **Linux-primitive stubs** that are correct: the primitive does
   not exist on Windows, the stub returns `io::ErrorKind::Unsupported`
   or `None`, and a real Windows path (IOCP `WriteFile`, `ReadFile`,
   `CopyFileExW`, ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, `MoveFileExW`
   via `std::fs::rename`) covers the same semantic surface elsewhere.
   These are not throughput regressions - they exist so cross-platform
   call sites compile against one type.
2. **Non-Linux fallbacks shared with Windows** that route through
   buffered `read`/`write` (256 KiB or 64 KiB) because no Windows
   equivalent has been wired into `fast_io`'s public dispatch yet.
   The `sendfile`/`splice` receive path is the only one in this class
   with a measurable transfer-throughput cost on Windows; everything
   else is metadata or test-only.

No production write path on Windows currently dead-ends at an
`Unsupported` stub: the IOCP file writer (`crates/fast_io/src/iocp/
file_writer.rs`) and the Windows `copy_basis_range` implementation
(`crates/fast_io/src/copy_basis_range.rs:243`) cover the
`vmsplice`/`splice`/`copy_file_range` semantic surface for body
transfer using overlapped `WriteFile`/`ReadFile` with `OVERLAPPED`
offsets. The audit confirms the gap is narrower than the original
phrasing suggested.

## 3. Stub catalog

Each row: file:line of the cfg attribute, name of the
function/type, Linux primitive being stubbed, what Windows runs
today, whether an IOCP-equivalent already replaces the stub, and a
priority (P0 = transfer-throughput-affecting, P1 = metadata or rare
path, P2 = stub-for-compile only).

| # | Site | Function / type | Linux primitive | Windows behavior today | IOCP / Windows equivalent wired? | Priority |
|---|------|-----------------|-----------------|------------------------|----------------------------------|----------|
| 1 | `vmsplice_writer.rs:207` (`#[cfg(not(all(target_os = "linux", feature = "vmsplice")))]`) | `VmspliceFileWriter::{new, write_chunk}` | `vmsplice(2)` page-aligned pipe write | `new` returns `Unsupported`; `write_chunk` returns `Unsupported`; `pipe_capacity` returns 0. Cross-platform callers compile but never enter this type's fast path. | Yes - `iocp::file_writer::IocpWriter` covers body writes via overlapped `WriteFile`. | P2 (compile-only) |
| 2 | `splice/mod.rs:330` (`#[cfg(not(target_os = "linux"))]`) | `SplicePipe::{new, with_capacity, capacity, splice_to_file, vmsplice_to_file}` | `pipe(2)` + `splice(2)` / `vmsplice(2)` | All constructors and methods return `Unsupported`; `capacity` returns 0. | Yes - real send path goes through IOCP or buffered `read`/`write` in `splice/syscalls.rs:recv_fd_to_file` (entry 3). | P2 (compile-only) |
| 3 | `splice/syscalls.rs:271, 280, 356` (`#[cfg(not(target_os = "linux"))]`, `#[cfg(not(unix))]`) | `try_vmsplice_to_file`, `try_splice_to_file`, `recv_fd_to_file` (non-unix variant) | `vmsplice(2)`, `splice(2)`, socket->file `splice(2)` | `try_*` return `Unsupported`; `recv_fd_to_file(non-unix)` returns `Unsupported`. Windows callers must route through a higher-level reader; there is no `fast_io` socket-to-file fast path on Windows. | Partial - `crates/fast_io/src/iocp/socket.rs` exists, but it is not invoked by the network receive path in transfer. The receive path uses `std::io::copy` / `BufReader` instead. | **P0 (Windows recv throughput)** |
| 4 | `splice/syscalls.rs:346` (`#[cfg(all(unix, not(target_os = "linux")))]`) | `recv_fd_to_file` (other-unix variant) | `splice(2)` | Calls `copy_fd_to_fd` (buffered `read`/`write` via libc, 256 KiB). Not Windows, but shares semantics with #3. | n/a (not Windows) | P1 |
| 5 | `copy_file_range.rs:329` (`#[cfg(not(target_os = "linux"))]`) | `try_copy_file_range` | `copy_file_range(2)` | Returns `Unsupported`; caller falls through to `copy_file_contents_readwrite` (256 KiB buffered loop). | Yes for local-copy: `platform_copy/dispatch.rs:92` Windows path uses ReFS reflink -> `CopyFileExW` -> `std::fs::copy` (entry 11). For delta basis copies: `copy_basis_range.rs:243` Windows impl uses `ReadFile`/`WriteFile` with `OVERLAPPED`. | P2 (replaced) |
| 6 | `sendfile/mod.rs:193, 235` (`#[cfg(not(unix))]`) | `send_file_to_fd`, `send_file_to_fd_with_policy` | `sendfile(2)` (Linux/macOS) | Both delegate to `send_file_to_writer` with `io::sink()`, which discards bytes. Effectively `send_file_to_fd` is a no-op on Windows that throws away the file. | No - `iocp::transmit_file.rs` exists (Windows `TransmitFile` for sockets), but is not the target of these calls. Producer side (sender) on Windows currently goes through `std::io::copy` instead. | **P0 (Windows send throughput, but only if any production caller hits these)** |
| 7 | `io_uring_stub/send_zc.rs:26` (`#[cfg(not(all(target_os = "linux", feature = "io_uring")))]`) | `try_send_zc`, `ZeroCopySender` | `io_uring` `IORING_OP_SEND_ZC` | `try_send_zc` returns `Unsupported`; `is_supported` returns `false`; `ZeroCopySender::{new, send_zc}` return `Unsupported`. | Partial - `iocp::socket.rs` provides overlapped socket send, but is gated by `iocp` feature and not unconditionally invoked. | P1 (advisory; non-zero-copy SEND is the default even on Linux) |
| 8 | `io_uring_stub/linked_chain.rs:86, 95` | `LinkedChain::submit_and_wait`, `read_then_write` | `io_uring` linked-SQE chains | Returns `Unsupported`. The stub `LinkedChain::new`/`read`/`write` builders compile but never append. | No direct equivalent (IOCP has no chain primitive). Use IOCP per-op submissions instead. | P2 (compile-only) |
| 9 | `io_uring/linkat.rs:94` (`#[cfg(not(target_os = "linux"))]`) | `linkat_supported` | `io_uring` `IORING_OP_LINKAT` | Returns `false`; callers fall back to `std::fs::hard_link`. | n/a - `std::fs::hard_link` is the real Windows path (`CreateHardLinkW`). | P2 (replaced) |
| 10 | `io_uring/statx.rs:122, 434, 754` | `statx_supported`, `submit_statx_blocking`, `submit_statx_batch` | `io_uring` `IORING_OP_STATX` | `statx_supported` returns `false`; submit fns return `Unsupported`; callers fall back to `std::fs::metadata`. | n/a - `std::fs::metadata` is the real Windows path (`GetFileAttributesExW`). | P2 (replaced) |
| 11 | `io_uring_ops.rs:270, 332, 389` (test-only `#[cfg(not(target_os = "linux"))]`) | `try_rename_via_io_uring`, `try_hard_link_via_io_uring`, `try_statx_batch_via_io_uring` test assertions | n/a (production fns at lines 32/92 return `None` on non-Linux via separate `impl`) | Production callers receive `None` and fall back to `std::fs` equivalents. | n/a (replaced; these are test guards) | P2 (test-only) |
| 12 | `platform_copy/dispatch.rs:707` (`#[cfg(not(target_os = "linux"))]`) | `try_ficlone_impl` | `FICLONE` ioctl | Returns `Unsupported`; Windows dispatch never calls this (the Windows arm at `dispatch.rs:92` uses ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` instead). | Yes - `try_refs_reflink_impl` at `dispatch.rs:281` is the Windows equivalent. | P2 (replaced) |
| 13 | `o_tmpfile/low_level.rs:328, 335, 347` | `o_tmpfile_available`, `open_anonymous_tmpfile`, `link_anonymous_tmpfile` | `O_TMPFILE` | `available` returns `false`; open/link return `Unsupported`. | Yes - `win_tmpfile.rs` provides `FILE_FLAG_DELETE_ON_CLOSE` via `CreateFileW`, wired into `WindowsTempFileStrategy` (`temp_file_strategy.rs:175`). | P2 (replaced) |
| 14 | `sqpoll_basis.rs:272, 277` | `WiredBasisWindow` | `mlock(2)` for SQPOLL basis | No-op stub; constructor always succeeds. | n/a - SQPOLL is Linux-only; Windows IOCP has no equivalent concept. | P2 (no-op) |
| 15 | `linux_capabilities.rs:114` | `openat2_supported` | `openat2(2)` | Returns `false`; callers fall back to `OpenOptions::open`. | n/a - Windows uses `CreateFileW` directly. | P2 (replaced) |
| 16 | `splice/probe.rs:19` | `is_splice_available` | `splice(2)` capability probe | Returns `false`. | n/a (capability indicator, not a transfer primitive) | P2 (informational) |
| 17 | `syscall_batch.rs:260` | `stat_file` (non-Linux) | `statx(2)` | Falls back to `std::fs::metadata` / `symlink_metadata`. | n/a - this is the documented Windows path. | P2 (replaced) |
| 18 | `signal/mod.rs:27` -> `signal/stub.rs:20` (`#[cfg(not(unix))]`) | `install_signal_handler` | `sigaction(2)` | No-op `Ok(())`. Windows uses `ctrlc` crate at higher layers. | Yes (at consumer layer, not in `fast_io`) | P2 (replaced) |
| 19 | `kqueue_stub.rs` (whole module, `#![cfg(not(target_os = "macos"))]`) | `KqueueLoop` and friends | `kqueue(2)` (BSD/macOS) | All constructors return `Unsupported`; never produces events. | n/a - Windows uses IOCP completion port (`iocp::completion_port.rs`); Linux uses io_uring. The audit treats this row as Windows-relevant because the type compiles on Windows and could be referenced by mistake. | P2 (compile-only) |
| 20 | `mmap_reader_stub.rs` (whole module, `#[cfg(not(unix))]`) | `MmapReader::open` and friends | `mmap(2)` | Reads entire file into a `Vec<u8>` via `BufReader::read_to_end`. Not zero-copy; allocates the file size. | Partial - Windows could use `CreateFileMappingW` + `MapViewOfFile`, but this is not wired. The `BufReader` fallback is correct for small files but degrades on multi-GB basis files. | P1 (large-file basis on Windows) |

Total cfg-gated stub or Linux-only fallback sites in
`crates/fast_io/src/`: 41 non-Linux entries + 13 non-unix entries +
Windows-specific real implementations. After deduplication (multiple
attributes for the same logical entry-point), there are **20 logical
stub families** as catalogued above.

## 4. Summary

The original phrasing "IOCP path has documented stubs in place of
splice/vmsplice equivalents" is technically accurate but materially
narrower than it sounds:

- 14 of the 20 families are **P2 compile-only stubs** that exist so
  cross-platform callers can name a single type. None of these
  represent a Windows throughput regression because a real Windows
  primitive (IOCP `WriteFile`/`ReadFile`/`TransmitFile`,
  `CopyFileExW`, ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`,
  `FILE_FLAG_DELETE_ON_CLOSE`, `CreateHardLinkW`,
  `GetFileAttributesExW`, `std::fs::rename`/`MoveFileExW`) covers the
  same semantic surface elsewhere in the crate.
- 1 family (`mmap_reader_stub.rs`, entry 20) is a **P1
  memory-pressure risk** on multi-GB basis files: the non-Unix
  fallback reads the entire file into a `Vec<u8>` instead of mapping
  it. Production callers that route to `MmapReader::open` with a
  large basis on Windows will allocate that much RAM.
- 2 families (entries 3 and 6) are **P0 throughput-affecting**: the
  non-Linux `recv_fd_to_file` and the non-unix `send_file_to_fd`
  return `Unsupported` (or worse, `send_file_to_fd(non-unix)` writes
  to `io::sink`). Their throughput impact depends entirely on whether
  any production call site on Windows actually invokes them - the
  receive path in `transfer/` uses `std::io::copy` plus `BufReader`
  today, not these helpers, so this is a latent risk rather than a
  measured regression. A follow-up audit must trace call graphs from
  `crates/transfer/` and `crates/protocol/` to confirm.

The `vmsplice_writer.rs:202` site identified in the user's prompt
falls into the **P2 compile-only** class: the type is never the
selected writer on Windows, the IOCP writer is, and the stub exists
purely so the type is namable in cross-platform call sites.

## 5. Recommended follow-ups

Tracked as WIN-S.12-style siblings under WIN-S:

1. **WIN-S.12.a (P0)** - Trace call graphs from
   `crates/transfer/src/` and `crates/protocol/src/` to confirm
   whether `splice::syscalls::recv_fd_to_file` or
   `sendfile::send_file_to_fd` are reachable on Windows in
   production. Output: one paragraph per call site stating the
   selected path on Windows today.
2. **WIN-S.12.b (P0)** - If WIN-S.12.a finds a reachable Windows
   path, wire `iocp::socket::IocpSocketReader` /
   `IocpSocketWriter` into the affected entry-point so receive
   throughput uses IOCP instead of `BufReader`. Mirror the
   `iocp::file_writer` wiring pattern documented in
   `docs/audits/windows-iocp-file-write-status.md`.
3. **WIN-S.12.c (P0)** - If WIN-S.12.a finds `send_file_to_fd`
   reachable on Windows, replace the `io::sink` fallback with
   either an IOCP `TransmitFile` dispatch (for sockets) or a
   buffered `read`/`write` loop (for arbitrary fds). The current
   behavior silently drops the file body and must not stay
   reachable.
4. **WIN-S.12.d (P1)** - Replace `mmap_reader_stub::MmapReader`
   with a `CreateFileMappingW` + `MapViewOfFile` implementation
   gated on `target_os = "windows"`. Match the public surface of
   the Unix `mmap_reader::MmapReader` so callers do not need
   `#[cfg]` branching. Add an interop test that opens a 4 GiB
   sparse basis and asserts RSS stays under 100 MiB.
5. **WIN-S.12.e (P2)** - Document each P2 row in the catalog
   inline at the cfg attribute so future audits can verify the
   "stub exists for cross-platform compile" intent without a
   round-trip to this file. One-line comment per site referencing
   the Windows-equivalent function name.
6. **WIN-S.12.f (P2)** - Add a doc-comment to
   `crates/fast_io/src/lib.rs` enumerating which Windows primitive
   replaces each Linux/Unix-only primitive, so consumers can pick
   the right entry-point without grepping for cfg attributes.

The audit makes no source changes. Implementation work belongs in
the follow-up tasks above.

## 6. Methodology

Search pattern, run from repo root:

```sh
grep -rn '#\[cfg(target_os = "windows")\]' crates/fast_io/src/
grep -rn '#\[cfg(not(target_os = "linux"))\]' crates/fast_io/src/
grep -rn '#\[cfg(not(unix))\]' crates/fast_io/src/
```

Cross-reference each non-Linux / non-unix entry against the IOCP
module surface (`crates/fast_io/src/iocp/`) and the Windows arms in
`platform_copy/dispatch.rs`, `copy_basis_range.rs`,
`temp_file_strategy.rs`, `win_tmpfile/`, and `lib.rs` re-exports.
Classify priority by whether the stub is reachable on a Windows
production path. Test-only `#[cfg(not(target_os = "linux"))]`
attributes inside `mod tests` were excluded from the throughput
analysis.
