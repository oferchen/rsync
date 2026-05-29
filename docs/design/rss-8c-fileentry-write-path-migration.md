# RSS-8.c: FileEntry write-path migration to PathHandle

> **Status: prototype / not landed.** This write-path migration was never
> applied. The production `FileEntry` still uses `PathBuf` + `Arc<Path>`,
> and no `PathHandle`/`PathArena` type exists in the tree. The only
> landed dedup is the `Arc<Path>` dirname interner. The real arena/flat
> backing store is designed in
> `docs/design/flat-flist-representation.md` and built from scratch by
> RSS-A.5.a-f (gated on RSS-2 profiling). See
> `docs/audit/arena-prototype-landing-gap.md`.

Task: RSS-8.c. Branch: `docs/rss-8c-write-path-migration-spec`.
Prerequisites: RSS-8.a (PathHandle type definition, merged PR #4980),
RSS-8.b (PathArena implementation over `lasso::Rodeo`/`RodeoReader`).
Downstream: RSS-8.d (dirname field migration), RSS-9 (consumer
migration), RSS-10 (benchmark validation).

## Summary

This document specifies how to migrate all FileEntry **write paths** -
constructors, builders, wire-decode factories, and test fixture
helpers - to produce `PathHandle` values instead of `PathBuf`. The
arena must be available at every construction site. Threading the
`PathArena` reference through builders is the primary design challenge.

The write-path migration replaces `name: PathBuf` (24 bytes inline +
one heap allocation per entry) with `name: PathHandle` (4 bytes
inline, zero heap allocations). After this step, `FileEntry` shrinks
from 88 bytes to 72 bytes on Unix.

## Scope

**In scope:**
- `FileEntry::new_file`, `new_directory`, `new_symlink`,
  `new_block_device`, `new_char_device`, `new_fifo`, `new_socket`
- `FileEntry::from_raw_bytes` (wire-decode factory)
- `FileEntry::from_raw` (test-only constructor)
- `FileEntry::new_with_type` (private template method)
- `FileEntryBuilder` / `ArenaFileEntryBuilder` unification
- `FileListReader::read_entry` arena threading
- `GeneratorContext::create_entry` arena threading
- Test fixture factories across `protocol`, `engine`, `transfer`

**Out of scope:**
- `dirname: Arc<Path>` migration (RSS-8.d)
- Consumer-side accessor changes (RSS-9)
- `ArenaFileEntry` prototype removal (RSS-10 cleanup)
- `link_target` interning (deferred per RSS-5)

## Producer inventory

All sites that construct `FileEntry` instances, categorized by role:

### Wire decode (receiver)

| Site | File | Call |
|------|------|------|
| Entry decode | `protocol/src/flist/read/mod.rs:642` | `FileEntry::from_raw_bytes(...)` |

Single call site. The `FileListReader` already owns a `PathInterner`
(line 116) which becomes the `PathArena` under RSS-8.b.

### Filesystem scan (sender/generator)

| Site | File | Call |
|------|------|------|
| Regular file | `transfer/src/generator/file_list/entry.rs:70,73` | `FileEntry::new_file(...)` |
| Directory | `transfer/src/generator/file_list/entry.rs:81` | `FileEntry::new_directory(...)` |
| Symlink | `transfer/src/generator/file_list/entry.rs:85` | `FileEntry::new_symlink(...)` |
| Block device | `transfer/src/generator/file_list/entry.rs:94` | `FileEntry::new_block_device(...)` |
| Char device | `transfer/src/generator/file_list/entry.rs:97` | `FileEntry::new_char_device(...)` |
| FIFO | `transfer/src/generator/file_list/entry.rs:99` | `FileEntry::new_fifo(...)` |
| Socket | `transfer/src/generator/file_list/entry.rs:101` | `FileEntry::new_socket(...)` |
| Fake-super | `transfer/src/generator/file_list/entry.rs:311-339` | `build_entry_from_fake_super(...)` |

All routed through `GeneratorContext::create_entry`. The generator
does not currently own a `PathArena`; one must be added to
`GeneratorContext` or its file-list-building state.

### Delete pipeline (engine)

| Site | File | Call |
|------|------|------|
| Plan materialization | `engine/src/delete/plan.rs:246-249` | `new_directory`, `new_symlink`, `new_file` |
| Traversal tests | `engine/src/delete/traversal.rs` (multiple) | `new_directory`, `new_file`, `new_symlink` |
| Context tests | `engine/src/delete/context/tests.rs` | `new_file`, `new_directory` |
| Extras tests | `engine/src/delete/extras.rs` | `new_file` |
| Cohort tests | `engine/src/delete/emitter/tests/` | `new_file` |
| Cohort index | `engine/src/delete/cohort_index.rs:250,263` | `new_file` |

Production path: `delete/plan.rs:246-249`. The delete plan
materializes `FileEntry` from a `DeleteEntryKind` + path. The plan
builder must receive a `&mut PathArena` reference.

### Protocol-internal (sort, segment, name_cmp)

| Site | File | Call |
|------|------|------|
| Sort tests | `protocol/src/flist/sort.rs:409,413` | `new_file`, `new_directory` |
| Segment tests | `protocol/src/flist/segment.rs:177` | `new_file` |
| Name-cmp tests | `protocol/src/flist/name_cmp.rs:152,156,191-192,301` | `new_file`, `new_directory` |

All test-only. These gain a test-local `PathArena` instance.

### Transfer/pipeline test factories

| Site | File | Call |
|------|------|------|
| Pipeline job | `transfer/src/pipeline/job.rs:186` | `new_file` |
| Async dispatch | `transfer/src/pipeline/async_dispatch.rs:70,74` | `new_file`, `new_directory` |
| Async pipeline | `transfer/src/pipeline/async_pipeline.rs:319` | `new_file` |
| Generator tests | `transfer/src/generator/tests.rs` (multiple) | `new_file`, `new_fifo` |
| Hardlink tests | `transfer/src/receiver/file_list/hardlinks.rs` (15+) | `new_file`, `new_directory` |
| Itemize | `transfer/src/generator/itemize.rs:253,257,261` | `new_file`, `new_directory`, `new_symlink` |

Mix of production (pipeline job creation) and test helpers. Production
sites need a `&mut PathArena` threaded from the owning transfer
context.

### Engine benchmarks

| Site | File | Call |
|------|------|------|
| delete_end_to_end | `engine/benches/delete_end_to_end.rs:103,134` | `new_file`, `new_directory` |
| delete_plan_compute | `engine/benches/delete_plan_compute.rs:90` | `new_file` |
| delete_emitter_unlink | `engine/benches/delete_emitter_unlink.rs:81` | `new_directory` |

Benchmark setup code. Each gains a bench-local `PathArena`.

## Arena threading strategy

### Decision: dependency injection via parameter

The `PathArena` is passed explicitly to every construction site. This
is the pattern established in RSS-8.a's API surface design.

**Rejected alternatives:**

1. **Global static arena (`OnceLock<Mutex<PathArena>>`).**
   Incompatible with per-flist ownership (INC_RECURSE per-segment
   arenas). Introduces lock contention. Prevents multi-session use
   (embedded library, test parallelism).

2. **Per-transfer thread-local arena.** Same lifetime problems as
   global; thread-locals do not have a controlled drop point. Arenas
   would leak until thread exit. Incompatible with rayon work-stealing.

3. **Arena stored in `FileEntry` itself (`Arc<PathArena>` field).**
   Re-introduces the `Arc` overhead we are eliminating. Adds 8 bytes
   per entry. Defeats the size reduction goal.

### Arena ownership hierarchy

```
FileListReader (receiver)         GeneratorContext (sender)
└── arena: PathArena              └── arena: PathArena
    ├── used during decode            ├── used during walk
    └── moved to FileList             └── moved to FileList

FileList (post-build, shared)     DeletePlan (engine)
├── arena: PathArena (frozen)     └── arena: PathArena
└── entries: Vec<FileEntry>           └── entries: Vec<FileEntry>
```

### Production threading paths

**Receiver (wire decode):**
```rust
impl FileListReader {
    // PathArena replaces PathInterner at line 116.
    // Already single-threaded; Rodeo is !Sync - matches existing model.
    arena: PathArena,  // was: dirname_interner: PathInterner

    pub fn read_entry<R: Read>(&mut self, reader: &mut R)
        -> io::Result<Option<FileEntry>>
    {
        // ... decode name bytes ...
        let name_handle = self.arena.intern_bytes(&cleaned_name);
        let entry = FileEntry::from_handle(name_handle, size, mode, ...);
        // dirname interning also goes through self.arena
        // (RSS-8.d scope, but compatible with this change)
        Ok(Some(entry))
    }
}
```

**Sender (filesystem walk):**
```rust
impl GeneratorContext {
    arena: PathArena,  // new field

    pub fn create_entry(
        &mut self,
        full_path: &Path,
        relative_path: PathBuf,
        metadata: &std::fs::Metadata,
    ) -> io::Result<FileEntry> {
        // Convert relative_path to handle via arena
        let name_handle = self.arena.intern_path(&relative_path);
        // ... rest of entry construction ...
    }
}
```

**Delete plan (engine):**
```rust
impl DeletePlan {
    pub fn materialize_entry(
        &self,
        arena: &mut PathArena,
        kind: DeleteEntryKind,
        path: PathBuf,
    ) -> FileEntry {
        let handle = arena.intern_path(&path);
        match kind {
            DeleteEntryKind::Dir => FileEntry::new_directory(handle, 0o755),
            DeleteEntryKind::Symlink => FileEntry::new_symlink(handle, ...),
            _ => FileEntry::new_file(handle, 0, 0o644),
        }
    }
}
```

## Constructor API changes

### Current signatures

```rust
impl FileEntry {
    fn new_with_type(name: PathBuf, size: u64, file_type: FileType,
                     permissions: u32, link_target: Option<PathBuf>) -> Self;
    pub fn new_file(name: PathBuf, size: u64, permissions: u32) -> Self;
    pub fn new_directory(name: PathBuf, permissions: u32) -> Self;
    pub fn new_symlink(name: PathBuf, target: PathBuf) -> Self;
    pub fn new_block_device(name: PathBuf, perms: u32, maj: u32, min: u32) -> Self;
    pub fn new_char_device(name: PathBuf, perms: u32, maj: u32, min: u32) -> Self;
    pub fn new_fifo(name: PathBuf, permissions: u32) -> Self;
    pub fn new_socket(name: PathBuf, permissions: u32) -> Self;
    pub fn from_raw_bytes(name: Vec<u8>, size: u64, mode: u32,
                          mtime: i64, nsec: u32, flags: FileFlags) -> Self;
}
```

### New signatures

```rust
impl FileEntry {
    fn new_with_type(name: PathHandle, size: u64, file_type: FileType,
                     permissions: u32, link_target: Option<PathBuf>) -> Self;
    pub fn new_file(name: PathHandle, size: u64, permissions: u32) -> Self;
    pub fn new_directory(name: PathHandle, permissions: u32) -> Self;
    pub fn new_symlink(name: PathHandle, target: PathBuf) -> Self;
    pub fn new_block_device(name: PathHandle, perms: u32, maj: u32, min: u32) -> Self;
    pub fn new_char_device(name: PathHandle, perms: u32, maj: u32, min: u32) -> Self;
    pub fn new_fifo(name: PathHandle, permissions: u32) -> Self;
    pub fn new_socket(name: PathHandle, permissions: u32) -> Self;

    /// Wire-decode factory. Interns `name` bytes into `arena`.
    pub fn from_raw_bytes(arena: &mut PathArena, name: &[u8], size: u64,
                          mode: u32, mtime: i64, nsec: u32,
                          flags: FileFlags) -> Self;
}
```

### Key changes

1. **`PathBuf` parameters become `PathHandle`.** Callers intern the
   path first, then pass the handle. This makes the interning step
   explicit and testable.

2. **`from_raw_bytes` takes `&mut PathArena` + `&[u8]`.** The arena is
   needed to intern the decoded bytes. The `Vec<u8>` ownership
   transfer is replaced by a borrow - the arena copies the bytes into
   its internal storage.

3. **`dirname` extraction deferred.** The current `new_with_type`
   calls `extract_dirname(&name)` to compute the dirname from the
   path. With PathHandle, dirname extraction requires resolving the
   handle back to `&Path` via the arena. Two options:

   - **Option A (selected):** Pass `dirname: PathHandle` as a separate
     parameter to `new_with_type`. Caller computes dirname handle via
     `arena.intern(parent_of(path))`. Keeps the constructor pure (no
     arena dependency).
   - **Option B:** Pass `&PathArena` to `new_with_type` so it can
     resolve the name handle and extract dirname. Adds arena coupling
     to the constructor.

   Option A is preferred because it decouples the RSS-8.c (`name`
   field) and RSS-8.d (`dirname` field) migrations. During the
   transition period between RSS-8.c and RSS-8.d, `dirname` remains
   `Arc<Path>` computed from the resolved name. The `new_with_type`
   signature during RSS-8.c is:

   ```rust
   fn new_with_type(
       name: PathHandle,
       dirname: Arc<Path>,  // becomes PathHandle in RSS-8.d
       size: u64,
       file_type: FileType,
       permissions: u32,
       link_target: Option<PathBuf>,
   ) -> Self;
   ```

4. **Convenience wrapper for callers that have a `&str`/`&Path`:**

   ```rust
   impl PathArena {
       /// Interns a path and returns the handle.
       /// Convenience for callers that have a PathBuf/&Path to intern.
       pub fn intern_path(&mut self, path: &Path) -> PathHandle;

       /// Interns raw bytes (wire format) and returns the handle.
       pub fn intern_bytes(&mut self, bytes: &[u8]) -> PathHandle;
   }
   ```

## Wire-decode integration

The wire decode path in `FileListReader::read_entry_with_flist` (line
491-743) currently:

1. Decodes name bytes via prefix compression (`read_name`)
2. Applies encoding conversion (`apply_encoding_conversion`)
3. Validates/cleans the name (`clean_and_validate_name`)
4. Calls `FileEntry::from_raw_bytes(cleaned_name, ...)`
5. Extracts parent from the constructed path
6. Interns dirname via `self.dirname_interner.intern(parent)`
7. Sets the interned dirname on the entry

After migration:

1. Decodes name bytes via prefix compression (unchanged)
2. Applies encoding conversion (unchanged)
3. Validates/cleans the name (unchanged)
4. **Interns cleaned bytes: `let name_handle = self.arena.intern_bytes(&cleaned_name)`**
5. **Constructs entry: `FileEntry::from_handle(name_handle, ...)`**
6. Dirname interning uses the same arena (RSS-8.d completes this)

The `from_raw_bytes` factory is replaced by `from_handle`:

```rust
impl FileEntry {
    /// Creates a file entry from a pre-interned path handle.
    ///
    /// This is the preferred constructor for wire protocol decoding.
    /// The caller is responsible for interning the path bytes into
    /// the arena before calling this constructor.
    pub fn from_handle(
        name: PathHandle,
        dirname: Arc<Path>,  // computed by caller; becomes PathHandle in RSS-8.d
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_nsec: u32,
        flags: FileFlags,
    ) -> Self {
        Self {
            name,
            dirname,
            size,
            mtime,
            uid: None,
            gid: None,
            extras: None,
            mode,
            mtime_nsec,
            flags,
            content_dir: true,
        }
    }
}
```

### Transition: `from_raw_bytes` compatibility shim

During migration, `from_raw_bytes` can be retained as a convenience
that takes `&mut PathArena`:

```rust
impl FileEntry {
    /// Backward-compatible factory that interns bytes and constructs.
    ///
    /// Prefer `from_handle` when the caller already has a PathHandle.
    pub fn from_raw_bytes(
        arena: &mut PathArena,
        name: &[u8],
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_nsec: u32,
        flags: FileFlags,
    ) -> Self {
        let handle = arena.intern_bytes(name);
        let path = arena.resolve_path(handle);
        let dirname = extract_dirname(path);
        Self::from_handle(handle, dirname, size, mode, mtime, mtime_nsec, flags)
    }
}
```

This shim is removed in RSS-8.d when `dirname` becomes a `PathHandle`.

## FileEntryBuilder unification

The current codebase has two builder types:
- `ArenaFileEntryBuilder<'arena>` (prototype, feature-gated)
- No production builder (constructors are direct)

### Plan

1. **Remove `ArenaFileEntryBuilder` and `ArenaFileEntry`.** The
   prototype served its measurement purpose (RSS-7). The production
   `FileEntry` type now gains native arena support.

2. **No builder pattern for `FileEntry` itself.** The direct
   constructors (`new_file`, `new_directory`, etc.) remain the primary
   API. Rationale: the type-based constructor pattern
   (`new_file`/`new_directory`/`new_symlink`) is clearer than a
   builder that sets type via a method, and matches upstream's
   `make_file()` dispatch.

3. **The `from_handle` constructor is the unified "builder" entry
   point** for the wire-decode path. Callers that need to set optional
   fields continue using the set-after-construct pattern (unchanged
   from today: `entry.set_uid(uid)`, `entry.set_gid(gid)`, etc.).

## Test factory updates

### Strategy: test-local arena with helper

Tests that construct `FileEntry` values gain a test-scoped
`PathArena`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a PathArena and helper for test fixture construction.
    fn test_arena() -> PathArena {
        PathArena::new()
    }

    /// Convenience: intern a string and create a file entry.
    fn make_file(arena: &mut PathArena, name: &str, size: u64) -> FileEntry {
        let handle = arena.intern(name);
        let dirname = extract_dirname(Path::new(name));
        FileEntry::new_file(handle, size, 0o644)
    }

    fn make_dir(arena: &mut PathArena, name: &str) -> FileEntry {
        let handle = arena.intern(name);
        FileEntry::new_directory(handle, 0o755)
    }
}
```

### Impact assessment

- **~848 total construction call sites** across the workspace.
- **~331 in the `protocol` crate** (mostly tests).
- **~80 in `engine`** (mostly delete pipeline tests and benchmarks).
- **~400+ in `transfer`** (generator, receiver, pipeline tests).
- Production (non-test) call sites: **~15-20.**

The majority of changes are mechanical: add a `&mut arena` parameter,
intern the path string, pass the handle. A workspace-wide
search-and-replace with a test helper function makes this tractable.

### Shared test helper module

To avoid duplicating the `make_file`/`make_dir`/`make_symlink` pattern
in every test module, add a `test_support` module to the `protocol`
crate (cfg(test)-gated or in a `dev-dependencies`-only helper crate):

```rust
/// Test fixture helpers for FileEntry construction with PathArena.
///
/// Provides convenience constructors that handle arena interning
/// internally, reducing boilerplate in test code.
#[cfg(test)]
pub mod test_support {
    use crate::flist::{FileEntry, PathArena, PathHandle};
    use std::path::Path;

