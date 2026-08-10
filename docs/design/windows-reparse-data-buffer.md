# REPARSE_DATA_BUFFER layout and IO_REPARSE_TAG_* constants (WPC-8'.2)

Tracks parent #2869 (Windows real-world parity series). Companion to
the classifier audit at `docs/audit/windows-reparse-point-classification.md`
(WPC-7, #2909) and to the classifier implementation spec at
`docs/design/wpc-8-reparse-point-classifier.md` (WPC-8, #2910). This
document is the deferred WPC-8'.2 deliverable - a focused wire-shape
reference for the buffer layout and tag constants used by
`crates/metadata/src/windows/reparse.rs`.

The classifier itself shipped under PR #5579 (`feat(metadata): classify
NTFS reparse points (symlink/junction/mount-point)`). The follow-on
target-name parser landed in commit 6f4d8b182 (`feat(metadata): parse
reparse-data target name for symlink + junction + mount-point`). The
WPC-9 regression-test surface (RAII fixture helpers + symlink/junction
transfer round-trip) landed in commits 93127fede and ded9a69ee.

## 1. Purpose

NTFS attaches a small block of provider-specific metadata - a
**reparse point** - to a file or directory. The kernel sets the
`FILE_ATTRIBUTE_REPARSE_POINT` bit on the entry and chains the
provider associated with the tag into every I/O operation. Until the
tag is identified, oc-rsync cannot decide:

- **Whether to transfer the entry as a symbolic link** versus a
  regular file. Genuine NTFS symbolic links
  (`IO_REPARSE_TAG_SYMLINK`) and junctions / volume mount-points
  (`IO_REPARSE_TAG_MOUNT_POINT`) all surface as `is_symlink() == true`
  through Rust's `std::fs::FileType` since Rust 1.49; every other tag
  collapses to `is_file()` or `is_dir()`. Without tag dispatch the
  sender cannot distinguish a symlink from a junction, nor a junction
  from a volume mount-point.
- **Whether opening the file forces hydration.** Cloud-files
  placeholders (`IO_REPARSE_TAG_CLOUD*`) silently materialise the file
  body on first read; over a tree the size of a OneDrive root this
  forces a multi-terabyte redownload on every transfer. The classifier
  is the entry point for an eventual opt-out flag that preserves the
  placeholder rather than rehydrating it.
- **Whether the body is user data or provider-private bytes.**
  AppExecLink shims (`IO_REPARSE_TAG_APPEXECLINK`), Windows Container
  Isolation placeholders (`IO_REPARSE_TAG_WCI`), and WSL POSIX shadows
  (`IO_REPARSE_TAG_LX_*`, `IO_REPARSE_TAG_AF_UNIX`) all return
  provider-specific blobs through `CreateFileW`. Shipping those bytes
  as a "regular file" is silently destructive on the destination.
- **Whether the entry is a junction loop.** Junctions can point at
  ancestors of their own path. The walker only avoids re-entry today
  because junctions are folded into the symlink branch and symlinks
  are not followed by default; `--copy-links` over a junction-bearing
  tree would recurse without bound without classification.

The WPC-7 audit enumerates the user-visible consequences in detail.
The job of `crates/metadata/src/windows/reparse.rs` is to map a raw
`ReparseTag` to a typed [`ReparseKind`] so every downstream branch
(file-list build, walker, local-copy executor, batch replay) makes the
right call.

## 2. REPARSE_DATA_BUFFER layout

The on-disk shape is documented in Microsoft Open Specifications
[MS-FSCC] section 2.1.2.1 (`REPARSE_DATA_BUFFER`). The buffer is a
small fixed header followed by a per-tag union; every reparse point
returned by `DeviceIoControl(handle, FSCTL_GET_REPARSE_POINT, ...)`
starts with the same eight-byte header:

```text
                   1 1 1 1 1 1 1 1 1 1 2 2 2 2 2 2 2 2 2 2 3 3
0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
-----------------------------------------------------------------
|                          ReparseTag                           |   offset 0  (u32 LE)
-----------------------------------------------------------------
|     ReparseDataLength         |          Reserved             |   offset 4  (2x u16 LE)
-----------------------------------------------------------------
|                                                               |
|                   payload (ReparseDataLength bytes,           |   offset 8  (per-tag union)
|                    union-typed by ReparseTag)                 |
|                                                               |
-----------------------------------------------------------------
```

Field semantics (MS-FSCC 2.1.2.1):

| Offset | Size | Field | Meaning |
|--------|------|-------|---------|
| 0 | 4 bytes | `ReparseTag` | `u32` little-endian. Identifies the reparse provider and the per-tag payload shape. High-bit conventions are defined in section 5 below. |
| 4 | 2 bytes | `ReparseDataLength` | `u16` little-endian. Byte length of the payload that follows; does **not** include the eight-byte header. Maximum is `MAXIMUM_REPARSE_DATA_BUFFER_SIZE` (16 KiB minus the header). |
| 6 | 2 bytes | `Reserved` | `u16` set to zero by the kernel; ignored on read. |
| 8 | `ReparseDataLength` bytes | payload | Per-tag union, described in section 4. |

The maximum total buffer is `MAXIMUM_REPARSE_DATA_BUFFER_SIZE = 16384`
bytes (`winnt.h`), pinned in
`crates/metadata/src/windows/reparse.rs:59`. The classifier always
allocates a 16 KiB receive buffer, hands it to `DeviceIoControl`, and
truncates to the returned byte count before parsing.

`classify_reparse_point` only needs the four-byte tag; the per-tag
parsers (`parse_symlink_reparse`, `parse_junction_reparse`,
`parse_mount_point_reparse`) walk the payload according to the
per-tag union. Header-only inspection lets cloud / `AF_UNIX` /
unknown tags route to `ReparseKind::OneDrive`, `ReparseKind::AfUnix`,
or `ReparseKind::Other(tag)` without ever touching the opaque
payload.

Implementation constants encoding this layout:

```rust
// REPARSE_TAG_OFFSET = 0, REPARSE_DATA_LENGTH_OFFSET = 4
// (header u32 + u16 + u16 = 8 bytes before the payload)
const REPARSE_HEADER_SIZE: usize = 8;
```

(`crates/metadata/src/windows/reparse.rs:85-90`).

## 3. IO_REPARSE_TAG_* constants table

The tag namespace is allocated by Microsoft. Every entry below is
sourced from `winnt.h` (Windows SDK 10.0.22621) and from the
Microsoft reparse-tag registry referenced by MS-FSCC 2.1.2. The
"Class" column maps each tag to the high-bit conventions described in
section 5. The "oc-rsync handling" column reflects the variants of
[`ReparseKind`] in `reparse.rs` as of the WPC-8'.13 close-out; tags
not enumerated by the classifier fall into `ReparseKind::Other(tag)`.

| Tag name | Hex value | Class | oc-rsync handling |
|----------|-----------|-------|-------------------|
| `IO_REPARSE_TAG_MOUNT_POINT` | `0xA000_0003` | Microsoft, surrogate | `ReparseKind::Junction` or `ReparseKind::MountPoint`, disambiguated by substitute-name prefix (`\??\Volume{GUID}\` -> mount-point; otherwise junction). |
| `IO_REPARSE_TAG_HSM` | `0xC000_0004` | Microsoft, non-surrogate | `ReparseKind::Other(0xC000_0004)`. Triggers HSM hydration on `CreateFileW`. |
| `IO_REPARSE_TAG_DRIVE_EXTENDER` | `0x8000_0005` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0005)`. |
| `IO_REPARSE_TAG_HSM2` | `0x8000_0006` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0006)`. |
| `IO_REPARSE_TAG_SIS` | `0x8000_0007` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0007)`. |
| `IO_REPARSE_TAG_WIM` | `0x8000_0008` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0008)`. |
| `IO_REPARSE_TAG_CSV` | `0x8000_0009` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0009)`. |
| `IO_REPARSE_TAG_DFS` | `0x8000_000A` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_000A)`. |
| `IO_REPARSE_TAG_FILTER_MANAGER` | `0x8000_000B` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_000B)`. |
| `IO_REPARSE_TAG_SYMLINK` | `0xA000_000C` | Microsoft, surrogate | `ReparseKind::Symlink`. Parsed by `parse_symlink_reparse`. |
| `IO_REPARSE_TAG_DFSR` | `0x8000_0012` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0012)`. |
| `IO_REPARSE_TAG_DEDUP` | `0x8000_0013` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0013)`. Triggers Server Data Deduplication rehydration. |
| `IO_REPARSE_TAG_NFS` | `0x8000_0014` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0014)`. |
| `IO_REPARSE_TAG_FILE_PLACEHOLDER` | `0x8000_0015` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0015)`. Legacy OneDrive (pre-Win10 1709). |
| `IO_REPARSE_TAG_WOF` | `0x8000_0017` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0017)`. Windows Overlay Filter (compact OS). |
| `IO_REPARSE_TAG_WCI` | `0x8000_0018` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0018)`. Windows Container Isolation. |
| `IO_REPARSE_TAG_WCI_1` | `0x9000_0018` | Microsoft, name-surrogate | `ReparseKind::Other(0x9000_0018)`. |
| `IO_REPARSE_TAG_GLOBAL_REPARSE` | `0xA000_0019` | Microsoft, surrogate | `ReparseKind::Other(0xA000_0019)`. Bind-mount equivalent. |
| `IO_REPARSE_TAG_CLOUD` | `0x9000_001A` | Microsoft, name-surrogate | `ReparseKind::OneDrive`. Matches the explicit constant. |
| `IO_REPARSE_TAG_CLOUD_1..F` | `0x9000_001A..=0x9000_001F` | Microsoft, name-surrogate | `ReparseKind::OneDrive`. The classifier matches the range `0x9000_0010..=0x9000_001F` to cover any cloud-files provider slot. |
| `IO_REPARSE_TAG_ONEDRIVE` | `0x9000_001B` | Microsoft, name-surrogate | `ReparseKind::OneDrive`. Legacy OneDrive placeholder. |
| `IO_REPARSE_TAG_APPEXECLINK` | `0x8000_001B` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_001B)`. Windows Store app-execution alias. |
| `IO_REPARSE_TAG_PROJFS` | `0x9000_001C` | Microsoft, name-surrogate | `ReparseKind::Other(0x9000_001C)`. Projected File System (VFS-for-Git, GVFS). |
| `IO_REPARSE_TAG_LX_SYMLINK` | `0xA000_001D` | Microsoft, surrogate | `ReparseKind::Other(0xA000_001D)`. WSL POSIX symlink shadow; reserved for future routing through the symlink branch. |
| `IO_REPARSE_TAG_STORAGE_SYNC` | `0x8000_0020` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0020)`. Azure File Sync. |
| `IO_REPARSE_TAG_WCI_TOMBSTONE` | `0xA000_0021` | Microsoft, surrogate | `ReparseKind::Other(0xA000_0021)`. |
| `IO_REPARSE_TAG_UNHANDLED` | `0x8000_0022` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0022)`. |
| `IO_REPARSE_TAG_AF_UNIX` | `0x8000_0023` | Microsoft, non-surrogate | `ReparseKind::AfUnix`. WSL `AF_UNIX` socket shadow. |
| `IO_REPARSE_TAG_LX_FIFO` | `0x8000_0024` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0024)`. WSL FIFO shadow. |
| `IO_REPARSE_TAG_LX_CHR` | `0x8000_0025` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0025)`. WSL character-device shadow. |
| `IO_REPARSE_TAG_LX_BLK` | `0x8000_0026` | Microsoft, non-surrogate | `ReparseKind::Other(0x8000_0026)`. WSL block-device shadow. |
| `IO_REPARSE_TAG_PROJFS_TOMBSTONE` | `0xA000_0027` | Microsoft, surrogate | `ReparseKind::Other(0xA000_0027)`. |

Variants currently materialised by `classify_from_buffer`:

```rust
pub enum ReparseKind {
    Symlink,
    Junction,
    MountPoint,
    OneDrive,
    AfUnix,
    Other(u32),
}
```

(`crates/metadata/src/windows/reparse.rs:129-160`).

All explicit constants in `reparse.rs`:

```rust
const IO_REPARSE_TAG_SYMLINK:      u32 = 0xA000_000C;
const IO_REPARSE_TAG_MOUNT_POINT:  u32 = 0xA000_0003;
const IO_REPARSE_TAG_AF_UNIX:      u32 = 0x8000_0023;
const IO_REPARSE_TAG_CLOUD:        u32 = 0x9000_001A;
const IO_REPARSE_TAG_ONEDRIVE:     u32 = 0x9000_001B;
const CLOUD_TAG_RANGE_START:       u32 = 0x9000_0010;
const CLOUD_TAG_RANGE_END:         u32 = 0x9000_001F;
```

(`crates/metadata/src/windows/reparse.rs:61-82`).

Tags not enumerated above (HSM, DFS, WIM, DEDUP, ProjFS, WCI, WOF,
LX_FIFO/CHR/BLK, etc.) intentionally fall into
`ReparseKind::Other(tag)` so callers can log the raw tag and pick a
documented fallback. Recommendations R1-R4 in the WPC-7 audit cover
the future routing decisions (junction-as-symlink-with-dir-flag,
cloud-placeholder opt-out, WSL LX_SYMLINK decoding, opaque
preservation as xattr). Those decisions are out of scope for the
classifier; the classifier only has to surface the tag.

## 4. Per-tag payload layouts

The header described in section 2 is identical for every tag. The
payload union starts at offset 8 and is shaped by the tag value. The
three shapes oc-rsync parses are documented below.

### 4.1 SYMLINK payload (`IO_REPARSE_TAG_SYMLINK`)

The payload follows the `SymbolicLinkReparseBuffer` C struct in
`winnt.h`:

```text
            offset (from start of buffer, header included)
            -----------------------------------------------
