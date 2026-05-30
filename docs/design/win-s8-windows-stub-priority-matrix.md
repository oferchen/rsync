# WIN-S.8: Windows Stub Replacement Priority Matrix

Ranks every `fast_io` platform stub on Windows by four axes - correctness
impact, throughput impact, call-site frequency, and availability of a
Windows-native equivalent - to guide replacement sequencing.

## Background

WIN-S.1 inventoried seven categories of platform stubs in `crates/fast_io`
that compile on Windows but provide degraded or no-op behavior. This
document assigns each a priority tier (P0-P3) based on evidence from the
source code, specifically which production call sites can reach each stub
and what the observable consequence is.

## Priority Matrix

| Stub | Correctness | Throughput | Frequency | Win32 Equiv | Priority |
|------|-------------|------------|-----------|-------------|----------|
| `send_file_to_fd` (io::sink) | **Critical** (data discard) | N/A | **None** (not wired) | TransmitFile | P1 |
| `copy_file_range` / `copy_file_contents` | None (clean fallback) | Moderate | **High** (every file transfer) | Already handled | P3 |
| `copy_basis_range` | None (returns Ok(0)) | Moderate | High (every delta COPY token) | None standard | P2 |
| splice / `recv_fd_to_file` | Error on non-unix | Low | **None** (not wired) | N/A | P3 |
| vmsplice / `VmspliceFileWriter` | Error (Unsupported) | Low | Gated (linux+feature) | N/A | P3 |
| `O_TMPFILE` | None (returns false/Unsupported) | None | Medium | FILE_FLAG_DELETE_ON_CLOSE | P2 |
| Landlock | None (returns Unavailable) | None | Low (daemon only) | Restricted tokens / AppContainer | P2 |
| io_uring stub | None (type stubs only) | None | None (IOCP path exists) | IOCP (already wired) | P3 |
| mmap_reader | None (reads into Vec) | Moderate | Medium (checksums, generator) | CreateFileMapping | P2 |

## Detailed Analysis

### P0: No Items at P0

The WIN-S.1 inventory identified `send_file_to_fd` writing to `io::sink()`
as a potential P0 correctness bug. After tracing call sites through the
entire codebase, this assessment is **downgraded to P1** for the following
reason:

**`send_file_to_fd` and `send_file_to_fd_with_policy` are exported from
`fast_io` but never called from production code.** No crate outside
`fast_io` - not `transfer`, `engine`, `daemon`, `core`, or `protocol` -
imports or invokes either function. The functions exist as library-level
APIs for a future sender-side zero-copy path that has not been wired yet.
Similarly, `TransmitFile` (the Windows-native counterpart already
implemented in `iocp/transmit_file.rs`) is also not called from production
code.

Because no production transfer path reaches the `io::sink()` stub today,
there is no active data-loss risk. The stub would only become dangerous if
someone wires `send_file_to_fd` into the sender without noticing the
Windows fallback - hence P1.

### P1: High Priority - Fix Before Wiring Sender Zero-Copy

#### 1. `send_file_to_fd` / `send_file_to_fd_with_policy` (sendfile/mod.rs:193-242)

**What the stub does:** On `#[cfg(not(unix))]`, both functions call
`send_file_to_writer(source, &mut io::sink(), length)`, which reads the
entire source file into a 256 KB buffer and discards every byte. The
function returns `Ok(length)` as if the transfer succeeded.

**Why P1:** The io::sink() behavior is a **latent correctness bomb**. Any
future integration of `send_file_to_fd` into the daemon or SSH sender
path on Windows would silently discard file data with no error. The
TransmitFile implementation already exists in `iocp/transmit_file.rs` but
is also not wired.

**Callers:** None in production. The function is re-exported via
`fast_io::send_file_to_fd_with_policy` (lib.rs:273) but no consumer crate
imports it.

**Recommended fix:** Replace `io::sink()` with an `Err(Unsupported)` return
that matches the pattern used by every other non-unix stub (splice,
vmsplice, recv_fd_to_file). This prevents silent data loss if the function
is ever wired and forces callers to handle the fallback explicitly.

**Windows equivalent:** `TransmitFile()` already implemented behind the
`transmitfile` Cargo feature in `iocp/transmit_file.rs`. When the sender
zero-copy path is wired, the dispatch should be:

```
#[cfg(unix)]     -> sendfile(2) / copy_via_fd_write
#[cfg(windows)]  -> TransmitFile() / WSASend fallback
```

### P2: Medium Priority - Improve Performance on Common Paths

#### 2. `copy_basis_range` (copy_basis_range.rs)

**What the stub does:** On non-Linux, returns `Ok(0)` immediately,
signaling the caller to fall back to the read+write path.

**Why P2:** Called on every delta-transfer COPY token in the receiver
(`transfer/src/delta_apply/applicator.rs:416`). Delta transfers are the
common case for incremental syncs. On Linux, `copy_file_range(2)` performs
the copy in-kernel; on Windows the fallback reads through userspace, adding
a memory copy per COPY token.

