# FileEntry Struct Layout and Padding Waste Audit (RSS-A.2)

Audit of the `FileEntry` struct layout on 64-bit targets. Identifies per-entry
memory overhead compared to upstream rsync's `file_struct` and documents the
post-RSS-A.3/RSS-A.12 compacted layout plus where the remaining savings can be
reclaimed by RSS-A.4 (flat flist).

Source files:
- `crates/protocol/src/flist/entry/core.rs` - `FileEntry`
- `crates/protocol/src/flist/entry/extras.rs` - `FileEntryExtras`
- upstream: `rsync-3.4.1/rsync.h:801` - `struct file_struct`

## FileEntry inline layout (80 bytes)

All sizes are for 64-bit targets. Rust reorders fields for optimal packing
(no `#[repr(C)]`), so the declared order does not determine the memory layout.
The minimum struct size equals the sum of field sizes rounded up to the struct
alignment (8 bytes). The `Option<u32>` uid/gid, `u32` mode and `FileFlags` +
`bool` fields the earlier revision of this audit measured have since been
compacted (RSS-A.3) into raw `u32` uid/gid, a `u16` mode, and a single `u8`
`present` bitfield.

| Field | Type | Size (B) | Align | Notes |
|---|---|---:|---:|---|
| `name` | `PathBuf` | 24 | 8 | ptr + len + cap (OsString inner Vec) |
| `dirname` | `Arc<Path>` | 16 | 8 | fat pointer (data ptr + length); Path is unsized |
| `size` | `u64` | 8 | 8 | file size |
| `mtime` | `i64` | 8 | 8 | seconds since epoch |
| `extras` | `Option<Box<FileEntryExtras>>` | 8 | 8 | null-pointer niche - same size as Box |
| `uid` | `u32` | 4 | 4 | raw value; presence tracked in `present` |
| `gid` | `u32` | 4 | 4 | raw value; presence tracked in `present` |
| `mtime_nsec` | `u32` | 4 | 4 | nanosecond component |
| `mode` | `u16` | 2 | 2 | type + permissions (accessor returns `u32`) |
| `present` | `u8` | 1 | 1 | metadata-presence + persisted wire-flag bits |
| **Subtotal** | | **79** | | |
| Tail padding | | **1** | | round 79 up to 8-byte struct alignment |
| **Total** | | **80** | | |

### Heap allocations per entry (common case)

In the typical transfer (regular files, no symlinks/devices/ACLs/xattrs), only
`name` and `dirname` trigger heap allocations:

| Allocation | Typical size | Overhead |
|---|---|---|
| `name` PathBuf backing buffer | path length + Vec capacity slack | 1 alloc/entry |
| `dirname` Arc<Path> backing | dirname length + Arc header (2 x usize) | shared via PathInterner |
| `extras` | None (null pointer) | 0 alloc |
| **Total heap per common entry** | | **1-2 allocs** |

The `PathInterner` deduplicates dirname allocations across entries sharing the
same parent directory, so the per-entry cost of `dirname` amortizes to near
zero when many files live in the same directory.

## FileEntryExtras layout (184 bytes)

Boxed behind `Option<Box<...>>` in `FileEntry`. Only allocated when at least one
rarely-used field is needed (symlinks, devices, hardlinks, ACLs, xattrs,
atimes, crtimes, checksums, user/group names). The six `Option<u32>` index
fields and two `Option<i64>` hardlink fields the earlier revision measured have
since been compacted (RSS-A.12) into raw `u32`/`i64` values plus a `u16`
`present` bitfield.

Rust reorders fields for minimal padding. Optimal packing groups 8-byte-aligned
fields first (152 bytes), then 4-byte-aligned fields (28 bytes), then the 2-byte
`present` field, totaling 182 bytes rounded up to 184 bytes.

| Field | Type | Size (B) | Align | Niche? | Notes |
|---|---|---:|---:|---|---|
| `link_target` | `Option<PathBuf>` | 24 | 8 | yes | NonNull niche |
| `user_name` | `Option<String>` | 24 | 8 | yes | NonNull niche |
| `group_name` | `Option<String>` | 24 | 8 | yes | NonNull niche |
| `checksum` | `Option<Vec<u8>>` | 24 | 8 | yes | NonNull niche; up to 32B data |
| `xattr_list` | `Option<XattrList>` | 24 | 8 | yes | XattrList wraps Vec |
| `atime` | `i64` | 8 | 8 | - | access time seconds |
| `crtime` | `i64` | 8 | 8 | - | creation time seconds |
| `hardlink_dev` | `i64` | 8 | 8 | - | raw value; presence in `present` |
| `hardlink_ino` | `i64` | 8 | 8 | - | raw value; presence in `present` |
| `atime_nsec` | `u32` | 4 | 4 | - | nanosecond component |
| `rdev_major` | `u32` | 4 | 4 | - | device major; presence in `present` |
| `rdev_minor` | `u32` | 4 | 4 | - | device minor; presence in `present` |
| `hardlink_idx` | `u32` | 4 | 4 | - | hardlink preservation; presence in `present` |
| `acl_ndx` | `u32` | 4 | 4 | - | access ACL index; presence in `present` |
| `def_acl_ndx` | `u32` | 4 | 4 | - | default ACL index (dirs); presence in `present` |
| `xattr_ndx` | `u32` | 4 | 4 | - | xattr index; presence in `present` |
| `present` | `u16` | 2 | 2 | - | presence bitfield for the compacted fields |
| **Subtotal** | | **182** | | | |
| Tail padding | | **2** | | | round 182 up to 8-byte alignment |
| **Total** | | **184** | | | + ~16B malloc header overhead |

