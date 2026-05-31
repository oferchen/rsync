# ASY-8.b: Sender tokio implementation plan

Status: Implementation plan.
Tracking: #2998 (follow-on from ASY-8.a design).
Depends on: ASY-8.a (design, merged), ASY-2 (tokio-transfer feature).
Companion: ASY-7.a (receiver tokio prototype).

## 1. Overview

This document is the implementation plan for migrating the sender-side
transfer loop to tokio. ASY-8.a defined the architecture - single sender
orchestrator task, separate NDX reader task via `tokio::sync::mpsc`
(capacity 128), `spawn_blocking` for delta generation and file reads, a
4-slot mpsc for whole-file streaming, and `CancellationToken` for
cooperative shutdown. This document specifies the concrete files,
changes, phase ordering, and validation criteria.

## 2. Implementation phases

### Phase 1: NDX reader task (priority: highest)

Extract the wire-read portion of the transfer loop into a dedicated async
task that pre-parses complete per-file request envelopes and dispatches
them to the sender task over a bounded channel.

**Deliverables:**
- New file: `crates/transfer/src/generator/transfer/ndx_reader.rs`
- New file: `crates/transfer/src/generator/transfer/transfer_loop_async.rs`
- Modified: `crates/transfer/src/generator/transfer/mod.rs` (conditional module inclusion)

### Phase 2: Async wire writes

Convert the sender orchestrator to use `.await` on all wire writes
(token emission, NDX echo, checksum output). The orchestrator consumes
`NdxRequest` structs from the channel and drives per-file processing.

**Deliverables:**
- Async writer trait usage in `transfer_loop_async.rs`
- Flush-before-block invariant preserved at channel recv points

### Phase 3: spawn_blocking for delta generation

Wrap `generate_delta_from_signature` and `open_source_reader` calls in
`tokio::task::spawn_blocking`. The delta script is computed off the async
runtime thread pool and returned via `JoinHandle`.

**Deliverables:**
- Blocking bridge in `transfer_loop_async.rs`
- Small-file batching threshold (files < 4 KB grouped into one blocking
  call to amortize scheduling overhead)

### Phase 4: Whole-file streaming pipeline

Implement the read-channel-write pipeline for whole-file transfers: a
`spawn_blocking` reader produces 256 KB chunks into a 4-slot
`tokio::sync::mpsc`, and the sender task drains chunks to async wire
writes. Overlaps disk I/O with network I/O.

**Deliverables:**
- `stream_whole_file_async` in `transfer_loop_async.rs`
- Checksum computed in the blocking reader (single-pass)

## 3. Feature flag wiring

### 3.1 Cargo feature definition

The `async` feature already exists in `crates/transfer/Cargo.toml`:

```toml
async = ["dep:tokio", "dep:tokio-util"]
```

The async sender implementation gates on this feature. No new feature is
needed - `async` is the umbrella for tokio-based transfer paths in this
crate.

### 3.2 Conditional compilation strategy

The transfer loop dispatches at the orchestrator level:

```rust
// crates/transfer/src/generator/transfer/orchestrator.rs
#[cfg(feature = "async")]
mod transfer_loop_async;

// In GeneratorContext::run():
#[cfg(feature = "async")]
if runtime_handle.is_some() {
    return self.run_transfer_loop_async(reader, writer, progress, itemize);
}
// Falls through to sync path
self.run_transfer_loop(reader, writer, progress, itemize)
```

The sync path remains the default. The async path activates only when:
1. The `async` feature is compiled in, AND
2. A tokio runtime handle is available (passed from `core::session`).

### 3.3 Module layout under feature gate

```
crates/transfer/src/generator/transfer/
  mod.rs                    -- existing, adds #[cfg(feature = "async")] mod
  transfer_loop.rs          -- existing sync implementation (unchanged)
  transfer_loop_async.rs    -- NEW: async sender orchestrator
  ndx_reader.rs             -- NEW: NDX reader task
  orchestrator.rs           -- existing, adds async dispatch branch
  goodbye.rs                -- existing (reused by both paths)
  stats.rs                  -- existing (reused by both paths)
```

## 4. NDX reader task design

### 4.1 Message type

```rust
/// A fully-parsed per-file request from the receiver/generator.
///
/// The NDX reader pre-parses the complete envelope so the sender task
/// never awaits on wire reads mid-file.
pub(crate) enum NdxMessage {
    /// A file transfer request with all data needed to generate a delta.
    FileRequest {
        ndx: i32,
        iflags: ItemFlags,
        xname: Option<Vec<u8>>,
        sum_head: SumHead,
        sig_blocks: Vec<WireBlock>,
        pending_xattr: Option<XattrResponse>,
    },
    /// Phase transition signal (NDX_DONE that is not a flist-free echo).
    PhaseDone,
    /// INC_RECURSE flist-free NDX_DONE echo.
    FlistDone,
    /// NDX_FLIST_EOF received from receiver.
    FlistEof,
    /// NDX_DEL_STATS with parsed deletion counts.
    DelStats(DeleteStats),
}
```

### 4.2 Reader task loop