**Windows equivalent:** No direct equivalent. Windows does not expose
`copy_file_range`. The options are:

- `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (ReFS only, already used in
  `platform_copy/dispatch.rs` for whole-file reflink)
- Manual `ReadFile`/`WriteFile` with explicit offsets (what the fallback
  already does via Rust's `std::io`)

The current fallback is functionally correct and competitive in throughput
because the delta COPY tokens are typically small (block-sized). No
immediate action needed, but worth revisiting if profiling shows the
read+write overhead is measurable on large-block delta transfers.

#### 3. `O_TMPFILE` (o_tmpfile/low_level.rs)

**What the stub does:** On non-Linux, `o_tmpfile_available()` returns
`false` and `open_anonymous_tmpfile()` returns `Err(Unsupported)`.

**Why P2:** The engine's write strategy (`engine/src/local_copy/executor/
file/copy/transfer/write_strategy.rs`) probes `o_tmpfile_available(dir)`
and falls back to named temp files when it returns false. Named temp files
are correct but create a TOCTOU window and leave cleanup debris on crash.

**Windows equivalent:** `FILE_FLAG_DELETE_ON_CLOSE` with `CreateFileW`
provides similar anonymous-until-committed semantics, though the
finalization mechanism differs (hardlink vs rename). An `ReplaceFile` or
`MoveFileEx(MOVEFILE_REPLACE_EXISTING)` would serve as the commit step.

**Impact:** Correctness is unaffected - named temp files work. The crash
safety improvement and cleanup simplification make this a quality-of-life
improvement, not a throughput concern.

#### 4. Landlock (landlock_stub.rs)

**What the stub does:** `is_supported()` returns `false`,
`restrict_to_module_paths()` returns `LandlockOutcome::Unavailable`.

**Why P2:** Called once per daemon connection in
`daemon/src/daemon/sections/module_access/transfer.rs:621`. The daemon
logs the unavailability and continues. The SEC-1 `*at` syscall defense
layer remains active as the sole defense on non-Linux.

**Windows equivalent:** Several options exist:
- Win32 restricted tokens (`CreateRestrictedToken`)
- AppContainer (`CreateAppContainerProfile`)
- Integrity levels (low-integrity mandatory labels)

None are direct equivalents. All require significant design work. The
daemon's directory sandbox (`DirSandbox`) uses `*at` syscalls on Unix and
is not compiled on Windows (`#[cfg(unix)]`), so the defense-in-depth
picture on Windows is different regardless.

**Impact:** Security hardening, not correctness or throughput.

#### 5. mmap_reader (mmap_reader_stub.rs)

**What the stub does:** `MmapReader::open()` reads the entire file into a
`Vec<u8>` instead of memory-mapping it.

**Why P2:** Used by the checksums crate for parallel file hashing
(`checksums/src/parallel/files.rs:39,229,330`) and by the transfer crate
for basis file mapping (`transfer/src/map_file/mmap.rs:38`). The Vec-based
fallback is correct but:

- Doubles memory usage for large files (file data in both page cache and
  heap)
- Does not benefit from OS-level paging (entire file loaded at open time)
- Cannot use `madvise` equivalent hints

**Windows equivalent:** `CreateFileMappingW` + `MapViewOfFile`. The
`memmap2` crate already supports Windows and could back a Windows-native
`MmapReader`. Alternatively, Rust's `std::fs::File` with memory-mapped I/O
via the `windows` crate.

**Impact:** Memory overhead on large files, not correctness. Noticeable
when checksumming files larger than available RAM.

### P3: Low Priority - No-ops or Already Handled

#### 6. `copy_file_range` / `copy_file_contents` (copy_file_range.rs)

**What the stub does:** `try_copy_file_range()` returns
`Err(Unsupported)`, `try_io_uring_copy()` returns `Err(Unsupported)`.
The top-level `copy_file_contents()` falls through to the read/write
fallback.

**Why P3:** The read/write fallback in `copy_file_contents_readwrite()`
uses a 256 KB buffer and is functionally correct. On Windows, the
`platform_copy` module already provides `CopyFileExW` with
`COPY_FILE_NO_BUFFERING` for whole-file local copies, and ReFS reflink for
CoW clones. The `copy_file_contents` function is used by the engine's
transfer path (`engine/src/local_copy/context_impl/transfer.rs:292`) but
only for file-to-file copies in the local-copy executor - a path that
already dispatches through `platform_copy` for whole-file operations.

**Impact:** The fallback is correct and competitive. Windows-specific
optimizations are already present in the `platform_copy` module.

#### 7. splice / `recv_fd_to_file` (splice/syscalls.rs:357)

**What the stub does:** Returns `Err(Unsupported)` on non-unix.

