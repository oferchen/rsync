# macOS Batched-Metadata API Survey vs Current `lstat` Pattern

Tracking task: #2153 (RESOLVED).
Related: #1864 (memory bench, same hot path), #2210 (arena allocator, same hot path),
#1045 (100K stat overhead), #1083 (parallel stat batch sizing), #1833 (io_uring statx).

This audit surveys the macOS-specific metadata APIs available for reducing per-file
syscall cost during file-list construction. oc-rsync ships a Linux fast path
(`statx`) and a portable fallback (`fstatat` / `lstat`); macOS currently rides the
fallback. The question for #2153 is whether `getattrlistbulk(2)` or related
Darwin-only interfaces would meaningfully cut metadata syscalls on the macOS
walker, and whether the engineering cost is justified.

The deliverable is forward-looking only. Upstream rsync 3.4.1 issues one
`lstat`/`fstatat` per entry on every platform, so this is not parity work.

## 1. Current oc-rsync macOS Stat Pattern

### 1.1 Call sites that issue one syscall per entry

| Phase | Site | Syscall |
|-------|------|---------|
| File-list walk (root + every child) | `crates/flist/src/file_list_walker.rs` | `lstat` via `fs::symlink_metadata` |
| File-list walk (lazy builder) | `crates/flist/src/lazy_metadata.rs:107` | `lstat` |
| Parallel collect (rayon fan-out) | `crates/flist/src/parallel.rs:131-151` | `lstat` per task |
| Receiver-side generator | `crates/transfer/src/generator/file_list/walk.rs:267` | dispatches to `batch_stat_dir_entries` |
| Receiver batch stat | `crates/transfer/src/generator/file_list/batch_stat.rs:43-50` | `fs::symlink_metadata` (or `fs::metadata` for `--copy-links`) |
| Quick-check (size+mtime) | `crates/transfer/src/generator/file_list/mod.rs:140,168` | a second `lstat` per file |
| Directory-relative variant | `crates/flist/src/batched_stat/dir_stat.rs:50-77` | `fstatat` against a held dir fd |

The Linux fast path (`statx` against a dir fd, see
`crates/flist/src/batched_stat/dir_stat.rs:89-140` and `statx_support.rs`) is
gated on `#[cfg(all(target_os = "linux", not(target_env = "musl")))]`. macOS
hits `fstatat` (the `#[cfg(unix)]` branch) for dir-relative work, plain
`fs::symlink_metadata` everywhere else.

### 1.2 Syscall cost on macOS today

For a tree of N files on macOS:

- File-list walk: N `lstat` (or `fstatat`) calls.
- Generator quick-check: another N `lstat` calls on the destination side.
- Total: 2N syscalls per refresh of a tree, plus `readdir` per directory.

Rayon parallelism above `DEFAULT_STAT_THRESHOLD = 64`
(`crates/transfer/src/parallel_io.rs:16`) saturates cores but does not reduce
syscall count. Sharded caching (`crates/flist/src/batched_stat/cache.rs`) cuts
duplicate stats inside a single transfer but does nothing for the first walk.

### 1.3 Where it hurts

The 100K-file profile (`docs/audits/100k-stat-syscall-overhead.md`) records the
warm-cache floor at ~1 us per `lstat`: 100 ms sequential, ~12.5 ms 8-way
parallel. Cold APFS lookups cost 50-200 us, so 100K files run 5-20 s wall.
APFS metadata reads are not cheap: each `lstat` traverses the path components
and reads the file record from the B-tree. The hot region for batching is
exactly this cold metadata fetch.

## 2. macOS APIs Surveyed

### 2.1 `getattrlist(2)` - single-file alternative to `stat`

- Path-based. Caller supplies an `attrlist` bitmap selecting which attribute
  groups to return (`ATTR_CMN_*`, `ATTR_VOL_*`, `ATTR_FILE_*`, etc.).
- Returns a packed buffer covering only the requested fields. Lets callers ask
  for the BSD subset (mode, size, mtime, owner) plus Darwin extras (`crtime`,
  Finder info, fork sizes) in **one syscall**.
- Cost: roughly equivalent to `lstat` for the BSD subset, slightly more for
  extras. Not a batching win on its own.
- Already used in the codebase: `setattrlist` for crtime writes in
  `crates/metadata/src/apply/timestamps.rs`. Apple's reference is the only
  way to read or set crtime on Darwin.

### 2.2 `getattrlistbulk(2)` - the batching primitive

- Operates on an **open directory fd**. One syscall returns metadata for
  multiple directory entries.
- Caller supplies: dir fd, `attrlist` describing which attributes to fetch per
  entry, output buffer, output buffer length, and flags.
- Returns: number of entries decoded into the buffer in this call, plus a
  packed stream of per-entry records. Caller iterates by reading each record's
  length prefix and advancing.
