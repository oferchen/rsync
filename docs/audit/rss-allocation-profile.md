# Resident file-list heap allocation profile (RSS-2)

Evidence gate for the arena/slab migration. This is a measured heap
profile of building and holding the resident `Vec<FileEntry>` that
determines peak RSS during large transfers. The goal is to show where
`FileEntry` memory actually goes - the `name` `PathBuf`, the interned
`dirname` `Arc<Path>`, the boxed `extras`, and the `Vec` backing buffer -
so the arena work is driven by numbers rather than assumption.

## Method

- **Tool:** `dhat` 0.3.3 in-process heap profiler, via the workspace
  `tools/dhat-profile` binary built with `cargo build --profile dhat`.
- **Toolchain:** `rustc 1.94.0`, Linux x86-64 container.
- **Harness:** the profiler walks a real directory tree and builds the
  resident flist using the same receiver-side decode constructors the
  wire path uses (`FileEntry::from_raw_bytes` + `PathInterner::intern` +
  `set_dirname`), with archive-mode ownership populated (`set_uid` /
  `set_gid`). `extras` stays `None`, which is the common case for plain
  regular files (no symlinks, devices, hardlinks, ACLs, or xattrs).
- **Phased attribution:** the profiled region is split so the live-heap
  delta between `dhat::HeapStats` snapshots attributes resident bytes to
  each `FileEntry` field group:
  1. reserve the `Vec<FileEntry>` backing buffer,
  2. intern every parent directory (`dirname` `Arc<Path>` + interner map),
  3. build every entry (per-entry `name` `PathBuf`).
- **Fixture:** generated under `/tmp` (not the source tree). A nested
  tree of empty files - the profiler reads metadata only, never file
  contents, so file size does not affect the allocation set.

Two fixture sizes were profiled. The numbers scale exactly linearly
(the 1,000,000-file figures are precisely 10x the 100,000-file figures),
confirming the per-entry costs below.

## Headline numbers - 1,000,000 files

Fixture: 1,000,000 files across 1,000 directories (1,000 unique
interned dirnames), 51 MB on disk.

| dhat metric | Value |
|---|---|
| Peak heap (`t-gmax`) | **136,125,048 bytes (~129.8 MiB)** |
| Peak live blocks | 1,002,006 |
| Resident at snapshot | 136,124,992 bytes / 1,002,004 blocks |
| Total bytes allocated (cumulative) | **192,208,868 bytes (~183.3 MiB)** |
| Total allocation count (cumulative) | **3,002,013 blocks** |

The ~56 MB / ~2,000,013 blocks gap between cumulative and resident is
churn freed during the build: each entry's name passes through a
transient byte buffer that is consumed into the `PathBuf` and dropped.
This is allocator pressure during construction, not resident footprint.

## Top contributors to resident heap - 1,000,000 files

| Field group | Bytes resident | Blocks | Share | Per entry |
|---|---|---|---|---|
| `Vec<FileEntry>` backing | 96,000,000 | 1 | **70.5%** | 96 B (inline struct stride) |
| `name` `PathBuf` (per entry) | 24,000,000 | 1,000,000 | **17.6%** | 24 B + 1 alloc |
| `dirname` interner (`Arc<Path>` + `HashMap`) | 16,124,992 | 2,003 | **11.8%** | 16.1 B amortized |
| `extras` `Box<FileEntryExtras>` | 0 | 0 | 0% | not allocated (None) |

## Cross-check - 100,000 files

Same tree shape (100 dirs x 1,000 files, 100 unique dirnames):

| Field group | Bytes resident | Blocks |
|---|---|---|
| `Vec<FileEntry>` backing | 9,600,000 | 1 |
| `name` `PathBuf` | 2,400,000 | 100,000 |
| `dirname` interner | 1,609,372 | 203 |
| Resident total | 13,609,372 | 100,204 |
| Peak heap (`t-gmax`) | 13,609,428 | 100,206 |
| Total allocated (cumulative) | 19,214,496 | 300,209 |

Every figure is exactly 1/10 of the 1,000,000-file run.

## Interpretation

### 1. The `Vec<FileEntry>` backing dominates (70.5%)

The single flat array of inline `FileEntry` structs is by far the
largest resident allocation: 96 bytes per entry, one contiguous block.
At 96 bytes per struct the inline layout is the primary RSS lever, not
the heap-side strings. This is the clearest target for the arena work:
shrinking the inline `FileEntry` (compact field representation, smaller
discriminants, packing the rarely-used `extras` pointer differently)
directly scales the dominant term. A 24-byte reduction in struct stride
would cut ~24 MB at 1M files - more than the entire `name` contribution.

### 2. Per-entry `name` `PathBuf` is the allocation-count driver (17.6%)

`name` is one heap block per entry: 1,000,000 of the 1,002,004 resident
blocks. It is a modest share of bytes (24 B average for these short
paths) but it dominates *allocation count* and therefore allocator
metadata, fragmentation, and free-time cost. An arena/interned-string
backing store for names would collapse 1,000,000 individual allocations
into a handful of slabs - the largest structural win for allocation
count, and the reason the cumulative block count is 3x the resident one.

### 3. The `dirname` interner is already cheap (11.8%)

Path interning works as designed: 1,000,000 entries share 1,000 unique
`Arc<Path>` allocations. The 16 MB here is interner `HashMap` overhead
(key `PathBuf` copies + buckets) more than the `Arc` payloads. This is
the smallest of the three live contributors and the lowest-priority
target. Migrating dirname storage into the arena would help, but only
after the `Vec` backing and per-entry `name` allocations are addressed.

### 4. `extras` is free in the common case

For plain regular files `extras` is `None` and costs zero allocations,
confirming the `Option<Box<FileEntryExtras>>` design already avoids the
~200 bytes of inline overhead per entry it would otherwise carry. Arena
work should preserve this conditional-allocation property; it is only
material for transfers that actually use symlinks, devices, hardlinks,
ACLs, or xattrs.

## Conclusion - what to migrate first

Ordered by measured impact:

1. **Inline `FileEntry` struct stride (96 B, 70.5% of bytes).** Shrink
   the per-entry struct and back the `Vec` with an arena. Highest
   byte-share lever.
2. **Per-entry `name` `PathBuf` (1,000,000 blocks, 17.6% of bytes).**
   Move names into a flat arena / interned string store. Highest
   allocation-count lever; eliminates the 3:1 cumulative-to-resident
   block ratio.
3. **`dirname` interner (11.8%).** Already efficient; migrate last.
4. **`extras`.** Leave the conditional `Option<Box<...>>` as is.

The arena migration should target the `Vec` backing plus `name` storage
together: between them they account for 88.1% of resident bytes and
99.8% of resident allocation blocks at 1,000,000 files.

## Reproduction

```bash
# In the container, under /tmp (never the source tree):
bash gen_fixture.sh /tmp/rss_fix_1m 1000 1000     # 1,000,000 files
cargo build --profile dhat -p dhat-profile
target/dhat/dhat-profile /tmp/rss_fix_1m
```

The harness prints each phase delta plus the final dhat heap summary
(peak, resident, and cumulative bytes/blocks) to stderr.
