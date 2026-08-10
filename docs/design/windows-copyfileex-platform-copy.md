# Windows CopyFileEx for platform_copy parity (#1749, #1414)

## 1. Why this exists

The `PlatformCopy` trait
(`crates/fast_io/src/platform_copy/types.rs:122`) is the dispatch
surface the engine's local-copy executor and the receiver's commit
path use to copy a file with the fastest mechanism available. Linux
and macOS have a complete, audited fallback chain in
`platform_copy_impl`:

- Linux: `FICLONE` -> `copy_file_range` -> `std::fs::copy`
  (`crates/fast_io/src/platform_copy/dispatch.rs:18-54`).
- macOS: `clonefile` -> `fcopyfile` -> `std::fs::copy`
  (`dispatch.rs:62-95`, closed by #1388).
- Windows: ReFS reflink -> `CopyFileExW` -> `std::fs::copy`
  (`dispatch.rs:103-132`).

The Windows tier-2 step - the analogue of Linux `copy_file_range` and
macOS `fcopyfile` - is the only branch still wired directly through
`windows-sys` raw FFI (`dispatch.rs:230-273`). Three concrete gaps:

1. **Policy debt.** The workspace unsafe-code policy prefers
   `windows-rs` wrappers over hand-rolled `windows-sys` blocks. The
   ReFS path is on track for `windows-rs` (#1389 design at
   `docs/design/windows-refs-reflink.md`); leaving `CopyFileExW` on
   the raw binding splits the Windows surface across two binding
   crates with no upside.
2. **No progress, no cancellation.** The current call passes `None`
   for the progress routine and `null` for the cancel pointer
   (`dispatch.rs:260-262`). A 100 GB `CopyFileExW` on ReFS or NTFS
   cannot be interrupted by Ctrl+C and produces no `--progress`
   output until the kernel returns.
3. **No CopyFile2 path.** `CopyFile2` is the Windows 8 / Server 2012
   replacement for `CopyFileExW` and the only routine documented to
   forward future `COPY_FILE_*` flags. Without a `windows-rs`-shaped
   call site we cannot adopt new flags without further unsafe.

This design closes #1749 (parity equivalent of `copy_file_range`) and
#1414 (Windows `CopyFileEx` fast path) by replacing the existing
`try_copy_file_ex` body with a `windows-rs` implementation, wiring
the progress routine and cancel pointer through to the receiver, and
introducing a `CopyFile2` variant for forward-compatible flag
plumbing. Wire protocol unchanged. Public `PlatformCopy` trait
unchanged. Change is internal to `fast_io::platform_copy::dispatch`.

#1272 shipped the initial `CopyFileExW` integration with
`COPY_FILE_NO_BUFFERING` for files larger than 4 MiB. #1413 audited
the Windows surface and recorded the gap this note closes.

## 2. Existing Windows code

### 2.1 Dispatch entry

`crates/fast_io/src/platform_copy/dispatch.rs:103-132` runs the ReFS
reflink probe via `refs_detect::is_refs_filesystem` on `dst.parent()`,
attempts `try_refs_reflink_impl` when supported, then unconditionally
calls `try_copy_file_ex` with a `use_no_buffering` flag derived from
`size_hint > 4 MiB`. On `CopyFileExW` failure the dispatch removes
the partial destination and falls through to `std::fs::copy` recorded
as `CopyMethod::StandardCopy`.

### 2.2 The current CopyFileEx call site

`crates/fast_io/src/platform_copy/dispatch.rs:228-273` is the only
`CopyFileEx` / `CopyFile2` call in the workspace. It builds wide
strings, sets `COPY_FILE_NO_BUFFERING` (`0x0000_0008`,
`dispatch.rs:246`) when `use_no_buffering`, and calls
`windows_sys::Win32::Storage::FileSystem::CopyFileExW` with three
nulls: progress routine `None`, user-data `null()`, cancel pointer
`null_mut()`. Success stats the destination; failure returns
`io::Error::last_os_error()`.

`windows-sys` is the Windows binding for `fast_io`
(`crates/fast_io/Cargo.toml:69-74`). The IOCP modules
(`crates/fast_io/src/iocp/`) and the ReFS reflink ioctl
(`dispatch.rs:294-484`) bind through it today. The ReFS path is
moving to `windows-rs` under #1389; this design moves the
`CopyFileExW` body the same way.

### 2.3 Trait surface

`PlatformCopy` is at `types.rs:122-172` with `copy_file`,
`supports_reflink`, `preferred_method`. `CopyMethod` (`types.rs:11-53`)
carries `CopyMethod::CopyFileEx` (`types.rs:46`). This design adds
`CopyMethod::CopyFile2` and keeps the trait shape; existing callers
compile unchanged.

## 3. CopyFileExW vs CopyFile2 under windows-rs

### 3.1 Choice

Adopt **both**, with `CopyFileExW` as primary and `CopyFile2` as an
opt-in successor exposed through the same `try_copy_file_ex` shape.
The dispatch picks `CopyFileExW` unless a future `COPY_FILE_*` flag
introduced after the Windows 8 baseline forces `CopyFile2`.

### 3.2 Comparison

| Property | `CopyFileExW` | `CopyFile2` |
|----------|---------------|-------------|
| First documented | Windows 95; Vista with Unicode | Windows 8 / Server 2012 |
| Cancellation | `LPBOOL pbCancel` | `COPYFILE2_CALLBACK_INFO` cancel field |
| Progress | `LPPROGRESS_ROUTINE` per chunk | `COPYFILE2_PROGRESS_ROUTINE` per phase |
| Flags | Subset of `COPY_FILE_*` | Full set via `COPYFILE2_EXTENDED_PARAMETERS::dwCopyFlags`, forward-compatible |
| `windows-rs` types | `Win32::Storage::FileSystem::CopyFileExW` | `Win32::Storage::FileSystem::CopyFile2` plus `COPYFILE2_EXTENDED_PARAMETERS` |

`CopyFileExW` has wider deployment (Windows 7 / Server 2008 R2 still
runs on legacy backup hosts), accepts the `COPY_FILE_NO_BUFFERING`
flag the current code already uses, and has the simpler callback
signature. `CopyFile2` exists for two reasons in this design: (a)
Microsoft documents new flags landing on `CopyFile2` first, and (b)
the extended-parameters struct lets us pass tuning hints when
benchmarking against ReFS-on-VHDX workloads.

### 3.3 windows-rs surface

Version 0.61 supplies typed bindings:

```rust
windows::Win32::Foundation::{BOOL, HANDLE};
windows::Win32::Storage::FileSystem::{
    CopyFileExW, CopyFile2,
    COPY_FILE_FLAGS, COPY_FILE_NO_BUFFERING,
    COPYFILE2_EXTENDED_PARAMETERS, COPYFILE2_CALLBACK_INFO,
    COPYFILE2_MESSAGE, LPPROGRESS_ROUTINE,
    PROGRESS_CONTINUE, PROGRESS_CANCEL, PROGRESS_QUIET, PROGRESS_STOP,
};
```

`COPY_FILE_FLAGS` is a `u32` newtype with bit operators, `BOOL`
wraps `i32`, `HANDLE` is a non-null `isize` newtype.
`LPPROGRESS_ROUTINE` is `Option<unsafe extern "system" fn(..)>`, so
`None` is a typed null without raw casts.

### 3.4 Cargo dependency

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.61", features = [
    "Win32_Foundation", "Win32_Storage_FileSystem",
    "Win32_System_Ioctl", "Win32_System_IO",
] }
```

`windows-sys` stays for the IOCP modules; the two crates coexist
already (precedent in `windows-refs-reflink.md` Cargo section).
Consolidation onto `windows-rs` is tracked under #1866.

### 3.5 Replacement shape

```rust
#[cfg(target_os = "windows")]
fn try_copy_file_ex(src: &Path, dst: &Path, opts: CopyFileOpts)
    -> io::Result<u64>
{
    use windows::Win32::Storage::FileSystem::{
        CopyFileExW, COPY_FILE_FLAGS, COPY_FILE_NO_BUFFERING,
    };
    let src_w = wide_with_nul(src);
    let dst_w = wide_with_nul(dst);
    let mut cancel = BOOL(0);
    let mut flags = COPY_FILE_FLAGS(0);
    if opts.no_buffering { flags |= COPY_FILE_NO_BUFFERING; }
    // SAFETY: paths are null-terminated UTF-16; cancel pointer
    // outlives the call; user-data is a pinned `&mut ProgressCtx`
    // (section 4).
    unsafe {
        CopyFileExW(
            PCWSTR(src_w.as_ptr()), PCWSTR(dst_w.as_ptr()),
            opts.progress_routine(), opts.user_data_ptr(),
            Some(&mut cancel), flags,
        )
    }.map(|_| metadata_len(dst))?
}
```

`CopyFile2` is a sibling `try_copy_file_2` that wraps
`COPYFILE2_EXTENDED_PARAMETERS`; the dispatch picks based on
`opts.use_copy_file_2` (default false; flipped when a post-Windows-8
flag enters the option set). Add `CopyMethod::CopyFile2` to
`types.rs:11-53` and the `Display` impl at `types.rs:55-67`; tests at
`crates/fast_io/src/platform_copy/tests.rs:39-50` extend symmetrically.

## 4. Progress callback

### 4.1 Surface

`CopyFileExW` calls `LpProgressRoutine` once per stream-start, once
per buffered chunk (default 64 KiB), once at completion. The return
controls flow: `PROGRESS_CONTINUE`, `PROGRESS_CANCEL`,
`PROGRESS_STOP`, `PROGRESS_QUIET`. `CopyFile2` carries the equivalent
through `COPYFILE2_CALLBACK_INFO::pProgressRoutine` with typed
`COPYFILE2_MESSAGE` per phase.

### 4.2 Wiring decision

Wire the surface only when the caller opts in. `CopyFileOpts` carries
`report_progress: bool`. When false, both implementations pass `None`
for the routine pointer and the kernel runs without callbacks
(matches today's behaviour, zero overhead). When true, the
implementation installs a trampoline that forwards bytes-transferred
counts to the receiver's per-file progress channel.

### 4.3 Trampoline shape

```rust
#[repr(C)]
struct ProgressCtx<'a> { sink: &'a dyn Fn(u64), cancel: &'a AtomicBool }