**Why P3:** Raw fd-to-fd transfer is fundamentally a Unix concept.
`recv_fd_to_file` is not called from any production code path - the
receiver uses the `Writer` enum in `disk_commit/writer.rs` which dispatches
to `ReusableBufWriter` (all platforms), `IoUring` (Linux), `Iocp`
(Windows), `Macos`, or `Vmsplice` (Linux). The IOCP `Writer::Iocp` variant
is already the Windows fast path.

**Impact:** None. The stub returns an explicit error, not a silent no-op.
No Windows equivalent needed because the data path is already served by
IOCP.

#### 8. vmsplice / `VmspliceFileWriter` (vmsplice_writer.rs)

**What the stub does:** Constructor returns `Err(Unsupported)`.

**Why P3:** The `Writer::Vmsplice` variant is gated behind
`#[cfg(all(target_os = "linux", feature = "vmsplice"))]`. The enum variant
does not exist on Windows at all. The stub type in `vmsplice_writer.rs`
exists only so library-level code compiles cross-platform.

**Impact:** None. Not reachable from any Windows code path.

#### 9. io_uring stub (io_uring_stub/)

**What the stub does:** Provides type-compatible stubs for the entire
io_uring API surface (73 KB of code). `is_io_uring_available()` returns
false. All operation functions return `Err(Unsupported)`.

**Why P3:** Windows has a full IOCP implementation in `iocp/` that is
already wired into the `Writer::Iocp` variant and the `IocpDiskBatch` used
by the disk-commit thread. The io_uring stub exists purely for
cross-platform compilation; the IOCP path is the production backend.

**Impact:** None. The 73 KB stub size is a maintenance cost tracked
separately by the io_uring_stub_size memory note, but has no runtime
impact.

## Action Items

| ID | Priority | Action | Effort |
|----|----------|--------|--------|
| WIN-S.8.1 | **P1** | Replace `send_file_to_fd` `io::sink()` with `Err(Unsupported)` | Small (< 1 hour) |
| WIN-S.8.2 | P2 | Implement `MmapReader` via `CreateFileMappingW` / `memmap2` | Medium (2-4 hours) |
| WIN-S.8.3 | P2 | Evaluate `FILE_FLAG_DELETE_ON_CLOSE` for anonymous temp files | Medium (4-8 hours, design needed) |
| WIN-S.8.4 | P2 | Document Landlock Windows alternatives in SEC-1 design | Small (1-2 hours) |
| WIN-S.8.5 | P2 | Benchmark `copy_basis_range` fallback vs kernel copy on Windows | Small (1-2 hours) |
| WIN-S.8.6 | P3 | No action needed for splice, vmsplice, io_uring, copy_file_range stubs | N/A |

## Key Finding: io::sink() Severity Downgrade

The WIN-S.1 assessment classified `send_file_to_fd` writing to `io::sink()`
as a **CRITICAL correctness bug**. This investigation downgrades it to P1
because:

1. **No production caller exists.** Exhaustive grep of `transfer`,
   `engine`, `daemon`, `core`, and `protocol` crates confirms zero imports
   of `send_file_to_fd` or `send_file_to_fd_with_policy`.

2. **The Windows TransmitFile implementation exists but is also unwired.**
   `iocp/transmit_file.rs` provides a correct `try_transmit_file()` but
   no production code calls it.

3. **The receiver-side write path already works.** The disk-commit thread
   uses `Writer::Iocp` (IOCP `IocpDiskBatch`) on Windows, completely
   bypassing the sendfile/splice APIs.

4. **The local-copy path already works.** `platform_copy/dispatch.rs`
   dispatches to `CopyFileExW` and ReFS reflink on Windows, bypassing
   `copy_file_range` and `sendfile`.

The stub is a **latent defect** - dangerous only if someone wires the
sender zero-copy path without checking the Windows fallback. The P1 fix
(changing `io::sink()` to `Err(Unsupported)`) eliminates this risk with
a one-line change.

## Windows Transfer Path Summary

For reference, this is how data actually flows on Windows today:

```
Sender -> wire:
  Protocol multiplexing writes data through std::io::Write
  No zero-copy send path is wired (sendfile/TransmitFile unused)

Wire -> Receiver (remote transfer):
  disk_commit thread selects Writer variant:
    - Writer::Iocp  (iocp feature on, IOCP available)
    - Writer::Buffered (fallback: ReusableBufWriter with 256 KB buffer)
  Delta COPY tokens: copy_basis_range returns Ok(0), fallback read+write
  Delta LITERAL tokens: written through Writer::write_chunk

Local copy:
  platform_copy::DefaultPlatformCopy dispatches:
    1. ReFS FSCTL_DUPLICATE_EXTENTS (if ReFS detected)
    2. CopyFileExW (with COPY_FILE_NO_BUFFERING for > 4 MB)
    3. std::fs::copy (fallback)

File creation strategy:
  O_TMPFILE probe returns false -> named temp file via NamedTempFile
```

All paths are functionally correct. The P1 item prevents a future
regression; the P2 items improve performance or safety margins.
