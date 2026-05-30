# RSS-A.7.a: FileEntry Read-Site Audit for Flat FList Migration

Comprehensive inventory of every site that reads from `protocol::flist::FileEntry`
across the oc-rsync workspace. This audit covers production code only; test files,
benchmarks, and fuzz targets are noted separately in the summary.

## Summary Statistics

| Category | Count |
|----------|-------|
| Production files referencing FileEntry | 64 |
| Test/bench/fuzz files referencing FileEntry | ~40 |
| Crates with FileEntry dependencies | 6 (protocol, transfer, engine, metadata, batch, fast_io) |
| Crates with zero FileEntry usage | 9 (core, cli, daemon, filters, transport, compress, checksums, logging, signature) |

### By Crate (production files only)

| Crate | Files | Role |
|-------|-------|------|
| protocol | 20 | Definition, serialization, sorting, comparison |
| transfer | 28 | Generator, receiver, pipeline, disk commit |
| engine | 8 | Delete subsystem, local copy, batch encoding |
| metadata | 4 | Permissions, timestamps, ownership application |
| batch | 6 | Batch read/write, replay |
| fast_io | 1 | Cached sort key (FileEntrySortKey, not FileEntry itself) |

### By Consumer Type

| Consumer Type | Sites | Primary Access Pattern |
|---------------|-------|----------------------|
| Wire encoding (write) | 6 files | All fields via &FileEntry |
| Wire decoding (read) | 2 files | Construction + set_* mutations |
| Sorting/comparison | 3 files | name_bytes(), is_dir(), path(), dirname() |
| Generator (sender-side) | 6 files | Construction + metadata fields |
| Receiver (receiver-side) | 10 files | path(), size(), is_dir(), flags(), hardlink_* |
| Delete subsystem | 5 files | path(), file_type(), hardlink_idx/dev/ino |
| Metadata application | 4 files | mode, uid, gid, mtime, atime, crtime, rdev, permissions |
| Pipeline/dispatch | 4 files | path(), size(), is_file(), is_dir() |
| Batch replay | 3 files | name(), size(), file_type(), link_target() |
| Tracing/debug | 1 file | name(), size(), mode(), flags(), is_dir() |
| Incremental flist | 3 files | name(), is_dir(), file_type() |

---

## Per-Crate Read-Site Tables

### protocol crate

Files within `crates/protocol/src/flist/` (excluding `entry/` definition module):

