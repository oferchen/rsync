# RSS-9.b: filter consumer PathHandle migration

> **Status: prototype / not landed.** This filter-consumer migration was
> never applied. The prerequisite `PathHandle` type does not exist; the
> production `FileEntry` still uses `PathBuf` + `Arc<Path>`. The only
> landed dedup is the `Arc<Path>` dirname interner. The real arena/flat
> backing store is designed in
> `docs/design/flat-flist-representation.md` and built from scratch by
> RSS-A.5.a-f (gated on RSS-2 profiling). See
> `docs/audit/arena-prototype-landing-gap.md`.

Task: RSS-9.b (#2926). Branch: `docs/rss-9b-filter-pathhandle`.
Prerequisites: RSS-8.a (PathHandle type - 4-byte `lasso::Spur`),
RSS-8.b (FileEntry read-path migration), RSS-8.c (FileEntry write-path
migration). Downstream: RSS-9.c (sort consumer migration), RSS-10
(benchmark validation).

## Summary

This document specifies the migration of all filter consumers -
call sites that match `FileEntry` paths against include/exclude/protect
rules - to use the `PathHandle` resolution API. After RSS-8.b/c,
`FileEntry.name` and `FileEntry.dirname` are `PathHandle` values (4-byte
opaque tokens). Every site that extracts a path from a `FileEntry` and
passes it to `FilterSet::allows()`, `FilterChain::allows()`, or
`FilterChain::allows_deletion()` must now resolve the handle through
the `PathArena` first.

The key constraint is that the `filters` crate itself does **not** depend
on the `protocol` crate (where `PathHandle` and `PathArena` live). The
filter API continues to accept `&Path` - the resolution from
`PathHandle` to `&Path` happens at each call site, not inside the
filters crate.

## Inventory: filter consumer call sites

### Category 1: generator-side transfer filtering

These sites evaluate include/exclude rules during file list enumeration
(sender/generator side). After RSS-8.b, the walker code must resolve
`PathHandle` before passing paths to `FilterChain::allows()`.

| # | File | Line | Current call | PathHandle change |
|---|------|------|-------------|-------------------|
| 1 | `transfer/src/generator/file_list/walk.rs` | 113 | `filter_chain.allows(&relative, ...)` | `relative` is a local `PathBuf` built from fs walk, not from `FileEntry` - **no change needed** |
| 2 | `transfer/src/generator/filters.rs` | 53,80 | `FilterChain::new(filter_set)` | Construction from wire rules - **no change needed** (rules are strings, not `PathHandle`) |

### Category 2: receiver-side daemon filter checking

These sites apply daemon-configured filter rules to `FileEntry` instances
received from the wire. The entries' `.name()` accessor now requires a
`PathArena` reference.

| # | File | Line | Current call | PathHandle change |
|---|------|------|-------------|-------------------|
| 3 | `transfer/src/receiver/transfer/candidates.rs` | 89 | `filters.allows(Path::new(e.name()), false)` | Change to `filters.allows(e.path(&arena), false)` |
| 4 | `transfer/src/receiver/directory/creation.rs` | 59 | `filters.allows(Path::new(name), true)` where `name = e.name()` | Change to `filters.allows(e.path(&arena), true)` |

### Category 3: receiver-side deletion filtering

These sites evaluate protect/risk rules before deleting destination files.
Paths come from filesystem readdir, not from `FileEntry`, so no
`PathHandle` resolution is needed.

| # | File | Line | Current call | PathHandle change |
|---|------|------|-------------|-------------------|
| 5 | `transfer/src/receiver/directory/deletion.rs` | 203 | `filter_chain.allows_deletion(&rel_for_filter, is_dir)` | `rel_for_filter` is a `PathBuf` from `dir_relative.join(&name)` (fs readdir) - **no change needed** |

### Category 4: engine-side filtered walker

The `FilteredWalker` in the engine crate applies filters during local
copy directory traversal. Paths come from `walkdir` entries, not from
`FileEntry`.

| # | File | Line | Current call | PathHandle change |
|---|------|------|-------------|-------------------|
| 6 | `engine/src/walk/filtered_walker.rs` | 109 | `self.filters.allows(rel_path, is_dir)` | `rel_path` is stripped from a `walkdir::DirEntry` path - **no change needed** |

### Category 5: CLI filter construction

Filter rules are constructed from CLI option strings and tested against
literal paths.

| # | File | Line | Current call | PathHandle change |
|---|------|------|-------------|-------------------|
| 7 | `cli/src/frontend/tests/transfer_request_with_include.rs` | 45 | `filter_set.allows(Path::new("keep"), true)` | Test uses literal path, not `FileEntry` - **no change needed** |

### Category 6: filter-internal evaluation chain

The filters crate's own evaluation path (`decision.rs`, `compiled/rule.rs`,
`chain/scope.rs`, `chain/mod.rs`) operates on `&Path` received from callers.
These modules never import `FileEntry` or `PathHandle`.

