# Re-ordering buffer for the parallel DeleteEmitter consumer (DEL-1.b)

Status: Design (task DEL-1.b; follows the audit in
`docs/design/del-1a-upstream-ordering-audit.md`; implementation lands as
DEL-2.a behind the `parallel-delete-consumer` feature flag once DEL-3
wire-byte parity proves out)
Audience: engine and transfer maintainers planning the parallel rework of
the `DeleteEmitter` drain.
Scope: a bounded, in-memory re-ordering buffer that lets multiple
producer threads compute per-cohort deletion results in any order while a
single consumer thread emits frames on the ndx and msg channels in strict
upstream-equivalent order. Design only; no source changes in this branch.

Out of scope: the cohort-batching strategy (how producers chunk their
work into cohort-sized inputs - that is DEL-1.c), the wire-byte
regression harness implementation (DEL-3), and any cross-cohort
parallelism between the early and late `write_del_stats` call sites
(upstream only ever fires one of the two per transfer, so there is
nothing to interleave; see DEL-1.a section 4).

## 1. Goal

Move from today's serial DeleteEmitter drain
(`crates/engine/src/delete/emitter/mod.rs:285-302`) to a producer/consumer
pipeline where:

- **N producers** (rayon `par_iter` workers) execute the per-cohort
  deletion-result generation in parallel. Each producer reads its slice
  of the cohort plan, performs the destination-side syscalls
  (`unlink`/`rmdir`/`remove_dir_all`), assembles the `MSG_DELETED` frame
  bodies for the entries it deleted, and accumulates per-kind counters
  for the cohort's `NDX_DEL_STATS` stats frame.
- **1 consumer** thread drains the re-ordering buffer in **strict
  `cohort_idx` order** and emits frames to the writer in the same
  byte sequence the existing single-emitter would produce, so a
  byte-for-byte capture of the wire is identical to the sequential
  baseline.

The consumer is the unique writer of every observable side effect
(single-emitter invariant, preserved from the current design at
`crates/engine/src/delete/emitter/mod.rs:73-79`). The buffer's only job
is to bridge the producer concurrency with the consumer's strict-order
drain without serialising the producers behind a per-cohort lock.

## 2. Buffer shape

The buffer is a small, fixed-capacity ring keyed by **cohort index**,
not by cohort path. The cohort index is assigned by the traversal cursor
in pre-order (see `crates/engine/src/delete/traversal.rs:118-160`
`next_ready`), so it is a dense, monotonically increasing `u32` that
matches the order in which `DirTraversalCursor` would have surfaced the
directory to today's single emitter.

### 2.1 Type sketch

The shape below is illustrative. The eventual implementation lives in a
new `crates/engine/src/delete/reorder.rs`; the type is internal to the
engine crate and is exercised through the existing public
`DeleteEmitter` surface.

```text
struct CohortDeletionBatch {
    cohort_idx: u32,
    msg_deleted_frames: Vec<MsgDeletedFrame>, // already in upstream
                                              // reverse-directory order
                                              // produced by the worker
    cohort_stats: DeleteStats,                // per-kind counts for the
                                              // five NDX_DEL_STATS varints
    io_error: i32,                            // per-cohort IOERR bitmap;
                                              // OR-folded into emitter
                                              // state at drain time
    cohort_records: Vec<CohortDeleteRecord>,  // CohortDeleteRecord per
                                              // successful dispatch when
                                              // a CohortIndex is attached
}

enum SlotState {
    Empty,                  // never written
    InFlight,               // a producer has claimed the slot,
                            //   batch is being assembled
    Filled(CohortDeletionBatch),
}

struct ReorderBuffer {
    slots: [SlotState; N],                  // power-of-two ring
    head:  AtomicU32,                       // next cohort_idx to drain
    tail:  AtomicU32,                       // highest seen + 1
    not_empty: Condvar,                     // wakes the consumer
    not_full:  Condvar,                     // wakes blocked producers
    lock: Mutex<()>,                        // guards slots[] mutation
}
```

The ring uses modulo indexing: `slot_at(idx) == slots[idx % N]`. The
two `Atomic` fields are advisory; the authoritative state transitions
all happen under the slot mutex (see section 3.3 on the synchronisation
trade-off). `head` and `tail` are exposed as atomics so `is_empty()` /
`is_full()` peeks from outside the lock are cheap, but every mutating
path takes the lock first.

