# Multi-file delta-apply pipeline with preserved wire ordering

This note designs the next layer of receiver-side concurrency: pipelining
delta-apply across files while preserving the strict wire-order acknowledgement
and commit semantics that upstream rsync 3.4.1 enforces. The earlier
parallel-dispatch work (#1543, #1547) and the reorder-buffer machinery (#1407,
#1566, #1650) target file dispatch and post-apply re-serialization. This design
covers the interior step: overlapping the actual delta-apply for files
N..N+W-1 in flight, on a transfer where today's receiver applies file N to
completion before reading file N+1's first delta byte.

The doc is purely architectural. No wire-protocol changes, no new flags, no
new on-disk artefacts. Every byte sent to and received from the peer remains
identical to upstream-compatible behaviour.

## 1. Problem statement

Today the receiver runs a single-threaded delta-apply per file:

- `crates/transfer/src/receiver/transfer/pipeline.rs:38`
  (`run_pipeline_loop_decoupled`) is the outer reception loop. The window
  fill, request emission, and signature precomputation are pipelined, but
  per-response delta-apply runs synchronously inside
  `process_file_response_streaming`.
- `crates/transfer/src/delta_apply/applicator.rs:436`
  (`apply_delta_stream`) is the per-file inner loop:
  `while applicator.apply_token(reader)? {}`. It blocks on the reader, copies
  basis blocks, writes literal bytes, updates the strong checksum, and only
  returns once the file's delta stream is fully consumed.
- `crates/transfer/src/delta_apply/mod.rs` exposes the applicator as a single
  public surface; today there is exactly one applicator alive per receiver
  thread.

Three observations turn this into a real bottleneck:

1. With a high-latency wire (SSH, daemon TCP across a WAN), the reader spends
   a substantial fraction of each per-file wall-clock budget waiting on
   `read(2)` for the next token chunk. CPU sits idle on a core that is
   otherwise capable of applying tokens for an earlier file or precomputing
   strong-checksum verification on the trailing whole-file digest of file
   N-1.
2. The token reader is shared across files for compression-state continuity
   (see the `token_reader` allocation comment in `pipeline.rs:97`), so the
   producer side already operates in a single-stream way. We cannot trivially
   parallelize the *reader*. We can, however, hand off complete delta blobs
   for file N to a worker while the main thread continues reading file N+1.
3. Many real workloads have hundreds to thousands of small-to-medium files
   (config trees, source repos, build artefacts). Per-file fixed overhead -
   basis-file open, signature index lookup setup, applicator init - dominates
   the CPU budget when files are small. Pipelining hides that overhead behind
   the wire-read latency of the next file.

The goal: keep the wire format and the receiver's externally visible
behaviour byte-identical, while overlapping delta-apply across W in-flight
files. Targets:

- Local-loopback transfers: no measurable regression (the wire is already
  faster than apply, so pipelining buys nothing but must not cost anything
  either).
- LAN daemon push of 1000 small files: 1.4-1.8x throughput.
- WAN SSH push of medium files: 1.3-1.6x throughput.

## 2. Wire-compat invariants

These invariants are non-negotiable. Any pipelining design that violates one
is rejected.

### 2.1 NDX acknowledgement order matches wire-arrival order

Upstream `receiver.c:recv_files()` processes file responses strictly in the
order their NDX appears on the wire. The acknowledgement that returns to the
generator (the post-apply NDX echo, success or `IT_BASIS_TYPE_FOLLOWS` redo)
MUST be emitted in the same order. Any reordering is observable to upstream
because the generator uses NDX echoes to advance its own per-file state
machine.

Today's invariant: the receiver emits ack for file N before reading file
N+1's first delta byte. After pipelining: the receiver MAY read file N+1's
delta into memory while file N applies, but it MUST NOT emit the ack for
file N+1 before the ack for file N. The reorder buffer
(`crates/transfer/src/reorder_buffer.rs:55-64`,
`BoundedReorderBuffer<T>`) is the enforcement point.

### 2.2 Disk-commit (rename) order matches wire-arrival order

Each file's temp file `.oc-rsync-tmp.{nonce}` (or `.partial.{nonce}` for
`--partial`) is renamed to its final path on commit. With `--delay-updates`
all renames are deferred until end-of-transfer; without `--delay-updates`
they happen as each file completes. In both modes, the *sequence* of
commits as observed on disk MUST match the file-list order so that:

