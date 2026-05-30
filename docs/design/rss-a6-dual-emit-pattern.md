# RSS-A.6: dual-emit pattern for the flat file-list migration

Task: RSS-A.6. Prerequisites: RSS-A.5.a-f (FlatFileList, PathArena, ExtrasArena).
Downstream: RSS-A.7 (remove Vec<FileEntry>, FlatFileList stands alone).

## Chosen pattern: DualFileList wrapper

A single `DualFileList` wrapper in `crates/protocol/src/flist/flat/dual.rs`
encapsulates both `Vec<FileEntry>` (legacy) and `FlatFileList` (new). Call
sites stay as `push(entry)`; the wrapper handles the split internally. This
keeps the feature gate and conversion logic in one place and lets RSS-A.7
remove the type with a one-line replacement at each call site.

Without the `flat-flist` feature the wrapper compiles to a transparent
newtype over `Vec<FileEntry>` with no overhead.

### Emission points

1. **Receiver wire-decode** - `crates/transfer/src/receiver/file_list/receive.rs:50`
   (main list) and `:195` (INC_RECURSE sub-lists). Both call `push(entry)`.
2. **Generator local-construction** - `crates/transfer/src/generator/context.rs:302`
   (`push_file_item` - single chokepoint for all local emission).
3. **Batch capture** - `crates/engine/src/local_copy/.../batch.rs:24`. Excluded:
   builds a transient `FileEntry` to serialize into wire bytes and discards it.
   Dual-emitting here would produce dangling arena data with no consumer.

## DualFileList API

```rust
pub struct DualFileList {
    legacy: Vec<FileEntry>,
    #[cfg(feature = "flat-flist")]
    flat: FlatFileList,
    #[cfg(feature = "flat-flist")]
    extras: ExtrasArena,
}
```

Key methods:
- `push(&mut self, entry: FileEntry)` - pushes to legacy; under `flat-flist`,
  also converts and pushes to flat/extras arenas.
- `len()`, `is_empty()`, `as_slice()`, `get(i)`, `iter()`, `iter_mut()` -
  delegate to legacy Vec.
- `segment_start() -> usize` - returns `legacy.len()` for INC_RECURSE
  segment boundary tracking.
- `Index<usize>`, `Index<RangeFrom<usize>>`, `IndexMut<usize>` - delegate
  to legacy Vec.
- `flat()` / `extras()` - `cfg(feature = "flat-flist")` accessors.

## Conversion algorithm: FileEntry -> FileEntryHeader

```rust
#[cfg(feature = "flat-flist")]
fn file_entry_to_flat(
    entry: &FileEntry,
    paths: &mut PathArena,
    extras: &mut ExtrasArena,
) -> FileEntryHeader
```

1. **Path splitting** - `entry.name()` is split at the last `/` into
   dirname + basename. Each is interned via `PathArena::intern()`.
2. **Presence bitfield** - built from `entry.uid().is_some()`,
   `entry.gid().is_some()`, `entry.mtime_nsec() != 0`,
   `entry.content_dir()`, `entry.size() > u32::MAX`.
3. **Extras encoding** - optional fields (link target, rdev, hardlink idx,
   ACL/xattr indices, checksum, user/group names, atime/crtime) are packed
   into a `FlatExtras` struct and appended to `ExtrasArena`.
4. **Flags** - `FileFlags` packed into `u16` (primary | extended << 8).
   `extended16` (varint third byte) is not preserved - it carries only
   `XMIT_CRTIME_EQ_MTIME` which is reconstructed from `crtime == mtime`
   at read time in RSS-A.7.

## Per-emission-point modifications

### Receiver (wire-decode)

- `ReceiverContext.file_list: Vec<FileEntry>` -> `DualFileList`
- Constructor: `Vec::new()` -> `DualFileList::new()`
- `pub fn file_list() -> &[FileEntry]` -> `self.file_list.as_slice()`
- All `push(entry)` calls unchanged (same signature).
- Slice ops `&self.file_list[seg_start..]` work via `Index<RangeFrom<usize>>`.

### Generator (local construction)

- `GeneratorContext.file_list: Vec<FileEntry>` -> `DualFileList`
- `push_file_item` body unchanged.
- `file_list()` accessor: `self.file_list.as_slice()`.
- `clear_file_list()`: reinitialize via `DualFileList::new()`.

### Batch capture (excluded)

No changes. Transient `FileEntry` is serialized and discarded.

## INC_RECURSE segment handling

The flat list is a single growing arena with no per-segment structure.
During RSS-A.6 (transition), segment boundaries remain implicit in the
legacy `Vec<FileEntry>` - sort and match operations apply to Vec slices.
The flat list grows in lock-step but is unused for consumption.

For RSS-A.7, callers record `segment_start()` before receiving each
sub-list. That value scopes `FlatFileList::sort()` to the segment range.

## Testing strategy

- **Conversion round-trip** - construct FileEntry with all optional fields,
  convert, decode via ExtrasArena/PathArena, assert field equality.
- **Empty extras** - common-case file produces `ExtrasRef::NO_EXTRAS`;
  PathArena deduplicates shared dirnames.
- **Push count parity** - after N pushes, `legacy.len() == flat.len()`.
- **INC_RECURSE parity** - push across two segments, verify
  `segment_start()` values match Vec slice boundaries.
- **Regression** - existing flist tests compile and pass both with and
  without `--features flat-flist`.

## RSS-A.7 migration path

1. Delete `DualFileList::legacy` field and Vec accessors.
2. `push()` drops the `self.legacy.push(entry)` line.
3. All consumers migrate from `&[FileEntry]` to `FlatFileEntry` views.
4. `file_entry_to_flat` is deleted; entries are built directly into
   `FileEntryHeader` at the emission point.
5. `DualFileList` is renamed to `FileList` or removed.

## Known limitations

- **atime/crtime sentinel** - `!= 0` maps to `Some(v)`. An atime of
  exactly Unix epoch (0) is lost. Acceptable for transition; RSS-A.7 adds
  presence bits to eliminate the ambiguity.
- **FileFlags.extended16 drop** - `flags: u16` holds only primary +
  extended. The `extended16` byte (`XMIT_CRTIME_EQ_MTIME`) is not stored.
  Since the flat list is not consumed during dual-emit, no information is
  lost for legacy-path consumers.
