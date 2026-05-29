# RSS-9.a: sort consumer PathHandle migration spec

> **Status: prototype / not landed.** This sort-consumer migration was
> never applied. The prerequisite `PathHandle` type does not exist; the
> production `FileEntry` still uses `PathBuf` + `Arc<Path>`. The only
> landed dedup is the `Arc<Path>` dirname interner. The real arena/flat
> backing store is designed in
> `docs/design/flat-flist-representation.md` and built from scratch by
> RSS-A.5.a-f (gated on RSS-2 profiling). See
> `docs/audit/arena-prototype-landing-gap.md`.

Task: RSS-9.a (#2925). Branch: `docs/rss-9a-sort-pathhandle`.
Prerequisites: RSS-8.a (PathHandle type definition), RSS-8.b (read-path
migration), RSS-8.c (write-path migration).
Downstream: RSS-9.b (non-sort consumer migration), RSS-10 (benchmark
validation).

## Summary

This document specifies the migration of all **sort consumers** -
functions that compare, order, or deduplicate `FileEntry` values by
path - from direct `PathBuf`/`Arc<Path>` field access to
`PathHandle` resolution through the `PathInterner`. Sort consumers
are the most performance-sensitive path consumers because they run
inside `O(n log n)` comparison closures. The migration must preserve
byte-identical sort ordering and add no measurable regression.

After RSS-8.c, `FileEntry::name` is a `PathHandle` (4-byte `Spur`)
and `FileEntry::dirname` is either `PathHandle` (if RSS-8.d has
landed) or `Arc<Path>` (if RSS-8.d is pending). All path accessor
methods (`name()`, `path()`, `dirname()`, `name_bytes()`) now require
a `&PathInterner` parameter. This task threads that parameter through
every sort-related call site.

## Sort consumer inventory

### Tier 1: `protocol` crate (sort infrastructure)

#### `protocol::flist::sort` (`crates/protocol/src/flist/sort.rs`)

| Function | Lines | Path access pattern | Notes |
|----------|-------|---------------------|-------|
| `SortKey::new` | 43-56 | `entry.name_bytes()`, `entry.is_dir()` | Called once per entry to precompute `last_slash` and `is_dir`. Hot path: `name_bytes()` resolves the handle to wire bytes. |
| `compare_file_entries` | 85-91 | `a.name_bytes()`, `b.name_bytes()` | Public comparator for ad-hoc pairwise comparison. Used by generator file list sorting. |
| `sort_file_list` | 206-279 | `file_list[idx].name_bytes()` inside closure | Main sort entry point. Precomputes `SortKey` per entry, then sorts by `compare_with_keys`. The closure captures `file_list` and calls `name_bytes()` per comparison. |
| `flist_clean` | 316-384 | `file_list[w].name()`, `file_list[r].name()` | Post-sort deduplication. Compares adjacent entries by name for equality. |
| `sort_and_clean_file_list` | 395-402 | Delegates to `sort_file_list` + `flist_clean` | Convenience wrapper. |

#### `protocol::flist::name_cmp` (`crates/protocol/src/flist/name_cmp.rs`)

| Function | Lines | Path access pattern | Notes |
|----------|-------|---------------------|-------|
| `f_name_cmp` | 60-71 | `a.dirname()`, `b.dirname()`, `basename_bytes(a)`, `basename_bytes(b)` | Foundational `(dirname, basename)` comparator. Used by delete pipeline. Does NOT use the protocol-29 file-before-directory rule. |
| `name_cmp_eq` | 88-98 | `a.dirname()`, `b.dirname()`, `basename_bytes(a)`, `basename_bytes(b)` | Equality check with trailing-slash tolerance. |
| `basename_bytes` | 105-124 | `entry.path()`, `entry.dirname()` | Extracts leaf name from full path by stripping dirname prefix. Private helper. |
| `f_name_cmp_components` | 137-142 | None (takes raw `&[u8]` slices) | Already arena-agnostic. No change needed. |

### Tier 2: `transfer` crate (sort call sites)

| Call site | File | Function called | Context |
|-----------|------|-----------------|---------|
| Generator flist build | `transfer/src/generator/file_list/mod.rs:98` | `compare_file_entries` | Sorts sender file list via indirect index permutation. Closure captures `&[FileEntry]`. |
| Generator implied-dirs sort | `transfer/src/generator/file_list/mod.rs:216` | `compare_file_entries` | Second sort call site for `--relative` mode implied-dir segment. |
| Receiver file list finalize | `transfer/src/receiver/file_list/receive.rs:104` | `sort_file_list` | Sorts received file list after wire decode. |
| Receiver INC_RECURSE segment | `transfer/src/receiver/file_list/receive.rs:219` | `sort_file_list` | Sorts sub-list segment for incremental transfer. |
| Receiver incremental builder | `transfer/src/receiver/file_list/incremental.rs:209` | `sort_file_list` | Sorts entries within an incremental segment. |

### Tier 3: `engine` crate (delete pipeline)

| Call site | File | Function called | Context |
|-----------|------|-----------------|---------|
| `DeletePlan::sort_by_name` | `engine/src/delete/plan.rs:214-224` | `f_name_cmp` | Sorts delete entries by constructing transient `FileEntry` per entry. |
| `DeletePlan::ascending_order` | `engine/src/delete/plan.rs:231-234` | `f_name_cmp` | Returns comparator result for external merge callers. |
| `entry_as_file_entry` | `engine/src/delete/plan.rs:243-251` | `FileEntry::new_directory`, `new_file`, `new_symlink` | Constructs throwaway `FileEntry` for comparison. |
| `sort_paths_by_f_name_cmp` | `engine/src/delete/traversal.rs:170-176` | `f_name_cmp` | Sorts directory paths via transient `FileEntry::new_directory`. |
| `DirTraversalCursor::observe_segment` | `engine/src/delete/traversal.rs:87-107` | `sort_paths_by_f_name_cmp` (indirect) | Sorts child directories after observation. |

### Tier 4: `flist` crate (parallel file list building)

| Call site | File | Function called | Context |
|-----------|------|-----------------|---------|
| `sort_file_entries` | `flist/src/sort.rs:6-8` | `sort_unstable_by` on `relative_path` | Sorts `FileListEntry` (flist crate's own entry type, not `FileEntry`). Uses `relative_path: PathBuf` field directly. This is a different type than `protocol::flist::FileEntry` and does not use `PathHandle`. **Not in scope for RSS-9.a.** |
| `sort_dir_entries` | `flist/src/sort.rs:13-15` | `sort_unstable_by_key` on `file_name()` | Sorts `std::fs::DirEntry` by OS filename. No `FileEntry` involvement. **Not in scope.** |
| `sort_os_strings` | `flist/src/sort.rs:19-21` | `sort_unstable` on `OsString` | Plain string sort. **Not in scope.** |

### Tier 5: Integration tests and benchmarks

| Call site | File | Function called |
|-----------|------|-----------------|
| Golden protocol tests | `protocol/tests/golden_protocol_v28_flist.rs` | `sort_file_list`, `compare_file_entries` |
| Sort key tests | `protocol/tests/flist_sort_keys_rp28h.rs` | `sort_file_list` |
| Stress tests | `protocol/tests/flist_stress_tests.rs` | `sort_file_list`, `compare_file_entries`, `sort_and_clean_file_list` |
| Flist benchmark | `protocol/benches/flist_benchmark.rs` | `sort_file_list` |

## Migration plan

### Phase 1: `protocol::flist::sort` - add `&PathInterner` parameter

The sort module is the core infrastructure. Every sort function gains a
`paths: &PathInterner` parameter.

#### `SortKey::new`

```rust
// Before:
impl SortKey {
    fn new(index: usize, entry: &FileEntry) -> Self {
        let bytes = entry.name_bytes();
        // ...
    }
}

// After:
impl SortKey {
    fn new(index: usize, entry: &FileEntry, paths: &PathInterner) -> Self {
        let bytes = entry.name_bytes(paths);
        // ...
    }
}
```

#### `compare_file_entries`

```rust
// Before:
pub fn compare_file_entries(a: &FileEntry, b: &FileEntry) -> Ordering {
    let key_a = SortKey::new(0, a);
    let key_b = SortKey::new(0, b);
    let bytes_a = a.name_bytes();
    let bytes_b = b.name_bytes();
    compare_with_keys(&bytes_a, &key_a, &bytes_b, &key_b)
}

// After:
pub fn compare_file_entries(
    a: &FileEntry,
    b: &FileEntry,
    paths: &PathInterner,
) -> Ordering {
    let key_a = SortKey::new(0, a, paths);
    let key_b = SortKey::new(0, b, paths);
    let bytes_a = a.name_bytes(paths);
    let bytes_b = b.name_bytes(paths);
    compare_with_keys(&bytes_a, &key_a, &bytes_b, &key_b)
}
```

`compare_with_keys` and `compare_with_keys_pre29` operate on raw
`&[u8]` slices and `SortKey` values. They do not access `FileEntry`
fields and require no changes.

#### `sort_file_list`

```rust
// Before:
pub fn sort_file_list(
    file_list: &mut [FileEntry],
    use_qsort: bool,
    protocol_pre29: bool,
)

// After:
pub fn sort_file_list(
    file_list: &mut [FileEntry],
    paths: &PathInterner,
    use_qsort: bool,
    protocol_pre29: bool,
)
```

The closure inside `sort_file_list` currently captures `file_list` to
call `file_list[idx].name_bytes()`. After migration it also captures
`paths`:

```rust
let cmp = |a: &SortKey, b: &SortKey| {
    let bytes_a = file_list[a.index as usize].name_bytes(paths);
    let bytes_b = file_list[b.index as usize].name_bytes(paths);
    compare_with_keys(&bytes_a, a, &bytes_b, b)
};
```

The `SortKey` precomputation loop also threads `paths`:

```rust
let mut keys: Vec<SortKey> = file_list
    .iter()
    .enumerate()
    .map(|(i, e)| SortKey::new(i, e, paths))
    .collect();
```

#### `flist_clean`

```rust
// Before:
pub fn flist_clean(mut file_list: Vec<FileEntry>)
    -> (Vec<FileEntry>, CleanResult)

// After:
pub fn flist_clean(
    mut file_list: Vec<FileEntry>,
    paths: &PathInterner,
) -> (Vec<FileEntry>, CleanResult)
```

The duplicate-detection comparison changes from:

```rust
if file_list[w].name() != file_list[r].name() {
```

to:

```rust
if file_list[w].name(paths) != file_list[r].name(paths) {
```

#### `sort_and_clean_file_list`

```rust
// Before:
pub fn sort_and_clean_file_list(
    mut file_list: Vec<FileEntry>,
    use_qsort: bool,
    protocol_pre29: bool,
) -> (Vec<FileEntry>, CleanResult)

// After:
pub fn sort_and_clean_file_list(
    mut file_list: Vec<FileEntry>,
    paths: &PathInterner,
    use_qsort: bool,
    protocol_pre29: bool,
) -> (Vec<FileEntry>, CleanResult)
```

Delegates to the updated `sort_file_list` and `flist_clean`.

### Phase 2: `protocol::flist::name_cmp` - add `&PathInterner` parameter

#### `f_name_cmp`

```rust
// Before:
pub fn f_name_cmp(a: &FileEntry, b: &FileEntry) -> Ordering {
    let dir_a = path_bytes_to_wire(a.dirname());
    let dir_b = path_bytes_to_wire(b.dirname());
    // ...
    let base_a = basename_bytes(a);
    let base_b = basename_bytes(b);
    base_a.cmp(&base_b)
}

// After:
pub fn f_name_cmp(
    a: &FileEntry,
    b: &FileEntry,
    paths: &PathInterner,
) -> Ordering {
    let dir_a = path_bytes_to_wire(a.dirname(paths));
    let dir_b = path_bytes_to_wire(b.dirname(paths));
    // ...
    let base_a = basename_bytes(a, paths);
    let base_b = basename_bytes(b, paths);
    base_a.cmp(&base_b)
}
```

#### `name_cmp_eq`

```rust
// Before:
pub fn name_cmp_eq(a: &FileEntry, b: &FileEntry) -> bool

// After:
pub fn name_cmp_eq(
    a: &FileEntry,
    b: &FileEntry,
    paths: &PathInterner,
) -> bool
```

#### `basename_bytes`

```rust
// Before:
fn basename_bytes(entry: &FileEntry) -> Vec<u8> {
    let name_cow = path_bytes_to_wire(entry.path().as_path());
    let dir_cow = path_bytes_to_wire(entry.dirname());
    // ...
}

// After:
fn basename_bytes(entry: &FileEntry, paths: &PathInterner) -> Vec<u8> {
    let name_cow = path_bytes_to_wire(entry.path(paths));
    let dir_cow = path_bytes_to_wire(entry.dirname(paths));
    // ...
}
```

#### `f_name_cmp_components` - no change

This function takes raw `&[u8]` slices and has no `FileEntry`
dependency. No migration needed.

### Phase 3: `transfer` crate callers

Each call site threads the `PathInterner` from its owning context.
The interner is co-located with the file list in the `FileList` struct
(or will be after RSS-8.b). All callers already have access to the
file list; they add `.paths` (or equivalent accessor) to reach the
interner.

#### Generator file list sort

```rust
// Before (mod.rs:98):
let cmp = |&a: &usize, &b: &usize|
    compare_file_entries(&file_list_ref[a], &file_list_ref[b]);

// After:
let cmp = |&a: &usize, &b: &usize|
    compare_file_entries(&file_list_ref[a], &file_list_ref[b], paths);
```

The `GeneratorContext` gains a `paths: PathInterner` field (added in
RSS-8.c). The sort closure captures `&self.paths`.

#### Receiver file list finalize

```rust
// Before (receive.rs:104):
sort_file_list(&mut self.file_list, self.config.qsort, pre29);

// After:
sort_file_list(&mut self.file_list, &self.paths, self.config.qsort, pre29);
```

The `FileListReceiver` already owns the `PathInterner` used during
wire decode. The same reference is passed to `sort_file_list`.

#### Receiver incremental segment

```rust
// Before (incremental.rs:209):
sort_file_list(&mut entries, self.use_qsort, false);

// After:
sort_file_list(&mut entries, &self.paths, self.use_qsort, false);
```

Each INC_RECURSE segment has its own `PathInterner` (per RSS-8.a's
per-segment arena design). The segment's interner is passed to the
sort.

### Phase 4: `engine` crate callers

The delete pipeline uses `f_name_cmp` to sort delete plans. These
call sites construct **transient** `FileEntry` values solely for
comparison. After migration, they also need a transient
`PathInterner`.

#### `DeletePlan::sort_by_name` and `ascending_order`

The transient `FileEntry` values are created by `entry_as_file_entry`,
which constructs `FileEntry::new_directory(full_path, 0o755)` etc.
After RSS-8.c, the constructor takes `PathHandle` instead of
`PathBuf`, so `entry_as_file_entry` must intern the path first.

**Option A (selected): transient interner per sort call.**

```rust
impl DeletePlan {
    pub fn sort_by_name(&mut self) {
        let mut paths = PathInterner::new();
        let dir = &self.directory;
        self.extras.sort_unstable_by(|a, b| {
            let ea = entry_as_file_entry(dir, a, &mut paths);
            let eb = entry_as_file_entry(dir, b, &mut paths);
            f_name_cmp(&ea, &eb, &paths)
        });
        self.extras.reverse();
        self.sorted = true;
    }
}
```

The transient interner is dropped after the sort completes. For
typical delete plans (< 10K entries per directory), the interner
overhead is negligible (~1 ms). The deduplication benefit is real:
all entries in the same directory share a dirname handle.

**Option B (rejected): pre-intern all entries.**

Pre-intern all leaf names before sorting, then compare handles.
This does not work because `PathHandle` ordering is insertion-order,
not lexicographic. The comparator still needs to resolve handles
to bytes for comparison.

**Option C (rejected): use `f_name_cmp_components` directly.**

The `f_name_cmp_components` function takes raw `&[u8]` slices and
needs no interner. But `entry_as_file_entry` exists to set the
entry's mode correctly (directory vs file distinction affects the
`sort.rs` comparator, though `f_name_cmp` itself ignores mode).
Switching to `f_name_cmp_components` would bypass the mode-based
sort key, which could cause ordering divergence if the comparator
is later upgraded to use `compare_file_entries`.

Selected option A because it is the smallest change and keeps the
delete pipeline using the same `f_name_cmp` call chain as all other
sort sites.

#### `sort_paths_by_f_name_cmp` (traversal.rs)

```rust
// Before:
fn sort_paths_by_f_name_cmp(paths: &mut [PathBuf]) {
    paths.sort_unstable_by(|a, b| {
        let ea = FileEntry::new_directory(a.clone(), 0o755);
        let eb = FileEntry::new_directory(b.clone(), 0o755);
        f_name_cmp(&ea, &eb)
    });
}

// After:
fn sort_paths_by_f_name_cmp(dir_paths: &mut [PathBuf]) {
    let mut interner = PathInterner::new();
    dir_paths.sort_unstable_by(|a, b| {
        let ha = interner.intern_path(a);
        let hb = interner.intern_path(b);
        let ea = FileEntry::new_directory(ha, 0o755);
        let eb = FileEntry::new_directory(hb, 0o755);
        f_name_cmp(&ea, &eb, &interner)
    });
}
```

Note: the parameter name changes from `paths` to `dir_paths` to
avoid shadowing the interner variable.

### Phase 5: tests and benchmarks

All test factories that construct `FileEntry` values for sort
verification gain a test-local `PathInterner`. The pattern is:

```rust
#[cfg(test)]
mod tests {
    fn make_file(paths: &mut PathInterner, name: &str) -> FileEntry {
        let handle = paths.intern(name);
        FileEntry::new_file(handle, 0, 0o644)
    }

    fn make_dir(paths: &mut PathInterner, name: &str) -> FileEntry {
        let handle = paths.intern(name);
        FileEntry::new_directory(handle, 0o755)
    }

    #[test]
    fn sort_order_golden_comprehensive() {
        let mut paths = PathInterner::new();
        let mut entries = vec![
            make_file(&mut paths, "a"),
            make_dir(&mut paths, "."),
            // ...
        ];
        paths.freeze();  // transition to RodeoReader for Sync reads
        sort_file_list(&mut entries, &paths, false, false);
        // assertions unchanged
    }
}
```

Test count by file:

| File | Approx. tests affected |
|------|----------------------|
| `protocol/src/flist/sort.rs` (inline tests) | 20 |
| `protocol/src/flist/name_cmp.rs` (inline tests) | 12 |
| `protocol/tests/golden_protocol_v28_flist.rs` | 8 |
| `protocol/tests/flist_sort_keys_rp28h.rs` | 4 |
| `protocol/tests/flist_stress_tests.rs` | 6 |
| `protocol/benches/flist_benchmark.rs` | 1 |
| `engine/src/delete/plan.rs` (inline tests) | 4 |
| `engine/src/delete/traversal.rs` (inline tests) | 4 |

## API surface changes

### Public function signature changes

All changes are additive (new `&PathInterner` parameter). No return
types change. No trait implementations change.

| Function | Current signature | New signature |
|----------|------------------|---------------|
| `compare_file_entries` | `(a: &FileEntry, b: &FileEntry) -> Ordering` | `(a: &FileEntry, b: &FileEntry, paths: &PathInterner) -> Ordering` |
| `sort_file_list` | `(file_list: &mut [FileEntry], use_qsort: bool, protocol_pre29: bool)` | `(file_list: &mut [FileEntry], paths: &PathInterner, use_qsort: bool, protocol_pre29: bool)` |
| `flist_clean` | `(file_list: Vec<FileEntry>) -> (Vec<FileEntry>, CleanResult)` | `(file_list: Vec<FileEntry>, paths: &PathInterner) -> (Vec<FileEntry>, CleanResult)` |
| `sort_and_clean_file_list` | `(file_list: Vec<FileEntry>, use_qsort: bool, protocol_pre29: bool) -> (Vec<FileEntry>, CleanResult)` | `(file_list: Vec<FileEntry>, paths: &PathInterner, use_qsort: bool, protocol_pre29: bool) -> (Vec<FileEntry>, CleanResult)` |
| `f_name_cmp` | `(a: &FileEntry, b: &FileEntry) -> Ordering` | `(a: &FileEntry, b: &FileEntry, paths: &PathInterner) -> Ordering` |
| `name_cmp_eq` | `(a: &FileEntry, b: &FileEntry) -> bool` | `(a: &FileEntry, b: &FileEntry, paths: &PathInterner) -> bool` |

### Re-exports from `protocol::flist`

The module re-exports all public sort and comparison functions:

```rust
pub use sort::{
    CleanResult, compare_file_entries, flist_clean,
    sort_and_clean_file_list, sort_file_list,
};
pub use name_cmp::{f_name_cmp, f_name_cmp_components, name_cmp_eq};
```

These re-exports remain unchanged. The new signatures propagate
transparently.

### Trait implementations - no changes

`FileEntry` does not implement `Ord` or `PartialOrd`. Ordering is
provided exclusively through free functions (`compare_file_entries`,
`f_name_cmp`). This is deliberate: the correct ordering depends on
context (protocol version, role). Adding `Ord` to `FileEntry` would
require choosing one ordering and embedding the arena reference, which
conflicts with the `Copy`-friendly `PathHandle` design.

The `PartialEq` implementation on `FileEntry` compares `self.name ==
other.name`. After RSS-8.c, `name` is a `PathHandle` (wrapping
`Spur`), so this becomes a `u32 == u32` comparison. Within the same
interner, two entries with identical path strings have identical
`Spur` values (deduplication guarantee). `PartialEq` remains correct
and does NOT need a `&PathInterner` parameter.

## Backward compatibility

### Breaking changes

All signature changes are breaking at the Rust API level. Every
caller of `sort_file_list`, `compare_file_entries`, `flist_clean`,
`sort_and_clean_file_list`, `f_name_cmp`, and `name_cmp_eq` must
be updated to pass `&PathInterner`.

### Scope of breakage

All callers are workspace-internal crates (`protocol`, `transfer`,
`engine`). No external consumers exist. The migration is a
workspace-internal refactor with no semver implications.

### Migration atomicity

The sort function signatures and their callers must change
atomically - the crate does not compile with a partial migration.
This means the `protocol` crate changes (phases 1-2) land in one
commit, and the cross-crate caller changes (phases 3-4) land in the
same commit or a tightly sequenced PR stack.

**Recommended approach:** single PR with the following commit
structure:

1. `refactor(protocol): thread PathInterner through sort and name_cmp`
   - Phases 1-2: all `protocol` crate sort infrastructure changes.
2. `refactor(transfer): pass PathInterner to sort call sites`
   - Phase 3: all `transfer` crate caller updates.
3. `refactor(engine): pass PathInterner to delete pipeline sort`
   - Phase 4: all `engine` crate caller updates.
4. `test: update sort and comparison tests for PathInterner`
   - Phase 5: test factory changes.

Commits 1-4 are reviewed as a single PR but structured for readable
diff review.

## Testing strategy

### Sort order preservation

The primary correctness invariant is: **the sort order produced
after migration is byte-identical to the sort order before
migration.** Any divergence would cause NDX (file index) mismatches
between sender and receiver, breaking the protocol.

#### Golden test: `sort_order_golden_comprehensive`

The existing golden test in `sort.rs` asserts a specific 23-entry
ordering covering root files, nested dirs, files-before-dirs,
shared prefixes, and deep nesting. This test is the primary
regression gate.

After migration, the test constructs entries via `make_file(&mut
paths, name)` and calls `sort_file_list(&mut entries, &paths,
false, false)`. The expected output vector is unchanged.

#### Golden test: `pre29_sort_order_golden`

Same pattern for protocol < 29 ordering. The expected vector is
unchanged.

#### Protocol v28 golden tests

`golden_protocol_v28_flist.rs` tests round-trip encode/decode with
sort. These tests verify that wire-format bytes are preserved through
the sort pipeline. After migration, the `PathInterner` is populated
during decode and passed to sort. Wire bytes are unchanged.

#### Stress tests

`flist_stress_tests.rs` generates 10K+ entry file lists with random
paths and verifies:
- Total order (no adjacent pair violates the comparator).
- Determinism (three shuffled copies produce the same sorted order).
- Transitivity (for sampled triples, `a <= b` and `b <= c` implies
  `a <= c`).
- `sort_and_clean` deduplication count.

These tests gain a shared `PathInterner` and are otherwise unchanged.

#### Property tests (proptest)

`name_cmp.rs` has proptest-based tests for antisymmetry, reflexivity,
transitivity, and agreement between `f_name_cmp` and
`f_name_cmp_components`. After migration:

```rust
fn arb_entry_with_paths() -> impl Strategy<Value = (FileEntry, PathInterner)> {
    arb_name().prop_map(|n| {
        let mut paths = PathInterner::new();
        let handle = paths.intern(&n);
        let entry = FileEntry::new_file(handle, 0, 0o644);
        paths.freeze();
        (entry, paths)
    })
}
```

The property tests must share a single interner across the two
entries being compared (entries from different interners cannot be
compared). The test setup creates one interner, interns both names,
freezes it, then passes `&paths` to `f_name_cmp`.

#### New regression test: cross-comparator agreement

Add a test verifying that `compare_file_entries(a, b, &paths)` and
`f_name_cmp(a, b, &paths)` agree on ordering for entries with no
file-before-directory ambiguity (both files or both dirs). This
ensures the two comparators remain consistent through the migration.

### Delete pipeline ordering

The delete pipeline tests in `engine/src/delete/plan.rs` and
`engine/src/delete/traversal.rs` verify that `sort_by_name` and
`sort_paths_by_f_name_cmp` produce upstream-compatible ordering.
After migration, these tests create a transient `PathInterner` for
the test scope and pass it through.

### Interop tests

The CI interop harness (`tools/ci/run_interop.sh`) tests end-to-end
wire compatibility with upstream rsync 3.0.9-3.4.2. File list
ordering is implicitly validated because:
- NDX values must agree between sender and receiver.
- Any sort-order divergence causes file-list index mismatches,
  which manifest as wrong-file transfers or protocol errors.

No interop test changes are needed. The migration is transparent at
the wire level.

## Performance considerations

### Resolve overhead in sort comparisons

`sort_file_list` calls `name_bytes()` inside the comparison closure.
After migration, `name_bytes(paths)` resolves the `PathHandle` to
`&str` via `RodeoReader::resolve()` (a single indexed array access),
then converts to wire bytes via `path_bytes_to_wire()`.

**Cost per resolve:** ~1-2 ns (L1 cache hit on the reader's internal
`Vec<&str>`).

**Comparison count:** For `n` entries, `sort_unstable_by` performs
`O(n log n)` comparisons. Each comparison calls `name_bytes` twice
(one per entry). At 100K entries, this is ~3.4M resolves.

**Total added overhead:** ~3.4M * 1.5 ns = ~5 ms for 100K entries.
The sort itself takes ~30-50 ms on commodity hardware, so the
resolve overhead is ~10-15% of sort time.

### SortKey precomputation mitigates overhead

`SortKey::new` is called once per entry (not per comparison). It
calls `name_bytes(paths)` to compute `last_slash` via `memrchr`.
This is `O(n)` total resolves for the precomputation pass,
amortized over the `O(n log n)` comparison phase.

The per-comparison closure calls `name_bytes(paths)` to fetch the
byte slice for `compare_with_keys`. This is the hot path.

### Optimization: cache wire bytes in SortKey

If the ~5 ms overhead is measurable in benchmarks, the sort key can
cache the wire bytes:

```rust
struct SortKey {
    index: u32,
    is_dir: bool,
    last_slash: u32,
    bytes: Vec<u8>,  // cached wire-format bytes
}
```

This eliminates per-comparison resolves at the cost of ~24 bytes per
key (Vec metadata) plus the byte data. For 100K entries with average
20-byte paths, this adds ~4.4 MB temporary allocation during sort.

**Decision:** defer to RSS-10 benchmarking. The current ~5 ms
overhead is likely not measurable against the ~30-50 ms sort
baseline. Premature caching adds complexity and memory.

### flist_clean: O(n) resolves

`flist_clean` makes one `name(paths)` call per entry pair (linear
scan). For 100K entries, this is ~100K resolves = ~150 us. Negligible.

### Delete pipeline: transient interner

`DeletePlan::sort_by_name` creates a transient `PathInterner` per
sort invocation. For a directory with `k` entries, this allocates
a hash map with `k` entries, interns `k` paths, sorts, then drops
the interner.

**Cost:** `O(k)` allocations + `O(k log k)` comparisons with
resolves. For typical delete plans (`k < 1000`), this is < 1 ms.
For pathological cases (`k = 100K` entries in one directory), the
transient interner adds ~2-3 ms. Acceptable.

### Parallel sort: RodeoReader is Sync

`sort_file_list` currently uses a sequential sort (`sort_by` or
`sort_unstable_by`). If a parallel sort is added in the future
(e.g., `rayon::par_sort_unstable_by`), the `&PathInterner` reference
backed by `RodeoReader` is `Sync` and can be shared across rayon
worker threads with no contention. No additional synchronization
is needed.

### Compared to current performance

| Operation | Before | After | Delta |
|-----------|--------|-------|-------|
| `name_bytes()` in comparator | Direct `PathBuf` borrow (0 ns) | `RodeoReader::resolve` + borrow (~1.5 ns) | +1.5 ns/call |
| `name()` in `flist_clean` | Direct `PathBuf::to_str` (0 ns) | `RodeoReader::resolve` (~1.5 ns) | +1.5 ns/call |
| Sort 100K entries total | ~30-50 ms | ~35-55 ms | +5 ms (+10-15%) |
| `flist_clean` 100K entries | ~1 ms | ~1.15 ms | +0.15 ms |

The net sort overhead is dominated by the ~34 MB RSS reduction from
the PathHandle migration (RSS-8.a through RSS-8.d). At 1M entries,
the smaller `FileEntry` size means fewer cache misses during sort,
which offsets the per-resolve overhead.

## Open questions for RSS-9.b

1. **`compare_file_entries` callers outside sort.** The generator
   file list build uses `compare_file_entries` inside a closure that
   sorts an index array. Should the `PathInterner` be captured by
   the closure or passed as a parameter to a wrapper function?
   Decision: closure capture (simpler, matches existing pattern).

2. **`entry_as_file_entry` elimination.** The delete pipeline
   constructs throwaway `FileEntry` values solely for `f_name_cmp`.
   An alternative is a `f_name_cmp_from_parts(dir: &[u8],
   base_a: &[u8], base_b: &[u8])` function that avoids constructing
   `FileEntry` entirely. This would eliminate the need for a transient
   interner. Decision: defer to post-RSS-10 optimization. The
   current approach preserves code structure parity with upstream.

3. **Proptest interner sharing.** The proptest strategies generate
   independent entries. Comparing entries from different interners
   is semantically undefined. The test setup must create a single
   interner for the test scope. This requires the strategy to return
   `(PathHandle, PathInterner)` pairs and merge interners before
   comparison. Decision: use a single shared interner per proptest
   case; refactor the strategy to accept `&mut PathInterner`.

## Cross-references

- `crates/protocol/src/flist/sort.rs` - sort infrastructure
- `crates/protocol/src/flist/name_cmp.rs` - foundational comparator
- `crates/protocol/src/flist/intern.rs` - current PathInterner
- `crates/protocol/src/flist/entry/core.rs:32-72` - FileEntry struct
- `crates/protocol/src/flist/entry/accessors.rs:21-135` - accessors
- `crates/transfer/src/generator/file_list/mod.rs:86-108` - generator
  sort
- `crates/transfer/src/receiver/file_list/receive.rs:89-219` -
  receiver sort
- `crates/transfer/src/receiver/file_list/incremental.rs:209` -
  incremental segment sort
- `crates/engine/src/delete/plan.rs:207-234` - delete plan sort
- `crates/engine/src/delete/traversal.rs:166-176` - traversal sort
- `crates/protocol/tests/golden_protocol_v28_flist.rs` - golden tests
- `crates/protocol/tests/flist_sort_keys_rp28h.rs` - sort key tests
- `crates/protocol/tests/flist_stress_tests.rs` - stress tests
- `docs/design/rss-8a-arena-handle-type.md` - PathHandle type
- `docs/design/rss-8b-fileentry-read-path-migration.md` - read-path
  migration
- `docs/design/rss-8c-fileentry-write-path-migration.md` - write-path
  migration
- `target/interop/upstream-src/rsync-3.4.1/flist.c:3217-3343` -
  upstream `f_name_cmp()`