- A consumer watching the destination tree (`inotify`, file-system audit
  log, cloud snapshotter) sees the same per-file commit order as upstream
  rsync.
- `--backup` collisions resolve identically: if two files in the same
  transfer touch the same backup-suffix path, the second commit overwrites
  the first in file-list order.

Enforcement point: the disk-commit thread drains the reorder buffer in
sequence order and issues `rename(2)` synchronously per item.

### 2.3 `--delete-during` directory fence

`--delete-during` deletes destination entries for a directory only after all
files in that directory are committed. See
`docs/architecture/delete-during.md` for the upstream-faithful semantics.
Pipelining must respect this fence: deletion for directory D cannot start
until every file with parent D in the file list has been *committed* (not
merely applied). The reorder buffer's commit head is the natural fence -
deletion for D is gated on `commit_head >= max_seq(file in D)`.

### 2.4 `--delete-after` and end-of-transfer fences

`--delete-after` deletes only after every file commit completes. Same
mechanism: deletion phase waits on the commit head reaching
`total_files - 1`. No new fence is required; the existing one is sufficient.

### 2.5 Stats accounting unchanged

Per-file stats (literal bytes, matched bytes, file-size) are reported from
the apply phase via `DeltaApplyResult`
(`crates/transfer/src/delta_apply/applicator.rs:90-102`). Stats from
parallel-applied files MUST be aggregated in commit order, not
apply-completion order, so that `--stats` and the delete-stats wire frame
emit identical numbers regardless of in-flight count.

## 3. Three approaches considered

### 3.1 Approach A: read-ahead plus serial apply

Keep the current single-threaded applicator. Add a buffered read-ahead
layer between the wire and the applicator: the producer reads up to
`READ_AHEAD_BYTES` of file N+1, N+2, ... into a per-file byte buffer while
the main thread applies file N from its already-buffered stream.

Pros:

- Minimal code change. The applicator stays untouched.
- Trivial wire-compat: ack and commit order are still serial.
- Backpressure is just a buffer high-watermark.

Cons:

- The bottleneck on most LAN/WAN traces is *CPU time inside apply*, not
  wire latency. Buffered reads do not unlock the CPU.
- Memory cost is proportional to the read-ahead window times mean delta
  size. For large files this is many megabytes per in-flight file.
- The token reader's compression context (zstd `DCtx`) is a single
  continuous stream across files. Splitting input into per-file
  byte-buffers requires either decompressing in the producer (defeating
  the purpose) or maintaining a parallel decompression context per
  in-flight file (significant new state).

Verdict: rejected. Buys little on the workloads that matter.

### 3.2 Approach B: parallel apply with reorder-buffered commit

Each in-flight file gets a worker thread that runs the existing
single-file applicator end-to-end into a temp file. The wire-side producer
hands off a `(seq, file_meta, decompressed_delta_stream_handle)` tuple per
file into a bounded work queue. Workers pull, apply into temp, and report
back. A reorder buffer collects results in arrival order and drains them
into a serial commit thread that emits the ack and renames the temp file.

The pieces already exist:

- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` is a bounded
  multi-producer-single-consumer channel (`crossbeam_channel` based) with
  `2 * rayon::current_num_threads()` default capacity (see
  `capacity.rs:8`, `CAPACITY_MULTIPLIER = 2`). #1547 wired this up for
  parallel dispatch with a 64-file activation threshold.
- `crates/transfer/src/reorder_buffer.rs:55-114`
  (`BoundedReorderBuffer<T>`) enforces sliding-window in-order delivery
  with backpressure. #1566 added the bounded variant; #1650 plugged it
  into the dispatch path.
- `crates/engine/src/concurrent_delta/reorder.rs:30-60`
  (`ReorderBuffer<T>`) is the engine-side complement, used by the
  concurrent delta consumer thread.

The new wiring: a **per-receiver-context apply pool** that owns
`min(W, rayon_thread_count)` worker threads, each holding the per-file
applicator state. The wire-side producer thread becomes the file-list
dispatcher; it does not block on apply, only on wire I/O and on the
reorder buffer's window.

Pros:

- Reuses the existing reorder-buffer infrastructure (#1566).
- The applicator itself is untouched - it's still
  `apply_delta_stream(reader, applicator)` per file.
- Backpressure flows naturally from commit-head stall up through the
  reorder buffer into the producer.

Cons:

- Per-worker memory cost: applicator state plus a basis-file mapping per
  in-flight file. See section 5.
- Failure semantics: an apply error in worker for file N must not be
  "lost" behind successful applies for files N+1..N+W. Section 9.

Verdict: recommended. Section 4 elaborates.

### 3.3 Approach C: stream multiplexing

Split a single conceptual matcher across multiple basis files. mmap N basis
files, run M parallel matchers, each consuming from a shared input ring
that delivers post-decompression delta tokens tagged by destination file
seq.

Pros:

- Could in principle saturate every available core regardless of file
  size distribution.

Cons:

- The matcher in `apply_delta` is fundamentally serial *per file*: the
  block-copy state advances one token at a time, with the cached basis
  offset (`delta/script.rs::apply_delta`'s sequential-COPY optimization)
  only valid within a single file. Cross-file parallelism inside the
  matcher requires re-architecting the entire delta application path.
- Mostly redundant with #1023's spatial-split design (intra-file
  parallelism), which targets the same CPU cores via a different axis.
- Substantial new failure modes: token-stream demultiplexing, partial
  basis-file load coordination, cross-file backpressure.

Verdict: rejected. The expected gain over Approach B does not justify the
complexity; #1023 covers the intra-file dimension.

## 4. Recommended design: Approach B with bounded reorder

### 4.1 Pipeline shape

```
Wire reader (1 thread)                Apply pool (W workers)              Commit (1 thread)
+---------------------+                +-------------------+               +-----------------+
| read NDX, file meta |                | pull (seq, meta)  |               | drain reorder   |
| read delta bytes    | ---bounded---> | open basis (mmap) | ---reorder--> | rename temp     |
| stage to per-file   |    work queue  | apply_delta_stream|    buffer     | emit NDX ack    |
| in-memory blob      |    (W slots)   | verify checksum   |    (W slots)  | update stats    |
+---------------------+                | push (seq, result)|               | run delete fence|
                                       +-------------------+               +-----------------+