| # | Module | Function | Input type | PathHandle change |
|---|--------|----------|-----------|-------------------|
| 8 | `decision.rs` | `first_matching_rule()` | `path: &Path` | **No change** |
| 9 | `compiled/rule.rs` | `CompiledRule::matches()` | `path: &Path` | **No change** |
| 10 | `chain/mod.rs` | `FilterChain::allows()` | `path: &Path` | **No change** |
| 11 | `chain/mod.rs` | `FilterChain::allows_deletion()` | `path: &Path` | **No change** |
| 12 | `chain/scope.rs` | `has_matching_rule()` | `path: &Path` | **No change** |
| 13 | `set.rs` | `FilterSet::allows()` | `path: &Path` | **No change** |
| 14 | `set.rs` | `FilterSet::allows_deletion()` | `path: &Path` | **No change** |
| 15 | `set.rs` | `FilterSet::excluded_dir_by_non_dir_rule()` | `path: &Path` | **No change** |

### Summary

Of 15 identified call sites, only **2 require changes** (sites 3 and 4) -
both in the receiver's transfer pipeline where daemon filter rules are
checked against `FileEntry` instances. All other sites either construct
paths from filesystem operations (walkdir, readdir) or from string
literals, none of which involve `PathHandle`.

## Migration plan

### Principle: resolve at the boundary

The `filters` crate's public API (`FilterSet::allows(&Path)`,
`FilterChain::allows(&Path)`) does **not** change. The `filters` crate
has no dependency on `protocol` and must not acquire one - it is a
standalone pattern-matching library.

`PathHandle` resolution happens at each call site in the consumer crate
(primarily `transfer`). The pattern is:

```rust
// Before (RSS-8.a/b/c era - FileEntry stores PathHandle)
let name = entry.name(&arena);          // resolve PathHandle -> &str
let path = entry.path(&arena);          // resolve PathHandle -> &Path
if !filters.allows(path, is_dir) { ... }

// The filters crate sees &Path as before - zero awareness of PathHandle.
```

### Site 3: `transfer/src/receiver/transfer/candidates.rs:89`

**Before:**
```rust
let name = e.name();
if name != "." && !filters.allows(Path::new(name), false) {
```

**After:**
```rust
let name = e.name(&arena);
if name != "." && !filters.allows(e.path(&arena), false) {
```

The `arena` reference must be threaded into the closure that filters
transfer candidates. The `arena` is available from the `FileList`
(or the receiver context) - the same `PathArena` that owns the flist's
interned strings.

### Site 4: `transfer/src/receiver/directory/creation.rs:59`

**Before:**
```rust
let name = e.name();
if name != "." && !name.is_empty() {
    return filters.allows(Path::new(name), true);
}
```

**After:**
```rust
let name = e.name(&arena);
if name != "." && !name.is_empty() {
    return filters.allows(e.path(&arena), true);
}
```

Same pattern - thread `&arena` from the receiver context into the
directory creation logic.

### Arena threading

Both changed sites are in the receiver's transfer pipeline. The
`ReceiverContext` already holds the `FileList`, which after RSS-8.b
co-owns the `PathArena`. The arena reference flows as:

