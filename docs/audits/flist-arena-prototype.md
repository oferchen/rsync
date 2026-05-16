# FileEntry arena allocator prototype (#2210)

Companion audit to `vec-fileentry-vs-pool.md` (#1050) and
`rss-flist-vec-vs-pool.md` (#1050). The earlier audits enumerated the
upstream pool design and the candidate crates; this report walks the
current source tree to confirm that a typed-arena swap is still blocked
and lists the exact code sites that would have to move before any
arena work can land.

Scope: prototype only. No production code change. The memory baseline
that motivated the task is recorded in
`docs/benchmarks/flist-memory-baseline-2026-05-01.md` (84 MiB oc-rsync
vs 7.5 MiB upstream at 100 K entries, ~11x overhead). The aim was to
swap `Vec<FileEntry>` for `bumpalo::Bump` in at least one owner site
and bench the win.

## TL;DR

A bumpalo `Bump` arena swap is still not viable as a single-PR change.
Every owner site exposes `FileEntry` by value through APIs that move,
swap, sort, drain, or `Arc::new` the entries. The arena variant must
return borrowed references (`&'bump FileEntry`) and would require a
lifetime parameter on `FileEntry`. That parameter would propagate to
`engine`, `transfer`, `cli`, and three integration test crates.

Additionally, `FileEntryExtras` owns `Vec<u8>`, `String`, `PathBuf`,
and `XattrList` values. bumpalo skips `Drop`, so any such entry placed
in the arena leaks. Replacing those owned types with `&'bump [u8]`
slices is its own multi-PR refactor that intersects with the
xattr/ACL caches (`crates/protocol/src/flist/read/mod.rs:120-137`).

The recommended next step from the prior audits stands: stack
pre-sizing (#1 in `rss-flist-vec-vs-pool.md`) and string collapse
(#5 there) before any arena work.

## Current owner sites that block arena ownership

The following sites either move `FileEntry` by value or wrap it in
`Arc`, both incompatible with `&'bump FileEntry`. Counts are direct
grep hits in the current tree.

| Site | Operation | Blocker |
|---|---|---|
| `crates/protocol/src/flist/segment.rs:31` | `entries: Vec<FileEntry>` | owns by value; `flatten()` returns a fresh `Vec<FileEntry>` |
| `crates/protocol/src/flist/segment.rs:157` | `flatten()` returns `Vec<FileEntry>` | by-value copy across segments |
| `crates/protocol/src/flist/sort.rs:316` | `flist_clean(file_list: Vec<FileEntry>) -> Vec<FileEntry>` | takes ownership; uses `swap` + `truncate` |
| `crates/protocol/src/flist/sort.rs:396` | `sort_and_clean_file_list(file_list: Vec<FileEntry>, ...)` | same |
| `crates/protocol/src/flist/incremental/mod.rs:82` | `ready: VecDeque<FileEntry>` | `push_back`/`pop_front` by value |
| `crates/protocol/src/flist/incremental/mod.rs:85` | `pending: HashMap<String, Vec<FileEntry>>` | per-parent vector of owned entries |
| `crates/protocol/src/flist/incremental/mod.rs:239` | `drain_ready() -> Vec<FileEntry>` | drains owned entries into a fresh Vec |
| `crates/protocol/src/flist/incremental/mod.rs:259` | `finish() -> Vec<FileEntry>` | collects orphans by value |
| `crates/protocol/src/flist/incremental/mod.rs:299` | `finalize()` builds `resolved_entries: Vec<FileEntry>` | by-value transfer |
| `crates/protocol/src/flist/incremental/mod.rs:489` | `resolved_entries: Vec<FileEntry>` | API return type |
| `crates/protocol/src/flist/read/mod.rs:494` | `read_entry_with_flist(segment_entries: &[FileEntry])` | hardlink leader lookup expects a borrowed slice into already-decoded entries; arena slice would work but Vec<FileEntry> is built before this call |
| `crates/transfer/src/pipeline/job.rs:39` | `Arc<Vec<FileEntry>>` | requires `FileEntry: 'static` |
| `crates/transfer/src/pipeline/job.rs:109` | `Arc<FileEntry>` per job | per-file heap allocation; needs `'static` |
| `crates/transfer/src/generator/mod.rs:528` | `file_list: Vec<FileEntry>` | owned by generator state |
| `crates/transfer/src/receiver/mod.rs:146` | `file_list: Vec<FileEntry>` | owned by receiver state |
| `crates/transfer/src/receiver/file_list.rs:559` | `drain_ready() -> Vec<FileEntry>` | by-value drain across crate boundary |
| `crates/transfer/src/receiver/file_list.rs:643` | `collect_sorted() -> Vec<FileEntry>` | terminal collection |

Owner sites in tests (acl_compat_309, symlink_target_encoding,
device_file_encoding, proptest_file_entry_roundtrip, flist_stress_tests)
all use `Vec<FileEntry>` as well; the arena variant would need a parallel
test surface.

## Drop semantics blocker

bumpalo intentionally does not run `Drop` on arena-allocated values
(<https://docs.rs/bumpalo/3.20.2/bumpalo/#allocation-vs-drop>). The
current `FileEntry` and its `FileEntryExtras` own heap data that must
be released:

```text
FileEntry (crates/protocol/src/flist/entry/core.rs:32)
  name: PathBuf                   -- owns heap bytes
  dirname: Arc<Path>              -- refcount needs decrement
  extras: Option<Box<FileEntryExtras>>
                                  -- box needs free

FileEntryExtras (crates/protocol/src/flist/entry/extras.rs:14)
  link_target: Option<PathBuf>    -- owns heap bytes
  user_name: Option<String>       -- owns heap bytes
  group_name: Option<String>      -- owns heap bytes
  checksum: Option<Vec<u8>>       -- owns heap bytes
  xattr_list: Option<XattrList>   -- owns nested heap data
```

A bumpalo-resident `FileEntry` leaks every one of those fields on
arena reset. Mitigations:

- Replace each owned field with `&'bump [u8]` / `&'bump Path` slices
  copied into the same arena. This is the upstream pattern
  (`flist.c:1018-1027` packs basename, dirname, and linkname into one
  `pool_alloc` chunk). It is also the only way to land the full win.
- Keep the owned fields outside the arena (`Vec<u8>`, `String` on the
  global heap). This defeats most of the arena win because
  allocator-metadata overhead, the second-largest contributor at
  100 K entries per `rss-flist-vec-vs-pool.md`, comes from these
  per-field allocations.

Either mitigation forces a lifetime parameter on `FileEntry` and a
matching API churn across the consumers listed above.

## Arc<FileEntry> in the job pipeline

`crates/transfer/src/pipeline/job.rs:109` stores `entry: Arc<FileEntry>`
per `FileJob` and ships jobs through a bounded tokio mpsc channel. Arc
requires `T: 'static + Send + Sync`. An arena-borrowed
`&'bump FileEntry` cannot be wrapped in `Arc` and cannot cross the
channel without scoped threads or a self-referential pipeline.
Removing `Arc` here would either require:

- Sending the entry by value through the channel (copies a 88 B
  struct per job, currently amortised by the `Arc` clone), or
- Sending only the NDX and looking up the entry on the consumer
  side via the arena reference. That arena reference still needs
  `'static` to live in a channel item, which loops back to the
  lifetime problem.

## Minimal narrow POC attempt: `IncrementalFileList::pending`

The cheapest theoretical pilot would be the per-parent pending
buffer in `crates/protocol/src/flist/incremental/mod.rs:85`. It is
short-lived (entries are evicted on parent creation), owns no
external references, and never crosses an `Arc` boundary.

Even there the arena does not help:

1. `push(entry: FileEntry)` already takes the entry by value, so the
   arena would receive a value that already owns its heap. Moving
   it into the arena means an arena-allocated `FileEntry` plus the
   original `PathBuf`/`Box<FileEntryExtras>` heap chunks still alive.
2. `release_pending_children` drains the per-parent vector into
   `ready: VecDeque<FileEntry>`, which is itself a by-value buffer.
   To preserve type identity the ready buffer would also have to
   become `VecDeque<&'bump FileEntry>`, and that propagates out
   through `pop() -> Option<FileEntry>` (line 197), the entire
   `IncrementalFileListIter` surface, and `process_ready_entry()`.

The pending buffer wins nothing in isolation; the win only materialises
when the entire `FileEntry` heap (name, dirname bytes, extras) is
co-located in the arena. That is the full conversion, not a narrow POC.

## Bench was not run

A Criterion bench (`crates/protocol/benches/flist_arena.rs`) requires
either a buildable arena variant of `FileEntry` or a synthetic
"`Vec<&FileEntry>` over a pre-allocated arena" microbench that does
not exercise the real heap (and therefore does not measure the real
win). The existing `crates/protocol/benches/file_entry_memory.rs`
already records the Vec<FileEntry> baseline (100 K entries, peak RSS
via `/proc/self/status` on Linux); a meaningful arena comparison
would need a working POC running the same workload.

No bench was added in this audit because the POC is not buildable
within the constraints (Drop semantics, lifetime propagation,
`Arc<FileEntry>` in the pipeline). Building one would require the
multi-PR refactor described in the next section.

## Blockers to full arena conversion

In dependency order:

1. **`FileEntryExtras` field types.** Replace `PathBuf`, `String`,
   `Vec<u8>`, `XattrList` with `&'bump [u8]` slices carved from the
   same arena. Touches
   `crates/protocol/src/flist/entry/extras.rs:14-56` and every
   accessor in `crates/protocol/src/flist/entry/accessors.rs`.
2. **`FileEntry` lifetime parameter.** Add `'a` to `FileEntry`,
   propagate through `FileEntryBuilder` (when added), `FileType`,
   `FileFlags`. Touches every public re-export in
   `crates/protocol/src/flist/mod.rs:49-77` and every consumer that
   names the type.
3. **`sort_file_list` / `flist_clean` semantics.** The current
   in-place `swap`/`truncate` pipeline cannot operate on
   `&'bump FileEntry` because swapping references would corrupt
   the arena layout. Switch sort+clean to operate on a
   `Vec<&'bump FileEntry>` (sorts pointers, not 88 B values; also
   faster). Touches `crates/protocol/src/flist/sort.rs:316-400`.
4. **`Arc<FileEntry>` in the job pipeline.** Either send entries by
   value through the channel (copy cost) or restructure the consumer
   to look up by NDX into a `'static`-lived arena owned by the
   receiver task. Touches `crates/transfer/src/pipeline/job.rs:39,109`
   and the entire generator/receiver pair.
5. **Hardlink follower lookup.** `read_entry_with_flist` takes
   `segment_entries: &[FileEntry]`
   (`crates/protocol/src/flist/read/mod.rs:494`); the slice would
   become `&[&'bump FileEntry]`. Touches the leader-resolution path
   in `crates/protocol/src/flist/hardlink/table.rs`.
6. **Cancellation paths.** Signal handlers and `RawSyncReceiver`
   cancel paths currently rely on `Drop` to release transient flist
   memory. An arena-only flist needs explicit `Bump::reset` wired
   into the cancellation tree to avoid leaks on SIGINT/abort.
7. **Test surface.** Three integration test files build
   `Vec<FileEntry>` directly
   (`crates/protocol/tests/proptest_file_entry_roundtrip.rs`,
   `flist_stress_tests.rs`, `acl_compat_309.rs`); each needs an
   arena-aware variant.

Aggregate estimate: 5-8 PRs, ~1500-2500 LOC across protocol, transfer,
engine, cli, and tests. The smallest safe ordering is (1) +
`FileEntryBuilder` first (introduces the builder without changing
storage), (3) sort/clean on pointers, then (2) lifetime parameter
landed behind a feature flag, then (4) and (5).

## Recommendation

Close #2210 as duplicate of the next-step recommendation in
`rss-flist-vec-vs-pool.md`: do #1 (pre-size `Vec<FileEntry>` from the
sender's getdents count) and #5 (collapse `name`+`dirname` into a
single `Box<[u8]>` with a `u16` basename offset) first. Those land
~40-50 MiB of the gap at 1 M entries with no API churn and no
lifetime parameter. The arena work, when it does land, then has a
smaller absolute win and a clearer benchmark target.

Specifically:

- Pre-size vectors: track as a new task; targets
  `crates/protocol/src/flist/segment.rs:37`,
  `crates/protocol/src/flist/incremental/mod.rs:111-119`,
  `crates/transfer/src/receiver/file_list.rs:643-659`.
- Path-field collapse: track as a new task; targets
  `crates/protocol/src/flist/entry/core.rs:35-42`. Companion to
  `pathbuf-arc-path-rss-overhead.md` (#1048).
- Arena: re-open after the above land. The new bench will measure
  the marginal arena win against a leaner baseline.

## References

- `docs/audits/vec-fileentry-vs-pool.md` (#1050) - upstream slab
  design walkthrough and crate survey.
- `docs/audits/rss-flist-vec-vs-pool.md` (#1050) - empirical RSS
  breakdown and five-path proposal.
- `docs/audits/pathbuf-arc-path-rss-overhead.md` (#1048) - path heap
  audit; intersects with arena work on the extras side.
- `docs/audits/incremental-flist-memory-bench.md` - incremental
  flist memory bench notes.
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md` -
  empirical 84 MiB vs 7.5 MiB baseline at 100 K entries.
- Upstream: `target/interop/upstream-src/rsync-3.4.1/lib/pool_alloc.c`,
  `flist.c:1018-1025`, `flist.c:2907-2935`,
  `rsync.h:786-937`.
