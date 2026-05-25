# PIP-9.b.3 - parallel arm `DeltaWork` -> `DeltaChunk` feed loop

Date: 2026-05-25
Status: design-only spec. No source files change as part of this task. The
implementation PR consumes this spec verbatim; the `flush_workers` drain at
the file boundary lands under PIP-9.b.4; the parity tests under PIP-9.b.5.

## 1. Scope

PIP-9.b.3 specifies the `DeltaWork` -> `DeltaChunk` feed loop that lives in
the parallel arm of the cfg-gated dispatch sketched by PIP-9.b.2 (PR #4776,
`docs/design/pip-9b2-cfg-dispatch-sketch.md`). It defines:

- the **source structure** the feed loop consumes (the receiver's
  `TokenReader` stream that today drives `apply_delta_tokens` at
  `crates/transfer/src/receiver/transfer/sync.rs:445-573`);
- the **destination structure** the feed loop produces
  (`DeltaChunk` in `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:159`);
- the **conversion logic** (the PIP-9.a `DeltaChunkAdapter` /
  `delta_work_to_chunk` free function in
  `crates/engine/src/concurrent_delta/chunk_adapter.rs:167-203`, plus the
  per-file builder in `crates/transfer/src/delta_pipeline/chunk_builder.rs`);
- the **back-pressure model** (delegated to
  `ParallelDeltaApplier::apply_one_chunk`, which serialises writes behind a
  per-file mutex and a per-file `ReorderBuffer`);
- the **error-propagation path** (early return; three error classes -
  wire parse, adapter, applier).

This task does NOT ship code. The PIP-9.b.3 implementation PR consumes this
spec without further design discussion. Memory note inline:
`[[project_parallel_interop_parity_gap]]`.

## 2. Pre-conditions

Already in master at the time of writing:

- **`DeltaWork`** struct, `crates/engine/src/concurrent_delta/types.rs:65`.
  Fields: `ndx: FileNdx`, `sequence: u64`, `dest_path: PathBuf`,
  `basis_path: Option<PathBuf>`, `source_path: Option<PathBuf>`,
  `target_size: u64`, `literal_bytes: u64`, `matched_bytes: u64`,
  `kind: DeltaWorkKind`. Constructors: `whole_file`, `delta`,
  `delta_with_source`.
- **`DeltaChunk`** struct,
  `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:159`. Fields:
  `ndx: FileNdx`, `chunk_sequence: u64`, `data: Vec<u8>`, `is_literal: bool`,
  `expected_strong: Option<ChecksumDigest>`. Constructors: `DeltaChunk::literal`,
  `DeltaChunk::matched`, builder `.with_expected_strong(...)`.