### 2.2 Slot lifecycle

```text
        claim_cohort(idx)
   Empty ---------------------> InFlight
                                    |
              produce_batch(idx, b) |
                                    v
                                  Filled(b)
                                    |
                drain (consumer)    |
                                    v
                                  Empty
```

- `claim_cohort(idx)` is called by a producer as soon as it knows it
  owns cohort `idx`. The slot transitions `Empty -> InFlight` and
  publishes `tail = max(tail, idx + 1)`. If `idx >= head + N`, the
  producer blocks on `not_full` until the consumer advances `head`.
- `produce_batch(idx, batch)` is the producer's commit. The slot
  transitions `InFlight -> Filled`, the buffer signals `not_empty`. The
  call MUST follow exactly one matching `claim_cohort(idx)`; double
  publishes are a producer bug and panic the buffer (see section 6.1).
- The consumer's `drain_one()` peeks `slots[head % N]`. If it is
  `Filled`, the buffer takes the batch, transitions the slot to `Empty`,
  advances `head`, and signals `not_full`. If the slot is `Empty` or
  `InFlight`, the consumer blocks on `not_empty` (this is the strict
  cohort-order rule).

### 2.3 Drain rule

The consumer pops in `cohort_idx` order. It blocks if `slot[head]` is
not `Filled`, even when later slots are `Filled` and ready. This is the
behaviour that gives DEL-1.b byte-for-byte parity with the sequential
emitter: a fast producer for cohort 7 cannot leapfrog a slow producer
for cohort 3 on the wire. Skipping is never allowed.

## 3. Concurrency model

### 3.1 Producer side

Producers run as a `rayon::par_iter()` over the **cohort plan list**
(one item per cohort). Each producer:

1. Calls `buffer.claim_cohort(cohort_idx)`. This may block on
   `not_full` if the producer is too far ahead of the consumer.
2. Executes its slice of the deletion plan (one cohort's worth of
   `DeleteEntry` dispatches) on the local thread, using the same
   `DeleteFs` dispatch the sequential emitter uses today
   (`crates/engine/src/delete/emitter/mod.rs:362-396` `run_entry`).
   Each successful syscall appends one `MsgDeletedFrame` to the
   producer-local batch buffer and bumps the local per-kind counters.
3. Calls `buffer.produce_batch(cohort_idx, batch)` when the cohort
   finishes (or fails fatally; see section 6.1).

Crucially, every producer's local batch is **already in upstream's
intra-cohort order** before publication: the producer walks
`plan.extras` in the order DEL-1.a section 5.1 cites as commutative
within a cohort, and upstream's per-cohort `MSG_DELETED` order is the
reverse-directory order `delete_in_dir` issues (`generator.c:272-347`).
Producers therefore do not need to wait for each other to coordinate
the intra-cohort order; the buffer only re-orders cohorts, not the
frames inside a cohort.

### 3.2 Consumer side

Exactly one thread runs the consumer loop:

```text
loop {
    let batch = buffer.drain_one()?;        // blocks until slot[head] == Filled
    for frame in batch.msg_deleted_frames {
        emit_msg_deleted(writer, frame)?;   // MSG_INFO side-channel
    }
    // The per-cohort NDX_DEL_STATS frame for this cohort goes onto
    // the ndx channel as soon as the cohort's MSG_DELETED frames have
    // been flushed. See section 5 for the exact wire fence.
    emitter.fold_batch(batch);              // OR io_error, sum stats,
                                            // append cohort_records
    if buffer.is_empty() && every_producer_finished() {
        emit_ndx_del_stats_if_needed(writer, &emitter.stats)?;
        emit_ndx_done(writer)?;
        break;
    }
}
```

The producer-finished check is supplied by the rayon scope barrier the
caller establishes; the consumer does not need to introspect rayon's
worker pool, only consult a `producers_done: AtomicBool` the scope flips
on completion.

### 3.3 Synchronisation: lockless ArrayQueue vs Mutex+Condvar?

Choice: **`Mutex<()>` + two `Condvar`s** (`not_empty`, `not_full`) over
the slot ring.

Justification:

