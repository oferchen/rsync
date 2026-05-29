# FileEntry Struct Layout and Padding Waste Audit (RSS-A.2)

Audit of the `FileEntry` struct layout on 64-bit targets. Identifies per-entry
memory overhead compared to upstream rsync's `file_struct` and documents where
savings can be reclaimed by RSS-A.3 (compaction) and RSS-A.4 (flat flist).

Source files:
- `crates/protocol/src/flist/entry/core.rs` - `FileEntry`
- `crates/protocol/src/flist/entry/extras.rs` - `FileEntryExtras`
- upstream: `rsync-3.4.1/rsync.h:801` - `struct file_struct`

## FileEntry inline layout (96 bytes)

All sizes are for 64-bit targets. Rust reorders fields for optimal packing
(no `#[repr(C)]`), so the declared order does not determine the memory layout.
The minimum struct size equals the sum of field sizes rounded up to the struct
alignment (8 bytes).

| Field | Type | Size (B) | Align | Notes |
|---|---|---:|---:|---|
| `name` | `PathBuf` | 24 | 8 | ptr + len + cap (OsString inner Vec) |
| `dirname` | `Arc<Path>` | 16 | 8 | fat pointer (data ptr + length); Path is unsized |
| `size` | `u64` | 8 | 8 | file size |
| `mtime` | `i64` | 8 | 8 | seconds since epoch |
| `uid` | `Option<u32>` | 8 | 4 | no niche - 4B discriminant + 4B payload |
| `gid` | `Option<u32>` | 8 | 4 | no niche - 4B discriminant + 4B payload |
| `extras` | `Option<Box<FileEntryExtras>>` | 8 | 8 | null-pointer niche - same size as Box |
| `mode` | `u32` | 4 | 4 | type + permissions |
| `mtime_nsec` | `u32` | 4 | 4 | nanosecond component |
| `flags` | `FileFlags` | 3 | 1 | 3 x u8 (primary, extended, extended16) |
| `content_dir` | `bool` | 1 | 1 | INC_RECURSE content flag |
| **Subtotal** | | **92** | | |
| Tail padding | | **4** | | round 92 up to 8-byte struct alignment |
| **Total** | | **96** | | |

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

## FileEntryExtras layout (224 bytes)

Boxed behind `Option<Box<...>>` in `FileEntry`. Only allocated when at least one
rarely-used field is needed (symlinks, devices, hardlinks, ACLs, xattrs,
atimes, crtimes, checksums, user/group names).

Rust reorders fields for minimal padding. Optimal packing groups 8-byte-aligned
fields first (168 bytes), then 4-byte-aligned fields (52 bytes), totaling 220
bytes rounded up to 224 bytes.

| Field | Type | Size (B) | Align | Niche? | Notes |
|---|---|---:|---:|---|---|
| `link_target` | `Option<PathBuf>` | 24 | 8 | yes | NonNull niche |
| `user_name` | `Option<String>` | 24 | 8 | yes | NonNull niche |
| `group_name` | `Option<String>` | 24 | 8 | yes | NonNull niche |
| `checksum` | `Option<Vec<u8>>` | 24 | 8 | yes | NonNull niche; up to 32B data |
| `xattr_list` | `Option<XattrList>` | 24 | 8 | yes | XattrList wraps Vec |
| `atime` | `i64` | 8 | 8 | - | access time seconds |
| `crtime` | `i64` | 8 | 8 | - | creation time seconds |
| `hardlink_dev` | `Option<i64>` | 16 | 8 | no | 8B discriminant + 8B payload |
| `hardlink_ino` | `Option<i64>` | 16 | 8 | no | 8B discriminant + 8B payload |
| `atime_nsec` | `u32` | 4 | 4 | - | nanosecond component |
| `rdev_major` | `Option<u32>` | 8 | 4 | no | device major |
| `rdev_minor` | `Option<u32>` | 8 | 4 | no | device minor |
| `hardlink_idx` | `Option<u32>` | 8 | 4 | no | hardlink preservation |
| `acl_ndx` | `Option<u32>` | 8 | 4 | no | access ACL index |
| `def_acl_ndx` | `Option<u32>` | 8 | 4 | no | default ACL index (dirs) |
| `xattr_ndx` | `Option<u32>` | 8 | 4 | no | xattr index |
| **Subtotal** | | **220** | | | |
| Tail padding | | **4** | | | round 220 up to 8-byte alignment |
| **Total** | | **224** | | | + ~16B malloc header overhead |

### Per-entry totals