- **PIP-9.a adapter** (PR #4737, merged):
  `crates/engine/src/concurrent_delta/chunk_adapter.rs:167` - zero-state
  `DeltaChunkAdapter` plus free function `delta_work_to_chunk` at line 201.
  Total in-memory transform from `(&DeltaWork, ChunkPayload)` to
  `DeltaChunk`; the `ndx` is taken from `work`, the payload fields move
  verbatim, `expected_strong` round-trips byte-for-byte.
- **PIP-9.b.1 audit** (PR #4747, merged):
  `docs/design/pip-9b-call-shape-audit.md` - documents the sequential call
  site at `sync.rs:241-253` and the ten equivalence invariants the parallel
  arm must preserve byte-for-byte.
- **PIP-9.b.2 cfg-gated dispatch sketch** (PR #4776, merged):
  `docs/design/pip-9b2-cfg-dispatch-sketch.md` - chose Variant A (single
  cfg if-else at the call site) and pinned the `ReceiverContext`
  placement of `Option<ParallelDeltaApplier>` for the applier's lifetime.
- **`ParallelDeltaApplier`** API surface
  (`crates/engine/src/concurrent_delta/parallel_apply/mod.rs`):
  - `register_file(ndx, writer: Box<dyn Write + Send>) -> io::Result<()>`
    at line 499;
  - `apply_one_chunk(chunk: DeltaChunk) -> io::Result<()>` at line 552;
  - `bytes_written(ndx) -> io::Result<u64>` at line 578;
  - `flush_workers(ndx) -> io::Result<()>` at `parallel_apply/drain.rs:146`;
  - `finish_file(ndx) -> io::Result<Box<dyn Write + Send>>` at
    `parallel_apply/drain.rs:49` (bakes the `flush_workers` barrier in
    front of the `Arc::try_unwrap` so callers never have to sequence them).
- **`ChunkBuilder`** per-file builder
  (`crates/transfer/src/delta_pipeline/chunk_builder.rs:215`,
  `pub fn next_chunk(&mut self, token: &DeltaToken, basis_bytes: Vec<u8>)
  -> Result<Option<DeltaChunk>, ChunkBuilderError>`) - populates
  `expected_strong` from the negotiated `FileSignature` for `BlockRef`
  tokens; literal tokens leave `expected_strong = None`. Holds the
  per-file `FileNdx` and the monotonic `chunk_sequence` counter.
- **`parallel-receive-delta`** Cargo feature - wired through
  `engine/Cargo.toml`, `transfer/Cargo.toml`, workspace `Cargo.toml`.
  Currently off by default; PIP-9.f.2 flips the default after the bake
  window closes (PIP-9.f.1 criterion, PIP-9.f.3 monitor).

## 3. Feed-loop architecture

The feed loop is a thin in-line driver inside the parallel arm of
`sync.rs:241-253`. PIP-9.b.2's sketch names it `apply_delta_tokens_parallel`
and places it as a sibling to the existing `apply_delta_tokens` (sync.rs:445).
The body is the same `loop { match read_token(...) }` skeleton; the
difference is the per-token action.

```
+---------------------+
|  TokenReader        |   produces DeltaToken (Literal | BlockRef | End)
+----------+----------+
           |
           v
+---------------------+
|  ChunkBuilder       |   wraps the per-file FileSignature; assigns
|  ::next_chunk       |   chunk_sequence; populates expected_strong
+----------+----------+
           |  Option<DeltaChunk>
           v
+---------------------+
|  ParallelDeltaApplier|  verify (rayon) + ingest under per-file Mutex
|  ::apply_one_chunk   |  per-file ReorderBuffer replays in sequence
+----------+----------+
           |  on End:
           v
+---------------------+
|  ::flush_workers     |  drains in-flight workers for this ndx
|  (PIP-9.b.4 wires)   |  before checksum_verifier swap & writer reclaim
+---------------------+
```

- **Producer.** `TokenReader::read_token(reader)` is the same wire decoder
  the sequential arm uses. Source: same network bytes, same decompressor
  dictionary, same `see_token` discipline for BlockRefs.
- **Adapter.** The PIP-9.a converter is invoked indirectly via
  `ChunkBuilder::next_chunk(&DeltaToken, basis_bytes: Vec<u8>)`. Per
  PIP-9.a's contract this is a pure in-memory transform. Literal tokens
  produce a literal `DeltaChunk` with `expected_strong = None`; BlockRef
  tokens produce a matched `DeltaChunk` with `expected_strong = Some(...)`
  sourced from the negotiated `FileSignature`. End tokens yield
  `Ok(None)`; the feed loop breaks the inner loop and proceeds to the
  drain step.
- **Consumer.** `ParallelDeltaApplier::apply_one_chunk(chunk)` runs the
  CPU-bound `verify_chunk` step on the rayon pool (via `rayon::join`,
  `mod.rs:564`), then commits under the per-file `Mutex` with replay
  through the per-file `ReorderBuffer`. Per-file byte order is preserved
  by the reorder buffer; cross-file ordering at the wire-output layer is
  irrelevant inside one file's apply.
- **Back-pressure.** `apply_one_chunk` is synchronous and serialises
  writes through the per-file mutex. When the per-file reorder buffer is
  full of out-of-order chunks (the producer is single-threaded so this
  never happens here), `slot.ingest` would block; in the single-producer
  shape the buffer is always drained as the producer's monotonic
  `chunk_sequence` advances. The natural back-pressure mirrors the
  sequential arm's `write_chunk(output, ...)` call: it blocks until the
  destination accepts the bytes, no chunk is ever dropped.
- **Termination.** When `TokenReader::read_token` returns `End` (mirrors
  `sync.rs:460`), the feed loop calls `applier.flush_workers(ndx)` to
  drain every in-flight verify before the verifier swap. The drain is
  PIP-9.b.4's wiring; this spec only stipulates the call site.

## 4. Concurrency model

The threads / tasks involved:

- **Token-loop thread (producer).** Reads from the wire, drives the
  `TokenReader`, emits `DeltaToken` values. Single-threaded, owned by
  the receiver's `run_sync`.
- **Feed-loop thread.** Same OS thread as the token-loop thread (see
  decision below). Pulls each `DeltaToken`, runs it through the
  per-file `ChunkBuilder`, calls `apply_one_chunk`. Single producer
  for the applier's per-file slot.
- **`ParallelDeltaApplier` internal rayon workers (consumer pool).**
  Already exists. `apply_one_chunk` schedules the strong-checksum
  verify on a rayon worker via `rayon::join`. The serial write step
  runs inline on the calling thread under the per-file mutex. Pool
  sizing and tuning are out of scope; covered by PIP-9.e (worker-pool
  tuning, PR #4906).

Two arrangements were considered:

- **(a) Same thread.** The token-loop calls the feed-loop inline. Each
  `DeltaToken` is converted to a `DeltaChunk` and submitted before the
  next token is read.
- **(b) Separate threads with `crossbeam_channel`.** The token-loop
  pushes into a bounded channel; a dedicated feed thread pulls and
  submits.

**Decision: pick (a).** Justification:

- Simpler. Matches the sequential arm's structure (one `loop`, one
  thread, no channel). The reviewer can see the parallel arm as a
  direct sibling of `apply_delta_tokens`.
- The parallelism that matters lives entirely inside
  `ParallelDeltaApplier`: rayon scheduling of `verify_chunk`, per-file
  mutex, per-file `ReorderBuffer`. The feed loop does not need its own
  parallelism to feed work into that pool because `apply_one_chunk` is
  cheap on the caller's side (the rayon join is short-lived on a worker;
  the per-file mutex hold is the only serial cost the caller pays).
- Lower overhead. No channel allocation, no extra thread, no message
  copy. The `DeltaToken` and `DeltaChunk` already share buffer
  ownership patterns (the literal payload's `Vec<u8>` moves through).
- Option (b) is a future optimisation if profiling shows the inline
  conversion blocks the wire reader. PIP-9.b.3 does not gate that
  optimisation; it only ships the inline form.

## 5. API surface

The feed-loop function lives next to the sequential helper in
`crates/transfer/src/receiver/transfer/sync.rs` (NOT in a new module;
keep the diff local to the cutover file per PIP-9.b.2's Variant A
recommendation). Proposed signature:

```rust
// New helper, sibling to `apply_delta_tokens` (sync.rs:445). Mirrors the
// parameter list so reviewers can diff the two side by side.
#[cfg(feature = "parallel-receive-delta")]
fn apply_delta_tokens_parallel<R: Read>(
    reader: &mut crate::reader::ServerReader<R>,
    sparse_state: &mut Option<SparseWriteState>,
    basis_map: &mut Option<MapFile>,
    signature_opt: Option<&engine::signature::FileSignature>,
    token_reader: &mut TokenReader,
    token_buffer: &mut TokenBuffer,
    checksum_verifier: &mut ChecksumVerifier,
    checksum_seed: i32,
    file_path: &std::path::Path,
    total_bytes: &mut u64,
    ndx: FileNdx,
    applier: &ParallelDeltaApplier,
) -> io::Result<()> {
    let signature = signature_opt.ok_or_else(|| io::Error::new(
        io::ErrorKind::InvalidData,
        format!("parallel arm requires a basis signature for {file_path:?}"),
    ))?;
    let mut builder = ChunkBuilder::new(ndx, signature);

    loop {
        match token_reader.read_token(reader)? {
            TokenReaderDeltaToken::End => {
                // PIP-9.b.4 wires the drain here; on success the
                // checksum verifier swap below mirrors the sequential
                // arm at sync.rs:467-494.
                applier.flush_workers(ndx)?;
                return verify_and_swap_checksum(
                    reader,
                    checksum_verifier,
                    checksum_seed,
                    file_path,
                );
            }
            TokenReaderDeltaToken::Literal(literal) => {
                let data = resolve_literal(reader, token_buffer, literal)?;
                let chunk = builder
                    .next_chunk(&DeltaToken::Literal(data.clone()), Vec::new())
                    .map_err(chunk_builder_error_to_io)?
                    .expect("Literal token always yields a chunk");
                checksum_verifier.update(&chunk.data);
                *total_bytes += chunk.data.len() as u64;
                applier.apply_one_chunk(chunk)?;
            }
            TokenReaderDeltaToken::BlockRef(block_idx) => {
                let basis_bytes = resolve_block(
                    signature, basis_map.as_mut(), block_idx, file_path,
                )?;
                let chunk = builder
                    .next_chunk(
                        &DeltaToken::BlockRef { index: block_idx },
                        basis_bytes,
                    )
                    .map_err(chunk_builder_error_to_io)?
                    .expect("BlockRef token always yields a chunk");
                checksum_verifier.update(&chunk.data);
                token_reader.see_token(&chunk.data)?;
                *total_bytes += chunk.data.len() as u64;
                applier.apply_one_chunk(chunk)?;
            }
        }
    }
}
```

Implementation notes:

- The `output: &mut BufWriter<File>` parameter is gone. The writer was
  moved into the applier via `register_file(ndx, Box::new(adapter))`
  before the call (see PIP-9.b.2 section 4 for the adapter shape and the
  `finish_file` recapture pattern). The applier owns the writer for
  the duration of the parallel arm; the caller reclaims it via
  `finish_file(ndx) -> Box<dyn Write + Send>` after the loop returns.
- `verify_and_swap_checksum`, `resolve_literal`, `resolve_block`, and
  `chunk_builder_error_to_io` are small extractions of the existing
  sequential helper's per-arm logic. They keep the parallel helper
  short enough to diff cleanly against `apply_delta_tokens`. PIP-9.b.3
  may keep them inline if the diff stays under ~120 LoC.
- The `expect` calls reflect `ChunkBuilder::next_chunk`'s contract:
  Literal and BlockRef tokens always produce a chunk; only the `End`
  variant returns `Ok(None)`. PIP-9.b.3 may switch to a match-and-bail
  shape if reviewers prefer no `expect` in the feed loop body.
- The signature drift from the sequential `apply_delta_tokens` is two
  added parameters (`ndx`, `applier`) and one removed (`output`). The
  rest is identical so the call sites line up.

## 6. Error propagation

Three error classes; all early-return with the original error type so
the existing `io::Result<()>` shape is preserved.

- **Wire parse errors** (from `token_reader.read_token(reader)`):
  early-return with the `io::Error` the reader produced. Same behaviour
  as the sequential arm (sync.rs:459). The applier is left holding
  partial state for the file; the caller's `Drop` path (the
  `ReceiverContext`'s applier slot map) reclaims it on the next
  `register_file` for the same `ndx` or on overall context drop.
- **Adapter errors** (from `ChunkBuilder::next_chunk`): early-return
  with a converted `io::Error`. Per PIP-9.a's contract
  (`chunk_adapter.rs:167-203`) and `ChunkBuilder`'s `ChunkBuilderError`
  variants (`chunk_builder.rs:55-87`), the recoverable cases are:
  - `BlockIndexOutOfBounds` - malformed stream; abort the file.
  - `BasisLenMismatch` - basis bytes do not match the recorded length;
    abort the file (a `ChecksumMismatch` from the applier would
    otherwise surface the same condition for the wrong reason).
  Both map to `io::Error::new(io::ErrorKind::InvalidData, ...)` with
  the existing role-trailer + error-location format used elsewhere in
  sync.rs (e.g. sync.rs:474-481).
- **Applier errors** (from `apply_one_chunk` and `flush_workers`):
  early-return with the `io::Error` the applier produced. The
  `ParallelApplyError` variants
  (`parallel_apply/mod.rs:73-136`) carry enough context for an
  operator to locate the failure (`ndx`, `chunk_sequence`, call-site
  tag); the `From<ParallelApplyError> for io::Error` impl preserves
  the typed message as the `Display` payload (`mod.rs:138-146`). The
  applier is responsible for tearing down its workers cleanly on error;
  the caller does not need to call `finish_file` after an error.

The feed loop holds no rollback state of its own. The `total_bytes`,
`checksum_verifier`, and `sparse_state` mutations are observable side
effects on the call frame; on error the caller (sync.rs around the
cutover site) discards the partial counters and returns the error up
the stack, matching the sequential arm's behaviour.

## 7. Memory + back-pressure model

Explicit invariants:

- **Feed loop holds no chunk buffer.** Each `DeltaChunk` is converted
  in-place and submitted immediately. No `Vec<DeltaChunk>` accumulates;
  no SPSC queue inside the feed loop.
- **Back-pressure is delegated.** `ParallelDeltaApplier::apply_one_chunk`
  blocks on:
  - the rayon `verify_chunk` join (CPU-bound, short-lived);
  - the per-file `Mutex` (the only serial cost the caller pays);
  - the per-file `ReorderBuffer` ingest (drains immediately in the
    single-producer case because `chunk_sequence` is monotonic).
  All three are bounded by the work the call has in flight for this
  `ndx`; none can grow with the wire bandwidth.
- **Memory bound.** `(worker count) * (max chunk size)` per applier
  instance, unchanged from the sequential arm. The applier already
  bounds its per-file reorder buffer
  (`parallel_apply/mod.rs:510` constructs `FileSlot::new(writer,
  self.per_file_reorder_capacity)`). The feed loop adds no allocation
  beyond what the chunk itself owns.
- **No drop semantics.** If the wire stalls, the feed loop blocks at
  the next `read_token`. If the applier stalls, the feed loop blocks
  at the next `apply_one_chunk`. Neither side drops chunks; both apply
  natural blocking back-pressure on the producer (the wire reader).
  This matches the sequential arm's `BufWriter` blocking on the OS
  write buffer.

## 8. Wire-byte parity

This task does not change wire format. The argument:

- Same `TokenReader`. Same network bytes. Same decompressor dictionary
  state (the `token_reader.see_token(...)` call on `BlockRef` is
  preserved exactly as in the sequential arm at sync.rs:567).
- Same `DeltaWork` -> `DeltaChunk` conversion as PIP-9.a (PR #4737),
  invoked via `ChunkBuilder::next_chunk` which is a pure in-memory
  transform: `ndx` moves verbatim, `data` moves verbatim, `is_literal`
  is set from the token variant, `expected_strong` is pulled from the
  negotiated `FileSignature` for BlockRef tokens. Zero buffer copies
  beyond what the sequential arm already does (the BufWriter write).
- Same `ParallelDeltaApplier` semantics as PIP-3 (already production
  wired-up): the per-file `ReorderBuffer` enforces submission-order
  writes; the per-file `Mutex` enforces single-writer; the
  `verify_chunk` strong-checksum compares against
  `expected_strong` when present.
- The destination bytes hit the writer in the same order as the
  sequential arm. The strong checksum at the end of the file
  (`verify_and_swap_checksum`) sees the same byte stream.

Conclusion: zero wire-byte risk if PIP-9.a (#4737) and PIP-3 are
already validated. PIP-9.c (PR #4738, `parallel_threshold_trip.rs`)
adds sha256 byte-identity assertions; PIP-9.b.6 (PR #4958) runs the
full upstream interop matrix under the parallel feature flag.

## 9. Test plan for the implementation PR

PIP-9.b.5 owns the parallel-path tests. Spec acceptance for PIP-9.b.3:

- **`parallel_threshold_trip.rs`** (PR #4738, PIP-9.c) - the sha256
  byte-identity scenario exercises the parallel path end-to-end against
  the sequential path's output. PIP-9.b.5 extends this scenario to
  cover:
  - whole-file transfers (no basis); should fall through to the
    sequential arm because the parallel arm requires a basis signature;
  - delta transfers with single-block files (BlockRef-only stream);
  - delta transfers with single-literal files (Literal-only stream);
  - delta transfers with mixed Literal+BlockRef interleavings;
  - delta transfers triggering `flush_workers` between files
    (multi-file submission to the same applier).
- **PIP-9.b.6 parallel interop matrix** (PR #4958) - runs the full
  upstream interop matrix under `--features parallel-receive-delta` so
  any wire-format drift surfaces against rsync 3.0.9, 3.1.3, 3.4.1,
  3.4.2.
- **PIP-9.c sha256 byte-identity** (PR #4738) remains authoritative
  for the per-file destination bytes invariant. PIP-9.b.3 does not
  add a new byte-identity test; it inherits this one.
- **Existing `parallel_apply/mod.rs` unit tests** - cover
  `register_file`, `apply_one_chunk`, `flush_workers`, `finish_file`
  in isolation. No new tests there; the new logic lives in
  `apply_delta_tokens_parallel`.
- **PIP-9.b.5** should also add a negative test: an out-of-bounds
  BlockRef token must surface `ChunkBuilderError::BlockIndexOutOfBounds`
  through the `chunk_builder_error_to_io` mapping, not a generic
  applier `ChecksumMismatch`. Catches the diagnostic regression PIP-9.a
  warned about.

## 10. Rollback / disable

The `parallel-receive-delta` Cargo feature controls whether the
parallel arm is reachable. The non-feature build elides every line of
the feed loop at compile time (Variant A cfg gate per PIP-9.b.2).

If a regression appears post-merge:

- **Step 1 - emergency revert of the default flip.** If PIP-9.f.2 has
  already flipped the default, revert that single workspace
  `Cargo.toml` change (PIP-9.f.2's domain). The cutover patch and the
  feed loop both stay in the tree; only the default feature set
  reverts. The bake-window monitor at PR #4949 detects the trigger
  conditions.
- **Step 2 - investigate, fix, restart bake window.** The fix lands in
  a follow-up PR; the PIP-9.f bake window restarts from day zero per
  the criterion at PR #4924 (`docs/design/pip-9-f-1-bake-criterion.md`).
- **Step 3 - if the feed loop itself is the regression source.**
  Revert the PIP-9.b.3 PR. The cfg gate guarantees the sequential
  arm is unaffected; the revert restores the previous build's
  parallel-arm shape (which was a no-op stub). No data path
  through the parallel arm is reachable after revert.

Operators with the feature compiled in but a runtime regression can
rebuild without the feature flag. There is no runtime `--no-parallel-
delta` toggle by design (PIP-9.b.2 section 3.2 rejected Variant B as
premature; reintroduce that variant if a runtime toggle becomes
necessary).

## 11. Cross-references

- **PIP-9.b.1** audit - PR #4747, `docs/design/pip-9b-call-shape-audit.md`.
- **PIP-9.b.2** cfg-gated dispatch sketch - PR #4776,
  `docs/design/pip-9b2-cfg-dispatch-sketch.md`.
- **PIP-9.a** `DeltaWork` -> `DeltaChunk` adapter - PR #4737,
  `crates/engine/src/concurrent_delta/chunk_adapter.rs:167-203`.
- **PIP-9.c** sha256 byte-identity scenario - PR #4738,
  `tests/parallel_threshold_trip.rs`.
- **PIP-9.d** CI matrix cell - PR #4736,
  `parallel-receive-delta + dist profile matrix`.
- **PIP-9.e** worker-pool tuning knobs - PR #4906.
- **PIP-9.f.1** bake criterion -
  `docs/design/pip-9-f-1-bake-criterion.md`, PR #4924.
- **PIP-9.f.3** bake-window monitor -
  `docs/operations/pip-9-f-3-bake-window-monitor.md`, PR #4949.
- **PIP-9.b.6** parallel interop matrix workflow - PR #4958
  (open at the time of writing).
- Memory note: `[[project_parallel_interop_parity_gap]]` - feature-gated
  scaffolding (default off until the bake window flip); production
  token_loop currently still uses the sequential DeltaWork path until
  PIP-9.b.3 lands the parallel arm; full upstream interop suite is
  validated through the parallel path under PIP-9.b.6.
