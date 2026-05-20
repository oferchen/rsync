# ZSO-7 - Per-segment ZSO state under INC_RECURSE

Date: 2026-05-21
Scope: read-only audit confirming the `DeltaSignatureIndex` lifecycle is
correct per file (and therefore correct per INC_RECURSE segment) before ZSO-1
(bithash) and ZSO-3 (prune-on-match) PRs build on it.
Tracked under: #2515

## 1. Goal

ZSO-1 (`bithash`) and ZSO-3 (matched-block prune) maintain in-memory state
keyed by basis block indices. Under INC_RECURSE the sender ships file-list
signatures grouped per directory segment rather than as a single corpus, so
this audit verifies the index lifecycle is correctly per-file (which is a
strict refinement of per-segment) and documents the design constraint that
ZSO-1 and ZSO-3 PRs must respect.

The investigation is read-only - no `matching`, `index`, or `incremental.rs`
source is modified.

## 2. Per-segment signature-index lifecycle trace

### 2.1 Wire shape (sender to receiver)

INC_RECURSE only batches the file-list metadata stream. Each file's
signature still travels as an isolated `sum_head + signature_blocks` chunk
addressed by NDX, with the sender allocating its `DeltaSignatureIndex`
strictly inside the per-NDX iteration of the transfer loop. The relevant
sites are:

- `crates/transfer/src/generator/transfer/transfer_loop.rs:306-308`
  `let sum_head = SumHead::read(&mut *reader)?;` per NDX.
- `crates/transfer/src/generator/transfer/transfer_loop.rs:321`
  `let sig_blocks = read_signature_blocks(&mut *reader, &sum_head)?;` per NDX.
- `crates/transfer/src/generator/transfer/transfer_loop.rs:345-354`
  per-file `DeltaGeneratorConfig` built and `generate_delta_from_signature`
  called, owning all `sig_blocks` for exactly one file.

Segment dispatch (`SegmentScheduler::next_if_needed`) only emits more file
list entries via `encode_and_send_segment`
(`crates/transfer/src/generator/transfer/transfer_loop.rs:106-117`). It
never touches the matching pipeline.

### 2.2 Index construction site

`generate_delta_from_signature` rebuilds a fresh signature from wire bytes
and allocates a brand-new index:

- `crates/transfer/src/generator/delta.rs:91-93` -
  `generate_delta_from_signature<R: Read>(source, config) -> io::Result<DeltaScript>`
  is the single production entry from the transfer loop.
- `crates/transfer/src/generator/delta.rs:159-169` -
  `let index = DeltaSignatureIndex::from_signature(&signature, checksum_algorithm).ok_or_else(...)`
  builds the index. The variable is then immediately consumed by
  `generator.generate(source, &index)` and dropped at the end of the
  function (`delta.rs:171-179`).

The factory function lives in `crates/matching/src/index/builder.rs:52-105`
(`from_signature` -> `from_signature_with_role`). Every call here allocates
a new `CompactLookup`, a new `tag_table: vec![false; TAG_TABLE_SIZE]`, and
a new `BitHash::with_block_count(...)` (`builder.rs:74-76`). There is no
mechanism, in production code, to re-enter `populate_index` against a
recycled instance after the borrow in `delta.rs` returns.

The `Drop` impl on `DeltaSignatureIndex`
(`crates/matching/src/index/mod.rs:352-364`) fires once at function exit
and emits the matching `--debug=HASH` destroy line, confirming the
single-file scope of the allocation.

### 2.3 Other production allocations

For completeness, the other production sites all follow the same
"build, use once, drop" pattern with no cross-file or cross-segment carry:

- `crates/engine/src/concurrent_delta/strategy.rs:215-216` -
  per-work-item local copy strategy.
- `crates/engine/src/local_copy/executor/file/comparison.rs:109-112` -
  per-destination-file quick-check decision.

Neither path participates in INC_RECURSE wire flow.

### 2.4 Receiver side has no `DeltaSignatureIndex`

The receiver only reads basis-file signatures and emits the wire
`sum_head + signature blocks` for the sender to consume:

- `crates/transfer/src/receiver/transfer/sync.rs:151-167` builds a per-file
  basis signature and writes blocks once.
- `crates/transfer/src/receiver/transfer/sync.rs:215-229` calls
  `apply_delta_tokens(...)` which streams the wire delta against a
  `MapFile` plus the per-file `signature_opt`. It never materializes a
  `DeltaSignatureIndex`.

So the only side that holds the matching-engine state machine is the
generator/sender, and that state is reconstructed from wire bytes for
every NDX.

### 2.5 `MatchedBlocks` (ZSO-3 in-place infrastructure)

The matched-block bitmap that backs ZSO-3 already exists and is
instantiated per call to `DeltaGenerator::generate`:

- `crates/matching/src/generator.rs:170` -
  `let mut matched_blocks = MatchedBlocks::with_block_count(index.block_count());`
