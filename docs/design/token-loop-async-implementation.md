# ASY-10.b: token_loop async implementation plan

Status: Implementation plan. Companion to ASY-10.a
(`docs/design/token-loop-async-migration.md`) which defined the
approach. This document provides the concrete implementation
phases, file-level changes, and testing strategy for landing the
async token_loop behind `tokio-transfer`.

Cross-links:

- `docs/design/token-loop-async-migration.md` - approach selection and
  architectural rationale (ASY-10.a)
- `docs/design/asy-2-tokio-runtime-feature.md` - `tokio-transfer` feature
  definition, default-off stance, composition with `async`
- `docs/design/asy-3-async-boundary-spec.md` - boundary contracts
  (especially #4 wire read, #6 channel send, #7 buffer recycle)
- `docs/design/sync-async-wire-parity-test.md` - ASY-11.a parity test
  that gates ASY-12 default-on flip

## 1. Implementation phases

### Phase 1: async function skeleton

Create `token_loop_async.rs` alongside the existing `token_loop.rs`.
The new file compiles only under `#[cfg(feature = "tokio-transfer")]`
and contains the async signatures without body logic - just enough to
prove the module structure compiles.

Deliverables:

- `process_remaining_tokens_async` stub returning
  `io::Result<StreamingResult>`
- `literal_to_buf_async` stub
- `drain_remaining_tokens` stub
- `recycle_or_alloc_async` (uses `tokio::sync::mpsc::Receiver::try_recv`)
- Module registration in `transfer_ops/mod.rs` behind feature gate

### Phase 2: wire read migration

Replace `reader.read_exact(&mut buf)` with
`reader.read_exact(&mut buf).await` on an `AsyncServerReader<R>`
that wraps `tokio::io::AsyncRead + Unpin`. This phase implements:

- `AsyncServerReader` adapter (section 6 of ASY-10.a) with the same
  internal buffer semantics as `ServerReader` but using `AsyncReadExt`.
- `TokenReader::read_token_async<R: AsyncRead + Unpin>` - async
  variant for both plain (4-byte LE read) and compressed paths.
- `CompressedTokenDecoder::recv_token_async` - drives the inflate
  state machine with async reads. The decoder's internal state
  (inflate window, partial output buffer) is preserved across `.await`
  points because it lives in `&mut self`.
- `AsyncServerReader::try_borrow_exact` - zero-copy path for buffered
  literals (same as sync `ServerReader::try_borrow_exact` but polls
  the internal buffer without awaiting).

### Phase 3: channel migration

Replace `spsc::Sender<FileMessage>` with `tokio::sync::mpsc::Sender<FileMessage>`:

- `file_tx.send(msg).await` replaces `file_tx.send(msg)` (spin-wait)
- `tokio::sync::mpsc::Receiver::try_recv()` replaces
  `spsc::Receiver::try_recv()` for buffer recycling (stays non-blocking)
- Error mapping: `mpsc::error::SendError` maps to
  `io::ErrorKind::BrokenPipe` (same semantics as current SPSC disconnect)

Channel capacity preserved at the configured value
(`DiskCommitConfig::effective_channel_capacity()`, default 128).

### Phase 4: error handling and drain

Wire up error propagation, abort signalling, and the drain helper:

- On any error, send `FileMessage::Abort` via `try_send` (non-blocking
  best-effort) before returning `Err`
- `drain_remaining_tokens` reads and discards tokens until
  `DeltaToken::End` to keep the wire stream synchronized
- `CancellationToken` integration via `tokio::select! { biased }` at
  the top of each loop iteration

## 2. Feature flag interaction

### 2.1 `tokio-transfer` (controls this implementation)

The async token_loop code compiles only when `tokio-transfer` is
enabled. This feature:

- Depends on `async` (which provides `dep:tokio`, `dep:tokio-util`)
- Gates `token_loop_async.rs`, `streaming_async.rs`, and the async
  `AsyncServerReader` adapter
- Does not affect the sync path - both coexist at compile time

Declaration in `crates/transfer/Cargo.toml`:

```toml
tokio-transfer = ["async", "dep:tokio", "dep:tokio-util"]
```

### 2.2 `async` (pre-existing)

The existing `async` feature in the transfer crate gates:

- `pipeline/async_pipeline.rs` (job dispatch orchestrator)
- `pipeline/async_dispatch.rs` (file job producer)
- `pipeline/async_signature.rs`
- `dep:tokio` and `dep:tokio-util`

`tokio-transfer` implies `async`, so all async infrastructure is
available when the token_loop async code compiles.

### 2.3 Parallel-receive-delta interaction

The parallel delta path (`ParallelDeltaPipeline` + rayon workers)
is orthogonal. Under the parallel path:

- The outer dispatch loop that feeds `WorkQueueSender` with
  `DeltaWork` items will become async (tokio task reads NDX from wire,
  creates work items, sends to bounded mpsc)
- Each rayon worker calls the **synchronous** `process_remaining_tokens`
  inside `spawn_blocking` - unchanged
- The async `process_remaining_tokens_async` applies only to the
  non-parallel receiver path (single-threaded token processing)

Feature flag composition:

| Scenario | Token loop used |
|----------|----------------|
| Neither feature | sync `process_remaining_tokens` |
| `tokio-transfer` only | async `process_remaining_tokens_async` |
| parallel-receive-delta only | sync inside rayon workers |
| Both enabled | Outer dispatch async, inner workers sync |

No conflict exists because the parallel path never calls the async
token_loop. The dispatch boundary between "which file to process" and
"process this file's tokens" remains clean.

## 3. Files to create and modify

### New files

| Path | Purpose |
|------|---------|
| `crates/transfer/src/transfer_ops/token_loop_async.rs` | Async token loop implementation |
| `crates/transfer/src/transfer_ops/streaming_async.rs` | Async variant of `process_file_response_streaming` |
| `crates/transfer/src/reader/async_reader.rs` | `AsyncServerReader<R>` adapter |
| `crates/transfer/src/token_reader_async.rs` | `TokenReader::read_token_async` and compressed decoder async path |

### Modified files

| Path | Change |
|------|--------|
| `crates/transfer/Cargo.toml` | Add `tokio-transfer` feature |
| `crates/transfer/src/transfer_ops/mod.rs` | `#[cfg(feature = "tokio-transfer")] mod token_loop_async;` |
| `crates/transfer/src/transfer_ops/mod.rs` | `#[cfg(feature = "tokio-transfer")] mod streaming_async;` |
| `crates/transfer/src/reader/mod.rs` | `#[cfg(feature = "tokio-transfer")] pub mod async_reader;` |
| `crates/transfer/src/token_reader.rs` | Add `#[cfg(feature = "tokio-transfer")]` impl block with async methods |
| `crates/transfer/src/lib.rs` | Re-export async streaming function under feature gate |
| `crates/core/Cargo.toml` | Forward `tokio-transfer` to transfer crate |
| `Cargo.toml` (workspace root) | Add `tokio-transfer` feature |

### Test files

| Path | Purpose |
|------|---------|
| `crates/transfer/src/transfer_ops/tests/token_loop_async_tests.rs` | Unit tests for async token loop |
| `crates/transfer/tests/wire_parity_token_loop.rs` | Integration test: sync vs async byte-identical output |
| `crates/transfer/tests/token_loop_cancellation.rs` | Cancellation latency and cleanup verification |

## 4. Self-referential state handling across .await points

The token_loop holds mutable references to three stateful objects
across iterations:

### 4.1 `basis_map: &mut Option<MapFile>`

`MapFile` wraps a file handle with an internal sliding buffer or mmap.
It is not `Send` if backed by a raw mmap pointer.

**Solution:** `MapFile` stays on the tokio task. For `BlockRef` tokens,
the `map_ptr()` call may trigger a page fault. Two approaches:

- **Primary (non-mmap path):** `MapFile::BufferedMap` uses `pread` into
  an owned buffer - no page faults, safe to call from async context.
  The sliding window read is a single syscall that completes quickly.
- **Mmap path fallback:** When `MapFile` uses the mmap strategy, wrap
  the `map_ptr` call in `tokio::task::block_in_place()` rather than
  `spawn_blocking`. This avoids moving `basis_map` to another thread
  (which would require `Send` + ownership transfer) while still
  signalling to the tokio scheduler that the current thread may block.
  `block_in_place` is acceptable here because block references are
  infrequent relative to literals, and the multi-threaded runtime can
  compensate by scheduling other tasks on its remaining worker threads.

The `&mut Option<MapFile>` borrow lives in the async function's stack
frame. Since `process_remaining_tokens_async` is a single long-lived
future pinned to one task, no self-referential issues arise - the
compiler captures `basis_map` as part of the generated state machine
enum.

### 4.2 `token_reader: &mut TokenReader`

`TokenReader` is an enum (`Plain` | `Compressed(CompressedTokenDecoder)`).
The compressed decoder holds:

- zlib inflate state (~37 KB for window + dictionary)
- Partial output buffer (up to 32 KB decompressed data)
- Framing state (current token type, remaining bytes)

All state is in `&mut self` fields - no raw pointers, no
self-references. The async compiler captures the `&mut TokenReader`
in its generated future. Since `TokenReader` is `Unpin` (no
self-referential fields), the borrow works naturally across `.await`
points.

The `read_token_async` method reads from the wire, updates internal
decoder state, and returns `DeltaToken`. Between `.await` points the
decoder state is frozen in the future's generated enum variant -
exactly like a local variable in synchronous code between function
calls.

### 4.3 `checksum_verifier: &mut ChecksumVerifier`

`ChecksumVerifier` holds a running hash context (MD4, MD5, or XXH3).
It is updated incrementally as data passes through. In the async
token_loop, the verifier is NOT updated directly - it was moved to
the disk commit thread via `std::mem::replace` in
`process_file_response_streaming`. The `&mut ChecksumVerifier` in the
token_loop parameter list is the *replacement* verifier (seeded for
the next file). It is only read for `digest_len()` at the `End` token.

No cross-await mutation concerns. The reference is captured in the
future but only accessed at the terminal `DeltaToken::End` state.

### 4.4 Future size considerations

The generated future struct captures all parameters plus loop locals.
Expected size breakdown:

| Field | Size |
|-------|------|
| `&mut AsyncServerReader` | 8 bytes (pointer) |
| `&mpsc::Sender<FileMessage>` | 8 bytes (pointer) |
| `&mut mpsc::Receiver<Vec<u8>>` | 8 bytes (pointer) |
| `&mut ChecksumVerifier` | 8 bytes (pointer) |
| `&Option<FileSignature>` | 8 bytes (pointer) |
| `&mut Option<MapFile>` | 8 bytes (pointer) |
| `total_bytes: u64` | 8 bytes |
| `literal_bytes: u64` | 8 bytes |
| `matched_bytes: u64` | 8 bytes |
| `next_delta: Option<DeltaToken>` | ~40 bytes |
| `token_reader` internal state | ~48 bytes (enum discriminant + pointer) |
| Discriminant + padding | ~16 bytes |

Total: approximately 170-200 bytes per future instance. Well within
the 1 KB guideline for non-boxed futures. No heap allocation needed.

## 5. Drain-on-error implementation details

### 5.1 When drain is needed

The receiver must drain remaining tokens from the wire after:

1. Disk write failure (channel disconnect, temp-file error)
2. Block reference out of bounds (protocol violation)
3. Checksum read failure after `DeltaToken::End` (partial read)
4. Basis file map failure (`map_ptr` I/O error)

In all cases the sender continues transmitting tokens for the current
file because it has no feedback channel until the next NDX exchange.

### 5.2 Implementation

```rust
#[cfg(feature = "tokio-transfer")]
pub(super) async fn drain_remaining_tokens<R: AsyncRead + Unpin>(
    reader: &mut AsyncServerReader<R>,
    token_reader: &mut TokenReader,
    checksum_len: usize,
) -> io::Result<()> {
    loop {
        let delta = token_reader.read_token_async(reader).await?;
        match delta {
            DeltaToken::End => {
                // Consume and discard the trailing checksum
                let mut discard = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                reader.read_exact(&mut discard[..checksum_len]).await?;
                return Ok(());
            }
            DeltaToken::Literal(LiteralData::Pending(len)) => {
                // Skip literal bytes without allocating a buffer.
                // AsyncServerReader::skip_exact advances the stream
                // position by reading into a small stack buffer in a loop.
                reader.skip_exact(len as u64).await?;
            }
            DeltaToken::Literal(LiteralData::Ready(_)) => {
                // Compressed: decompressed data already materialized
                // by the decoder. Drop it (no wire bytes to skip).
            }
            DeltaToken::BlockRef(_) => {
                // Block references have no wire payload beyond the
                // 4-byte token itself (already consumed by read_token).
            }
        }
    }
}
```

### 5.3 `skip_exact` on AsyncServerReader

A dedicated method avoids allocating large buffers just to discard data:

```rust
impl<R: AsyncRead + Unpin> AsyncServerReader<R> {
    /// Discards exactly `n` bytes from the stream.
    ///
    /// Uses a small stack buffer (4 KB) to read and discard in chunks.
    /// More efficient than allocating a Vec for large literals that
    /// will be thrown away during error recovery.
    pub async fn skip_exact(&mut self, mut n: u64) -> io::Result<()> {
        let mut scratch = [0u8; 4096];
        while n > 0 {
            let to_read = n.min(4096) as usize;
            self.read_exact(&mut scratch[..to_read]).await?;
            n -= to_read as u64;
        }
        Ok(())
    }
}
```

### 5.4 Drain vs cancellation interaction

When a `CancellationToken` fires during drain, the drain should abort
immediately - continuing to read wire data during shutdown wastes time:

```rust
async fn drain_with_cancel<R: AsyncRead + Unpin>(
    reader: &mut AsyncServerReader<R>,
    token_reader: &mut TokenReader,
    checksum_len: usize,
    cancel: &CancellationToken,
) -> io::Result<()> {
    loop {
        let delta = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "drain cancelled during shutdown",
                ));
            }
            result = token_reader.read_token_async(reader) => result?,
        };
        match delta {
            DeltaToken::End => {
                let mut discard = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                reader.read_exact(&mut discard[..checksum_len]).await?;
                return Ok(());
            }
            DeltaToken::Literal(LiteralData::Pending(len)) => {
                reader.skip_exact(len as u64).await?;
            }
            DeltaToken::Literal(LiteralData::Ready(_)) | DeltaToken::BlockRef(_) => {}
        }
    }
}
```

## 6. Integration with disk commit task

### 6.1 Current architecture (sync)

The disk commit thread is spawned via `spawn_disk_thread()` which
returns `DiskThreadHandle { file_tx, result_rx, buf_return_rx, join_handle }`.
All channels are lock-free SPSC. The disk thread runs a blocking loop:

```text
loop { file_rx.recv() -> process_file/process_whole_file -> result_tx.send() }
```

### 6.2 Async architecture

Under `tokio-transfer`, the disk commit task runs as a long-lived
`spawn_blocking` task. This preserves:

- Blocking disk I/O (write, fsync, rename) without polluting the
  tokio thread pool
- The existing `process_file` / `process_whole_file` logic unchanged
- Buffer recycling (the disk task sends used buffers back)

The bridge between async token_loop and sync disk task uses
`tokio::sync::mpsc` channels:

```text
[tokio task: token_loop_async]
    |
    | tokio::sync::mpsc::Sender<FileMessage>  (.await on full)
    v
[spawn_blocking: disk_commit_task]
    |
    | tokio::sync::mpsc::Sender<io::Result<CommitResult>>
    v
[tokio task: result collector]
    |
    | tokio::sync::mpsc::Sender<Vec<u8>>  (buffer return)
    v
[tokio task: token_loop_async]  (.try_recv() non-blocking)
```

### 6.3 Disk task spawn pattern

```rust
#[cfg(feature = "tokio-transfer")]
pub fn spawn_disk_commit_task(
    config: DiskCommitConfig,
) -> AsyncDiskHandle {
    let capacity = config.effective_channel_capacity();
    let (file_tx, mut file_rx) = tokio::sync::mpsc::channel::<FileMessage>(capacity);
    let (result_tx, result_rx) = tokio::sync::mpsc::channel::<io::Result<CommitResult>>(capacity * 2);
    let (buf_return_tx, buf_return_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(capacity * 2);

    let join_handle = tokio::task::spawn_blocking(move || {
        // Enter a tokio Handle context so we can call block_on for
        // channel recv inside the blocking thread.
        let rt = tokio::runtime::Handle::current();
        loop {
            let msg = match rt.block_on(file_rx.recv()) {
                Some(msg) => msg,
                None => break, // sender dropped - shutdown
            };
            match msg {
                FileMessage::Begin(begin) => {
                    process_file(&mut file_rx, &result_tx, &buf_return_tx, begin, &config, &rt);
                }
                FileMessage::WholeFile { begin, data } => {
                    process_whole_file(&result_tx, &buf_return_tx, begin, data, &config);
                }
                FileMessage::Abort { .. } | FileMessage::Chunk(_) | FileMessage::Commit => {
                    // Spurious message before Begin - protocol error, skip
                }
            }
        }
    });

    AsyncDiskHandle { file_tx, result_rx, buf_return_rx, join_handle }
}
```

### 6.4 Lifetime management

The disk task's lifetime is bounded by the `file_tx` sender:

1. Token loop completes (success or error) - drops its clone of `file_tx`
2. If all senders are dropped, `file_rx.recv()` returns `None`
3. Disk task exits its loop, completing any in-progress file to its
   temp-file boundary
4. The `JoinHandle` is awaited by the outer receiver orchestrator
5. Any remaining temp files are cleaned up by `TempFileGuard::drop()`

On cancellation: the token loop returns early, drops `file_tx`. The
disk task observes disconnect on its next `recv()`, exits gracefully.

## 7. Testing strategy

### 7.1 Wire-byte parity test

Proves the async path produces identical wire output to the sync path
for the same input stream. Architecture:

- Create a `WireCapture<R>` wrapper that logs all bytes read
- Run `process_file_response_streaming` (sync) against a recorded
  wire stream, capture all bytes consumed
- Run `process_file_response_streaming_async` (async) against the
  same recorded stream, capture all bytes consumed
- Assert byte-for-byte equality of consumed sequences

Test vectors:

| Case | Description |
|------|-------------|
| Small literal-only | Single chunk, WholeFile coalesce path |
| Multi-chunk literal | 3+ literal tokens, tests buffer recycling |
| Block reference | Basis file with matching blocks |
| Mixed literal + block | Interleaved tokens |
| Compressed (zlib) | CompressedTokenDecoder state across awaits |
| Empty file | Zero-byte transfer (End immediately) |

### 7.2 Error-path coverage

| Test | Verifies |
|------|----------|
| `wire_read_error_sends_abort` | Network failure triggers `FileMessage::Abort` before returning `Err` |
| `channel_disconnect_maps_to_broken_pipe` | Disk task panic/exit detected as `BrokenPipe` |
| `block_ref_out_of_bounds_aborts` | Invalid block index sends abort, returns `InvalidData` |
| `drain_consumes_all_tokens` | After error, drain reads until `End` + checksum |
| `drain_skips_literal_without_alloc` | Large pending literal during drain uses `skip_exact`, not `Vec` |
| `basis_map_error_sends_abort` | `map_ptr` failure triggers abort + error return |

### 7.3 Cancellation test

Verifies bounded shutdown latency:

```rust
#[tokio::test]
async fn cancellation_interrupts_within_one_token_read() {
    let cancel = CancellationToken::new();
    let (file_tx, _file_rx) = tokio::sync::mpsc::channel(16);

    // Create a reader that blocks forever on the second read
    let wire_data = /* one valid literal token + hanging stream */;
    let reader = AsyncServerReader::new(SlowReader::new(wire_data));

    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move {
        process_remaining_tokens_async(
            &mut reader, &file_tx, /* ... */, &cancel_clone,
        ).await
    });

    // Cancel after the first token is processed
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_millis(100), handle).await;
    assert!(result.is_ok(), "should complete within 100ms of cancel");
    let inner = result.unwrap().unwrap();
    assert_eq!(inner.unwrap_err().kind(), io::ErrorKind::Interrupted);
}
```

### 7.4 Integration test against disk commit task

End-to-end test that:

1. Spawns `spawn_disk_commit_task` with a temp directory
2. Feeds a multi-file wire stream through `process_file_response_streaming_async`
3. Collects `CommitResult` values from `result_rx`
4. Verifies files are correctly written to disk with expected checksums
5. Verifies buffer recycling (pool size stays bounded)

### 7.5 Regression gate

All tests above run under the `tokio-transfer` feature in CI:

```yaml
- name: nextest (tokio-transfer)
  run: cargo nextest run --workspace --features tokio-transfer
```

This ensures the async path never regresses while remaining off by
default in production builds.

## 8. Migration sequence (ordered)

| Step | Depends on | Deliverable |
|------|-----------|-------------|
| 1 | - | `AsyncServerReader` adapter with `read_exact`, `try_borrow_exact`, `skip_exact` |
| 2 | Step 1 | `TokenReader::read_token_async` (plain + compressed) |
| 3 | Step 1 | `literal_to_buf_async` using `AsyncServerReader` + `mpsc::Receiver::try_recv` |
| 4 | Steps 2, 3 | `process_remaining_tokens_async` - full async token loop |
| 5 | Step 4 | `drain_remaining_tokens` and `drain_with_cancel` |
| 6 | Step 4 | `process_file_response_streaming_async` (WholeFile coalesce + dispatch) |
| 7 | Step 6 | `spawn_disk_commit_task` (spawn_blocking bridge) |
| 8 | All above | Wire-parity integration test |
| 9 | Step 8 | CI gate addition |

## 9. Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|-----------|
| Future size exceeds L1 cache line | Low | Minor perf regression on task switch | Monitor with `std::mem::size_of_val` in tests; box if > 1 KB |
| `block_in_place` starves workers under heavy block-ref load | Low | Throughput drop for basis-heavy transfers | BufferedMap strategy (pread) is default; mmap only for large files where page cache is warm |
| Compressed decoder async variant diverges from sync | Medium | Silent wire incompatibility | Golden byte tests with identical input produce identical output |
| `spawn_blocking` disk task leaks on panic | Low | Temp files not cleaned up | `TempFileGuard::Drop` + `catch_unwind` wrapper in disk task |
| Buffer return channel fills up (disk slower than network) | Medium | Excessive allocation instead of recycling | Acceptable degradation - same behavior as current sync path when disk falls behind |
