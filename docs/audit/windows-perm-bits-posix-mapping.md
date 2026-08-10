# Windows file-attribute - POSIX mode-bit round-trip audit (WPC-12)

Tracks parent #2869 (Windows real-world parity series). Sibling
#2915 / PR #4920 (WPC-13 Windows support matrix) already names the
read-only mapping and lists the hidden / system / archive gap; this
audit drills into the conversion sites, the wire encoding, the test
coverage, and the concrete follow-up tasks. Memory notes
`project_windows_real_world_parity_unclear` and
`project_windows_parity_wip` flag this as an open parity gap.

## 1. Scope

WPC-12 audits the bidirectional Win32 file-attribute - POSIX
mode-bit mapping in oc-rsync. Specifically:

- Read-side mapping when the **source** is Windows: how the sender
  derives the wire-format mode_t from the host file's permissions.
- Write-side mapping when the **destination** is Windows: how the
  receiver materialises the wire-format mode_t back onto the NTFS
  file.
- The wire encoding itself: what mode_t crosses the wire and what
  it cannot represent.
- The fidelity gap for the other NTFS attribute bits
  (`FILE_ATTRIBUTE_HIDDEN`, `FILE_ATTRIBUTE_SYSTEM`,
  `FILE_ATTRIBUTE_ARCHIVE`) in both directions.

Out of scope:

- NTFS DACL / ACL preservation. Covered by WPC-10 (`-A`) and the
  ACL direction matrix at `docs/audit/acl-xattr-direction-matrix.md`.
- Reparse-point classification. Covered by WPC-7 / WPC-8 / WPC-9.
- Alternate data stream preservation. Covered by WPC-1 / WPC-2 /
  WPC-3 / WPC-4.

## 2. Background

NTFS exposes a **file-attribute** namespace that is separate from
the DACL permission namespace:

- `FILE_ATTRIBUTE_READONLY` (0x01) - file cannot be modified.
- `FILE_ATTRIBUTE_HIDDEN` (0x02) - file not shown by default in
  Explorer.
- `FILE_ATTRIBUTE_SYSTEM` (0x04) - system file.
- `FILE_ATTRIBUTE_DIRECTORY` (0x10) - set on directories.
- `FILE_ATTRIBUTE_ARCHIVE` (0x20) - modification marker. Set on
  write, cleared by backup software.
- `FILE_ATTRIBUTE_NORMAL` (0x80) - placeholder value meaning no
  other flags. Only valid in `CreateFileW` `dwFlagsAndAttributes`
  when used alone.
- Other lesser-known bits (`FILE_ATTRIBUTE_TEMPORARY`,
  `FILE_ATTRIBUTE_OFFLINE`, `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED`,
  `FILE_ATTRIBUTE_ENCRYPTED`, `FILE_ATTRIBUTE_COMPRESSED`,
  `FILE_ATTRIBUTE_SPARSE_FILE`, `FILE_ATTRIBUTE_REPARSE_POINT`).

POSIX `mode_t` is unrelated:

- Owner read / write / execute (0o400 / 0o200 / 0o100).
- Group read / write / execute (0o040 / 0o020 / 0o010).
- Other read / write / execute (0o004 / 0o002 / 0o001).
- Set-uid / set-gid / sticky (0o4000 / 0o2000 / 0o1000).
- High type bits (S_IFDIR, S_IFREG, S_IFLNK, ...).

Conventional Cygwin / WSL mappings (lossy in both directions):

- `FILE_ATTRIBUTE_READONLY` <-> strip owner / group / other write
  bits.
- `FILE_ATTRIBUTE_DIRECTORY` <-> `S_IFDIR`.
- `FILE_ATTRIBUTE_HIDDEN` / `SYSTEM` / `ARCHIVE` have no canonical
  POSIX equivalent. Cygwin's `chmod +H` semantics are non-standard
  and not exposed via the rsync wire protocol.

In the reverse direction:

- `mode_t` with no write bit anywhere -> set
  `FILE_ATTRIBUTE_READONLY`.
- Other POSIX bits do not project onto Windows attributes and are
  typically ignored. Suid / sgid / sticky are no-ops on NTFS.

## 3. Inventory of current attribute / mode handling

