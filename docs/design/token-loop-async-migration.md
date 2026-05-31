# ASY-10.a: token_loop async migration design

Status: Design. Part of the ASY-7..12 implementation series defined by
ASY-3 (`docs/design/asy-3-async-boundary-spec.md`) and deferred by ASY-6
(`docs/design/asy-6-adopt-or-defer-decision.md`). This document sketches
the migration of `crates/transfer/src/transfer_ops/token_loop.rs` from
synchronous blocking I/O to a tokio-native async implementation.

Scope: state-machine vs handcrafted-future approach selection, error
propagation semantics, drain-on-error semantics, channel strategy
alignment with ASY-3 boundary contracts.

Out of scope: ASY-9 native tokio-uring integration, ASY-12 feature
gate flip, benchmark validation (ASY-4).

## 1. Current architecture

The token_loop is the innermost receiver hot path. It reads delta tokens
from the wire (via `ServerReader<R: Read>`) and dispatches them as
`FileMessage` chunks to the disk commit thread through a lock-free SPSC
channel (`pipeline::spsc`). Per-file flow:

```text
Wire (ServerReader)
    |
    v
TokenReader::read_token()  -- plain 4-byte LE or compressed DEFLATE
    |
    v
process_remaining_tokens() -- main loop
    |
    +---> DeltaToken::Literal -> literal_to_buf() -> FileMessage::Chunk
    +---> DeltaToken::BlockRef -> MapFile::map_ptr() -> FileMessage::Chunk
    +---> DeltaToken::End -> read expected checksum -> FileMessage::Commit
    |
    v
spsc::Sender<FileMessage> -------> disk commit thread
```

Key synchronous dependencies:

