# PIP-9 - parallel-receive-delta production wire-up

Date: 2026-05-22
Status: **OPEN** - design pass that follows the PIP-8 dead-scaffolding
teardown (#4731). Implementation is split across the sub-tasks listed in
Section 5 and lands behind the existing `parallel-receive-delta` feature
flag.

This document supersedes the earlier closure-style note at
`docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` for the
purpose of architecture and punch-list planning. That file remains as the
upstream acceptance reference; this file is what subsequent sub-task PRs
cite.

## 1. Current state (post-PIP-8)

### 1.1 Where the production receive path consumes delta tokens today

The receiver still drives a per-file sequential token loop. The single
production reader is
`apply_delta_tokens()` at
`crates/transfer/src/receiver/transfer/sync.rs:445-573`, called once per
file from the per-entry transfer loop at
`crates/transfer/src/receiver/transfer/sync.rs:241-253`.

The function:

- Reads tokens from a `TokenReader` via `read_token()`
  (`crates/transfer/src/receiver/transfer/sync.rs:459`).
- Resolves `Literal` payloads either through an inline read or
  `try_borrow_exact()` (`sync.rs:497-517`).
- Resolves `BlockRef` tokens against a `MapFile` basis
  (`sync.rs:518-570`).
- Writes through `write_chunk()` straight into a buffered file writer.
- Updates a single per-file `ChecksumVerifier` and compares the
  receiver-side digest against the sender's at the `End` token
  (`sync.rs:460-495`).

There is no chunking abstraction, no fan-out, and no thread pool: the
loop owns its writer and runs entirely on the receive thread.

### 1.2 Where `DeltaWork` / `DeltaResult` flow today

`DeltaWork` and `DeltaResult` only appear inside the delta-pipeline
substrate. The list of writer sites is:

- `crates/transfer/src/delta_pipeline/parallel.rs:7` (type imports).
- `crates/transfer/src/delta_pipeline/parallel.rs:162-184`
  (`ReceiverDeltaPipeline::submit_work` / `poll_result` / `drain_remaining`
  on `ParallelDeltaPipeline`).
- `crates/transfer/src/delta_pipeline/sequential.rs` (the sequential
  pipeline implementation).
- `crates/transfer/src/delta_pipeline/mod.rs` (`ReceiverDeltaPipeline`
  trait surface).

Every reader of those producers lives under `crates/transfer/src/delta_pipeline/tests/`,
`crates/engine/tests/`, or `crates/engine/benches/`. Nothing in
`crates/transfer/src/receiver/`, `crates/transfer/src/pipeline/`, or
`crates/transfer/src/transfer_ops/` reads them. This is the same
"1 writer / 0 readers" pattern PIP-7 traced for `delta_pipeline` -
the field is dead state at runtime; only test scaffolding consumes the
producers.

### 1.3 Where `ParallelDeltaApplier` lives and what it exposes

`ParallelDeltaApplier` lives at
`crates/engine/src/concurrent_delta/parallel_apply.rs`. Post-PIP-8 the
public surface is:

- `ParallelDeltaApplier::new(concurrency)` (`parallel_apply.rs:501`).
- `ParallelDeltaApplier::with_strategy(concurrency, strategy)`
  (`parallel_apply.rs:516`) - the constructor the receiver should use
  once the negotiated `ChecksumStrategy` is in hand.
- `ParallelDeltaApplier::register_file(ndx, writer)`
  (`parallel_apply.rs:562`) - registers a per-file `Box<dyn Write + Send>`.
- `ParallelDeltaApplier::apply_one_chunk(chunk)`
  (`parallel_apply.rs:618`) - single-chunk dispatch. Note RJN-1's
  finding: the `rayon::join(verify, || ())` shape here is not
  multi-chunk parallelism.
- `ParallelDeltaApplier::apply_batch_parallel(chunks)`
  (`parallel_apply.rs:650`) - the actual fan-out site. RJN-3 (#4686)
  established this as the only real parallelism path.
- `ParallelDeltaApplier::finish_file(ndx)`
  (`parallel_apply.rs:703`) - blocks on `flush_workers(ndx)`
  (`parallel_apply.rs:712`) and returns the `Box<dyn Write + Send>` for
  receiver-side commit (sparse finish, checksum verify, temp rename).
- `ParallelDeltaApplier::bytes_written(ndx)`
  (`parallel_apply.rs:684`).
- Typed `ParallelApplyError` variants at `parallel_apply.rs:66-129`
  (ATU-3 wiring), including `ApplierStillReferenced`, `SlotPoisoned`,
  `UndrainedChunks`, and `ChecksumMismatch`.

`DeltaChunk` (`parallel_apply.rs:152-225`) is the per-chunk
input type. `DeltaChunk::with_expected_strong()` attaches the BR-3i.d
per-chunk strong-checksum digest that the applier verifies on a rayon
worker.

The applier holds files in a `DashMap<FileNdx, Arc<SlotBarrier>>`
(BR-3j.c), so per-file registration and lookup do not serialise on a
single mutex. The slot mutex still serialises writes on a given file -
see Section 2.4.

### 1.4 What PIP-3 actually wired vs what it claimed

PIP-3 + PIP-5 (#4666 - "enable parallel receive-delta by default via
Path B heuristic") shipped:

- `ReceiverContext::enable_parallel_receive_delta()` - a setter that
  swapped a `Box<dyn ReceiverDeltaPipeline>` into the
  `delta_pipeline` field on the receiver context.
- `dispatch_receiver_strategy()` / `select_receiver_strategy()` and the
  `PARALLEL_RECEIVE_*` thresholds.
- A switch in `tools/ci/run_interop.sh` to a default-on feature set.

What PIP-3 did **not** wire was a reader. PIP-7 (#4730) confirmed by
grep that the swapped `delta_pipeline` field had 1 writer
(`ReceiverContext::set_delta_pipeline`) and 0 readers. The only
production-observable behaviour the feature flag enabled was the
`DeltaConsumer::spawn` side effect inside `ParallelDeltaPipeline::new`
(`crates/engine/src/concurrent_delta/consumer/mod.rs:186`), which is the
suspected cause of the `parallel-threshold-trip` receiver corruption
(PIP-4, #4720). PIP-8 (#4731) removed the dispatch glue and left the
`ParallelDeltaApplier`, `ParallelDeltaPipeline`, and `DeltaConsumer`
types compiled. The feature flag is currently a no-op
(`Cargo.toml:72-76`, `crates/transfer/Cargo.toml:111`,
`crates/engine/Cargo.toml:112`).

Net effect: PIP-3 claimed "token_loop -> ParallelDeltaApplier
integration" but the production token loop has never reached
`ParallelDeltaApplier`. PIP-9 is the actual integration.

## 2. Target architecture

### 2.1 Proposed insertion point

The cutover happens at `apply_delta_tokens()` -
`crates/transfer/src/receiver/transfer/sync.rs:445-573`. The current
call site at `sync.rs:241-253` becomes a dispatcher that selects
between the sequential token-loop body (kept verbatim) and the new
parallel applier path. Selection is purely the
`parallel-receive-delta` feature flag - no runtime threshold, no env
knob (PIP-8 removed those, and Section 6 keeps them out).

Implementation sketch:

```text
crates/transfer/src/receiver/transfer/sync.rs:241

#[cfg(feature = "parallel-receive-delta")]
apply_delta_via_parallel_applier(
    reader,
    &mut output,
    &mut sparse_state,
    &mut basis_map,
    signature_opt.as_ref(),
    &mut token_reader,
    &mut token_buffer,
    &mut checksum_verifier,
    self.checksum_seed,
    file_ndx,
    &applier,
    &mut total_bytes,
)?;

#[cfg(not(feature = "parallel-receive-delta"))]
apply_delta_tokens(...)?;
```

The `applier: &ParallelDeltaApplier` is held on the receiver context
(replacing the deleted `delta_pipeline` field). It is constructed once
per transfer with the negotiated `ChecksumStrategy` via
`ParallelDeltaApplier::with_strategy()` (`parallel_apply.rs:516`).

### 2.2 The `DeltaWork` -> `DeltaChunk` adapter

`DeltaWork` is the existing SPSC pipe shape; `DeltaChunk` is what
`ParallelDeltaApplier::apply_one_chunk` and
`apply_batch_parallel` consume (`parallel_apply.rs:152-225`). The
adapter is the new piece PIP-9.a delivers. It lives next to
`ChunkBuilder` at
`crates/transfer/src/delta_pipeline/chunk_builder.rs` (the module
already calls into `ParallelDeltaApplier::apply_one_chunk` from tests
- see line 95 / line 288 / line 399 in that file).

Per-token translation rules, byte-identical to the sequential loop:

1. **`TokenReaderDeltaToken::Literal(LiteralData::Ready(data))`** ->
   `DeltaChunk::literal(file_ndx, seq, data)`. `seq` is the per-file
   monotonic sequence the chunk builder increments. Strong checksum is
   `None` unless the sender attached a per-chunk digest (BR-3i.d
   wired the basis-block path; literal payloads stay `None` for now).
2. **`TokenReaderDeltaToken::Literal(LiteralData::Pending(len))`** ->
   read into `token_buffer` exactly as today (`sync.rs:504-515`), then
   emit the same literal chunk shape as case 1.
3. **`TokenReaderDeltaToken::BlockRef(block_idx)`** -> resolve the
   basis-block bytes through `MapFile::map_ptr()` exactly as
   `sync.rs:561` does, then `DeltaChunk::matched(file_ndx, seq, data)`.
   When the basis signature carries a per-block digest (BR-3i.d
   pipeline), attach it via `.with_expected_strong()` so the applier's
   `verify_chunk` runs the real comparison rather than the
   skip-on-`None` path documented at `parallel_apply.rs:168-182`.
4. **`TokenReaderDeltaToken::End`** -> drop the in-flight batch,
   call `applier.finish_file(file_ndx)` to barrier-block on
   `flush_workers` (`parallel_apply.rs:712`), retrieve the
   `Box<dyn Write + Send>` writer, and run the existing checksum-verify
   block from `sync.rs:460-495` against the receiver-side
   `ChecksumVerifier`. The sender's end-of-file digest read
   (`reader.read_exact(...)` at `sync.rs:463`) and the
   `for_algorithm_seeded` reset (`sync.rs:467-470`) move into the
   adapter unchanged.

The receiver-side `ChecksumVerifier` must stay updated in submission
order. The simplest correctness-preserving option: keep updating
`checksum_verifier` on the receive thread inside the adapter (the
loop already owns the chunk bytes before they ship to the applier),
so the applier's parallel verify is for the strong per-chunk digest
only. The full-file MD4/MD5 verify keeps its existing serialised
path.

Wire-format parity: the adapter never reads new bytes from the wire,
never changes the `token_reader.see_token()` call shape
(`sync.rs:567`), and never reorders end-of-file checksum reads.
Section 6 captures the no-protocol-changes constraint.

### 2.3 RJN-3 fan-out caller

`apply_chunk_parallel` was renamed in RJN-3 (#4686) and the audit
`docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md`
established `apply_batch_parallel` as the only call that actually
fans out across the rayon pool. The token-loop wire-up therefore
batches chunks before dispatching:

- The adapter accumulates chunks in a `Vec<DeltaChunk>` up to a small
  bound (target: a single sender token window, capped at e.g.
  `ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY = 64` from
  `parallel_apply.rs:489`).
- On flush, the adapter calls
  `applier.apply_batch_parallel(chunks)` (`parallel_apply.rs:650`).
- The single-chunk `apply_one_chunk` path is kept for the
  end-of-file tail when the batch is `len == 1`, since
  `rayon::join(verify, || ())` is cheaper than spinning up
  `into_par_iter` for one item.

`apply_batch_parallel` already returns the first `io::Error` it sees
and surfaces typed `ParallelApplyError::ChecksumMismatch`
(`parallel_apply.rs:114-128`) when an attached
`expected_strong` does not match, so the adapter just propagates
`io::Result`. The receiver translates the typed error into the
existing role-trailer-prefixed message at
`sync.rs:474-493`.

### 2.4 Writer serialisation - what we resolve vs punt

The standing concern in
`project_apply_batch_write_serial.md` is that `apply_batch_parallel`
verifies in parallel via `into_par_iter`, then serialises the actual
writes through the per-file slot mutex
(`parallel_apply.rs:669-675`). Pipelined verify+write overlap was the
ABW-2/3 design.

ABW-3 (#4673) shipped the analysis and deferred the implementation
pending BR-3j.f bench evidence (see
`docs/design/abw-3-closure-2026-05-21.md`). PIP-9 does **not**
reopen ABW-3. The wire-up uses the existing `apply_batch_parallel`
write loop as-is; the serial-write tail is documented as an explicit
follow-up. If BR-3j.f shows a measurable receive-throughput win on a
real workload, ABW-2 can ship later as a transparent applier-internal
change with no wire-up impact.

PIP-9's only correctness commitment is: per-file byte order is
preserved (handled by the per-file `ReorderBuffer` already inside the
applier, see `parallel_apply.rs:25-33`), and the receiver-side
`ChecksumVerifier` sees bytes in the same order the sequential loop
would have written them (handled by Section 2.2's receive-thread
update rule).

## 3. Interop test plan

### 3.1 Reintroduce `parallel-threshold-trip`

The scenario was removed by PR #4725 (PIP-7 mitigation); see the
removal note at
`tools/ci/run_interop.sh:9630-9633`. PIP-9.c reintroduces it in the
same scenario array immediately above that note, at
`tools/ci/run_interop.sh:9629` (where the `acls` row currently
terminates the default list).

The scenario:

- **Setup**: create `parallel_threshold/file_1.txt` ...
  `parallel_threshold/file_120.txt`, each containing
  `pt-payload-NNN\n` (the original PIP-4 shape; see
  `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`
  reproduction block).
- **Trigger**: 120 files exceeds the historical
  `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD = 100`. Since PIP-8 removed
  the threshold, the scenario instead asserts the
  `--features parallel-receive-delta` build dispatches every file
  through the parallel applier; the 120-file fixture stays as
  defensive coverage of multi-file batches and the original
  reproducer.
- **Assertion**: compute sha256 of `parallel_threshold/file_1.txt`
  in both source and destination and require byte-identical match.
  The PIP-7 corruption manifested as wrong bytes on `file_1.txt`
  only, so the sha256 of that file is the load-bearing check. Also
  assert that the dest directory's file count matches the source
  and that every file's sha256 matches (defensive sweep).
- **Matrix**: both `up:` (upstream sender -> oc-rsync receiver) and
  `oc:` (oc-rsync sender -> upstream receiver). The PIP-4 failure
  hit both directions.

### 3.2 Required `dist`-profile CI cell

The PIP-7 investigation confirmed the bug only reproduced under
`cargo build --profile dist` (LTO + `panic=abort` + `opt-level=z`).
The container repro under `cargo build --release` never failed.

PIP-9.d adds a CI matrix cell that runs the interop suite against a
build produced by `cargo build --profile dist
--features parallel-receive-delta`. The cell extends the existing
"Interop Validation" workflow and runs `tools/ci/run_interop.sh` with
the binary the dist build produced. The cell is **required** for
merge so the corruption regression cannot resurface silently.

The standard `release`-profile cell continues to exercise the default
feature set (still parallel-receive-delta-off pending PIP-9.f), so
both profiles are covered.

### 3.3 Non-regression

Every other scenario in `tools/ci/run_interop.sh` keeps its existing
assertions. The `--features parallel-receive-delta` build runs the
full scenario list with zero skipped cells. Specifically the
fixtures from `setup_comprehensive_src` (the `hello.txt` /
`binary.dat` / `large.dat` mix) keep their byte-identical checks
against both `up:` and `oc:` peers.

## 4. Feature flag transition

PIP-9.a / PIP-9.b ship the wire-up behind the existing
`parallel-receive-delta` flag. The flag remains opt-in. Default
builds keep the sequential path so production users see byte-for-byte
identical behaviour to today.

PIP-9.f flips the flag back into the default feature set
(`Cargo.toml:72-76`, `crates/transfer/Cargo.toml:111`,
`crates/engine/Cargo.toml:112`) - the work PIP-5 originally
claimed and #4725 reverted. The flip gates on:

1. The dist-profile interop cell from Section 3.2 stays green for
   at least three consecutive CI runs across every required-check
   matrix (`fmt+clippy`, `nextest (stable)`, Windows, macOS, Linux
   musl, plus the new dist cell).
2. The `release`-profile interop cell with
   `--features parallel-receive-delta` is also green for three
   consecutive runs.
3. PIP-7 is closed as fixed via PIP-9.e once both 1 and 2 hold for
   the same three CI windows.
4. PIP-6's end-to-end parallel-vs-sequential bench harness
   (`crates/engine/benches/parallel_receive_delta_perf.rs` plus the
   PIP-6 scaffolding at
   `docs/design/pip-6-end-to-end-parallel-vs-sequential-bench-2026-05-21.md`)
   shows non-negative receive-side throughput vs sequential on the
   reference workload. PIP-9 is wire-up only - it does not need a
   throughput win to ship, but the flip in PIP-9.f must not regress
   throughput.

PIP-9 itself does **not** flip the default. The flip is a separate
follow-up PR (PIP-9.f) that cites the three-CI-window evidence in
its description.

## 5. Punch list

The following sub-tasks implement this design. Each is a separate PR
behind the `parallel-receive-delta` feature flag (except PIP-9.f,
which removes the flag from the gate).

1. **PIP-9.a** - implement the `DeltaWork` -> `DeltaChunk` adapter.
   Extends `crates/transfer/src/delta_pipeline/chunk_builder.rs`
   with the per-token translation rules from Section 2.2. No call
   site yet; the adapter is exercised by new unit tests that match
   the sequential `apply_delta_tokens` output byte-for-byte across
   literal / pending-literal / block-ref / mixed shapes.
2. **PIP-9.b** - rewire the production token loop. Adds the
   feature-gated branch at
   `crates/transfer/src/receiver/transfer/sync.rs:241` per Section
   2.1, constructs the `ParallelDeltaApplier` on the receiver context
   with the negotiated `ChecksumStrategy`, batches via
   `apply_batch_parallel`, and uses `finish_file` to barrier-block
   on `flush_workers` before the receiver-side checksum compare.
3. **PIP-9.c** - reintroduce `parallel-threshold-trip` in
   `tools/ci/run_interop.sh` per Section 3.1, including the
   sha256-of-`file_1.txt` assertion and the comprehensive-fixtures
   sweep. Replaces the removal note at
   `tools/ci/run_interop.sh:9630-9633`.
4. **PIP-9.d** - add a CI matrix cell that builds with
   `cargo build --profile dist --features parallel-receive-delta`
   and runs the interop suite. Marks the cell required for merge.
5. **PIP-9.e** - close PIP-7 (#2588 in the tracker) once PIP-9.b
   plus PIP-9.c plus PIP-9.d are green for three consecutive CI
   runs. Updates
   `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`
   from `MITIGATED-PENDING-PIP-9` to `FIXED-IN-PIP-9` and removes
   the open-ticket banners from `README.md`, `CHANGELOG.md`,
   `docs/design/parallel-receive-delta-default-on.md`,
   `docs/design/pip-4-closure-2026-05-21.md`, and
   `docs/design/br-6-sign-off-check-in-2026-05-21.md`.
6. **PIP-9.f** - flip `parallel-receive-delta` back into the
   default feature set on the workspace `Cargo.toml`,
   `crates/cli/Cargo.toml`, `crates/core/Cargo.toml`,
   `crates/transfer/Cargo.toml`, and `crates/engine/Cargo.toml`.
   Gated on PIP-9.e plus the throughput non-regression check from
   Section 4.

## 6. What we are NOT doing

- **No wire-protocol changes.** PIP-9 reads the same `MSG_DATA` /
  `MSG_DELTA` token stream the sequential loop reads. No new
  capability flags, no negotiation-prologue bits, no varint shape
  changes. This rule comes from
  `feedback_no_wire_protocol_features.md` and is non-negotiable for
  upstream interop.
- **No new IPC primitives.** The applier already owns its
  per-file slots via `DashMap` and its per-file mutex/reorder
  buffer via `SlotBarrier`. PIP-9 reuses the existing SPSC
  `FileMessage` pipe between receive and disk-commit threads
  (`crates/transfer/src/pipeline/spsc.rs`,
  `crates/transfer/src/disk_commit/thread.rs`) only for the
  whole-file / commit signalling that already lives on it; the
  delta-chunk path runs through the applier directly.
- **No reintroduction of `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD`
  or `PARALLEL_RECEIVE_BYTES_THRESHOLD`.** PIP-8 deleted both
  (`docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`
  section "Resolution"). The `parallel-receive-delta` feature flag
  is the only dispatch gate. Bringing the constants back would
  re-introduce the dead `1 writer / 0 readers` shape PIP-7
  identified.
- **No new env knobs.** `OC_RSYNC_FORCE_PARALLEL` was removed in
  PIP-8 and stays removed. PIP-9 does not add a replacement.
- **No ABW-2 pipelined writes in this design.** ABW-3 closure
  defers verify+write overlap to a later cycle pending BR-3j.f
  bench evidence. PIP-9 uses `apply_batch_parallel`'s existing
  serial-write tail and leaves that optimisation to its own PR.
- **No changes to the sequential code path.** The default build
  remains byte-identical to today's receiver behaviour until
  PIP-9.f flips the flag.

## References

### Code citations

- `crates/transfer/src/receiver/transfer/sync.rs:241-253` - cutover
  point for `apply_delta_tokens`.
- `crates/transfer/src/receiver/transfer/sync.rs:445-573` - sequential
  token loop body.
- `crates/transfer/src/delta_pipeline/parallel.rs:162-184` -
  `submit_work` / `poll_result` / `drain_remaining` (writers, no
  production reader today).
- `crates/transfer/src/delta_pipeline/chunk_builder.rs:95-225` -
  existing `ChunkBuilder` test scaffolding; adapter lives next door.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:152-225` -
  `DeltaChunk` shape.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:485-704` -
  `ParallelDeltaApplier` public surface for PIP-9.b.
- `crates/engine/src/concurrent_delta/consumer/mod.rs:186` -
  `DeltaConsumer::spawn` (side effect PIP-7 traced).
- `tools/ci/run_interop.sh:9630-9633` - `parallel-threshold-trip`
  removal note (replaced by PIP-9.c).
- `Cargo.toml:72-76`, `crates/transfer/Cargo.toml:105-111`,
  `crates/engine/Cargo.toml:107-112` - no-op feature definitions.

### Related PRs and docs

- #4666 (PIP-3+5), #4720 (PIP-4), #4725 (PIP-7 mitigation),
  #4730 (PIP-7 note), #4731 (PIP-8 teardown), #4686 (RJN-3),
  #4673 (ABW-3 deferral), #4657 (PIP-1 audit).
- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` -
  upstream PIP-9 acceptance note this design implements.
- `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md` -
  PIP-7 investigation and PIP-8 resolution.
- `docs/design/parallel-receive-delta-application.md` - umbrella
  design with per-file ordering invariants.
- `docs/design/parallel-receive-delta-default-on.md` - default-on flip
  rationale (historical until PIP-9.f).
- `docs/design/pip-6-end-to-end-parallel-vs-sequential-bench-2026-05-21.md` -
  bench harness for the PIP-9.f non-regression gate.
- `docs/design/abw-3-closure-2026-05-21.md` - ABW-3 deferral PIP-9
  inherits.
- `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md` -
  the call-site catalogue.