Grep of `crates/metadata/src/` and `crates/fast_io/src/` for the
Windows attribute constants, the Win32 entry points, and the
mode-conversion helpers.

### 3.1 `FILE_ATTRIBUTE_*` constants

All hits use `FILE_ATTRIBUTE_NORMAL` as the `CreateFileW` placeholder
value, never the semantic READONLY / HIDDEN / SYSTEM / ARCHIVE bits:

- `crates/metadata/src/xattr_windows.rs:55` -
  `use ... FILE_ATTRIBUTE_NORMAL`.
- `crates/metadata/src/xattr_windows.rs:274` - passed to
  `CreateFileW` when opening an alternate data stream for write.
- `crates/metadata/src/xattr_windows.rs:318` - same, on read.
- `crates/fast_io/src/iocp/file_writer.rs:12,90` - IOCP file writer
  passes `FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED`.
- `crates/fast_io/src/iocp/file_reader.rs:13,52` - same for read.
- `crates/fast_io/src/iocp/pump.rs:566,627` - same.
- `crates/fast_io/src/iocp/disk_batch/tests.rs:9,38,287` - tests.
- `crates/fast_io/src/platform_copy/dispatch.rs:289,376,402,510,588,613` -
  `CopyFileExW` source / destination handle creation.
- `crates/fast_io/tests/iocp_disk_full_simulation.rs:28,71` -
  integration test.
- `crates/fast_io/tests/iocp_partial_write_integration.rs:47,104,136` -
  integration test.

**No hit for `FILE_ATTRIBUTE_READONLY`, `FILE_ATTRIBUTE_HIDDEN`,
`FILE_ATTRIBUTE_SYSTEM`, or `FILE_ATTRIBUTE_ARCHIVE` anywhere in
the workspace.** The codebase never reads or writes those bits
directly via the Win32 constant; the read-only flag is only ever
touched through the Rust standard library wrapper
`Permissions::readonly()` / `set_readonly()`.

### 3.2 Win32 attribute entry points

Grep for `GetFileAttributesW`, `SetFileAttributesW`,
`GetFileAttributesExW`, `GetFileInformationByHandle`: **zero hits
across `crates/` and `tests/`**. The crate never calls those
entry points directly. All attribute reads go through
`std::fs::metadata()` -> `MetadataExt::file_attributes()` (which is
only used in stat-cache hashing, not in mode derivation) or through
`Permissions::readonly()`. All attribute writes go through
`Permissions::set_readonly()`.

### 3.3 Conversion helpers

Grep for `attrs_to_mode`, `mode_to_attrs`, `FileAttributes`,
`file_attributes`: **zero hits**. There is no named helper that
performs the bidirectional translation. The mapping is open-coded
at the four sites listed in 3.4 and 3.5.

### 3.4 Read-side conversion sites (Windows -> POSIX mode_t)

Single site in the sender / generator:

- `crates/transfer/src/generator/file_list/entry.rs:53-74` -
  `GeneratorContext::create_entry()`. The Windows branch reads
  `metadata.permissions().readonly()` and emits a wire mode of
  `0o444` (read-only) or `0o644` (read-write) for regular files,
  and a hard-coded `0o755` for directories
  (`entry.rs:75-81`). No other attribute bits are consulted; no
  group / other / suid / sgid / sticky bits are derived; HIDDEN,
  SYSTEM, ARCHIVE are silently dropped.

### 3.5 Write-side conversion sites (POSIX mode_t -> Windows)

Three sites apply mode_t to a Windows destination. All three use
the same convention: owner write bit absent -> `set_readonly(true)`,
else `set_readonly(false)`. None of them touch HIDDEN, SYSTEM, or
ARCHIVE, and none of them preserve those bits across an overwrite.

- `crates/metadata/src/apply/permissions.rs:336-357` -
  `apply_permissions_from_entry()` non-Unix branch. Derives
  `readonly = entry.permissions() & 0o200 == 0`, reads the current
  destination permissions, mutates the read-only flag if it
  differs, writes back via `fs::set_permissions`. The intermediate
  `Permissions` object is a Rust wrapper that only mutates the
  read-only flag, so other attributes survive the
  `set_permissions` call by virtue of the standard library
  preserving them.