- `reader.read_exact()` - blocking wire read (ASY-3 boundary #4)
- `file_tx.send()` - spin-wait SPSC send (ASY-3 boundary #6)
- `buf_return_rx.try_recv()` - non-blocking buffer recycle (ASY-3 boundary #7)
- `basis_map.map_ptr()` - mmap page fault (ASY-3 boundary #3, spawn_blocking)

The parallel delta path (`ParallelDeltaPipeline`) operates at a higher
level - it dispatches entire files to rayon workers via a bounded
`WorkQueueSender`. The token_loop runs inside each worker's per-file
processing, so its async migration is orthogonal to the parallel delta
dispatch strategy.

## 2. Approach: state-machine vs handcrafted future

### 2.1 State-machine (selected)

Express the token loop as an `async fn` compiled by rustc into an
implicit state machine. Each `.await` point becomes a suspension
boundary:

```rust
async fn process_remaining_tokens(
    reader: &mut AsyncServerReader,
    file_tx: &mpsc::Sender<FileMessage>,
    buf_return_rx: &mut mpsc::Receiver<Vec<u8>>,
    checksum_verifier: &mut ChecksumVerifier,
    signature: &Option<FileSignature>,
    basis_map: &mut Option<MapFile>,
    mut total_bytes: u64,
    pending_delta: Option<DeltaToken>,
    token_reader: &mut TokenReader,
    initial_literal_bytes: u64,
) -> io::Result<StreamingResult> {
    // ...
    loop {
        let delta = match next_delta.take() {
            Some(d) => d,
            None => token_reader.read_token_async(reader).await?,
        };

        match delta {
            DeltaToken::End => {
                let mut expected_checksum = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                reader.read_exact(&mut expected_checksum[..checksum_len]).await?;
                file_tx.send(FileMessage::Commit).await.map_err(/* ... */)?;
                return Ok(StreamingResult { /* ... */ });
            }
            DeltaToken::Literal(literal_data) => {
                let buf = literal_to_buf_async(literal_data, reader, buf_return_rx).await?;
                file_tx.send(FileMessage::Chunk(buf)).await.map_err(/* ... */)?;
                // ...
            }
            DeltaToken::BlockRef(block_idx) => {
                // basis_map.map_ptr() is a potential page fault - see section 2.3
                let block_data = blocking_io(|| basis_map.map_ptr(offset, len)).await?;
                file_tx.send(FileMessage::Chunk(buf)).await.map_err(/* ... */)?;
                // ...
            }
        }
    }
}
```

Advantages:

- Minimal cognitive overhead - the structure mirrors the sync version.
- Rustc-generated state machine is well-optimized (enum discriminant, no heap alloc per state transition).
- Natural `?` error propagation works across `.await` boundaries.
- Easy to add cancellation via `tokio::select!` at each iteration.

### 2.2 Handcrafted future (rejected)

Implement `Future` manually with an explicit `enum TokenLoopState`:

```rust
enum TokenLoopState {
    ReadingToken,
    ReadingLiteral { remaining: usize },
    ReadingBlockRef { block_idx: usize },
    ReadingChecksum { read_so_far: usize },
    SendingChunk { buf: Vec<u8> },
    SendingCommit,
    Done,
}
```

Advantages:

- Total control over suspension points.
- Can avoid pinning large state when only a subset is live.

Disadvantages:

- High implementation complexity (manual `Pin` management, unsafe self-referential borrows for `basis_map`).
- The token_loop holds mutable references to `basis_map`, `token_reader`, and `checksum_verifier` across iterations - encoding these as enum-held borrows requires either unsafe code or `Arc<Mutex<>>` wrapping that destroys the performance argument.
- Maintenance burden disproportionate to benefit - the state machine generated by async/await is correct by construction.
- No measurable performance gain: the hot path is I/O-bound (wire reads, channel sends), not poll-dispatch-bound.

### 2.3 Basis map page faults

`MapFile::map_ptr()` can trigger page faults that block the calling
thread. Under async, this would block the tokio worker thread. ASY-3
boundary #3 specifies `spawn_blocking` for basis-file reads.

Two options for the token_loop:

**Option A (recommended): spawn_blocking per block reference.**
Acceptable because block references are infrequent relative to literal
data in typical delta transfers. The spawn_blocking overhead (~1-2 us)
is amortized over the 700+ byte minimum block size.

```rust
DeltaToken::BlockRef(block_idx) => {
    let basis = basis_map.as_mut().unwrap();
    let data = tokio::task::spawn_blocking(move || {
        basis.map_ptr(offset, bytes_to_copy)
    }).await??;
    // ...
}
```

**Option B: run entire token_loop in spawn_blocking.** The disk task
(ASY-3 boundary #9) already uses this pattern - a long-lived
spawn_blocking task that calls `Handle::block_on(recv())` for async
channel operations. This preserves the current structure but limits
concurrency to one OS thread per connection for the wire-read path.
Rejected because it prevents the primary benefit of async: yielding
the thread during wire reads (the dominant wait).

## 3. Channel strategy

Aligned with ASY-3 boundaries #6 and #7:

| Channel | Sync (current) | Async (proposed) |
|---------|---------------|------------------|
| FileMessage (network -> disk) | `spsc::Sender<FileMessage>` spin-wait | `tokio::sync::mpsc::Sender<FileMessage>` `.await` on full |
| CommitResult (disk -> network) | `spsc::Receiver<Result<CommitResult>>` spin-wait | `tokio::sync::mpsc::Receiver<Result<CommitResult>>` `.await` on empty |
| Buffer recycle (disk -> network) | `spsc::Receiver<Vec<u8>>` `try_recv` | `tokio::sync::mpsc::Receiver<Vec<u8>>` `try_recv` (non-blocking) |

Buffer recycle remains non-blocking (`try_recv`): if no buffer is
available, allocate a new one. This matches the current behavior and
avoids adding an `.await` in the hot literal path. The sync SPSC
`try_recv` maps directly to `mpsc::Receiver::try_recv()`.

Channel capacity: preserved at the current default (128, clamped
8..=4096 via `DiskCommitConfig::effective_channel_capacity()`).

### 3.1 Backpressure model

Under the sync SPSC, `send()` spin-waits when the queue is full -
the network thread burns CPU but never yields. Under async:

- `file_tx.send(msg).await` suspends the token_loop task when the
  channel is full, yielding the tokio worker thread.
- The disk commit thread (running in `spawn_blocking`) drains messages,
  freeing slots.
- When a slot opens, the token_loop task is woken and resumes.

This converts spin-wait CPU burn into cooperative park/wake, reducing
CPU usage under disk I/O pressure while preserving throughput when
the disk can keep up.

## 4. Error propagation semantics

### 4.1 Error sources

1. **Wire read errors** - `reader.read_exact()` returns `io::Error`.
2. **Channel disconnection** - `file_tx.send().await` returns `SendError` when the disk thread has panicked or exited.
3. **Basis file errors** - `map_ptr()` returns `io::Error` (I/O fault on mmap'd region).
4. **Protocol violations** - block index out of bounds, block ref without basis.

### 4.2 Propagation path

All errors map to `io::Result<StreamingResult>`:

```rust
// Wire read: natural ? propagation
let delta = token_reader.read_token_async(reader).await?;

// Channel disconnect: map to BrokenPipe (matches current behavior)
file_tx.send(msg).await.map_err(|_| {
    io::Error::new(io::ErrorKind::BrokenPipe, "disk commit thread disconnected")
})?;

// Basis fault: spawn_blocking join error + inner io::Error
let data = tokio::task::spawn_blocking(move || basis.map_ptr(offset, len))
    .await
    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))? // JoinError (panic)
    ?; // inner io::Error
```

### 4.3 Abort message on error

Before returning an error, the token_loop sends `FileMessage::Abort`
to the disk thread so it can clean up the temp file. This behavior is
preserved:

```rust
async fn send_abort(tx: &mpsc::Sender<FileMessage>, reason: String) {
    // Best-effort: if the channel is full or disconnected, we're
    // tearing down anyway.
    let _ = tx.try_send(FileMessage::Abort { reason });
}
```

Note: `try_send` (non-blocking) instead of `.await` because we are in
an error path and must not block indefinitely. If the channel is full,
the disk thread will observe the sender drop and clean up via its own
disconnect detection.

### 4.4 Disk thread error feedback

The disk thread sends `io::Result<CommitResult>` back through the
result channel. Under async, the receiver task polls this channel after
each file completes:

```rust
// In the outer receive loop (not inside token_loop itself):
let commit_result = result_rx.recv().await
    .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "disk thread exited"))?;
match commit_result {
    Ok(result) => { /* verify checksum, advance state */ }
    Err(e) => { /* handle disk error - may trigger redo or abort */ }
}
```

## 5. Drain-on-error semantics

When a file fails mid-transfer, the receiver must continue reading the
remaining tokens from the wire to stay in sync with the sender's stream
position. The sender does not know about receiver-side failures until
the next NDX exchange.

### 5.1 Current behavior (sync)

The current `process_remaining_tokens` returns `Err` immediately on
failure after sending `FileMessage::Abort`. The caller
(`process_file_response_streaming`) propagates the error, and the outer
pipeline loop handles stream re-sync by reading and discarding tokens
until the end marker.

### 5.2 Async behavior

Same structure, with one addition: cancellation-aware drain.

```rust
async fn drain_remaining_tokens(
    reader: &mut AsyncServerReader,
    token_reader: &mut TokenReader,
) -> io::Result<()> {
    loop {
        match token_reader.read_token_async(reader).await? {
            DeltaToken::End => {
                // Read and discard the expected checksum
                let checksum_len = /* from context */;
                let mut discard = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                reader.read_exact(&mut discard[..checksum_len]).await?;
                return Ok(());
            }
            DeltaToken::Literal(LiteralData::Pending(len)) => {
                // Discard literal bytes without allocating
                reader.skip_exact(len).await?;
            }
            DeltaToken::Literal(LiteralData::Ready(_)) => {
                // Compressed: already decompressed, just drop
            }
            DeltaToken::BlockRef(_) => {
                // Nothing to read from wire for block refs
            }
        }
    }
}
```

### 5.3 Graceful shutdown on connection-level failure

When the connection itself fails (wire read error during drain, or
cancellation token fired):

1. The token_loop task returns `Err`.
2. The outer receiver loop drops `file_tx` (the mpsc sender).
3. The disk thread observes `recv() -> None`, finishes any in-progress
   file commit to its temp-file boundary, then exits.
4. Temp files are cleaned up by `TempFileGuard`'s `Drop` impl (no
   partial data reaches the destination).
5. The receiver task joins the disk thread's `JoinHandle` and
   collects any final errors.

### 5.4 CancellationToken integration

Each connection holds a `CancellationToken`. The token_loop checks
cancellation at each loop iteration via `tokio::select!`:

```rust
loop {
    let delta = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            send_abort(&file_tx, "transfer cancelled".into()).await;
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        result = token_reader.read_token_async(reader) => result?,
    };
    // ... process delta
}
```

The `biased` mode ensures cancellation is checked first, providing
bounded shutdown latency (at most one token read in flight at the time
of cancellation).

## 6. TokenReader async adaptation

`TokenReader` currently takes `&mut impl Read`. The async variant needs
`&mut impl AsyncRead + Unpin`:

```rust
impl TokenReader {
    /// Reads one delta token from the async wire.
    pub async fn read_token_async<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut AsyncServerReader<R>,
    ) -> io::Result<DeltaToken> {
        match &mut self.mode {
            TokenMode::Plain => self.read_plain_token_async(reader).await,
            TokenMode::Compressed(decoder) => {
                decoder.read_compressed_token_async(reader).await
            }
        }
    }
}
```

The plain token path reads 4 bytes (`read_exact`) - trivially async.
The compressed path drives the `CompressedTokenDecoder` state machine
which may require multiple small reads. Both map cleanly to async
without structural changes because the decoder already maintains
internal state between reads.

## 7. Feature gating

The async token_loop lives behind `#[cfg(feature = "tokio-transfer")]`,
co-existing with the sync implementation:

```rust
// token_loop.rs (sync, always compiled)
pub(super) fn process_remaining_tokens<R: Read>(...) -> io::Result<StreamingResult> { ... }

// token_loop_async.rs (async, feature-gated)
#[cfg(feature = "tokio-transfer")]
pub(super) async fn process_remaining_tokens_async<R: AsyncRead + Unpin>(
    ...
) -> io::Result<StreamingResult> { ... }
```

The sync path remains the default and production path until ASY-12 flips
the gate. Both paths share types (`StreamingResult`, `DeltaToken`,
`FileMessage`) and differ only in I/O and channel operations.

## 8. Migration sequence

1. **Add `AsyncServerReader` adapter** - wraps `tokio::io::AsyncRead`
   with the same buffering semantics as `ServerReader`.
2. **Add `TokenReader::read_token_async`** - async variant of token
   reading for both plain and compressed modes.
3. **Add `literal_to_buf_async`** - async literal read with buffer
   recycle from `mpsc::Receiver::try_recv`.
4. **Add `process_remaining_tokens_async`** - the async token loop
   using `tokio::sync::mpsc` for dispatch.
5. **Wire into `process_file_response_streaming_async`** - async
   variant of the streaming response processor.
6. **Add drain helper** - `drain_remaining_tokens` for error recovery.
7. **Integration test** - golden-byte parity test proving the async
   path produces identical wire output to the sync path.

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
|------|--------|-----------|
| Basis mmap page fault blocks tokio worker | Thread starvation under heavy block-ref workloads | spawn_blocking per block ref (section 2.3 Option A) |
| Compressed token decoder holds internal state across awaits | Large future size | Decoder state is < 64 KB (zlib window + zstd context); acceptable for a per-connection task |
| Channel capacity mismatch vs sync SPSC | Throughput regression | Preserve capacity 128; benchmark in ASY-4 |
| CancellationToken overhead on hot loop | Latency per token | `biased` select with cancel first; branch prediction favors the non-cancelled path |
| Two code paths (sync + async) | Maintenance burden | Shared types, shared test fixtures, golden-parity tests, ASY-12 gate to eventually retire sync |

## 10. Relationship to parallel delta path (PIP-9)

The `ParallelDeltaPipeline` dispatches entire `DeltaWork` items to
rayon workers. Each worker calls the synchronous `process_remaining_tokens`
internally. Under async:

- The outer dispatch loop (which feeds `WorkQueueSender`) becomes async.
- Each rayon worker continues to run synchronous code inside
  `spawn_blocking` - the token_loop within a rayon worker stays sync.
- The async token_loop applies only to the non-parallel path (single-
  threaded receiver) where the token loop runs on the tokio task itself.

This avoids the complexity of running async code inside rayon workers
(which would require `Handle::block_on` per-worker or a bridge).
The parallel path keeps its current synchronous SPSC + rayon structure;
async benefits accrue on the single-connection, non-parallel receiver.
