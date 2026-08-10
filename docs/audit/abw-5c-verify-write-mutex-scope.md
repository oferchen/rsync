# ABW-5.c: apply_batch_parallel verify-vs-write Mutex scope analysis

Tracking: ABW-5.c
Status: Complete
Date: 2026-05-26

## Summary

This audit determines whether the per-file Mutex scope in
`apply_batch_parallel` permits a data race between the verify step of
batch N+1 and the write step of batch N for the same file. The answer
is **no** - the current design is safe by sequential batch dispatch.

---

## 1. Lock acquisition timeline

### 1.1 apply_batch_parallel write path (batch.rs:45-71)

```
apply_batch_parallel(chunks: Vec<DeltaChunk>)
  |
  |-- Phase 1: parallel verify (rayon par_iter) ---------+
  |   chunks.into_par_iter()                             |
  |     .map(|chunk| verify_chunk(strategy, chunk))      |
  |     .collect::<Result<Vec<VerifiedChunk>, _>>()      |
  |   (no locks held; pure CPU; owned data only)         |
  +------------------------------------------------------+
  |   BARRIER: rayon collect() joins all workers
  |
  |-- Phase 2: serial write loop (calling thread) -------+
  |   for v in verified {                                |
  |     handle = slot_for(ndx)       // DashMap get+clone|
  |     slot = handle.lock_slot()    // Mutex::lock()    |
  |     slot.ingest(v.chunk)         // reorder + write  |
  |     drop(slot)                   // MutexGuard drop  |
  |     drop(handle)                 // SlotHandle drop  |
  |   }                                                  |
  +------------------------------------------------------+
  |
  RETURN Ok(())
```

### 1.2 Per-chunk write detail (mod.rs:255-274)

Within `FileSlot::ingest`, the MutexGuard is held for:

```
lock_slot()          -- Mutex<FileSlot>::lock()
  ingest(chunk)
    reorder.insert() -- O(1) ring-buffer insert
    reorder.drain_ready().collect()
    for chunk in ready:
      write_chunk()
        writer.write_all(&chunk.data)   -- I/O
        bytes_written += data.len()
drop(MutexGuard)     -- release
```

The lock covers the reorder-buffer bookkeeping and the actual
`write_all` I/O. There is no seek operation - the writer is a
trait-object `Box<dyn Write + Send>` with append semantics. The file
offset is implicit in the writer's internal state and is protected by
the Mutex because it is a field of `FileSlot`.

### 1.3 apply_one_chunk write path (mod.rs:552-570)

Identical lock discipline but for a single chunk:

```
apply_one_chunk(chunk)
  handle = slot_for(ndx)
  rayon::join(|| verify_chunk(strategy, chunk), || ())
  slot = handle.lock_slot()    // Mutex::lock()
  slot.ingest(verified.chunk)  // reorder + write
  drop(slot)                   // MutexGuard drop
  drop(handle)                 // SlotHandle drop
```

---

## 2. Verify-write overlap analysis

### 2.1 Batch dispatch is synchronous

`apply_batch_parallel` is a synchronous function. The caller submits
one `Vec<DeltaChunk>`, waits for `collect()` to join the parallel
verify, runs the serial write loop, and only then returns. There is no
asynchronous dispatch, no background thread, no channel. The function
signature returns `io::Result<()>` - the caller blocks until the
entire batch is committed.

This means **batch N must complete before batch N+1 can begin** at any
single call site. The caller cannot invoke `apply_batch_parallel`
concurrently for the same applier instance without external
synchronisation, because the write loop takes `&self` and accesses the
per-file `DashMap` and `Mutex` through shared references.

### 2.2 Can two batches execute concurrently?

Yes, theoretically. `ParallelDeltaApplier` is `Send + Sync` (by
construction - `DashMap` and `Arc<dyn ChecksumStrategy>` are both
`Send + Sync`). Two threads could call `apply_batch_parallel`
simultaneously on the same applier.

However, the current code has no call site that does this. The only
caller pattern is the sequential receiver loop, which submits one
batch at a time. Even if two threads did call it concurrently:

- **Verify steps**: read only `chunk.data` (owned) and the shared
  `strategy` (immutable trait object behind `Arc`). No per-file state
  is accessed. Safe.
- **Write steps**: each chunk's write acquires the per-file Mutex via
  `lock_slot()`. Two threads writing to the same file F would
  serialise on `Mutex<FileSlot>`. The reorder buffer inside the slot
  handles out-of-order `chunk_sequence` values, so writes arrive in
  the correct byte order regardless of which thread submits first.

### 2.3 Can verify of batch N+1 read stale data from file F?

No. The verify step (`verify_chunk`) operates exclusively on:

1. `chunk.data` - owned `Vec<u8>` passed by value into the closure.
2. `strategy` - immutable `Arc<dyn ChecksumStrategy>`.
3. `chunk.expected_strong` - optional `ChecksumDigest` attached to the
   chunk at construction time.

The verify step never reads the destination file, never reads
`FileSlot`, and never acquires the per-file Mutex. It computes a
strong checksum of the chunk's in-memory data and compares it against
the expected digest (if present). It does not touch the filesystem or
any shared mutable state.

Therefore, there is no data dependency between verify(batch N+1) and
write(batch N). They cannot race because they access disjoint data.

---

## 3. File offset state protection

### 3.1 Writer state is inside the Mutex

`FileSlot` contains three fields (mod.rs:234-238):

```rust
struct FileSlot {
    writer: Box<dyn Write + Send>,
    reorder: ReorderBuffer<DeltaChunk>,
    bytes_written: u64,
}
```

All three are behind `Mutex<FileSlot>` inside `SlotData` (slot_barrier.rs:261-263):

