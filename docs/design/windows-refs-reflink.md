# Windows ReFS Reflink via FSCTL_DUPLICATE_EXTENTS_TO_FILE (#1389)

## Summary

oc-rsync ships copy-on-write fast paths for Linux (`FICLONE` on
Btrfs/XFS/bcachefs) and macOS (`clonefile` on APFS, completed in
#1388). The Windows side advertises `CopyMethod::ReFsReflink`
through the `PlatformCopy` trait but the FSCTL wiring sits in
`crates/fast_io/src/platform_copy/dispatch.rs` as a raw
`windows-sys` block that bypasses the project's "safe wrappers
preferred" policy. The cross-platform parity-matrix audit
(PR #3681) flagged this as a top-3 Windows gap, alongside ACL
read/write (#1866, completed) and IOCP socket support (#1928,
completed).

This note designs a `windows-rs`-backed `try_refs_reflink` that
plugs into the existing `PlatformCopy` trait (#1136), replaces
the Windows stub (#1139), and slots cleanly into the planned
`--cow` / `--no-cow` CLI surface (#1826). The wire protocol is
untouched. Public APIs stay stable. The change is internal to
`fast_io`.

## Motivation

ReFS (Resilient File System) is the recommended filesystem for
Windows file servers, Hyper-V hosts, and Storage Spaces Direct
deployments. It supports block-level copy-on-write via
`FSCTL_DUPLICATE_EXTENTS_TO_FILE`, the direct analogue of Linux's
`FICLONE` and macOS's `clonefile`. Without this fast path, a
ReFS user copying a 10 GB VHDX pays full disk bandwidth where
Linux and macOS users on equivalent CoW filesystems pay zero
bytes.

The cross-platform benchmark (#1659) documented the gap: a
same-volume 10 GB local copy completes in 18 ms on Linux Btrfs
versus 9.6 s on Windows ReFS through `CopyFileExW`. Secondary
benefits:

- **Storage efficiency.** Reflinks share extents until a write
  diverges them. 100 daily snapshots of a 50 GB VHDX cost 50 GB
  plus deltas, not 5 TB.
- **Virtual-machine workflows.** Hyper-V differencing disks
  depend on ReFS reflinks for instant-clone semantics. oc-rsync
  transfers into a Hyper-V backup target should preserve that
  property.

Closing this gap is wire-compatibility-neutral: the optimization
is local to the receiver's commit step.

## FSCTL_DUPLICATE_EXTENTS_TO_FILE Primer

The control code instructs the filesystem driver to make a
region of the destination share the source's on-disk extents,
marking them copy-on-write. Subsequent writes to either file
allocate fresh blocks. Cost is O(1) per call regardless of byte
count.

**Inputs:** an open destination handle plus a
`DUPLICATE_EXTENTS_DATA` struct carrying the source handle,
source offset, target offset, and byte count.

### Alignment requirements

Byte count and both offsets must be multiples of the volume's
**cluster size** (4 KiB default, 64 KiB on volumes >= 64 GB).
Misaligned inputs return `ERROR_INVALID_PARAMETER` with no
partial effect. Callers must:

1. Query cluster size via `GetDiskFreeSpaceW` at runtime.
2. Round byte count up to the next cluster boundary.
3. Pre-extend the destination via `set_len` before the ioctl.
4. Truncate the destination back to the actual size after.

### Maximum range size

A single FSCTL call covers up to 4 GiB minus one cluster
(`byte_count` is `i64` but the kernel internal counter is ULONG-
sized). Files larger than 4 GiB require multiple sequential
ioctls. The reference implementation in `winfsp` chunks at 1 GiB
for safety.

### Error codes

| Win32 code | Meaning | Action |
|---|---|---|
| `ERROR_BLOCK_TOO_MANY_REFERENCES` (1252) | Per-extent refcount cap (65535) exhausted. | Fall through to `CopyFileExW`. |
| `ERROR_INVALID_PARAMETER` (87) | Misaligned offset or cross-volume handles. | Fall through. The misalignment case is a bug; debug-log it. |
| `ERROR_NOT_SUPPORTED` (50) | Filesystem lacks block cloning (NTFS, FAT32, exFAT). | Fall through. |
| `ERROR_VIRUS_INFECTED` (225) | AV agent intercepted the ioctl. | Fall through; user-visible warning. |
| `ERROR_FILE_NOT_FOUND` (2) | Source handle invalid. | Hard error (programming bug). |
| `ERROR_ACCESS_DENIED` (5) | Caller lacks `FILE_WRITE_DATA` on dst. | Hard error. |

The first four are recoverable; the dispatch maps them to
`Err(_)` and the higher chain in `platform_copy_impl` falls
through. The last two propagate.

### Filesystem support

ReFS is the only Microsoft filesystem that supports block
cloning. NTFS does not, even on Server 2022. Detection
(see next section) layers a capability flag check over a
filesystem-name string match.

## Detection Plan

`crates/fast_io/src/refs_detect.rs` already exposes
`is_refs_filesystem(path) -> io::Result<bool>` with a
process-wide `Mutex<HashMap>` cache keyed on volume root
(`GetVolumePathNameW`). The new design extends it with a
second predicate:

```rust
pub fn supports_block_refcounting(path: &Path) -> io::Result<bool>;
```

Returns `true` when either of the following holds:

1. The volume's `FileSystemFlags & FILE_SUPPORTS_BLOCK_REFCOUNTING`
   (`0x08000000`) is non-zero.
2. The filesystem name string equals `"ReFS"` (legacy fallback
   for Windows Server 2016 RTM, which shipped before the flag
   was documented).

The capability flag is the forward-compatible signal: any future
filesystem that opts in becomes eligible without a code change.

This pattern matches the cached one-time runtime probe used by
`is_io_uring_available()` in `crates/fast_io/src/io_uring/config.rs:167`
(#1748). Cache key is the volume root, not the file path,
because filesystem type is immutable for a mounted volume. A
`clear_refs_cache` helper is exposed for tests.

### Why per-volume, not per-file

Per-file probes pay an extra `CreateFileW` + `CloseHandle` round-
trip per copy. The per-volume cache amortizes this across the
whole transfer. For a 100K-file transfer the saved syscall budget
is roughly 100K * 50 us = 5 s.

## API Surface

### Free function

```rust
/// Attempts a copy-on-write block clone using Windows ReFS
/// FSCTL_DUPLICATE_EXTENTS_TO_FILE.
///
/// Returns Ok(true) on a successful reflink, Ok(false) when the
/// destination is not on a CoW-capable filesystem, and Err(_) on
/// hard failures (permissions, I/O). Soft failures (refcount
/// fan-out, AV interference) are mapped to Err(_) so the higher
/// dispatch chain falls through.
///
/// Caller passes the unaligned file length. Cluster rounding,
/// chunked iteration, and final truncation are internal.
pub fn try_refs_reflink(src: &File, dst: &File, len: u64) -> io::Result<bool>;
```

The `bool` return distinguishes "succeeded" from "not
applicable", which the callsite in `platform_copy_impl` uses to
choose between recording the reflink method and falling through
quietly.

### Pseudo-code (windows-rs)

```rust
#[cfg(target_os = "windows")]
pub fn try_refs_reflink(src: &File, dst: &File, len: u64) -> io::Result<bool> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Ioctl::{
        FSCTL_DUPLICATE_EXTENTS_TO_FILE, DUPLICATE_EXTENTS_DATA,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    if !refs_detect::supports_block_refcounting_handle(dst)? {
        return Ok(false);
    }
    let cluster = refs_detect::cluster_size_for_handle(dst)?;
    let aligned = len.div_ceil(cluster) * cluster;
    dst.set_len(aligned)?;

    let mut offset: u64 = 0;
    while offset < aligned {
        let chunk = (aligned - offset).min(CHUNK_SIZE);
        let dup = DUPLICATE_EXTENTS_DATA {
            FileHandle: HANDLE(src.as_raw_handle() as isize),
            SourceFileOffset: offset as i64,
            TargetFileOffset: offset as i64,
            ByteCount: chunk as i64,
        };
        let mut returned: u32 = 0;
        // SAFETY: handles outlive the call; dup is fully
        // initialised; output pointer is a valid stack slot.
        unsafe {
            DeviceIoControl(
                HANDLE(dst.as_raw_handle() as isize),
                FSCTL_DUPLICATE_EXTENTS_TO_FILE,
                Some(&dup as *const _ as *const _),
                size_of::<DUPLICATE_EXTENTS_DATA>() as u32,
                None, 0, Some(&mut returned), None,
            ).ok()?;
        }
        offset += chunk;
    }
    dst.set_len(len)?;
    Ok(true)
}

const CHUNK_SIZE: u64 = 1 << 30; // 1 GiB, well under 4 GiB cap
```

The `windows` crate (microsoft/windows-rs) supplies typed
handles, the `DUPLICATE_EXTENTS_DATA` definition, and a
`Result`-aware `DeviceIoControl`. The single `unsafe` block is
the inevitable kernel boundary; the crate eliminates the manual
struct-layout, constant, and handle-validity tracking that the
current `windows-sys` block carries.

### Migration from windows-sys

The existing `try_refs_reflink_impl` in
`crates/fast_io/src/platform_copy/dispatch.rs:296-484` uses
`windows-sys` with hand-written struct definitions and manually
defined access constants. The migration is mechanical:

1. Replace local `DuplicateExtentsData` with
   `windows::Win32::System::Ioctl::DUPLICATE_EXTENTS_DATA`.
2. Replace the manual `FSCTL_DUPLICATE_EXTENTS_TO_FILE` constant
   with the typed import.
3. Replace `windows-sys::CreateFileW` with the windows-rs form
   that returns `Result<HANDLE>`.
4. Replace manual `INVALID_HANDLE_VALUE` checks with the
   `Result` flow.
5. Replace `CloseHandle` calls with RAII via `OwnedHandle` or
   `File::from_raw_handle` (already used for the destination).

Net diff: roughly 30 fewer lines, no behavioural change, type-
checked struct layouts. The `windows` crate is the project's
preferred Windows binding (per the workspace unsafe-code policy)
and is already on the daemon-design roadmap (#1866).

### Cargo dependency

```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
    "Win32_System_Ioctl",
    "Win32_System_IO",
] }
```

`windows-sys` is retained because IOCP still uses it; both crates
coexist already. A separate cleanup PR (#1866) consolidates the
crate onto `windows-rs`.

## Integration with PlatformCopy Trait

The trait in `crates/fast_io/src/platform_copy/types.rs` exposes
three methods. The new function plugs in at one site:

```rust
#[cfg(target_os = "windows")]
fn platform_copy_impl(src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult> {
    let parent = dst.parent().unwrap_or(dst);
    if refs_detect::supports_block_refcounting(parent).unwrap_or(false) {
        let source = File::open(src)?;
        let destination = File::create(dst)?;
        match try_refs_reflink(&source, &destination, size_hint) {
            Ok(true)  => return Ok(CopyResult::new(0, CopyMethod::ReFsReflink)),
            Ok(false) => { /* fall through */ }
            Err(e) if recoverable(&e) => { let _ = std::fs::remove_file(dst); }
            Err(e) => return Err(e),
        }
    }
    try_copy_file_ex_or_std(src, dst, size_hint)
}
```

`recoverable()` matches the four soft failure codes
(`ERROR_BLOCK_TOO_MANY_REFERENCES`, `ERROR_INVALID_PARAMETER`,
`ERROR_NOT_SUPPORTED`, `ERROR_VIRUS_INFECTED`). Other errors
propagate.

The Windows stub (#1139) is replaced. The trait signature does
not change; `supports_reflink()` continues to return `true` on
Windows because the per-file decision is made at copy time after
the volume probe. `CopyMethod::ReFsReflink` already exists in
the enum (`types.rs:36-41`).

## Fallback Chain

Windows dispatch order:

```
1. FSCTL_DUPLICATE_EXTENTS_TO_FILE  (ReFS only, same volume, aligned)
2. CopyFileExW                       (any NTFS/ReFS, optional NO_BUFFERING)
3. std::fs::copy                     (portable buffered fallback)
```

Step 1 falls through to step 2 when:

- the destination volume is not ReFS,
- source and destination are on different volumes,
- a chunk hits `ERROR_BLOCK_TOO_MANY_REFERENCES`,
- a chunk hits `ERROR_INVALID_PARAMETER` (alignment bug, debug-
  logged), or
- an AV agent blocks the FSCTL.

Step 2 falls through to step 3 on transient `CopyFileExW` errors
or when the no-buffering path encounters an unaligned source
offset (rare). The `CopyFileExW` enhancement work (#1414) is
independent of this design.

## Wiring with --reflink CLI Flag

The CLI flag work (#1826) introduces `--cow` / `--no-cow` mapped
to a `CowPolicy` enum on `CoreConfig`:

| Policy | Semantics |
|---|---|
| `Auto` (default) | Use reflink when supported; fall through silently otherwise. |
| `Always` | Require reflink; fail the transfer when unsupported. |
| `Never` | Skip the reflink path; always use `CopyFileExW` / `std::fs::copy`. |

`DefaultPlatformCopy` gains a `with_cow_policy` builder method.
The dispatch consults the policy:

```rust
match cow_policy {
    CowPolicy::Never  => skip_reflink_branch(),
    CowPolicy::Auto   => try_then_fall_through(),
    CowPolicy::Always => {
        if !refs_detect::supports_block_refcounting(dst)? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "reflink required but destination is not on a CoW filesystem",
            ));
        }
        try_then_propagate_failure()
    }
}
```

This keeps the policy decision in one place and lets all three
platforms (Linux `FICLONE`, macOS `clonefile`, Windows ReFS)
honour the same flag with no per-platform CLI plumbing. The CLI
flag arrives in #1826; this design notes the hook point so that
work lands without churn.

## Test Plan

### Unit tests (any platform)

Mocked FSCTL backend trait, exercised in-process without a ReFS
volume:

```rust
trait IoctlBackend {
    fn duplicate_extents(&self, dst: HANDLE, data: &DUPLICATE_EXTENTS_DATA)
        -> windows::core::Result<()>;
}
```

| Test | Mock setup | Expected |
|---|---|---|
| `refs_unsupported_falls_through` | capability probe returns `false`. | `Ok(false)`; outer dispatch tries `CopyFileExW`. |
| `block_too_many_references_falls_through` | FSCTL returns `ERROR_BLOCK_TOO_MANY_REFERENCES`. | `Err(_)`; outer dispatch removes partial dst, falls through. |
| `invalid_parameter_falls_through` | FSCTL returns `ERROR_INVALID_PARAMETER`. | `Err(_)` plus debug log. |
| `cluster_alignment_math` | Length 5000, cluster 4096. | Aligned 8192; `set_len(8192)` then `set_len(5000)`. |
| `chunk_split_at_1gib` | Length 3 GiB. | Three FSCTL calls at offsets 0, 1 GiB, 2 GiB. |
| `zero_length_short_circuits` | Length 0. | Empty destination, no FSCTL call. |

The trait is `pub(crate)`. Production wires `RealIoctlBackend`;
tests substitute `MockIoctlBackend` with a configurable response
sequence.

### Integration tests (Windows + ReFS)

Gated `#[cfg(windows)]` plus a runtime ReFS probe; skip
gracefully on NTFS-only runners.

| Test | Workload | Assertion |
|---|---|---|
| `same_volume_reflink_succeeds` | Copy 100 MB on ReFS. | Method `ReFsReflink`; bytes match source. |
| `cross_volume_falls_through` | Copy ReFS -> NTFS. | Method `CopyFileEx`. |
| `large_file_chunked` | Copy 5 GB on ReFS. | Method `ReFsReflink`; size matches. |
| `cow_semantics_preserved` | Reflink, then write to source. | Destination unchanged; source diverges. |

### CI runner

GitHub `windows-2022` does not provide ReFS by default. The job
provisions a dynamic VHDX, formats it ReFS, mounts it, and runs
the gated tests against that mountpoint:

```powershell
$vhd = New-VHD -Path "$env:TEMP\refs.vhdx" -SizeBytes 10GB -Dynamic
Mount-VHD -Path $vhd.Path
$disk = Get-Disk | Where-Object PartitionStyle -eq RAW | Select-Object -First 1
Initialize-Disk -Number $disk.Number -PartitionStyle GPT
$part = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
Format-Volume -DriveLetter $part.DriveLetter -FileSystem ReFS -Force -Confirm:$false
```

This adds about 90 s to the first execution and is cached as a
job artifact. Older runner images lack the ReFS driver entirely.
Local developers without ReFS access run only the mocked unit
suite; the integration tests skip without failing.

## Risks and Limitations

1. **`ERROR_BLOCK_TOO_MANY_REFERENCES` fan-out.** ReFS caps each
   extent's refcount at 65535. A workflow reflinking the same
   source into 65536 destinations hits the cap on the final
   call. Mitigation: that one destination falls through to
   `CopyFileExW`; the previous 65535 reflinks remain intact.

2. **Alignment-driven size inflation.** The destination is
   briefly extended to a cluster-aligned size before truncation.
   Power loss between `set_len(aligned)` and the final
   `set_len(len)` leaves it up to one cluster too large. This is
   the same risk window any temp-file-then-rename copy carries,
   and the final truncation happens before any rename in the
   commit phase. The temp file is `unlink`ed on retry.

3. **Cross-volume reflink unsupported.** ReFS does not implement
   reflinks across volumes even when both are ReFS. Matches
   `FICLONE` and `clonefile` semantics. Cross-volume falls
   through automatically.

4. **AV agent interference.** Some AV agents intercept FSCTL
   calls and either block or rewrite them. The dispatch logs at
   debug level and falls through; user-visible behaviour is
   identical to a non-ReFS run.

5. **ReFS feature levels.** Block cloning is supported from
   ReFS v3.0 onward; all currently-supported Windows versions
   ship with that or newer. Pre-v3.0 (Server 2012 R2) is out-of-
   support upstream and detected correctly by the
   `FILE_SUPPORTS_BLOCK_REFCOUNTING` flag.

6. **Cluster size variance.** A volume formatted with non-default
   cluster size (e.g., 64 KiB on a small volume) requires the
   runtime cluster query. Hardcoding 4 KiB would silently corrupt
   the alignment math. The design queries `GetDiskFreeSpaceW`
   per volume and caches the result.

7. **Refcount fragmentation.** Repeated reflink-then-modify
   cycles on a frequently-edited file produce many short extents
   in the volume metadata. This is a ReFS-internal concern with
   no oc-rsync workaround; query cost is microseconds and ReFS
   coalesces during scrub.

## Non-Goals

1. **NTFS support.** NTFS does not implement
   `FSCTL_DUPLICATE_EXTENTS_TO_FILE`. There is no plan to emulate
   the semantics with sparse files or hardlinks; the user
   experience would diverge from upstream rsync.
2. **Cross-volume reflink.** Always copies across volumes; no
   plan to chain partial reflinks across volume boundaries.
3. **Wire-protocol extensions.** The reflink decision is local
   to the receiver's commit path. No new tokens, no negotiation
   bytes, no version-bump implications. Upstream rsync 3.4.1 is
   unaware of this optimization and the behaviour is invisible
   to it.
4. **Snapshot-based deduplication.** ReFS supports volume-level
   integrity streams and dedup as a separate Server feature. This
   design touches only the per-file FSCTL.
5. **Replacing `CopyFileExW`.** It remains the second-tier
   optimization for non-ReFS volumes and the fallback when
   reflink is unavailable.
6. **Modifying upstream rsync wire protocol.** Out of scope.

## Decision

Land the design note now. The follow-up implementation PR:

1. Adds the `windows` crate with the four feature flags above.
2. Adds `supports_block_refcounting` to `refs_detect.rs`,
   sharing the existing cache.
3. Rewrites `try_refs_reflink_impl` against the `windows` crate
   per the pseudo-code above.
4. Threads the ReFS path through `platform_copy_impl` with
   chunked alignment handling.
5. Adds the unit-test mock layer and the gated integration
   tests.
6. Wires `CowPolicy` once #1826 lands (separate PR).

The wire protocol is untouched, the public `PlatformCopy` trait
is untouched, and the change is internal to the receiver's
commit path. Cross-platform parity with Linux `FICLONE` and
macOS `clonefile` (#1388) is restored at the third covered
filesystem.

## Tracking

Navigation only; no new tracker items added by this note:

- #1136 - `PlatformCopy` trait (landed).
- #1139 - Windows stub in `PlatformCopy` (landed; this design
  replaces it).
- #1272 - `CopyFileExW` partial implementation (landed).
- #1388 - macOS `clonefile` (landed; reference implementation).
- #1389 - this design and the follow-up implementation PR.
- #1414 - `CopyFileExW` enhancements (pending; independent).
- #1659 - cross-platform benchmark documenting the gap (landed).
- #1748 - io_uring runtime probe (landed; pattern reference for
  the volume-cache layer).
- #1826 - `--cow` / `--no-cow` CLI flag (pending; see "Wiring
  with --reflink CLI Flag").
- #1866 - Windows ACL via windows-rs (landed; reference for the
  windows-rs migration).
- #1928 - IOCP socket support (landed).
- PR #3681 - parity-matrix audit that surfaced this gap.