- `crates/matching/src/generator.rs:335` -
  `matched_blocks.mark_matched(match_idx);` after each emitted Copy token.

`MatchedBlocks` is stack-local to `generate()` and dropped when the
function returns - the same scope as the per-file `DeltaSignatureIndex`
borrow in `delta.rs:172`. The bitmap therefore cannot leak across files,
let alone across segments.

### 2.6 `rebuild()` exists but is never called from production

`DeltaSignatureIndex::rebuild` (`crates/matching/src/index/builder.rs:116-150`)
correctly clears `lookup`, `tag_table`, and `bithash` and re-emits the
`hashtable_growing` HASH line when the bucket count changes
(`builder.rs:142-146`). However the only call sites are unit and integration
tests; the production path always uses fresh `from_signature`. This means
the rebuild contract documented on `bithash::clear`
(`crates/matching/src/index/bithash.rs:108-116`) and `MatchedBlocks::clear`
(`crates/matching/src/index/matched_blocks.rs:99-103`) is currently a
defensive promise, not an exercised code path.

## 3. Answers to audit questions

### Question 1 - Per-segment signature handoff: discard or accumulate?

**Discard.** The `DeltaSignatureIndex` for file N is dropped before file
N+1's signature wire bytes are read. Concretely, `generate_delta_from_signature`
owns the index inside one stack frame
(`crates/transfer/src/generator/delta.rs:159-178`); the borrow lives only
until `generator.generate(source, &index)` returns, after which Rust
ownership and the `Drop` impl
(`crates/matching/src/index/mod.rs:352-364`) free every component
(`CompactLookup`, `tag_table`, `bithash`, `blocks`, `MatchedBlocks` from
the generator stack). The transfer loop then reads the next NDX's
`sum_head` and `sig_blocks` from a clean slate
(`crates/transfer/src/generator/transfer/transfer_loop.rs:307-321`).

Because INC_RECURSE never groups signatures (only file list metadata) the
per-file discard already satisfies the per-segment constraint by
construction. There is no shared per-segment matching state to corrupt.

### Question 2 - Index reuse across segments

Production code never reuses a `DeltaSignatureIndex` instance. The
`rebuild` method exists for a future per-segment recycling optimisation
and already calls `bithash.clear()` and `tag_table.iter_mut().for_each(|v|
*v = false)` (`crates/matching/src/index/builder.rs:124-127`), so the
bithash invariant for ZSO-1 is preserved if a future PR wires
`rebuild` into the hot path. Until then, ZSO-1 inherits per-file
freshness from `from_signature` allocating a new `BitHash` per call
(`crates/matching/src/index/builder.rs:76`).

### Question 3 - Prune-on-match scope (ZSO-3)

The matched-block bitmap is allocated per call to
`DeltaGenerator::generate` and sized to the current index's
`block_count()` (`crates/matching/src/generator.rs:170`). It is never
shared between files or stored on the index. ZSO-3's "first match
consumes the basis block index" invariant therefore cannot leak across
segments: every segment's first file starts with a zeroed bitmap whose
indices are valid only for that file's freshly-built index.

If a future PR moves the bitmap onto `DeltaSignatureIndex` (for example
to expose it across multi-pass match phases), the audit requires that
the bitmap be cleared inside `rebuild`
(`crates/matching/src/index/builder.rs:116-150`), exactly as
`bithash.clear()` and `tag_table` zeroing already are. The existing
`MatchedBlocks::clear` (`crates/matching/src/index/matched_blocks.rs:99-103`)
matches that contract one-for-one.

### Question 4 - Fix-shape recommendation

**ZSO-1 (bithash) - keep state on `DeltaSignatureIndex`.** The bithash is
sized from the indexed block count and probed by the same key used for
the strong-checksum lookup; co-locating it with the existing
`CompactLookup` and `tag_table` mirrors the upstream `match.c`
hashtable+tag pairing. The structure is already present as the
`bithash: BitHash` field on `DeltaSignatureIndex`
(`crates/matching/src/index/mod.rs:70`) and is correctly handled in
both `from_signature` and `rebuild`. No relocation is needed; ZSO-1's
remaining work is downstream (probe wiring, debug emissions,
benchmarking) and not in scope for ZSO-7.

**ZSO-3 (prune-on-match) - keep `MatchedBlocks` on the generator stack
frame, NOT on `DeltaSignatureIndex`.** Rationale:

1. The bitmap is mutated by every successful match, while the index
   represents the immutable basis. Stuffing mutable per-session state
   onto a `Clone + Debug` value invites silent bugs when callers
   `index.clone()` and end up with shared-but-divergent bitmaps.
2. The bitmap's bit count is bound to the basis size at the moment
   matching begins. Holding it on the index ties its lifetime to the
   index allocation; under any future scheme that recycles an index
   across files via `rebuild`, the bitmap would need a synchronized
   `clear` + `resize`. Keeping it on the stack avoids that coupling.