```
ReceiverContext
├── file_list: FileList
│   ├── arena: PathArena       (owns interned strings)
│   └── entries: Vec<FileEntry> (owns PathHandle tokens)
├── filter_chain: FilterChain
└── daemon_filter_set: Option<FilterSet>
```

The `arena` is accessed via `self.file_list.arena()` or passed as a
parameter to methods that need it. Since `PathArena` is `Sync` after
freezing, it can be shared across rayon parallel iterators.

## API surface changes

### filters crate: no changes

The `filters` crate's public API is unchanged:

```rust
// These signatures remain identical
impl FilterSet {
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool;
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool;
    pub fn allows_deletion_when_excluded_removed(&self, path: &Path, is_dir: bool) -> bool;
    pub fn excluded_dir_by_non_dir_rule(&self, path: &Path) -> bool;
}

impl FilterChain {
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool;
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool;
}
```

The `GlobMatcher::is_match()` call inside `CompiledRule::matches()`
continues to receive `&Path`. The glob matching library (`globset`)
accepts `AsRef<Path>`, which `&Path` satisfies directly.

### transfer crate: accessor call site changes

Two call sites change from `e.name()` (no args) to `e.name(&arena)` and
from `Path::new(name)` to `e.path(&arena)`. This is a local code change
in the `transfer` crate with no public API impact - both sites are in
private implementation modules.

### Trait compatibility

`PathArena::resolve_path()` returns `&Path`, which implements
`AsRef<Path>`. This is the same type that `GlobMatcher::is_match()`
and `FilterSet::allows()` accept. No trait adapter or wrapper is needed.

## Backward compatibility

### FilterChain public API: unchanged

`FilterChain`'s public API (`allows`, `allows_deletion`,
`enter_directory`, `leave_directory`, `push_scope`) takes `&Path` and
returns `bool`. No signatures change. Code that constructs `FilterChain`
from `FilterSet` continues to work identically.

### FilterSet public API: unchanged

`FilterSet`'s public API (`from_rules`, `allows`, `allows_deletion`,
`is_empty`, etc.) is unaffected. The `from_rules` constructor takes
`FilterRule` values (string patterns), which are independent of
`PathHandle`.

### Why the filters crate stays decoupled

The `filters` crate occupies a low position in the dependency graph:

```
cli -> core -> engine, transfer
                         |
               transfer -> protocol (PathHandle, PathArena)
                         |
               transfer -> filters  (FilterSet, FilterChain)

filters -> globset, logging  (no dependency on protocol)
```

Adding `protocol` as a dependency of `filters` would create a circular
dependency (`protocol -> filters` is already needed for
`FilterRuleWireFormat`). Even without circularity, coupling a generic
pattern-matching library to the flist wire format would violate single
responsibility. The resolve-at-the-boundary pattern keeps the
architecture clean.

### Semver impact

No public API changes in any crate. The migration is a workspace-internal
refactor touching two call sites in private modules of the `transfer`
crate. No semver implications.

## Testing strategy

### Requirement: behavioral equivalence

The migration must not change any filter matching outcomes. Every path
that was included/excluded/protected before the migration must produce
the same result after.

### Approach 1: existing test coverage (primary gate)

The existing test suites cover the filter matching pipeline end-to-end:

- **Unit tests in `filters/src/tests.rs`** (25+ tests): anchored, unanchored,
  directory-only, wildcard, recursive, CVS, AppleDouble, perishable,
  negation, protect/risk, sender/receiver side, clear rules.
- **Unit tests in `filters/src/compiled/rule.rs`** (10+ tests): pattern
  matching specifics - anchored, directory-only, descendant, negated,
  complex globs.
- **Unit tests in `filters/src/chain/tests.rs`** (14+ tests): per-directory
  scoping, push/pop semantics, innermost-first evaluation, merge file
  reading.
- **Property tests in `filters/src/tests.rs`**: random pattern generation
  with proptest verifying compilation never panics and evaluation is
  deterministic.