| File | Line(s) | Methods Accessed | Read/Mutate | Classification |
|------|---------|-----------------|-------------|----------------|
| `sort.rs:43-48` | SortKey::new | name_bytes(), is_dir() | Read | sort |
| `sort.rs:85` | compare_file_entries | delegates to f_name_cmp | Read | sort |
| `sort.rs:206` | sort_file_list | &mut [FileEntry] in-place sort | Mutate (reorder) | sort |
| `sort.rs:316-399` | flist_clean | set_content_dir(true), dedup by name_cmp_eq | Mutate | sort |
| `name_cmp.rs:60` | f_name_cmp | path(), dirname() via name_bytes/basename_bytes | Read | sort |
| `name_cmp.rs:88` | name_cmp_eq | same as f_name_cmp | Read | sort |
| `name_cmp.rs:105` | basename_bytes | path() via path_bytes_to_wire | Read | sort |
| `trace.rs:140` | mem size reporting | std::mem::size_of::<FileEntry>() | Read (type) | other |
| `trace.rs:164` | output_flist | iterates &[FileEntry], calls output_flist_entry | Read | other |
| `trace.rs:178-206` | output_flist_entry | name(), is_dir(), size(), mode(), flags() | Read | other |
| `read/mod.rs:472-759` | read_entry / read_entry_inner | Construction (from_raw_bytes), then set_dirname, set_link_target, set_rdev, set_hardlink_idx, set_uid, set_gid, set_user_name, set_group_name, set_atime, set_atime_nsec, set_crtime, set_content_dir, set_hardlink_dev, set_hardlink_ino, set_checksum, set_acl_ndx, set_def_acl_ndx, set_xattr_ndx | Mutate (construct) | receiver |
| `read/mod.rs:654-738` | post-construction | path(), is_dir(), is_symlink(), name(), size(), mode(), mtime(), uid(), gid() | Read | receiver |
| `read/extras.rs:263-278` | update_stats | is_dir(), is_file(), size(), is_symlink(), link_target(), is_device(), is_special() | Read | other |
| `write/mod.rs:345-454` | write_entry, is_abbreviated_follower | hardlink_idx(), name_bytes(), is_dir(), is_symlink(), mode(), xattr_list(), acl_ndx() | Read | sender |
| `write/xflags.rs:73-275` | calculate_xflags + sub-methods | is_dir(), flags(), mode(), mtime(), uid(), gid(), is_device(), is_special(), rdev_major(), rdev_minor(), hardlink_idx(), hardlink_dev(), user_name(), group_name(), atime(), crtime(), mtime_nsec(), content_dir() | Read | sender |
| `write/metadata.rs:37-160` | write_metadata fields | size(), mtime(), mtime_nsec(), crtime(), mode(), uid(), gid(), user_name(), group_name(), atime(), atime_nsec(), is_dir() | Read | sender |
| `write/encoding.rs:107-389` | write_symlink, write_device, write_hardlink30+, write_hardlink_pre30, write_checksum, update_stats | is_symlink(), link_target(), is_device(), is_special(), rdev_major(), rdev_minor(), hardlink_idx(), is_dir(), hardlink_dev(), hardlink_ino(), is_file(), checksum(), size() | Read | sender |
| `segment.rs:31,142,157` | FileListSegment | entries: Vec<FileEntry>, get_by_ndx, flatten | Read | other |
| `batched_writer/writer.rs:224,251` | add_entry, add_entries | &FileEntry passed to write_entry | Read | sender |
| `incremental/mod.rs:147-360` | IncrementalFileList | push, pop, peek, drain_ready, finish - name(), is_dir() for routing | Both | receiver |
| `incremental/ready_entry.rs:130-169` | process_ready_entry | name(), is_dir(), is_file(), is_symlink(), is_device(), is_special() | Read | receiver |
| `incremental/streaming.rs:57,132` | StreamingFileListReceiver | yields FileEntry via next_ready / Iterator | Read | receiver |
| `flat/flist.rs` | FlatFileList | References FileEntryHeader (new flat type, not FileEntry) | N/A | other |
| `flat/header.rs` | FileEntryHeader | Separate struct; mirrors FileEntry fields for arena | N/A | other |

### transfer crate