- `crossbeam_queue::ArrayQueue` is FIFO by push order, not by cohort
  index. We need keyed in-order drain, so a queue cannot service the
  HoL-blocking rule (drain rule, section 2.3) without an external
  scoreboard, which collapses back to a `Mutex`-protected map of cohorts
  pending publication.
- The contention shape is light. With the cap chosen below (N = 64),
  the buffer has at most 64 slots, and the lock is held only for slot
  state transitions and `head`/`tail` updates. Producers' actual work
  (the syscalls and the local frame assembly) happens entirely outside
  the lock.
- The two `Condvar`s give us the natural cohort-order backpressure for
  free: consumers wait on `not_empty` keyed off `head`; producers wait
  on `not_full` keyed off `tail - head < N`.
- A per-slot `Mutex+Condvar` (one lock per slot) is a possible
  refinement but offers no measurable win at N = 64 and complicates the
  drain logic - the consumer would have to take a slot lock to inspect
  state, defeating the cheap-peek property the atomic `head`/`tail`
  fields are designed to give.

The implementation may revisit the lockless approach in a follow-up
once benches show the central mutex is the bottleneck (analogous to the
`DDP-B4` follow-up for `DeletePlanMap`,
`crates/engine/src/delete/plan_map.rs:25-29`). The DEL-2.a baseline
ships with the `Mutex`+`Condvar` ring.

### 3.4 Producer-consumer overlap

The wire-emission step in the consumer (section 3.2) is **not** a
trivial copy: each `MsgDeletedFrame` ultimately becomes a
`send_msg(MSG_DELETED, ...)` call which buffers into `iobuf.msg` and
may flush against the socket. While the consumer is in that emission
loop, the buffer mutex is **not** held; the consumer takes the lock,
swaps `Filled -> Empty`, drops the lock, then iterates the batch. This
keeps the producers writing into `slots[head+1 .. head+N]` in parallel
with the consumer's wire-flush work, which is the whole point of the
re-ordering buffer (the syscalls and the wire flush are the two
expensive pieces of the current single-thread emitter; we want to
overlap them).

## 4. Memory bounds

### 4.1 Worst case

The buffer caps simultaneous in-memory cohorts at **N = 64**. Each
slot holds at most one `CohortDeletionBatch`, sized by:

- `msg_deleted_frames`: one `MsgDeletedFrame` per deletion. Each frame
  carries the path bytes (at most `MAXPATHLEN` ~= 4 KiB on Linux, but
  typically tens of bytes) plus a single byte for kind and a length
  prefix.
- `cohort_stats`: 40 bytes (`DeleteStats` = 5 `u32` plus padding).
- `io_error`: 4 bytes.
- `cohort_records`: at most one per dispatch; populated only when a
  `CohortIndex` is attached.

For an average cohort of ~100 deletions of 64-byte paths, that is on
the order of 8-10 KiB per slot, ~640 KiB peak across the 64 slots. This
is well below the existing reorder buffer's peak (see
`docs/design/streaming-reorder-buffer.md` for the delta reorder's
memory profile, which spans MiB).

### 4.2 Cap selection

N = **64**, matching the existing reorder-buffer default in the delta
pipeline (`crates/engine/src/concurrent_delta/work_queue/capacity.rs`,
`CAPACITY_MULTIPLIER = 2 * rayon::current_num_threads()` clamped at 64
in practice; see memory note `project_reorder_capacity_hard_default.md`
for context). Rationale:

- N = 64 lets a 32-core box keep at least one in-flight cohort per
  worker plus a one-batch overflow per worker before any producer
  blocks. Smaller values starve high-core hosts; larger values inflate
  worst-case memory without measurable throughput gain.
- A small, fixed cap also bounds the wire-divergence blast radius on
  the failure paths in section 6: at most 64 cohorts' worth of frames
  are ever in-flight on the producer side.
- The cap is a compile-time const initially; a runtime flag falls
  outside the DEL-1.b scope and is left to DEL-3 once metrics tell us
  whether per-deployment tuning is warranted.

### 4.3 Backpressure

When `tail - head >= N`, `claim_cohort` blocks on `not_full`. The
consumer's `drain_one()` signals `not_full` after every slot eviction.
This produces a natural pacing loop where fast producers wait for the
consumer to catch up rather than allocating unbounded memory.