```rust
async fn ndx_reader_task(
    reader: AsyncReader,
    tx: mpsc::Sender<NdxMessage>,
    cancel: CancellationToken,
    protocol_version: u8,
    inc_recurse: bool,
    // State needed for parsing: xattr config, phase tracking
    mut parse_state: NdxParseState,
) -> io::Result<()> {
    let mut codec = create_ndx_codec(protocol_version);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = read_next_ndx_async(&mut reader, &mut codec) => {
                let ndx = result?;
                let message = parse_ndx_envelope(
                    &mut reader, ndx, &mut parse_state
                ).await?;
                if tx.send(message).await.is_err() {
                    break; // sender task dropped
                }
            }
        }
    }
    Ok(())
}
```

### 4.3 Backpressure and ordering

- Channel capacity 128 limits read-ahead. If the sender falls behind
  (blocked on delta generation or slow network), the reader parks on
  `send().await`.
- Messages arrive in wire order - the mpsc preserves FIFO. The sender
  task processes them sequentially, matching upstream's serial model.
- The reader task owns the `AsyncReader` exclusively - no concurrent
  reads from multiple tasks.

### 4.4 Error propagation

If the reader task encounters a wire error:
1. It drops the `tx` sender, causing the orchestrator's `recv().await`
   to return `None`.
2. The `JoinHandle` carries the `io::Result` which the orchestrator
   inspects after the channel closes.
3. For tolerant mode (dry-run), early-close errors are mapped to a
   graceful `NdxMessage::PhaseDone` before dropping.

## 5. INC_RECURSE segment encoding in async context

### 5.1 Placement

Segment encoding stays inline in the sender task at the top of the main
loop - same position as the sync path. This is a protocol ordering
invariant: segments must be emitted before the next file's NDX echo.

```rust
// Async sender main loop (simplified)
loop {
    // INC_RECURSE: dispatch pending segments
    if inc_recurse {
        let remaining = dispatched_entry_count.saturating_sub(files_transferred);
        while let Some(seg) = scheduler.next_if_needed(remaining) {
            encode_and_send_segment_async(&mut writer, seg, ...).await?;
            segments_sent += 1;
            flist_done_remaining += 1;
            dispatched_entry_count += seg.count;
        }
        if !flist_eof_sent && scheduler.is_exhausted() {
            send_flist_eof_async(&mut writer, ...).await?;
            flist_eof_sent = true;
        }
    }
    writer.flush().await?;

    // Receive next NDX from reader task
    let msg = match ndx_rx.recv().await { ... };
    ...
}
```

### 5.2 Segment encoder state

The flist writer cache (`FlistWriterCache`) and flist NDX codec remain
owned by the sender task. They are not `Send` across spawn boundaries -
they stay in the single orchestrator task where all wire writes occur.

### 5.3 Flist-done echo timing

When the reader task receives `NDX_DONE` during INC_RECURSE with
pending flist frees:
1. Reader classifies it as `NdxMessage::FlistDone`.
2. Sender task echoes `NDX_DONE` on the wire without phase increment.
3. Decrements `flist_done_remaining`.
4. When `flist_done_remaining == 0 && flist_eof_sent`, sender performs
   the proactive phase transition (matching the sync path logic at
   `transfer_loop.rs:185`).

The reader task must track phase state to distinguish flist-done echoes
from real phase transitions. This state is passed via `NdxParseState`.

## 6. Files to create and modify

### 6.1 New files

| File | Role |
|------|------|
| `crates/transfer/src/generator/transfer/ndx_reader.rs` | NDX reader async task: wire parsing, envelope assembly, channel send |
| `crates/transfer/src/generator/transfer/transfer_loop_async.rs` | Async sender orchestrator: main loop, spawn_blocking dispatch, wire writes |

### 6.2 Modified files

| File | Change |
|------|--------|
| `crates/transfer/src/generator/transfer/mod.rs` | Add `#[cfg(feature = "async")] mod ndx_reader; mod transfer_loop_async;` |
| `crates/transfer/src/generator/transfer/orchestrator.rs` | Add async dispatch branch in `run()` when tokio handle available |
| `crates/transfer/src/generator/context.rs` | Store optional `tokio::runtime::Handle` in `GeneratorContext` |
| `crates/transfer/src/generator/mod.rs` | Thread runtime handle through to context |
| `crates/transfer/Cargo.toml` | Possibly add `tokio-util/sync` to the async feature deps (for `CancellationToken`) |
| `crates/core/src/session.rs` | Pass runtime handle to transfer crate when `async` feature active |

### 6.3 Files explicitly unchanged

| File | Reason |
|------|--------|
| `crates/transfer/src/generator/transfer/transfer_loop.rs` | Sync path stays as-is; no refactoring |
| `crates/transfer/src/generator/delta.rs` | Delta generation is sync-only; called from `spawn_blocking` |
| `crates/transfer/src/generator/segments.rs` | `SegmentScheduler` logic unchanged; used by both paths |
| `crates/matching/src/generator.rs` | ZSO-1..4 stay inside `spawn_blocking` (per ASY-8.a section 7) |

