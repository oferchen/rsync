# Windows Equivalent of copy_file_range

## Problem

On Linux, `copy_file_range(2)` provides zero-copy kernel-side file-to-file
transfer. oc-rsync uses it in two distinct call sites:

1. **Whole-file copy** (`copy_file_range.rs`): `copy_file_contents()` uses
   `copy_file_range` as tier 2 (after io_uring) for files >= 64 KB.
2. **Delta-apply COPY token** (`copy_basis_range.rs`): `copy_basis_range()`
   copies `[basis_off..basis_off+len]` into `[dest_off..dest_off+len]`
   during delta reconstruction, avoiding userspace bounce.

On Windows, both stubs return `Unsupported` / `Ok(0)`, forcing the generic
read/write fallback. This document maps each call site to its Windows
equivalent and defines the implementation plan.

## Call-site Mapping

### 1. Whole-file copy (`copy_file_contents`)

| Linux tier | Windows equivalent | Status |
|---|---|---|
| io_uring | IOCP (separate feature) | Existing stub |
| `copy_file_range` | `CopyFileExW` | **Already implemented** in `copy_file_ex.rs` |
| read/write | read/write | Existing fallback |

`CopyFileExW` is path-based, not fd-based, so it cannot directly substitute
into the `copy_file_contents(&File, &File, u64)` signature. The path-based
whole-file copy is already wired in `platform_copy/dispatch.rs` for the
engine's local-copy path. No change needed here - callers that have paths
use the platform copy trait; callers that only have fds use the read/write
fallback which is adequate.

### 2. Delta-apply COPY token (`copy_basis_range`)

| Linux mechanism | Windows equivalent | Availability |
|---|---|---|
| `copy_file_range` | `ReadFile`/`WriteFile` with `OVERLAPPED` | All Windows |
| Same-FS reflink via `copy_file_range` | `FSCTL_DUPLICATE_EXTENTS_TO_FILE` | ReFS only |

This is the primary gap. The delta applicator calls `copy_basis_range(basis,
basis_off, dest, dest_off, len)` for COPY tokens. On Windows, this returns
`Ok(0)` forcing a `map_ptr` + `write_all` round-trip through userspace. We
implement a Windows path using `ReadFile`/`WriteFile` with `OVERLAPPED`
offset structs to perform the copy in kernel-buffered mode without seeking
the file handles.

## Windows API Selection

### Primary: `ReadFile`/`WriteFile` with `OVERLAPPED` (NTFS + ReFS)

Works on all Windows filesystems. Uses `OVERLAPPED` structs to specify
offsets without moving the file pointer, matching the `copy_file_range`
contract of not advancing file positions.

Advantages:
- Available on all Windows versions (XP+)
- Works with NTFS, ReFS, FAT32, network shares
- No alignment requirements
- Preserves file positions (uses OVERLAPPED offsets)

Implementation: chunk loop with 256 KB buffer, capped at `len`.

### Optional fast path: `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (ReFS only)

Block-level CoW clone for range copies on ReFS. O(1) regardless of range
size. Already implemented for whole-file reflink in
`platform_copy/dispatch.rs::try_refs_reflink_range_impl`.

Constraints:
- ReFS only (Windows Server 2016+ or Windows 10+ with ReFS volume)
- Cluster-aligned offsets and byte count required
- Source and destination must be on the same volume

Not wired into `copy_basis_range` in this change because the delta
applicator operates on `&File` handles without path information, and the
ReFS detection API (`is_refs_filesystem`) requires a path. A future change
could thread path info through the applicator to enable this optimization.

### Not applicable: `CopyFileExW`

`CopyFileExW` is path-based and copies whole files. It cannot copy a
sub-range from one file to another at specific offsets, so it is not
suitable for the delta-apply COPY token path. Already used for whole-file
copies via `platform_copy/dispatch.rs`.

## Implementation Plan

### Files Modified

1. **`crates/fast_io/src/copy_basis_range.rs`**
   - Add `#[cfg(windows)]` module `imp` alongside the existing Linux and
     non-Linux modules
   - Implement `copy_basis_range` using `ReadFile`/`WriteFile` with
     `OVERLAPPED` offset structs
   - Implement `copy_file_range_supported()` returning `true` on Windows
     (the ReadFile/WriteFile path is always available)

2. **`crates/transfer/src/delta_apply/applicator.rs`**
   - Update `resolve_same_fs` to return `SameFs` on Windows (the
     ReadFile/WriteFile path works cross-volume, unlike `copy_file_range`)

### API Surface

No public API changes. `copy_basis_range()` and
`copy_file_range_supported()` retain their existing signatures and
semantics. The Windows implementation is an internal detail behind the
existing `#[cfg]` gates.

### Testing

- Existing cross-platform tests in `copy_basis_range.rs` exercise the
  non-Linux stub path; these now exercise the Windows `ReadFile`/`WriteFile`
  path on Windows CI
- The `empty_len_returns_zero_without_syscall` test validates the zero-length
  short-circuit on all platforms
- Windows CI (already required) validates the implementation end-to-end

## Future Work

- Thread path information through `DeltaApplicator` to enable ReFS reflink
  range detection for COPY tokens
- Profile `ReadFile`/`WriteFile` with `OVERLAPPED` vs the existing
  `map_ptr` + `write_all` path to quantify the improvement on Windows