header      0x00  ReparseTag        u32   = 0xA000_000C
            0x04  ReparseDataLength u16   = (12 + name bytes)
            0x06  Reserved          u16
payload     0x08  SubstituteNameOffset u16
            0x0A  SubstituteNameLength u16
            0x0C  PrintNameOffset      u16
            0x0E  PrintNameLength      u16
            0x10  Flags                u32   (bit 0 = SYMLINK_FLAG_RELATIVE)
            0x14  PathBuffer[...]            UTF-16LE substitute then print name
```

`SubstituteNameOffset` and `PrintNameOffset` are byte offsets
**relative to the start of `PathBuffer`** (not relative to the start
of the reparse buffer). `SubstituteNameLength` and `PrintNameLength`
are byte lengths (so the wide-char count is `length / 2`). Names are
stored without NUL terminators; the kernel writes them back-to-back
in `PathBuffer` (substitute first, print second in practice, though
the parser uses the offsets and does not assume order).

Extraction algorithm (parser at `parse_symlink_reparse`,
`crates/metadata/src/windows/reparse.rs:512`):

1. Validate the tag matches `IO_REPARSE_TAG_SYMLINK`.
2. Validate `8 + ReparseDataLength <= returned_bytes` and that the
   payload covers at least `SubstituteNameOffset + SubstituteNameLength`
   bytes (and the print-name equivalents). The minimum payload size is
   12 bytes (the four `u16`s + the `u32` `Flags`) before any name bytes.
3. Read `SubstituteNameOffset / Length` and `PrintNameOffset / Length`
   as little-endian `u16`s.
4. Read `Flags` as a little-endian `u32`. Bit 0 is
   `SYMLINK_FLAG_RELATIVE` (`0x0000_0001`); when set, the substitute
   name is a path relative to the link's containing directory rather
   than an absolute NT-namespace path.
5. Slice `PathBuffer` (offset `0x14` from buffer start) at
   `[SubstituteNameOffset .. SubstituteNameOffset + SubstituteNameLength]`
   and decode as UTF-16LE via `OsStringExt::from_wide`. Repeat for the
   print name.
6. Return `SymlinkReparseData { substitute_name, print_name, is_relative }`.

Substitute-name encoding: absolute targets carry the NT-namespace
prefix `\??\` (e.g. `\??\C:\Users\Public\Documents`). Relative targets
carry the bare relative path with no prefix (e.g. `..\sibling\file`).
The Win32 `CreateSymbolicLinkW` function handles the prefix
transparently when reconstructing on the destination; oc-rsync passes
the substitute name through to the wire intact.

Print-name encoding: the user-facing string the kernel chose to
display (e.g. `C:\Users\Public\Documents`). May be empty. Callers
that want a human-readable target prefer the print name; callers that
need to recreate the link bit-exactly use the substitute name.

### 4.2 MOUNT_POINT payload (`IO_REPARSE_TAG_MOUNT_POINT`)

Junctions and volume mount-points share the same on-disk shape, the
`MountPointReparseBuffer` C struct from `winnt.h`:

```text
            offset (from start of buffer, header included)
            -----------------------------------------------