The producer-side blocking is acceptable for `--delete-*` because the
deletion phase is not on the critical-path for file-transfer throughput
(deletion runs strictly before or strictly after the per-file transfer
loop; see DEL-1.a section 1 table). Throttling the producers does not
backpressure into the data-transfer pipeline.

### 4.4 What is "a cohort" in this design?

A cohort is **one upstream `write_del_stats` call site's contribution
from one destination parent directory**. DEL-1.a section 4 establishes
that upstream has exactly one `write_del_stats` cohort per transfer
(early xor late), spanning every per-directory delete made during the
sweep. Within that single goodbye-phase cohort, the natural producer
unit is the **per-parent-dir batch** that
`delete_in_dir`/`do_delete_pass` iterate over - that is the same unit
the current `DeleteEmitter`'s `drain_plan` consumes
(`crates/engine/src/delete/emitter/mod.rs:311-333`), and it matches the
boundary at which `DirTraversalCursor` yields paths
(`crates/engine/src/delete/traversal.rs:118-160`).

So DEL-1.b's "cohort" is **per-parent-dir at the producer/consumer
boundary** (one `CohortDeletionBatch` per source directory the
`DirTraversalCursor` yields), and these per-dir batches all fold into
**the single upstream `write_del_stats` cohort** at the goodbye fence.
The single-frame invariant in section 5 talks about the goodbye-phase
cohort; the per-parent-dir cohorts are an internal subdivision the
buffer uses to extract producer parallelism without violating it.

DEL-1.c will refine the batching strategy further (a producer may
combine multiple traversal-yielded directories into one batch when
they are small, to amortise the per-batch overhead). That is out of
scope here; the DEL-1.b buffer treats each cohort as opaque.

## 5. Wire-byte invariants preserved

The buffer's correctness criterion is that the byte sequence the
consumer writes is identical to what the sequential emitter would have
written. The audit's strictest invariant (DEL-1.a section 7) is:

> Per cohort, exactly one `NDX_DEL_STATS` frame, carrying exactly five
> varints (regular-files-by-subtraction, dirs, symlinks, devices,
> specials) must appear on the ndx channel between the last
> `MSG_DELETED` for the cohort and the closing `NDX_DONE` of the
> goodbye phase. Anything before or after that window is recoverable as
> stat divergence; violating either the count or the position breaks
> the goodbye state machine and exits the receiver with
> `RERR_PROTOCOL`.

The DEL-1.b buffer preserves the invariant by construction:

1. **Per-cohort frames are emitted contiguously.** The consumer drains
   one `CohortDeletionBatch` at a time and walks its
   `msg_deleted_frames` in order before touching the next batch. Two
   producers' frames cannot interleave on the wire because the consumer
   is single-threaded and processes one batch end-to-end.

2. **`NDX_DEL_STATS` is emitted exactly once per goodbye cohort,
   carrying exactly five varints.** Per-dir batches accumulate into a
   single emitter-wide `DeleteStats` (`fold_batch` in section 3.2). The
   goodbye-phase emission site
   (`crates/transfer/src/generator/transfer/goodbye.rs:79-110`,
   `should_send_del_stats` + `write_to`) is unchanged: it serialises
   the **folded** stats exactly once, producing exactly five varints
   via `DeleteStats::write_to`
   (`crates/protocol/src/stats/delete.rs:74-107`). The buffer never
   emits the `NDX_DEL_STATS` frame itself; it only delivers the
   per-cohort counter contributions that the goodbye-phase writer
   serialises.

3. **`NDX_DEL_STATS` lands at the correct position.** The consumer
   guarantees every `MSG_DELETED` for every per-dir cohort is on the
   wire before signalling the goodbye-phase that the stats are ready,
   because the consumer drains the buffer fully before returning to
   the caller in `phases.rs`. The closing `NDX_DONE` follows the stats
   frame exactly as today (`goodbye.rs:101`). This satisfies the
   "stats first, then `NDX_DONE`" hard fence (DEL-1.a section 5.2).

4. **Per-kind counter accumulation is complete before the frame is
   written.** DEL-1.a section 5.2 mandates that the regular-file count
   is derived by subtraction (`main.c:231-233`), so any partial fold
   under-counts files. The consumer's `fold_batch` step happens
   inside the consumer loop, **before** the goodbye-phase emission;
   the buffer is fully drained (and every producer has joined) before
   `handle_goodbye` reaches the `should_send_del_stats` check. There is
   no race window.

