# Windows ReFS Reflink via FSCTL_DUPLICATE_EXTENTS_TO_FILE (#1389)

## Summary

oc-rsync ships copy-on-write fast paths for Linux (`FICLONE` on
Btrfs/XFS/bcachefs) and macOS (`clonefile` on APFS). The Windows
dispatch in `crates/fast_io/src/platform_copy/dispatch.rs` has no
reflink fast path and falls through to `CopyFileExW`, then
`std::fs::copy`. This note plans a ReFS-only block-clone path so
Windows reaches Linux/macOS parity for same-volume copies on
supported volumes.

## 1. Current fast_io reflink dispatch

The platform-specific entry point lives in
`crates/fast_io/src/platform_copy/dispatch.rs::platform_copy_impl`:

- **Linux**: tries `FICLONE` via `rustix::fs::ioctl_ficlone`,
  falls back to `copy_file_range` for files >= 64 KiB, then
  `std::fs::copy`. Reflink covers Btrfs, XFS (reflink-enabled),
  bcachefs.
- **macOS**: tries `clonefile(2)` via `libc::clonefile`, falls
  back to `fcopyfile(3)` with `COPYFILE_DATA`, then
  `std::fs::copy`. Reflink covers APFS only.
- **Windows**: no fast path. Picks `CopyFileExW` (with
  `COPY_FILE_NO_BUFFERING` for files > 4 MiB), falls back to
  `std::fs::copy`. `CopyMethod::ReFsReflink` is enumerated in
  `platform_copy/types.rs` but unused.

## 2. ReFS-only feature: FSCTL_DUPLICATE_EXTENTS_TO_FILE

Available on Windows Server 2016+ and Windows 11 24H2+ (client
ReFS). Block clone is O(1) regardless of file size when both
files share the same ReFS volume. Call shape:

- Open destination handle with `GENERIC_READ | GENERIC_WRITE`,
  `CREATE_ALWAYS`, no share modes during the clone window.
- Open source handle with `GENERIC_READ | FILE_SHARE_READ`.
- Pre-size destination to a cluster-aligned length via
  `SetFileInformationByHandle(FileEndOfFileInfo)`.
- Issue `DeviceIoControl(dst, FSCTL_DUPLICATE_EXTENTS_TO_FILE, &data, sizeof(data), ...)`
  where `data` is `DUPLICATE_EXTENTS_DATA { FileHandle: src,
  SourceFileOffset, TargetFileOffset, ByteCount }`. All three
  numeric fields must be cluster-aligned.
- After ioctl success, truncate the destination back to the
  real source length so the on-disk size matches the source.

Errors of interest: `ERROR_BLOCK_TOO_MANY_REFERENCES` (extent
already shared at refcount cap), `ERROR_INVALID_PARAMETER`
(alignment violation), `ERROR_NOT_SUPPORTED` (NTFS or older ReFS
without block clone).

## 3. Detection

Filesystem type is queried via `GetVolumeInformationW` (or
`GetVolumeInformationByHandleW`) on the volume root resolved by
`GetVolumePathNameW`. The filesystem name string equals `"ReFS"`
on supported volumes. Since filesystem type is immutable for a
mounted volume, results are memoized in a process-wide
`OnceLock<Mutex<HashMap<PathBuf, bool>>>` keyed by volume root.
This already exists in `crates/fast_io/src/refs_detect.rs`; the
dispatch consumes it via `is_refs_filesystem(dst.parent())`.

Cluster size for alignment is queried per volume via
`GetDiskFreeSpaceW` (sectors-per-cluster x bytes-per-sector,
typically 4 KiB or 64 KiB on ReFS) and cached alongside the
filesystem-type entry in the same map structure.

## 4. Fallback chain

On Windows, ordered by descending speed:

1. `try_refs_reflink_impl` if `is_refs_filesystem(dst.parent())`
   returns `Ok(true)`. O(1), zero data copied. On error, remove
   the partial destination and fall through.
2. `CopyFileExW` with `COPY_FILE_NO_BUFFERING` for files larger
   than 4 MiB (matches current behaviour). On error, fall
   through.
3. `std::fs::copy` as the portable last resort.

Each step removes any partial destination on failure to keep the
next step starting from a clean slate.

## 5. Risks and constraints

- **Source handle lifetime.** `DUPLICATE_EXTENTS_DATA.FileHandle`
  references the source by HANDLE. The source must stay open
  across the ioctl and may be closed only after the call
  returns. Closing prematurely yields
  `ERROR_INVALID_HANDLE`.
- **Cluster alignment.** `SourceFileOffset`, `TargetFileOffset`,
  and `ByteCount` must each be a multiple of the volume cluster
  size. Whole-file clones round `ByteCount` up to the next
  cluster boundary, then `SetEndOfFile`-truncate the
  destination back to the source's real length.
- **Same-volume only.** Cross-volume clones are not supported by
  the fsctl and surface as `ERROR_INVALID_PARAMETER`. Detection
  at `dst.parent()` gates the path; cross-volume copies skip
  step 1.
- **Refcount cap.** Each ReFS extent has a maximum reference
  count. Hot files cloned beyond the cap return
  `ERROR_BLOCK_TOO_MANY_REFERENCES`; the chain falls through to
  `CopyFileExW`.
- **Adoption.** ReFS is opt-in on Windows Server, not the
  default Windows client filesystem. The fast path is dead code
  on NTFS hosts but pays only a single cached
  `GetVolumeInformationW` call per volume.

## 6. Safe-wrapper policy

`fast_io` is the only crate permitted to hold unsafe code, but
new bindings prefer the
[`windows`](https://github.com/microsoft/windows-rs) crate over
hand-written `windows-sys` FFI for type safety and HRESULT
ergonomics. Concretely:

- `windows::Win32::Storage::FileSystem::{CreateFileW,
  GetVolumeInformationW, GetVolumePathNameW, GetDiskFreeSpaceW,
  SetFileInformationByHandle}`.
- `windows::Win32::System::Ioctl::{DUPLICATE_EXTENTS_DATA,
  FSCTL_DUPLICATE_EXTENTS_TO_FILE}` (constant + struct exposed by
  the crate; no manual `CTL_CODE` macro reproduction).
- `windows::Win32::System::IO::DeviceIoControl`.
- `windows::Win32::Foundation::{HANDLE, CloseHandle,
  INVALID_HANDLE_VALUE}`.

The public API stays at `pub fn try_refs_reflink(src, dst) ->
io::Result<()>`. All `unsafe` is scoped to the wrapper body; no
raw FFI types leak through `fast_io`'s public surface.
