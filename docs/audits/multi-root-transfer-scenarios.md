# Multi-root transfer scenarios and the multi-producer WorkQueue

Tracking issue: oc-rsync task #1690 ("Evaluate multi-root transfer scenarios
requiring multi-producer WorkQueue"). Branch:
`docs/multi-root-transfer-1690`. This is an investigation document only;
no code is changed.

Related (still open) trackers, referenced where relevant but not closed by
this audit: #1382 (design multi-producer WorkQueue for multi-root),
#1405 (design multi-producer WorkQueue for parallel generator fan-in),
#1573 (parallel source enumeration), #1609 (audit `WorkQueueSender`
usage sites), #1610 (Arc-wrapped `WorkQueueSender`),
#1383 (Arc-wrapped sender for multi-generator fan-in),
#1572 / #1613 (benchmark single-producer vs multi-producer overhead).
Closed and reused here: #1404 (Clone added to `WorkQueueSender` behind
the `multi-producer` feature flag, currently unused in production).

## Summary

oc-rsync today serves a multi-root invocation
(`oc-rsync -av src1/ src2/ src3/ dst/`) by iterating the source list
sequentially in two distinct call paths:

- Local copy: `crates/engine/src/local_copy/executor/sources/orchestration.rs:79`
  walks `for source in plan.sources()` one source at a time.
- Wire transfer (sender side): `crates/transfer/src/generator/file_list/mod.rs:64`
  walks `for base_path in base_paths` one base at a time, then sorts the
  combined file list once (`mod.rs:70-90`).

This mirrors upstream rsync 3.4.1 byte-for-byte
(`target/interop/upstream-src/rsync-3.4.1/flist.c:2258-2271`,
the `while (1) { ... if (argc-- == 0) break; ... *argv++ ... }`
arg-by-arg loop inside `send_file_list()`). The receiver consumes one
merged, monotonically-NDX-ordered stream
(`flist.c:380-575 send_file_entry()`,
`crates/protocol/src/codec/ndx/codec.rs:357-407 MonotonicNdxWriter`).

`WorkQueueSender` lives at
`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48` and is by
default `Send` but **not** `Clone`. A `multi-producer` cargo feature
(`crates/engine/Cargo.toml:90`) opts in to a `Clone` impl
(`crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17`),
which is exercised today only by a regression test
(`crates/engine/tests/multi_producer_work_queue.rs:13`,
`#![cfg(feature = "multi-producer")]`). No production call site uses it.