```rust
pub(super) struct SlotData {
    slot: Mutex<FileSlot>,
}
```

The writer's internal offset, the reorder buffer's next-expected
sequence number, and the cumulative bytes-written counter are all
protected by the same Mutex. A thread holding the MutexGuard has
exclusive access to all three.

### 3.2 Concurrent writes to the same file

If two threads execute the write loop of `apply_batch_parallel`
concurrently for chunks belonging to the same file, they serialise on
`Mutex<FileSlot>`. The second thread blocks at `lock_slot()` until
the first thread drops its MutexGuard. The reorder buffer inside the
slot handles interleaved `chunk_sequence` values correctly: if thread
A holds chunk 5 and thread B holds chunk 4, thread A's insert buffers
chunk 5 in the reorder ring; when thread B subsequently inserts
chunk 4, the reorder buffer drains both 4 and 5 to the writer in
order.

### 3.3 No external file handle aliasing

The writer is a `Box<dyn Write + Send>` moved into `FileSlot` at
`register_file` time. No external reference to the writer exists after
registration. The only path to the writer is through the per-file
Mutex, so no alias can bypass the lock.

---

## 4. Data race window diagram

The worst case is two concurrent `apply_batch_parallel` calls from
different threads, both containing chunks for file F. Thread T1 has
batch N (chunks 0-3), thread T2 has batch N+1 (chunks 4-7).

```
Time -->

T1 (batch N)           T2 (batch N+1)          File F Mutex
|                      |                        |
|-- par_iter verify    |-- par_iter verify      | (unlocked)
|   chunk 0: hash      |   chunk 4: hash        |
|   chunk 1: hash      |   chunk 5: hash        |
|   chunk 2: hash      |   chunk 6: hash        |
|   chunk 3: hash      |   chunk 7: hash        |
|-- collect() join     |-- collect() join       |
|                      |                        |
|-- write loop -----   |-- write loop -----     |
|   lock(F)         -->|   lock(F)              | LOCKED by T1
|   ingest(chunk 0)    |   (blocked)            |
|   ingest(chunk 1)    |   (blocked)            |
|   ingest(chunk 2)    |   (blocked)            |
|   ingest(chunk 3)    |   (blocked)            |
|   unlock(F)       -->|                        | UNLOCKED
|                      |   (acquires lock)       | LOCKED by T2
|                      |   ingest(chunk 4)       |
|                      |   ingest(chunk 5)       |
|                      |   ingest(chunk 6)       |
|                      |   ingest(chunk 7)       |
|                      |   unlock(F)             | UNLOCKED
|                      |                        |
|   RETURN Ok(())      |   RETURN Ok(())        |
```

In this worst case:

- Verify steps for both batches run concurrently with zero contention
  (they touch only owned data).
- Write steps serialise on the per-file Mutex.
- The reorder buffer inside `FileSlot` handles the arrival order:
  if T2 wins the lock first and ingests chunks 4-7 before T1 ingests
  0-3, the reorder buffer queues 4-7 and waits for 0 before draining.
  When T1 subsequently ingests 0-3, the buffer drains 0-7 in order.
- The bytes reach the writer in strict `chunk_sequence` order
  regardless of which thread wins the Mutex first.

**No data race exists.** The verify step and write step access disjoint
state.

---

## 5. Safety verdict

**Safe.** The current design has no data race between verify and write
operations across batches.

The safety rests on three invariants:

1. **Verify is pure.** `verify_chunk` reads only owned `chunk.data`
   and the immutable shared `strategy`. It never reads the destination
   file, never touches `FileSlot`, and never acquires the per-file
   Mutex.

2. **Write is Mutex-guarded.** Every write to a file's destination
   writer goes through `lock_slot()` on the per-file
   `Mutex<FileSlot>`. The Mutex protects the writer, the reorder
   buffer, and the bytes-written counter as an atomic unit.

3. **Reorder buffer restores sequence order.** Even when chunks arrive
   at the Mutex out of `chunk_sequence` order (from concurrent batch
   calls), the per-file `ReorderBuffer` inside `FileSlot` holds
   out-of-order chunks and only drains a contiguous run starting from
   the next expected sequence number.

The design is safe under both the current single-caller sequential
dispatch pattern and the hypothetical concurrent multi-caller pattern.

---

## 6. Recommendations

No fix is needed. The design is safe.

### Invariants that must be preserved

1. **verify_chunk must remain stateless.** If a future change makes
   `verify_chunk` read from the destination file (e.g. for
   incremental checksum verification against already-written data),
   the verify step would need to acquire the per-file Mutex, and the
   "verify is pure" invariant would break. Any such change must be
   accompanied by a lock-ordering analysis.

2. **FileSlot must remain behind a single Mutex.** Splitting the
   writer and the reorder buffer into separate locks would introduce a
   window where the reorder buffer could drain a chunk to the writer
   while another thread is mid-write. The current single-Mutex design
   avoids this class of bug entirely.

3. **ReorderBuffer must be inside the Mutex.** Moving the reorder
   buffer outside the Mutex (e.g. into a per-file `DashMap` entry)
   would allow two threads to race on `insert` + `drain_ready`,
   producing duplicate or dropped writes.

### Performance note

The per-file Mutex in the write loop means that chunks for the same
file serialise even when they come from different batches. This is the
documented "writes are serial after parallel verify" shape
(project memory: `apply_batch_write_serial`). A future pipelined
design that overlaps write batch N with verify batch N+1 would not
violate the safety invariants above, because verify never touches the
per-file state. However, it would require the write loop to run on a
dedicated thread rather than the calling thread, which changes the
error propagation and backpressure model.
