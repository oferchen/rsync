# Windows FSCTL_SET_ZERO_DATA Hole Punching (#2131)

## Summary

oc-rsync writes sparse files on Linux and macOS by punching holes
into already-allocated regions, but the Windows path falls back to
writing physical zero bytes. This note plans a Windows backend that
uses `FSCTL_SET_ZERO_DATA` to deallocate ranges in NTFS sparse files,
matching the Unix sparse semantics promised by `--sparse-detect=`
(#1828) and feeding the sparse-writer Decorator planned in #2132.

## 1. Unix baseline

Both Unix targets expose hole punching as a single syscall on an
open file descriptor:

- **Linux**: `fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
  offset, len)` deallocates the byte range while preserving the
  logical EOF. Supported on ext4, XFS, Btrfs, tmpfs.
- **macOS**: `fcntl(fd, F_PUNCHHOLE, &fpunchhole_t { fp_offset, fp_length })`
  deallocates on APFS and HFS+. Aligned to fs block size.

`fast_io` already wires both behind a sparse helper invoked by the
delta-apply receiver. The Windows arm currently writes zero buffers,
which doubles physical allocation for sparse-source files.

## 2. Windows: FSCTL_SET_ZERO_DATA

`DeviceIoControl(handle, FSCTL_SET_ZERO_DATA, ...)` punches holes
inside an NTFS sparse file. The control code is documented in the
Win32 IOCTL reference; payload is:

```text
FILE_ZERO_DATA_INFORMATION {
    FileOffset:     i64,  // inclusive start, byte offset
    BeyondFinalZero: i64, // exclusive end, byte offset
}
```

Pre-conditions enforced by NTFS:

- The handle must be opened with `FILE_WRITE_DATA`.
- The file must already carry `FILE_ATTRIBUTE_SPARSE_FILE`. Set it
  once at create time via `FSCTL_SET_SPARSE` (idempotent).
- The volume must support sparse files (NTFS, ReFS with sparse
  enabled). FAT32, exFAT, and SMB shares without the sparse
  capability return `ERROR_INVALID_FUNCTION` or silently no-op.
- `FileOffset` and `BeyondFinalZero` are rounded to cluster size
  (typically 4 KiB) by the filesystem; partial clusters at the
  boundaries are zero-filled rather than deallocated.

## 3. API shape

Expose two thin wrappers in `fast_io` over the `windows` crate
(`microsoft/windows-rs`, already a transitive dep through
`windows-sys` for CopyFileExW):

```rust
#[cfg(windows)]
pub trait PlatformSparse {
    /// Tag the file as sparse. No-op if already tagged or if the
    /// volume does not support sparse files.
    fn set_sparse_attr(handle: HANDLE) -> io::Result<()>;

    /// Punch [start, start + len) using FSCTL_SET_ZERO_DATA.
    fn punch_hole(handle: HANDLE, start: u64, len: u64) -> io::Result<()>;
}
```

`set_sparse_attr` calls `DeviceIoControl` with `FSCTL_SET_SPARSE`
and treats `ERROR_INVALID_FUNCTION` as a graceful no-op (returns
`Ok(())`, sparse not available). `punch_hole` constructs
`FILE_ZERO_DATA_INFORMATION { FileOffset: start as i64,
BeyondFinalZero: (start + len) as i64 }` and dispatches the ioctl,
mapping `ERROR_INVALID_FUNCTION` and `ERROR_NOT_SUPPORTED` to a
typed `SparseUnsupported` error so the caller can fall back to
zero-write.

## 4. Integration

Two options, both keep the existing Unix code paths intact:

1. **Extend `PlatformCopy`**: add `set_sparse_attr` and
   `punch_hole` defaults that return `SparseUnsupported`, then
   override on Windows. Lowest churn.
2. **New `PlatformSparse` trait** alongside `PlatformCopy`. Cleaner
   separation when the sparse decorator (#2132) wraps an arbitrary
   `Write` and needs to call into platform sparse without dragging
   the copy dispatch table along. Preferred.

Either way, the Windows entry point goes in
`crates/fast_io/src/platform_sparse/windows.rs` (new module) and
gets wired from the sparse decorator. Linux and macOS keep their
existing `fallocate`/`fcntl` paths, exposed through the same trait
for symmetry.

## 5. Risks and edge cases

- Forgetting `FSCTL_SET_SPARSE` causes `FSCTL_SET_ZERO_DATA` to
  zero-fill instead of deallocating, defeating the optimisation
  silently. The wrapper must always set sparse before the first
  punch.
- Non-NTFS volumes (FAT32, exFAT, network mounts without sparse
  semantics) must degrade to zero-write. Detection is by ioctl
  return code, not pre-flight, since `GetVolumeInformationW`
  flags are advisory.
- Cluster alignment: callers should align punch ranges to 4 KiB
  where possible. Misaligned ranges still succeed but the boundary
  clusters stay allocated and zero-filled.
- Concurrent writers: hole punching does not invalidate cached
  reads. Tests must flush the destination handle before assertions.

## 6. Cross-references

- #1828 - `--sparse-detect=` CLI option, sparse policy surface.
- #2132 - Sparse writer Decorator that owns the cross-platform
  abstraction; this design is its Windows backend.
- `crates/fast_io/src/platform_copy/dispatch.rs` - existing pattern
  for per-OS fast paths plus graceful fallback.