### Per-entry totals

**Common case** (regular file, no extras):
- FileEntry inline: 80 bytes
- PathBuf name heap: ~30 bytes typical + ~16B malloc overhead = ~46 bytes
- Arc<Path> dirname: amortized ~0 bytes (shared via PathInterner)
- extras: None (0 bytes)
- **~126 bytes per entry**

**Worst case** (extras populated):
- FileEntry inline: 80 bytes
- FileEntryExtras heap block: 184 bytes + ~16B malloc overhead
- Plus heap allocs for populated Option<PathBuf>, Option<String>, Option<Vec>
- **~280+ bytes per entry**

## Upstream rsync file_struct (24 bytes fixed)

```c
struct file_struct {              // upstream: rsync.h:801
    const char *dirname;          //  8B - shared pointer
    time_t modtime;               //  8B - mtime
    uint32 len32;                 //  4B - low 32 bits of size
    uint16 mode;                  //  2B - type + permissions
    uint16 flags;                 //  2B - FLAG_* bits
    const char basename[];        //  0B - flexible array member
};                                // = 24 bytes (FILE_STRUCT_LEN)
```

Additional fields are stored as `union file_extras` (4 bytes each) prepended
before the `file_struct` pointer in a contiguous allocation. Extras are
conditionally allocated based on global config flags (`uid_ndx`, `gid_ndx`,
`acls_ndx`, `xattrs_ndx`, etc.). The `basename` flexible array member stores
the filename inline after the struct - no separate heap allocation.

### Upstream per-entry total (common case)

For a typical transfer with uid + gid preservation:

| Component | Size |
|---|---|
| file_struct fixed | 24 B |
| uid extra (1 x 4B) | 4 B |
| gid extra (1 x 4B) | 4 B |
| file_extra_cnt base (1 x 4B) | 4 B |
| basename inline (avg ~15 chars + NUL) | ~16 B |
| dirname pointer (shared, not per-entry) | 0 B |
| **Subtotal per allocation** | **~52 B** |
| files[] pointer (8B per entry) | 8 B |
| **Total per entry** | **~60 B** |

## Comparison summary

| Metric | oc-rsync | upstream | Ratio |
|---|---:|---:|---:|
| Inline struct size | 80 B | 24 B | 3.3x |
| Common-case total per entry | ~126 B | ~60 B | 2.1x |
| At 1M files | ~126 MB | ~57 MB | 2.2x |
| Heap allocations per entry | 1-2 | 0 | - |

The measured 25.9x RSS gap at 1M files (197 MB vs 7.6 MB) exceeds the 2.2x
structural overhead calculated here. The additional gap comes from:
1. `Vec<FileEntry>` capacity overhead (Vec doubles capacity, wasting up to 50%)
2. Per-allocation malloc metadata (16 bytes per alloc on glibc/jemalloc)
3. Malloc fragmentation (small allocations waste alignment padding)
4. `std::fs::Metadata` cached in `FileListEntry` during traversal
5. Additional data structures (sort index, filter chain, hardlink maps)

## Top waste contributors

Ranked by per-entry cost in the common case (regular files, no extras). The
inline-field waste the earlier revision ranked (uid/gid, flags, content_dir)
has since been reclaimed by the RSS-A.3/RSS-A.12 compaction; the remaining
contributors are the heap-allocated path fields and the always-8-byte size:

1. **PathBuf `name` (24B inline + heap alloc)** - upstream stores basename
   inline via flexible array member with zero separate allocation. Savings:
   24B inline + 1 alloc/entry. Fix: arena-allocate name (RSS-7, in progress)
   or flat flist with inline basename (RSS-A.4).

2. **Arc<Path> `dirname` (16B vs 8B)** - upstream uses a plain `const char*`
   (8 bytes). `Arc<Path>` is a fat pointer (16 bytes) because Path is unsized.
   Savings: 8B/entry. Fix: use a thin u32 arena offset or intern index.

3. **u64 `size` (8B vs 4B)** - upstream stores only the low 32 bits inline
   (`len32`), with a conditional 4B extra for the high 32 bits
   (`FLAG_LENGTH64`). Savings: 4B/entry. Fix: store u32 inline, promote to
   u64 via extras only for files > 4 GB.

### Reclaimed by RSS-A.3 / RSS-A.12 (landed)

- `Option<u32>` uid/gid (8B each) -> raw `u32` (4B each) + `present` bit.
  Saved 8B inline per entry.
- `FileFlags` (3B) + `bool content_dir` (1B) -> folded into the `u8` `present`
  bitfield. Saved ~3B inline per entry.
- `u32` mode -> `u16` mode (accessor still returns `u32`). Saved 2B inline.
- Six `Option<u32>` extras fields (rdev_major/minor, hardlink_idx, acl_ndx,
  def_acl_ndx, xattr_ndx) -> raw `u32` + `present` bit. Saved 6 x 4B = 24B.
- Two `Option<i64>` extras fields (hardlink_dev, hardlink_ino) -> raw `i64` +
  `present` bit. Saved 2 x 8B = 16B. `FileEntryExtras` shrank 224B -> 184B.

## Recommendations for RSS-A.4

### RSS-A.4: Flat flist backing store

Match upstream's contiguous allocation model:
- Single contiguous buffer per flist segment.
- Fixed-size header (target: 32-40 bytes) per entry.
- Variable-length basename packed inline after the header.
- Optional extras packed before the header (upstream convention).
- Zero per-entry heap allocations.
- Target: ~52-60 bytes per common-case entry, matching upstream.