    pub fn file_entry(arena: &mut PathArena, name: &str, size: u64) -> FileEntry {
        let handle = arena.intern(name);
        FileEntry::new_file(handle, size, 0o644)
    }

    pub fn dir_entry(arena: &mut PathArena, name: &str) -> FileEntry {
        let handle = arena.intern(name);
        FileEntry::new_directory(handle, 0o755)
    }

    pub fn symlink_entry(
        arena: &mut PathArena,
        name: &str,
        target: &str,
    ) -> FileEntry {
        let handle = arena.intern(name);
        FileEntry::new_symlink(handle, target.into())
    }
}
```

## Migration ordering

The write-path migration is sequenced to keep the codebase compilable
and tests green at every intermediate commit.

### Phase 1: Add PathArena field alongside PathInterner

- Add `arena: PathArena` field to `FileListReader` (alongside existing
  `dirname_interner: PathInterner`).
- Add `arena: PathArena` field to `GeneratorContext`.
- Both fields unused initially. This establishes the ownership
  topology without changing any behavior.

### Phase 2: Migrate `from_raw_bytes` to arena-based

- Change `FileEntry::from_raw_bytes` signature to take
  `&mut PathArena, &[u8]` instead of `Vec<u8>`.
- Internally, intern the bytes and store `PathHandle` in the `name`
  field.
- `FileEntry::name` field type changes from `PathBuf` to `PathHandle`.
- All accessors that returned `&PathBuf` / `&str` now require
  `&PathArena` (RSS-9 scope, but the accessor changes must land
  simultaneously with the field change).
- Update `FileListReader::read_entry` to call the new signature.

**Critical constraint:** The `name` field type change and accessor
signature changes must land atomically (same commit/PR). Otherwise the
codebase does not compile. This makes phase 2 the largest single
change.

### Phase 3: Migrate typed constructors

- Change `new_file`, `new_directory`, `new_symlink`,
  `new_block_device`, `new_char_device`, `new_fifo`, `new_socket`
  to take `PathHandle` instead of `PathBuf`.
- Update `GeneratorContext::create_entry` to intern paths before
  constructing entries.
- Update `build_entry_from_fake_super` similarly.
- Update `delete/plan.rs` materialization.

### Phase 4: Migrate test factories

- Add test helper functions to protocol crate.
- Mechanically update all ~800+ test construction sites.
- Split into per-crate PRs to keep reviews manageable:
  - `protocol` crate tests (~331 sites)
  - `engine` crate tests (~80 sites)
  - `transfer` crate tests (~400+ sites)

### Phase 5: Remove legacy PathBuf support

- Remove `from_raw` (test-only constructor that takes `PathBuf`).
- Remove `extract_dirname` usage that returns `Arc<Path>` from a
  `PathBuf` (RSS-8.d will replace dirname entirely).
- Remove `ArenaFileEntry` and `ArenaFileEntryBuilder` prototypes.
- Tighten size assertion: 88 -> 72 bytes (Unix).

### Alternative: big-bang vs incremental

**Big-bang** (phases 2+3+4 in one PR): simpler to review holistically,
avoids intermediate states, but produces a 2000+ line diff touching
every crate. Acceptable if done as a single focused PR with clear
section headers.

**Incremental** (as described above): safer, reviewable in isolation,
but phases 2 and 3 cannot truly be separated because changing the
`name` field type forces all constructors to change simultaneously.

**Recommended:** Phases 1+2+3 in one PR (the "core migration"), phase
4 split into per-crate follow-up PRs, phase 5 as cleanup.

## Interaction with RSS-8.d (dirname migration)

RSS-8.d replaces `dirname: Arc<Path>` with `dirname: PathHandle`. The
design here deliberately keeps `dirname` as `Arc<Path>` during
RSS-8.c. The interaction points:

1. **`new_with_type` computes dirname from name.** During RSS-8.c, it
   resolves the `PathHandle` back to `&Path` via the arena, then calls
   `extract_dirname()` to get `Arc<Path>`. This is a temporary
   round-trip that RSS-8.d eliminates.

2. **`FileListReader` interns dirname separately.** Today it calls
   `dirname_interner.intern(parent)` after construction (line 654-658).
   During RSS-8.c, this continues using the legacy `PathInterner`.
   RSS-8.d switches to `arena.intern(parent_str)` returning a
   `PathHandle`.

3. **Delete plan dirname.** `delete/plan.rs` constructs entries whose
   dirnames come from path splitting. During RSS-8.c, dirname is still
   `Arc<Path>`. RSS-8.d migrates it.

This ordering means RSS-8.c can land independently of RSS-8.d. Each
delivers a measurable size reduction: RSS-8.c drops 20 bytes (PathBuf
24 -> PathHandle 4), RSS-8.d drops 12 bytes (Arc<Path> 16 ->
PathHandle 4).

## Error handling

### Arena capacity

`lasso::Rodeo` can hold up to `u32::MAX - 1` (4,294,967,294) unique
strings. For realistic file lists (even at 10M entries with 200K unique
basenames), this is never exhausted. No capacity error handling is
needed.

If an unrealistic workload somehow exhausts `Spur` space, `Rodeo`
panics on `get_or_intern`. This matches the existing behavior where
`Vec::push` panics on OOM - both are unrecoverable. No special
handling.

### Invalid handle resolution

Resolving a `PathHandle` from the wrong arena panics in
`RodeoReader::resolve`. This is a programming error (logic bug), not a
runtime condition. The structural co-ownership invariant (arena and
entries in the same `FileList`) prevents this at the API level.

In debug builds, `try_resolve` can be used for extra validation. In
release builds, the direct `resolve` path is preferred for
performance.

## Performance impact

### Construction hot path

| Operation | Before | After | Delta |
|-----------|--------|-------|-------|
| `from_raw_bytes` (receiver) | `PathBuf::from(OsStr::from_bytes(&name))` - malloc + memcpy | `arena.intern_bytes(&name)` - hash + bump-copy | ~same latency; eliminates free on teardown |
| `new_file` (sender) | `PathBuf` already constructed by caller | Caller calls `arena.intern_path()`; hash + dedup | +1 hash per entry; saves malloc + atomic-clone on dirname |
| Clone during sort | `PathBuf::clone()` - malloc + memcpy | `PathHandle::clone()` - memcpy of 4 bytes | -1 malloc per clone |

### Deduplication benefit (sender)

The sender walks the filesystem and often encounters repeated directory
structures. With the arena, if the sender has 10 files in `src/lib/`,
the path prefix `src/lib/` is stored once. Today each
`PathBuf::from("src/lib/foo.rs")` allocates independently.

For a monorepo with 1M files across 10K directories where average
basename length is 20 bytes:
- Before: 1M `malloc(~48)` calls (PathBuf heap backing) = ~48 MB heap
- After: ~210K unique strings interned (10K dirs + 200K basenames) at
  ~20 bytes average = ~4.2 MB arena + ~17 MB hash table = ~21 MB

Net saving: ~27 MB heap at the sender alone.

## Cross-references

- `crates/protocol/src/flist/entry/core.rs:32-72` - struct to modify.
- `crates/protocol/src/flist/entry/constructors.rs:18-174` -
  constructors to migrate.
- `crates/protocol/src/flist/entry/accessors.rs:11-130` - accessors
  affected (RSS-9 scope but must land atomically with field change).
- `crates/protocol/src/flist/entry/arena.rs` - prototype to remove.
- `crates/protocol/src/flist/read/mod.rs:116,642-659` - wire decode
  path and interner ownership.
- `crates/protocol/src/flist/intern.rs` - `PathInterner` to replace
  with `PathArena`.
- `crates/transfer/src/generator/file_list/entry.rs:31-339` - sender
  construction path.
- `crates/engine/src/delete/plan.rs:246-249` - delete plan
  materialization.
- `docs/design/rss-8a-arena-handle-type.md` - PathHandle type
  definition.
- `docs/design/rss-5-fileentry-pool-shape.md` - target layout and
  API-change table.
- `docs/design/rss-4-arena-allocator-eval.md` - lasso selection
  rationale.