header      0x00  ReparseTag        u32   = 0xA000_0003
            0x04  ReparseDataLength u16   = (8 + name bytes)
            0x06  Reserved          u16
payload     0x08  SubstituteNameOffset u16
            0x0A  SubstituteNameLength u16
            0x0C  PrintNameOffset      u16
            0x0E  PrintNameLength      u16
            0x10  PathBuffer[...]            UTF-16LE substitute then print name
```

Same offset/length semantics as the symlink payload, except there is
no `Flags` word - junctions are always absolute, and volume
mount-points always target a volume GUID. `PathBuffer` therefore
starts at offset `0x10` rather than `0x14`. Minimum payload size is 8
bytes (the four `u16`s) before any name bytes.

Extraction algorithm (shared validator
`parse_mount_point_layout`, `crates/metadata/src/windows/reparse.rs:589`,
used by `parse_junction_reparse` and `parse_mount_point_reparse`):

1. Validate the tag matches `IO_REPARSE_TAG_MOUNT_POINT`.
2. Validate `8 + ReparseDataLength <= returned_bytes` and that the
   payload covers `SubstituteNameOffset + SubstituteNameLength` and the
   print-name equivalents.
3. Read the four `u16` offset/length fields little-endian.
4. Slice `PathBuffer` (offset `0x10` from buffer start) at the
   substitute-name and print-name ranges and decode as UTF-16LE.
5. Return `(substitute_name, print_name)` to the caller, which wraps
   it as `JunctionReparseData` or `MountPointReparseData` per the
   classification decided by `classify_mount_point`.

Disambiguation between junction and volume mount-point happens in
`classify_mount_point`
(`crates/metadata/src/windows/reparse.rs`): the substitute name is
case-insensitively compared against the literal prefix
`\??\Volume{` (eight wide characters: `\`, `?`, `?`, `\`, `V`, `o`,
`l`, `u`...). When the prefix matches, the entry is a volume
mount-point; otherwise it is a directory junction. The substitute-name
helper `substitute_name_is_volume` lives in the same file and uses an
explicit `ascii_eq_ignore_case` to match the case-insensitive prefix
without allocating.

### 4.3 APPEXECLINK payload (`IO_REPARSE_TAG_APPEXECLINK`)

oc-rsync does not parse this payload today; AppExecLinks fall into
`ReparseKind::Other(0x8000_001B)`. The shape is documented here for
the eventual opaque-preservation flag (WPC-7 recommendation R4) and
to prevent accidental confusion with the symlink layout.

The payload is **not** a `SymbolicLinkReparseBuffer`. It is a
sequence of four UTF-16LE null-terminated strings packed
back-to-back:

```text
            offset (from start of buffer, header included)
            -----------------------------------------------
header      0x00  ReparseTag        u32   = 0x8000_001B
            0x04  ReparseDataLength u16
            0x06  Reserved          u16
payload     0x08  Version           u32   = 3 (observed)
            0x0C  Package family name  UTF-16LE NUL-terminated
                  Entry-point identifier UTF-16LE NUL-terminated
                  Target executable path UTF-16LE NUL-terminated
                  Application user-model ID  UTF-16LE NUL-terminated
```

The string ordering above matches Microsoft's documented use. The
total payload length is `4 + sum(strlen+1 of each UTF-16LE string)`
bytes. Because the layout is provider-private and may shift between
Windows builds, the WPC-8 design treats the buffer as opaque - the
classifier does not enumerate the strings, and any future preservation
path should store the raw bytes verbatim as a designated xattr.

## 5. High-bit conventions on `ReparseTag`

The tag namespace is partitioned by the high four bits of the
32-bit value:

| Bit | Mask | Meaning |
|-----|------|---------|
| 31 (M) | `0x8000_0000` | Microsoft bit. Set on every Microsoft-allocated tag; cleared on third-party tags. The kernel treats Microsoft-tagged reparses authoritatively. |
| 30 (R) | `0x4000_0000` | Reserved. Must be zero. |
| 29 (N) | `0x2000_0000` | Name-surrogate bit. Set on tags whose reparse handler causes the entry to behave as a name surrogate (the kernel transparently redirects further I/O). |
| 28 (D) | `0x1000_0000` | Directory bit (informational on some tags). Cleared on the tags oc-rsync parses. |

Examples decoded for the tags oc-rsync handles explicitly:

- `IO_REPARSE_TAG_SYMLINK = 0xA000_000C`: bit 31 (M=1) + bit 29
  (N=1) -> Microsoft, surrogate. Kernel redirects to the link
  target on `CreateFileW` unless `FILE_FLAG_OPEN_REPARSE_POINT` is
  set.
- `IO_REPARSE_TAG_MOUNT_POINT = 0xA000_0003`: bit 31 + bit 29 ->
  Microsoft, surrogate. Same redirect semantics.
- `IO_REPARSE_TAG_AF_UNIX = 0x8000_0023`: bit 31 only -> Microsoft,
  non-surrogate. The kernel hands the I/O to the WSL reparse handler;
  no automatic redirect.
- `IO_REPARSE_TAG_CLOUD = 0x9000_001A`: bit 31 + bit 28 (D=1) ->
  Microsoft, name-surrogate (the kernel notifies the cloud provider
  but does not redirect to a different path). The classifier matches
  the entire `0x9000_0010..=0x9000_001F` range to cover every
  provider-allocated slot.

The classifier uses these conventions to decide that any tag not
explicitly enumerated must surface as `ReparseKind::Other(tag)`.
Surrogate bits are not inspected directly in code today; the
constants are matched literally. The bit layout is documented here so
a future reviewer can verify that the surrogate decision implicit in
the tag-to-kind table above is consistent with the kernel's
behaviour.

## 6. Cross-references

Internal documents:

- `docs/audit/windows-reparse-point-classification.md` - WPC-7 audit
  (#2909): every tag currently observable in the wild, the
  user-visible consequences, and the F1-F4 findings.
- `docs/design/wpc-8-reparse-point-classifier.md` - WPC-8 design spec
  (#2910): module placement, public API surface, integration plan
  with the file-list build / receiver / local-copy / batch-replay
  call sites, cfg-gate strategy, and the test taxonomy.
- `docs/design/wpc-9-reparse-point-regression-test.md` - WPC-9
  regression-test spec (#2911): the unit-test fixtures with
  synthetic buffers (this document's payload layouts) and the
  Windows-only integration tests that build live reparse points
  via `mklink` and `DefineDosDevice`.
- `docs/user/windows-support-matrix.md` - the user-facing matrix
  that lists symlink and reparse-point handling as Partial pending
  the WPC-7/8/9 sequence.

Code:

- `crates/metadata/src/windows/reparse.rs` - production implementation
  shipped under PR #5579 (classifier) and commit 6f4d8b182
  (target-name parser). Tests at the bottom of the same file
  exercise `parse_symlink_reparse`, `parse_junction_reparse`, and
  `parse_mount_point_reparse` against synthetic buffers built from
  the layouts in section 4.
- `crates/metadata/src/windows/mod.rs` - module root, gated
  `#[cfg(target_os = "windows")]` per the WPC-8 spec.

External:

- MS-FSCC, section 2.1.2 "REPARSE_DATA_BUFFER":
  https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/c3a420cb-6f2b-4bce-9fe9-1f0e1c95c5e0
- MS-FSCC, section 2.1.2.1 "Reparse Tags":
  https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/12f8c11c-9c83-462d-941b-edb50e1b62ed
- `winnt.h`, Windows SDK 10.0.22621 - canonical source for the
  `IO_REPARSE_TAG_*` constant values and the
  `SymbolicLinkReparseBuffer` / `MountPointReparseBuffer` C structs.

## 7. Open questions

### 7.1 Cloud-files payload structure is undocumented externally

The cloud-files providers (`IO_REPARSE_TAG_CLOUD_0..F`,
`IO_REPARSE_TAG_ONEDRIVE`) store a provider-private blob whose layout
is not part of MS-FSCC. Microsoft documents the user-facing API
(`cldapi.dll`, `CfRegisterSyncRoot`, etc.) but does not publish the
on-disk reparse-buffer shape. Our defensive strategy:

- **Do not parse.** The classifier returns
  `ReparseKind::OneDrive` based on the tag alone; the per-tag
  payload parsers are not invoked.
- **Do not require the buffer.** No code path depends on the
  payload bytes for transfer behaviour. The default (hydrate on
  read) does not need the placeholder header.
- **Reserve the raw buffer for opt-in preservation.** A future
  `--preserve-cloud-placeholders` flag would forward the entire
  `FSCTL_GET_REPARSE_POINT` buffer (header + opaque payload) to
  the destination as a designated xattr (`user.win32.reparse`)
  without inspecting the payload. The destination would call
  `DeviceIoControl(FSCTL_SET_REPARSE_POINT, raw_buffer)` to
  recreate the placeholder. This works only when source and
  destination share the same cloud provider; the placeholder is
  meaningless on a different machine. The opt-in flag is the
  subject of WPC-7 recommendation R2.

### 7.2 WSL POSIX-shadow payloads (`IO_REPARSE_TAG_LX_*`)

`IO_REPARSE_TAG_LX_SYMLINK` (`0xA000_001D`) is documented by the WSL
team to carry the raw UTF-8 POSIX target as the payload, but the
exact pre-amble (whether there is a version word, a length prefix,
or a `CHAR_INFO` header) is not part of MS-FSCC. Microsoft's
in-tree code in `lxcore.sys` is the only reference. Our defensive
strategy mirrors 7.1: classify on tag, do not parse the payload, and
reserve routing through the symlink branch (WPC-7 recommendation R3)
for a follow-up task that can verify the payload shape against a live
WSL install.

`IO_REPARSE_TAG_AF_UNIX`, `LX_FIFO`, `LX_CHR`, `LX_BLK` are similarly
opaque: the major/minor device numbers for `LX_CHR`/`LX_BLK` are
believed to live in the payload but are not externally documented.
Today the classifier surfaces `AfUnix` for the socket case and
`Other(tag)` for the rest; preservation is deferred.

### 7.3 AppExecLink payload version drift

Section 4.3 documents the shape observed against Windows 11. The
`Version` word is the only field with a documented purpose, and its
value has been `3` in every observed build. Microsoft does not
guarantee the layout across releases, so any future preservation path
should store the raw buffer (xattr-passthrough) rather than re-emitting
the four UTF-16LE strings; preserving via the four-string form risks
drift if Microsoft adds a fifth field.

### 7.4 Tag-registry drift

Microsoft has added new tags in every recent Windows release. The
classifier matches a fixed set today; any unknown tag surfaces as
`ReparseKind::Other(tag)`. The `Other(u32)` payload preserves the raw
value so a future log line can name the tag without code change. New
explicit `ReparseKind` variants land as recommended-handling decisions
are made; no protocol or wire change is required.
