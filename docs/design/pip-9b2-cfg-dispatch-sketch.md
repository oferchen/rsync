# PIP-9.b.2 - cfg-gated dispatch sketch for the `token_loop` cutover

Date: 2026-05-23
Status: design-only sketch. No source files change as part of this task. The
implementation lands under PIP-9.b.3; the `flush_workers` drain at the file
boundary lands under PIP-9.b.4.

Inputs:

- PIP-9.b.1 audit (`docs/design/pip-9b-call-shape-audit.md`) - fixes the
  sequential call shape and the 10 equivalence invariants the parallel arm
  must preserve byte-for-byte.
- PIP-9.a adapter (`crates/engine/src/concurrent_delta/chunk_adapter.rs`) -
  pure in-memory shape conversion from `DeltaWork` + `ChunkPayload` to
  `DeltaChunk`. Zero state, no I/O, no threads.
- `ParallelDeltaApplier` scaffold
  (`crates/engine/src/concurrent_delta/parallel_apply.rs`) - per-file slot
  map keyed by `FileNdx`; `register_file` / `apply_one_chunk` /
  `apply_batch_parallel` / `flush_workers` / `finish_file` public API.
- `parallel-receive-delta` Cargo feature - wired through
  `engine/Cargo.toml:112`, `transfer/Cargo.toml:111`, workspace
  `Cargo.toml:72-76`. Currently a no-op gate; PIP-9.b is the cutover.

## 1. The cutover site

The sequential call lives at `crates/transfer/src/receiver/transfer/sync.rs`,
lines 241-253:

```
241            apply_delta_tokens(
242                reader,
243                &mut output,
244                &mut sparse_state,
245                &mut basis_map,
246                signature_opt.as_ref(),
247                &mut token_reader,
248                &mut token_buffer,
249                &mut checksum_verifier,
250                self.checksum_seed,
251                &file_path,
252                &mut total_bytes,
253            )?;
```

The callee (sync.rs:445-573) is a single `loop { match read_token(...) }`
that writes per chunk through `write_chunk(output, sparse_state, ...)` and
updates `checksum_verifier`. There is no SPSC pipe on this path: bytes go
straight into the per-file `BufWriter<File>` (`output` at sync.rs:214) until
the `End` token is observed. The streaming token loop at
`crates/transfer/src/transfer_ops/token_loop.rs:143,187` is a *different*
caller (the disk-commit pipeline) that does push `FileMessage::Chunk(buf)`
over the SPSC pipe; PIP-9.b only cuts over the sync.rs:241-253 site per the
PIP-9.b.1 audit. The streaming path stays sequential; if a future task wants
the parallel arm there too, it follows the same shape but with a different
adapter on the `FileMessage::Chunk` boundary.

The "current sequential code that calls `FileMessage::Chunk(buf)` over the
SPSC pipe" referenced in the task brief is the streaming-pipeline callee
(token_loop.rs:143) and is *not* the cutover site. The sync.rs callee writes
to `output` directly. The cutover is the synchronous path.

## 2. The cfg-gated dispatch shape

The two arms are siblings inside the per-file loop body. The sequential arm
is the call as it stands today. The parallel arm fetches a per-file
[`ParallelDeltaApplier`] handle, drives the existing tokeniser to produce
`(DeltaWork, ChunkPayload)` pairs, runs them through the PIP-9.a adapter,
and dispatches each `DeltaChunk` to the applier:

```
#[cfg(feature = "parallel-receive-delta")]
{
    // Parallel arm. The applier handle is constructed once per
    // ReceiverContext (see section 4) and pinned for the file's lifetime.
    let applier = self
        .parallel_applier
        .as_ref()
        .expect("PIP-9.b: applier is Some when feature is enabled");
    let work = DeltaWork::from_basis_or_whole(
        ndx,
        file_path.clone(),
        basis_path_opt.clone(),
        file_entry.size(),
    );
    applier.register_file(ndx, Box::new(BufWriterAdapter::new(&mut output)))?;
    apply_delta_tokens_parallel(
        reader,
        &mut sparse_state,
        &mut basis_map,
        signature_opt.as_ref(),
        &mut token_reader,
        &mut token_buffer,
        &mut checksum_verifier,
        self.checksum_seed,
        &file_path,
        &mut total_bytes,
        &work,
        applier,
    )
    .map_err(parallel_apply_error_to_io)?;
}
#[cfg(not(feature = "parallel-receive-delta"))]
{
    // Sequential arm - the existing call, unchanged.
    apply_delta_tokens(
        reader,
        &mut output,
        &mut sparse_state,
        &mut basis_map,
        signature_opt.as_ref(),
        &mut token_reader,
        &mut token_buffer,
        &mut checksum_verifier,
        self.checksum_seed,
        &file_path,
        &mut total_bytes,
    )?;
}
```

`apply_delta_tokens_parallel` is the symmetric helper the parallel arm calls
in PIP-9.b.3; its body runs the same `loop { read_token }` skeleton as the
sequential `apply_delta_tokens`, but on each Literal/BlockRef it builds a
`ChunkPayload` via the PIP-9.a adapter and submits the resulting
`DeltaChunk` through `applier.apply_one_chunk(...)`. The `End` token
triggers `applier.flush_workers(ndx)` (PIP-9.b.4) before the verifier swap.
The brief's "submit through `parallel_applier.apply_chunk(chunk)?`"
spelling collapses to `apply_one_chunk` (the verbatim public API name on
[`ParallelDeltaApplier`]).

The arms produce the same observable side effects on every invariant listed
in PIP-9.b.1 section 6:

- `total_bytes` final value matches (sequential `+=` vs parallel reducer).
- `checksum_verifier` consumes the same byte stream in the same order
  (parallel arm enforces destination-offset order via the per-file
  reorder buffer and the post-loop `flush_workers` drain).
- `sparse_state.pending_zeros` ends at the same value (sparse handling
  stays on the commit thread; section 4 covers the writer adapter).
- `total_bytes`, `checksum_verifier`, `sparse_state`, and `basis_map`
  belong to the call frame and are not aliased across threads; only
  resolved `data: Vec<u8>` payloads cross the worker boundary.

## 3. Variant decisions: simple cfg if-else vs trait + two impls

Two variants were considered.

### 3.1 Variant A - single cfg if-else at the call site (chosen)

Shape: two sibling code blocks gated by `#[cfg(feature = "parallel-receive-delta")]`
and `#[cfg(not(feature = ...))]` respectively, as shown in section 2.

Pros:

- Smallest possible diff at the cutover site. One file changes; the gate
  is local to the per-file loop body.
- Trivial to read in `git blame` - the sequential arm stays verbatim;
  the parallel arm is additive.
- Compile-time dead-code elimination removes whichever arm is not
  selected; no runtime dispatch cost.
- Easiest to bisect: PIP-9.b.3 lands the parallel arm under the cfg
  gate; PIP-9.b.4 wires `flush_workers`; PIP-9.b.5/.6 validate; PIP-9.f
  flips the default. Each step is a self-contained cfg arm change.
- Mirrors the existing cfg pattern in the same file (sync.rs:203-211 for
  Unix sandboxed temp-create vs non-Unix temp-create, sync.rs:312-332
  for sandboxed renameat vs std::fs::rename). The reviewer's eye is
  already trained on this shape.

Cons:

- Conditional code lives at every cutover site. There is currently one
  cutover site; if a future task fans the cutover out to N call sites,
  the gate is duplicated N times.
- No runtime toggle. A future `--no-parallel-delta` flag cannot
  selectively disable the parallel arm in a build that has the
  feature compiled in.

### 3.2 Variant B - trait + two impls dispatched at construction time