- `crates/metadata/src/apply_batch.rs:171-203` -
  `BatchMetadataContext::apply_permissions_cached()` non-Unix
  branch. Same logic via `metadata.permissions().readonly()` ->
  `set_readonly` -> `fs::set_permissions`.
- `crates/metadata/src/apply/permissions.rs:39-50` -
  `set_permissions_like()` non-Unix branch. Same logic when
  applying source metadata directly (used by `--archive`-style
  local copies and chmod-less paths).
- `crates/fast_io/src/syscall_batch.rs:374-383` -
  `set_file_permissions()` non-Unix fallback. Same logic. This is
  the syscall-batched write path that mirrors the
  `apply_permissions_from_entry` logic for the IOCP / batched
  writer.

### 3.6 Wire-format mode_t accessor

- `crates/protocol/src/flist/entry/accessors.rs:150-160` -
  `FileEntry::permissions()` returns the wire-format mode bits
  masked to permission space (no high type bits).
- `crates/protocol/src/flist/entry/constructors.rs:23-41` -
  `FileEntry::new_with_type()` packs `file_type.to_mode_bits() |
  (permissions & 0o7777)` into `mode`. The wire mode is a single
  `u32` containing type bits + 12-bit permission word. There is no
  Windows-attribute side-channel field on the entry.
- `crates/protocol/src/flist/write/mod.rs` and
  `crates/protocol/src/flist/read/mod.rs` - serialise and
  deserialise this same mode field. Grep for `FILE_ATTRIBUTE`,
  `hidden`, `system_attr`, `archive_attr`, `windows`: **zero
  hits**. The wire format has no slot for Windows-only attribute
  bits.

## 4. Read-side mapping (Windows -> POSIX)

When the sender enumerates a Windows source file, the only Win32
attribute it consults is the read-only flag, surfaced through
`std::fs::Permissions::readonly()`:

```text
crates/transfer/src/generator/file_list/entry.rs:53-74

let mut entry = if file_type.is_file() {
    #[cfg(unix)]
    let mode = metadata.mode() & 0o7777;
    #[cfg(not(unix))]
    let mode = if metadata.permissions().readonly() {
        0o444
    } else {
        0o644
    };
    ...
    FileEntry::new_file(relative_path, metadata.len(), mode)
} else if file_type.is_dir() {
    #[cfg(unix)]
    let mode = metadata.mode() & 0o7777;
    #[cfg(not(unix))]
    let mode = 0o755;
    FileEntry::new_directory(relative_path, mode)
}
```

Consequences:

- `FILE_ATTRIBUTE_READONLY` -> the wire mode loses the 0o200 owner
  write bit (and 0o020 / 0o002), yielding 0o444 for regular files.
  The receiver on Linux sees a `-r--r--r--` file.
- `FILE_ATTRIBUTE_HIDDEN` -> not consulted, silently dropped from
  the wire payload.
- `FILE_ATTRIBUTE_SYSTEM` -> not consulted, silently dropped.
- `FILE_ATTRIBUTE_ARCHIVE` -> not consulted, silently dropped.
- Group / other read / execute bits are never derived from any
  Windows source; the sender hard-codes `0o644` / `0o444` /
  `0o755`. There is no notion of `g+x` or `o+r` distinguished from
  `u+x` / `u+r` on the wire.

## 5. Write-side mapping (POSIX -> Windows)

When the receiver applies a wire-format mode_t to a Windows
destination, it inspects the owner write bit only and toggles
`FILE_ATTRIBUTE_READONLY` accordingly:

```text
crates/metadata/src/apply/permissions.rs:336-357

#[cfg(not(unix))]
{
    if options.permissions() {
        let readonly = entry.permissions() & 0o200 == 0;
        let dest_perms_meta = if let Some(meta) = cached_meta {
            meta.permissions()
        } else {
            fs::metadata(destination)
                .map_err(...)?
                .permissions()
        };
        let mut dest_perms = dest_perms_meta;
        if dest_perms.readonly() != readonly {
            dest_perms.set_readonly(readonly);
            fs::set_permissions(destination, dest_perms).map_err(...)?;
        }
    }
}
```

