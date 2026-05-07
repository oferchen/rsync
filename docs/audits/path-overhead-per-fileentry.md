# PathBuf and Arc<Path> Overhead per FileEntry RSS Audit

Tracks RSS gap remaining after the dirname interner landed (#1049). Focuses on
the path storage carried by every `FileEntry` once a file list is materialised.

## 1. Current FileEntry path storage

Defined in `crates/protocol/src/flist/entry/core.rs:32-72`. Two path-bearing
fields:

- `name: PathBuf` (line 35) -- relative path of the entry.
- `dirname: Arc<Path>` (line 42) -- interned parent directory.

`PathBuf` is a `Vec<u8>` wrapper: 24 bytes inline (ptr + len + cap on 64-bit)
plus a heap allocation sized to `cap` (typically rounded up to 16 / 32 / 64).
`Arc<Path>` is 16 bytes inline (thin pointer to a `[u8]` slice fat header) and
one heap allocation per unique parent that holds an atomic refcount, weak
count, length, and the bytes (`std::sync::Arc::<[u8]>` layout: 16 bytes header
+ payload).

The walker side `FileListEntry` in `crates/flist/src/entry.rs:7-13` keeps two
`PathBuf` fields (`full_path`, `relative_path`) -- 48 bytes inline + two heap
buffers per yielded entry. It is consumed during traversal and not retained at
scale, so the dominant cost lives in `FileEntry`.

## 2. Per-entry overhead estimate

Assumptions: 100 K entries, average relative path 100 bytes, 100 unique parent
directories of average length 32 bytes.

| Component | Per-entry | 100 K entries |
|-----------|-----------|---------------|
| `PathBuf` inline (`name`) | 24 B | 2.40 MB |
| `PathBuf` heap (round-up to 128 B) | 128 B | 12.80 MB |
| `Arc<Path>` inline (`dirname` ptr) | 16 B | 1.60 MB |
| `Arc<[u8]>` header + payload (shared, 100 dirs) | -- | 0.005 MB |
| jemalloc / glibc overhead (~15 %) | -- | ~2.50 MB |
| **Total path-attributable RSS** | | **~19.3 MB** |

Upstream rsync allocates names from a slab (`pool_alloc` in `lib/pool.c`) and
stores them as `char *` (8 B) plus a shared `dirname` pointer in
`file_struct`. Per-entry overhead is ~16 B fixed + the path bytes themselves
(no allocator header), giving 4-8 B amortised structural overhead. Even at
100-byte names upstream stores ~10.0 MB of payload with negligible header
amplification, versus our ~19 MB.

## 3. Comparison against #1049

#1049 introduced `PathInterner` (`crates/protocol/src/flist/intern.rs`) so the
`dirname` field shares one `Arc<Path>` per unique directory. Before interning,
dirname duplication added ~3.2 MB across 100 K entries (32 B path * 100 K
duplicates). After interning that collapses to a few KB. Remaining gap is
dominated by the `name` `PathBuf`: 24 B inline + heap buffer + allocator
header per entry, none of which interning addressed. The audit estimates ~14
MB still recoverable after #1049.

## 4. Compact-path proposals

1. **Whole-path interning relative to root** -- replace `name: PathBuf` with
   `name: Arc<Path>` and intern via the existing `PathInterner` keyed on the
   full relative path. Saves the per-entry heap buffer (~128 B with rounding)
   when filenames repeat across directories (manifests, lockfiles, license
   files). Trades 24 B inline for 16 B inline.
2. **Common-prefix table** -- store `(parent_index: u32, file_name: SmolStr)`
   with parent_index pointing into a `Vec<Arc<Path>>` owned by the file list.
   Drops the 16 B `Arc` to a 4 B index, leaving an inline 16 B `SmolStr` for
   the basename. Inline budget drops from 40 B (PathBuf+Arc) to 20 B and
   eliminates one allocation for ASCII basenames <= 22 bytes.
3. **Inline name via `SmolStr` / `compact_str`** -- `SmolStr` is 24 B with
   inline storage for <= 22 bytes (Unix) and zero heap alloc on the common
   case (most filenames are short). For paths > 22 B it falls back to an
   `Arc<str>`, ref-countable across renames and clones. Combined with proposal
   2 the per-entry path footprint becomes 24 B (`SmolStr`) + 4 B (parent idx)
   = 28 B with no heap alloc for the typical short basename.

A combined approach (proposals 2 + 3) projects ~10-12 MB saved on the 100 K
benchmark, closing the gap with upstream to within ~3 MB.

## 5. Risks

- **Path mutation.** `--iconv` rewrites filenames during read/write
  (`crates/protocol/src/flist/write/encoding.rs`); converted bytes must
  produce a fresh `Arc<Path>`/`SmolStr`. Local rename paths similarly need a
  reallocation hook -- interned names cannot be mutated in place.
- **Thread-safety of interner.** `PathInterner` is `!Sync` by design (intern.rs
  doc comment, lines 14-18). Whole-path interning would multiply lookups; the
  generator and receiver run in separate threads, so per-thread interners or a
  `RwLock<HashMap<...>>` (with read-mostly traffic) is required. Avoid
  `DashMap` unless contention measurements justify it.
- **Drop ordering.** `Arc<Path>` cycles are impossible (paths are leaves), but
  `FileList` must outlive every `FileEntry` referencing its interner. Today
  the interner is dropped after decoding completes; switching to `Arc`-based
  names means each `FileEntry` carries its own `Arc`, decoupling lifetimes
  cleanly. Verify hardlink groups and `--link-dest` paths still drop in the
  expected order under `--delete`.
- **Hash key cost.** Interning on the full relative path increases the hash
  key length; budget extra CPU for the read path and reuse the existing
  `PathBuf` -> `Arc<Path>` map shape from `intern.rs:43-48`.

## References

- `crates/protocol/src/flist/entry/core.rs:32-83`
- `crates/protocol/src/flist/intern.rs:42-114`
- `crates/flist/src/entry.rs:7-13`
- Upstream `flist.c`, `lib/pool.c` (slab allocator backing `file_struct.basename`).