Shape: define a `DeltaApplyExecutor` trait with one method
`fn apply(&mut self, ...) -> io::Result<()>`; provide a `SequentialExecutor`
(today's `apply_delta_tokens`) and a `ParallelExecutor` (PIP-9.b.3); the
`ReceiverContext` holds `Box<dyn DeltaApplyExecutor>` chosen at construction.

Pros:

- One call site at sync.rs:241-253 (`self.executor.apply(...)`).
- Runtime toggle is a feature-free choice between the two impls.
- Cleaner extension point if a third strategy (e.g. io_uring-direct)
  later appears.

Cons:

- More code: a new trait, two impls, the construction-time choice, the
  field on `ReceiverContext`.
- Dyn-dispatch per call (negligible at file granularity, but the
  argument list is non-trivial - eleven items plus `&self`).
- Higher review surface for the cutover patch: trait extraction is
  reshuffling that obscures the actual parallel/sequential wiring
  difference under a coat of indirection.
- Premature: there is no `--no-parallel-delta` flag in scope, no
  third strategy proposed. Adding the trait now is speculative
  generality (YAGNI).

### 3.3 Recommendation

Adopt **Variant A** (single cfg if-else at the call site) for PIP-9.b.3.
Reserve Variant B for the day a `--no-parallel-delta` runtime toggle or a
third strategy lands. Migration from A to B is mechanical: extract a trait,
two impls, replace the cfg with construction-time selection - the change is
local to sync.rs plus a new module under `crates/transfer/src/receiver/`.

## 4. State threading - where does the `ParallelDeltaApplier` live?

The applier's lifecycle does not fit inside a single file iteration: every
slot the applier owns lives until `finish_file(ndx)` completes (which
internally calls `flush_workers(ndx)`; see parallel_apply.rs:703-772). The
file boundary is the moment the applier releases its writer back to the
caller. The applier itself must outlive every per-file iteration so its
internal rayon scheduling, shard map, and barrier state survive across
files.

Three placement candidates:

### 4.1 Per-file (constructed inside the loop)

Rejected: throws away rayon worker affinity, re-allocates the slot map
shards every file, drops the wakeup discipline. Defeats the purpose of
the applier.

### 4.2 Lazy-init on first chunk

Rejected: makes the cfg arm's first chunk pay a one-time construction
cost it cannot amortise, complicates error handling when the lazy init
itself fails partway through a file, and requires an `Option` field
either way.

### 4.3 `Option<ParallelDeltaApplier>` on `ReceiverContext` (chosen)

Construct once at `ReceiverContext::run_sync` entry (sync.rs around line
50) under `#[cfg(feature = "parallel-receive-delta")]`; store as
`Option<ParallelDeltaApplier>` so the non-feature build pays zero size and
zero construction cost. Per-file iteration: `register_file(ndx, writer)`
at the start of the parallel arm, `flush_workers(ndx)` + `finish_file(ndx)`
at the end. The applier survives across files. `Drop` of `ReceiverContext`
naturally drops the applier, which `drain_inflight()` on its own internal
drop semantics (the slot map drains as `Arc<SlotBarrier>` references go
to zero).

The chosen shape is:

```
struct ReceiverContext {
    // ...existing fields...
    #[cfg(feature = "parallel-receive-delta")]
    parallel_applier: Option<ParallelDeltaApplier>,
}
```

The `Option` is `Some` for the duration of `run_sync` and `None` outside;
the `expect` in the parallel arm (section 2) is documented by the cfg
gate.

The writer the applier owns is *not* the BufWriter directly. The applier's
`register_file` expects `Box<dyn Write + Send>` and runs writes on its
internal commit thread; the receiver's `BufWriter<File>` is `!Send` only
because of the File handle on some platforms but is actually `Send` here.
PIP-9.b.3 may pass `BufWriter` directly; if the borrow check rejects (the
`&mut output` form precludes moving), introduce a thin adapter that owns
the `BufWriter` and surfaces the post-loop file back via
`finish_file -> Box<dyn Write + Send>` -> downcast or recapture pattern.
The audit invariant 3 (writer flush boundaries) requires the BufWriter
flush stays at sync.rs:271; the adapter design must not flush per chunk.

## 5. `flush_workers` integration

The parallel arm must drain all in-flight workers for `ndx` before:

1. Replacing `checksum_verifier` on the `End` token (audit invariant 8,
   risk 7) - the End handling rebuilds the verifier in place via
   `ChecksumVerifier::for_algorithm_seeded`; any worker still feeding
   `verifier.update(...)` after the swap hashes the *next* file's prefix
   into the *previous* file's digest position.
2. Calling `applier.finish_file(ndx)` to recover the writer - the applier
   already bakes `flush_workers(ndx)` into `finish_file` (parallel_apply.rs:712),
   so the second drain is implicit; the explicit drain at point 1 is the
   one PIP-9.b.4 must wire.
3. The caller's post-loop sequence at sync.rs:255-374 (sparse finish,
   fsync, backup rename, temp-rename, metadata apply). Bytes must be on
   disk and ordered before the temp-rename commits the file.