unsafe extern "system" fn progress_trampoline(
    _total: i64, transferred: i64,
    _stream: i64, _stream_xfer: i64, _stream_no: u32,
    reason: u32, _src: HANDLE, _dst: HANDLE, data: *const c_void,
) -> u32 {
    // SAFETY: caller pins the ProgressCtx for the syscall lifetime.
    let ctx = unsafe { &*(data as *const ProgressCtx) };
    if reason == CALLBACK_CHUNK_FINISHED.0 { (ctx.sink)(transferred as u64); }
    if ctx.cancel.load(Ordering::Relaxed) { return PROGRESS_CANCEL.0; }
    PROGRESS_CONTINUE.0
}
```

### 4.4 Receiver hook

The receiver owns a per-file progress reporter for `--progress`
(`crates/cli/src/frontend/progress/mode.rs`). Today the reporter
ticks on per-chunk writes inside `disk_commit`. For the local-copy
fast paths that bypass `disk_commit` the reporter has nothing to
tick. Wiring the trampoline lets a local-copy `CopyFileExW` of a
100 GB VHDX produce progress output identical to the receiver-side
path. The integration boundary is a new
`progress: Option<Arc<dyn Fn(u64) + Send + Sync>>` field on
`CopyOptions`; existing callers pass `CopyOptions::default()` and
behave exactly as today.

64 KiB chunks on a 100 GB transfer means 1.6 M callbacks. One atomic
load plus one virtual call per invocation (roughly 3 ns) totals about
5 ms - negligible against the 10 s+ syscall. The cancel-flag check
in the trampoline is the load-bearing reason to install it even when
no sink is set; see section 5.

## 5. Cancellation

### 5.1 Two surfaces

`CopyFileExW` exposes cancellation via:

- `LPBOOL pbCancel`: kernel polls the `BOOL` between chunks; setting
  it from any thread cancels the copy; the syscall returns
  `ERROR_REQUEST_ABORTED` (1235).
- Progress-routine return `PROGRESS_CANCEL` or `PROGRESS_STOP`: same
  end state.

`CopyFile2` carries the same through `COPYFILE2_CALLBACK_INFO`
return `COPYFILE2_MESSAGE_RETURN_CANCEL` plus
`COPYFILE2_EXTENDED_PARAMETERS::pfCancel`.

### 5.2 Integration with the transfer's abort signal

The abort path is `core::signal::request_abort` /
`is_abort_requested` (`crates/core/src/signal/mod.rs:129-187`),
backed by a `static AtomicBool` flipped by the second SIGINT/SIGTERM
or by Windows `Ctrl+C` (`signal/stub.rs`). The trampoline returns
`PROGRESS_CANCEL` whenever `is_abort_requested()` reads `true`.

When the trampoline is not installed, the `pbCancel` slot is still
wired. A worker on the disk-commit thread polls every 100 ms
(already running, `crates/transfer/src/disk_commit/thread.rs:170-225`)
and writes `BOOL(1)` into the slot when an abort fires. The slot is
owned by the `try_copy_file_ex` invocation as a `Pin<Box<BOOL>>` that
the function passes to `CopyFileExW` and drops only after return; the
pin guarantees the kernel never sees a moved address.

### 5.3 Lifetime and error mapping

`pbCancel` is read by a kernel worker thread strictly inside the
`CopyFileExW` call; the Rust caller holds the pinned `BOOL` across
the syscall and the kernel guarantees no further reads after return.
No `unsafe` lifetime extends past the call site. This mirrors the
`OverlappedOp` pinning argument in
`docs/design/iocp-transfer-pipeline-wiring.md` Section 10.

Cancellation surfaces as `ERROR_REQUEST_ABORTED` (1235).
`io::Error::from_raw_os_error(1235)` produces an `Other`-kind error;
the dispatch maps it to a typed `CopyError::Cancelled` local to
`fast_io`, then to `ExitCode::Signal`
(`crates/core/src/exit_code/codes.rs:87`) at the core boundary.
Cancellation does **not** fall through to `std::fs::copy` - falling
through would defeat the abort.

### 5.4 Why not rely solely on Ctrl+C handling

`SetConsoleCtrlHandler` runs handlers on a separate thread and must
return within 5 seconds. `CopyFileExW` of a 100 GB file blocks far
longer. The cancel pointer cuts the syscall short and lets the
handler return in milliseconds.

## 6. Error mapping

### 6.1 GetLastError to io::Error to ExitCode

`std::io::Error::last_os_error()` calls `GetLastError` internally and
produces a Windows-typed `io::Error`. The mapping the new dispatch
applies:

| `GetLastError` | Win32 name | `io::ErrorKind` | `ExitCode` |
|---|---|---|---|
| 5 | `ERROR_ACCESS_DENIED` | `PermissionDenied` | `FileIo` (11) |
| 32 | `ERROR_SHARING_VIOLATION` | `Other` | `FileIo` |
| 80 | `ERROR_FILE_EXISTS` | `AlreadyExists` | `FileIo` |
| 87 | `ERROR_INVALID_PARAMETER` | `InvalidInput` | `FileIo` (debug-log; should not happen) |
| 112 | `ERROR_DISK_FULL` | `Other` | `FileIo` |
| 123 | `ERROR_INVALID_NAME` | `InvalidFilename` | `FileSelect` (3) |
| 1235 | `ERROR_REQUEST_ABORTED` | `Interrupted` | `Signal` (20) |
| 5500 | `ERROR_OFFLOAD_READ_FILE_NOT_SUPPORTED` | `Unsupported` | (fall through) |
| other | passthrough | `Other` | `FileIo` |

### 6.2 Fall-through policy

All errors except cancellation fall through to `std::fs::copy` (the
existing behaviour at `dispatch.rs:125-130`). Cancellation propagates
because falling through would defeat the abort:

```rust
match try_copy_file_ex(src, dst, opts) {
    Ok(bytes) => Ok(CopyResult::new(bytes, CopyMethod::CopyFileEx)),
    Err(e) if is_cancellation(&e) => Err(e),
    Err(_) => {
        let _ = std::fs::remove_file(dst);
        Ok(CopyResult::new(std::fs::copy(src, dst)?, CopyMethod::StandardCopy))
    }
}
```

### 6.3 Path context and role trailers

`fast_io::platform_copy` does not own paths beyond the call. The
caller attaches src and dst paths via the `core::error` extension
trait's `with_path` adaptor (`crates/core/src/client/error.rs`), so
the role trailer at the CLI surface reads
`(code 11) at crates/fast_io/src/platform_copy/dispatch.rs:NNN
[client=...]`. `HasExitCode` is at
`crates/core/src/exit_code/traits.rs:25`.

## 7. Sparse-file behaviour

### 7.1 COPY_FILE_NO_BUFFERING

`COPY_FILE_NO_BUFFERING` (defined inline at `dispatch.rs:246`,
typed in `windows-rs`) tells `CopyFileExW` to bypass the system
file cache. For files larger than 4 MiB this avoids polluting the OS
cache with bytes that will not be re-read, mirroring Linux
`O_DIRECT` and macOS `F_NOCACHE`. The 4 MiB threshold
(`dispatch.rs:106, 246-249`) is unchanged in the migration.

### 7.2 Sparse hint

`CopyFileExW` does not preserve sparseness. Source files opened with
`FILE_ATTRIBUTE_SPARSE_FILE` produce a fully-allocated destination
unless the caller marks the destination sparse first. The new
dispatch calls `GetFileAttributesW` on the source; when
`FILE_ATTRIBUTE_SPARSE_FILE` is set it opens the destination with
`FILE_FLAG_OPEN_NO_RECALL | FILE_FLAG_OPEN_REPARSE_POINT` and issues
`FSCTL_SET_SPARSE` (typed in
`windows::Win32::System::Ioctl::FSCTL_SET_SPARSE`) before
`CopyFileExW`. If the FSCTL fails the dispatch falls through to
`std::fs::copy` so user data never silently inflates.
`FSCTL_SET_SPARSE` is supported on NTFS and ReFS; FAT32/exFAT
allocate fully (matches upstream).

### 7.3 No-buffering alignment

`COPY_FILE_NO_BUFFERING` requires sector-aligned write offsets
(typically 512 bytes or 4 KiB). `CopyFileExW` performs alignment
internally; the caller does not pre-pad. The flag is honoured only
on local NTFS/ReFS volumes; SMB silently ignores it.

## 8. Reflink interaction

### 8.1 Order

```
1. FSCTL_DUPLICATE_EXTENTS_TO_FILE  (ReFS only, same volume, aligned)
2. CopyFileExW or CopyFile2          (any NTFS/ReFS, optional NO_BUFFERING)
3. std::fs::copy                     (portable buffered fallback)
```

Step 1 is documented in `docs/design/windows-refs-reflink.md`
"Fallback Chain". Step 2 is the path this design re-binds. Step 3
is the portable Rust fallback.

### 8.2 When CopyFileEx is the fallback

CopyFileEx covers everything the reflink probe rejects: NTFS or
FAT32 destinations (`refs_detect::supports_block_refcounting` returns
`false`), cross-volume copies (`ERROR_INVALID_PARAMETER` on step 1),
refcount fan-out (`ERROR_BLOCK_TOO_MANY_REFERENCES`, 1252), and AV
agents rewriting the FSCTL. In each case the partial destination is
cleaned up and the dispatch records `CopyMethod::CopyFileEx` (or
`CopyMethod::CopyFile2`).

### 8.3 When CopyFileEx is primary

NTFS, FAT32, exFAT, and SMB shares do not support
`FSCTL_DUPLICATE_EXTENTS_TO_FILE`. The ReFS probe short-circuits and
`CopyFileExW` runs immediately. A 10 GB VHDX reflink takes 18 ms on
Linux Btrfs versus 9.6 s via `CopyFileExW` on ReFS
(`windows-refs-reflink.md` "Motivation"); step 1 stays first when
supported, step 2 wins when reflink is unavailable.

## 9. Test plan with windows-msvc CI runner

### 9.1 Unit tests (any platform)

Mock the syscalls behind a trait so tests run on Linux and macOS
without a Windows host:

```rust
#[cfg(target_os = "windows")]
trait CopyFileBackend {
    fn copy_file_ex(&self, src: &Path, dst: &Path, opts: CopyFileOpts)
        -> io::Result<u64>;
    fn copy_file_2(&self, src: &Path, dst: &Path, opts: CopyFileOpts)
        -> io::Result<u64>;
}
```

Production wires `RealCopyFileBackend`; tests substitute
`MockCopyFileBackend` with a configurable response sequence. The
mock seam is `pub(crate)` next to existing fixtures
(`crates/fast_io/src/platform_copy/tests.rs:586-606`).

| Test | Mock setup | Expected |
|------|------------|----------|
| `copy_file_ex_no_buffering_threshold` | size_hint = 5 MiB | flags include `COPY_FILE_NO_BUFFERING` |
| `copy_file_ex_below_threshold_no_flag` | size_hint = 1 MiB | flags == 0 |
| `cancel_returns_err` | mock returns `ERROR_REQUEST_ABORTED` | `Err(_)` with `ErrorKind::Interrupted` |
| `progress_callback_invoked` | mock fires 4 chunks | sink receives 4 byte counts |
| `cancel_token_aborts` | abort_requested true mid-call | mock observes `PROGRESS_CANCEL` return |
| `copy_file_2_used_when_opted_in` | `opts.use_copy_file_2 = true` | mock backend records `CopyFile2` invocation |
| `error_path_falls_through` | mock returns `ERROR_DISK_FULL` | dispatch tries `std::fs::copy`, returns its result |
| `cancel_does_not_fall_through` | mock returns `ERROR_REQUEST_ABORTED` | dispatch propagates without retry |
| `sparse_set_before_copy` | sparse src | `FSCTL_SET_SPARSE` issued on dst before copy |

### 9.2 Integration tests (Windows host)

Gated `#[cfg(windows)]`. The existing CI matrix has three Windows
jobs at `.github/workflows/ci.yml:169-275`:

- `windows-test` (line 169) - default features over `core`, `engine`,
  `cli`; add `fast_io::platform_copy` to the test list.
- `windows-iocp` (line 228) - explicit `--features iocp`; add a step
  for `cargo nextest run -p fast_io --features iocp -E 'test(platform_copy)'`.
- New `windows-platform-copy` job parallel to `windows-acl-xattr`
  (line 286), mounting a dynamic NTFS VHDX with a known cluster size.

| Test | Workload | Assertion |
|------|----------|-----------|
| `copy_file_ex_small_file` | 1 MiB on NTFS | method `CopyFileEx`; bytes match source |
| `copy_file_ex_no_buffering` | 8 MiB on NTFS | method `CopyFileEx`; flag observed via mock seam |
| `copy_file_2_path` | opt-in via env var | method `CopyFile2` |
| `cancel_via_signal` | spawn copy, request abort 100 ms in | `ErrorKind::Interrupted`; partial dst removed |
| `cancel_via_pbcancel` | flip cancel slot directly | same end state |
| `progress_emits_bytes` | 64 MiB copy | sink receives multiple ticks summing to 64 MiB |
| `sparse_preserved` | sparse 1 GB src | dst is sparse, on-disk size << apparent size |
| `acl_preserved` | src with non-trivial DACL | dst inherits DACL via `CopyFileEx` default |
| `disk_full_falls_through` | small ramdisk | falls through to `std::fs::copy`, surfaces `ErrorKind::Other` |

