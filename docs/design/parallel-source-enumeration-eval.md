# Evaluate Parallel Source Enumeration via Multi-Producer WorkQueue

Tracking: oc-rsync task #1573. Focused evaluation of one hypothesis: would
fanning the sender's source enumeration across multiple producer threads
pushing into the shared `WorkQueue` reduce the receiver's first-byte
latency at 100K+ source files?

This note answers the question, engages with the #4173 single-producer
conclusion that is already on the books, and lands a recommendation.

## 1. Cross-references

- **#4173** - `WorkQueueSender` usage audit. Concluded that every
  production `WorkQueueSender` call site is correctly single-producer
  and that the `multi-producer` cargo feature flag has no current use
  case. Captured in `crates/engine/src/concurrent_delta/multi_producer_audit.rs:36-95`.
- **#4196** - `spawn_blocking` design for the async daemon. Establishes
  how rayon work crosses the tokio runtime boundary. Source enumeration
  is one of the rayon dispatches it covers.
- **#4203** - Sync channel benchmarking under multi-producer load.
- **#2196** - INC_RECURSE I1 instrumentation. Already merged. Records
  the elapsed time from `send_file_list` entry to the first wire byte
  via `FirstByteWriter` in
  `crates/transfer/src/generator/protocol_io.rs:517-550`.