- **Fuzz target `fuzz/fuzz_targets/fuzz_filter_chain.rs`**: feeds arbitrary
  rule sequences and paths through the full evaluation chain.
- **Interop tests in `transfer/src/receiver/tests/`**: daemon filter
  rules parsed from wire format and applied to file entries.
- **Golden tests in `protocol/tests/golden/`**: wire format round-trip
  for filter rules.

All these tests continue to pass unchanged because the `filters` crate
API is unchanged.

### Approach 2: targeted migration tests

Add tests at the changed call sites (sites 3 and 4) that verify the
resolved `&Path` from `PathHandle` produces the same filter result as
the pre-migration `Path::new(name)` path:

```rust
#[test]
fn daemon_filter_with_resolved_pathhandle_matches_literal() {
    let rules = [FilterRule::exclude("*.tmp")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Simulate PathHandle resolution
    let arena = PathArena::new();
    let handle = arena.intern("test.tmp");
    arena.freeze();
    let resolved: &Path = arena.resolve_path(handle);

    // Verify: resolved path matches same as literal path
    assert_eq!(
        set.allows(resolved, false),
        set.allows(Path::new("test.tmp"), false),
    );
}
```

Test cases should cover:

1. Simple filename (`test.tmp` vs `test.txt`)
2. Nested path (`dir/subdir/file.bak`)
3. Anchored rule (`/root_only`)
4. Directory-only rule (`cache/`) with `is_dir = true` and `false`
5. Protect rule preventing deletion
6. Unicode path components
7. Paths with multiple extension dots (`archive.tar.gz`)

### Approach 3: property test - PathHandle resolution equivalence

A property test that generates random path strings, interns them in a
`PathArena`, resolves back to `&Path`, and verifies filter evaluation
produces identical results:

```rust
proptest! {
    #[test]
    fn pathhandle_resolution_preserves_filter_result(
        pattern in "[a-z*?]{1,10}",
        path_str in "[a-z0-9/.]{1,30}",
        is_dir in proptest::bool::ANY,
    ) {
        let rules = vec![FilterRule::exclude(pattern.clone())];
        if let Ok(set) = FilterSet::from_rules(rules) {
            let direct = set.allows(Path::new(&path_str), is_dir);

            let mut arena = PathArena::new();
            let handle = arena.intern(&path_str);
            arena.freeze();
            let resolved = arena.resolve_path(handle);

            let via_handle = set.allows(resolved, is_dir);
            prop_assert_eq!(direct, via_handle);
        }
    }
}
```

This test validates the critical invariant: `PathArena::resolve_path()`
returns a `&Path` that is byte-equivalent to the original interned
string, and `GlobMatcher::is_match()` produces the same result on both.

### Approach 4: interop regression gate

The interop test suite (`tools/ci/run_interop.sh`) exercises transfers
with filter rules against upstream rsync 3.0.9, 3.1.3, 3.4.1, 3.4.2.
These tests exercise the full pipeline from CLI filter specification
through wire encoding, filter compilation, and application during
transfer. They serve as the end-to-end regression gate.

## Performance considerations

### Filter matching is a hot path

During file list enumeration, `FilterChain::allows()` is called once per
filesystem entry (files, directories, symlinks, devices). For a
million-file transfer, this means ~1M evaluations. Each evaluation
iterates the compiled rule list and calls `GlobMatcher::is_match()` on
each rule until the first match.

### PathHandle resolution cost: effectively zero overhead

The `PathArena::resolve_path()` operation is a single indexed array
access into `RodeoReader`'s internal `Vec<&str>`, then `Path::new()` on
the result. On Unix, `Path::new(&str)` is a zero-cost reinterpretation
(no allocation, no copy). The total resolve cost is ~1 ns per call
(single L1 cache hit for the array lookup).

The pre-migration code path was:

```
FileEntry.name: PathBuf  ->  .name() returns &str
                          ->  Path::new(name) constructs &Path
                          ->  FilterSet::allows(&Path)
                          ->  GlobMatcher::is_match(&Path)
```