5. **`DEL_MAKE_ROOM` silence.** Producers do not enqueue
   `MsgDeletedFrame`s for `DEL_MAKE_ROOM` paths (these are the
   destination-clearing deletes from a same-named overwrite; DEL-1.a
   section 1, `delete.c:156-159, 179`). The buffer schema cannot
   represent them, because the producer's per-cohort batch builder
   uses the same `!(flags & DEL_MAKE_ROOM)` guard the current emitter
   uses implicitly (the make-room path runs in a different code path
   that bypasses `DeleteEmitter` entirely; see
   `crates/engine/src/delete/extras.rs`).

6. **ENOTEMPTY recursive fallback stays in the same cohort.** When a
   producer takes the `ENOTEMPTY` recursive-fallback path
   (`crates/engine/src/delete/emitter/mod.rs:468-498` `dispatch_dir`),
   every nested `MSG_DELETED` and every nested stats increment is
   folded into the **same** `CohortDeletionBatch` the producer started
   with. Producers do not spawn sub-producers; recursion stays on the
   same worker. This matches DEL-1.a section 5.2's "cohort identity
   must survive the ENOTEMPTY recursive fallback" requirement.

## 6. Failure modes

### 6.1 Producer panics mid-cohort

If a producer panics after `claim_cohort` but before `produce_batch`,
the slot is stuck in `InFlight` and the consumer will block forever on
`not_empty` once `head` reaches that cohort. Recovery:

- Producers run inside a `rayon::scope` that propagates panics. The
  scope establishes a `panic_guard` (a sentinel that the producer's
  `Drop` impl calls if the panic unwinds past `produce_batch`).
- On panic, the guard transitions the slot `InFlight -> Filled` with an
  **empty** batch and sets a `producer_panicked: AtomicBool` flag.
- The consumer treats an empty batch as a no-op (zero `MSG_DELETED`,
  zero stat increments) and continues draining.
- After the scope joins, the caller checks `producer_panicked` and
  surfaces a fatal `io::Error` to the existing `EmitterErrorPolicy`
  pathway (`crates/engine/src/delete/emitter/policy.rs`). The transfer
  fails with `RERR_PARTIAL` (23), matching upstream's behaviour when
  `delete_item` aborts via `rsyserr + cleanup_and_exit`
  (`delete.c:201-205`).

The buffer itself **does not** carry partial frames from a panicked
producer onto the wire. Wire integrity is the highest priority; a
panicked cohort is lost-on-wire (no `MSG_DELETED` for its entries) but
the cohort counters fold in as zero, so the rest of the transfer's
goodbye state machine stays valid. The receiver sees fewer deletions
than were actually attempted destination-side, which is logged as a
stat-divergence; the transfer's exit code makes the failure visible.

### 6.2 Buffer overflow (slow consumer + fast producers)

Cannot happen: producers block on `not_full` before any allocation
beyond the N-slot ring. The cap is hard; the buffer does not grow.

If the consumer truly deadlocks (e.g. the writer goroutine has stalled
on a wedged socket), producers will pile up against `not_full` until
the transfer-level write timeout
(`crates/transfer/src/timeout.rs`) fires and the surrounding scope
unwinds. The buffer participates passively in the timeout: when the
consumer thread observes the timeout, it sets a `shutdown: AtomicBool`
flag and signals `not_full`; producers blocked on `not_full` wake up,
observe `shutdown`, and return without producing. The transfer exits
with the usual timeout error.

We do **not** drop oldest, crash, or backpressure into the data path.
Drop-oldest would silently under-delete; crash would lose the entire
transfer's goodbye; backpressure into data is not applicable because
deletion runs in a separate phase from data transfer.

### 6.3 Wire-byte regression test

DEL-3 ships a **golden wire capture** test that satisfies the brief's
"regression test that takes the parallel output and compares it
byte-for-byte to a known-good sequential capture":