- Earlier scoping at `docs/design/parallel-source-enumeration.md` (#1573)
  sketched a multi-root prototype. This eval is a sharper, more
  opinionated revisit informed by the I1 evidence and the #4173 audit.

## 2. Current sequential enumeration

The sender enumerates the source tree on a single thread inside
`build_file_list`, then sorts, then opens the wire to the receiver:

- `GeneratorContext::build_file_list`
  (`crates/transfer/src/generator/file_list/mod.rs:52`) iterates the
  caller-supplied `base_paths` and dispatches one `walk_path` per root
  from a single thread (`mod.rs:70-84`).
- `walk_path` (`crates/transfer/src/generator/file_list/walk.rs:34`)
  recurses sequentially and pushes entries into `self.file_list` and
  `self.full_paths` in arrival order. NDX is the implicit `Vec` index;
  index allocation stays monotonic because there is only one producer.
- Inside `walk_path`, directory children are batch-stat'd with rayon
  via `batch_stat_dir_entries`
  (`crates/transfer/src/generator/file_list/batch_stat.rs:38`). This is
  the only existing parallelism: `stat()` fan-out per directory, not
  enumeration fan-out across directories. The walk itself stays
  single-threaded.
- After the walk completes, `build_file_list` runs the indirect-permutation
  sort (`mod.rs:88-108`) and assigns hardlink indices (`mod.rs:110-114`)
  before returning.

Only then does the transfer loop hand the file list to
`send_file_list`:

```
crates/transfer/src/generator/transfer.rs:771-783
    // upstream: flist.c:2192 - send_file_list()
    if files_from_paths.is_empty() {
        self.build_file_list(paths)?;
    } else {
        self.build_file_list_with_base(&base_dir, &files_from_paths)?;
    }
    self.partition_file_list_for_inc_recurse();
    self.send_file_list(writer)?
```

`send_file_list` is in `protocol_io.rs:206`. It is the first place the
sender writes file list bytes to the wire.

The `WorkQueue` in `crates/engine/src/concurrent_delta/` is receiver-side
infrastructure. The audit in `multi_producer_audit.rs:36-95` shows it has
exactly three production users, all on the wire-read side, all
correctly single-producer.

## 3. Does enumeration latency actually delay first-byte?

This is the load-bearing question. The hypothesis assumes parallel
enumeration shortens the gap the receiver sees before any file list
bytes arrive. The I1 instrumentation (#2196) tells us precisely where
that gap lives.

### What I1 measures

`FirstByteWriter` is wired into the wire writer at
`protocol_io.rs:224-227`. It records elapsed time from `Instant::now()`
captured at `protocol_io.rs:209` (the entry of `send_file_list`) to the
first non-empty `write()` (`protocol_io.rs:541-549`). The result lands
in `self.timing.flist_first_byte_latency`
(`protocol_io.rs:257`) and is logged at `-vv` (flist1) and
`--info=stats3`.

**I1 starts AFTER `build_file_list` returns.** Enumeration latency is
not part of I1. It is upstream of I1, inside the
`PhaseTimer::new("file-list-build-send")` block at
`transfer.rs:772-783`, and is captured separately as
`flist_buildtime_ms` in `transfer.rs:799-803`.

### So which latency does the hypothesis target?

Two distinct windows exist:

1. **W1: process start -> first wire byte.** Includes
   `build_file_list` + `partition_file_list_for_inc_recurse` +
   `send_file_list` entry. Parallel enumeration would shrink W1 by
   shortening `build_file_list`.
2. **W2: `send_file_list` entry -> first wire byte (I1).** Independent
   of enumeration. Parallel enumeration would not change I1 at all
   unless we also restructured `send_file_list` to begin transmitting
   before enumeration completes.

The hypothesis in the task description ("let the receiver start
processing earlier and improve first-byte latency") is W1. I1 is the
right instrument to *exclude* enumeration as a confound, not to measure
its impact. To measure W1 we need a separate timestamp at sender
process start or at `core::session()` entry, compared against the
receiver-side timestamp of the first inbound flist byte.

### Is W1 the bottleneck at 100K files?

The evidence we have is indirect:

- `flist_buildtime` (sender wall time for `build_file_list`) is already
  recorded and shipped to the receiver in protocol >= 29
  (`transfer.rs:692-713`). On the `rsync-profile` container with cold
  caches over 1M synthetic files, the existing
  `batch_stat_dir_entries` rayon parallelism (`batch_stat.rs:38`)
  already collapses the per-directory `stat()` cost to near
  `getdents()`-bound time.
- The dominant remaining cost in a 100K+ file walk is the recursive
  `readdir` traversal itself, which is sequential per directory. With
  one root, parallelism within a directory does not help: `readdir` on
  one inode is intrinsically serial at the VFS layer.
- Multi-root parallelism (`oc-rsync src1/ src2/ src3/ dst/`) could in
  principle parallelise the *across*-root walks, but `build_file_list`
  loops over `base_paths` on a single thread (`mod.rs:70-84`). This is
  the only place parallel enumeration plausibly buys wall-clock time
  for the wire path today.

### So would multi-root parallel walks improve W1?

For pure multi-root sender invocations: yes, by approximately
`min(num_roots, available_io_parallelism)` in the best case. The
existing `docs/design/parallel-source-enumeration.md` Section 2
captures the scenario list.

But this benefits W1, not I1, and only when:
- The user passes multiple sibling roots, and
- The roots live on separate I/O devices or readdir is CPU-bound (NFS
  with high RTT, FUSE, slow union mounts), and
- The receiver's downstream pipeline is starved waiting for the file
  list, not the file list bytes themselves.

The third condition is the killer. Once `send_file_list` starts, the
receiver pulls bytes as fast as the wire allows. Pre-shortening W1 by
parallelising the walk only matters if the receiver is sitting idle
waiting for the first byte. At 100K files the typical receiver is far
from idle: it is opening basis files, checksumming, and queuing work
for the consumer pool the moment any segment arrives.

## 4. Multi-producer design sketch (and why it does not change the answer)

For completeness, the technically sound shape of the proposed fan-in:

### Routing

1. Fan out per-root walks onto `rayon::scope`. Each thread receives one
   `base_path` and walks it independently using the existing `walk_path`
   logic adapted to push into a thread-local `(Vec<FileEntry>, Vec<PathBuf>)`
   instead of `self.file_list` / `self.full_paths`.
2. After all threads join, the orchestrator concatenates the per-thread
   vectors and runs the existing indirect-permutation sort
   (`mod.rs:88-108`). Wire order is restored at the sort, so the walk
   phase does not need to preserve per-thread arrival order.
3. NDX values are assigned post-sort. This matches upstream
   `flist.c:f_name_cmp()` exactly and avoids the producer-id offset
   scheme the earlier sketch raised
   (`docs/design/parallel-source-enumeration.md` Section 4, mitigation
   1). The sort-merge mitigation (Section 4, mitigation 2) is the
   correct one.

### Cross-cutting state

- **Hardlink dev/inode tables.** Each producer keeps a local map. The
  orchestrator merges them before `assign_hardlink_indices` runs
  (`mod.rs:110-114`). Collisions across roots resolve to the lowest
  post-sort NDX, matching upstream `hlink.c:match_hard_links`.
- **`io_error` flag.** Each producer accumulates locally; the
  orchestrator OR-folds before `send_io_error_flag`
  (`transfer.rs:786`).
- **Filter chain.** `FilterChain` is `Sync` for read-only matching but
  `.rsync-filter` merge files mutate per-directory state. Each producer
  needs a cloned filter cursor, scoped to its root. Cheap if the chain
  is small, surprisingly expensive if `--filter merge` files are large.
- **INC_RECURSE.** `partition_file_list_for_inc_recurse`
  (`crates/transfer/src/generator/file_list/inc_recurse.rs:38`) runs
  after the sort. It is order-sensitive but consumes the sorted list,
  so parallel walk does not affect it as long as the merge produces the
  same sorted output. Confirmed by inspection of `classify_file_list_entries`
  (`inc_recurse.rs:61-95`) - it depends only on the entry sequence in
  the sorted `file_list`.

### Why this is not a `WorkQueue` use case

The proposed design does not actually push into the engine `WorkQueue`.
It pushes into thread-local `Vec`s and merges before the sort. The
`WorkQueue` (`crates/engine/src/concurrent_delta/work_queue/`) is a
bounded crossbeam channel that feeds the delta pipeline consumers on
the *receiver* side. It is structurally the wrong primitive for sender
enumeration:

- The sender does not have a consumer pool waiting for `DeltaWork`. It
  has `send_file_list` waiting for a sorted `Vec<FileEntry>`.
- `WorkQueueSender::Clone` (gated by the `multi-producer` cargo
  feature, `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17-23`)
  exists to support a future MPMC scenario. The #4173 audit
  (`multi_producer_audit.rs:8-95`) ruled that no production site needs
  it, and parallel source enumeration does not change that ruling: the
  fan-in target is a `Vec<FileEntry>` plus a global sort, not a
  bounded MPMC channel.

So the task's framing ("multi-producer WorkQueue") and the actually
useful design ("rayon scope, thread-local vecs, sort-merge") are
different mechanisms. The audit conclusion holds even if we ship the
parallel walk: `WorkQueueSender` stays single-producer.

## 5. Recommendation: defer

**Defer.** Do not prototype until we have W1 evidence that justifies
the engineering cost.

Rationale:

1. **I1 is the wrong instrument for the hypothesis, but it is the
   instrument we have.** The hypothesis targets W1 (process start to
   first byte). We need a W1 probe before any prototype is worth
   building. Without it the prototype's benefit is unmeasurable in CI.
2. **The audit conclusion (#4173) is unaffected.** The actually useful
   design does not touch the `WorkQueue`. Multi-producer enumeration
   uses thread-local vectors plus the existing sort. The audit's
   single-producer ruling for the `WorkQueue` stays correct. The doc
   title's framing is a red herring.
3. **Scope of benefit is narrow.** Parallel walk helps only the
   multi-root invocation pattern (`oc-rsync src1/ src2/ src3/ dst/`)
   when roots straddle independent I/O paths. The single-root case,
   which is the dominant pattern in production rsync usage, gets
   nothing.
4. **Engineering cost is real.** Per-root filter chain cloning,
   per-thread hardlink-table merging, INC_RECURSE serialisation, and
   `io_error` fan-in all need test coverage and interop validation
   against upstream 3.4.1. The wire output must be byte-identical to
   the single-producer path. None of this is free.
5. **The async daemon migration (#4196) does not require it.** The
   `spawn_blocking` bridge already crosses the runtime boundary
   correctly. Parallel enumeration is not on its critical path.

If the W1 benchmark in Section 6 produces a clear win (>15% wall-clock
reduction on multi-root 1M-file workloads without single-root
regression), revisit. Until then, the existing `batch_stat`
parallelism (`batch_stat.rs:38`) captures the cheap win and the
single-producer enumeration captures the simplicity.

## 6. Open question

**The one bench that would settle this:**

> On the `rsync-profile` container with cold page cache, what is the
> wall-clock W1 (process start to first inbound file list byte on the
> receiver) for the following four scenarios, with and without a
> parallel walk prototype?

| Scenario | Source layout | Total entries |
|---|---|---|
| S1 | single root, deep tree | 1 M |
| S2 | single root, wide tree (10K dirs x 100 files) | 1 M |
| S3 | 4 roots, disjoint, same device | 1 M |
| S4 | 4 roots, disjoint, different devices (loop mounts) | 1 M |

Measurement protocol:
1. Wire a sender-side `Instant` at `core::session()` entry. Embed it in
   the first multiplex frame as a sender timestamp (debug-only,
   feature-gated). Receiver records its own `Instant` when the first
   flist byte is parsed. Delta is W1.
2. Run baseline (single-producer enumeration) and prototype
   (`rayon::scope` per root) under `hyperfine --warmup 3 --runs 30`
   with `echo 3 > /proc/sys/vm/drop_caches` in the `--prepare` block.
3. Decision rule: prototype wins only if S3 and S4 show >=15%
   reduction in W1 and S1, S2 are within 2% of baseline. Any
   single-root regression rejects the prototype outright.

If the bench is not run, the recommendation stands at defer
indefinitely. There is no cheap way to validate the hypothesis from a
synthetic microbench - the file list send path is too entangled with
the wire format to mock convincingly.

## 7. Outcome

- `WorkQueueSender` stays SPMC. #4173 conclusion preserved.
- Parallel source enumeration is deferred pending the W1 bench in
  Section 6.
- The earlier prototype sketch
  (`docs/design/parallel-source-enumeration.md`) remains as historical
  context. This doc replaces its recommendation: prototype-then-bench
  becomes bench-first, prototype-only-if-justified.