The same pattern repeats at `apply_batch.rs:181-202` and
`syscall_batch.rs:378-383`.

Consequences:

- POSIX mode with no owner write bit (e.g. `0o400`, `0o444`, `0o555`)
  -> destination acquires `FILE_ATTRIBUTE_READONLY`.
- POSIX mode with the owner write bit set (e.g. `0o644`, `0o755`)
  -> destination clears `FILE_ATTRIBUTE_READONLY` if previously set.
- Group / other / suid / sgid / sticky bits -> no Windows effect.
- `FILE_ATTRIBUTE_HIDDEN`, `SYSTEM`, `ARCHIVE` on the destination
  before the transfer -> **survive** the call because
  `fs::Permissions::set_readonly()` only flips the read-only bit
  and `fs::set_permissions()` only writes the resulting permissions
  back via `SetFileAttributesW` with the existing other bits
  intact. This is a positive accident of the standard library
  wrapper: the receiver does not clobber HIDDEN / SYSTEM / ARCHIVE
  bits already on the destination. It does not propagate them
  either (there is no wire input).

## 6. Wire encoding

The wire-format file entry carries a single 32-bit mode field
(type bits + 12-bit permission word):

- `crates/protocol/src/flist/entry/constructors.rs:23-41` -
  `FileEntry::new_with_type` packs
  `file_type.to_mode_bits() | (permissions & 0o7777)`.
- `crates/protocol/src/flist/entry/accessors.rs:150-160` -
  `FileEntry::permissions()` returns the 12-bit permission word.

Grep for `FILE_ATTRIBUTE`, `hidden`, `system_attr`, `archive_attr`,
`windows` across `crates/protocol/src/flist/write/` and
`crates/protocol/src/flist/read/`: zero hits. The wire format has
no slot for Windows-only attribute bits. This matches upstream
rsync 3.4.1, which does not transport Windows attributes either.

Consequence: there is no native way to round-trip
`FILE_ATTRIBUTE_HIDDEN`, `SYSTEM`, or `ARCHIVE` across a
Windows-to-Windows transfer through the standard mode field.

The only available side channel is `-X` (extended attributes). A
Windows backend for `user.windows.attrs` could carry the raw
`FILE_ATTRIBUTE_*` DWORD. The current
`crates/metadata/src/xattr_windows.rs` exposes named NTFS
alternate-data-stream attributes (see WPC-1 audit) but does **not**
define a `user.windows.attrs` synthetic xattr that exposes the
file-attribute DWORD. This is the gap captured under R2 below.

## 7. Existing test coverage

Search across `crates/metadata/`, `crates/engine/`, and `tests/`
for any test that exercises a perm-bit round-trip with a Windows
attribute:

- `crates/metadata/src/apply_batch.rs:527-552` -
  `windows_readonly_attribute()` `#[cfg(windows)]`. Creates a
  source file, calls `perms.set_readonly(true)`, runs
  `apply_file_metadata` with `set_permissions(true)`, asserts the
  destination is now readonly. **Verifies the write-side mapping
  only when the source already has the readonly bit set** (an
  in-process metadata copy, not a wire round-trip).
- `crates/engine/src/local_copy/tests/execute_skip.rs:17-63` -
  `execute_skips_rewriting_identical_destination()`. Cross-platform
  test that pre-sets the destination read-only, transfers, and
  asserts the destination loses the read-only bit because the
  source is writable. **Verifies the write-side mapping only**.
- `crates/metadata/src/stat_cache.rs:662-672` -
  `windows_readonly_metadata()` `#[cfg(windows)]`. Caches a
  read-only file's metadata and reads back the cached flag. Does
  not exercise the wire conversion.

**Absent**:

- No test sets `FILE_ATTRIBUTE_READONLY` on a Windows **source**
  file, runs an oc-rsync transfer to a Linux receiver, and asserts
  the destination has mode 0o444.
- No test sets a POSIX 0o444 mode on a Linux **source** file, runs
  an oc-rsync transfer to a Windows receiver, and asserts the
  destination has `FILE_ATTRIBUTE_READONLY`.
- No test creates a Windows source file with
  `FILE_ATTRIBUTE_HIDDEN`, `SYSTEM`, or `ARCHIVE`, runs a
  Windows-to-Windows transfer, and asserts the destination has the
  same attributes.
