# Arena migration: prototype-vs-production landing gap

Task: RSS-A.0.a (audit). Verified against the tree on 2026-05-29.

## Summary

The file-list arena/slab migration tracked by RSS-7, RSS-8 (and RSS-8.a-c),
and RSS-9 (and RSS-9.a-c) is **not present in the production representation**.
The production `FileEntry` still uses owned per-entry allocations, and the
arena types those tasks describe (`PathHandle`, `PathArena`, `StringArena`)
do not exist anywhere in the workspace. What landed is an unused prototype
plus a modest dirname interner. The flat backing-store design
(`docs/design/flat-flist-representation.md`) is written on the false premise
that the `PathHandle` migration already shipped.

This document records the actual state so downstream planning works from the
tree rather than from the tracker.

## What the tree actually contains

### Production FileEntry is unchanged

`crates/protocol/src/flist/entry/core.rs` still defines:

```rust
pub struct FileEntry {
    name: PathBuf,                       // per-entry heap allocation
    dirname: Arc<Path>,                  // shared via PathInterner
    size: u64,
    mtime: i64,
    uid: Option<u32>,
    gid: Option<u32>,
    extras: Option<Box<FileEntryExtras>>, // 224 B heap block when set
    mode: u32,
    mtime_nsec: u32,
    flags: FileFlags,
    content_dir: bool,
}
```

No arena handle is used for `name` or `dirname`.

### The arena types named by the design do not exist

A workspace-wide search for `PathHandle`, `PathArena`, `StringArena`,
`FlatFileList`, `FileEntryHeader`, and the `lasso` / `Spur` interner returns
zero references. None of these types have been written.

### What did land

1. A `bumpalo::Bump` prototype in
   `crates/protocol/src/flist/entry/arena.rs`: `ArenaFileEntry`,
   `ArenaFileEntryBuilder`, and `FilePath`. It is re-exported from
   `crates/protocol/src/flist/entry/mod.rs` but has **no production caller** -
   the `FileListReader` build path does not use it. It is dead public API.

2. The dirname interner in `crates/protocol/src/flist/intern.rs`:
   `PathInterner` maps each unique directory `PathBuf` to a shared
   `Arc<Path>` via a `HashMap`. This deduplicates dirnames but does not
   remove the per-entry `name: PathBuf` allocation and does not provide a
   4-byte handle.

## Consequences

- The 25.9x RSS gap at 1M files (per `docs/audit/file-entry-layout-audit.md`)
  is unaddressed by code: every cost contributor that the arena migration was
  meant to remove is still live.
- `docs/design/flat-flist-representation.md` builds `FileEntryHeader` on a
  `PathHandle` that does not exist and states "RSS-8 already replaced
  name/dirname with PathHandle." That premise is false and must be corrected
  before the header layout is frozen (tracked by RSS-A.0.c).
- The dead `arena.rs` prototype is exported but unused; it should be wired
  into production or removed (tracked by RSS-A.0.b).

## Follow-ups

- RSS-A.0.b: wire the bumpalo prototype into production or delete it.
- RSS-A.0.c: correct the flat-flist design doc's `PathHandle` premise and
  specify the header's own concrete 4-byte handle scheme.
- RSS-A.0.d: mark RSS-7/8/9 as prototype-only in the tracker.
- RSS-A.5.a-f: build the flat store from scratch (no `PathHandle`
  prerequisite), gated on RSS-2 allocation profiling per the design's own
  validation gate.

## References

- `crates/protocol/src/flist/entry/core.rs` - current `FileEntry`.
- `crates/protocol/src/flist/entry/arena.rs` - unused bumpalo prototype.
- `crates/protocol/src/flist/intern.rs` - `Arc<Path>` dirname interner.
- `docs/design/flat-flist-representation.md` - flat store design (premise to fix).
- `docs/audit/file-entry-layout-audit.md` - per-entry overhead numbers.