| File | Line(s) | Methods Accessed | Read/Mutate | Classification |
|------|---------|-----------------|-------------|----------------|
| `generator/context.rs:47,289,302` | GeneratorContext | file_list: Vec<FileEntry>, file_list() -> &[FileEntry], push_file_item | Both | generator |
| `generator/file_list/entry.rs:36-296` | FileEntryBuilder::build | Construction: new_file/dir/symlink/etc., set_mtime, set_atime, set_crtime, set_uid, set_gid, set_user_name, set_group_name, set_hardlink_dev, set_hardlink_ino, set_xattr_list; reads: is_dir(), xattr_list() | Mutate (construct) | generator |
| `generator/file_list/mod.rs:33-368` | file_list building | compare_file_entries for sorting, FileEntry::name for trace | Read | generator |
| `generator/file_list/walk.rs` (via mod.rs) | walk_path | set_flags on new entries | Mutate | generator |
| `generator/file_list/hardlinks.rs:13-127` | match_hard_links (post-30) | hardlink_dev(), hardlink_ino(), uid(), gid(), set_hardlink_idx, set_hardlink_dev, set_hardlink_ino | Both | generator |
| `generator/file_list/inc_recurse.rs:61-152` | classify + sub-list build | name(), is_dir() | Read | generator |
| `generator/itemize.rs:67-234` | generate_itemize_flags, format_itemize | is_symlink(), is_dir(), is_special(), is_device(), path(), link_target() | Read | generator |
| `generator/protocol_io.rs:520-549` | write_acl_for_entry, build_xattr_cache_for_entry | is_symlink(), mode(), is_dir(), acl_ndx() | Read | generator |
| `generator/transfer/transfer_loop.rs:298-449` | sender transfer loop | path(), is_file(), size() | Read | sender |
| `receiver/mod.rs:147,461,559,664,720` | Receiver struct | file_list: Vec<FileEntry>, file_list(), resolve_xattr_for_entry (xattr_ndx), resolve_acl_for_entry (acl_ndx, def_acl_ndx, mode) | Read | receiver |
| `receiver/quick_check.rs:32-211` | is_hardlink_follower, quick_check_ok_stateless, dest_mtime_newer | flags(), size(), checksum(), mtime(), path(), is_symlink() | Read | receiver |
| `receiver/file_list/hardlinks.rs:42-127` | match_hard_links (post-30), normalize_pre30_hardlinks | &mut [FileEntry]: hardlink_idx, hardlink_dev, hardlink_ino, is_file, flags_mut, set_hardlink_idx, set_hlinked, set_hlink_first | Both | receiver |
| `receiver/file_list/receive.rs:83-327` | receive_file_list | flags(), set_hardlink_idx, strip_leading_slashes, path() | Both | receiver |
| `receiver/file_list/incremental.rs:58-225` | IncrementalFileListReceiver | drain_ready, collect_sorted, next_ready - wraps IncrementalFileList | Read | receiver |
| `receiver/file_list/sanitize.rs:39-108` | sanitize entries | path(), strip_leading_slashes() | Both | receiver |
| `receiver/directory/creation.rs:15-382` | create_directories | path(), is_dir(), name() | Read | receiver |
| `receiver/directory/links.rs:44-272` | create_symlinks, create_hardlinks | is_symlink(), link_target(), path(), flags(), hardlink_idx(), name() | Read | receiver |
| `receiver/directory/deletion.rs:72-188` | handle deletions | path(), file_type() | Read | receiver |
| `receiver/transfer/candidates.rs:46-183` | build_transfer_candidates | path(), is_symlink() | Read | receiver |
| `receiver/transfer/pipeline.rs:47-450` | transfer_pipeline | path(), size(), is_device() | Read | receiver |
| `receiver/transfer/pipelined.rs:12-162` | pipelined transfer | path(), is_dir() | Read | receiver |
| `receiver/transfer/pipelined_incremental.rs:12-141` | pipelined incremental | path(), is_dir() | Read | receiver |
| `receiver/transfer/sync.rs:105-391` | synchronous transfer | path(), is_file(), is_dir(), size(), is_symlink() | Read | receiver |
| `disk_commit/config.rs:68` | DiskCommitConfig | file_list: Option<Arc<Vec<FileEntry>>> | Read (storage) | receiver |
| `disk_commit/process.rs:435-474` | apply_metadata_acls_and_xattrs | acl_ndx(), is_symlink(), def_acl_ndx(), mode() | Read | receiver |
| `pipeline/job.rs:27-186` | SharedFileList, TransferJob | Vec<FileEntry> in Arc, entry: Arc<FileEntry>, path(), size(), is_file() | Read | other |
| `pipeline/async_dispatch.rs:38-42` | dispatch entry | is_file(), path() | Read | other |
| `delta_transfer.rs:207` | doc comment only | N/A | N/A | other |

### engine crate

| File | Line(s) | Methods Accessed | Read/Mutate | Classification |
|------|---------|-----------------|-------------|----------------|
| `delete/cohort_index.rs:106-228` | CohortIndex::build_from_flist_segment, cohort_of | hardlink_idx(), path(), hardlink_dev(), hardlink_ino() | Read | delete |
| `delete/context/core.rs:228-329` | DeleteContext | observe_segment_for_delete(&[FileEntry]), observe_directory(&[FileEntry]), begin_directory(Vec<FileEntry>), segment_entries: Mutex<Vec<FileEntry>>, children: Vec<FileEntry> | Both | delete |
| `delete/extras.rs:77-135` | compute_extras, segment_basenames | path() (via file_name()) | Read | delete |
| `delete/plan.rs:210-247` | DeletePlan, entry_as_file_entry | Constructs transient FileEntry (new_directory, new_symlink, new_file) for f_name_cmp sorting | Mutate (construct) | delete |
| `delete/traversal.rs:87-180` | DeletionTraversal, child_basename, sort_paths_by_f_name_cmp | file_type(), path() (via file_name()), constructs transient FileEntry::new_directory for f_name_cmp | Both | delete |
| `local_copy/executor/directory/recursive/batch.rs:16-109` | build_protocol_entry | Constructs FileEntry: new_directory, new_symlink, new_file; set_size, set_mtime, set_uid, set_gid, set_user_name, set_group_name, flags_mut() | Mutate (construct) | other |
| `local_copy/executor/special/device.rs:248` | comment only | N/A | N/A | other |
| `lib.rs:145` | re-export | FileEntry re-exported | N/A | other |

### metadata crate