- No test preserves an existing `FILE_ATTRIBUTE_HIDDEN` /
  `SYSTEM` / `ARCHIVE` on the destination when the receiver
  rewrites the read-only flag (the positive accident in section 5
  is uncovered by any regression test).

## 8. Findings

- **F1: read-side READONLY -> POSIX mapping is present, lossy on
  the high bits.**
  `crates/transfer/src/generator/file_list/entry.rs:53-74` derives
  the wire mode from `metadata.permissions().readonly()` and emits
  `0o444` (readonly) or `0o644` (writable) for regular files and a
  hard-coded `0o755` for directories. The bit-stripping is
  conservative: the readonly flag strips all three write bits, not
  just owner-write. Group and other read / execute bits are
  hard-coded and not derived from any Windows source.

- **F2: read-side HIDDEN / SYSTEM / ARCHIVE handling is absent.**
  Grep for `FILE_ATTRIBUTE_HIDDEN`, `FILE_ATTRIBUTE_SYSTEM`,
  `FILE_ATTRIBUTE_ARCHIVE`, `GetFileAttributesW`,
  `GetFileAttributesExW`, `GetFileInformationByHandle` returns zero
  hits in `crates/`. The sender never consults those bits. They
  are silently dropped from the wire payload.

- **F3: write-side POSIX -> READONLY mapping is present, symmetric
  with F1.**
  Three sites (`crates/metadata/src/apply/permissions.rs:336-357`,
  `crates/metadata/src/apply_batch.rs:181-202`,
  `crates/fast_io/src/syscall_batch.rs:378-383`) flip
  `FILE_ATTRIBUTE_READONLY` based on `mode & 0o200 == 0`
  (owner-write bit absent). The condition uses the owner-write
  bit only, not the union of all three write bits. This is the
  standard Cygwin / WSL convention and matches the asymmetry on
  the read side (the read side **strips** all three; the write
  side **checks** only owner).

- **F4: write-side other-attribute preservation on overwrite is
  accidentally correct.**
  All three write sites read the current `Permissions` object via
  `fs::metadata(destination)?.permissions()`, mutate only the
  read-only flag via `set_readonly()`, and write it back via
  `fs::set_permissions`. The Rust standard library wraps
  `SetFileAttributesW` such that only the read-only bit is
  modified; other bits (`HIDDEN`, `SYSTEM`, `ARCHIVE`, and the
  rest) survive. This is undocumented in the audited code and not
  covered by any regression test. Behaviour could regress silently
  if the implementation switches to a direct `SetFileAttributesW`
  call.

- **F5: Windows-to-Windows attribute round-trip via xattr is not
  implemented.**
  `crates/metadata/src/xattr_windows.rs` implements named-stream
  xattrs only. There is no synthetic `user.windows.attrs` xattr
  that exposes the raw `FILE_ATTRIBUTE_*` DWORD. A
  Windows-to-Linux-to-Windows transit therefore cannot preserve
  HIDDEN / SYSTEM / ARCHIVE under any flag combination.

- **F6: regression-test coverage for F1-F5 is absent.**
  The three existing tests
  (`apply_batch.rs:527`, `execute_skip.rs:17`,
  `stat_cache.rs:662`) cover the write-side readonly toggle only,
  and only as a same-host metadata copy. There is no transfer-level
  test that exercises the wire mode_t round-trip with a Windows
  attribute on either end.

## 9. Risk surface

Concrete breakage today:

- **Silent metadata loss on Windows source files** with
  `FILE_ATTRIBUTE_HIDDEN`, `SYSTEM`, or `ARCHIVE` set. The
  attributes are dropped at the sender; the receiver has no input
  to set them; there is no diagnostic emitted (no `vvv` log entry,
  no warning). Operators using `oc-rsync` to back up Windows
  filesystems lose those flags.
- **Windows-to-Linux-to-Windows round-trip cannot restore
  attributes.** Even if the Linux intermediate were to store them
  somewhere (e.g. in an xattr that `-X` would carry), the sender
  side never emits and the receiver side never consumes such an
  xattr.