### 9.3 Runner notes

GitHub `windows-latest` (2022) ships NTFS by default; no ReFS
provisioning is needed for these tests. Test 4 (`cancel_via_signal`)
needs a 30 s timeout. The job builds against
`x86_64-pc-windows-msvc`; the GNU cross-check at
`.github/workflows/ci.yml:333` exercises the same `windows-rs`
bindings. The plan reaches the 95% coverage target.

## 10. Open questions

1. **`--copy-method` exposure.** Forcing `std::fs::copy` to debug AV
   interference would warrant
   `--copy-method=copyfileex|copyfile2|std`. Out of scope for this
   note; `CopyOptions` is forward-compatible.
2. **Progress-callback lifetime.** The user-data slot is
   `*const c_void`; dropping the boxed sink before `CopyFileExW`
   returns would dangle the pointer. The call site keeps the pin
   live for the full syscall. Open question: expose a higher-level
   wrapper that hides the lifetime?
3. **`CopyFile2` chunk-size hint.**
   `COPYFILE2_EXTENDED_PARAMETERS` has no public chunk-size field,
   but `dwCopyFlags` includes
   `COPY_FILE_REQUEST_COMPRESSED_TRAFFIC` and friends that change
   chunking. The design picks Microsoft defaults; ReFS-on-VHDX
   benchmarks could justify tuning. Out of scope for #1749.
