# ASY-8.a: Sender tokio prototype design

Status: Design (prototype sketch).
Tracking: #2998.
Companion to: ASY-7.a (receiver tokio prototype, PR #5246), ASY-2
(`tokio-transfer` feature), ASY-3 (per-boundary disposition).

## 1. Scope

Sketch how the sender-side transfer loop
(`crates/transfer/src/generator/transfer/transfer_loop.rs`) migrates to
tokio under the `tokio-transfer` cargo feature. The sender role
corresponds to upstream `sender.c:send_files()` - it reads NDX requests,
receives signatures, generates deltas, and streams tokens over the wire.

Covers:

- File read futures (source file I/O for delta generation and whole-file
  streaming).
- Hash compute scheduling (rolling checksum + strong checksum via the
  `crates/matching` engine).
- Token emission (wire writes of delta ops and whole-file chunks).
- zsync-inspired matching optimizations (ZSO-1..4) interaction with
  async.
- Sender-side INC_RECURSE segment scheduling under tokio.

Out of scope: SSH transport layer (covered by ASY-3 boundary 12),
receiver-side changes (ASY-7.a), and the ASY-12 flip-to-on gate.

## 2. Current sender architecture

The sender runs a synchronous `loop` in `run_transfer_loop`:

```
                       +---------+
                       |  Wire   |
                       |  Reader |
                       +----+----+
                            |  NDX request
                            v
                  +--------------------+
                  |  Transfer Loop     |
                  |  (blocking)        |
                  |                    |
                  |  1. read NDX       |
                  |  2. read iflags    |
                  |  3. read sig_blocks|
                  |  4. open source    |
                  |  5. delta generate |
                  |  6. write tokens   |
                  |  7. write checksum |
                  +--------------------+
                            |
                            v
                       +---------+
                       |  Wire   |
                       |  Writer |
                       +---------+
```

### 2.1 Blocking points

| Step | Operation | Duration | ASY-3 boundary |
|------|-----------|----------|----------------|
| 1-3 | Wire reads (NDX, iflags, signatures) | Network-bound | #1 (`.await`) |
| 4 | `open_source_reader` (file open) | Disk-latency | #3 (`spawn_blocking`) |
| 5 | `generate_delta_from_signature` | CPU-bound | #8 (`spawn_blocking`) |
| 6-7 | Wire writes (tokens, checksum) | Network-bound | #2 (`.await`) |

### 2.2 INC_RECURSE interleaving

The `SegmentScheduler` dispatches sub-file-lists at the top and bottom
of the main loop, throttled by `MIN_FILECNT_LOOKAHEAD`. This
interleaving is synchronous today - segment encoding happens inline
between NDX reads and token writes.

## 3. Async sender task topology

```
+---------------------------------------------------+
|  tokio runtime (owned by core::session)           |
|                                                   |
|  +-----------+     mpsc(128)    +-------------+   |
|  | NDX Reader|  ------------->  | Sender Task |   |
|  | (async)   |                  | (orchestr.) |   |
|  +-----------+                  +------+------+   |
|                                        |          |
|                              +---------+---------+|
|                              |                   ||
|                              v                   v|
|                   +-------------------+  +-------+------+
|                   | spawn_blocking    |  | Token Writer  |
|                   | (file read +      |  | (async wire   |
|                   |  delta generate)  |  |  writes)      |
|                   +-------------------+  +--------------+|
|                                                          |
|  +-------------------+                                   |
|  | Segment Scheduler |  (cooperatively yields between    |
|  | (inline in sender |   segment encodes)                |
|  |  task)            |                                   |
|  +-------------------+                                   |
+----------------------------------------------------------+
```

### 3.1 Task breakdown

| Task | Type | Role |
|------|------|------|
| NDX reader | `tokio::spawn` | Reads NDX + iflags + signatures from wire, sends to sender task via mpsc |
| Sender orchestrator | `tokio::spawn` | Drives the per-file state machine: dispatches file reads, delta generation, token emission |
| File read + delta | `spawn_blocking` per file | Opens source, runs `DeltaGenerator::generate()` or `stream_whole_file_transfer` |
| Token writer | Inline in sender task (`.await` on wire writes) | Streams delta ops and whole-file chunks to the wire writer |
| Segment scheduler | Inline in sender task | Encodes and emits INC_RECURSE sub-lists at throttle boundaries |

### 3.2 Channel strategy (mirrors ASY-7.a receiver design)

Following ASY-7.a's established pattern:

- **NDX channel:** `tokio::sync::mpsc` with capacity 128. Carries
  `NdxRequest { ndx: i32, iflags: ItemFlags, xname: Option<Vec<u8>>,
  sum_head: SumHead, sig_blocks: Vec<WireBlock> }`. The reader task
  pre-parses the full per-file request envelope before sending, so the
  sender task never awaits on wire reads mid-file.
- **Backpressure:** Channel bound (128) limits read-ahead. If the sender
  is slower than the receiver at requesting files, the mpsc fills and
  the reader task parks on `send().await`. This matches upstream's
  implicit backpressure via `perform_io()` select-loop.
- **No SPSC swap needed:** The sender loop is single-consumer; the
  receiver's SPSC-to-mpsc swap (ASY-3 boundaries 6/7) does not apply
  here because the sender has no disk-commit pipeline.

### 3.3 Cancellation

A shared `CancellationToken` (consistent with ASY-7.a) is checked:

1. Between files in the sender task's main loop.
2. Inside `spawn_blocking` closures at file-boundary checkpoints (not
   mid-hash - rolling checksum is atomic per window advance).
3. In the NDX reader task's recv loop.

Cancellation during a `spawn_blocking` delta generation runs to
completion (same contract as ASY-3 boundary #3). The result is discarded
on return if the token is tripped.

## 4. File read futures

### 4.1 Source file open

```rust
async fn open_and_generate_delta(
    source_path: PathBuf,
    file_size: u64,
    config: DeltaGeneratorConfig,
) -> io::Result<DeltaScript> {
    tokio::task::spawn_blocking(move || {
        let source = open_source_reader_sync(&source_path, file_size)?;
        generate_delta_from_signature(source, config)
    })
    .await
    .map_err(join_error_to_io)?
}
```

Rationale: file opens may trigger page-in, fadvise, or NFS round-trips.
`tokio::fs::File` adds no value here because we immediately pass the fd
to the delta generator which reads synchronously via `Read` trait.
Keeping the open inside the same `spawn_blocking` as the generate avoids
an extra thread hop.

### 4.2 Whole-file streaming path

For files without a basis (whole-file transfer), the sender reads the
source and writes tokens to the wire in a streaming loop. Under tokio:

```rust
async fn stream_whole_file_async(
    source_path: PathBuf,
    file_size: u64,
    checksum_algorithm: ChecksumAlgorithm,
    compression: Option<CompressionAlgorithm>,
    writer: &mut AsyncWriter,
) -> io::Result<StreamResult> {
    // Read source in blocking context, produce chunks via channel
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Bytes>(4);

    let read_handle = tokio::task::spawn_blocking(move || {
        let mut source = open_source_reader_sync(&source_path, file_size)?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut verifier = ChecksumVerifier::for_algorithm(checksum_algorithm);
        loop {
            let n = source.read(&mut buf)?;
            if n == 0 { break; }
            verifier.update(&buf[..n]);
            if chunk_tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                break; // writer dropped - cancelled
            }
        }
        Ok(verifier.finalize())
    });

    // Async wire writes overlap with blocking reads
    while let Some(chunk) = chunk_rx.recv().await {
        write_token_stream_async(writer, &chunk, compression).await?;
    }
    write_token_end_async(writer).await?;

    let checksum = read_handle.await.map_err(join_error_to_io)??;
    writer.write_all(&checksum).await?;
    Ok(StreamResult { ... })
}
```

This pipelining overlaps disk reads with network writes - a win when
disk and network are independent I/O paths. The 4-slot channel limits
memory to ~1 MB per file while hiding read latency.

### 4.3 Delta path: two-phase approach

For delta transfers, the current code generates the full `DeltaScript`
in memory before writing it to the wire. Under tokio the generation
stays inside `spawn_blocking` (CPU-bound), and the wire write phase
becomes async:

```rust
let delta_script = open_and_generate_delta(path, size, config).await?;
let wire_ops = script_to_wire_delta(delta_script, block_length);
write_delta_with_compression_async(writer, &wire_ops, encoder).await?;
writer.write_all(&checksum_buf[..checksum_len]).await?;
```

The two-phase approach (blocking generate, then async write) is simpler
than streaming tokens from within `spawn_blocking` and is consistent
with ASY-7.a's pattern of keeping CPU-heavy engine code synchronous
behind `spawn_blocking` boundaries.

**Future optimization (not in prototype):** For very large files where
the `DeltaScript` vector is large, a streaming variant could produce
tokens via an mpsc channel from within `spawn_blocking`, matching the
whole-file pattern above. This is deferred until benchmarks demonstrate
that the two-phase approach is memory-constrained.

## 5. Hash compute scheduling

### 5.1 Rolling checksum (hot path)

The rolling checksum (`RollingChecksum::roll()` with SIMD dispatch) runs
byte-by-byte over the source file inside `DeltaGenerator::generate()`.
This is inherently sequential and CPU-bound.

**Disposition:** Stays inside `spawn_blocking`. No benefit from async
here - the rolling window cannot yield between bytes, and the SIMD path
(AVX2/SSE2/NEON) expects a tight loop without context switches.

### 5.2 Strong checksum verification

When the rolling checksum hits a tag-table match, the strong checksum
(MD4/MD5/XXH3) verifies the match. This is a short computation (one
block, typically 700-32768 bytes) that cannot be usefully parallelized
per-match. It stays inline in the generate loop.

### 5.3 Whole-file checksum

The sender computes a whole-file checksum appended after the token
stream. For the delta path, this currently requires a second pass over
the source file (`compute_file_checksum`). Under tokio:

- **Whole-file path:** The `ChecksumVerifier` runs inline during the
  single streaming read pass (already implemented in the current sync
  code). No change needed - the blocking read task computes it as it
  reads.
- **Delta path:** The second-pass file read for `compute_file_checksum`
  moves into the same `spawn_blocking` as delta generation, eliminating
  a separate blocking island. The `DeltaGenerator` could be extended to
  accumulate the whole-file hash during its single scan pass, avoiding
  the second open entirely. This optimization is orthogonal to async but
  becomes more valuable when each `spawn_blocking` has scheduling cost.

### 5.4 Rayon bridge

Per `docs/design/tokio-spawn-blocking-rayon.md`, if future work
parallelizes per-file delta generation across multiple files (e.g.,
batching N files into a rayon `par_iter`), the bridge pattern applies:

```rust
let results = rayon_bridge(SENDER_BATCH_THRESHOLD, file_count, || {
    files.par_iter().map(|f| generate_delta(f)).collect()
}).await?;
```

The current sender processes files one at a time (matching upstream's
serial `send_files()` loop), so rayon is not used on the sender hot
path today. The bridge is reserved for a potential multi-file batching
optimization under the `parallel-send` feature (not in this prototype).

## 6. Token emission

### 6.1 Wire write disposition

All wire writes (NDX echo, iflags, delta tokens, whole-file chunks,
end-of-file checksum) become `.await` on `AsyncWrite`. This matches
ASY-3 boundary #2 - socket/stdio writes are trivially pollable.

### 6.2 Flush discipline

Upstream rsync flushes buffered output before blocking on the next NDX
read (`perform_io()` interleaves reads and writes via `select()`). Under
tokio:

```rust
// Top of loop, before reading next NDX from channel
writer.flush().await?;
```

The flush-before-read invariant is preserved by construction: the sender
task awaits on `chunk_rx.recv()` only after flushing, and the NDX mpsc
receive is preceded by a flush. This matches ASY-3's "flush-before-block"
defended invariant (section 3, row 7).

### 6.3 Compression encoder state

The `CompressedTokenEncoder` (zlib/zstd session context) is per-transfer
and stateful. It stays owned by the sender task - never shared across
`spawn_blocking` boundaries. Token writes flow through it synchronously
within the async task (the encoder operates on in-memory buffers, not
I/O).

## 7. ZSO-1..4 interaction with async

The zsync-inspired optimizations live entirely within
`DeltaGenerator::generate()` in `crates/matching/src/generator.rs`.
They are called from a `spawn_blocking` island and are unaffected by the
async boundary. Analysis per optimization:

### ZSO-1: Bithash prefilter

The `BitHash` (`crates/matching/src/index/bithash.rs`) is a per-file
bit-array that rejects ~7/8 of rolling-hash false positives before the
hash-table probe. It is:

- Constructed once per file (inside `DeltaSignatureIndex::from_signature`).
- Probed on every rolling-hash advance (hot inner loop).
- Thread-local to the `spawn_blocking` closure. No cross-task sharing.

**Async interaction:** None. The bithash lives and dies within a single
`spawn_blocking` invocation. No `Send` bound issues.

### ZSO-2: Sequential-match lookahead (seq-match)

The `want_i` hint and `flush_seq_match_run` coalescing logic
(`generator.rs:166,289`) predict the next matching block by following the
`DeltaSignatureIndex::next_match` chain. This optimization:

- Operates entirely within the byte-by-byte scan loop.
- Maintains mutable state (`want_i`, `run_start_idx`, `run_len`) that is
  local to the closure.
- Produces coalesced `DeltaToken::Copy` tokens that the wire layer
  expands back to per-block ops.

**Async interaction:** None. The seq-match state is local to the
`spawn_blocking` closure. The coalesced tokens are identical on the wire
(expansion happens in `script_to_wire_delta`), so async write ordering is
preserved.

### ZSO-3: Matched-block pruning

`MatchedBlocks` and `DeltaSignatureIndex::mark_consumed()` prune
already-matched basis blocks from future probes. Two layers:

- Per-session `MatchedBlocks` bitmap (stack-local, no sharing).
- Shared `consumed` `AtomicU64` array on the index (interior mutability
  for concurrent generators in `concurrent_delta`).

**Async interaction:** The per-session bitmap is `spawn_blocking`-local.
The shared atomic consumed array uses `Ordering::Relaxed` stores and is
safe across threads by construction. If the sender processes files
serially (one `spawn_blocking` at a time), no cross-task coordination is
needed. If a future `parallel-send` batches multiple files against the
same index, the atomic consumed array provides the coordination.

### ZSO-4: Compact key lookup

The `CompactLookup` (`crates/matching/src/index/compact_lookup.rs`)
addresses hash-table buckets using only `sum2` (upper 16 bits of the
rolling sum), with `sum1` as an in-bucket discriminator. This is a
data-structure optimization with no I/O or synchronization concerns.

**Async interaction:** None. Pure computation within `spawn_blocking`.

### Summary

All four ZSO optimizations are encapsulated within the synchronous
`DeltaGenerator::generate()` call, which runs inside `spawn_blocking`.
The async boundary does not cross into the matching engine. No changes
to the ZSO implementation are required for the tokio migration.

## 8. INC_RECURSE segment scheduling under tokio

### 8.1 Current model

The `SegmentScheduler` yields pending sub-file-lists when the
`remaining` count drops below `MIN_FILECNT_LOOKAHEAD`. Segment encoding
(`encode_and_send_segment`) writes to the same wire writer as token
data, interleaved at the top and bottom of the main loop.

### 8.2 Async model

Segment scheduling stays inline in the sender task (not a separate
spawned task). Rationale:

1. Segment encodes are wire-writes (async-compatible, `.await`).
2. The throttling heuristic depends on `dispatched_entry_count` and
   `files_transferred` - state owned by the sender task.
3. Upstream's interleaving order (segments at top/bottom of loop) is a
   protocol invariant that must be preserved.

```rust
// Top of loop - before reading next NDX
if inc_recurse {
    let remaining = dispatched_entry_count.saturating_sub(files_transferred);
    while let Some(seg) = scheduler.next_if_needed(remaining) {
        self.encode_and_send_segment_async(&mut writer, seg, ...).await?;
        segments_sent += 1;
        flist_done_remaining += 1;
        dispatched_entry_count += seg.count;
    }
    if !self.incremental.flist_eof_sent && scheduler.is_exhausted() {
        self.send_flist_eof_async(&mut writer, ...).await?;
    }
}
writer.flush().await?;

// Read next NDX from channel (not from wire directly)
let request = ndx_rx.recv().await.ok_or_else(|| {
    io::Error::new(io::ErrorKind::UnexpectedEof, "NDX reader closed")
})?;
```

### 8.3 NDX_FLIST_EOF timing

The `NDX_FLIST_EOF` sentinel must be sent after all segments are
dispatched but before the final `NDX_DONE` echo. Under async, the
sender task observes `scheduler.is_exhausted()` at the same points as
today (top of loop, after exhausting remaining segments). The
sequentiality guarantee holds because segment sends and NDX reads are
serialized in the same task - there is no reordering risk from
concurrency.

### 8.4 Flist-done echo path

When INC_RECURSE is active, the receiver sends one `NDX_DONE` per
completed sub-file-list. The NDX reader task forwards these as control
messages through the mpsc channel (tagged enum variant), and the sender
task echoes them without incrementing phase. This preserves upstream's
`flist_free(first_flist)` loop.

## 9. `spawn_blocking` pool sizing

Following ASY-3's recommendation
(`TOKIO_TRANSFER_BLOCKING_THREADS >= max_connections * 4`):

- **Sender-side budget per connection:** 1 long-lived slot for the
  per-file delta generation loop. Unlike the receiver's disk-commit task
  (ASY-3 boundary #9), the sender's blocking work is per-file, not
  per-connection-lifetime. Each file's `spawn_blocking` is short-lived
  (duration of delta generation or source read).
- **Concurrency:** At most 1 outstanding `spawn_blocking` per sender
  connection (files processed serially). The pool is shared with
  receiver-side boundaries #3, #8, #9, #10.
- **Oversubscription guard:** The sender's `spawn_blocking` closures
  do not call rayon. No double-pool risk on the sender hot path.

## 10. Error handling and wire-byte parity

### 10.1 JoinError mapping

```rust
fn join_error_to_io(e: tokio::task::JoinError) -> io::Error {
    if e.is_panic() {
        io::Error::new(
            io::ErrorKind::Other,
            format!("sender delta task panicked {}{}", error_location!(), sender()),
        )
    } else {
        io::Error::new(io::ErrorKind::Interrupted, "sender task cancelled")
    }
}
```

### 10.2 Wire-byte parity contract

The async sender MUST produce identical wire bytes as the sync sender
for any given (file list, source data, negotiated options) tuple. This
is enforced by:

1. `crates/protocol/tests/golden/` byte-comparison tests run under both
   `tokio-transfer` on and off.
2. The `DeltaGenerator` is unchanged - same algorithm, same token
   output.
3. `script_to_wire_delta` is unchanged - same expansion of coalesced
   tokens.
4. Compression encoder state is per-session, same initialization order.
5. INC_RECURSE segment encoding is unchanged - same flist wire format.

### 10.3 Open failure recovery

`MSG_NO_SEND` for source files that cannot be opened is emitted from the
sender task (not from `spawn_blocking`). The sender task catches the
`spawn_blocking` result and, on open failure, writes the `MSG_NO_SEND`
frame and continues to the next NDX - matching upstream
`sender.c:354-369`.

## 11. Prototype implementation plan

### Phase 1: Async sender task skeleton

1. Add `async fn run_transfer_loop_async` behind
   `#[cfg(feature = "tokio-transfer")]` in
   `crates/transfer/src/generator/transfer/transfer_loop.rs`.
2. Wire NDX reader as a separate `tokio::spawn` task with mpsc channel.
3. Replace wire writes with `.await` on `AsyncWriter`.
4. Keep `spawn_blocking` for file opens and delta generation.

### Phase 2: INC_RECURSE async encoding

5. Convert `encode_and_send_segment` to async (wire writes only).
6. Preserve throttle heuristic (inline in sender task).
7. Test: golden-byte parity with INC_RECURSE on 100+ directory tree.

### Phase 3: Whole-file streaming pipeline

8. Implement the read-channel-write pipeline for whole-file transfers.
9. Benchmark: overlap disk read with network write on 1 GB file.
10. Verify checksum correctness (verifier runs in blocking reader).

### Phase 4: Validation

11. Golden-byte parity: `crates/protocol/tests/golden/` unchanged.
12. Interop: `tools/ci/run_interop.sh` green on 3.0.9/3.1.3/3.4.1/3.4.2.
13. Benchmark: sender throughput on `rsync-profile` 100k-file corpus,
    measure against sync baseline (target: >= parity, not regression).

## 12. Consistency with ASY-7.a (receiver design)

| Concern | ASY-7.a (receiver) | ASY-8.a (sender) | Rationale |
|---------|-------------------|-------------------|-----------|
| Channel type | `tokio::sync::mpsc` | `tokio::sync::mpsc` | Uniform async channel, `send().await` parks on full |
| Channel capacity | 128 | 128 | Same backpressure ceiling |
| `spawn_blocking` use | Disk commit (long-lived) + basis load (per-batch) | Delta generation (per-file) | Sender has no persistent disk task |
| Cancellation | `CancellationToken` shared across tasks | Same | Cooperative, checked between files |
| Wire-byte parity | Golden tests + interop | Same | Non-negotiable |
| Rayon bridge | Not needed (receiver disk path is sequential) | Not needed (sender is serial per-file) | Both defer rayon-async bridging to parallel variants |
| Compression state | N/A (receiver decompresses) | Per-session encoder in sender task | Single-owner, no cross-task sharing |
| INC_RECURSE | Receiver observes segments | Sender drives segment dispatch | Segment scheduling inline in main task (both sides) |

## 13. Risks and mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| `spawn_blocking` scheduling latency adds per-file overhead | Sender throughput regression on many-small-files | Batch multiple small files into one `spawn_blocking` (threshold: files < 4 KB get batched) |
| Token write ordering violated by concurrent tasks | Wire desync, interop failure | Single sender task serializes all wire writes; no concurrent writers |
| Compression encoder state corruption from task migration | Silent data corruption | Encoder owned by sender task, never crosses `spawn_blocking` boundary |
| INC_RECURSE segment timing shifted by async scheduling | Receiver starvation or premature NDX_FLIST_EOF | Segment dispatch is inline in sender task at same loop positions as sync path |
| `DeltaScript` memory spike on large files | OOM under concurrent connections | Per-connection memory budget; stream variant for files > 256 MB (future) |

## 14. Decision record

This design follows Option A (Adopt) from ASY-6, restricted to the
sender side. The sender's single-file serial loop maps cleanly to a
single async task with `spawn_blocking` islands for CPU work.
Implementation is behind `tokio-transfer` (default off) and does not
affect the sync path.

The prototype validates the sender-side boundary dispositions from ASY-3
(#1, #2, #3, #8) before committing to the full 12-boundary migration.
If the prototype shows regression on the sender-specific benchmark
(many-small-files throughput), the `spawn_blocking` batching mitigation
(section 13, row 1) is the first lever before reconsidering the approach.