**Common case** (regular file, no extras):
- FileEntry inline: 96 bytes
- PathBuf name heap: ~30 bytes typical + ~16B malloc overhead = ~46 bytes
- Arc<Path> dirname: amortized ~0 bytes (shared via PathInterner)
- extras: None (0 bytes)
- **~160 bytes per entry**

**Worst case** (extras populated):
- FileEntry inline: 96 bytes
- FileEntryExtras heap block: 224 bytes + ~16B malloc overhead
- Plus heap allocs for populated Option<PathBuf>, Option<String>, Option<Vec>
- **~336+ bytes per entry**

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
| Inline struct size | 96 B | 24 B | 4.0x |
| Common-case total per entry | ~160 B | ~60 B | 2.7x |
| At 1M files | ~153 MB | ~57 MB | 2.7x |
| Heap allocations per entry | 1-2 | 0 | - |

The measured 25.9x RSS gap at 1M files (197 MB vs 7.6 MB) exceeds the 2.7x
structural overhead calculated here. The additional gap comes from:
1. `Vec<FileEntry>` capacity overhead (Vec doubles capacity, wasting up to 50%)
2. Per-allocation malloc metadata (16 bytes per alloc on glibc/jemalloc)
3. Malloc fragmentation (small allocations waste alignment padding)
4. `std::fs::Metadata` cached in `FileListEntry` during traversal
5. Additional data structures (sort index, filter chain, hardlink maps)

## Top waste contributors

Ranked by per-entry cost in the common case (regular files, no extras):

1. **PathBuf `name` (24B inline + heap alloc)** - upstream stores basename
   inline via flexible array member with zero separate allocation. Savings:
   24B inline + 1 alloc/entry. Fix: arena-allocate name (RSS-7, in progress)
   or flat flist with inline basename (RSS-A.4).

2. **Arc<Path> `dirname` (16B vs 8B)** - upstream uses a plain `const char*`
   (8 bytes). `Arc<Path>` is a fat pointer (16 bytes) because Path is unsized.
   Savings: 8B/entry. Fix: use a thin u32 arena offset or intern index.

3. **Option<u32> uid/gid (8B each vs 4B each)** - upstream stores uid/gid as
   4-byte extras. `Option<u32>` wastes 4 bytes per field on the discriminant.
   Savings: 8B/entry (4B per field x 2). Fix: presence bitfield + raw u32.

4. **u64 `size` (8B vs 4B)** - upstream stores only the low 32 bits inline
   (`len32`), with a conditional 4B extra for the high 32 bits
   (`FLAG_LENGTH64`). Savings: 4B/entry. Fix: store u32 inline, promote to
   u64 via extras only for files > 4 GB.

5. **FileFlags (3B vs 2B)** - upstream uses uint16 for flags. Our 3-byte
   `FileFlags` adds 1 byte. Savings: 1B/entry. Fix: pack into u16.

6. **bool content_dir (1B)** - could be a single bit in a flags field.
   Savings: 1B/entry (minor).

## Recommendations for RSS-A.3 and RSS-A.4

### RSS-A.3: Compact Option fields

- Replace `Option<u32>` uid/gid with raw u32 fields and a u16 presence
  bitfield. Saves 8 bytes inline per entry.
- Pack `content_dir` into the presence bitfield. Saves 1 byte.
- Consider storing mode as u16 (upstream uses uint16). Saves 2 bytes but
  truncates upper mode bits - verify upstream parity first.
- Move `mtime_nsec` behind a presence bit (most transfers omit nsec).
  Saves 4 bytes when unused.
- Net savings: 12-15 bytes per entry (96B -> 81-84B, rounds to 80-88B).

### RSS-A.4: Flat flist backing store

Match upstream's contiguous allocation model:
- Single contiguous buffer per flist segment.
- Fixed-size header (target: 32-40 bytes) per entry.
- Variable-length basename packed inline after the header.
- Optional extras packed before the header (upstream convention).
- Zero per-entry heap allocations.
- Target: ~52-60 bytes per common-case entry, matching upstream.

### FileEntryExtras compaction (RSS-A.12)

- Replace six `Option<u32>` fields (rdev_major/minor, hardlink_idx, acl_ndx,
  def_acl_ndx, xattr_ndx) with raw u32 values and a presence bitfield.
  Saves 6 x 4B = 24 bytes.
- Replace two `Option<i64>` fields (hardlink_dev, hardlink_ino) with raw i64
  values and presence bits. Saves 2 x 8B = 16 bytes.
- atime/crtime are already stored as raw i64 (0 = absent via Default) with
  no Option overhead - no change needed.
- Net savings in extras: ~40 bytes (224B -> ~184B).