| File | Line(s) | Methods Accessed | Read/Mutate | Classification |
|------|---------|-----------------|-------------|----------------|
| `apply/mod.rs:220-285` | apply_metadata_from_file_entry, apply_metadata_with_attrs | entry: &FileEntry - delegates to timestamps, ownership, permissions | Read | other |
| `apply/timestamps.rs:89-208` | apply_timestamps, apply_atime_only, apply_crtime | mtime(), mtime_nsec(), atime(), crtime() | Read | other |
| `apply/ownership.rs:264-454` | apply_ownership (Unix), apply_ownership (Windows) | uid(), gid(), user_name(), group_name(), mode(), file_type(), rdev_major(), rdev_minor() | Read | other |
| `apply/permissions.rs:275-339` | apply_permissions (Unix), apply_permissions (Windows) | permissions(), mode() | Read | other |

### batch crate

| File | Line(s) | Methods Accessed | Read/Mutate | Classification |
|------|---------|-----------------|-------------|----------------|
| `reader/flist.rs:24-220` | read_file_entry, read_protocol_flist, read_inc_protocol_flist | Returns Vec<FileEntry>, batch::FileEntry construction | Both | other |
| `replay/mod.rs:159-244` | prepare_directories_and_symlinks, apply_all_metadata | name(), size(), file_type(), link_target() | Read | other |
| `replay/delta_phase.rs:38-398` | apply_delta_phase, process_file_ndx, receive_inc_flist_segment | entries: &mut Vec<FileEntry>, name(), size(), sort_file_list | Read | other |
| `replay/fs_ops.rs:25-44` | apply_entry_metadata | entry: &FileEntry passed to metadata crate | Read | other |
| `writer.rs:142` | write_file_entry | entry: &FileEntry (batch::FileEntry, not protocol::FileEntry) | Read | other |

### fast_io crate

| File | Line(s) | Methods Accessed | Read/Mutate | Classification |
|------|---------|-----------------|-------------|----------------|
| `cached_sort.rs:124-292` | FileEntrySortKey | Independent struct (not FileEntry); caches path_bytes + is_dir for sorting. Used by protocol::flist::sort.rs SortKey::new which reads entry.name_bytes() and entry.is_dir() | N/A | sort |

---

## Field Access Frequency (production code, excluding definition module)

| Field/Method | Read Sites | Mutate Sites | Primary Consumers |
|-------------|-----------|-------------|-------------------|
| path() | ~35 | 0 | receiver, generator, delete, batch |
| name() | ~20 | 0 | batch replay, incremental, trace, creation |
| is_dir() | ~25 | 0 | sort, write, receiver, generator, incremental |
| size() | ~15 | 1 (set_size) | receiver, write, batch, pipeline |
| mode() | ~12 | 0 | write, metadata, receiver |
| is_file() | ~8 | 0 | receiver, pipeline, stats |
| is_symlink() | ~12 | 0 | write, receiver, metadata, quick_check |
| flags() | ~8 | 3 (set_flags, flags_mut) | receiver, write, hardlinks |
| mtime() | ~8 | 1 (set_mtime) | write, quick_check, metadata |
| uid() | ~8 | 1 (set_uid) | write, metadata, generator |
| gid() | ~8 | 1 (set_gid) | write, metadata, generator |
| file_type() | ~8 | 0 | batch replay, delete, receiver |
| hardlink_idx() | ~8 | 5 (set_hardlink_idx) | write, receiver, delete |
| link_target() | ~6 | 1 (set_link_target) | write, receiver, batch replay |
| permissions() | ~5 | 0 | metadata, write |
| name_bytes() | ~3 | 0 | sort, write |
| dirname() | ~2 | 1 (set_dirname) | name_cmp, read |
| is_device() | ~5 | 0 | write, generator, pipeline |
| is_special() | ~5 | 0 | write, generator, incremental |
| rdev_major() | ~4 | 1 (set_rdev) | write, metadata |
| rdev_minor() | ~4 | 1 (set_rdev) | write, metadata |
| content_dir() | ~3 | 2 (set_content_dir) | write, sort |
| checksum() | ~2 | 1 (set_checksum) | write, quick_check |
| acl_ndx() | ~3 | 1 (set_acl_ndx) | receiver, disk_commit |
| def_acl_ndx() | ~3 | 1 (set_def_acl_ndx) | receiver, disk_commit |
| xattr_ndx() | ~2 | 1 (set_xattr_ndx) | receiver |
| xattr_list() | ~3 | 1 (set_xattr_list) | write, generator |
| user_name() | ~3 | 1 (set_user_name) | write, metadata, generator |
| group_name() | ~3 | 1 (set_group_name) | write, metadata, generator |
| atime() | ~4 | 1 (set_atime) | write, metadata |
| atime_nsec() | ~2 | 1 (set_atime_nsec) | write, metadata |
| crtime() | ~3 | 1 (set_crtime) | write, metadata |
| mtime_nsec() | ~3 | 0 | write, metadata |
| hardlink_dev() | ~4 | 3 (set_hardlink_dev) | write, receiver, delete |
| hardlink_ino() | ~4 | 3 (set_hardlink_ino) | write, receiver, delete |
| is_block_device() | ~1 | 0 | generator |
| is_char_device() | ~1 | 0 | generator |
| prepend_dir() | - | 1 | read (incremental) |
| strip_leading_slashes() | - | 1 | receiver sanitize |

