# Evaluating the Repository pattern for the file list

Task: [#2135](https://github.com/oferchen/oc-rsync/issues/2135).
Branch: `docs/repository-pattern-flist-2135`.

## Scope

The project's design-pattern guidance lists the Repository pattern
as an accepted shape, but the codebase does not apply it to the file
list. The flist
is exposed today as a plain `Vec<FileEntry>` owned by the
generator/receiver and shared into the pipeline as
`Arc<Vec<FileEntry>>`. This document asks one question: does wrapping
that storage in a `FileListRepository` trait with queryable accessors
(`find_by_path`, `iter_by_dir`, `sort_by_inode`, etc.) earn its keep
against the cost of indirection and API churn.

This is an evaluation, not a plan. The longer adopt-and-stage analysis
lives in
[`docs/design/file-list-repository-pattern.md`](file-list-repository-pattern.md);
this note revisits the call after the
[#4166](https://github.com/oferchen/oc-rsync/issues/4166) arena audit
landed and reaches a different conclusion.

## 1. Current flist surface

### 1.1 Owner sites

| Site | Type | Ownership |
|---|---|---|
| `crates/protocol/src/flist/segment.rs:31` | `entries: Vec<FileEntry>` | one segment in INC_RECURSE wire layout |
| `crates/protocol/src/flist/incremental/mod.rs:82` | `ready: VecDeque<FileEntry>` | per-parent staging buffer |
| `crates/protocol/src/flist/incremental/mod.rs:85` | `pending: HashMap<String, Vec<FileEntry>>` | per-parent pending children |
| `crates/protocol/src/flist/incremental/mod.rs:489` | `resolved_entries: Vec<FileEntry>` | finalised builder output |
| `crates/transfer/src/generator/mod.rs:528` | `file_list: Vec<FileEntry>` | generator-owned working flist |
| `crates/transfer/src/receiver/mod.rs:146` | `file_list: Vec<FileEntry>` | receiver-owned working flist |
| `crates/transfer/src/pipeline/job.rs:39` | `entries: Arc<Vec<FileEntry>>` | frozen shared flist for the dispatch pipeline |
| `crates/transfer/src/pipeline/job.rs:109` | `entry: Arc<FileEntry>` | per-job entry handle |

The pipeline `FileList` wrapper at
`crates/transfer/src/pipeline/job.rs:38-82` is the only existing
abstraction. It exposes `new`, `get`, `len`, `is_empty`, `entries`,
`shared` and nothing else. Every other consumer goes straight to the
`Vec` via field access.

### 1.2 Distinct query patterns at consumers

Grep tally of `self.file_list.<method>` and direct indexing across
`crates/transfer/src` and `crates/engine/src`. Nine patterns survive
de-duplication; counts include both crates.

| # | Pattern | Sites | Representative location |
|---|---|---:|---|
| 1 | NDX lookup (`[ndx]`, `get(ndx)`) | 11 | `crates/transfer/src/generator/protocol_io.rs:174`, `crates/transfer/src/receiver/transfer/candidates.rs:155` |
| 2 | Contiguous range (`[start..end]`) | 5 | `crates/transfer/src/receiver/file_list.rs:53,185,204,205,209` |
| 3 | Full-list `iter()` | 9 | `crates/transfer/src/generator/file_list/hardlinks.rs:96`, `crates/transfer/src/generator/transfer.rs:706`, `crates/transfer/src/receiver/transfer/candidates.rs:52`, `crates/transfer/src/receiver/directory/links.rs:185` |
| 4 | `len()` / `is_empty()` | 23 | `crates/transfer/src/generator/transfer.rs:115,291`, `crates/transfer/src/generator/mod.rs:743` |
| 5 | Per-entry mutation (`[i].set_*`) | 6 | `crates/transfer/src/generator/file_list/hardlinks.rs:60,64,70,71` |
| 6 | Build-phase `push` / `reserve` / `clear` | 6 | `crates/transfer/src/generator/file_list/mod.rs:61,153`, `crates/transfer/src/generator/mod.rs:760,768` |
| 7 | `iter_mut()` for ID mapping rewrite | 1 | `crates/transfer/src/receiver/file_list.rs:87` |
| 8 | `retain()` for sanitisation | 1 | `crates/transfer/src/receiver/file_list.rs:364` |
| 9 | `clone()` to lift into `Arc` | 1 | `crates/transfer/src/receiver/transfer/pipeline.rs:125` |

Two named queries that the task description asks about specifically:

- `find_by_path` is **not** a current query pattern. Nothing in the
  codebase searches the flist by path; NDX is the only identifier
  threaded through the wire protocol. Hardlink lookup goes through
  the side `HardlinkTable` at
  `crates/protocol/src/flist/hardlink/table.rs`, not a path scan.
- `sort_by_inode` is **not** a current query pattern either. Sort is a
  one-shot operation on `(dirname, basename)` via
  `crates/protocol/src/flist/sort.rs:316` and
  `crates/protocol/src/flist/sort.rs:396`, run once during build.
  Re-sorting by inode would break the wire NDX invariant.

`iter_by_dir` is approximated by pattern #2 (contiguous range), which
works only because sort already groups entries by parent. There is no
explicit dir->range index today; the receiver knows the segment start
because it just wrote it (`flat_start = self.file_list.len()` at
`crates/transfer/src/receiver/file_list.rs:170`).

### 1.3 What this tells us

The flist is accessed almost exclusively as (a) by-NDX random access
and (b) full or contiguous-range iteration. Of the nine patterns,
seven are direct `Vec` operations with `O(1)` cost on the existing
storage. The remaining two (`retain`, `clone`) are one-shot.

There are no path-keyed lookups, no inode-keyed lookups, no predicate
indices, and no scenarios where a consumer needs to learn the storage
strategy. The pipeline's `FileList::shared() -> Arc<Vec<FileEntry>>`
at `crates/transfer/src/pipeline/job.rs:79` already provides the only
abstraction the call sites have asked for: hand me a reference-counted
read view.

## 2. Repository sketch

A minimal trait that captures patterns #1-#4 (the ones consumers
share). #5-#9 are construction-phase concerns that stay on a separate
builder type.

```rust
/// Read-side abstraction over a sorted, frozen file list.
pub trait FileListRepository: Send + Sync {
    /// Pattern #1. Replaces `self.file_list.get(ndx)` and `[ndx]`.
    fn get(&self, ndx: u32) -> Option<&FileEntry>;

    /// Pattern #2. Replaces `&self.file_list[start..end]`.
    fn range(&self, start: u32, end: u32) -> &[FileEntry];

    /// Pattern #3. Replaces `self.file_list.iter()`.
    fn iter(&self) -> std::slice::Iter<'_, FileEntry>;

    /// Pattern #4. Replaces `len()` / `is_empty()`.
    fn len(&self) -> u32;
    fn is_empty(&self) -> bool { self.len() == 0 }
}
```

Two named queries the task asks about, layered on top:

```rust
impl dyn FileListRepository + '_ {
    /// Linear scan; no path index exists today and adding one costs
    /// ~16 B per entry for a `BTreeMap<PathBuf, u32>` lookup table.
    fn find_by_path(&self, path: &Path) -> Option<u32> {
        self.iter().position(|e| e.path() == path).map(|i| i as u32)
    }

    /// Linear scan with parent change detection. Works only because
    /// the underlying Vec is already sorted by (dirname, basename);
    /// makes the implicit invariant explicit.
    fn iter_by_dir<'a>(&'a self, dir: &Path)
        -> impl Iterator<Item = (u32, &'a FileEntry)> + 'a
    { /* scan range from sort-index of `dir` */ }
}
```

The asked-for `sort_by_inode` is omitted: re-sorting after wire
exchange would shift NDX values and break the receiver's index-based
ack protocol (`crates/transfer/src/pipeline/job.rs:6-13` documents the
NDX-stability invariant). If inode-ordered iteration is ever needed
(e.g. for receiver-side readahead heuristics), the right answer is a
side `Vec<u32>` of NDX values pre-sorted by inode, not a repository
mutation.

## 3. Pros

1. **Mockable in tests.** A `MockFileListRepository` could be passed
   into `GeneratorContext` and `ReceiverContext` instead of stuffing a
   real `Vec<FileEntry>` into the field. The generator tests at
   `crates/transfer/src/generator/tests.rs:197-1736` and receiver
   tests at `crates/transfer/src/receiver/tests.rs:2222-3071` push
   entries directly today; a trait seam would let unit tests target
   specific access patterns without constructing valid wire entries.
2. **Storage swap.** The
   [#4166](https://github.com/oferchen/oc-rsync/issues/4166) audit
   enumerates an arena-backed `FileEntry` layout that cannot be a
   drop-in `Vec<FileEntry>` because of lifetime parameters. A
   repository trait would absorb that lifetime behind the trait object
   and let arena/mmap backings ship without touching consumers.
3. **Telemetry hook.** A decorator
   `TracingRepository<R: FileListRepository>` could log every `get`
   call (NDX, latency) for protocol debugging. Today this requires
   either editing every call site or wrapping the field in a smart
   pointer with logging in `Deref`, both of which are intrusive.
4. **Invariant centralisation.** The "post-sort NDX values are stable"
   contract is currently asserted only by a doc comment at
   `crates/transfer/src/pipeline/job.rs:32-36`. A `FileListRepository`
   that takes a pre-sorted source and exposes `Frozen` semantics could
   make the invariant a type-system property (no `&mut` accessors
   after freeze).

## 4. Cons

1. **Indirection cost on hot iteration.** `Vec::iter()` is a pointer
   bump; `Box<dyn Iterator<Item = &FileEntry>>` allocates per call and
   defeats the inliner. The transfer loop at
   `crates/transfer/src/receiver/transfer/candidates.rs:52` iterates
   the whole flist once per dispatch round. At 1 M entries the vtable
   indirection adds 2-3 ns/entry (~3 ms total per round); the heap
   allocation for the iterator object dominates only if the loop runs
   to completion every time. Returning concrete `slice::Iter` (as in
   the sketch above) avoids the allocation but locks the trait to
   `Vec` semantics, undermining pro #2.
2. **API explosion for ad-hoc queries.** Each new caller wants a
   slightly different cut: "entries with size > N", "entries marked
   as directories", "entries whose path starts with X". Today these
   are one-line iterator chains; under a repository they become trait
   methods or force callers to use `iter()` anyway, regressing to
   pattern #3.
3. **Migration blast radius.** The companion document quantifies the
   cost: 305 `Vec<FileEntry>` hits across `crates/protocol`,
   `crates/transfer`, and tests. Five PRs minimum, ~25 minutes of
   interop CI per PR. The arena audit
   ([#4166](https://github.com/oferchen/oc-rsync/issues/4166))
   recommends pre-sizing and path collapse first; doing repository
   migration before those land does the work twice.
4. **`Arc<FileEntry>` per job stays.** The job dispatch pipeline at
   `crates/transfer/src/pipeline/job.rs:109` holds an
   `Arc<FileEntry>` per `FileJob`. The repository wraps the
   container; it does not remove the per-job refcount. The pipeline's
   memory shape is dominated by `FileJob` allocations, not the flist
   container. A repository abstraction without removing
   `Arc<FileEntry>` per job leaves the heaviest cost untouched.
5. **Trait-object lifetime gymnastics.** `Arc<dyn FileListRepository>`
   needs `'static`. Any backing that wants to borrow from a wire-side
   arena (the win in pro #2) must either own the arena via `Arc` or
   give up the trait-object form. The receiver's pipeline expects
   `Arc<Vec<FileEntry>>` to be `'static`; an arena-backed repository
   needs the same property, which kills the borrow-from-arena
   optimisation that justified the abstraction in the first place.

## 5. Recommendation

**Reject for now. Reopen after #4166 follow-ups land.**

The current `Vec<FileEntry>` plus `Arc<Vec<FileEntry>>` is the right
shape for the nine query patterns counted in section 1.2:

- Patterns #1-#4 (NDX lookup, range, full iter, len) are direct `Vec`
  primitives at `O(1)` per call. A trait wrapper adds vtable cost
  without changing any algorithmic complexity.
- Patterns #5-#7 (per-entry mutation, build-phase push, ID rewrite)
  are construction-time operations. The receiver's `file_list.rs:42`
  ingest path needs `&mut Vec<FileEntry>` to slice into the latest
  segment; that is fundamentally an exclusive-reference operation
  that no trait surface improves.
- Patterns #8-#9 (retain, clone) are one-shot and already
  encapsulated.

The two named "queryable repository" methods the task asks about
(`find_by_path`, `sort_by_inode`) do not match any existing access
pattern. `find_by_path` would be a linear scan added because the
trait exists, not because a caller needs it. `sort_by_inode` actively
violates the NDX-stability invariant.

The strongest pro (storage swap for the arena backing in #4166) does
not survive the trait-object lifetime constraint in con #5. The arena
needs to own its own bytes (`Arc<Bytes>` or equivalent) to live in an
`Arc<dyn FileListRepository>`, at which point the arena win shrinks
to the entry-struct savings and the path savings disappear. The
[#4166](https://github.com/oferchen/oc-rsync/issues/4166) audit
already concluded that pre-sizing
(`crates/protocol/src/flist/segment.rs:37`,
`crates/transfer/src/receiver/file_list.rs:643-659`) and path
collapse should land first. Both of those are local changes that need
no trait abstraction.

The mockability pro (#3.1) is real but cheap to solve a different
way: extract a `&[FileEntry]` accessor on `GeneratorContext` and
`ReceiverContext` (the latter already exists at
`crates/transfer/src/receiver/mod.rs:379` as
`pub fn file_list(&self) -> &[FileEntry]`; the generator has the
matching `crates/transfer/src/generator/mod.rs:741`) and let tests
construct a `Vec<FileEntry>` directly. That is the status quo.

If the arena work lands and the per-entry savings still motivate
hiding the layout, the right shape is **not** a queryable repository
with `find_by_path`/`sort_by_inode`. The right shape is a narrower
sealed trait covering only patterns #1-#4 (the four `Vec`
primitives), without ad-hoc query methods. That is what
[`docs/design/file-list-repository-pattern.md`](file-list-repository-pattern.md)
already sketches, gated on the #4166 prerequisites.

In short: the codebase needs a leaner `FileEntry` more than it needs
a richer flist abstraction. Adopt this pattern after the arena
prerequisites land; until then a `Vec<FileEntry>` with direct
iteration is faster than any repository.

## 6. Cross-references

- [#4166](https://github.com/oferchen/oc-rsync/issues/4166) -
  `FileEntry` arena allocator prototype audit. Recommends pre-sizing
  and path collapse before arena work, the same prerequisites this
  doc cites for repository adoption. Audit at
  `docs/audits/flist-arena-prototype.md`.
- [#4173](https://github.com/oferchen/oc-rsync/issues/4173) -
  `WorkQueueSender` multi-producer usage audit. Same lesson at a
  different layer: a multi-producer trait wrapper around the existing
  SPSC channel did not pay for itself. Audit at
  `docs/audits/workqueue-sender-multi-producer-audit.md`.
- [#2210](https://github.com/oferchen/oc-rsync/issues/2210) - arena
  prototype task (closed as duplicate per #4166's recommendation).
- [#2133](https://github.com/oferchen/oc-rsync/issues/2133),
  [#2134](https://github.com/oferchen/oc-rsync/issues/2134),
  [#2136](https://github.com/oferchen/oc-rsync/issues/2136) -
  companion pattern-evaluation tasks. The pattern catalog at
  `docs/design/pattern-usage-catalog.md` is the running ledger.
- [`docs/design/file-list-repository-pattern.md`](file-list-repository-pattern.md)
  - longer adopt-and-stage analysis (proposes a four-PR ladder
  conditional on #4166's pre-size/path-collapse landing first). This
  document differs by recommending defer-until-prerequisites rather
  than start-now.
- Upstream reference:
  `target/interop/upstream-src/rsync-3.4.1/flist.c`,
  `target/interop/upstream-src/rsync-3.4.1/lib/pool_alloc.c`. Upstream
  has no equivalent abstraction; the pool allocator is the entire
  story.
