# Intra-file parallel rolling-hash for large basis files

Addendum to `docs/design/intra-file-parallelism.md` (#2206). That note frames
intra-file parallelism for the **receiver-side delta matcher** at
`crates/matching/src/generator.rs::DeltaGenerator::generate`. This addendum
narrows the focus to one piece of that pipeline: the **rolling-hash computation
itself**, i.e. the sliding `RollingChecksum` advance at
`crates/checksums/src/rolling/checksum/mod.rs::RollingChecksum::roll` and its
SIMD batch variants in `x86.rs` (AVX2 / SSE2) and `neon.rs`. It also covers the
sender-side basis-file rolling that produces block signatures via
`crates/signature/src/parallel.rs::generate_file_signature_parallel`.

Read `intra-file-parallelism.md` first for:

- Wire-compat invariants (token-stream byte identity, golden-byte test stability).
- Receiver-side matcher decomposition (approaches A / B / C).
- Stripe-boundary merge dedup proof for the spatial split.
- Composition rule against inter-file parallel pipelines.

This addendum does NOT repeat that material. It addresses the question: "can we
parallelise the rolling-hash advance itself, and if so, how?"

## The serial dependency

The rolling-hash advance at offset `i` reads state `(s1_{i-1}, s2_{i-1})` and
produces `(s1_i, s2_i)`:

```
s1_i = (s1_{i-1} - schar(in[i-B]) + schar(in[i])) & 0xffff
s2_i = (s2_{i-1} - B * schar(in[i-B]) + s1_i)    & 0xffff
```

(`B` = `block_length` from `crates/signature/src/block_size.rs`,
`schar` = sign-extended byte as in `checksum.c:get_checksum1()`.)

The dependency `s_i -> s_{i+1}` is the textbook serial reduction. A naive split
of the input across N threads at offset `k * (file_size / N)` is **incorrect**
because thread `k` starts with `s1 = s2 = 0` and has no way to reconstruct the
state that thread `0` would have computed at offset `k * (file_size / N) - 1`.

This is the rolling-hash analogue of the streaming-hash split problem. Unlike
a cumulative-sum scan it cannot be decomposed by associative reduction: the
rolling state at offset `i` depends on bytes `in[i-B .. i]` rather than on
`in[0 .. i]`, so prefix-sum / scan-based parallelism does not apply.

## Two valid strategies

### Strategy (a): independent block-aligned segments

Each thread starts at a **block boundary** (`offset = k * B`) with a clean
state (`s1 = s2 = 0`), reads exactly `B` bytes, and computes the block's
rolling digest from scratch. This is what
`generate_file_signature_parallel` already does today
(`crates/signature/src/parallel.rs:138-158`):

```text
sender-side basis signature:

  block 0   block 1   block 2   block 3   ...   block N-1
[ B bytes ][ B bytes ][ B bytes ][ B bytes ] ... [ B bytes ]
   |          |          |          |               |
   v          v          v          v               v
 worker 0  worker 1  worker 2  worker 3  ...    worker N-1
   each computes RollingDigest::from_bytes(block)
```

Pros: no inter-thread state, no overlap reads, identical to the serial
output by construction. Maps directly onto rayon `par_chunks(BATCH_SIZE)`.

Cons: only valid when the rolling window aligns to block boundaries.

Strategy (a) is correct for **basis-side signature generation** because each
basis block has independent rolling state in the wire signature: every block
contributes one `(rolling, strong)` pair and the rolling values do NOT chain
across blocks. The wire format treats block digests as a set, not a stream.

### Strategy (b): overlap windows with seeding

Each thread covers an offset range `[k*S, (k+1)*S)`. To compute the rolling
state at its first emit position `k*S`, thread `k` reads `B-1` extra bytes
of overlap `[k*S - (B-1), k*S)` and seeds its `RollingChecksum` over that
prefix via `RollingChecksum::update(...)` before entering the steady-state
roll loop:

```text
input-stream basis rolling (matcher hot path):

   [stripe 0]      [stripe 1]      [stripe 2]      ...
   [---- S ----]   [---- S ----]   [---- S ----]
                ^^^^             ^^^^
                seed B-1 bytes   seed B-1 bytes
                from end of      from end of
                previous stripe  previous stripe

  worker 0: roll over input[0 .. S]
  worker 1: seed = update(input[S-B+1 .. S]); then roll input[S .. 2S]
  worker 2: seed = update(input[2S-B+1 .. 2S]); then roll input[2S .. 3S]
```

Pros: works for the **streaming matcher** where the rolling state must be
continuous across stripe boundaries because the matcher probes at every byte
offset, not just on block boundaries.

Cons: each worker pays a `B-1` byte seed cost (negligible: ~64 KB per worker
at `block_length = 64 KB`); requires the input to be seekable or fully
buffered in memory (mmap is the natural fit).

Strategy (b) is the only correct option for the **receiver-side matcher**
described in `intra-file-parallelism.md` approach A, because the matcher emits
COPY/LITERAL tokens at byte offsets between block boundaries, and each
emit-eligible offset needs the same rolling state the serial loop would have
at that offset.

### Tradeoff for sender-side basis rolling

The sender-side hot path for very large basis files has two phases:

1. **Signature generation** (`crates/signature/src/parallel.rs:84`) - happens
   once per file before any matching. Strategy (a) applies: block-aligned,
   already parallel via rayon, no rolling-state chain across blocks.
2. **Delta matcher** (`crates/matching/src/generator.rs:141`) - happens once
   per file at receive time. The rolling state advances byte by byte; only
   strategy (b) works without breaking byte-stream semantics.

The split is decisive. Phase 1 has nothing left to optimise from a
state-dependency angle. Phase 2 is the only place where rolling-hash-specific
parallelism work has any leverage, and there it composes directly with
approach A from the companion note.

## Receiver-side strong-checksum verification

Once the rolling-hash advance flags a candidate match (the rsum tag hits in
the index lookup at `crates/matching/src/index/mod.rs::find_match_bytes_filtered`),
the strong-checksum verify on the candidate window is **embarrassingly
parallel per candidate** and **independent across windows**:

- Computing MD4 / MD5 / XXH3 over a `B`-byte window has zero dependency on
  any other window's hash.
- The existing `find_match_slices_parallel` at
  `crates/matching/src/index/mod.rs:199` already parallelises within a single
  rsum hash chain (threshold 4 candidates).
- Cross-window parallelism is the territory of approach C in the companion
  note (SIMD batched verify) and approach A (stripe workers each running
  their own verifies).

This addendum does not add anything new for verify; it merely records that
strong-checksum verify has no rolling-hash-style state-dependency problem.
The only contention point is access to the shared `DeltaSignatureIndex`,
which is `Sync` for read access.

## Cache-line considerations

Rolling-hash state is 16 bytes (`s1: u32`, `s2: u32`, `len: usize`). With 64
workers each holding their own `RollingChecksum` on the stack or in a per-thread
arena, false sharing is a non-issue **as long as the rolling state is not
packed into a shared `Vec<RollingChecksum>` indexed by worker ID**. The safe
pattern is one rolling state per rayon task stack frame; rayon scheduling
guarantees stack isolation.

For strategy (b) the **input chunk** matters more than the rolling state.
Each worker linearly streams its stripe. To avoid cache-line ping-pong on the
input:

- Chunk size should be `>= 64 KB` (16 cache lines on x86-64 / aarch64, 1024
  on POWER), matching the `READ_BUFFER` size used in
  `crates/matching/src/generator.rs:176` (`vec![0u8; self.buffer_len.max(block_len)]`).
- Rayon `par_chunks(64 * 1024)` is the minimum safe chunk size; smaller
  chunks cross cache-line boundaries on the input read and amplify false
  sharing on the source mmap region.
- The cache-line concern is independent of rolling-state placement; it is
  driven by the rolling-hash's byte-stream access pattern over the input.

For strategy (a), the same constraint applies to `par_chunks(BATCH_SIZE)` in
`crates/signature/src/parallel.rs:139`: at `BATCH_SIZE = 16` and
`block_length = 64 KB`, each batch covers 1 MB which is well above the
false-sharing threshold.

## SIMD interaction

Rolling-hash SIMD operations (AVX2 32-byte loop in
`crates/checksums/src/rolling/checksum/x86.rs:113-130`, SSE2 16-byte loop at
`:158-176`, NEON 16-byte loop in `neon.rs`) accelerate the **intra-thread**
update of a single `RollingChecksum`. They reduce per-byte cycle cost of the
sliding-window advance without changing its semantics or output.

Parallel splits (a) and (b) operate at the **inter-thread** level. They
distribute disjoint byte ranges to disjoint `RollingChecksum` instances, each
of which uses its host CPU's best available SIMD lane width.

Composition is multiplicative, not additive:

- AVX2 alone: ~4-8x scalar throughput on the rolling-hash advance.
- 16-way parallel alone: ~16x throughput ceiling (limited by memory
  bandwidth on large files).
- AVX2 + 16-way parallel: ~64-128x scalar throughput ceiling, bounded by
  L3 / DRAM bandwidth long before reaching the arithmetic ceiling.

No SIMD code in `rolling/checksum/x86.rs` or `neon.rs` needs to change for
either parallel strategy. The dispatch ladder
(`accumulate_chunk_dispatch` -> AVX2 -> SSE2 -> scalar; or
`accumulate_chunk_dispatch` -> NEON -> scalar) is per-call and per-thread,
already protected by `OnceLock`-cached feature detection.

## Threshold for triggering parallelism

The existing thresholds give the right shape:

- `crates/signature/src/parallel.rs:172` -
  `PARALLEL_THRESHOLD_BYTES = 256 * 1024` (256 KB) for signature generation
  via strategy (a).
- `crates/signature/src/block_size.rs:45` - `DEFAULT_BLOCK_SIZE = 700`
  bytes for small files; `MAX_BLOCK_SIZE_V30 = 131_072` (128 KB) for
  protocol >= 30. The square-root sizing means a 100 GB file lands at
  `block_length` ~1 MB.
- `intra-file-parallelism.md::INTRA_FILE_PARALLEL_THRESHOLD_BYTES =
  64 * 1024 * 1024` (64 MB) for the matcher-side spatial split.

For the rolling-hash-specific path:

```rust
/// Minimum input size for strategy (b) parallel rolling-hash advance.
///
/// Below this, the cost of seeding each worker over `B-1` bytes plus
/// rayon dispatch exceeds the gain from parallel rolling-hash advance.
/// At 16 * block_length the per-worker stripe is large enough that
/// the seed phase is < 6% of total work.
pub const INTRA_FILE_ROLLING_PARALLEL_THRESHOLD: u64 = 16 * MAX_BLOCK_SIZE_V30 as u64;
//                                                  = 16 * 131_072
//                                                  = 2_097_152  (2 MB)
```

Rationale: at 2 MB and `block_length = 64 KB`, 4 workers see 512 KB per
stripe and the per-worker seed cost (`B-1 = 64 KB - 1`) is ~12.5% of stripe
work. At 16 MB the same arithmetic gives 4% overhead, well below noise. The
constant is a tuning input to the microbench plan below, not a hard
guarantee.

The threshold should live next to `INTRA_FILE_PARALLEL_THRESHOLD_BYTES` in
`crates/transfer/src/delta_pipeline.rs` (no new public flag, no `clap`
argument).

## Verification

Two parity invariants must hold:

1. **SIMD vs scalar parity** (existing). The parity proptest at
   `crates/checksums/tests/rolling_simd_parity.rs` already covers this for
   the rolling-hash advance. No change.
2. **Parallel vs serial parity** (new). For strategy (b) over a randomly
   chosen input and a randomly chosen stripe count `N`, the rolling digest
   computed at every offset `i` (for `i` in a sampled set, not every byte
   for cost) must equal the digest the serial loop would have produced. The
   acceptance criterion is byte-exact equality of the digest, not just of
   the eventual COPY/LITERAL output (which is already covered by the
   matcher-level parity test described in the companion note).

New test target:

```text
crates/checksums/tests/rolling_parallel_parity.rs
  - proptest input: random bytes, length 64 KB .. 16 MB
  - proptest stripe count: 1, 2, 4, 8, 16, 32, 64
  - for each (input, N): run strategy (b) over N stripes, sample 256
    random offsets, compare RollingChecksum.digest() at each offset
    against the serial loop's digest at the same offset
  - fail on any mismatch
```

Strategy (a) parity is already covered by
`crates/signature/src/parallel.rs::tests::parallel_matches_sequential_*`
(lines 250-306) and needs no new test.

## Recommendation: **defer until benchmark proves win**

The case for landing strategy (b) intra-file rolling-hash parallelism rests
on two assumptions:

1. The rolling-hash advance is the dominant cost in the receiver-side delta
   matcher (assumed in `intra-file-parallelism.md`, not yet measured on a
   100 GB synthetic).
2. The 64 KB / 1 MB block sizes typical for 100 GB+ files leave enough
   stripe size per worker to amortise the seed cost.

Both deserve direct measurement before code lands. The existing approach C
(SIMD batched verify) and approach B (pipeline staging) from the companion
note cover the easier wins and have lower implementation risk.

The recommendation is: **do not implement strategy (b) until a microbench
on a 1 GB synthetic with random-similar input proves that the rolling-hash
advance accounts for >= 40% of `DeltaGenerator::generate` wall time on a
modern 16-core machine.** Approach A in the companion note subsumes
strategy (b) at a higher level (the stripe-worker matcher each runs its own
serial rolling-hash, which is identical to strategy (b) embedded inside the
matcher). A standalone "parallel `RollingChecksum::roll_many`" API is
unnecessary; the work belongs at the matcher layer.

### Bench plan (if measurement proves the case)

`crates/checksums/benches/rolling_parallel.rs`:

- Input sizes: 1 MB, 16 MB, 256 MB, 1 GB synthetic with
  `(idx % 251) as u8` byte pattern (same as
  `crates/signature/src/parallel.rs::tests`).
- Worker counts: 1 (serial baseline), 2, 4, 8, 16, 32, 64.
- Block sizes: 4 KB, 64 KB, 1 MB.
- Measure: rolling-hash advance throughput in GB/s, with and without
  SIMD (gate the SIMD path off via the existing feature-flag mechanism).
- Pass criterion: parallel path is no slower than serial at every measured
  point, and >= 1.5x serial at >= 4 workers on inputs >= 16 MB.

`tools/ci/run_interop.sh` and `crates/protocol/tests/golden/` must remain
green throughout - this is a per-thread acceleration, not a wire change.

## Five-step implementation sequencing (if recommended)

These five steps land in order. Each step is independently revertible.

1. **Land matcher-level approach C first.** SIMD batched strong-checksum
   verify (`crates/matching/src/index/mod.rs:178::find_match_slices_sequential`).
   No rolling-hash change; tracked under #1763. Lowest risk, validates the
   benchmark harness, frees CPU budget that today is spent on serial
   strong-checksum verify.
2. **Microbench the rolling-hash advance share** of
   `DeltaGenerator::generate` on a 1 GB synthetic. If the rolling-hash
   advance is < 40% of wall time, stop here; the rolling-hash-specific
   parallel path will not move the needle.
3. **Wire approach A (spatial split)** per the companion note. Each stripe
   worker runs its own serial `RollingChecksum::roll` over a `block_length`-
   overlapped range. This **is** strategy (b) embedded inside the matcher;
   no separate `RollingChecksum` parallel API needed.
4. **Add the parity test** (`crates/checksums/tests/rolling_parallel_parity.rs`)
   as described above. Gate the matcher path on the test passing in CI.
5. **Tune `INTRA_FILE_ROLLING_PARALLEL_THRESHOLD`** on 4-core, 16-core, and
   64-core hardware. Pick the smallest threshold that keeps the parallel
   path no slower than serial at every measured point.

## References

- `docs/design/intra-file-parallelism.md` - companion note, framing
- `docs/design/sve-rolling-checksum-aarch64.md` - SVE acceleration plan
- `crates/checksums/src/rolling/checksum/mod.rs::RollingChecksum::roll` -
  serial advance
- `crates/checksums/src/rolling/checksum/mod.rs:332` - `roll(outgoing, incoming)`
- `crates/checksums/src/rolling/checksum/x86.rs:113` - AVX2 accumulation
- `crates/checksums/src/rolling/checksum/x86.rs:137` - SSE2 accumulation
- `crates/checksums/src/rolling/checksum/neon.rs` - NEON accumulation
- `crates/checksums/tests/rolling_simd_parity.rs` - SIMD vs scalar proptest
- `crates/signature/src/parallel.rs:84` -
  `generate_file_signature_parallel` (strategy (a) for basis signature)
- `crates/signature/src/parallel.rs:172` - `PARALLEL_THRESHOLD_BYTES = 256 KB`
- `crates/signature/src/block_size.rs:45` - `DEFAULT_BLOCK_SIZE = 700`
- `crates/signature/src/block_size.rs:50` - `MAX_BLOCK_SIZE_V30 = 128 KB`
- `crates/matching/src/generator.rs:141` -
  `DeltaGenerator::generate` (receiver-side matcher hot path)
- `crates/matching/src/generator.rs:176` - 64 KB read buffer
- `crates/matching/src/index/mod.rs:178` - `find_match_slices_sequential`
- `crates/matching/src/index/mod.rs:199` - `find_match_slices_parallel`
  (4-candidate threshold)
- `crates/engine/src/local_copy/context_impl/delta_transfer.rs:48` -
  sender-side rolling advance on the basis stream
- Upstream rsync 3.4.1 `checksum.c:get_checksum1()` - reference scalar impl
- Upstream rsync 3.4.1 `match.c:hash_search()` - reference matcher loop