---

## Collection-Level Access Patterns

### Vec<FileEntry> Ownership

| Location | Pattern | Notes |
|----------|---------|-------|
| `transfer::generator::context` | `file_list: Vec<FileEntry>` | Owned, built during walk |
| `transfer::receiver::mod` | `file_list: Vec<FileEntry>` | Owned, received from wire |
| `transfer::pipeline::job` | `entries: Arc<Vec<FileEntry>>` | Shared across tokio tasks |
| `transfer::disk_commit::config` | `file_list: Option<Arc<Vec<FileEntry>>>` | Shared for metadata |
| `engine::delete::context::core` | `children: Vec<FileEntry>`, `segment_entries: Mutex<Vec<FileEntry>>` | Delete observation |
| `protocol::flist::segment` | `entries: Vec<FileEntry>` | INC_RECURSE segments |
| `protocol::flist::incremental` | `ready: VecDeque<FileEntry>`, `pending: HashMap<String, Vec<FileEntry>>` | Incremental routing |
| `batch::replay::delta_phase` | `entries: &mut Vec<FileEntry>` | Batch replay |

### &[FileEntry] Slice Consumers

| Location | Consumer | Access Pattern |
|----------|----------|----------------|
| `protocol::flist::sort::sort_file_list` | Sort | Reorder in-place, reads name_bytes/is_dir |
| `protocol::flist::trace::output_flist` | Debug | Iterate, read name/size/mode/flags |
| `protocol::flist::read::mod` | Segment entries | Context for inc_recurse |
| `engine::delete::cohort_index` | CohortIndex::build | Reads hardlink_idx/dev/ino/path |
| `engine::delete::extras` | compute_extras | Reads path (basenames) |
| `engine::delete::context` | observe_segment/directory | Reads file_type, path |
| `engine::delete::traversal` | observe_segment | Reads file_type, path |
| `batch::replay::mod` | prepare_dirs, apply_metadata | Reads name, size, file_type, link_target |
| `transfer::generator::context` | file_list() | Exposes as &[FileEntry] |
| `transfer::receiver::mod` | file_list() | Exposes as &[FileEntry] |
| `transfer::pipeline::job` | entries() | Exposes as &[FileEntry] |
| `transfer::generator::file_list::inc_recurse` | classify | Reads name, is_dir |

### &mut [FileEntry] Mutable Slice Consumers

| Location | Consumer | Mutation |
|----------|----------|----------|
| `protocol::flist::sort::sort_file_list` | Sort | Reorders entries via permutation |
| `protocol::flist::sort::flist_clean` | Dedup | set_content_dir, removes duplicates |
| `transfer::receiver::file_list::hardlinks` | match_hard_links | set_hardlink_idx, flags_mut |
| `transfer::receiver::file_list::hardlinks` | normalize_pre30_hardlinks | set_hardlink_idx, flags_mut |

---

## Migration Complexity Assessment

### Trivial (read-only, few fields, no collection ownership)

These sites access FileEntry through `&FileEntry` and use only a small subset
of accessors. They can migrate to a trait with minimal effort.

- **metadata crate** (4 files) - Pure consumers of `&FileEntry`. All four files
  take `entry: &protocol::flist::FileEntry` and call field accessors. A trait
  with the same accessor signatures is a drop-in replacement.