- A test harness in `crates/engine/tests/delete_wire_parity.rs`
  constructs a synthetic destination tree, runs the **sequential
  emitter** (today's `DeleteEmitter`) against it, captures the writer's
  byte stream (`MSG_INFO` envelopes for `MSG_DELETED` and the ndx
  channel bytes including `NDX_DEL_STATS` and `NDX_DONE`), and stores
  the result as a golden under
  `crates/engine/tests/golden/delete_wire/sequential.bin`.
- The same harness runs the **parallel emitter** (the DEL-2.a
  implementation) against an identical synthetic tree and asserts the
  captured stream equals the golden byte-for-byte.
- Property-test variant: a `proptest` strategy generates random per-dir
  cohort shapes (varying entry counts and kinds, depth, ENOTEMPTY
  recursion presence) and asserts equality for every generated case.
- The test is repeated with `RAYON_NUM_THREADS` set to 1, 2, 4, and
  the host's natural width to catch ordering bugs that only surface
  under specific worker counts.

These tests are added in DEL-3 alongside the feature-flag flip; DEL-2.a
ships the implementation and runs the goldens under the
`parallel-delete-consumer` feature.

## 7. Feature gate

The parallel consumer ships behind a Cargo feature on the `engine`
crate:

```toml
[features]
default = []
parallel-delete-consumer = []
```

While the feature is off (the default for all of DEL-1.b, DEL-2.a, and
DEL-2.b), the existing single-emitter `DeleteEmitter::emit_all` path is
the only one compiled into the build. With the feature on, the new
`ParallelDeleteEmitter` (DEL-2.a) is exposed as a sibling type with the
same public method surface, and the call site in
`crates/transfer/src/receiver/transfer/phases.rs` selects between them
behind a `#[cfg(feature = "parallel-delete-consumer")]` switch.

The feature stays off-by-default until DEL-3 demonstrates wire-byte
parity across the interop matrix (3.0.9, 3.1.3, 3.4.1, 3.4.2). At that
point, DEL-4 promotes the feature to default-on, and the sequential
emitter is retained for one release as a fallback before deletion.

This phased flag matches the policy in
`project_parallel_interop_parity_gap.md` (memory note about
`parallel-receive-delta`): never advertise a parallel path as shipping
until interop captures prove byte-for-byte parity.

## 8. Plug-in point

The current single-emitter call site is in
`crates/transfer/src/receiver/transfer/phases.rs` (the deletion phase
that wraps `delete_extraneous_files` / the
`receiver/directory/deletion.rs` impl). The plug-in change is small.

Today:

```rust
// crates/transfer/src/receiver/directory/deletion.rs:121-291
let per_dir_results: Vec<(DeleteStats, Vec<PathBuf>)> =
    crate::parallel_io::map_blocking(dirs_to_scan, threshold, move |dir| {
        // ... scan, dispatch, accumulate per-dir results
        (stats, deleted_paths)
    });

for (s, deleted_paths) in &per_dir_results {
    combined.fold(s);
    if self.should_emit_itemize() {
        for rel_path in deleted_paths {
            let _ = writer.send_msg_info(format!("*deleting   {}\n", rel_path.display()).as_bytes());
        }
    }
}
```

Sketch under the feature flag:

```rust
#[cfg(feature = "parallel-delete-consumer")]
{
    let buffer = Arc::new(ReorderBuffer::new(REORDER_CAPACITY)); // 64
    let consumer_handle = spawn_consumer_thread(
        Arc::clone(&buffer),
        writer.clone_msg_info_sink(),
    );

    rayon::scope(|s| {
        for (cohort_idx, dir_relative) in dirs_in_traversal_order.into_iter().enumerate() {
            let buffer = Arc::clone(&buffer);
            s.spawn(move |_| {
                buffer.claim_cohort(cohort_idx as u32);
                let batch = produce_cohort_batch(dir_relative, &dir_children, ...);
                buffer.produce_batch(cohort_idx as u32, batch);
            });
        }
    });

    buffer.mark_producers_done();
    let (combined, io_error, limit_exceeded) = consumer_handle.join()?;
    return Ok((combined, limit_exceeded));
}
```

The cohort-idx assignment is done by the receiver-side caller (the only
thing that knows the traversal order), not by the buffer. The consumer
thread owns the writer-side emission of `MSG_INFO` envelopes for
`*deleting` itemize lines, and folds per-cohort stats into the
emitter-wide `DeleteStats` that the goodbye-phase writer reads from in
`crates/transfer/src/generator/transfer/goodbye.rs:79-110`.

Note that the `NDX_DEL_STATS` frame itself is **not** emitted by the
consumer. Per DEL-1.a section 4, the frame is owned by the generator
goodbye path; the consumer only delivers the folded stats so the
goodbye writer has the right numbers when it serialises. This keeps the
ndx-channel writer (a `MonotonicNdxWriter`) untouched by the parallel
consumer, which is what gives the byte-for-byte parity guarantee in
section 5.

## 9. Open questions for DEL-1.c

DEL-1.c covers the **cohort batching strategy**: how producers group
their work into cohort-sized chunks. Questions DEL-1.b deliberately
leaves open:

1. **How many directories belong in a single producer batch?** Today
   the buffer assumes one batch per directory yielded by
   `DirTraversalCursor`. For deep trees with many tiny directories
   (e.g. a vendored `node_modules`), per-dir batches inflate the
   producer-overhead-to-work ratio. DEL-1.c needs to decide whether to
   coalesce successive small directories into a single
   `CohortDeletionBatch` and how to choose the threshold (entry-count
   heuristic? byte-size heuristic? cohort-count target?).

2. **Should batches preserve `cohort_idx` density?** If DEL-1.c
   coalesces, the `cohort_idx -> batch` mapping is no longer 1:1, and
   the buffer's monotonic-`u32` invariant needs to be relaxed (or the
   coalescer needs to re-number). The simplest answer is to keep
   cohort indices monotonic and let the coalescer be responsible for
   skipping no-op slots; the buffer should not learn coalescing.