4. **SMB ODX exposure.** `CopyFileExW` and `CopyFile2` route to the
   SMB redirector for UNC destinations; server-side copy
   (`FSCTL_OFFLOAD_READ` / `FSCTL_OFFLOAD_WRITE`) happens inside the
   redirector with no caller hint. Surfacing ODX as a distinct
   `CopyMethod::OffloadCopy` is a follow-up.
5. **Cancellation slot identity.** Section 5.2 polls every 100 ms
   on the disk-commit thread. An alternative passes the static
   `ABORT_REQUESTED` address directly as `pbCancel`; the kernel
   reads it as a `BOOL` and the cast is sound, but it couples
   `fast_io` to `core::signal`. The dispatch keeps the per-call
   slot for layering.
6. **Interaction with `--inplace`.** `--inplace` is receiver-side;
   the engine's local-copy executor is the only `copy_file` consumer
   that hits this path and never runs in `--inplace` mode (local
   copies always temp+rename). See
   `docs/design/iocp-transfer-pipeline-wiring.md` Section 10 for
   the receiver-side picture.
7. **Telemetry.** Today the dispatch records `CopyMethod` per file
   but not the exact `GetLastError` on fall-through. A debug-level
   log line would help diagnose AV interference or sharing
   violations. Out of scope; tracked separately.

