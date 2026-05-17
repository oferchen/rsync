# Delta intra-file parallel rolling-hash for large files

Design note for issue #2206. Scoped to the single hypothesis that a single
large basis file (>= ~100 MB) could be matched faster by splitting the
input stream into overlapping windows and running N rolling-hash matchers
in parallel, one per window, instead of the current single sequential
loop.

This note is **purely a design proposal**. It does not introduce a CLI
flag, change the wire protocol, or add new public APIs. The token stream
emitted by any future parallel matcher MUST be byte-identical to the
serial matcher's output (same COPY/LITERAL sequence in basis-offset
order). Without that guarantee `crates/protocol/tests/golden/` would
diverge from upstream-recorded wire dumps and the change would land as a
silent incompatibility.

Companion notes already merged that this proposal builds on:

- `docs/design/intra-file-parallelism.md` - broader survey of three
  intra-file strategies (spatial split, pipeline staging, SIMD batch
  verify). This note narrows to the spatial-split (window) strategy
  applied to the rolling-hash search.
- `docs/design/upstream-match-token-reference.md` - upstream `match.c`
  and `token.c` reference walk-through (the doc tracked by upstream
  reference PR #4202).
- `docs/design/zsync-inspired-matching.md` - parallel pruning, bithash,
  seq-match audit. Same invariants apply to this proposal.

## 1. Where the sequential scan lives today

The rolling-hash matcher is a single tight loop in `crates/matching/`,
driven by a single caller in `crates/transfer/`. The hot path is
end-to-end serial per file.

**Matcher** (`crates/matching/`):

- `crates/matching/src/generator.rs:73-83` `DeltaGenerator::new` -
  constructs the per-call matcher. Defaults to `DEFAULT_BUFFER_LEN` (the
  read-ahead chunk size).
- `crates/matching/src/generator.rs:141-145` `DeltaGenerator::generate`
  - the public entry point that takes any `impl Read` plus a
  `&DeltaSignatureIndex` and returns a `DeltaScript`. This is the only
  function any production path calls; everything below is private to
  this loop.
- `crates/matching/src/generator.rs:147-149` allocates one
  `RingBuffer::with_capacity(block_len)` and one
  `RollingChecksum::new`, both stack-sized scalars (~16 B for the
  checksum, `block_len` bytes for the ring).
- `crates/matching/src/generator.rs:196-263` the main do-while loop.
  Each iteration reads exactly one input byte
  (`buffer[buffer_pos]`), pushes it through the ring, rolls the
  checksum (`crates/matching/src/generator.rs:213` `rolling.roll`),
  and on each fully-populated window probes the signature index via
  `index.find_match_slices_filtered`
  (`crates/matching/src/generator.rs:259, :263`).
- `crates/matching/src/generator.rs:163-174` carries the `want_i`
  adjacent-match hint and the `MatchedBlocks` pruning bitmap. Both are
  single-thread state owned by this loop.
- `crates/matching/src/index/mod.rs:180-187` `find_match_slices` -
  the per-window probe. Stateless against the index
  (`&self` only), so the index itself is trivially shareable across
  threads via `Arc`. The tag table
  (`crates/matching/src/index/mod.rs:45 TAG_TABLE_SIZE = 1 << 16`) and
  `CompactLookup` are both read-only after construction.
- `crates/matching/src/index/mod.rs:241-263` `check_block_match_slices`
  - the `want_i` hint verifier (upstream `match.c:144-190`).
- `crates/matching/src/ring_buffer.rs:34-44` `RingBuffer` - per-matcher
  sliding window. One instance per parallel worker would be needed.
- `crates/checksums/src/rolling/checksum/mod.rs:83-89` `RollingChecksum`
  + `crates/checksums/src/rolling/checksum/mod.rs:333` `roll(outgoing,
  incoming)` - the single-byte advance. Carries 16 bytes of state and
  must be private to each window worker.

**Caller** (`crates/transfer/`):

- `crates/transfer/src/generator/delta.rs:171-178` is the only call
  site to `DeltaGenerator::generate` in production. The matcher runs
  once per file on the sender side and there is no fan-out.
- `crates/transfer/src/transfer_ops/streaming.rs:74`
  `process_file_response_streaming<R: Read>` is the receiver side
  that drives `apply_delta_stream`
  (`crates/transfer/src/delta_apply/applicator.rs:436`) and would
  receive the same token stream regardless of how it was produced.
- `crates/transfer/src/delta_pipeline.rs:42`
  `DEFAULT_PARALLEL_THRESHOLD = 64` is the existing inter-file
  threshold (file count). It does not apply intra-file.
- `crates/transfer/src/parallel_io.rs:16`
  `DEFAULT_STAT_THRESHOLD = 64` and `:22`
  `DEFAULT_SIGNATURE_THRESHOLD = 32` are sibling thresholds for stat
  batching and signature build, also not applicable here.

**Upstream reference** (`docs/design/upstream-match-token-reference.md`,
the document that PR #4202 captures):

- Upstream `match.c:140-345 hash_search` is the line-for-line model for
  the loop at `crates/matching/src/generator.rs:196-263`. Upstream is
  also single-threaded per file; we inherit the bottleneck.
- `match.c:155 want_i = 0` is the hint cursor that gives a 1-byte cost
  on contiguous matches. Our `want_i` at
  `crates/matching/src/generator.rs:163` is the same hint.
- `match.c:174 end = len + 1 - last_block_len` is the tail-flush
  trigger that any spatial-split scheme must reproduce inside the
  last worker.

## 2. Window-split scheme

The single tight loop in `generator.rs:196-263` consumes one input byte
per iteration in the steady state, with a serial dependency through
`RollingChecksum::roll` (each output sum feeds the next input byte). To
break that dependency we split the input stream into `N` contiguous
windows by byte offset:

```
input stream:  [-------------- L bytes --------------]
                ^win 0  ^win 1  ^win 2  ...  ^win N-1

stripe S = ceil(L / N)
block B = index.block_length()

worker k input range:
  [max(0, k*S - B), min(L, (k+1)*S + B))
```

Each worker owns its own `RingBuffer` and `RollingChecksum`. Each worker
shares the same `Arc<DeltaSignatureIndex>` (read-only inside `generate`).

### 2.1 Boundary overlap

The serial loop emits a COPY when `input[j .. j+B]` equals a basis
block, for `j` walking from 0 to `L - B`. A window-split scheme misses
matches at the boundary if worker `k+1` starts its rolling state at
`(k+1) * S` with an empty ring buffer. It would not have the bytes at
`(k+1)*S - 1 .. (k+1)*S - B + 1` in its window and so would not detect a
match whose `j` falls just before the boundary.

The fix is the `B`-byte overlap shown above. Worker `k+1` reads from
`(k+1)*S - B` onwards. By the time its rolling checksum reaches the
first emit position (`(k+1)*S`), the ring buffer holds the same `B`
bytes the serial loop would have, and the rolling sum equals what the
serial loop would have at the same offset. Every match `j` in
`[(k+1)*S - B, (k+1)*S]` is detected by both worker `k` (which reads up
to `(k+1)*S + B`) and worker `k+1` (which starts at `(k+1)*S - B`).

The duplication at boundaries is bounded: each boundary produces at
most `B` candidate COPYs duplicated across two workers, and there are
`N - 1` boundaries. For `B = 1 MB` (typical for files >= 16 GB; see
`crates/signature/src/block_size.rs:129 calculate_block_length`) and
`N = 16` the overlap zone is `16 MB` of redundant scanning - negligible
against the `L >= 100 MB` file size that activates this path.

### 2.2 The `want_i` hint at boundaries

The serial loop carries `want_i` across the entire input (see
`crates/matching/src/generator.rs:163`). A parallel worker starts with
`want_i = None` because it has no preceding match context. This is
strictly safe: an unset `want_i` just degenerates to a full hash-table
probe on the first window. After the first match inside the worker's
stripe, `want_i` is re-armed locally.

The consequence is one missed hint per worker per stripe (vs zero in
the serial case). On real workloads the cost is one extra
`find_match_slices_filtered` probe per `N`-stripe pair, which the tag
table at `crates/matching/src/index/mod.rs:147-149` resolves in ~3
instructions on a miss. Negligible.

## 3. Result ordering

Two viable schemes; both produce a token stream byte-identical to the
serial output.

### 3.1 Per-window buffer plus stable merge sort

Each worker emits a `Vec<TaggedToken>` where `TaggedToken` carries the
input-stream offset of the token's first byte. After all workers
complete, concatenate the per-worker vectors and stable-sort by
`(input_offset, kind_priority, basis_index)`, then drop any token whose
input-byte range is fully covered by an earlier token. The dedup
guarantee is straightforward: every input byte is covered by at most
one COPY in the serial output, so any duplicate COPY produced by two
workers across a boundary is exactly the same `(input_offset, length,
basis_index)` triple and the stable-sort leaves only the first.

Cost: `O(T log T)` for `T` tokens, where `T = L / B` in the high-match
limit and `T = L / CHUNK_SIZE` in the all-literal limit (see
`crates/matching/src/generator.rs:219 pending_literals.len() >=
block_len + CHUNK_SIZE`).

### 3.2 Sequence-indexed `ReorderBuffer` (#1885)

`crates/engine/src/concurrent_delta/reorder/mod.rs:89 ReorderBuffer<T>`
already implements ordered drain over arbitrary completion order with
a `next_expected` cursor (`reorder/mod.rs:432`) and `drain_ready`
(`reorder/mod.rs:426`) interface. Per-window tokens carry a sequence
key derived from `(worker_id, intra_worker_seq)`; the merge layer
flushes them in `(window_offset, intra_worker_seq)` order.

This reuses an existing, well-tested data structure and avoids the
sort step. It also composes with the parallel inter-file delta
dispatcher
(`crates/transfer/src/delta_pipeline.rs:181 ParallelDeltaPipeline`)
because the `ReorderBuffer` already serves that dispatcher. Issue
#1885 tracks the generalisation needed to key on intra-file sequence
rather than the current per-file sequence.

**Preferred scheme**: 3.2. The sort-based scheme (3.1) is simpler to
prototype but adds an `O(T log T)` step that the `ReorderBuffer`
scheme avoids. Reusing the existing buffer also keeps the surface area
of the change small: one new keying convention, no new ordered-merge
infrastructure.

## 4. Threshold

A new constant gates intra-file parallelism. Proposed name and
placement (next to existing thresholds in
`crates/transfer/src/parallel_io.rs`):

```rust
/// Minimum file size for intra-file parallel rolling-hash matching.
///
/// Below this size, the spawn cost of N window workers plus the
/// ordered-merge pass exceeds the gain from parallel rolling-hash
/// advance. The matcher uses the serial loop in
/// `crates/matching/src/generator.rs:141 DeltaGenerator::generate`.
/// At or above this size, the matcher fans out across worker windows
/// and reassembles tokens in input-offset order.
pub const MIN_PARALLEL_FILE_SIZE_BYTES: u64 = 128 * 1024 * 1024;
```

Rationale for 128 MB (not 64 MB, the value
`docs/design/intra-file-parallelism.md` proposes for the broader
spatial-split feature):

- `crates/signature/src/block_size.rs:129 calculate_block_length`
  derives `block_length = floor(sqrt(file_size))` rounded down to a
  power-of-2-aligned width. At 64 MB file size that yields
  `block_length ~= 8 KB`. Each worker's overlap zone is 8 KB and
  per-worker fixed overhead (`RingBuffer + RollingChecksum +
  pending_literals` capacity) is on the order of 24 KB. With 16
  workers the fixed overhead is ~400 KB, still small vs 64 MB input,
  but the rolling-hash advance throughput at 8 KB blocks
  (~1.5 M advances/s/core) means a single core finishes a 64 MB file
  in ~50 ms - below the rayon dispatch + Arc clone + worker spawn
  break-even point measured on the existing
  `crates/engine/benches/parallel_dispatch_overhead.rs` harness.
- At 128 MB and above, single-core time is ~100 ms and parallel speedup
  is measurable on 4-core and 16-core hosts. The threshold is
  intentionally conservative: a parallel path that loses to the serial
  path for any tested input violates the bench gate in section 5.

The constant must be a private `pub const` in `parallel_io.rs`, not a
`clap` flag. There is no user-facing knob; the matcher picks its path
from `file_entry.length()` at the call site in
`crates/transfer/src/generator/delta.rs:171`.

## 5. Bench evidence needed

The headline question - "does intra-file parallel rolling-hash beat
serial on real-world large files?" - is unanswered until benchmarks
land. Three adjacent benchmark efforts already cover overlapping
ground and must be inspected before adding a fourth.

**Existing or in-flight benches that touch this region:**

- `crates/checksums/benches/md4_multibuffer_benchmark.rs` (#4189
  MD4 multibuf). Measures 4-way and 8-way SIMD batch MD4 against
  serial MD4. If multibuf MD4 lifts single-thread throughput by 3-4x,
  the strong-checksum verify stops being the bottleneck for
  duplicate-heavy basis files and approach C in
  `docs/design/intra-file-parallelism.md` absorbs most of the gain
  intra-file parallelism would have captured. Inspect this bench
  first.
- `crates/checksums/benches/md5_multibuffer_benchmark.rs` (#4191
  MD5 multibuf). Same logic for MD5-strong checksums. Same gating
  question: if the single-thread verify is no longer the hot stage,
  intra-file parallelism's payoff shrinks.
- `crates/matching/benches/seq_match_redundant.rs` (#2067 zsync
  seq-match bench). Measures the redundant-probe cost on duplicate-
  heavy basis layouts. Tangential to intra-file parallelism but
  shares the input-fixture generator: any new intra-file bench
  should reuse the synthetic-fixture helpers in this file.
- `crates/matching/benches/prune_duplicate_heavy.rs` (#2071 zsync
  prune bench). Measures the matched-block pruning gain on
  duplicate-heavy inputs. Same fixture-reuse note.
- `crates/matching/benches/bithash_rejection.rs` (#2063 zsync bithash
  bench). Measures the bithash post-tag rejection rate. Tangential
  to intra-file parallelism but again shares helpers.

**Recommended new bench, if and only if MD4/MD5 multibuf benches
prove single-thread headroom is exhausted:**

- `crates/matching/benches/intra_file_parallel.rs` (criterion).
  Three axes:
  1. File size: {16 MB, 64 MB, 128 MB, 256 MB, 1 GB}.
  2. Worker count: {1, 2, 4, 8, 16, 32}.
  3. Similarity: {0% match (all-literal), 50% match, 100% match}.
  The serial path runs at worker count 1; the parallel path runs at
  worker count >= 2. The bench fails to land if any row of the
  resulting table shows parallel slower than serial; the
  `MIN_PARALLEL_FILE_SIZE_BYTES` constant is set to the smallest
  file size that keeps parallel within 5% of serial across all
  worker-count rows.

**Bench prerequisites that must land first:**

1. #4189 MD4 multibuf bench results.
2. #4191 MD5 multibuf bench results.
3. A decision in the per-PR descriptions for those two that
   single-thread strong-checksum verify is no longer the hot stage.

Without those three datapoints, an intra-file parallel matcher could
land and immediately be made redundant by a strong-checksum SIMD
upgrade landing in the same release.

## 6. Recommendation

**Defer.** Do not implement until #4189 and #4191 (MD4/MD5 multibuf
SIMD benches) show single-thread strong-checksum verify is no longer
the hot stage on representative workloads.

Reasoning:

- The serial loop in `crates/matching/src/generator.rs:196-263` is
  bottlenecked on two things: rolling-hash advance and
  strong-checksum verify on rsum hits. On low-similarity inputs
  rolling-hash advance dominates and intra-file parallelism is the
  right answer. On high-similarity inputs strong-checksum verify
  dominates and SIMD multibuf is the right answer. Without #4189 and
  #4191 numbers we cannot tell which workload mix is hot on real
  users.
- Intra-file parallelism is the largest of the three approaches
  catalogued in `docs/design/intra-file-parallelism.md` (spatial
  split + boundary merge + threshold tuning + golden-byte parity
  tests). It is also the one most likely to introduce a subtle
  byte-divergence regression at the boundary. Approach C (SIMD batch
  verify) and approach B (pipeline staging) are smaller, more
  composable, and lower-risk landings.
- Issue #2206 (this proposal) is the right place to **record** the
  design but not the right place to land the implementation. The
  prerequisite benches must come first; the implementation should
  follow the threshold-tuned numbers, not precede them.
- If #4189 / #4191 SIMD multibuf benches show single-thread
  throughput has headroom of 1.5x or more (i.e. parallel matching
  would lift it further), revisit this design as an `xtask` bench
  scaffolding PR, then a feature-flagged implementation PR (gated
  behind `MIN_PARALLEL_FILE_SIZE_BYTES`), then a per-worker NUMA
  affinity follow-up.
- "Defer until SIMD multibuf benches show single-thread headroom is
  exhausted" is fully consistent with the wire-compat invariants:
  the token stream is unchanged whether the matcher runs serial or
  parallel, so deferring costs nothing in compatibility terms.

A prototype-behind-feature path is **not** recommended for this work.
Behavioural divergence in matched tokens between a feature-gated and
default build is the exact failure mode the golden-byte tests are
designed to catch, and a prototype that ships without those tests
would re-litigate the wire-compat contract on every PR touching
`generator.rs`. Either land the full path (with parity tests, bench
table, and threshold) or do not land it at all.

## 7. Cross-references

Upstream and adjacent design work:

- **#4202** - upstream `match.c` / `token.c` reference walk-through
  (`docs/design/upstream-match-token-reference.md`). The
  single-threaded matcher we mirror.
- **#4189** - MD4 multibuf SIMD benchmark
  (`crates/checksums/benches/md4_multibuffer_benchmark.rs`). Must
  land first; gates this proposal.
- **#4191** - MD5 multibuf SIMD benchmark
  (`crates/checksums/benches/md5_multibuffer_benchmark.rs`). Must
  land first; gates this proposal.
- **#1885** - sequence-indexed `ReorderBuffer` keying. Implementation
  prerequisite for the preferred result-ordering scheme in section
  3.2.

zsync-inspired adjacent matching work (all wire-compatible
optimisations on the same serial loop; composable with intra-file
parallelism but not blocking):

- **#2079** - shifted-insertion adversarial fixture
  (`docs/design/zsync-shifted-insertion-test.md`). Boundary-correctness
  cousin: similar concern about offsets crossing structural
  boundaries.
- **#2080** - sparse-match adversarial fixture
  (`docs/design/zsync-sparse-match-test.md`). Same family.
- **#2063** - bithash rejection bench
  (`crates/matching/benches/bithash_rejection.rs`). Shares the
  synthetic-input fixture helpers any intra-file bench should reuse.
- **#2067** - seq-match redundant bench
  (`crates/matching/benches/seq_match_redundant.rs`). Same.
- **#2071** - prune duplicate-heavy bench
  (`crates/matching/benches/prune_duplicate_heavy.rs`). Same.

Self:

- **#2206** - this design note. Implementation, if approved,
  branches into separate per-piece PRs (bench scaffolding, then
  spatial-split matcher, then threshold tuning, then NUMA affinity).

Companion notes already in `docs/design/`:

- `docs/design/intra-file-parallelism.md` - broader survey of
  spatial-split, pipeline-staging, and SIMD-batch-verify approaches.
  This note narrows to the spatial-split (window) path applied to
  rolling-hash search and proposes the `MIN_PARALLEL_FILE_SIZE_BYTES`
  threshold name.
- `docs/design/zsync-inspired-matching.md` - wire-compat invariants
  shared across the matching family. Section "Wire-compat
  invariants" applies verbatim.

## Wire-format guarantee

Mandatory for any future PR implementing this design:

1. The COPY/LITERAL token stream MUST be byte-identical to today's
   serial output for every input. A property test running both paths
   against random inputs and comparing token streams is non-optional.
2. `crates/protocol/tests/golden/` byte-comparison tests pass
   unchanged.
3. `tools/ci/run_interop.sh` against upstream 3.0.9 / 3.1.3 / 3.4.1
   produces zero new entries in `tools/ci/known_failures.conf`.
4. `tcpdump`-captured application-layer payloads for an oc-rsync push
   to an upstream daemon are byte-identical with the matcher in
   parallel mode vs serial mode.
5. No new CLI flag in `crates/cli/`. Internal toggles, if any, are
   cfg-gated benchmark scaffolding only.

These mirror the invariants in `docs/design/intra-file-parallelism.md`
and `docs/design/zsync-inspired-matching.md`; restating them here so
this note is self-contained for #2206 reviewers.