The file boundary in the parallel arm sits *inside* `apply_delta_tokens_parallel`,
at the `End` token handler. That is the exact analogue of sync.rs:460-495
in the sequential arm. PIP-9.b.4 wires the drain there:

```
// Inside apply_delta_tokens_parallel (sketch only):
TokenReaderDeltaToken::End => {
    // PIP-9.b.4 drain: every chunk the wire produced for this file has
    // been dispatched. Wait for the applier's per-file workers to
    // finish their verify+write before swapping the verifier and
    // surfacing the file's final digest.
    applier.flush_workers(ndx)?;            // (1) drain
    // ... read expected digest from wire ...
    let _writer = applier.finish_file(ndx)?; // (2) recover writer; internal flush_workers is a no-op now
    // ... verifier swap + digest compare exactly as sequential arm ...
}
```

The drain is what makes risks 1, 4, 6, 7, and 10 in PIP-9.b.1 section 7
unreachable. Without it, the cfg arm is unsound.

PIP-9.b.4 is the implementation task. PIP-9.b.2 (this doc) sketches the
call site only - one `applier.flush_workers(ndx)?` before the verifier
swap, one `applier.finish_file(ndx)?` to recover the writer.

## 6. Error propagation

The sequential arm returns `Result<(), io::Error>`. Every error inside
`apply_delta_tokens` is already `io::Error` (see audit section 3).

The parallel arm's APIs return mixed shapes:

- `ParallelDeltaApplier::register_file` -> `io::Result<()>`.
- `ParallelDeltaApplier::apply_one_chunk` -> `io::Result<()>`.
- `ParallelDeltaApplier::flush_workers` -> `io::Result<()>`.
- `ParallelDeltaApplier::finish_file` -> `io::Result<Box<dyn Write + Send>>`.
- Internally, `ParallelApplyError` (parallel_apply.rs) is the typed error
  variant; it converts into `io::Error` via `impl From<ParallelApplyError>
  for io::Error` (already present in the applier scaffold).

So the parallel arm naturally yields `io::Result<()>` at every call site.
The cfg boundary in section 2 needs no extra conversion: every
`applier.*?` already produces `io::Error`. The brief's
"convert `ApplyError` to `io::Error` at the cfg boundary" reduces to "no
op - the applier already does this for us." The map-shim
`parallel_apply_error_to_io` in the section 2 sketch is therefore a
documentation placeholder; PIP-9.b.3 will delete it once the typed
`From` impl is confirmed in place.

The error-message string contract from audit section 3 - `file_path`,
`error_location!()`, `role_trailer::receiver()` - must be preserved.
PIP-9.b.3 will run a side-by-side `cargo nextest` test that captures the
exact error string for a known-bad input on each arm and asserts they
match.

## 7. Bisect-friendliness

The cutover lands in five separately reviewable PRs. Each leaves the tree
compile-green and CI-green on both `--no-default-features` and
`--features parallel-receive-delta` builds.