3. **Interaction with `INC_RECURSE` segment boundaries.** DEL-1.a
   section 4 notes that upstream `delete_during == 2`
   (`--delete-delay`) routes through `remember_delete` and replays at
   end-of-flist, so the segment boundary is not a cohort boundary on
   the wire. DEL-1.c needs to confirm the producer-side batching
   strategy does not accidentally make it one.

4. **Producer-affinity vs work-stealing.** Rayon's default `par_iter`
   uses work-stealing. If we want the per-cohort batches to land in
   `cohort_idx` order on average (to reduce backpressure on the
   `not_full` condvar), we may want a custom split strategy that gives
   each worker a contiguous range of cohort indices. DEL-1.c should
   measure whether work-stealing causes pathological `claim_cohort`
   blocking patterns.

5. **Failure isolation between coalesced cohorts in one batch.** If
   one producer owns three directories and the second one's dispatch
   panics, do we keep the first directory's frames? DEL-1.b's
   per-cohort lifecycle is binary (Empty/InFlight/Filled). A coalesced
   batch is one slot; the per-directory failure boundary needs an
   explicit decision in DEL-1.c.

These are the only inputs DEL-1.b needs from DEL-1.c to land
unmodified. The buffer's interface (`claim_cohort` / `produce_batch` /
`drain_one`) is stable across any reasonable answer to the questions
above.

## 10. Cross-references

- DEL-1.a audit: `docs/design/del-1a-upstream-ordering-audit.md`.
- DDP design (parent context for the single-emitter invariant):
  `docs/design/parallel-deterministic-delete.md`.
- Strict-order gate (the constraint this work eventually retires):
  `docs/design/delete-during-strict-order-gate.md`.
- Current emitter implementation:
  `crates/engine/src/delete/emitter/mod.rs:73-642`.
- Current `DeletePlanMap`:
  `crates/engine/src/delete/plan_map.rs:31-100`.
- Current traversal cursor:
  `crates/engine/src/delete/traversal.rs:48-160`.
- Current goodbye-phase `NDX_DEL_STATS` writer:
  `crates/transfer/src/generator/transfer/goodbye.rs:79-159`.
- Current receiver-side deletion driver:
  `crates/transfer/src/receiver/directory/deletion.rs:52-319`.
- Wire codec for the five-varint frame:
  `crates/protocol/src/stats/delete.rs:74-107`.
- Existing delta-pipeline reorder buffer (prior art for the
  `Mutex+Condvar` ring shape):
  `crates/engine/src/concurrent_delta/` and
  `docs/design/streaming-reorder-buffer.md`.
- Memory note on the existing reorder default:
  `project_reorder_capacity_hard_default.md`.