- **Read-only round-trip survives, but only for the owner-write
  bit.** A Windows source with `FILE_ATTRIBUTE_READONLY` -> Linux
  destination `0o444` -> Windows destination
  `FILE_ATTRIBUTE_READONLY` is correct. A Linux source `0o600`
  (owner read+write, no group / other) -> Windows destination ->
  Linux destination loses the group / other distinction because
  the Windows receiver cannot store sub-owner permission detail.
  This is the standard NTFS lossy floor; not a defect.
- **Suid / sgid / sticky bits** are silently ignored on Windows
  receive. Documented in the support matrix as a known limitation.
- **Direct `SetFileAttributesW` regression risk.** F4 is an
  accidental property of the standard library wrapper. Any future
  refactor that replaces the `fs::Permissions::set_readonly()` call
  with a direct `SetFileAttributesW(handle, FILE_ATTRIBUTE_READONLY
  | ...)` would wipe HIDDEN / SYSTEM / ARCHIVE bits unless the
  refactor explicitly ORs them in. No test would catch this.

## 10. Recommendations

- **R1: Document the F1 / F3 asymmetry and add wire-level round-trip
  tests.**
  The minimum correct round-trip
  (`FILE_ATTRIBUTE_READONLY` <-> no owner write bit) is implemented
  but uncovered by transfer-level tests. Add:

  1. Windows source -> Linux destination test:
     `FILE_ATTRIBUTE_READONLY` -> wire mode `0o444`.
  2. Linux source -> Windows destination test: `0o444` ->
     `FILE_ATTRIBUTE_READONLY`.
  3. Windows source -> Windows destination round-trip test for the
     read-only flag.

  Place tests under `crates/transfer/src/receiver/tests/` and
  `crates/metadata/tests/`, gated `#[cfg(windows)]` where
  applicable.

- **R2: Add `user.windows.attrs` synthetic xattr in
  `crates/metadata/src/xattr_windows.rs`.**
  Carry the raw `FILE_ATTRIBUTE_*` DWORD as a fixed-width xattr
  value (4 bytes little-endian) so that Windows-to-Windows
  transfers with `-X` preserve HIDDEN / SYSTEM / ARCHIVE. The
  sender populates the xattr from `GetFileAttributesW`; the
  receiver applies via `SetFileAttributesW`. Mask out
  `FILE_ATTRIBUTE_DIRECTORY` and `FILE_ATTRIBUTE_REPARSE_POINT`
  (handled by the file-type code path) and
  `FILE_ATTRIBUTE_NORMAL` (placeholder).

- **R3: Preserve existing destination bits when applying
  `user.windows.attrs` from xattr.**
  On the receive side, OR the xattr value with the bits not
  represented in the wire payload (e.g. `FILE_ATTRIBUTE_OFFLINE`,
  `FILE_ATTRIBUTE_REPARSE_POINT`) rather than overwriting the
  whole attribute word. Read the current attributes via
  `GetFileAttributesW`, clear the bits that the wire payload
  represents (READONLY / HIDDEN / SYSTEM / ARCHIVE), then OR in
  the wire-carried bits.

- **R4: Cross-link the lossy Windows-to-Linux round-trip in the
  Windows support matrix.**
  `docs/user/windows-support-matrix.md:106-109` already names this
  gap. Add a link to this audit and to the R2 follow-up so
  operators can see the planned fix and its prerequisites.

## 11. Cross-references

- **WPC-13 Windows support matrix** -
  `docs/user/windows-support-matrix.md` (PR #4920). The
  "Permissions (`-p`)" row at line 47 and the "Windows attribute
  bits" entry at lines 106-109 already document the gap; this
  audit drills into the conversion sites.
- **WPC-1 ADS handling audit** -
  `docs/audit/windows-ads-handling.md`. Parallel work on the
  Windows xattr backend that R2 extends.
- **WPC-2 ADS strategy** - `docs/design/windows-ads-strategy.md`
  (when shipped). R2 depends on whether the strategy commits to a
  `user.windows.*` xattr namespace.
- **WPC-10 inherited DACL round-trip** - related Windows-only
  metadata that lives outside the mode_t field.
- Memory notes: `project_windows_real_world_parity_unclear`,
  `project_windows_parity_wip`.
