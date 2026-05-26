# Token Loop Parallel vs Sequential Dispatch Audit (PFF-2)

Audit date: 2026-05-26

Follow-up to PFF-1 (PR #5049), which cataloged the `parallel-receive-delta`
feature flag surface. PFF-2 traces the actual runtime code paths through the
receiver's token loop to document what runs with the feature on vs off.

---

## 1. Executive Summary

The `parallel-receive-delta` feature is currently a **compile-time-only gate**
with **no runtime dispatch path**. The production receiver has two independent
transfer loops - a synchronous path (`run_sync`) and a pipelined path
(`run_pipelined` / `run_pipelined_incremental`). Neither loop reads the feature
flag at runtime, and neither loop contains a `#[cfg(feature =
"parallel-receive-delta")]` branch point. The feature gate controls only which
engine and transfer modules compile:

- **Feature OFF**: `chunk_adapter`, `parallel_apply`, and `chunk_builder`
  modules are excluded from compilation. The `ParallelDeltaPipeline` and
  `ThresholdDeltaPipeline` types compile unconditionally but are never
  instantiated by any production receiver path.
- **Feature ON**: The same three modules compile in, but still have **zero
  production callers**. They are exercised only by benchmarks and unit tests.

The PIP-9.b series is the planned cutover. As of this audit, PIP-9.b.1
(call-shape audit), PIP-9.b.2 (cfg-dispatch sketch), PIP-9.b.3 (feed loop
spec), and PIP-9.b.6 (interop matrix CI cell) are design-only merges. The
implementation PRs - PIP-9.b.3 (code) and PIP-9.b.4 (`flush_workers` wiring) -
have not landed. The cutover site (`sync.rs:241-253`) remains sequential-only.

---

## 2. Receiver Transfer Loop Architecture

The receiver has three entry points, all on `ReceiverContext`:

| Entry point | File | Token processing | Production? |
|-------------|------|-----------------|-------------|
| `run_sync` | `receiver/transfer/sync.rs:42` | Direct `apply_delta_tokens` per file | Non-default; kept for testing |
| `run_pipelined` | `receiver/transfer/pipelined.rs` | `process_file_response_streaming` -> SPSC -> disk thread | Default (non-INC_RECURSE) |
| `run_pipelined_incremental` | `receiver/transfer/pipelined_incremental.rs` | Same streaming path as above | Default (INC_RECURSE) |

### 2.1 Which path runs in production

`ReceiverContext::run` (the top-level dispatch at `receiver/transfer.rs:40`)
selects between:

- `run_pipelined_incremental` when `incremental-flist` feature is on (default)
- `run_pipelined` otherwise

`run_sync` is never called by `run`. It exists for testing and as the PIP-9.b
cutover target.

### 2.2 The `ReceiverDeltaPipeline` trait

The `delta_pipeline` module at `crates/transfer/src/delta_pipeline/mod.rs`
defines a `ReceiverDeltaPipeline` trait with three implementations:

- `SequentialDeltaPipeline` - processes `DeltaWork` items synchronously via
  `strategy::dispatch()` (always compiled)
- `ParallelDeltaPipeline` - dispatches to rayon workers via bounded
  `WorkQueueSender` -> `DeltaConsumer` -> `ReorderBuffer` (always compiled)
- `ThresholdDeltaPipeline` - buffers items, promotes to parallel at threshold
  (always compiled)

**None of these are instantiated by any production code path.** They have zero
callers outside tests, benchmarks, and the PIP-6 bench harness. The trait
exists as substrate for the PIP-9.b wire-up.

---

## 3. Sequential Path (Feature OFF or ON - Current Production)

### 3.1 Synchronous path (`run_sync`)

The per-file loop at `sync.rs:98-398` calls `apply_delta_tokens` at line 241.
This function (`sync.rs:445-573`) is a tight `loop { match read_token }`:

```
loop {
    match token_reader.read_token(reader)? {
        End => { verify checksum; return Ok(()) }
        Literal(data) => { write_chunk(output, sparse, data); verifier.update(data) }
        BlockRef(idx) => { map basis bytes; write_chunk(output, sparse, block); verifier.update(block) }
    }
}
```

Bytes go directly into a `BufWriter<File>` (`output`). No SPSC channel, no
background thread, no rayon dispatch. The checksum verifier accumulates inline.
This matches upstream `receiver.c:recv_files()` exactly.

### 3.2 Pipelined path (`run_pipelined` / `run_pipelined_incremental`)

The pipelined loop at `receiver/transfer/pipeline.rs:38` calls
`process_file_response_streaming` per file (line 310). This function at
`transfer_ops/streaming.rs:74` reads the response header, then enters the token
loop via `process_remaining_tokens` at `transfer_ops/token_loop.rs:77`.

The token loop (`token_loop.rs:97-204`) reads tokens and sends `FileMessage`
chunks over an SPSC channel to a disk-commit background thread:

```
loop {
    match token_reader.read_token(reader) {
        End => { send Commit; return StreamingResult }
        Literal(data) => { send Chunk(buf); total_bytes += len }
        BlockRef(idx) => { map basis bytes; send Chunk(buf); total_bytes += len }
    }
}
```

The disk-commit thread (`PipelinedReceiver`) receives `FileMessage::Begin`,
`FileMessage::Chunk`, and `FileMessage::Commit` messages. Checksum verification
happens on the disk thread side, not in the token loop.

**There is no `#[cfg(feature = "parallel-receive-delta")]` branch point in
either token loop.**

---

## 4. Parallel Path - What the Feature Flag Compiles

When `parallel-receive-delta` is enabled, three additional modules compile:

### 4.1 Engine: `chunk_adapter` module

File: `crates/engine/src/concurrent_delta/chunk_adapter.rs`

Pure in-memory shape transformer. `DeltaChunkAdapter::from_delta_work` converts
`(&DeltaWork, ChunkPayload)` into a `DeltaChunk`. Zero state, no I/O, no
threads. Types:

- `ChunkSource` - literal vs copy discriminator
- `ChunkPayload` - per-chunk data + sequence + optional expected digest
- `DeltaChunkAdapter` - zero-state converter struct
- `delta_work_to_chunk` - free-function alias

### 4.2 Engine: `parallel_apply` module

File: `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` + submodules
(`batch.rs`, `decrement_guard.rs`, `drain.rs`, `slot_barrier.rs`)

The `ParallelDeltaApplier` type. Owns a `DashMap<FileNdx, Arc<SlotBarrier>>`
for per-file slot management and a configurable concurrency limit. Public API:

- `register_file(ndx, writer)` - opens a per-file slot with a `Box<dyn Write>`
- `apply_one_chunk(chunk)` - verifies chunk digest (rayon), writes under
  per-file mutex, preserves chunk order via per-file `ReorderBuffer`
- `apply_batch_parallel(chunks)` - batch variant using `par_iter`
- `flush_workers(ndx)` - drains in-flight chunks for a file
- `finish_file(ndx)` - bakes `flush_workers` + recovers the writer
- `bytes_written(ndx)` - query written bytes for a file

**Zero production callers.** Exercised by benchmarks (`parallel_receive_delta_perf`,
`parallel_verify_chunk`, `br_3j_f_dashmap_cores_vs_throughput`) and unit tests
only.

### 4.3 Transfer: `chunk_builder` module

File: `crates/transfer/src/delta_pipeline/chunk_builder.rs`

Per-file builder that converts `DeltaToken` values into `DeltaChunk` values,
populating `expected_strong` from the negotiated `FileSignature` for `BlockRef`
tokens. Types:

- `ChunkBuilder` - holds `FileNdx`, borrows `&FileSignature`, tracks
  monotonic `chunk_sequence`
- `ChunkBuilderError` - typed errors for out-of-bounds or length mismatch
- `TokenForBuild` - input enum pairing tokens with resolved bytes

**Zero production callers.** Unit tests only.

---

## 5. The Planned Cutover (PIP-9.b Series)

### 5.1 Status of PIP-9.b sub-tasks

| Task | PR | Status | Content |
|------|----|--------|---------|
| PIP-9.b.1 | #4747 | Merged | Call-shape audit of sequential `apply_delta_tokens` - 10 equivalence invariants |
| PIP-9.b.2 | #4776 | Merged | Cfg-dispatch sketch - chose Variant A (single `#[cfg]` if-else at sync.rs:241) |
| PIP-9.b.3 | #4960 | Merged (spec only) | Feed loop spec - `DeltaWork` -> `DeltaChunk` conversion design. **No code landed.** |
| PIP-9.b.4 | #5047 | Open | `flush_workers` drain at file boundary. **Not merged.** |
| PIP-9.b.5 | - | Not started | Parallel-threshold-trip parity test |
| PIP-9.b.6 | #4958 | Merged | CI cell running upstream interop under `--features parallel-receive-delta` |

### 5.2 The planned cutover site

PIP-9.b.2 chose `sync.rs:241-253` as the cutover site. The planned shape is
a `#[cfg(feature = "parallel-receive-delta")]` / `#[cfg(not(...))]` pair:

```text
#[cfg(feature = "parallel-receive-delta")]
{
    // 1. applier.register_file(ndx, writer)
    // 2. loop { read_token -> ChunkBuilder -> DeltaChunk -> applier.apply_one_chunk(chunk) }
    // 3. applier.flush_workers(ndx)  (PIP-9.b.4)
    // 4. applier.finish_file(ndx)    (recover writer)
}
#[cfg(not(feature = "parallel-receive-delta"))]
{
    apply_delta_tokens(reader, output, ...);  // unchanged
}
```

The `ParallelDeltaApplier` would live as an `Option<ParallelDeltaApplier>` field
on `ReceiverContext`, constructed once at `run_sync` entry. **This field does
not exist on master today.**

### 5.3 The pipelined path is NOT targeted by PIP-9.b

PIP-9.b.2 section 1 explicitly states: "The streaming path stays sequential; if
a future task wants the parallel arm there too, it follows the same shape but
with a different adapter on the `FileMessage::Chunk` boundary." The pipelined
token loop at `transfer_ops/token_loop.rs` is not a cutover target. This means
even after PIP-9.b completes, the production default path (`run_pipelined` /
`run_pipelined_incremental`) will still run sequentially. The parallel path
only activates in `run_sync`, which is not called in production.

### 5.4 What PIP-9.f would do

PIP-9.f is the planned "flip the default" task. It would add
`parallel-receive-delta` back to the `default` feature set in all crates
(removed post-PIP-7). But even then, the only affected path is `run_sync` -
the production pipelined path is unaffected.

---

## 6. Data Flow Comparison

### 6.1 Sequential (current production - pipelined)

```text
Wire -> ServerReader -> TokenReader::read_token()
                              |
                    DeltaToken (Literal/BlockRef/End)
                              |
                    process_remaining_tokens()
                              |
                    FileMessage::Chunk(buf)
                              |
                    SPSC channel
                              |
                    Disk commit thread (PipelinedReceiver)
                              |
                    BufWriter<File> -> temp file -> rename
```

### 6.2 Sequential (current - synchronous, for testing)

```text
Wire -> ServerReader -> TokenReader::read_token()
                              |
                    DeltaToken (Literal/BlockRef/End)
                              |
                    apply_delta_tokens() [sync.rs:445]
                              |
                    write_chunk(BufWriter<File>)
                              |
                    Direct disk write -> temp file -> rename
```

### 6.3 Planned parallel (PIP-9.b target - sync path only)

```text
Wire -> ServerReader -> TokenReader::read_token()
                              |
                    DeltaToken (Literal/BlockRef/End)
                              |
                    ChunkBuilder::next_chunk()
                              |
                    DeltaChunk { ndx, sequence, data, is_literal, expected_strong }
                              |
                    ParallelDeltaApplier::apply_one_chunk()
                              |
                    +--> verify_chunk (rayon pool)
                    |         |
                    |    per-file Mutex<BufWriter>
                    |         |
                    |    write in chunk_sequence order (per-file ReorderBuffer)
                    |
                    +---> flush_workers(ndx) at End token
                              |
                    finish_file(ndx) -> recover writer
```

---

## 7. Dead Code Analysis

### 7.1 Always-compiled but never instantiated in production

| Type | File | Callers |
|------|------|---------|
| `ParallelDeltaPipeline` | `transfer/src/delta_pipeline/parallel.rs` | Tests, benches only |
| `ThresholdDeltaPipeline` | `transfer/src/delta_pipeline/threshold.rs` | Tests only |
| `ReceiverDeltaPipeline` trait | `transfer/src/delta_pipeline/mod.rs` | Tests, bench harness only |
| `SequentialDeltaPipeline` | `transfer/src/delta_pipeline/sequential.rs` | Tests, bench harness, ThresholdDeltaPipeline::flush |
| `DeltaConsumer` | `engine/src/concurrent_delta/consumer/` | ParallelDeltaPipeline (itself unused in prod) |
| `ReorderBuffer` | `engine/src/concurrent_delta/reorder/` | DeltaConsumer, ParallelDeltaApplier (the latter only when feature on) |
| `DeltaWork` | `engine/src/concurrent_delta/types.rs` | All pipeline types (none in prod) |
| `DeltaResult` | `engine/src/concurrent_delta/types.rs` | All pipeline types (none in prod) |
| `strategy::dispatch` | `engine/src/concurrent_delta/strategy.rs` | SequentialDeltaPipeline (itself unused in prod) |

### 7.2 Feature-gated - compiled but never called in production

| Type | File | Feature gate | Callers |
|------|------|-------------|---------|
| `ParallelDeltaApplier` | `engine/src/concurrent_delta/parallel_apply/` | `parallel-receive-delta` | Benchmarks, unit tests |
| `DeltaChunkAdapter` | `engine/src/concurrent_delta/chunk_adapter.rs` | `parallel-receive-delta` | Unit tests |
| `ChunkBuilder` | `transfer/src/delta_pipeline/chunk_builder.rs` | `parallel-receive-delta` | Unit tests |

### 7.3 Feature-gated correctly?

The `ParallelDeltaPipeline` and `ThresholdDeltaPipeline` types compile
unconditionally (no feature gate) even though they logically belong to the
parallel infrastructure. PFF-1 flagged this as inconsistency #3: "unconditionally
compiled parallel pipeline types." The delta pipeline types are architectural
substrate (the `ReceiverDeltaPipeline` trait and its implementations) that
predate the PIP-8 teardown. They serve the PIP-6 bench harness and integration
tests today.

---

## 8. Runtime Fallback

There is **no runtime fallback from parallel to sequential**. The dispatch is
purely compile-time via `#[cfg]` gates. A build with `parallel-receive-delta`
enabled will (once PIP-9.b.3 code lands) always take the parallel arm at
`sync.rs:241`; a build without it always takes the sequential arm. There is no
`--no-parallel-delta` CLI flag, no environment variable override, no threshold
check that falls back to sequential at runtime.

The `ThresholdDeltaPipeline` (which auto-selects sequential vs parallel based
on file count) is a runtime heuristic, but it operates at the `DeltaWork` level,
not the token loop level. It has no production callers and would not affect the
token loop dispatch even if it did.

---

## 9. Inconsistencies and Gaps

### 9.1 PIP-9.b targets the wrong path

The PIP-9.b cutover targets `run_sync`, which is not called in production. The
production path (`run_pipelined` via `run_pipelined_incremental`) uses a
fundamentally different architecture (SPSC streaming to a disk-commit thread)
that PIP-9.b explicitly defers. This means:

- After PIP-9.b completes, default builds still run fully sequential.
- The parallel path is only reachable via `run_sync` + the feature flag.
- Benchmarks and interop tests that use `run_sync` would exercise the parallel
  path, but production transfers never would.

### 9.2 Feature flag is default-on in cli/core but workspace-off

PFF-1 flagged this: `cli/Cargo.toml` and `core/Cargo.toml` include
`parallel-receive-delta` in their `default` features, but the workspace root
`Cargo.toml` does not. The workspace-level `default` is what `cargo build`
at the repo root uses. This means `cargo build` compiles without the feature;
`cargo build -p cli` compiles with it. The inconsistency is harmless today
(the feature is a no-op) but will matter when PIP-9.b.3 code lands.

### 9.3 No negated `#[cfg(not(feature = ...))]` gates exist

Confirmed: zero instances of `#[cfg(not(feature = "parallel-receive-delta"))]`
in the codebase. The planned cutover (PIP-9.b.2 sketch) calls for a
`#[cfg(not(...))]` block around the sequential arm, but this code has not been
written.

### 9.4 The `ReceiverDeltaPipeline` trait is disconnected from the token loop

The trait and its three implementations form a complete parallel pipeline at the
`DeltaWork` level. But the actual token-level dispatch (reading `DeltaToken`
from the wire and producing file bytes) has no abstraction point for the
parallel path. PIP-9.b.3 plans to add `apply_delta_tokens_parallel` as a
sibling function to `apply_delta_tokens`, rather than integrating through the
`ReceiverDeltaPipeline` trait. This means the trait hierarchy is parallel
infrastructure at one level of abstraction (per-file `DeltaWork` dispatch),
while the planned parallel token loop is at a different level (per-chunk
`DeltaChunk` dispatch via `ParallelDeltaApplier`). These two levels do not
compose through a shared interface.

---

## 10. Summary

| Question | Answer |
|----------|--------|
| Does the feature flag change the production token loop today? | **No** |
| Is the parallel path wired end-to-end? | **No** - substrate exists but no production caller |
| Is there a runtime fallback? | **No** - compile-time only |
| Which token loop is the cutover target? | `sync.rs:445-573` (`apply_delta_tokens`) |
| Is the production pipelined token loop affected? | **No** - PIP-9.b explicitly defers it |
| What does the feature compile? | `chunk_adapter`, `parallel_apply`, `chunk_builder` modules |
| What runs differently? | Nothing - zero production callers in either path |
| When will it change? | When PIP-9.b.3 (code) and PIP-9.b.4 (drain) land |
| What blocks the flip to default? | PIP-9.b.3 code, PIP-9.b.4 drain, PIP-9.b.5 parity test, PIP-9.f bake criterion |

---

## 11. References

- PFF-1 (PR #5049) - feature flag surface audit
- PIP-7 (#4730) - investigation proving dispatch scaffolding was dead code
- PIP-8 (#4731) - teardown of dead scaffolding
- PIP-9 (#4735) - wire-up design
- PIP-9.a (#4737) - `DeltaChunkAdapter` shape adapter
- PIP-9.b.1 (#4747) - sequential call-shape audit
- PIP-9.b.2 (#4776) - cfg-dispatch sketch
- PIP-9.b.3 (#4960) - feed loop spec (design only, no code)
- PIP-9.b.4 (#5047) - `flush_workers` drain (open, not merged)
- PIP-9.b.6 (#4958) - interop matrix CI cell
- PIP-9.c (#4738) - parallel-threshold-trip interop scenario
- PIP-9.f.1 (#4924) - bake criterion for default-on flip
- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md`
- `docs/design/pip-9b2-cfg-dispatch-sketch.md`
- `docs/design/pip-9-b-3-parallel-arm-feed-loop.md`