- **protocol::flist::trace** - Read-only debug output.
- **transfer::generator::itemize** - Read-only itemize flag computation.
- **transfer::receiver::quick_check** - Read-only comparison.
- **transfer::disk_commit::process** - Read-only metadata application.
- **transfer::pipeline::async_dispatch** - Read-only dispatch routing.
- **batch::replay::fs_ops** - Delegates to metadata crate.
- **engine::delete::extras::segment_basenames** - Reads path().file_name() only.
- **engine::delete::traversal::child_basename** - Reads path() only.

Estimated: ~15 files, minimal trait surface required.

### Moderate (read-only but uses collection patterns)

These sites iterate over `&[FileEntry]` or index into `Vec<FileEntry>`.
Migration requires the collection type to also support the same trait or expose
entries via a common interface.

- **protocol::flist::sort** - Operates on `&mut [FileEntry]`, uses name_bytes()
  and is_dir() for comparisons. The sort key extraction (SortKey::new) reads
  two fields. The permutation-based sort itself moves entries in-place.
- **protocol::flist::name_cmp** - Comparison functions taking `&FileEntry`.
- **batch::replay::mod** - Iterates `&[FileEntry]` for directory creation and
  metadata application.
- **batch::replay::delta_phase** - Indexes entries[flat_index] for name/size.
- **transfer::receiver::transfer::*** - Multiple pipeline files iterate
  file lists.
- **engine::delete::cohort_index** - Builds index from `&[FileEntry]` slice.
- **engine::delete::context** - Stores Vec<FileEntry> and &[FileEntry].

Estimated: ~15 files, requires trait-aware collection abstraction.

### Complex (mutates FileEntry or owns Vec<FileEntry>)

These sites construct FileEntry values, mutate them via set_* methods, or
own the Vec<FileEntry> that other code borrows from.

- **protocol::flist::read** - Constructs FileEntry from wire bytes, calls
  ~15 different set_* methods. This is the primary construction site alongside
  the entry module itself. Migration requires a builder trait or factory.
- **protocol::flist::write** - Reads all fields for wire encoding. The most
  comprehensive reader of FileEntry state. Migration requires the full
  accessor trait surface.
- **transfer::generator::file_list::entry** - Second construction site.
  Builds FileEntry from filesystem metadata. Same complexity as read.
- **transfer::generator::context** - Owns `file_list: Vec<FileEntry>`.
  Central storage for sender-side file list.
- **transfer::receiver::mod** - Owns `file_list: Vec<FileEntry>`.
  Central storage for receiver-side file list.
- **transfer::receiver::file_list::hardlinks** - Mutates entries in-place
  (set_hardlink_idx, flags_mut). Must have mutable access.
- **transfer::receiver::file_list::receive** - Mutates entries
  (set_hardlink_idx, strip_leading_slashes). Owns the receive path.
- **transfer::pipeline::job** - Wraps Vec<FileEntry> in Arc for sharing.
- **engine::delete::context::core** - Owns Vec<FileEntry> and Mutex<Vec<FileEntry>>.
- **engine::delete::plan** - Constructs transient FileEntry values for sorting.
- **engine::local_copy::executor::directory::recursive::batch** - Constructs
  FileEntry from filesystem metadata for batch encoding.

Estimated: ~15 files, requires construction/mutation trait or dual-path support.

---

## Recommended Migration Order

The migration from `Vec<FileEntry>` to `FlatFileList` should proceed in
dependency order (leaf consumers first, ownership sites last) to minimize
the blast radius of each change.

### Phase 1: Define the trait (RSS-A.7.b)

Define a `FileEntryRef` trait in the protocol crate exposing all read-only
accessors. The existing `FileEntry` gets a blanket impl.

Trait surface (based on this audit):
- `name() -> &str`
- `path() -> &PathBuf`
- `dirname() -> &Arc<Path>`
- `name_bytes() -> Cow<[u8]>`
- `size() -> u64`
- `mode() -> u32`
- `permissions() -> u32`
- `mtime() -> i64`
- `mtime_nsec() -> u32`
- `uid() -> Option<u32>`
- `gid() -> Option<u32>`
- `file_type() -> FileType`
- `is_dir() -> bool`
- `is_file() -> bool`
- `is_symlink() -> bool`
- `is_device() -> bool`
- `is_special() -> bool`
- `is_block_device() -> bool`
- `is_char_device() -> bool`
- `link_target() -> Option<&PathBuf>`
- `rdev_major() -> Option<u32>`
- `rdev_minor() -> Option<u32>`
- `flags() -> FileFlags`
- `content_dir() -> bool`
- `hardlink_idx() -> Option<u32>`
- `hardlink_dev() -> Option<i64>`
- `hardlink_ino() -> Option<i64>`
- `checksum() -> Option<&[u8]>`
- `acl_ndx() -> Option<u32>`
- `def_acl_ndx() -> Option<u32>`
- `xattr_ndx() -> Option<u32>`
- `xattr_list() -> Option<&XattrList>`
- `user_name() -> Option<&str>`
- `group_name() -> Option<&str>`
- `atime() -> i64`
- `atime_nsec() -> u32`
- `crtime() -> i64`

