# Reparse-point classifier implementation spec (WPC-8)

Tracks parent #2869 (Windows real-world parity series). Follows WPC-7
(#2909, `docs/audit/windows-reparse-point-classification.md`). Feeds
WPC-9 (#2911, regression tests).

## 1. Summary of WPC-7 findings

The WPC-7 audit established that oc-rsync recognises exactly two
reparse tags today - both implicitly through Rust's `std::fs::FileType`
predicates:

- `IO_REPARSE_TAG_SYMLINK` (`0xA000_000C`) - reported as `is_symlink()`.
- `IO_REPARSE_TAG_MOUNT_POINT` (`0xA000_0003`) - also reported as
  `is_symlink()` since Rust 1.49.

Every other tag collapses to `is_file()` or `is_dir()` based solely on
`FILE_ATTRIBUTE_DIRECTORY`. The consequences documented in WPC-7:

- **Cloud placeholders** (`IO_REPARSE_TAG_CLOUD*`) silently trigger
  full hydration on every transfer, defeating Files-On-Demand.
- **WSL Linux symlinks** (`IO_REPARSE_TAG_LX_SYMLINK`) are shipped as
  regular files containing the POSIX target string - the symlink is
  lost.
- **WSL special files** (AF_UNIX, FIFO, CHR, BLK) collapse to regular
  files with provider-private content.
- **AppExecLink** reparse points produce non-functional opaque blobs
  on the destination.
- **Junctions** are indistinguishable from symlinks; volume mount
  points cannot be detected or warned about.

No `IO_REPARSE_TAG_*` constants, no `FSCTL_GET_REPARSE_POINT` calls,
and no reparse-buffer parsing exist anywhere in the workspace.

## 2. Module placement

```
crates/metadata/src/
  windows/
    mod.rs          -- #[cfg(windows)] module root
    reparse.rs      -- classifier, buffer reader, tag constants
  lib.rs            -- re-exports via `#[cfg(windows)] pub mod windows;`
```

The `windows/` submodule follows the same pattern as
`crates/metadata/src/acl_windows/` (directory module with `mod.rs`
plus focused submodules). Starting with a single `reparse.rs` keeps
the initial surface small; future WPC work (DACL inheritance, etc.)
can add sibling files under `windows/`.

A non-Windows stub (`reparse_stub.rs`) is not needed: callers will
gate on `#[cfg(windows)]` at the call site (matching the existing
`acl_windows` and `xattr_windows` pattern where non-Windows builds
simply do not compile the module).

## 3. Public API

### 3.1 `ReparseKind` enum

```rust
/// Classification of an NTFS reparse point by its reparse tag.
///
/// The variants cover every tag family that oc-rsync must distinguish
/// to produce correct transfer behaviour. Unrecognised tags are
/// captured in `Other(u32)` so callers can log the raw value and fall
/// back to a documented default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReparseKind {
    /// Genuine NTFS symbolic link (`IO_REPARSE_TAG_SYMLINK`, 0xA000_000C).
    Symlink,

    /// NTFS junction - a directory pointing to another directory path
    /// on the same or different volume. Distinguished from `MountPoint`
    /// by the substitute-name prefix in the reparse buffer:
    /// junctions use `\??\<drive>:\...`, mount points use
    /// `\??\Volume{GUID}\`.
    Junction,

    /// Volume mount point - a directory that redirects to a different
    /// volume's root. Shares `IO_REPARSE_TAG_MOUNT_POINT` (0xA000_0003)
    /// with `Junction`; distinguished by the `\??\Volume{GUID}\`
    /// substitute-name prefix.
    MountPoint,

    /// Cloud Files API placeholder (OneDrive, Dropbox, etc.).
    /// The inner `u32` is the full tag so callers can distinguish
    /// sub-variants in the 0x9000_001A..0x9000_031A range.
    Cloud(u32),

    /// Windows Store app-execution alias
    /// (`IO_REPARSE_TAG_APPEXECLINK`, 0x8000_001B).
    AppExecLink,

    /// Windows Container Isolation
    /// (`IO_REPARSE_TAG_WCI`, 0x8000_0018).
    Wci,

    /// Bind-mount equivalent
    /// (`IO_REPARSE_TAG_GLOBAL_REPARSE`, 0xA000_0019).
    GlobalReparse,

    /// WSL POSIX symbolic link
    /// (`IO_REPARSE_TAG_LX_SYMLINK`, 0xA000_001D).
    LxSymlink,

    /// WSL POSIX FIFO
    /// (`IO_REPARSE_TAG_LX_FIFO`, 0x8000_0024).
    LxFifo,

    /// WSL POSIX character device
    /// (`IO_REPARSE_TAG_LX_CHR`, 0x8000_0025).
    LxChr,

    /// WSL POSIX block device
    /// (`IO_REPARSE_TAG_LX_BLK`, 0x8000_0026).
    LxBlk,

    /// WSL AF_UNIX socket shadow
    /// (`IO_REPARSE_TAG_AF_UNIX`, 0x8000_0023).
    AfUnix,

    /// Projected File System (VFS-for-Git, GVFS)
    /// (`IO_REPARSE_TAG_PROJFS`, 0x9000_001C).
    ProjFs,

    /// Windows Overlay Filter (compact OS)
    /// (`IO_REPARSE_TAG_WOF`, 0x8000_0017).
    Wof,

    /// Hierarchical Storage Manager (v1 or v2)
    /// (`IO_REPARSE_TAG_HSM`, 0xC000_0004 or
    /// `IO_REPARSE_TAG_HSM2`, 0x8000_0006).
    Hsm,

    /// Any Microsoft-registered tag not explicitly handled above.
    /// The `u32` is the raw reparse tag for logging.
    Other(u32),
}
```

### 3.2 `classify_reparse` pure function

```rust
/// Maps a raw reparse tag to a `ReparseKind`.
///
/// This is the single source of truth for tag-to-kind mapping in
/// the workspace. Every call site that branches on a reparse tag
/// must go through this function.
///
/// The function is pure: no I/O, no allocation. It handles the
/// `IO_REPARSE_TAG_MOUNT_POINT` tag by returning `Junction` as a
/// default; callers that need the junction-vs-mount-point
/// distinction must inspect the substitute-name prefix in the
/// `ReparseData` returned by `read_reparse_data`.
pub fn classify_reparse(tag: u32) -> ReparseKind;
```

Tag-to-kind dispatch logic:

| Tag value(s) | Returned variant |
|---|---|
| `0xA000_000C` | `Symlink` |
| `0xA000_0003` | `Junction` (disambiguated to `MountPoint` only after buffer inspection) |
| `0x9000_001A..=0x9000_031A` | `Cloud(tag)` |
| `0x8000_001B` | `AppExecLink` |
| `0x8000_0018` | `Wci` |
| `0xA000_0019` | `GlobalReparse` |
| `0xA000_001D` | `LxSymlink` |
| `0x8000_0024` | `LxFifo` |
| `0x8000_0025` | `LxChr` |
| `0x8000_0026` | `LxBlk` |
| `0x8000_0023` | `AfUnix` |
| `0x9000_001C` | `ProjFs` |
| `0x8000_0017` | `Wof` |
| `0xC000_0004`, `0x8000_0006` | `Hsm` |
| anything else | `Other(tag)` |

The function is a `match` over constants defined in section 5. The
`CLOUD` family is detected with a range check rather than 16 individual
arms.

### 3.3 `ReparseData` struct and `read_reparse_data` function

```rust
/// Parsed reparse-point metadata read from an NTFS path.
#[derive(Debug, Clone)]
pub struct ReparseData {
    /// Classified kind, refined from the tag and buffer contents.
    /// For `IO_REPARSE_TAG_MOUNT_POINT`, this is `Junction` or
    /// `MountPoint` depending on the substitute-name prefix.
    pub kind: ReparseKind,

    /// Substitute-name target for symlinks, junctions, mount points,
    /// and WSL Linux symlinks. `None` for cloud placeholders,
    /// AppExecLink, and other opaque kinds.
    pub target: Option<PathBuf>,

    /// The verbatim `FSCTL_GET_REPARSE_POINT` buffer. Retained so a
    /// downstream consumer can preserve it as an opaque xattr when
    /// round-tripping reparse data.
    pub raw: Vec<u8>,
}

/// Reads and classifies the reparse point attached to `path`.
///
/// Opens the path with `CreateFileW` using
/// `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`,
/// issues `DeviceIoControl(FSCTL_GET_REPARSE_POINT, ...)`, parses
/// the tag and substitute-name buffer, and returns the structured
/// value.
///
/// # Errors
///
/// Returns `io::Error` if the path cannot be opened, the path does
/// not carry a reparse point, or the reparse buffer is malformed.
pub fn read_reparse_data(path: &Path) -> io::Result<ReparseData>;
```

### 3.4 `has_reparse_point` predicate

```rust
/// Returns `true` if `attrs` has the `FILE_ATTRIBUTE_REPARSE_POINT`
/// bit set. This is a cheap bit-test on the `dwFileAttributes` value
/// from `GetFileAttributesW` or `WIN32_FIND_DATAW`.
pub fn has_reparse_point(attrs: u32) -> bool {
    attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
}
```

This predicate is the fast-path gate: callers check
`has_reparse_point(attrs)` before issuing the more expensive
`read_reparse_data` call.

## 4. Implementation approach

### 4.1 Opening the reparse point

`CreateFileW` with flags:

- `FILE_FLAG_OPEN_REPARSE_POINT` - prevents the kernel from
  following the reparse handler, returning the reparse-point
  container itself.
- `FILE_FLAG_BACKUP_SEMANTICS` - required to open directories
  (junctions, mount points, container isolation).
- `GENERIC_READ` access is not needed; `FILE_READ_ATTRIBUTES` (or
  even zero access) suffices because `DeviceIoControl` with
  `FSCTL_GET_REPARSE_POINT` reads from the file-system metadata,
  not the data stream.

### 4.2 Reading the reparse buffer

```rust
let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
let mut bytes_returned: u32 = 0;
DeviceIoControl(
    handle,
    FSCTL_GET_REPARSE_POINT,
    None,       // no input buffer
    0,          // input size
    Some(buffer.as_mut_ptr().cast()),
    buffer.len() as u32,
    Some(&mut bytes_returned),
    None,       // no overlapped
)?;
buffer.truncate(bytes_returned as usize);
```

`MAXIMUM_REPARSE_DATA_BUFFER_SIZE` is 16384 bytes (defined in
`winnt.h`). Stack-allocating a 16 KB buffer is acceptable for a
per-path operation that is already behind an I/O call.

### 4.3 Parsing the reparse buffer

The first 8 bytes of every reparse buffer share a common header:

```
offset 0: u32  ReparseTag
offset 4: u16  ReparseDataLength
offset 6: u16  Reserved
offset 8: ...  type-specific payload
```

For `IO_REPARSE_TAG_SYMLINK` (tag `0xA000_000C`), the payload at
offset 8 is:

```
offset  8: u16  SubstituteNameOffset  (relative to offset 20)
offset 10: u16  SubstituteNameLength  (bytes, not chars)
offset 12: u16  PrintNameOffset
offset 14: u16  PrintNameLength
offset 16: u32  Flags (0 = absolute, 1 = relative)
offset 20: [u8] PathBuffer (UTF-16LE, contains both names)
```

For `IO_REPARSE_TAG_MOUNT_POINT` (tag `0xA000_0003`), the payload
at offset 8 is:

```
offset  8: u16  SubstituteNameOffset  (relative to offset 16)
offset 10: u16  SubstituteNameLength
offset 12: u16  PrintNameOffset
offset 14: u16  PrintNameLength
offset 16: [u8] PathBuffer (UTF-16LE)
```

The difference: mount-point / junction buffers have no `Flags` field.

The substitute-name is the string that disambiguates junctions from
volume mount points:

- `\??\Volume{<GUID>}\` - volume mount point. Return
  `ReparseKind::MountPoint`.
- `\??\<drive>:\...` or `\??\UNC\...` - junction. Return
  `ReparseKind::Junction`.

For `IO_REPARSE_TAG_LX_SYMLINK` (tag `0xA000_001D`), the payload
at offset 8 is 4 bytes of flags followed by the raw UTF-8 POSIX
target string (no null terminator required but one may be present).

All other tags: the substitute-name is not parsed. The raw buffer
is returned verbatim; `target` is `None`.

### 4.4 Unsafe code policy

The `metadata` crate carries `#![deny(unsafe_code)]`. The reparse
buffer parsing itself is safe (byte-slice indexing and
`u16::from_le_bytes` on checked slices). The FFI call to
`DeviceIoControl` goes through the `windows` crate's safe wrappers
(`windows::Win32::System::IO::DeviceIoControl`), which are already
depended on by `acl_windows`. The `CreateFileW` call similarly uses
the safe `windows::Win32::Storage::FileSystem::CreateFileW` wrapper.

No `#[allow(unsafe_code)]` annotation is needed. If any future
revision requires raw pointer manipulation (e.g., casting the
reparse buffer to a layout struct), the unsafe code must be moved
to `crates/fast_io` and exposed through a safe public API, per the
project's unsafe code policy.

## 5. Windows API types and constants

All constants are defined as `pub(crate) const` in `reparse.rs`:

```rust
// Reparse tags from winnt.h (Windows SDK 10.0.22621)
pub(crate) const IO_REPARSE_TAG_MOUNT_POINT: u32   = 0xA000_0003;
pub(crate) const IO_REPARSE_TAG_HSM: u32            = 0xC000_0004;
pub(crate) const IO_REPARSE_TAG_HSM2: u32           = 0x8000_0006;
pub(crate) const IO_REPARSE_TAG_SYMLINK: u32        = 0xA000_000C;
pub(crate) const IO_REPARSE_TAG_WOF: u32            = 0x8000_0017;
pub(crate) const IO_REPARSE_TAG_WCI: u32            = 0x8000_0018;
pub(crate) const IO_REPARSE_TAG_GLOBAL_REPARSE: u32 = 0xA000_0019;
pub(crate) const IO_REPARSE_TAG_CLOUD_MIN: u32      = 0x9000_001A;
pub(crate) const IO_REPARSE_TAG_CLOUD_MAX: u32      = 0x9000_031A;
pub(crate) const IO_REPARSE_TAG_APPEXECLINK: u32    = 0x8000_001B;
pub(crate) const IO_REPARSE_TAG_PROJFS: u32         = 0x9000_001C;
pub(crate) const IO_REPARSE_TAG_LX_SYMLINK: u32     = 0xA000_001D;
pub(crate) const IO_REPARSE_TAG_AF_UNIX: u32        = 0x8000_0023;
pub(crate) const IO_REPARSE_TAG_LX_FIFO: u32        = 0x8000_0024;
pub(crate) const IO_REPARSE_TAG_LX_CHR: u32         = 0x8000_0025;
pub(crate) const IO_REPARSE_TAG_LX_BLK: u32         = 0x8000_0026;

pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

pub(crate) const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16384;
```

The `windows` crate dependency in `Cargo.toml` already includes
`Win32_Storage_FileSystem` and `Win32_Foundation`. The only
additional feature needed is `Win32_System_IO` for
`DeviceIoControl`. The `FSCTL_GET_REPARSE_POINT` constant
(`0x000900A8`) is provided by `Win32_System_Ioctl`; if the
`windows` crate does not expose it under an importable feature, it
is defined inline as a constant.

Required `Cargo.toml` change:

```toml
[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Security_Authorization",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_Ioctl",
    "Win32_System_SystemServices",
    "Win32_System_Threading",
] }
```

## 6. Integration with file-entry metadata population

### 6.1 File-list build (sender side)

The integration point is the file-list entry construction in
`crates/transfer/src/generator/file_list/entry.rs` and
`crates/flist/src/file_list_walker.rs`. The change is:

1. After `fs::symlink_metadata(path)`, check
   `has_reparse_point(attrs)` on the raw `dwFileAttributes`.
2. If the reparse-point bit is set, call `read_reparse_data(path)`.
3. Match on `reparse_data.kind`:
   - `Symlink | Junction` - route to the existing symlink branch
     using `reparse_data.target` as the link target.
   - `MountPoint` - emit a warning ("skipping volume mount point:
     <path>") and skip the entry. Volume mount points cannot be
     safely reconstructed on the destination.
   - `LxSymlink` - route to the existing symlink branch using the
     decoded POSIX target from the reparse buffer.
   - `LxFifo | LxChr | LxBlk | AfUnix` - route to the existing
     special-file branch (device/FIFO). On non-Unix destinations
     the entry is skipped with a warning.
   - `Cloud(_)` - continue with the current behaviour (treat as
     regular file, triggering hydration). Emit a one-shot INFO log
     on the first dehydrated file encountered.
   - `AppExecLink | Wci` - skip the entry with a per-path warning.
     The file body is provider-private and non-functional on the
     destination.
   - `Wof | Hsm | ProjFs | GlobalReparse | Other(_)` - treat as
     regular file (current behaviour). These tags either
     auto-hydrate transparently or represent storage optimisations
     where shipping the real bytes is correct.

### 6.2 Receiver / apply side

No receiver-side changes are required for WPC-8. The classifier is
a sender-side inspection tool. Receiver-side reparse-point
reconstruction (e.g., recreating junctions instead of symlinks on
Windows destinations) is deferred to a follow-up task.

### 6.3 Local-copy executor

`crates/engine/src/local_copy/executor/special/symlink.rs` currently
probes the source path's metadata to choose between `symlink_dir`
and `symlink_file`. After WPC-8, when the source `ReparseKind` is
available through the file-entry metadata, the executor can use the
kind directly:

- `Junction` - create with `symlink_dir` (junctions always target
  directories).
- `Symlink` - check the target type from `ReparseData.target` via
  `fs::metadata` (existing behaviour, but now only for genuine
  symlinks).
- `LxSymlink` - create with `symlink_file` on Windows, or
  `std::os::unix::fs::symlink` on Unix.

### 6.4 Batch replay

`crates/batch/src/replay/fs_ops.rs` hard-codes `symlink_file`. After
WPC-8, the batch file format can carry the `ReparseKind` discriminant
so the replay chooses the correct symlink variant. This is a follow-up
change that depends on wire-format encoding decisions outside WPC-8's
scope.

## 7. cfg gates and platform stubs

### 7.1 Module visibility

```rust
// In crates/metadata/src/lib.rs:
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::reparse::{
    ReparseData, ReparseKind, classify_reparse, has_reparse_point,
    read_reparse_data,
};
```

### 7.2 Non-Windows builds

The module is not compiled on non-Windows targets. Callers that need
to branch on reparse-point classification gate their call sites with
`#[cfg(windows)]`:

```rust
#[cfg(windows)]
{
    if metadata::has_reparse_point(attrs) {
        let data = metadata::read_reparse_data(&path)?;
        match data.kind {
            // ...
        }
    }
}
```

No stub module is needed. The `acl_windows` and `xattr_windows`
modules follow this same pattern: they exist only on Windows, and
callers gate their usage with `#[cfg(windows)]`.

### 7.3 Test compilation

Unit tests for `classify_reparse` (which is pure logic over `u32`
constants) are placed in a `#[cfg(test)]` module inside `reparse.rs`.
These tests compile and run on all platforms because `classify_reparse`
has no Windows dependencies - it is a pure `match` over integer
constants.

Integration tests that call `read_reparse_data` (which requires
`DeviceIoControl`) are gated with `#[cfg(all(test, windows))]` and
live in a separate `tests/` submodule or in a `#[cfg(windows)]`
test module.

## 8. Test strategy

### 8.1 Unit tests (all platforms)

`classify_reparse` is a pure function that maps `u32` to
`ReparseKind`. The unit tests are exhaustive over every named
constant:

```rust
#[test]
fn classify_symlink_tag() {
    assert_eq!(classify_reparse(0xA000_000C), ReparseKind::Symlink);
}

#[test]
fn classify_mount_point_tag() {
    assert_eq!(classify_reparse(0xA000_0003), ReparseKind::Junction);
}

#[test]
fn classify_cloud_range() {
    assert_eq!(
        classify_reparse(0x9000_001A),
        ReparseKind::Cloud(0x9000_001A),
    );
    assert_eq!(
        classify_reparse(0x9000_031A),
        ReparseKind::Cloud(0x9000_031A),
    );
    // Just outside the range
    assert!(matches!(classify_reparse(0x9000_0019), ReparseKind::Other(_)));
    assert!(matches!(classify_reparse(0x9000_031B), ReparseKind::Other(_)));
}

#[test]
fn classify_wsl_tags() {
    assert_eq!(classify_reparse(0xA000_001D), ReparseKind::LxSymlink);
    assert_eq!(classify_reparse(0x8000_0024), ReparseKind::LxFifo);
    assert_eq!(classify_reparse(0x8000_0025), ReparseKind::LxChr);
    assert_eq!(classify_reparse(0x8000_0026), ReparseKind::LxBlk);
    assert_eq!(classify_reparse(0x8000_0023), ReparseKind::AfUnix);
}

#[test]
fn classify_unknown_tag() {
    assert_eq!(classify_reparse(0xDEAD_BEEF), ReparseKind::Other(0xDEAD_BEEF));
}
```

### 8.2 Buffer-parsing tests (all platforms)

Construct mock reparse buffers as `&[u8]` slices and feed them to an
internal `parse_reparse_buffer(buf: &[u8]) -> io::Result<ReparseData>`
function that is factored out of `read_reparse_data` for testability.
This function takes raw bytes and returns `ReparseData` without any
Win32 calls.

Test cases:

- **Symlink buffer**: valid `IO_REPARSE_TAG_SYMLINK` header with a
  UTF-16LE substitute-name `\??\C:\target\file.txt`. Assert `kind ==
  Symlink`, `target == Some("\\??\\C:\\target\\file.txt")`.
- **Junction buffer**: valid `IO_REPARSE_TAG_MOUNT_POINT` header with
  substitute-name `\??\C:\Users\Public`. Assert `kind == Junction`.
- **Volume mount-point buffer**: valid `IO_REPARSE_TAG_MOUNT_POINT`
  header with substitute-name `\??\Volume{GUID}\`. Assert
  `kind == MountPoint`.
- **LxSymlink buffer**: valid `IO_REPARSE_TAG_LX_SYMLINK` header with
  4-byte flags + UTF-8 target `/home/user/link`. Assert
  `kind == LxSymlink`, `target == Some("/home/user/link")`.
- **Truncated buffer**: header-only buffer (< 8 bytes). Assert
  `Err(io::ErrorKind::InvalidData)`.
- **Truncated substitute-name**: header claims a substitute-name
  length that exceeds the buffer. Assert
  `Err(io::ErrorKind::InvalidData)`.
- **Cloud tag**: buffer with tag `0x9000_001A`, minimal header.
  Assert `kind == Cloud(0x9000_001A)`, `target == None`.
- **AppExecLink**: buffer with tag `0x8000_001B`. Assert
  `kind == AppExecLink`, `target == None`.

### 8.3 Integration tests (Windows CI only)

These tests create real reparse points on the filesystem and verify
the full `read_reparse_data` round-trip. Gated with
`#[cfg(all(test, windows))]`.

- **Symlink**: create with `std::os::windows::fs::symlink_file`.
  Assert `kind == Symlink` and `target` matches the provided path.
- **Directory symlink**: create with
  `std::os::windows::fs::symlink_dir`. Assert `kind == Symlink`.
- **Junction**: create with `mklink /J` via `std::process::Command`
  or the `junction` crate. Assert `kind == Junction`.
- **Non-reparse file**: regular file without
  `FILE_ATTRIBUTE_REPARSE_POINT`. Assert `has_reparse_point` returns
  `false`.
- **Cloud / AppExecLink / WSL tags**: these require third-party
  providers or WSL and are gated with `#[ignore]`. Documented manual
  reproduction recipe in the test docstring.

### 8.4 Coverage target

All `classify_reparse` arms must be covered. All `parse_reparse_buffer`
error paths must be covered. The integration tests contribute to
coverage only on Windows CI runners. Target: 100% branch coverage on
`classify_reparse` and `parse_reparse_buffer`; best-effort on
`read_reparse_data` (I/O-dependent).

## 9. Centralisation invariant

After WPC-8, there must be exactly one `classify_reparse` definition
and exactly one `read_reparse_data` definition in the workspace.
Existing call sites that implicitly classify reparse points through
`std::fs::FileType::is_symlink()` must be updated to call through
the classifier when the reparse-point attribute bit is set.

The migration is incremental: WPC-8 lands the module and its tests.
Follow-up tasks wire the classifier into the file-list build
(section 6.1), local-copy executor (section 6.3), and batch replay
(section 6.4).

## 10. Acceptance criteria

- [ ] `crates/metadata/src/windows/reparse.rs` exists with
      `ReparseKind`, `ReparseData`, `classify_reparse`,
      `parse_reparse_buffer`, `read_reparse_data`, and
      `has_reparse_point`.
- [ ] `classify_reparse` covers all tags listed in section 3.2.
- [ ] `parse_reparse_buffer` correctly decodes symlink, junction,
      mount-point, and LxSymlink buffers.
- [ ] Unit tests for `classify_reparse` pass on all platforms.
- [ ] Buffer-parsing tests for `parse_reparse_buffer` pass on all
      platforms.
- [ ] Integration tests for `read_reparse_data` pass on Windows CI.
- [ ] No `#[allow(unsafe_code)]` added to the metadata crate.
- [ ] `Cargo.toml` updated with `Win32_System_IO` and
      `Win32_System_Ioctl` features.
- [ ] `lib.rs` re-exports the public API behind `#[cfg(windows)]`.
- [ ] Existing fmt, clippy, and nextest CI checks pass on all
      platforms.

## 11. Cross-references

Internal:

- `docs/audit/windows-reparse-point-classification.md` (WPC-7,
  #2909) - the audit this spec implements.
- `docs/design/windows-ads-strategy.md` (WPC-2) - parallel xattr
  pipeline pattern.
- `docs/user/windows-support-matrix.md` (WPC-13, #4920) -
  user-facing matrix listing reparse support as Partial.

Tracking:

- Parent: **#2869** (Windows real-world parity series).
- This document: **#2910** (WPC-8).
- Follow-up: **#2911** (WPC-9, regression tests).