3. `MatchedBlocks` is borrowed by `find_match_*_filtered` only as
   `Option<&MatchedBlocks>` (`crates/matching/src/index/mod.rs:140,
   201`), so passing it as a parameter is already the established
   ergonomic.

Concrete shape for ZSO-3 PR: keep the current declaration at
`crates/matching/src/generator.rs:170`, gate the
`Some(&matched_blocks)` filter argument on the existing
`prune_matched` flag (already wired at
`crates/matching/src/generator.rs:253, 380`), and drop the bitmap at
function exit by leaving it on the stack. No `DeltaSignatureIndex`
field is needed.

## 4. Latent bugs

**None blocking ZSO-1 or ZSO-3.** The audit found two minor
robustness observations:

1. The `Clone` derive on `DeltaSignatureIndex`
   (`crates/matching/src/index/mod.rs:54`) carries the `bithash`,
   `tag_table`, and `blocks` deep-copies, but also the `role` and
   `last_traced_size` fields. Each clone fires an independent
   `trace_destroyed` line in `Drop`
   (`crates/matching/src/index/mod.rs:357-363`). This is documented as
   intended behaviour, but is worth keeping in mind for ZSO-3 if any
   future caller starts cloning indices to pre-stage matched bitmaps
   per worker.
2. `DeltaSignatureIndex::rebuild` is dead code in production today.
   Adding `#[cfg(test)]`-gated unit coverage that exercises the
   `bithash.clear()` and `tag_table` zeroing on rebuild would prevent
   a silent regression if a future PR wires the recycling path. This
   is optional - the existing `bithash` and `MatchedBlocks` unit tests
   already cover the clear invariants in isolation.

Neither observation blocks ZSO-1 or ZSO-3. The per-segment lifecycle
constraint is satisfied by the per-file constraint, which the current
code already enforces by allocation locality.

## 5. Top-line answer

The per-segment lifecycle is **correct today by virtue of being
per-file**. `DeltaSignatureIndex` is built, used, and dropped strictly
inside a single NDX iteration of the sender's transfer loop; the
matched-block bitmap that backs ZSO-3 has the same per-call scope inside
`DeltaGenerator::generate`. No latent fix is required before ZSO-1 or
ZSO-3 start.

## 6. Constraint that ZSO-1 and ZSO-3 PRs must respect

- The bithash must be reset (cleared, not freed) on any future
  per-segment `rebuild` call. The existing
  `crates/matching/src/index/builder.rs:127` line already does this -
  do not remove it.
- The matched-block bitmap MUST stay tied to the same scope as the
  current `DeltaSignatureIndex` borrow inside `DeltaGenerator::generate`.
  Promoting it onto the index requires synchronized clearing in
  `rebuild` and explicit reasoning about the `Clone` derive.
- Wire compatibility: neither optimisation may change the bytes the
  sender emits on the wire (`docs/design/zsync-bithash.md` and
  `docs/design/zsync-prune.md` already document this contract). The
  per-segment audit adds no new wire-shape constraint - INC_RECURSE
  segment boundaries are entirely a file-list-metadata concept and do
  not surface in the delta token stream.

## 7. References

- `crates/matching/src/index/mod.rs:54-77` - `DeltaSignatureIndex`
  struct layout and `bithash` field.
- `crates/matching/src/index/mod.rs:352-364` - `Drop` impl emitting
  the `--debug=HASH` destroy line.
- `crates/matching/src/index/builder.rs:52-105` - `from_signature`
  fresh allocation path.
- `crates/matching/src/index/builder.rs:116-150` - `rebuild` per-segment
  recycling path (not currently called from production).
- `crates/matching/src/index/bithash.rs:108-116` - `BitHash::clear`
  per-segment reset hook.
- `crates/matching/src/index/matched_blocks.rs:99-103` -
  `MatchedBlocks::clear` per-segment reset hook.
- `crates/matching/src/generator.rs:170` - per-call `MatchedBlocks`
  allocation.
- `crates/matching/src/generator.rs:253, 380` - `prune_matched` gate
  for ZSO-3's filtered probes.
- `crates/transfer/src/generator/transfer/transfer_loop.rs:306-354` -
  per-NDX `sum_head`, `sig_blocks`, and delta dispatch.
- `crates/transfer/src/generator/delta.rs:159-178` -
  `DeltaSignatureIndex::from_signature` ownership scope.
- `crates/transfer/src/generator/segments.rs:71-126` -
  `SegmentScheduler` confirming INC_RECURSE segments group only
  file-list metadata.
- `crates/transfer/src/receiver/transfer/sync.rs:151-229` - receiver
  per-file signature emission and `apply_delta_tokens` call, with no
  `DeltaSignatureIndex` allocation on this side.
- `docs/design/zsync-bithash.md:187, 463-464` - the parent design
  note that flagged INC_RECURSE per-segment rebuilds as a follow-up.
- `docs/design/zsync-prune.md:122, 443-444` - the parent prune note
  that flagged INC_RECURSE per-segment rebuilds as a follow-up.