The post-migration code path is:

```
FileEntry.name: PathHandle  ->  arena.resolve_path(handle) returns &Path
                             ->  FilterSet::allows(&Path)
                             ->  GlobMatcher::is_match(&Path)
```

Both paths produce a `&Path` that is passed to `GlobMatcher::is_match()`.
The difference is how the `&Path` is obtained:

- **Before:** deref through `PathBuf`'s internal `OsString` -> `Vec<u8>` ->
  `&[u8]` -> `&OsStr` -> `&Path`. This is a pointer chase through
  `PathBuf`'s heap allocation.
- **After:** indexed array access into `RodeoReader`'s contiguous
  `Vec<&str>`, then `Path::new()` (zero-cost on Unix). The resolved
  strings are packed in arena memory with better cache locality than
  scattered `PathBuf` heap allocations.

**Net effect:** the post-migration path is marginally faster due to
improved cache locality (arena strings are contiguous) versus the
pre-migration path (per-entry `PathBuf` heap allocations scattered
across the allocator's address space). The difference is negligible
in practice - well under 1% of filter evaluation time, which is
dominated by glob pattern matching.

### No additional allocations

`PathArena::resolve_path()` returns a borrowed `&Path` - no allocation.
`FilterSet::allows()` passes this `&Path` to `GlobMatcher::is_match()`
by reference - no allocation. The entire filter evaluation path from
`PathHandle` to match result is allocation-free.

### Bulk resolve amortization

For the two changed call sites (candidates filtering and directory
creation), the `arena` reference is obtained once and reused across
all entries in the batch. There is no per-entry overhead for acquiring
the arena - it is a shared `&PathArena` borrowed from the `FileList`.

### GlobMatcher internal cost is unchanged

`GlobMatcher::is_match()` (from the `globset` crate) accepts
`AsRef<Path>`. It internally converts to bytes and runs a DFA-based
matcher. This cost is identical regardless of whether the `&Path` came
from a `PathBuf` deref or a `PathArena` resolve. The glob matching
dominates filter evaluation time (~100-500 ns per rule evaluation
depending on pattern complexity), making the ~1 ns resolve cost
invisible.

## Implementation sequence

1. **Thread `&PathArena` into receiver candidate filtering** (site 3).
   Change `e.name()` to `e.name(&arena)` and `Path::new(name)` to
   `e.path(&arena)` in `candidates.rs`. Verify the `arena` is accessible
   from the enclosing context.

2. **Thread `&PathArena` into receiver directory creation** (site 4).
   Same pattern in `creation.rs`.

3. **Add targeted unit tests** at both sites verifying resolved paths
   produce correct filter results.

4. **Add property test** in `transfer` crate verifying PathHandle
   resolve + filter equivalence.

5. **Run full CI** (fmt, clippy, nextest, all platforms, interop).

The implementation is estimated at 10-20 lines of code changes total
(two call sites, each changing 1-2 lines). The bulk of the work is
threading the `&PathArena` reference through the receiver context, which
was already done as part of RSS-8.b's read-path migration.

## Cross-references

- `crates/filters/src/set.rs:143-175` - FilterSet::allows/allows_deletion
- `crates/filters/src/chain/mod.rs:126-149` - FilterChain::allows/allows_deletion
- `crates/filters/src/decision.rs:27-128` - first_matching_rule evaluation
- `crates/filters/src/compiled/rule.rs:38-76` - CompiledRule::matches (glob hot path)
- `crates/transfer/src/receiver/transfer/candidates.rs:87-93` - site 3
- `crates/transfer/src/receiver/directory/creation.rs:56-61` - site 4
- `crates/protocol/src/flist/entry/arena.rs` - FilePath/ArenaFileEntry prototype
- `crates/protocol/src/flist/intern.rs` - PathInterner (current)
- `docs/design/rss-8a-arena-handle-type.md` - PathHandle type spec
- `docs/design/rss-8b-fileentry-read-path-migration.md` - read-path migration
- `docs/design/rss-8c-fileentry-write-path-migration.md` - write-path migration