## 7. Testing strategy

### 7.1 Golden-byte parity with sync path

The async sender MUST produce bit-identical wire output for any given
input. Validation approach:

- **Capture-compare test:** A test harness feeds identical (file list,
  source data, negotiated options) to both sync and async paths, captures
  wire output into byte buffers, and asserts equality.
- **Existing golden tests:** `crates/protocol/tests/golden/` tests run
  under both `--features async` and without. CI matrix includes both.
- **INC_RECURSE parity:** Dedicated test with 100+ directory tree
  verifying segment wire bytes match between sync and async.

### 7.2 Interop suite

The interop harness (`tools/ci/run_interop.sh`) runs against upstream
3.0.9, 3.1.3, 3.4.1, 3.4.2. The async path must pass all existing
interop scenarios:

- Delta push (small files, large files)
- Whole-file push
- Checksum mode push
- INC_RECURSE push with deep trees
- Compressed push (zlib, zstd)
- Dry-run push

A new CI matrix dimension gates on `--features async` for the interop
run.

### 7.3 Performance gate

The async sender must not regress throughput. Benchmarks:

| Scenario | Baseline | Gate |
|----------|----------|------|
| 100k small files (1-4 KB), delta push | sync path throughput | >= 95% of sync |
| 10 large files (1 GB), whole-file push | sync path throughput | >= 100% of sync (expect win from pipeline) |
| 1k files, INC_RECURSE deep tree | sync path throughput | >= 95% of sync |

Benchmarks run on the `rsync-profile` container with `BENCH_RUNS=5`.
Results gate promotion from `default = off` to `default = on` (ASY-12).

### 7.4 Cancellation tests

- Cancel mid-delta: verify no wire corruption (partial writes are not
  flushed).
- Cancel mid-whole-file-stream: verify reader task exits cleanly, no
  zombie `spawn_blocking` threads.
- Cancel during INC_RECURSE segment encode: verify partial segment is
  not emitted.

### 7.5 Error recovery tests

- Source file disappears between NDX receipt and open: `MSG_NO_SEND`
  emitted, transfer continues.
- Wire write fails mid-file: error propagates, reader task cancelled.
- `spawn_blocking` panic: JoinError mapped to `io::Error`, transfer
  aborts with correct exit code.

## 8. Estimated effort and phase ordering

| Phase | Description | Estimated effort | Dependencies |
|-------|-------------|-----------------|--------------|
| 1 | NDX reader task + async skeleton | 3-4 days | None |
| 2 | Async wire writes (orchestrator loop) | 2-3 days | Phase 1 |
| 3 | spawn_blocking for delta + open | 2 days | Phase 2 |
| 4 | Whole-file streaming pipeline | 2-3 days | Phase 2 |
| 5 | INC_RECURSE async encoding | 1-2 days | Phase 2 |
| 6 | Testing + golden parity | 3-4 days | Phases 1-5 |
| 7 | Performance benchmarking + tuning | 2-3 days | Phase 6 |

**Total: 15-21 days.**

Phases 3, 4, and 5 are independent of each other (all depend on Phase 2)
and can be worked in parallel.

### Phase ordering rationale

Phase 1 is first because the NDX reader task is the structural
prerequisite - it separates wire reads from the orchestrator, enabling
async writes without deadlock risk. Phase 2 follows immediately because
the orchestrator cannot be tested until it can write to the wire.
Phases 3-5 are interchangeable but phase 3 (delta `spawn_blocking`)
covers the most common code path so it validates the blocking bridge
pattern early.

## 9. Risk register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| NdxParseState diverges from sync path on edge cases | Medium | Wire desync | Shared parsing logic extracted to helper fns used by both paths |
| spawn_blocking pool exhaustion under high connection count | Low | Sender stalls | Budget: 1 slot per connection; pool sized per ASY-3 recommendation |
| Whole-file pipeline adds latency for small files (< chunk size) | Medium | Regression on small-file workload | Bypass pipeline for files < 256 KB; direct async write |
| INC_RECURSE phase tracking bug in reader task | Medium | Deadlock or panic | Fuzz test: random NDX_DONE/NDX_FLIST_EOF sequences |
| Feature gate leaks async deps into default build | Low | Compile time regression | CI builds without `async` feature; verify no tokio in dependency tree |

## 10. Open questions

1. **Runtime handle injection:** Should `GeneratorContext` store a
   `Handle` directly, or should the async path be entered via a new
   entry point (`run_async`) that takes the handle as a parameter?
   Recommendation: new entry point to keep the sync path zero-cost.

2. **Small-file batching threshold:** ASY-8.a proposes 4 KB. Should this
   be configurable or hardcoded? Recommendation: hardcoded constant,
   tunable via benchmark results in Phase 7.

3. **Compression encoder placement:** The `TokenEncoder` is stateful and
   per-session. Under async, it stays in the orchestrator task (never
   crosses `spawn_blocking`). If whole-file streaming needs compression,
   the chunks are compressed in the orchestrator after recv - not in the
   blocking reader. This adds a serial compression step but preserves
   encoder state integrity.