### Phase 2: Migrate leaf consumers (RSS-A.7.c)

Convert read-only, single-entry consumers to use `&dyn FileEntryRef` or
generic `<E: FileEntryRef>`:

1. **metadata crate** - 4 files, all `entry: &protocol::flist::FileEntry`
2. **transfer::generator::itemize** - `entry: &FileEntry`
3. **transfer::receiver::quick_check** - `entry: &FileEntry`
4. **transfer::disk_commit::process** - `entry: Option<&FileEntry>`
5. **protocol::flist::trace** - `entry: &FileEntry`
6. **batch::replay::fs_ops** - `entry: &FileEntry`

### Phase 3: Migrate slice consumers (RSS-A.7.d)

Convert `&[FileEntry]` slice consumers. Requires either a trait-based
iterator or a dual-path abstraction:

1. **engine::delete::extras** - `segment_entries: &[FileEntry]`
2. **engine::delete::traversal** - `children: &[FileEntry]`
3. **engine::delete::cohort_index** - `entries: &[FileEntry]`
4. **batch::replay::mod** - `entries: &[FileEntry]`
5. **protocol::flist::read::extras** - `entry: &FileEntry` (per-entry stats)

### Phase 4: Migrate sort/comparison (RSS-A.7.e)

Sort and comparison functions are tightly coupled to the in-place
`&mut [FileEntry]` pattern. Migration options:

1. Make `sort_file_list` generic over the collection type
2. Or keep it on Vec<FileEntry> and add a parallel sort for FlatFileList

### Phase 5: Migrate wire encoding (RSS-A.7.f)

The write module is the most comprehensive reader. All 35+ accessor methods
are used. Migration is mechanical but touches 6 files in one submodule.

### Phase 6: Migrate collection owners (RSS-A.7.g)

The Vec<FileEntry> owners must be converted last since all downstream code
depends on them:

1. **transfer::receiver::mod** - `file_list: Vec<FileEntry>`
2. **transfer::generator::context** - `file_list: Vec<FileEntry>`
3. **transfer::pipeline::job** - `Arc<Vec<FileEntry>>`
4. **engine::delete::context::core** - `Vec<FileEntry>`, `Mutex<Vec<FileEntry>>`

### Phase 7: Migrate construction sites (RSS-A.7.h)

Construction sites must produce FlatFileList entries instead of FileEntry:

1. **protocol::flist::read** - Wire decoding
2. **transfer::generator::file_list::entry** - Filesystem scanning
3. **engine::local_copy::executor::directory::recursive::batch** - Local copy
4. **engine::delete::plan** - Transient entries for sorting

---

## Key Risks

1. **Sort stability**: `sort_file_list` does in-place permutation on
   `&mut [FileEntry]`. FlatFileList uses contiguous headers; an indirect
   permutation sort (already used in generator) is the correct approach.

2. **Arc<Vec<FileEntry>> sharing**: The pipeline wraps the file list in Arc
   for cross-task sharing. FlatFileList is already arena-backed and can be
   wrapped similarly, but the entry reference type changes from
   `&FileEntry` to `FlatFileEntry<'a>` (a borrow into the arena).

3. **Mutation sites**: ~15 files call set_* methods. FlatFileList entries are
   fixed-size headers; mutations on extras fields require arena indirection.
   The sort/clean pass mutates content_dir, and hardlink normalization mutates
   hardlink_idx/flags. These must be supported on the flat layout.

4. **Transient FileEntry construction**: The delete subsystem constructs
   throwaway FileEntry values solely for f_name_cmp sorting. This pattern
   should be replaced with direct byte comparison to avoid coupling delete
   to FileEntry construction.

5. **Batch crate**: Uses its own `batch::format::FileEntry` (distinct type)
   plus `protocol::flist::FileEntry`. The protocol type is used for wire
   decode/encode; the batch type is internal bookkeeping. Only the protocol
   type is affected by the migration.