```

W = effective in-flight window. Default 16 (see section 10). Hard upper
bound: `2 * rayon::current_num_threads()` to align with the existing
work-queue capacity policy
(`crates/engine/src/concurrent_delta/work_queue/capacity.rs:8`,
`CAPACITY_MULTIPLIER`).

### 4.2 Why this fits the existing infrastructure

- The bounded work queue already exists (#1547) and already enforces
  rayon-aware capacity; we plug an additional consumer category (delta
  apply) into it.
- `BoundedReorderBuffer<T>` already enforces in-order delivery with
  backpressure (#1566). We use it with `T = AppliedFileResult` carrying
  the post-apply temp-file path, the `DeltaApplyResult` stats, the
  `ChecksumVerifier` outcome, and the metadata to commit.
- The disk-commit thread that already runs in `pipeline.rs:135`
  (`PipelinedReceiver::new(disk_config)`) is the natural reorder-buffer
  drain target. Today it processes results in produce order; in the new
  design it processes results in reorder-buffer drain order, which by
  construction matches wire-arrival order.

### 4.3 What does NOT change

- The wire reader. Same `ServerReader<R>`, same NDX codec, same
  `token_reader` shared across files for compression-state continuity.
- The applicator. Same `DeltaApplicator` and `apply_delta_stream`
  function signature.
- The disk-commit serialization. Exactly one rename happens at a time,
  exactly one ack returns at a time.
- Wire format. Tcpdump-replay on an oc-rsync-to-upstream push must remain
  byte-identical with pipelining on vs off.

### 4.4 What does change

- `run_pipeline_loop_decoupled` gains a third co-routine (the apply pool)
  between the existing wire reader and the existing disk-commit thread.
- The per-file `process_file_response_streaming` is split into a
  produce-side (read delta bytes into a staging buffer) and a
  consume-side (apply onto basis, verify checksum, hand to commit).
- A new `MultiFileApplyPool` type (proposed module:
  `crates/transfer/src/delta_pipeline/apply_pool.rs`) owns the workers
  and the reorder buffer.

## 5. Resource model

### 5.1 Per-file in-flight footprint

Each in-flight file holds:

- A temp-file FD (open `O_TMPFILE` on Linux when available; otherwise
  `creat` + immediate-unlink fallback). ~1 FD.
- A basis-file mapping. Lazy: opened the first time the worker touches
  basis. On Unix the strategy is `AdaptiveMapStrategy`
  (`crates/transfer/src/delta_apply/applicator.rs:25-28`): files >= 1
  MiB use mmap (cost: page-cache-backed, no new RSS until faulted);
  files < 1 MiB use a 256 KiB sliding `BufferedMap`. So the worst-case
  resident memory per basis is ~256 KiB (small files) plus mmap-VMA
  overhead per large file.
- A `DeltaSignatureIndex` reference (one per file). Inline ~135 bytes
  plus tag table (~1 KiB) plus lookup hashmap (~60-200 KiB for typical
  block counts; cited from `docs/design/zsync-inspired-matching.md`
  section "hot data structures").
- A `RingBuffer` window for rolling-checksum byte-by-byte advance, sized
  to `block_length`. Default block_length is 1 MiB-ish for large files,
  much smaller for small files. So worst case ~1 MiB per in-flight
  large file.
- Per-file applicator state plus a token-stream staging buffer (the
  decompressed-delta blob handed off from the reader). The blob is
  proportional to delta size; for highly modified files this can be
  ~10-50% of file size. We bound it explicitly in section 6.

### 5.2 Aggregate footprint at default window

W = 16, all files in the >= 1 MiB cohort:

- 16 temp-file FDs.
- 16 basis mmaps. Page cache absorbs them; RSS impact bounded by the
  actively-faulted working set.
- 16 ring buffers x ~1 MiB = ~16 MiB.
- 16 signature indices x ~200 KiB = ~3.2 MiB.
- 16 staging buffers, each capped at the configured per-file budget
  (proposed: `MAX_STAGED_DELTA_BYTES = 4 MiB` per slot, with overflow
  triggering temp-file spill).

Aggregate: O(20 MiB) at W=16. Acceptable on every supported target. On
constrained targets (embedded Linux, low-memory daemon), the activation
threshold gates this off entirely (section 10).

### 5.3 Capacity multiplier alignment

The work-queue uses `CAPACITY_MULTIPLIER = 2` against the rayon thread
count. The apply pool uses the same multiplier for symmetry: a system
with 8 rayon threads gets W = 16 by default. This keeps the multi-file
pipeline aligned with the file-dispatch parallelism budget; #1547 sized
its threshold to leave headroom for downstream consumers.

## 6. Backpressure

The reorder buffer's bounded window is the master backpressure mechanism.
Concrete flow:

1. Wire reader stages file N+W's delta bytes into a per-slot blob. If the
   blob exceeds `MAX_STAGED_DELTA_BYTES`, the reader spills to a
   pre-allocated scratch temp file (`O_TMPFILE` again). This keeps
   pipelining from blowing the heap on adversarial transfers (one tiny
   file followed by a multi-GB delta).
2. Wire reader pushes `(seq, file_meta, staged_handle)` into the bounded
   work queue. If the queue is full, the reader blocks on send. Today's
   bounded queue uses `crossbeam_channel`; the block is a parking
   semantic, no spinning.
3. Workers pull and apply. Workers push `(seq, AppliedFileResult)` into
   the reorder buffer. If the seq is outside the bounded window
   (BackpressureError, see `reorder_buffer.rs:78-86`), the worker
   blocks on a condvar until commit advances the head.
4. Commit thread drains the reorder buffer head-of-line. As soon as
   `next_expected` is satisfied, commits happen in a tight loop. Every
   commit advances the head, releases a reorder-buffer slot, releases
   the matching work-queue slot, and unblocks one wire-reader push.

Head-of-line semantics are documented in
`docs/architecture/reorder-buffer.md` (section "Current head-of-line
blocking behaviour"). The audit there (#1883 - reorder-buffer HoL doc)
formalizes the rule: at most W-1 items can be ahead of the stalled head,
bounding the unfair-wait tail. We adopt the same rule here without
modification.

## 7. Interaction with `--inplace`

`--inplace` writes directly to the destination file, no temp, no rename.
This breaks the assumption that two in-flight files have disjoint output
FDs: in-place writers for files N and N+1 trivially share a destination
FD only when they write the *same* path, which file-list ordering already
prevents. So in principle pipelining is safe.

However, two subtleties force a stricter rule:

1. The applicator under `--inplace` reads from the *same* file it writes
   to. The basis file IS the destination. If file N's apply is still in
   progress (still mid-stream of writes) and the worker for file N+1
   tries to open file N+1's basis, the open is fine (different file).
   But in the `--inplace --partial-dir` corner case, the partial leftover
   from a prior run might be at a path that another in-flight worker is
   touching. That is a pre-existing hazard, not new to pipelining; we
   inherit upstream's "don't mix `--inplace` with shared paths" rule.
2. `--inplace --append` advances a single FD that the applicator writes
   to and reads from, with offset moving monotonically. Concurrent apply
   on a different file is fine; concurrent apply on the *same* file from
   two workers is impossible (the seq numbers are unique per file in the
   file list).

The rule: `--inplace` is compatible with multi-file pipelining. No
serialization is required across files. We keep one worker = one file
= one FD; no two workers share a destination FD. The mode is permitted
in the same window as everything else.

The one exception: when the file-list contains two entries that resolve
to the same destination (`--copy-links` plus a symlink loop in the
source, or duplicate paths on case-insensitive filesystems), upstream
rsync produces undefined behaviour. We match upstream and do not
introduce new locking - the user's existing flag combination is the
hazard.

## 8. Interaction with `--partial` and `--partial-dir`

`--partial` keeps the temp file at `.partial.{nonce}` instead of deleting
it on apply failure. `--partial-dir=DIR` parks the partial under DIR.

Each in-flight file's partial is independent: the temp-file path is
nonce-tagged per file, scoped under either the file's parent or the
configured partial dir. There is no shared state between in-flight
partials; each worker owns its own.

On apply failure for file N with `--partial`:

- The worker stops writing, closes the temp-file FD (which keeps the
  file on disk under the partial name), and reports the failure to the
  reorder buffer.
- The reorder buffer drains the failure result in commit order. The
  commit thread sees the failure, does NOT rename, leaves the partial
  in place, and emits the appropriate NDX ack with `IT_BASIS_TYPE_FOLLOWS`
  to trigger a redo on the next phase.
- Files N+1..N+W in flight continue normally. They have their own
  partials, untouched by N's failure.

No special pipelining handling is needed beyond what
`crates/transfer/src/temp_guard.rs` already provides for nonce-tagged
temp files.

## 9. Failure semantics

Three failure classes:

### 9.1 Apply failure for file N (recoverable)

A checksum mismatch, a basis-read error, or a delta-stream malformation
is reported as `Err(io::Error)` from `apply_delta_stream`. The worker
reports `(seq=N, AppliedFileResult::Failed(error))` to the reorder
buffer. The reorder buffer admits it normally. The commit thread, on
draining seq N, sees the failure, leaves the temp file in place
(`--partial`) or removes it (default), emits the failure ack, and
continues with seq N+1. Files N+1..N+W in flight are NOT cancelled -
they each succeed or fail independently.

This matches upstream behaviour: a single file's failure does not abort
the transfer. Section 2.3's delete fence still holds: deletion for a
directory is gated on commit-head, regardless of whether the commits
were successes or failures.

### 9.2 Apply failure for file N (unrecoverable)

A panic in the worker, an OOM, or an I/O error that the applicator
cannot represent as a clean `io::Result`. Today the receiver context
unwinds and reports a transfer-level error; we preserve that.

For pipelining: the work-queue and reorder-buffer drop semantics must
ensure no worker leaks on unwind. The existing
`crates/transfer/src/temp_cleanup.rs` registers temp files with the
`TempGuard` so a panic cleans them. Workers must drop their `TempGuard`
on unwind boundary; this is achieved by RAII - no special handling. The
abort path is exercised by the property tests added in #2049 (reorder
buffer drop-on-error).

### 9.3 Wire-side failure during pipelining

The producer's `read(2)` returns `Err`. The producer drops the
work-queue sender, which closes the channel; workers see the close and
drain remaining items. For each in-flight file that has not yet been
committed, the commit thread emits no ack (the wire is dead, no point)
and removes the temp file. The error propagates up to the receiver
context as a transfer-level error.

This is functionally identical to today's serial path: the wire fails,
no further commits happen, partial state is cleaned by `TempGuard`.

## 10. Activation threshold

The pipelining mechanism MUST be off by default for transfers where it
would cost more than it gains:

- Local-loopback or RAM-disk transfers: wire latency is sub-microsecond.
  Pipelining adds reorder-buffer overhead and worker-spawn overhead for
  no benefit.
- Tiny transfers (1-2 files): no opportunity for cross-file overlap.
- Single-core systems: workers contend for the same core; serial is at
  least as fast.

Activation rules (all must hold):

1. **File count** in flight >= 16 at any point during the transfer.
   The dispatcher's queue depth surfaces this; pipelining engages
   lazily once the queue depth crosses 16.
2. **Wire round-trip estimate** > 5 ms. Measured via the time between
   the first request flush and the first response byte for file 0.
   Below 5 ms, the wire latency is too low to amortize the worker
   handoff cost.
3. **Available rayon thread count** >= 4. Below 4, dispatch parallelism
   (#1547) and pipelining together would oversubscribe the CPU.

These rules are intentionally a sub-threshold of #1547's 64-file
parallel-dispatch threshold. Parallel dispatch engages at >= 64 files;
pipelining engages at >= 16 files. The rationale: parallel dispatch
allocates per-file signature workers that each saturate a core; once
that's running, pipelining the apply phase only competes with the
dispatch workers if the workload is dispatch-bound. By engaging at a
lower threshold, pipelining covers the apply-bound regime that
parallel dispatch alone does not address.

The thresholds are heuristics. They live as named constants in the
proposed `delta_pipeline::apply_pool` module so a follow-up TODO can
tune them with real benchmark data.

## 11. Wire-compat restatement

After all of the above, the externally observable behaviour against an
upstream peer is unchanged:

- NDX wire-arrival order = NDX ack order (section 2.1).
- Disk commit order = file-list order (section 2.2).
- `--delete-during` directory fence preserved (section 2.3).
- `--delete-after` end-of-transfer fence preserved (section 2.4).
- Stats accounting per-file = file-list order (section 2.5).
- Token stream over the wire is byte-identical: same compression
  context, same NDX framing, same iflags, same xattr handling.
- Golden byte tests (`crates/protocol/tests/golden/`) continue to pass.
- `tools/ci/run_interop.sh` against upstream 3.0.9 / 3.1.3 / 3.4.1
  produces zero new entries in `tools/ci/known_failures.conf`.
- Tcpdump-captured payloads of an oc-rsync push to upstream daemon
  match byte-for-byte with pipelining on vs off.

The pipelining is, by construction, a pure CPU-side optimization. The
wire never sees it.

## 12. Risks and mitigations

### 12.1 Temp-file FD exhaustion

W in-flight files = W temp FDs + W basis FDs + the wire FD + control
FDs. At W=16, that's roughly 32 file-data FDs. Within `RLIMIT_NOFILE`
on every supported target. But concurrent transfers could multiply
this: two pipelined transfers in the same process push the count up.

Mitigations:

- Use `O_TMPFILE` on Linux (already the preferred path in
  `crates/transfer/src/temp_guard.rs`) to avoid persistent dirent
  entries. Tracked under #1011.
- Cap W via the activation threshold and the work-queue capacity.
- For the spill scratch file (when staged delta exceeds budget), share
  a pool of pre-opened scratch FDs across workers rather than
  allocating per-slot.

### 12.2 Basis-file mmap cache thrashing

When all W in-flight files have large (>= 1 MiB) basis files,
`AdaptiveMapStrategy` mmaps each. The kernel page cache must serve the
working set; in worst case, W files x basis-size pages compete for the
cache. On low-memory systems this thrashes.

Mitigations:

- The basis-IO policy (`docs/design/basis-file-io-policy.md`) already
  forces `BufferedMap` (256 KiB sliding window) for io_uring writers,
  side-stepping mmap entirely. We extend the same policy to "tight
  memory budget" via a configurable budget knob.
- Future shared mmap pool: a process-wide LRU of basis mappings, so
  back-to-back accesses to the same basis file (rare but possible)
  reuse the mapping. Out of scope here.

### 12.3 Reorder-buffer head-of-line stall

Worker for file N runs slow (large file, slow disk, expensive checksum
verification). Workers N+1..N+W finish quickly but cannot commit.
Reorder buffer fills. Producer blocks. Throughput drops to N's apply
rate.

Mitigations:

- The bounded window is the bound on memory cost during stall; nothing
  unbounded grows.
- The adaptive capacity policy
  (`crates/engine/src/concurrent_delta/adaptive.rs`) already grows the
  reorder buffer under sustained pressure and shrinks it back. We
  reuse the same policy here.
- The HoL audit (#1883 - reorder-buffer HoL doc) confirmed the
  worst-case wait is bounded by `W * max(per-file apply time)`, which
  is the same bound serial application has on *every* file. We do
  not regress.

### 12.4 Compression-state assumptions

The token reader maintains a single zstd `DCtx` across files. Workers
do not own decompression context; they receive *post-decompression*
delta blobs from the wire reader. This means the wire reader still
performs decompression in its own thread, single-stream. Pipelining
parallelizes apply, not decompression. This is intentional: zstd's
streaming context cannot be split across files without re-initializing
between every file (and that defeats the cross-file dictionary reuse
that makes zstd useful here).

If decompression itself becomes the bottleneck on a future workload, a
separate design note will treat it. Out of scope here.

### 12.5 Failure-cascade test coverage

The property tests added in #2049 cover reorder-buffer drop and abort
paths. We need an additional test class: workers panic / return error
in random orderings, the reorder buffer must still emit the correct
acks in seq order, and the commit thread must not deadlock waiting on
a never-arriving result.

This is one of the follow-up TODOs in section 13.

## 13. Tracking (follow-up TODOs)

Listed here for reference. NOT added to the persistent TODO list - the
implementation team picks these up after this design lands.

1. **Implementation:** wire `MultiFileApplyPool` into
   `run_pipeline_loop_decoupled`, gated by the section-10 thresholds.
   Module placement: `crates/transfer/src/delta_pipeline/apply_pool.rs`.
2. **Threshold tuning:** benchmark the 16-file in-flight and 5 ms RTT
   thresholds against the LAN/WAN/loopback workload matrix; produce a
   tuning report and set the named constants accordingly.
3. **Integration with #1547:** confirm the parallel-dispatch threshold
   (64 files) and the pipelining threshold (16 files) compose
   correctly. Add a regression that exercises both engaged
   simultaneously on a 1000-file LAN transfer.
4. **FD-exhaustion regression test:** simulate `RLIMIT_NOFILE` near the
   pipelining envelope (W=16, two concurrent transfers in the same
   process), confirm graceful degradation and no leaks via
   `TempGuard`.

## 14. Out of scope

- Sender-side pipelining of delta generation. Today the sender already
  pipelines signature read and delta send via the parallel-dispatch
  path; multi-file delta-apply is strictly a receiver concern.
- Cross-transfer pipelining (multiple peers in one process). Not on the
  roadmap.
- Compression context parallelism. See section 12.4.
- Intra-file parallelism. That is #1023's territory.
- Wire-protocol changes. None permitted, ever, by repo policy.

## 15. Cross-references

- `crates/transfer/src/delta_apply/applicator.rs:436` - the unchanged
  `apply_delta_stream` entry point that workers call.
- `crates/transfer/src/delta_apply/mod.rs` - the public surface of the
  applicator module.
- `crates/transfer/src/receiver/transfer/pipeline.rs:38` -
  `run_pipeline_loop_decoupled`, the host of the new pool.
- `crates/transfer/src/reorder_buffer.rs:55-114` -
  `BoundedReorderBuffer`, the in-order delivery primitive.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` - the
  bounded work queue providing producer-side backpressure.
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs:8` -
  `CAPACITY_MULTIPLIER = 2`, the rayon-aware capacity knob.
- `crates/engine/src/concurrent_delta/reorder.rs:30-60` -
  `ReorderBuffer`, the engine-side complement.
- `docs/architecture/reorder-buffer.md` - head-of-line semantics
  formalization (#1883).
- `docs/architecture/delete-during.md` - the delete-during fence
  upstream-faithful semantics.
- `docs/design/basis-file-io-policy.md` - mmap vs `BufferedMap` choice
  for basis files.
- `docs/design/zsync-inspired-matching.md` - hot data-structure sizes
  cited in section 5.
- #1011 - `O_TMPFILE` adoption.
- #1023 - intra-file parallelism (alternative axis, not redundant).
- #1407, #1543, #1547, #1553, #1566, #1650 - the parallel-dispatch and
  reorder-buffer prior art.
- #1883 - head-of-line semantics doc.
- #2049 - reorder buffer drop/abort property tests.