| Task     | Change                                                                                              | Cfg behaviour after merge                                                                                                                              |
|----------|-----------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| PIP-9.b.2 | This doc. No source change.                                                                         | Both cfg arms behave as today (sequential).                                                                                                            |
| PIP-9.b.3 | Add `apply_delta_tokens_parallel` + cfg if-else at sync.rs:241-253 + `parallel_applier` field on `ReceiverContext`. | `--features parallel-receive-delta` enables parallel arm; default build is unchanged.                                                                  |
| PIP-9.b.4 | Wire `flush_workers(ndx)` + `finish_file(ndx)` inside the End handler of `apply_delta_tokens_parallel`. | `--features parallel-receive-delta` becomes sound (drain present); default build is unchanged.                                                         |
| PIP-9.b.5 | Add the `parallel_threshold_trip` test under `#[cfg(feature = "parallel-receive-delta")]`.          | Test runs in feature CI; default build skips.                                                                                                          |
| PIP-9.b.6 | Run the full upstream interop matrix (3.0.9, 3.1.3, 3.4.1, 3.4.2) under feature build.              | Interop pass becomes green prerequisite for PIP-9.f.                                                                                                   |

Cfg gate stays compile-safe at each landing:

- After PIP-9.b.3, the feature-on build is *technically* unsound (no drain)
  but the test suite at this stage does not exercise the file-end edge
  case beyond single-file inputs. Document this in the PIP-9.b.3 PR body.
  The window is closed by PIP-9.b.4.
- The non-feature default build remains exactly as today after every step.
  CI's required matrix (fmt+clippy, nextest, Windows, macOS, Linux musl)
  runs both feature configurations.

## 8. What this leaves to subsequent tasks

- **PIP-9.b.3** - implement the parallel arm. Lands `apply_delta_tokens_parallel`,
  the cfg if-else at sync.rs:241-253, the `parallel_applier:
  Option<ParallelDeltaApplier>` field on `ReceiverContext`, and the
  writer-adapter contract from section 4.3.
- **PIP-9.b.4** - wire `flush_workers(ndx)` at the file boundary inside
  the End handler of `apply_delta_tokens_parallel`; pair with
  `finish_file(ndx)` to recover the writer.
- **PIP-9.b.5** - run the `parallel_threshold_trip` test that exercises
  the dispatch threshold under feature CI (verifies the parallel arm is
  reached when the receiver-side chunk count crosses the threshold and
  that the sequential arm covers below-threshold inputs).
- **PIP-9.b.6** - run the full upstream interop matrix under
  `--features parallel-receive-delta`; verify byte-identical outputs vs
  sequential arm for every supported upstream version.
- **PIP-9.e** - close PIP-7 (the prior corruption-on-cutover finding)
  once PIP-9.b.6 is green.
- **PIP-9.f** - flip default-on after N consecutive green CI cycles
  across all required matrices (fmt+clippy, nextest stable, Windows
  stable, macOS stable, Linux musl stable) plus the parallel-feature
  CI matrix.

## 9. References

- `crates/transfer/src/receiver/transfer/sync.rs:241-253` - cutover site.
- `crates/transfer/src/receiver/transfer/sync.rs:445-573` - sequential
  callee body.
- `crates/transfer/src/receiver/transfer/sync.rs:577-588` - `write_chunk`.
- `crates/engine/src/concurrent_delta/chunk_adapter.rs` - PIP-9.a
  in-memory shape adapter.
- `crates/engine/src/concurrent_delta/parallel_apply.rs` -
  `ParallelDeltaApplier` public API (`register_file`,
  `apply_one_chunk`, `apply_batch_parallel`, `flush_workers`,
  `finish_file`, `drain_inflight`).
- `docs/design/pip-9b-call-shape-audit.md` - PIP-9.b.1 audit: inputs,
  outputs, side effects, equivalence invariants, risk catalogue.
- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` -
  PIP-9 design.
- `Cargo.toml:72-76`, `crates/transfer/Cargo.toml:111`,
  `crates/engine/Cargo.toml:112` - feature wiring for
  `parallel-receive-delta`.
