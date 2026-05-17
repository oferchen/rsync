# Windows CopyFileEx Fast Path - Shipped Status (#1414, #1749)

Status: Shipped. Owner: fast_io. Closes #1414 and #1749.

This note records the final wiring of the Windows `CopyFileExW` fast path
and confirms parity with the Linux `copy_file_range` analogue. It
supersedes the planning notes in `windows-copyfileex-impl.md` and
`windows-copyfileex-platform-copy.md`.

## What shipped

The Windows fast path is live in two coordinated surfaces.

### 1. Standalone safe wrapper

`crates/fast_io/src/copy_file_ex.rs` exposes a safe `try_copy_file_ex(src,
dst) -> io::Result<u64>` that mirrors the shape of
`fast_io::copy_file_range::copy_file_contents` (the Linux analogue used in
#1749). The wrapper:

- Encodes paths as null-terminated UTF-16 via `OsStrExt::encode_wide`.
- Stats the source to derive `file_size` and sets the
  `COPY_FILE_NO_BUFFERING` flag (`0x0000_0008`) when the size exceeds
  `NO_BUFFERING_THRESHOLD` (4 MiB), matching the Microsoft guidance for
  large sequential copies.
- Calls `windows_sys::Win32::Storage::FileSystem::CopyFileExW` inside a
  single `#[allow(unsafe_code)]` module (`ffi`) and surfaces a safe
  function to the rest of the crate.
- Returns the destination size as the copied byte count
  (`CopyFileExW` does not report it directly).
- On non-Windows, returns `io::ErrorKind::Unsupported` so callers can
  fall back to portable copy methods - same contract as
  `try_copy_file_range` on non-Linux.

### 2. Dispatch integration

`crates/fast_io/src/platform_copy/dispatch.rs::platform_copy_impl`
(Windows branch) drives the full fallback chain:

1. `is_refs_filesystem(dst.parent())` probes ReFS support.
2. If ReFS, `try_refs_reflink_impl` attempts
   `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (instant CoW clone).
3. Otherwise (or on reflink failure), `try_copy_file_ex` is invoked
   with `use_no_buffering = size_hint > 4 MiB`.
4. On `CopyFileExW` failure, the partial destination is removed and
   `std::fs::copy` runs as the portable fallback, recorded as
   `CopyMethod::StandardCopy`.

The dispatch reports the method via `CopyResult { bytes_copied, method }`
so callers can attribute stats and feed adaptive strategies.

### 3. Public types

`crates/fast_io/src/platform_copy/types.rs`:

- `CopyMethod::CopyFileEx` - the Windows variant, with `Display` impl
  rendering `"CopyFileExW"`.
- `CopyMethod::ReFsReflink` - the upper tier on ReFS volumes.
- `CopyMethod::StandardCopy` - the portable fallback.
- `CopyResult::is_zero_copy()` returns `false` for `CopyFileEx` (the
  call still moves bytes through the kernel even with no-buffering) and
  `true` for `ReFsReflink`.

Re-exported from `crates/fast_io/src/lib.rs`:

```rust
pub mod copy_file_ex;
pub mod platform_copy;
pub use platform_copy::{CopyMethod, CopyResult, DefaultPlatformCopy, PlatformCopy, ...};
```

## Test coverage

Coverage is split across the two surfaces.

### `copy_file_ex.rs` unit tests

- `test_try_copy_file_ex_nonexistent_src` - error on missing source
  (runs on all platforms; on non-Windows the `Unsupported` error
  satisfies the assertion).
- `test_try_copy_file_ex_returns_unsupported` (non-Windows) -
  confirms the cross-platform contract.
- `test_try_copy_file_ex_copies_content` (Windows) - verifies
  byte-exact copy of a non-empty file.
- `test_try_copy_file_ex_empty_file` (Windows) - verifies
  zero-byte copy succeeds and creates the destination.
- `test_no_buffering_threshold_value` - asserts the 4 MiB constant.

### `platform_copy/tests.rs`

- `copy_method_display` - asserts
  `CopyMethod::CopyFileEx.to_string() == "CopyFileExW"`.
- `copy_method_is_zero_copy` - confirms `CopyFileEx` is not classified
  as zero-copy.
- `preferred_method_large_file` (Windows) - large size hint maps to
  `CopyMethod::CopyFileEx`.
- `preferred_method_small_file` (Windows) - small size hint maps to
  `CopyMethod::StandardCopy`.
- `dispatch_falls_back_from_reflink_on_ntfs` (Windows) - end-to-end:
  on NTFS, `DefaultPlatformCopy::copy_file` succeeds and reports either
  `CopyFileEx` or `StandardCopy`, with byte-exact destination contents.
- `refs_reflink_fails_gracefully_on_ntfs` (Windows) - the upper tier
  fails cleanly so the dispatch can fall through.

The Windows-gated tests run on the `windows-msvc` and `windows-gnu`
matrix entries in CI; the non-Windows variants run on every Linux and
macOS job.

## Parity with `copy_file_range`

| Concern | Linux `copy_file_range` | Windows `CopyFileEx` |
|---|---|---|
| Safe wrapper module | `copy_file_range.rs` | `copy_file_ex.rs` |
| Tiered dispatch entry | `platform_copy_impl` (Linux) | `platform_copy_impl` (Windows) |
| Upper tier | `FICLONE` (CoW) | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (ReFS CoW) |
| Middle tier | `copy_file_range` syscall | `CopyFileExW` syscall |
| Portable fallback | `std::fs::copy` | `std::fs::copy` |
| Size hint threshold | 64 KiB for `copy_file_range` | 4 MiB for `COPY_FILE_NO_BUFFERING` |
| `CopyMethod` enum variant | `CopyFileRange` | `CopyFileEx` |
| Cross-platform `Unsupported` | yes | yes |
| Reflink capability flag | `platform_supports_reflink` -> true | `platform_supports_reflink` -> true |

Both branches share the same `PlatformCopy` trait dispatch, the same
`CopyResult` shape, and the same fallback semantics. No call site in
`core`, `engine`, or `transfer` needs to be aware of the platform.

## What is intentionally out of scope

The two planning notes proposed additional work that has not shipped and
is tracked separately:

- **Progress callback wiring.** The current call passes `None` for
  `LPPROGRESS_ROUTINE`. The receiver's `--progress` line therefore only
  updates on completion of each `CopyFileExW` call, not mid-copy.
  Tracked under the broader Windows progress-pump issue, not part of
  the fast-path scope.
- **`COPY_FILE_RESTARTABLE`.** Not enabled. Resume semantics are
  handled by the rsync delta/temp-file commit path; the
  `CopyFileExW` step is whole-file copy of a not-yet-existing
  destination, so the restartable sidecar would add cost without
  benefit.
- **`CopyFile2` migration.** Proposed in
  `windows-copyfileex-platform-copy.md` as a forward-compatible
  replacement that takes a `COPYFILE2_EXTENDED_PARAMETERS` struct.
  Deferred until a concrete `COPY_FILE_*` flag is needed that
  `CopyFileExW` cannot pass.
- **`windows-rs` migration of the FFI.** The single unsafe call lives
  in a 30-line `ffi` module behind a safe API. Migration to the
  higher-level `windows` crate is tracked alongside the ReFS reflink
  migration (#1389); it does not change behaviour.

## Conclusion

Both #1414 (Windows `CopyFileExW` fast path) and #1749 (parity with the
Linux `copy_file_range` fast path) are complete. The Windows dispatch
chain matches the Linux and macOS structure, is exercised by
Windows-gated tests in CI, and exposes the same public surface
(`CopyMethod`, `CopyResult`, `PlatformCopy`) used by the rest of the
workspace. No further code changes are required for these issues.