- Behaviour: each call drains as many entries as fit in the buffer. Repeated
  calls continue the iteration; an empty return signals end of directory.
- Available since OS X 10.10 (2014). Apple recommends it for tools that walk
  large trees (Finder, Spotlight indexer, `mds`).
- Replaces the `readdir` + per-entry `lstat` loop with one syscall per buffer.
  For a buffer that holds ~50-200 entries, that is a 50-200x reduction in
  syscall count for the walk phase.

Limitations:

- Symlink semantics are fixed. `getattrlistbulk` returns metadata for the
  entry itself (no follow). Useful for the `lstat` path; the `--copy-links`
  path still needs a follow-up `getattrlist` per symlink target.
- The output format is opaque: each record is a `u32` length prefix followed
  by the requested attributes in `attrlist` order, with alignment padding.
  Decoding requires correct knowledge of every attribute's wire layout.
- Behaviour on non-APFS filesystems (HFS+, SMB, NFS, FAT) varies. SMB and NFS
  re-fetch from the server per entry, so the syscall-count win exists but the
  wall-time win shrinks. FAT lacks Darwin-specific attributes and falls back.
- Does not return errors per entry: a permission error or stale entry aborts
  the current call and the caller restarts with the next offset. Mid-batch
  failures are messier to attribute than a per-entry `lstat` error.

### 2.3 `fstatat64(2)` / `getattrlistat(2)`

- macOS has `fstatat` (POSIX). No `fstatat64` is publicly documented; macOS
  uses 64-bit inode types by default since 10.5.
- `getattrlistat(2)` is the dir-relative form of `getattrlist`. Same single-entry
  cost, useful when a directory fd is already open.

### 2.4 `attrlist` (the struct) and `searchfs(2)`

- `attrlist` is the bitmap descriptor used by `getattrlist`,
  `getattrlistbulk`, `setattrlist`, and `getattrlistat`. Not a syscall itself.
- `searchfs(2)` performs a kernel-side query (e.g. "files with mtime > X").
  Not useful for unconditional walks but interesting for incremental modes
  (`--update`, `--ignore-existing`, future `--newer-than`).

### 2.5 Foundation bulk fetch (`NSURL` / `NSFileManager`)

- Objective-C/Swift API.
  `NSFileManager.enumerator(at:includingPropertiesForKeys:options:errorHandler:)`
  walks a tree and pre-fetches properties. Backed by `getattrlistbulk` since
  10.10 according to public WWDC sessions and disassembly.
- Wrapping Foundation from Rust requires `objc2`/`core-foundation` and a
  bridging layer. The underlying syscall path is the same as #2.2 with extra
  Objective-C dispatch overhead. Not the right entry point for oc-rsync.

## 3. Performance Comparison Estimates

Per-walk syscall counts for a directory of N entries:

| Path | Syscalls (walk only) | Notes |
|------|----------------------|-------|
| Today on macOS | 1 `opendir` + N `lstat` (or `fstatat`) | + 2N including generator quick-check |
| `getattrlistbulk` | 1 `open` + ceil(N/B) calls | B = entries per buffer, typically 50-200 |
| Linux `statx` (today) | 1 `open` + N `statx` | Same count, lower per-call cost |
| Linux io_uring statx chain (#1833) | 1 `open` + ceil(N/SQE) `io_uring_enter` | SQE batch typically 64-1024 |

Wall-time estimates for 100K files on APFS (SSD):

- Today (sequential `lstat`): ~100 ms warm, 5-20 s cold (from #1045 profile,
  extrapolated to APFS - measured Linux numbers are within 2x for warm).
- Today (8-way parallel `lstat`): ~12.5 ms warm, 1-3 s cold.
- `getattrlistbulk` (sequential, B=100): ~5-10 ms warm (1000 calls vs 100K),
  300-800 ms cold (kernel reads the directory's B-tree pages contiguously and
  packs many records per page-fault).
- `getattrlistbulk` (per-directory parallelism over many subdirs): bounded by
  filesystem IOPS rather than syscall entry cost; expected 2-5x over
  parallel `lstat` on cold APFS, near-parity on warm.

The cold-cache win is the interesting one. APFS lays out file records in a
single B-tree per volume; pulling 100 records from one page beats 100
independent lookups that each touch the path-resolution cache. Empirical
numbers from Apple's own tools (`mdfind`, Spotlight reindex) suggest 5-10x
wall-time improvements on cold trees with deep directory structure.

## 4. Recommendation: Defer, with a Tracked Follow-Up

**Defer implementation. Document the interface and revisit after #1864 and
#2210 land.**

Reasoning:

1. **The walker is not currently the macOS bottleneck.** The
   `macos-fastio-fallback.md` and `fast-io-fallback-macos-vs-linux.md` audits
   identify the write path (no `copy_file_range`, no io_uring, fallback to
   `read`/`write`) as the dominant gap. Walker work spent before fixing the
   data-plane gap returns less.
2. **The Linux fast path is also not wired into the walker.** `statx` exists
   in `crates/flist/src/batched_stat/` but the call sites in
   `crates/transfer/src/generator/file_list/batch_stat.rs` still go through
   `fs::symlink_metadata`. Adding a second platform-specific path before the
   first is consumed compounds complexity.
3. **`getattrlistbulk` decoding is fiddly and unsafe.** The per-entry record
   layout is sensitive to the `attrlist` bitmap order, alignment padding, and
   variable-length fields (names, security tokens). A bug returns garbage
   metadata silently. This needs differential testing against `lstat` results
   on every supported macOS version and filesystem (APFS, HFS+, SMB, NFS).
4. **Symlinks and `--copy-links` still need fallback.** A non-trivial fraction
   of real rsync workloads follow symlinks, which `getattrlistbulk` cannot
   express. The macOS walker would need both code paths.
5. **Memory pressure from `BatchedStatCache` and per-entry `fs::Metadata`
   allocation (tracked in #1864 and #2210) is the more impactful win.** Once
   the cache is right-sized and entries are arena-allocated, the marginal
   benefit of cutting walker syscalls is smaller in absolute terms.

Revisit triggers:

- After #1864 (memory bench) shows the walker is still hot on macOS.
- After #2210 (arena allocator) lands and per-entry allocation is no longer the
  bottleneck.
- If a user reports that an oc-rsync transfer on a 1M-file APFS tree is
  meaningfully slower than upstream rsync (which would be surprising since
  upstream uses the same `lstat` path).
- If macOS gains parity with Linux's io_uring for the write path, exposing the
  walker as the next bottleneck.

## 5. Implementation Roadmap (If Adopted)

Sketch only - to be filled in when the audit is revisited.

### 5.1 New module

`crates/flist/src/batched_stat/getattrlistbulk.rs` (Darwin-only).

Public surface:

```rust
#[cfg(target_os = "macos")]
pub struct BulkAttrBatch {
    dir_file: fs::File,
    dir_fd: RawFd,
    attrlist: libc::attrlist,
    buffer: Vec<u8>,
    cursor: usize,
    remaining: u32,
}

#[cfg(target_os = "macos")]
impl BulkAttrBatch {
    pub fn open<P: AsRef<Path>>(dir_path: P) -> io::Result<Self>;
    pub fn next_entry(&mut self) -> io::Result<Option<BulkEntry>>;
}

#[cfg(target_os = "macos")]
pub struct BulkEntry {
    pub name: OsString,
    pub stat: FstatResult,  // reuse the existing type
}
```

Behaviour: each `next_entry` returns the next decoded record from the buffer,
calling `getattrlistbulk` again when the buffer empties. Returns `Ok(None)` on
end of directory.

### 5.2 Integration point

`crates/transfer/src/generator/file_list/walk.rs`: add a Darwin branch that
opens a `BulkAttrBatch` per directory and feeds entries into the existing
`StatResult` pipeline. Keep `batch_stat_dir_entries` as the fallback for
symlink-following mode and non-APFS filesystems.

### 5.3 Safety and review obligations

- Move the new code into `crates/fast_io` (the long-term unsafe consolidation
  target per the workspace unsafe-code policy). Expose a safe API to `flist`.
- Property test: for every directory in a fixture tree, compare
  `BulkAttrBatch` results against `fs::symlink_metadata` per entry. Mode,
  size, mtime, owner, ino must match exactly.
- Filesystem matrix in CI: APFS (default), HFS+ (legacy disk image), SMB
  (macOS-to-macOS share), NFS (macOS-to-Linux export). Fall back to
  `fstatat` on any filesystem that returns `ENOTSUP`.
- Version matrix: macOS 11, 12, 13, 14, 15. Apple has tightened
  `getattrlistbulk` behaviour on each major release.

### 5.4 Out-of-scope for this audit

- io_uring-style submission queue. macOS has no equivalent; `getattrlistbulk`
  is itself the batching primitive.
- Replacement of `readdir` callers outside the file-list walker (deletion
  scanner, hardlink table). These are lower-volume and not on the hot path.

## 6. References

- Apple Developer: `getattrlistbulk(2)`, `getattrlist(2)`, `setattrlist(2)`,
  `attrlist` struct definitions in `<sys/attr.h>`.
- WWDC 2014 Session 712 "What's New in CoreFoundation" (introduction of
  bulk metadata fetch backing for `NSFileManager`).
- oc-rsync internal: `docs/audits/100k-stat-syscall-overhead.md`,
  `docs/audits/parallel-stat-batch-size.md`,
  `docs/audits/macos-fastio-fallback.md`,
  `docs/audits/fast-io-fallback-macos-vs-linux.md`.
- Upstream rsync 3.4.1: `flist.c:send_directory()` issues `do_stat` per
  entry on every platform; no batching is attempted.
