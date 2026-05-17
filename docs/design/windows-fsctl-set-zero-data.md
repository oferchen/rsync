# Windows FSCTL_SET_ZERO_DATA Hole Punching (#2131)

## Summary

oc-rsync writes sparse files on Linux via `fallocate(FALLOC_FL_PUNCH_HOLE)`
and falls back to "seek past zeros" or "write physical zeros" everywhere
else. The Windows arm currently writes zero bytes verbatim, which doubles
physical allocation for sparse-source files and prevents `--sparse-detect=`
(#1828) from producing real holes on NTFS. This document plans a Windows
backend that uses `FSCTL_SET_ZERO_DATA` to deallocate ranges in NTFS
sparse files, restoring parity with the Unix sparse semantics and feeding
the sparse-writer Decorator planned in #2132.

## 1. Current sparse handling

The receiver-side sparse pipeline lives in
`crates/engine/src/local_copy/executor/file/sparse/`:

- `writer.rs::SparseWriter` and `state.rs::SparseWriteState` accumulate
  consecutive zero bytes across `write()` calls into a single
  `pending_zeros` counter.
- `detect::leading_zero_run` / `trailing_zero_run` delegate to
  `fast_io::zero_detect`, which scans buffers using AVX2 (32 bytes per
  iter), SSE2 / NEON (16 bytes per iter), or a scalar `u128` fast path
  (also 16 bytes per iter). This is the "16-byte `u128` zero-run
  detection" referenced in the project guidelines.
- When non-zero data arrives, `flush_pending_zeros` issues a single
  `seek(SeekFrom::Current(pending as i64))`, maintaining the
  single-seek-per-zero-run invariant. The kernel materialises the gap as
  a hole on filesystems that support sparse files when the file is
  later extended past the gap.
- `state::SparseWriteState::flush_with_punch_hole` is the alternative
  path for in-place updates: it calls `hole_punch::punch_hole`, which on
  Linux issues `fallocate(PUNCH_HOLE | KEEP_SIZE)` and on every other
  platform (macOS, BSD, Windows) falls back to `write_zeros_fallback`,
  allocating disk space for the run.

The caller that detects a zero run and wants to "punch" instead of
"write zeros" is `write_sparse_chunk` (used during local copy) and the
delta-apply receiver (`crates/transfer/src/delta_apply.rs`) via its own
`SparseWriteState`. Both end at the same Linux-only `punch_hole`.

## 2. Linux analogue

```c
fallocate(fd,
          FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
          offset,
          len);
```

`PUNCH_HOLE | KEEP_SIZE` deallocates the byte range while preserving
the logical EOF. Supported on ext4, XFS, Btrfs, tmpfs. Returns
`EOPNOTSUPP` on filesystems that lack hole-punch support; `hole_punch.rs`
then tries `FALLOC_FL_ZERO_RANGE` and finally `write_zeros_fallback`.
This three-tier ladder mirrors upstream rsync's `do_punch_hole` in
`syscall.c`.

## 3. macOS analogue

```c
struct fpunchhole_t arg = { .fp_offset = offset, .fp_length = len };
fcntl(fd, F_PUNCHHOLE, &arg);
```

Available on APFS and HFS+ since macOS 10.13. Offsets and lengths must
be aligned to the filesystem block size; the kernel rejects misaligned
calls with `EINVAL` rather than zero-filling boundary blocks. Not wired
in oc-rsync today; documented here for cross-platform symmetry but out
of scope for this design.

## 4. Windows path: FSCTL_SET_ZERO_DATA

`DeviceIoControl(handle, FSCTL_SET_ZERO_DATA, ...)` punches holes inside
an NTFS sparse file. Payload:

```text
FILE_ZERO_DATA_INFORMATION {
    FileOffset:      i64,  // inclusive start, byte offset
    BeyondFinalZero: i64,  // exclusive end, byte offset
}
```

Pre-conditions enforced by NTFS:

- The handle must be opened with `FILE_WRITE_DATA` (a `GENERIC_WRITE`
  open satisfies this).
- The file must already carry `FILE_ATTRIBUTE_SPARSE_FILE`. Set it once
  at create time via `FSCTL_SET_SPARSE` (idempotent, but required before
  the first `FSCTL_SET_ZERO_DATA`).
- The volume must support sparse files (NTFS, ReFS with sparse enabled).
  FAT32, exFAT, and SMB shares without the sparse capability return
  `ERROR_INVALID_FUNCTION` or silently no-op.
- `FileOffset` and `BeyondFinalZero` are rounded to cluster size
  (typically 4 KiB) by the filesystem; partial clusters at the boundary
  are zero-filled rather than deallocated.

The control code values come from `windows_sys::Win32::System::Ioctl`:

```rust
const FSCTL_SET_SPARSE: u32     = 0x000900C4;
const FSCTL_SET_ZERO_DATA: u32  = 0x000980C8;
```

The existing `try_refs_reflink_impl` in
`crates/fast_io/src/platform_copy/dispatch.rs` is the working template
for `DeviceIoControl` plumbing through `windows-sys`.

## 5. API shape

Expose two thin wrappers in `fast_io`:

```rust
#[cfg(windows)]
pub trait PlatformSparse {
    /// Tag the file as sparse. No-op if already tagged or if the volume
    /// does not support sparse files.
    fn set_sparse_attr(handle: HANDLE) -> io::Result<()>;

    /// Punch [start, start + len) using FSCTL_SET_ZERO_DATA.
    fn punch_hole(handle: HANDLE, start: u64, len: u64) -> io::Result<()>;
}
```

`set_sparse_attr` calls `DeviceIoControl` with `FSCTL_SET_SPARSE` and
treats `ERROR_INVALID_FUNCTION` / `ERROR_NOT_SUPPORTED` as a graceful
no-op (returns `Ok(())`, sparse not available). `punch_hole` constructs
`FILE_ZERO_DATA_INFORMATION { FileOffset: start as i64,
BeyondFinalZero: (start + len) as i64 }` and dispatches the ioctl,
mapping the unsupported-volume error codes to a typed
`SparseUnsupported` error so the caller can fall back to zero-write.

## 6. Integration

Two options, both keep the existing Unix code paths intact:

1. **Extend `PlatformCopy`** with `set_sparse_attr` / `punch_hole`
   defaults that return `SparseUnsupported`, then override on Windows.
   Lowest churn.
2. **New `PlatformSparse` trait** alongside `PlatformCopy`. Cleaner
   separation when the sparse decorator (#2132) wraps an arbitrary
   `Write` and needs to call into platform sparse without dragging the
   copy dispatch table along. **Preferred.**

The Windows entry point lives in
`crates/fast_io/src/platform_sparse/windows.rs` (new module) and is
wired from the sparse decorator. Linux (`fallocate`) and macOS
(`F_PUNCHHOLE` if we add it later) keep their existing paths exposed
through the same trait for symmetry. Other platforms get a no-op stub
that returns `SparseUnsupported`, matching the existing fallback
behaviour.

## 7. Failure modes

- **Non-NTFS / non-ReFS volume**: FAT32, exFAT, network mounts without
  sparse semantics return `ERROR_INVALID_FUNCTION` (0x1) or
  `ERROR_NOT_SUPPORTED` (0x32). Detect by ioctl return code, not by
  `GetVolumeInformationW` flags (which are advisory and racy under
  network redirectors). Surface as `SparseUnsupported` and degrade to
  the existing zero-write path.
- **Missing `FSCTL_SET_SPARSE`**: NTFS silently zero-fills the requested
  range without deallocating, defeating the optimisation. The wrapper
  must always call `set_sparse_attr` before the first punch.
- **Insufficient permissions**: the handle must hold `FILE_WRITE_DATA`.
  Read-only handles fail with `ERROR_ACCESS_DENIED` (0x5). Map to
  `io::ErrorKind::PermissionDenied`.
- **Cluster misalignment**: ranges that do not align to the volume
  cluster size (default 4 KiB) still succeed, but the boundary clusters
  remain allocated and zero-filled. Callers should round inward when a
  zero run is large enough to span complete clusters; small runs below
  one cluster should fall through to plain writes.
- **Read-only or compressed files**: `FSCTL_SET_SPARSE` fails on
  compressed files (`FILE_ATTRIBUTE_COMPRESSED`) with
  `ERROR_INVALID_PARAMETER` (0x57). Skip the optimisation and write
  zeros.
- **Stale cached reads**: hole punching does not invalidate page-cache
  data already returned to readers. Tests must flush the destination
  handle (`FlushFileBuffers`) before assertions.

## 8. Performance benefit

- Physical disk reservation for sparse-source files drops from "every
  zero byte allocated" to "only data extents allocated", matching the
  Linux baseline.
- Backup tools (`robocopy /B`, `wbadmin`, vSphere CBT) and copy tools
  (`xcopy /B`, Explorer) see real holes and skip the deallocated
  regions, cutting backup time and storage proportionally to the sparse
  ratio.
- `--sparse-detect=map` becomes meaningful on Windows: the receiver can
  honour source-side holes instead of materialising zeros.
- Expected wins on representative VM disk image transfers (50 % sparse,
  16 GiB logical): ~8 GiB physical written today, ~0 GiB after this
  change. Disk write bandwidth ceases to be the bottleneck for sparse
  payloads; throughput becomes CPU-bound on the SIMD zero-detect path.

## 9. Recommendation

**Implement.**

The current behaviour silently breaks the user-visible promise of
`--sparse`: on Windows, a sparse-source file lands as a fully allocated
destination, which is functionally a regression versus upstream rsync
when used against a Cygwin / MSYS backend. The work is bounded
(one new module, one trait, well-trodden `DeviceIoControl` pattern from
`platform_copy/dispatch.rs`), the failure modes are well-understood, and
the gracious-fallback story is already in place.

Trigger conditions to defer:

- If #2132 (sparse writer Decorator) is rescoped or rejected, this
  design loses its caller and should be deferred until a replacement
  consumer lands.
- If CI cannot exercise the path on a real NTFS volume (the GitHub
  Windows runners do provide one; ReFS is not available), defer until
  an interop runner can validate the cluster-alignment edge cases.

Trigger conditions to reject:

- If usage telemetry shows `--sparse` is rarely used on Windows targets
  (currently unmeasured; would need #1828 to ship first and gather
  data).

## 10. Five-step implementation plan

1. **Add the trait.** Create `crates/fast_io/src/platform_sparse/`
   (`mod.rs`, `unix.rs`, `windows.rs`, `stub.rs`). Define
   `PlatformSparse` with `set_sparse_attr` and `punch_hole`. Export from
   `fast_io::lib`. Add `SparseUnsupported` to `fast_io` error types.
2. **Implement Linux.** Wrap the existing
   `fallocate(PUNCH_HOLE | KEEP_SIZE)` ladder behind the trait. Move
   `engine/src/local_copy/executor/file/sparse/hole_punch.rs` to
   delegate to `fast_io::PlatformSparse` so the Linux path keeps its
   three-tier fallback without duplication. No behavioural change.
3. **Implement Windows.** Add `windows.rs` using `windows-sys`
   `DeviceIoControl`. Constants: `FSCTL_SET_SPARSE = 0x000900C4`,
   `FSCTL_SET_ZERO_DATA = 0x000980C8`. Mirror the `unsafe` block
   structure of `try_refs_reflink_impl`. Map `ERROR_INVALID_FUNCTION`
   and `ERROR_NOT_SUPPORTED` to `SparseUnsupported`; all other Win32
   errors surface as `io::Error::from_raw_os_error`.
4. **Wire callers.** Update
   `engine/src/local_copy/executor/file/sparse/state.rs::flush_with_punch_hole`
   and the delta-apply `SparseWriteState` to call
   `PlatformSparse::punch_hole`. On `SparseUnsupported`, fall back to
   `write_zeros_fallback`. Tag the destination once on `O_CREAT` via
   `set_sparse_attr` from the receiver's tempfile-open path.
5. **Test.** Unit tests for cluster alignment, unsupported-volume
   simulation (mock `DeviceIoControl` via a thin indirection or test
   against a temp file on `%TEMP%` and assert `GetCompressedFileSize`
   shrinks). Integration test: receive a 64 MiB file containing two
   16 MiB zero runs into an NTFS tempdir, assert allocated size equals
   one cluster + data extents. Property test: random zero runs at
   random offsets, post-punch byte content must equal the pre-punch
   byte content.

## 11. Cross-references

- #1828 - `--sparse-detect=` CLI option, sparse policy surface.
- #2132 - Sparse writer Decorator that owns the cross-platform
  abstraction; this design is its Windows backend.
- `crates/fast_io/src/platform_copy/dispatch.rs` - existing pattern for
  per-OS fast paths plus graceful fallback (`try_refs_reflink_impl` is
  the closest template).
- `crates/engine/src/local_copy/executor/file/sparse/hole_punch.rs` -
  current Linux-only `punch_hole` and its three-tier fallback.
- `crates/engine/src/local_copy/executor/file/sparse/writer.rs` -
  `SparseWriter` decorator, the in-process consumer of the new trait.
- Upstream rsync: `syscall.c::do_punch_hole`, `fileio.c::write_sparse`.