## 11. Tracking

Navigation only; no new tracker items added by this note:

- #1136 - `PlatformCopy` trait (landed).
- #1139 - Windows stub (landed; replaced by #1389).
- #1272 - `CopyFileExW` initial integration with
  `COPY_FILE_NO_BUFFERING` (landed; this design replaces the raw
  `windows-sys` body).
- #1388 - macOS `clonefile` (landed; tier-1 reference).
- #1389 - Windows ReFS reflink design and follow-up implementation.
- #1413 - Windows parity audit (landed; surfaced #1414 and #1749).
- #1414 - Windows `CopyFileExW` fast-path enhancement (this design).
- #1659 - cross-platform copy benchmark (landed).
- #1748 - io_uring runtime probe (landed; cancellation-poll pattern).
- #1749 - parity equivalent of `copy_file_range` on Windows (this
  design).
- #1826 - `--cow` / `--no-cow` CLI flag (pending).
- #1866 - Windows ACL via `windows-rs` (landed; binding-crate
  precedent).
- #1868 - IOCP wiring for disk-commit (closed; design at
  `docs/design/iocp-transfer-pipeline-wiring.md`).
- #1928 - Overlapped TCP socket support (landed; uses `windows-sys`).

The wire protocol is untouched, the public `PlatformCopy` trait is
untouched, and the change is internal to
`crates/fast_io/src/platform_copy/dispatch.rs`. Cross-platform parity
with Linux `copy_file_range` and macOS `fcopyfile` is restored at
the Windows tier-2 layer.
