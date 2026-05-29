# RSS-9.c: transfer consumer PathHandle migration

> **Status: prototype / not landed.** This transfer-consumer migration
> was never applied. The prerequisite `PathHandle`/`PathArena` types do
> not exist; the production `FileEntry` still uses `PathBuf` +
> `Arc<Path>`. The only landed dedup is the `Arc<Path>` dirname interner.
> The real arena/flat backing store is designed in
> `docs/design/flat-flist-representation.md` and built from scratch by
> RSS-A.5.a-f (gated on RSS-2 profiling). See
> `docs/audit/arena-prototype-landing-gap.md`.

Task: RSS-9.c (#2927). Branch: `docs/rss-9c-transfer-pathhandle`.
Prerequisites: RSS-8.a (PathHandle type definition, PR #4980),
RSS-8.b (read-path migration), RSS-8.c (write-path migration).
Downstream: RSS-10 (benchmark validation, size assertion tightening).

## Summary

After RSS-8.a-c replaced `PathBuf` and `Arc<Path>` fields in
`FileEntry` with 4-byte `PathHandle` tokens backed by a `PathArena`
(frozen `lasso::RodeoReader`), all consumers that access entry paths
must thread a `&PathArena` reference through their call chains. The
`protocol` crate's own consumers (sort, name_cmp, incremental) were
migrated in RSS-8.b. This document covers the remaining consumers in
the `transfer`, `engine`, and `matching` crates - the code paths that
open files, construct destination paths, build protocol messages, and
drive the delta pipeline.

The `transfer` crate is the largest consumer, with ~65 call sites
across the generator, receiver, and pipeline modules. The `engine`
crate has ~5 call sites in the delete pipeline. The `matching` crate
has zero `FileEntry` path calls (its `fuzzy/search.rs:69` calls
`fs::DirEntry::path()`, not `FileEntry::path()`).

## Call-site inventory

### Notation

Every call site listed below calls one of the four path accessors
that gain a `&PathArena` parameter:

| Accessor | Current signature | New signature |
|----------|------------------|---------------|
| `path()` | `&self -> &PathBuf` | `&self, &PathArena -> &Path` |
| `name()` | `&self -> &str` | `&self, &PathArena -> &str` |
| `dirname()` | `&self -> &Arc<Path>` | `&self, &PathArena -> &Path` |
| `name_bytes()` | `&self -> Cow<[u8]>` | `&self, &PathArena -> Cow<[u8]>` |

### Receiver: transfer loop modules

The receiver is the densest consumer cluster. It constructs filesystem
destination paths from `FileEntry` path data for every file operation.

| File | Line(s) | Accessor | Usage |
|------|---------|----------|-------|
| `receiver/transfer/sync.rs` | 105 | `path()` | Destination path: `dest_dir.join(entry.path())` |
| `receiver/transfer/sync.rs` | 118-121 | `path()` | Verbose dir listing: `relative_path.display()` |
| `receiver/transfer/candidates.rs` | 58 | `path()` | Debug logging: `entry.path().display()` |
| `receiver/transfer/candidates.rs` | 88 | `name()` | Daemon filter: `e.name()` |
| `receiver/transfer/candidates.rs` | 95 | `name()` | Failed-dir check: `fd.failed_ancestor(e.name())` |
| `receiver/transfer/candidates.rs` | 101 | `name()` | Skip logging |
| `receiver/transfer/candidates.rs` | 119 | `path()` | Dry-run path: `dest_dir.join(entry.path())` |
| `receiver/transfer/candidates.rs` | 138 | `path()` | Stat path: `dest_dir.join(entry.path())` |
| `receiver/transfer/pipeline.rs` | 196, 215 | `path()` | Basis config: `file_entry.path()` into `BasisFileConfig` |
| `receiver/transfer/pipeline.rs` | 234, 261 | `path()` | Verbose logging: `file_entry.path().display()` |
| `receiver/transfer/pipeline.rs` | 362 | `path()` | Progress event: `path: file_entry.path()` |
| `receiver/transfer/pipeline.rs` | 450 | `path()` | Verbose logging |
| `receiver/transfer/pipelined.rs` | 128 | `path()` | Redo path construction: `entry.path()` |
| `receiver/transfer/pipelined.rs` | 162 | `path()` | Dir verbose logging |
| `receiver/transfer/pipelined_incremental.rs` | 141 | `path()` | Redo path construction |

### Receiver: directory operations

| File | Line(s) | Accessor | Usage |
|------|---------|----------|-------|
| `receiver/directory/creation.rs` | 57 | `name()` | Daemon filter check |
| `receiver/directory/creation.rs` | 65 | `path()` | Dir path: `entry.path().to_path_buf()` |
| `receiver/directory/creation.rs` | 244 | `path()` | Single-dir path construction |
| `receiver/directory/creation.rs` | 322 | `path()` | `create_directory_if_needed` |
| `receiver/directory/creation.rs` | 330, 336, 340, 382 | `name()` | Failed-dir tracking |
| `receiver/directory/deletion.rs` | 72 | `path()` | Build dir-children map |
| `receiver/directory/links.rs` | 53 | `path()` | Symlink relative path |
| `receiver/directory/links.rs` | 221 | `path()` | Hardlink relative path |
| `receiver/directory/links.rs` | 243 | `path()` | Error logging |
| `receiver/directory/links.rs` | 255 | `name()` | Hardlink name display |
| `receiver/directory/links.rs` | 272 | `path()` | Device/FIFO path |

### Receiver: file list operations

| File | Line(s) | Accessor | Usage |
|------|---------|----------|-------|
| `receiver/file_list/sanitize.rs` | 39 | `path()` | Security validation |
| `receiver/file_list/sanitize.rs` | 107 | `path()` | `--relative` slash stripping |
| `receiver/file_list/receive.rs` | 311 | `path()` | Delete pipeline segment publish |
| `receiver/quick_check.rs` | 147 | `path()` | Reference dir join |

### Generator

| File | Line(s) | Accessor | Usage |
|------|---------|----------|-------|
| `generator/itemize.rs` | 223 | `path()` | Itemize line formatting |
| `generator/file_list/inc_recurse.rs` | 72 | `name()` | INC_RECURSE segment matching |
| `generator/transfer/transfer_loop.rs` | 259 | `path()` | Debug log: `send_files(ndx, path)` |
| `generator/transfer/transfer_loop.rs` | 298 | `path()` | Dry-run itemize: `file_entry.path().to_string_lossy()` |
| `generator/transfer/transfer_loop.rs` | 437 | `path()` | Debug log: `sender finished path` |
| `generator/transfer/transfer_loop.rs` | 447 | `path()` | Progress event: `path: file_entry.path()` |

### Engine: delete pipeline

| File | Line(s) | Accessor | Usage |
|------|---------|----------|-------|
| `engine/src/delete/extras.rs` | 135 | `path()` | `segment_basenames`: extract leaf filename |
| `engine/src/delete/cohort_index.rs` | 224 | `path()` | `basename_of(entry.path())` for cohort lookup |

### Matching crate

No `FileEntry` path accessor calls. The `fuzzy/search.rs:69` call
is `std::fs::DirEntry::path()`, which is unrelated. No migration
needed.

### Test fixtures (non-exhaustive)

Tests that construct `FileEntry` and call path accessors exist across
all consumer crates. These gain a test-local `PathArena` instance.
Representative sites:

| File | Usage |
|------|-------|
| `receiver/tests/errors_and_timeouts/sanitize_file_list.rs` | `ctx.file_list[0].path()` assertions |
| `engine/src/delete/cohort_index.rs` tests | `FileEntry::new_file(PathBuf::from(...))` |
| `transfer/src/pipeline/job.rs` tests | `FileEntry::new_file(name.into(), ...)` |
| `generator/file_list/entry.rs` tests | `FileEntry::new_file(...)` |
| `generator/open_source.rs` tests | `TempDir::path()` (not FileEntry) |

## Migration plan

### Strategy: arena co-located with file list

Each `ReceiverContext` and `GeneratorContext` already owns a
`Vec<FileEntry>`. The migration adds a `PathArena` field alongside
this vector, forming the same `(arena, entries)` ownership pair
that `FileList` uses in the `protocol` crate. The arena is populated
during file list reception (receiver) or filesystem walk (generator),
frozen before the transfer loop begins, and shared by reference
across all consumer methods.

### Phase 1: ReceiverContext (largest consumer, ~40 call sites)

**Step 1a: Add arena field.**

```rust
pub struct ReceiverContext {
    // ... existing fields ...
    file_list: Vec<FileEntry>,
    /// Path arena for resolving FileEntry path handles.
    /// Populated during file list reception, frozen before transfer.
    paths: PathArena,
    // ...
}
```

The arena is initialized during `receive_file_list()` via the
`FileListReader`, which already owns a `PathInterner` (line 116 of
`read/mod.rs`). After RSS-8.b, `FileListReader` builds entries with
`PathHandle` tokens pointing into its arena. On completion, the
arena transfers to `ReceiverContext::paths`.

**Step 1b: Thread `&PathArena` through receiver methods.**

Each method on `ReceiverContext` that accesses entry paths gains
access to `self.paths`. Since these are `&self` or `&mut self`
methods, the arena is available as `&self.paths` without any
signature changes to the methods themselves. The migration is
purely internal:

```rust
// Before:
let relative_path = entry.path();

// After:
let relative_path = entry.path(&self.paths);
```

For closures in iterator chains (e.g., `candidates.rs:119`), the
arena reference is captured:

```rust
// Before:
.map(|(idx, entry)| (idx, entry, dest_dir.join(entry.path())))

// After:
let paths = &self.paths;
.map(|(idx, entry)| (idx, entry, dest_dir.join(entry.path(paths))))
```

**Step 1c: Parallel consumers (`rayon::par_iter`).**

The pipeline module uses `rayon::par_iter` for parallel signature
computation (`pipeline.rs:191`). The `PathArena` in its frozen state
wraps `RodeoReader`, which is `Sync`. Sharing `&self.paths` across
rayon worker threads requires no synchronization:

```rust
let paths = &self.paths;
batch.par_iter().map(|(_, file_entry, file_path)| {
    let basis_config = BasisFileConfig {
        relative_path: file_entry.path(paths),
        // ...
    };
    find_basis_file_with_config(&basis_config)
}).collect()
```

**Step 1d: Sanitize and strip_leading_slashes.**

`sanitize_file_list()` calls `entry.path()` inside `retain()` and
mutates entries via `strip_leading_slashes()`. The mutable arena
reference is needed for `strip_leading_slashes` (which may re-intern
the stripped path). Since `retain()` yields `&FileEntry` (immutable),
and `strip_leading_slashes` runs in a separate loop over `&mut
self.file_list`, there is no borrow conflict:

```rust
// Immutable access for validation:
let paths = &self.paths;
self.file_list.retain(|entry| {
    let path = entry.path(paths);
    // ... validation ...
});

// Mutable access for slash stripping:
for entry in &mut self.file_list {
    if entry.path(&self.paths).has_root() {
        entry.strip_leading_slashes(&mut self.paths);
    }
}
```

The second loop requires splitting the borrow: `entry` borrows
`self.file_list[i]` mutably, while `self.paths` is borrowed
separately. Rust's borrow checker allows this because
`file_list` and `paths` are distinct fields.

### Phase 2: GeneratorContext (~6 call sites)

**Step 2a: Add arena field.**

```rust
pub struct GeneratorContext {
    // ... existing fields ...
    file_list: Vec<FileEntry>,
    full_paths: Vec<PathBuf>,
    /// Path arena for resolving FileEntry path handles.
    paths: PathArena,
    // ...
}
```

The generator builds its file list via `create_entry()` in
`file_list/entry.rs`. After RSS-8.c, entry construction interns
paths into the arena. The arena transfers to `GeneratorContext`
after the file list build phase and is frozen before the transfer
loop.

**Step 2b: Thread through generator methods.**

All generator path accesses are in methods on `&self` or `&mut self`,
so `self.paths` is directly available:

```rust
// transfer_loop.rs:259 - debug log
let entry_path = self.file_list[ndx].path(&self.paths).display().to_string();

// transfer_loop.rs:447 - progress event
let event = TransferProgressEvent {
    path: file_entry.path(&self.paths),
    // ...
};

// itemize.rs:223 - format line
let path = entry.path(paths);
```

The `format_itemize_line` function is a free function that takes
`&FileEntry`. It gains a `&PathArena` parameter:

```rust
pub(crate) fn format_itemize_line(
    iflags: &ItemFlags,
    entry: &FileEntry,
    paths: &PathArena,  // new
    is_sender: bool,
    ctx: &ItemizeContext,
) -> String { ... }
```

### Phase 3: Engine delete pipeline (~2 call sites)

The delete pipeline receives `&[FileEntry]` slices from the transfer
layer. After migration, these slices must be accompanied by a
`&PathArena`.

**`compute_extras` in `extras.rs`:**

```rust
// Before:
fn segment_basenames(entries: &[FileEntry]) -> HashSet<OsString> {
    for entry in entries {
        if let Some(name) = entry.path().file_name() { ... }
    }
}

// After:
fn segment_basenames(entries: &[FileEntry], paths: &PathArena) -> HashSet<OsString> {
    for entry in entries {
        if let Some(name) = entry.path(paths).file_name() { ... }
    }
}
```

**`CohortIndex::record` in `cohort_index.rs`:**

```rust
// Before:
if let Some(name) = basename_of(entry.path()) { ... }

// After:
if let Some(name) = basename_of(entry.path(paths)) { ... }
```

The `compute_extras` and `CohortIndex::build` functions gain a
`&PathArena` parameter. The arena flows from
`ReceiverContext::delete_ctx` -> `DeleteContext::observe_segment_for_delete`
-> `compute_extras`.

### Phase 4: Test fixtures

Every test that constructs `FileEntry` and calls path accessors
creates a test-local arena:

```rust
#[cfg(test)]
fn test_arena() -> PathArena {
    PathArena::new()
}

#[test]
fn example() {
    let mut arena = test_arena();
    let entry = FileEntry::new_file(&mut arena, "foo.txt", 100, 0o644);
    assert_eq!(entry.name(&arena), "foo.txt");
}
```

Test helpers like `make_test_entry` in `pipeline/job.rs:185` gain
an `&mut PathArena` parameter or use a module-level fixture.

## API surface changes

### Public function signatures that change

| Function | Crate | Change |
|----------|-------|--------|
| `format_itemize_line()` | `transfer` | Adds `paths: &PathArena` parameter |
| `segment_basenames()` | `engine` | Adds `paths: &PathArena` (private) |
| `basename_of()` | `engine` | No change (takes `&Path`, not `&FileEntry`) |
| `CohortIndex::build()` | `engine` | Adds `paths: &PathArena` |
| `compute_extras()` | `engine` | Adds `paths: &PathArena` |

### Struct fields added

| Struct | Field | Type |
|--------|-------|------|
| `ReceiverContext` | `paths` | `PathArena` |
| `GeneratorContext` | `paths` | `PathArena` |

### TransferProgressEvent unchanged

`TransferProgressEvent::path` is `&'a Path`. After migration, the
value comes from `entry.path(&arena)` which returns `&Path`. The
lifetime ties to the arena, which outlives the event. No type change
needed.

### BasisFileConfig unchanged

`BasisFileConfig::relative_path` is `&'a Path`. After migration,
the value comes from `entry.path(&arena)`. No type change.

### TransferConfig unchanged

`TransferConfig` (in `config/mod.rs`) contains no `FileEntry` path
accessors. It stores destination paths, flags, and algorithm
selections. The PathHandle migration does not touch it.

### Public transfer API stability

The `ReceiverContext::run`, `ReceiverContext::run_sync`, and
`GeneratorContext::run` entry points take the same parameters before
and after migration. The arena is internal state - callers provide
entry lists via `receive_file_list()` (receiver) or `build_file_list()`
(generator), which populate the arena as a side effect. No public API
signature changes propagate beyond the `transfer` crate boundary.

## Backward compatibility

### Within-workspace breakage (intentional, contained)

The `FileEntry` path accessor changes (`path()`, `name()`, etc.)
break all call sites that do not pass a `&PathArena`. This breakage
is intentional - it makes wrong-arena usage a compile-time error.
All affected call sites are within the `oc-rsync` workspace (no
external consumers exist). The compiler enumerates every site that
needs updating.

### No semver implications

`FileEntry` is `pub` but its fields are `pub(super)`. Accessor
methods are `pub`, but the `protocol` crate is a workspace-internal
dependency. The `transfer`, `engine`, and `matching` crates are also
workspace-internal. No external crate depends on these APIs.

### TransferConfig and ServerConfig unaffected

The only transfer-crate types exposed to `core` and `cli` are
`TransferConfig`, `ServerConfig`, `TransferProgressEvent`, and the
`run`/`run_sync` entry points. None of these carry `FileEntry` path
data in their signatures. The migration is invisible to callers
above the `transfer` crate.

## Testing strategy

### Tier 1: compilation

After migration, `cargo check --workspace --all-targets` must
succeed. The compiler enforces that every path accessor receives a
`&PathArena`. Any missed call site produces a type error.

### Tier 2: golden wire tests

`crates/protocol/tests/golden/` contains byte-level golden tests for
wire encoding/decoding. The arena is in-memory only and does not
affect the wire format. These tests pass unchanged, confirming that
the read-path round-trip (intern -> freeze -> resolve) preserves
byte-exact fidelity.

### Tier 3: interop tests

`tools/ci/run_interop.sh` runs end-to-end transfers against upstream
rsync 3.0.9, 3.1.3, 3.4.1, 3.4.2 in push, pull, daemon, and SSH
modes. These tests exercise every consumer in this spec (receiver
destination path construction, generator source path opening,
itemize formatting, delete extras computation). Byte-identical
transfer results confirm that PathHandle resolution produces the
same paths as the pre-migration `PathBuf` fields.

Specific interop scenarios that exercise migrated consumers:

| Scenario | Consumer exercised |
|----------|--------------------|
| daemon pull (3.4.2) | Receiver: candidates, pipeline, directory creation |
| daemon push (3.4.2) | Generator: transfer_loop, itemize |
| `--delete` pull | Engine: compute_extras, cohort_index |
| `--relative` pull | Receiver: sanitize (slash stripping) |
| `--hard-links` pull | Receiver: links.rs hardlink path construction |
| `--link-dest` pull | Receiver: quick_check reference dir join |
| `--checksum` pull | Receiver: candidates always_checksum path |
| INC_RECURSE pull | Receiver: pipelined_incremental path construction |

### Tier 4: unit tests for arena round-trip

Each consumer module's existing unit tests are updated to use a
test-local `PathArena`. A new helper function validates the
round-trip:

```rust
fn assert_path_roundtrip(arena: &PathArena, handle: PathHandle, expected: &str) {
    assert_eq!(arena.resolve(handle), expected);
    assert_eq!(arena.resolve_path(handle), Path::new(expected));
}
```

### Tier 5: CI matrix

All required CI checks must pass:
- `fmt+clippy` (all platforms)
- `nextest` (stable, Linux/macOS/Windows)
- Linux musl
- Interop validation

### Tier 6: size assertion

After RSS-9.c + RSS-8.d (dirname migration), tighten the
`FileEntry` size assertion from 88 bytes to 64 bytes. This is
deferred to RSS-10 to avoid coupling size changes with consumer
migration.

## Performance considerations

### Path construction frequency

The receiver constructs destination paths (`dest_dir.join(entry.path())`)
once per file per phase. For a 100K-file transfer, this is ~100K
`join` calls in phase 1 and ~0-100 in phase 2 (redo). The join itself
is the dominant cost (~50 ns for path allocation + copy). The
PathHandle resolve adds ~1-2 ns per call (indexed array read on
frozen `RodeoReader`). At 100K files, the total added overhead is
~0.1-0.2 ms - negligible.

### PathHandle deref cost model

`entry.path(&arena)` compiles to:

1. Read `entry.name` (4-byte `Spur` value from inline struct field).
2. Index into `RodeoReader`'s internal `Vec<&str>` (bounds check +
   pointer read).
3. Reinterpret `&str` as `&Path` (zero-cost on Unix, UTF-8
   validation already done at intern time).

Total: 2 memory reads (entry inline field + arena vector slot).
Both are L1-cache-hot during sequential file list iteration because
entries and arena slots are accessed in insertion order, which
matches iteration order.

### Comparison with pre-migration cost

Pre-migration `entry.path()` is a single pointer deref into the
inline `PathBuf` field, then a pointer deref through the `PathBuf`'s
heap allocation. Total: 2 memory reads (inline field + heap buffer).
The heap buffer is separately allocated per entry, so sequential
iteration has poor spatial locality at scale.

Post-migration: 2 memory reads (inline field + arena vector slot).
The arena vector is contiguous, so sequential iteration has excellent
spatial locality. Net effect: comparable or better cache behavior.

### Parallel pipeline (rayon)

The pipeline module's `par_iter` over signature batches
(`pipeline.rs:191`) shares `&PathArena` across rayon workers.
`RodeoReader` is `Sync` and resolve is lock-free (no atomic
operations, no mutex). There is no contention point. The frozen
arena is read-only for the entire transfer phase.

### Generator full_paths parallel array

The generator maintains a parallel `full_paths: Vec<PathBuf>` array
for source file opening (`transfer_loop.rs:318`). This array is
unaffected by PathHandle migration - it stores absolute filesystem
paths for `File::open()`, not the relative wire-format paths stored
in `FileEntry`. The `full_paths` array remains as-is.

### Memory impact

This migration does not reduce memory by itself - the RSS reduction
comes from RSS-8.a-d replacing inline `PathBuf`/`Arc<Path>` fields
with `PathHandle`. RSS-9.c is a pure call-site migration that threads
the arena reference through consumers. The only new memory is two
`PathArena` references (pointer-sized) stored in `ReceiverContext`
and `GeneratorContext`.

## Migration ordering within RSS-9.c

The migration can be done in a single PR or split into sub-PRs by
consumer cluster. Recommended ordering:

1. **Receiver transfer loop** (`sync.rs`, `candidates.rs`,
   `pipeline.rs`, `pipelined.rs`, `pipelined_incremental.rs`).
   Largest cluster, highest risk. Validates the core pattern.

2. **Receiver directory ops** (`creation.rs`, `deletion.rs`,
   `links.rs`). Uses both `path()` and `name()`.

3. **Receiver file list ops** (`sanitize.rs`, `receive.rs`,
   `quick_check.rs`). Includes the mutable `strip_leading_slashes`
   pattern.

4. **Generator** (`itemize.rs`, `transfer_loop.rs`,
   `inc_recurse.rs`). Smaller cluster, straightforward.

5. **Engine delete** (`extras.rs`, `cohort_index.rs`). Leaf consumer,
   2 call sites.

6. **Test fixtures** across all crates. Can be done incrementally
   alongside each cluster.

Each sub-PR keeps all existing tests green. The golden wire tests
and interop tests serve as regression gates at each step.

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Borrow conflict between `self.file_list` and `self.paths` | Low | Distinct struct fields; borrow checker allows simultaneous borrows |
| Mutable arena needed in `sanitize_file_list` | Medium | `strip_leading_slashes` loop runs after `retain` loop; no overlap |
| Large PR size (~65 call sites) | Medium | Split by consumer cluster (5 sub-PRs) |
| Wrong arena passed in cross-segment code | Low | Each context owns one arena; no cross-context sharing |
| Test fixture churn | Medium | Provide `test_arena()` helper; mechanical update |
| Regression in delete pipeline from missing arena | Low | `compute_extras` is called from a single site; compiler catches |

## Cross-references

- `crates/transfer/src/receiver/mod.rs:140-245` - `ReceiverContext` struct
- `crates/transfer/src/generator/context.rs:37-92` - `GeneratorContext` struct
- `crates/transfer/src/receiver/transfer/candidates.rs:34-160` - candidate builder
- `crates/transfer/src/receiver/transfer/pipeline.rs:185-275` - pipelined receiver
- `crates/transfer/src/receiver/transfer/sync.rs:98-130` - sync receiver
- `crates/transfer/src/receiver/directory/creation.rs:50-73` - dir creation
- `crates/transfer/src/receiver/directory/links.rs:43-140` - symlink/hardlink
- `crates/transfer/src/receiver/file_list/sanitize.rs:30-114` - sanitize
- `crates/transfer/src/generator/itemize.rs:216-244` - itemize formatting
- `crates/transfer/src/generator/transfer/transfer_loop.rs:259-447` - send loop
- `crates/engine/src/delete/extras.rs:132-140` - segment_basenames
- `crates/engine/src/delete/cohort_index.rs:224` - cohort basename lookup
- `crates/transfer/src/progress.rs:14-33` - TransferProgressEvent
- `docs/design/rss-8a-arena-handle-type.md` - PathHandle type definition
- `docs/design/rss-8b-fileentry-read-path-migration.md` - read-path migration
- `docs/design/rss-8c-fileentry-write-path-migration.md` - write-path migration