**Recommendation: do NOT pursue multi-producer WorkQueue for multi-root
parallelism on the wire-protocol path.** The wire protocol's
single-stream invariant (`MonotonicNdxWriter` + the receiver's
`recv_files()` loop) means even fully parallel per-root file-list
generation must merge into one ordered NDX stream before any byte
leaves the sender. Parallelism, where it pays, lives elsewhere
(parallel `lstat`, parallel signature build, the existing receiver-
side `concurrent_delta` pipeline). The trackers asking for a multi-
producer queue at the receiver's work queue (#1382, #1405, #1610,
#1383) should be **closed as not-required** with a pointer to this
audit. Trackers about the *engine-internal* infrastructure work
(#1404 done, #1609 done by `multi_producer_audit.rs`,
#1572 / #1613 benchmarks) can stand independently. See section 6.

## Source files inspected

All paths repository-relative.

- `crates/cli/src/frontend/execution/operands.rs:40`
  (`extract_operands` - parses positional arguments).
- `crates/cli/src/frontend/execution/file_list/resolver.rs`
  (`resolve_file_list_entries` - `--files-from` resolution against a
  single base when only one base source is present).
- `crates/cli/src/frontend/execution/drive/workflow/run.rs:281`
  (top-level orchestration, calls `extract_operands`).
- `crates/core/src/client/run/mod.rs:248`
  (`LocalCopyPlan::from_operands(config.transfer_args())`).
- `crates/engine/src/local_copy/plan/plan_impl.rs:50`
  (`LocalCopyPlan::from_operands` - splits N+1 operands into
  `sources: Vec<SourceSpec>` + `destination: DestinationSpec`).
- `crates/engine/src/local_copy/executor/sources/orchestration.rs:27`
  (`copy_sources` - the `for source in plan.sources()` loop at line 79).
- `crates/transfer/src/generator/file_list/mod.rs:52`
  (`build_file_list(base_paths: &[PathBuf])`).
- `crates/transfer/src/generator/file_list/mod.rs:124`
  (`build_file_list_with_base` - `--files-from` path).
- `crates/transfer/src/generator/file_list/walk.rs:32`
  (`walk_path` - per-root recursion).
- `crates/transfer/src/generator/transfer.rs:737-748`
  (`build_file_list` then `partition_file_list_for_inc_recurse` then
  `send_file_list`).
- `crates/transfer/src/delta_pipeline.rs:181`
  (`ParallelDeltaPipeline` - holds one `WorkQueueSender`,
  `next_sequence: u64` is a single counter).
- `crates/transfer/src/receiver/mod.rs:192`
  (receiver holds `Option<Box<dyn ReceiverDeltaPipeline>>`,
  default is `SequentialDeltaPipeline`).
- `crates/transfer/src/receiver/transfer/pipeline.rs:160-200`
  (the `par_iter().map().collect()` signature batch pattern that
  existing receiver-side parallelism uses).
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48`
  (`WorkQueueSender` default no-Clone definition).
- `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17`
  (feature-gated `Clone` impl).
- `crates/engine/src/concurrent_delta/work_queue/mod.rs:11-35`
  (SPMC contract documentation, multi-producer references #1382 and
  #1569).
- `crates/engine/src/concurrent_delta/multi_producer_audit.rs:1-95`
  (the existing #1609 site-by-site audit; predates #1690).
- `crates/engine/src/concurrent_delta/consumer.rs:120-194`
  (`DeltaConsumer::spawn` - the single reorder-buffer thread that
  enforces in-order delivery from a sequence-tagged work queue).
- `crates/protocol/src/codec/ndx/mod.rs:1-58`
  (NDX wire format, two strategies for protocol < 30 vs >= 30).
- `crates/protocol/src/codec/ndx/codec.rs:357-407`
  (`MonotonicNdxWriter` - asserts strictly increasing positive NDX in
  debug builds, "file indices must be strictly increasing on the wire").
- `crates/protocol/src/flist/segment.rs:21-32`
  (`FileListSegment` - INC_RECURSE per-directory sub-list with
  `ndx_start` global offset).
- Upstream rsync 3.4.1 under
  `target/interop/upstream-src/rsync-3.4.1/`:
  `flist.c:2192 send_file_list()`,
  `flist.c:2258-2271 while-loop over argv`,
  `flist.c:380 send_file_entry()`,
  `flist.c:1534 send_file_name()`,
  `generator.c:2226 generate_files()`,
  `generator.c:2312 for (i = cur_flist->low; i <= cur_flist->high; i++)`.

## 1. Status quo: how oc-rsync handles a multi-root invocation today

### 1.1 CLI to plan

When a user runs `oc-rsync -av /etc/ /var/log/ /home/user/ dst/`:

1. `extract_operands` (`crates/cli/src/frontend/execution/operands.rs:40`)
   collects every non-option argument into a single `Vec<OsString>`.
   Multiple positional args become multiple operands. There is no
   special multi-root flag; `argc` simply > 2.
2. The drive workflow (`crates/cli/src/frontend/execution/drive/workflow/run.rs:281`)
   passes the operands into config building, which ultimately drives
   `core::client::run::run_client`. For local transfers the call is
   `LocalCopyPlan::from_operands(config.transfer_args())`
   (`crates/core/src/client/run/mod.rs:248`).
3. `LocalCopyPlan::from_operands`
   (`crates/engine/src/local_copy/plan/plan_impl.rs:50-85`) splits the
   operands by treating the last as destination and all preceding as
   sources:

   ```rust
   // crates/engine/src/local_copy/plan/plan_impl.rs:55-58
   let sources: Vec<SourceSpec> = operands[..operands.len() - 1]
       .iter()
       .map(SourceSpec::from_operand)
       .collect::<Result<_, _>>()?;
   ```

   Each `SourceSpec` records the path plus its trailing-slash semantics.
   No remote-vs-local mixing is allowed at this site (remote operands
   are rejected), but a transfer with an SSH/daemon source still
   reaches a different code path that preserves the same per-source
   list shape (`crates/core/src/client/remote/`).

### 1.2 Local copy execution: serial per-source loop

`copy_sources`
(`crates/engine/src/local_copy/executor/sources/orchestration.rs:79`)
explicitly iterates one source at a time:

```rust
// crates/engine/src/local_copy/executor/sources/orchestration.rs:79-109
for source in plan.sources() {
    let result = process_single_source(
        context,
        plan,
        source,
        destination_path,
        destination_behaves_like_directory,
        multiple_sources,
    );
    if let Err(error) = result {
        if error.is_vanished_error() {
            // upstream: flist.c:1289 - vanished files produce a warning
            // and set IOERR_VANISHED, but transfer continues.
            ...
        }
        ...
    }
}
```

Each source is fully walked, copied, and committed before the next
source starts. Within a single source, parallelism happens via
`rayon::par_iter` over directory entries (see
`crates/engine/src/local_copy/executor/directory/parallel_planner.rs:101`
and the matrix in `docs/parallelism_audit.md`). Cross-source
parallelism does not exist in the local-copy path. `WorkQueue` is
not used here at all (see
`crates/engine/src/concurrent_delta/multi_producer_audit.rs:67-81`,
"Opportunity 3: Local copy" - "Conclusion: Not applicable").

### 1.3 Wire transfer (sender / generator role): serial walk, single sort, single send

For a transfer with the wire protocol active, the sender's
`build_file_list`
(`crates/transfer/src/generator/file_list/mod.rs:52-110`) is the
canonical multi-root entry point:

```rust
// crates/transfer/src/generator/file_list/mod.rs:64-66
for base_path in base_paths {
    self.walk_path(base_path, base_path.clone())?;
}
```

Each `walk_path` (`crates/transfer/src/generator/file_list/walk.rs:32`)
is a recursive traversal that pushes entries into one shared
`self.file_list` and `self.full_paths` vector. After all roots have
been walked, the combined list is sorted **once** by
`compare_file_entries`
(`crates/transfer/src/generator/file_list/mod.rs:70-90`):

```rust
// crates/transfer/src/generator/file_list/mod.rs:79-89
let cmp =
    |&a: &usize, &b: &usize| compare_file_entries(&file_list_ref[a], &file_list_ref[b]);
if self.config.qsort {
    indices.sort_unstable_by(cmp);
} else {
    indices.sort_by(cmp);
}
apply_permutation_in_place(&mut self.file_list, &mut self.full_paths, indices);
```

After sort, `partition_file_list_for_inc_recurse` and `send_file_list`
emit one merged stream
(`crates/transfer/src/generator/transfer.rs:737-748`):

```rust
// crates/transfer/src/generator/transfer.rs:737-748
let file_count = {
    let _t = PhaseTimer::new("file-list-build-send");
    if files_from_paths.is_empty() {
        self.build_file_list(paths)?;
    } else {
        let base_dir = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
        self.build_file_list_with_base(&base_dir, &files_from_paths)?;
    }
    self.partition_file_list_for_inc_recurse();
    self.send_file_list(writer)?
};
```

Roots are NOT segmented per-arg on the wire. Each root contributes
entries to the same combined NDX space; with INC_RECURSE on, the
per-directory partitioning happens after the merged sort, not per-arg.

### 1.4 Where `WorkQueueSender` plugs in (today: receiver only, never per-root)

`WorkQueueSender`
(`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50`) is
held by exactly one production type:
`ParallelDeltaPipeline`
(`crates/transfer/src/delta_pipeline.rs:181-220`). The sender field is
`work_tx: Option<WorkQueueSender>`; it is constructed once via
`work_queue::bounded_with_capacity(capacity)` and never cloned:

```rust
// crates/transfer/src/delta_pipeline.rs:209-220
pub fn new(worker_count: usize) -> Self {
    let capacity = worker_count.saturating_mul(2).max(2);
    let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
    let consumer = DeltaConsumer::spawn(work_rx, capacity);

    Self {
        next_sequence: 0,
        work_tx: Some(work_tx),
        consumer: Some(consumer),
    }
}
```

`submit_work` stamps a monotonic `next_sequence` per call
(`crates/transfer/src/delta_pipeline.rs:222-234`) and uses the single
sender. The receiver
(`crates/transfer/src/receiver/mod.rs:192-242`) holds an
`Option<Box<dyn ReceiverDeltaPipeline>>`, default
`SequentialDeltaPipeline::new()`. The pipeline is wire-level: items
correspond to file entries pulled off the multiplexed stream by the
single network reader. There is no per-root cloning of the sender
because there is no per-root reader thread.

The `multi-producer` feature
(`crates/engine/Cargo.toml:90 multi-producer = []`) wires in a `Clone`
impl
(`crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17-23`)
that lets two or more producer threads share one bounded channel.
This was landed under #1404 with documentation
(`work_queue/mod.rs:31-35`) noting it is "forward-looking infrastructure"
and exercised only by `crates/engine/tests/multi_producer_work_queue.rs`.
No production binary turns the feature on.

The pre-existing site audit
`crates/engine/src/concurrent_delta/multi_producer_audit.rs:1-95`
already classifies all three `WorkQueueSender` holders
(`ParallelDeltaPipeline`, `DeltaConsumer::spawn` consumer side,
`ThresholdDeltaPipeline`) as correctly single-producer because the
wire protocol delivers a single multiplexed stream. #1690 is the
*receiver's* mirror question for multi-root specifically; the answer
this audit reaches is the same.

## 2. Upstream comparison

Upstream rsync 3.4.1 handles multi-root inside `send_file_list()`
(`target/interop/upstream-src/rsync-3.4.1/flist.c:2192`). The relevant
loop is:

```c
/* target/interop/upstream-src/rsync-3.4.1/flist.c:2258-2271 */
while (1) {
    char fbuf[MAXPATHLEN], *fn, name_type;

    if (use_ff_fd) {
        if (read_line(filesfrom_fd, fbuf, sizeof fbuf, rl_flags) == 0)
            break;
        sanitize_path(fbuf, fbuf, "", 0, SP_KEEP_DOT_DIRS);
    } else {
        if (argc-- == 0)
            break;
        strlcpy(fbuf, *argv++, MAXPATHLEN);
        if (sanitize_paths)
            sanitize_path(fbuf, fbuf, "", 0, SP_KEEP_DOT_DIRS);
    }
    ...
}
```

This is the canonical multi-root traversal: a single thread walking
`argv++` one name at a time, calling `send_file_name()`
(`flist.c:1534`) for each. `send_file_name` invokes `send_file_entry`
(`flist.c:380-575`), which writes one `xflags` byte / shortint, an
optional `first_hlink_ndx`, the fname slice, and the entry's metadata
fields. Each entry is identified by its position in the combined
`flist->files[]` array; on the wire the receiver reads them with the
expectation that **NDX values are strictly increasing within a phase**
(`flist.c:509`: `np->data = (void*)(long)(first_ndx + ndx);`).

The receiver-side counterpart, `generate_files()`
(`generator.c:2226`), is also strictly serial:

```c
/* target/interop/upstream-src/rsync-3.4.1/generator.c:2312-2315 */
for (i = cur_flist->low; i <= cur_flist->high; i++) {
    struct file_struct *file = cur_flist->sorted[i];

    if (!F_IS_ACTIVE(file))
        ...
}
```

Upstream walks the merged, sorted file list in monotonic NDX order.
There is no parallelism, no fan-in, no concurrent producers. INC_RECURSE
adds incremental sub-list discovery
(`flist.c:send_extra_file_list` reachable from `flist.c:2069,2071`),
but each sub-list is built and emitted serially before the next
sub-list begins, again driven by a single thread.

The upstream model is therefore: **one producer (the sender's main
thread), one consumer (the receiver's main thread), one ordered NDX
stream, regardless of how many positional source roots the user
passed**. Multi-root is a CLI affordance, not a protocol feature.

## 3. Real-world multi-root use cases

The user-visible scenarios that involve multi-root invocations:

### 3.1 Mixed-source backup
`oc-rsync -av /etc/ /var/log/ /home/user/ backup-host:/backups/host1/`
A single host backup that consolidates several top-level directories
under one remote destination. Common in shell-based backup scripts.
Each root is a different filesystem subtree, possibly on a different
mount point.

- Each root's traversal is independent; parallel `walk_path` is feasible
  in principle (no shared state inside the walker).
- The merged sort
  (`crates/transfer/src/generator/file_list/mod.rs:79-90`) is a global
  barrier: every root must finish walking before the sort can run.
- Disk seek pattern: HDDs penalise concurrent walks across distant
  inodes; on SSD this is largely free.

### 3.2 Remote-to-local multi-root pull
`oc-rsync -av sshhost:src1/ sshhost:src2/ dst/`
A single SSH session, multi-root sender. Upstream's protocol does not
support multiple sender roots over one SSH channel except by passing
them all to the remote `rsync --server`, which then runs the
arg-by-arg loop in section 2 on the remote side. oc-rsync inherits
this through `crates/core/src/client/remote/invocation/builder.rs`
(builds the remote argv from `transfer_args`).

- All root walking happens on the *remote* (sender) side.
- Local oc-rsync sees the merged stream from the remote and never
  knows how many roots were asked for.
- Multi-producer queue on the local receiver would not help: there is
  exactly one TCP socket to read.

### 3.3 Mixed file and directory operands
`oc-rsync -av /etc/hosts /etc/resolv.conf /etc/passwd dst/`
Three regular files, no directories. Upstream's
`flist.c:2455-2480` `send1extra` path emits each as a single
`send_file_name` call.

- Walking is trivial (no recursion).
- Number of file-list entries == number of args.
- Parallelism gain across roots is tiny because per-root work is
  microseconds.

### 3.4 `--files-from` plus extra positional roots
`oc-rsync -av --files-from=list.txt /base/ dst/`
Upstream rsync requires exactly one positional source when
`--files-from` is active (`flist.c:2240-2244 change_dir(argv[0])`).
oc-rsync mirrors this in
`crates/transfer/src/generator/transfer.rs:743-744`:

```rust
let base_dir = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
self.build_file_list_with_base(&base_dir, &files_from_paths)?;
```

There is no second positional root in this mode; the entries inside
the file are not "roots" in the same sense (they are paths relative
to the single base). Multi-root parallelism is irrelevant here.

### 3.5 Shell glob expansion to many args
`oc-rsync -av /var/log/*.gz backup:/archive/`
A glob in the shell can expand to hundreds or thousands of positional
args, each a single file. oc-rsync's `walk_path` handles each with a
single `lstat` plus `push_file_item` (no recursion required for
non-directories).

- Per-arg work: one `lstat`, one `FileEntry` construction, one push
  into `self.file_list`.
- The bottleneck is `lstat` latency, not CPU.
- Parallel `lstat` across globbed args could win on a slow filesystem,
  but the existing batched-stat machinery
  (`crates/flist/src/batched_stat/`) is per-directory not per-arg, so
  this case currently is not parallelised.

### 3.6 Cross-filesystem multi-root
`oc-rsync -av /mnt/nfs1/ /mnt/nfs2/ /local/data/ dst/`
Three roots, three different filesystems, different latency
characteristics. The slowest filesystem dominates wall time even with
sequential walk because nothing else can proceed.

- This is the most plausible parallel-root win: `lstat` against an
  NFS mount can take milliseconds, and three concurrent walks would
  hide latency.
- Disk I/O bandwidth is shared per device, not across devices, so a
  cross-FS workload has more headroom than a single-FS one.
- Still constrained by the merged-sort barrier and the single-stream
  send.

### Summary table

| # | Scenario | Roots | Parallelism worth pursuing? | Note |
|---|---------|------|---------------------------|------|
| 3.1 | Mixed-source backup | 3-10 | Marginal | Walking is fast on SSD, NFS mounts shift the answer |
| 3.2 | Remote multi-root pull | N | No | Walking happens on the remote sender, single TCP back |
| 3.3 | File-only mixed args | 5-20 | No | Per-arg work is microseconds |
| 3.4 | `--files-from` + base | 1 | N/A | Not multi-root by construction |
| 3.5 | Shell glob expansion | 100s | Maybe (per-arg `lstat`) | `lstat` latency is the bottleneck |
| 3.6 | Cross-filesystem | 2-5 | Yes (latency-bound) | Walking different mounts in parallel hides latency |

The conclusion across all scenarios: **walking and stat'ing roots in
parallel can pay off in narrow cases (3.5, 3.6), but the wire
protocol does not benefit from multi-producer dispatch into a single
WorkQueue**. Parallelism, where it pays, lives in the file-list build
phase, not the delta-dispatch phase.

## 4. Parallelism opportunities and constraints

### 4.1 Where multi-producer WorkQueue could (in theory) help

The receiver-side `ParallelDeltaPipeline`
(`crates/transfer/src/delta_pipeline.rs:181`) submits `DeltaWork`
items to a bounded queue. If the receiver had multiple independent
sources of work items (one per root, say), each could clone the
`WorkQueueSender` and feed in parallel. The reorder buffer in
`DeltaConsumer::spawn`
(`crates/engine/src/concurrent_delta/consumer.rs:129-188`) would still
deliver results in `sequence` order via its
`ReorderBuffer::insert(seq, ...) -> drain_ready()` discipline.

**But there is no source of multiple producer threads on the receiver
side.** The receiver reads file entries off one multiplexed stream.
There is one network read loop, period. Quoting
`crates/engine/src/concurrent_delta/work_queue/mod.rs:16-22`:

> This is SPMC rather than MPMC because the rsync wire protocol is
> inherently single-threaded on the receiving side - one multiplexed
> stream delivers file entries in sequence, so there is exactly one
> thread reading from the wire and producing work items.

For multi-producer to help, you would need *N* concurrent producers
that can each generate work items independently. Multi-root at the
CLI does not generate that condition: by the time bytes reach the
receiver, the sender has already merged all roots into one stream
ordered by global NDX.

### 4.2 Where the wire protocol prohibits per-root parallelism

The wire protocol mandates strictly increasing positive NDX values
within a transfer phase. oc-rsync enforces this in debug builds via
`MonotonicNdxWriter`
(`crates/protocol/src/codec/ndx/codec.rs:392-407`):

```rust
// crates/protocol/src/codec/ndx/codec.rs:393-408
fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
    #[cfg(debug_assertions)]
    if ndx >= 0 {
        if let Some(prev) = self.last_positive {
            debug_assert!(
                ndx > prev,
                "NDX monotonicity violation: emitted {ndx} after {prev} - \
                 file indices must be strictly increasing on the wire"
            );
        }
        self.last_positive = Some(ndx);
    }
    self.inner.write_ndx(writer, ndx)
}
```

Upstream's `send_file_entry` (`flist.c:380-575`) writes
`first_ndx + ndx` as the entry index
(`flist.c:509: np->data = (void*)(long)(first_ndx + ndx);`). The
receiver indexes into `cur_flist->files[ndx - cur_flist->ndx_start]`.
This works **only if `flist->files[]` is the merged, sorted list**.
A per-root parallel sender that emits two interleaved NDX streams
without a global merge step would either:

- Produce duplicate NDX values (collision), violating
  `MonotonicNdxWriter` and the receiver's array index assumption.
- Require renumbering on the wire, which is not part of the protocol.

Therefore the sender's emission step is **irreducibly serial** in
NDX-monotonicity terms. You may parallelise the *production* of
file-list entries (multiple `walk_path` workers writing into a
shared `Vec<FileEntry>` under a mutex, or per-root buffers merged in
sort), but you cannot parallelise the wire emission.

### 4.3 Invariants that must hold

| Invariant | Where enforced | Why per-root parallelism breaks it |
|-----------|----------------|------------------------------------|
| Strictly-increasing positive NDX on the wire | `crates/protocol/src/codec/ndx/codec.rs:399-405` (`MonotonicNdxWriter`) | Two concurrent producers would emit interleaved NDX |
| File-list completion before delta phase | `crates/transfer/src/generator/transfer.rs:737-748` (build then send then run_transfer_loop) | Parallel root walks must complete a global barrier before send_file_list |
| INC_RECURSE segment ordering | `crates/protocol/src/flist/segment.rs:21-32` (`ndx_start` is global, `parent_dir_ndx` references prior segments) | Cross-root parallelism would race on `ndx_start` allocation |
| Receiver's `flist->files[]` indexability | Upstream `flist.c:509`, `generator.c:2312` | Requires merged sorted list |
| `MonotonicNdxWriter` debug assertion | `crates/protocol/src/codec/ndx/codec.rs:399-403` | Asserts ndx > prev; trips immediately on out-of-order writes |

The merged-sort barrier in
`crates/transfer/src/generator/file_list/mod.rs:70-90` is the
practical anchor: after the sort, every entry has a deterministic
position in the combined list, and that position becomes its NDX.
Anything before the sort can run in parallel; nothing after the sort
can.

## 5. Design implications for #1382 / #1405 / #1610

### 5.1 Would multi-producer WorkQueue change the wire format?

**No.** The wire format is fixed by protocol 28-32. `WorkQueue` is an
internal, in-memory scheduling primitive
(`crates/engine/src/concurrent_delta/work_queue/`), not a wire
construct. Adding clones of `WorkQueueSender` does not touch the
multiplex framing
(`crates/protocol/src/`), the NDX codec
(`crates/protocol/src/codec/ndx/`), the file-list encoding
(`crates/protocol/src/flist/`), or any byte that crosses the network.

This matches the user feedback in
`feedback_no_wire_protocol_features.md`: do not add protocol
extensions for niche performance features. Multi-producer queue is
strictly off-the-wire.

### 5.2 Where exactly would `Arc<WorkQueueSender>` or cloned senders plug in?

There are exactly three holders of `WorkQueueSender` today
(per `crates/engine/src/concurrent_delta/multi_producer_audit.rs:9-35`):

1. `ParallelDeltaPipeline.work_tx`
   (`crates/transfer/src/delta_pipeline.rs:185`) - one sender, fed by
   the receiver's single network read loop.
2. `DeltaConsumer::spawn` - consumer-side, holds the receiver half;
   does not hold a sender.
3. `ThresholdDeltaPipeline` - constructs a `ParallelDeltaPipeline`
   internally, delegates to (1).

To plug in multi-producer for multi-root specifically, you would need
either:

- **Receiver-side fan-in (#1382, #1405).** Have multiple network
  reader threads, each pulling its own sub-stream, each cloning
  the `WorkQueueSender`. **Blocked by the wire protocol**: there is
  exactly one TCP connection / one multiplex stream per session.
  Even with multiple roots on the CLI, the receiver gets one stream.
- **Sender-side fan-in (#1383).** Have the sender's `walk_path`
  workers each clone a sender and push entries into a queue, with
  a single drain thread sorting and emitting. **Architecturally
  redundant**: the existing
  `crates/transfer/src/generator/file_list/mod.rs:64-90` build then
  sort sequence already merges per-root walks; making the merge use
  a queue is a refactor, not an enabler. The current
  `Vec<FileEntry>` plus indirect-permutation sort is more efficient
  than a producer-consumer queue with a downstream sort because the
  sort is already in-place and parallel-friendly via rayon.
- **`Arc<WorkQueueSender>` (#1610).** Wrapping the sender in `Arc`
  would let multiple references share lifetime semantics.
  `crossbeam-channel`'s `Sender` (the underlying type at
  `bounded.rs:8`) is already `Clone` and shares its inner state via
  its own `Arc`. Wrapping our wrapper in another `Arc` adds an
  indirection without unlocking new capability. The feature-gated
  `Clone` already gives the same effect more directly.

The honest plug-in points for multi-producer simply do not exist on
the wire-protocol path.

### 5.3 Test gaps

- No end-to-end interop test exercises multi-root explicitly. The
  closest is `multiple_sources` mentions in
  `crates/engine/src/local_copy/tests/execute_skip.rs` (and four
  other local-copy tests), all local-only.
- Wire-protocol multi-root has no dedicated golden test. A new test
  in `crates/protocol/tests/golden/` would capture the wire bytes
  for a 2-root invocation and verify they match upstream byte-for-
  byte. Recommend filing a follow-up tracker for this gap; it stands
  on its own merit independently of #1690.
- The `crates/engine/tests/multi_producer_work_queue.rs` integration
  test is gated on `feature = "multi-producer"` and is the only
  exercise of cloned senders. It is a unit-style test of the queue
  primitive, not an end-to-end test of multi-root transfer.

## 6. Recommendation

**Multi-root parallelism via multi-producer WorkQueue is not worth
pursuing for beta. Close trackers #1382, #1405, #1610, #1383 as
not-required, citing this audit.** Keep #1404 (done), #1572,
#1613, and #1609 as standalone benchmark / audit work; they are
useful even without a multi-root use case.

Reasoning recap:

1. The wire protocol's monotonic-NDX invariant
   (`crates/protocol/src/codec/ndx/codec.rs:392-408`) makes the
   sender's emission inherently serial. A multi-producer queue feeding
   the wire writer cannot help.
2. The receiver's single multiplex stream
   (`crates/engine/src/concurrent_delta/work_queue/mod.rs:16-22`)
   means there is exactly one producer thread, full stop. Cloning the
   sender is a non-op.
3. Upstream rsync is purely serial across roots
   (`flist.c:2258-2271`, `generator.c:2312-2315`). Adding parallelism
   here would diverge from upstream behaviour without a corresponding
   wire benefit.
4. The narrow scenarios that *would* benefit from per-root
   parallelism (3.5 globbed args, 3.6 cross-filesystem) want
   parallel `lstat` / `walk_path`, not parallel `WorkQueue` dispatch.
   Those wins, if pursued, belong to
   `crates/transfer/src/generator/file_list/walk.rs` and
   `crates/flist/src/parallel.rs`, not to the
   `concurrent_delta` infrastructure.
5. The existing
   `crates/engine/src/concurrent_delta/multi_producer_audit.rs:36-95`
   already concludes the same thing for the broader question; #1690
   is the multi-root specialisation and the answer is unchanged.

If a future maintainer wants to revisit this, the conditions that
would justify reopening:

- A new wire-protocol-level mechanism for parallel NDX streams (which
  upstream has not proposed and which oc-rsync explicitly rejects per
  `feedback_no_wire_protocol_features.md`).
- A non-rsync consumer of the engine crate that ingests a different,
  inherently-multi-stream protocol.
- A measurable benchmark win on the cross-filesystem case (3.6) that
  is tied specifically to `WorkQueue`-based dispatch, not to
  `walk_path` parallelism.

### Suggested issue dispositions

| Issue | Disposition | Rationale |
|-------|------------|-----------|
| #1382 design multi-producer WorkQueue for multi-root | Close as not-required | No multi-producer condition exists on the wire path |
| #1405 design multi-producer WorkQueue for parallel generator fan-in | Close as not-required | Same as #1382 - single multiplex stream |
| #1610 Arc-wrapped `WorkQueueSender` | Close as not-required | `crossbeam_channel::Sender` already `Clone`; wrapper Clone added in #1404 |
| #1383 Arc-wrapped sender for multi-generator fan-in | Close as not-required | No multi-generator threads exist; receiver has one reader |
| #1573 parallel source enumeration with multi-producer WorkQueue | Re-scope | Drop the WorkQueue framing; the win is parallel walk + merged sort, not queue dispatch. Re-file as parallel walk_path investigation |
| #1572 / #1613 single vs multi-producer benchmarks | Keep open | Useful as engine-level micro-benchmarks regardless of multi-root |
| #1609 audit `WorkQueueSender` usage sites | Close as done | Completed by `multi_producer_audit.rs` and superseded by this audit |
| #1404 add Clone behind feature flag | Already closed | Done; feature flag remains as forward-looking |
| #1690 (this audit) | Close on merge | Document landed |

Per `feedback_no_wire_protocol_features.md`: this audit explicitly
does not propose any wire-protocol extension to enable multi-root
parallelism. The trackers above either become not-required or remain
as engine-internal hygiene.

## 7. Distinct from sibling tasks

| Tracker | Scope | This audit |
|---------|-------|-----------|
| #1382 | Multi-producer WorkQueue for multi-root | Recommends close |
| #1405 | Multi-producer for parallel generator fan-in | Recommends close |
| #1573 | Parallel source enumeration via WorkQueue | Recommends re-scope to walk-only parallelism |
| #1609 | Audit `WorkQueueSender` usage sites | Already done in `multi_producer_audit.rs` |
| #1610 | Arc-wrapped sender | Recommends close - already Clone-able under feature flag |
| #1383 | Arc-wrapped for multi-generator fan-in | Recommends close - same root cause |
| #1572 / #1613 | Benchmark single vs multi-producer | Keep open as engine-level micro-benchmarks |
| #1404 | Clone behind `multi-producer` feature | Done |
| **#1690 (this audit)** | **Evaluate multi-root scenarios for multi-producer WorkQueue** | **Closes with the recommendation in section 6** |
