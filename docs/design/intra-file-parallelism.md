# Intra-file parallelism for single large file transfers

Design note for accelerating delta computation on a single large basis file by
distributing rolling-checksum search and strong-checksum verify across multiple
CPU cores. All approaches in this note are **purely in-memory** - they MUST NOT
change anything serialized on the wire, the protocol-32 negotiation, the
signature payload format, or the COPY/LITERAL token stream byte sequence.

## Problem statement

A 100 GB single-file transfer pegs one core on the receiver doing rolling-hash
advance plus strong-checksum verify, while N-1 cores idle. The receiver hot
path is sequential per file:

```
RECEIVER DATA ARRIVES
     |
     v
crates/transfer/src/receiver/transfer/pipeline.rs:38
   ReceiverContext::run_pipeline_loop_decoupled()
     |
     v  (one file at a time, one thread)
crates/transfer/src/receiver/transfer/pipeline.rs:299
   process_file_response_streaming()
     |
     v
crates/transfer/src/transfer_ops/streaming.rs:74
   process_file_response_streaming<R: Read>()
     |
     v
crates/transfer/src/delta_apply/applicator.rs:436
   apply_delta_stream()  (token-by-token apply)
     |
     v
crates/match/src/generator.rs:81
   DeltaGenerator::generate()  (single-threaded match loop)
     |
     +- crates/match/src/ring_buffer.rs - RingBuffer::push_back
     |
     +- crates/checksums/src/rolling/checksum/mod.rs:333
     |    RollingChecksum::roll(outgoing, incoming)
     |
     +- crates/match/src/index/mod.rs:151
          DeltaSignatureIndex::find_match_slices()
```

The single match loop in `crates/match/src/generator.rs:125-285` consumes one
input byte per iteration in the steady state and dispatches a strong-checksum
verify on every rsum hit. For a 100 GB file at typical block sizes
(`SignatureBlock` length ~64 KB, see `signature/src/block_size.rs::calculate_block_length`),
the receiver does on the order of 1.5 M rolling-hash advances per second on
one core - leaving every other core idle.

Existing parallelism on adjacent paths does NOT help this case:

- **Inter-file dispatch.** `crates/transfer/src/delta_pipeline.rs:42` declares
  `DEFAULT_PARALLEL_THRESHOLD = 64`, the file-count cutoff that switches the
  receiver from `SequentialDeltaPipeline` to `ParallelDeltaPipeline`. Single
  large files hit this code path with batch size 1.
- **Parallel signature generation.** `crates/signature/src/parallel.rs:84`
  parallelises rolling+strong checksums across blocks of the basis file
  (`generate_file_signature_parallel`). It runs once per file at signature
  build time; it does not accelerate the per-file delta search.
- **Parallel candidate verify within a hash chain.**
  `crates/match/src/index/mod.rs:199 find_match_slices_parallel` only fans out
  when one rsum chain has more than `PARALLEL_THRESHOLD = 4` candidates
  (line 107). For typical files most chains are length 1-2 and this path is
  cold.
- **Per-block parallel stat / metadata.** `crates/transfer/src/parallel_io.rs:16`
  defines `DEFAULT_STAT_THRESHOLD = 64`, which is filesystem metadata, not
  delta computation.

The hot data structures inside `DeltaGenerator::generate` are small (`RollingChecksum`
~16 B, `RingBuffer` ~40 B + `block_length` bytes). The bottleneck is purely
serial dependency on the rolling checksum: each output byte feeds the next
roll. We need a way to break that dependency for very large files without
breaking wire compatibility.

## Wire-compat invariants

The following must hold for every PR landing one of these techniques. Mirrors
the invariants in `docs/design/zsync-inspired-matching.md`.

1. The COPY/LITERAL token stream MUST be byte-identical to today's output.
   Multi-thread reordering of internal work is permitted; the **emitted token
   sequence in basis-offset order** must not differ.
2. `crates/protocol/tests/golden/` byte-comparison tests pass unchanged.
3. `tools/ci/run_interop.sh` against upstream 3.0.9 / 3.1.3 / 3.4.1 produces
   zero new entries in `tools/ci/known_failures.conf`.
4. `tcpdump`-captured application-layer payloads for an oc-rsync push to an
   upstream daemon are byte-identical with the optimization on vs off.
5. No new flag in `crates/cli/`. Internal toggles, if any, are cfg-gated
   benchmark scaffolding, never `clap` arguments.

## Three approaches

The design space breaks into three qualitatively distinct strategies. They
are not mutually exclusive: B and C compose with each other and with serial
matching; A excludes B for the same file because it splits the matcher. The
recommended landing order is C, then B, then A, behind benchmark gates.

### A: spatial split

Split the basis file (or, equivalently, the search range over the input
stream) into N contiguous stripes by byte offset. Run N parallel matchers,
each producing COPY/LITERAL tokens for its own stripe. Merge in basis-offset
order at the end.

```
input stream:  [---------- 100 GB ----------]
                     ^stripe 0  ^stripe 1  ^stripe 2  ... ^stripe N-1

worker 0: match input[0          .. stripe_size + block_size]
worker 1: match input[stripe_size - block_size .. 2*stripe_size + block_size]
worker 2: match input[2*stripe_size - block_size .. 3*stripe_size + block_size]
...

merge:    sort token streams by basis-offset key, dedup boundary matches
```

Pros: linear speedup in the limit, scales to file size, attacks the dominant
cost (rolling-hash advance).

Cons: every worker holds its own `RingBuffer` + `RollingChecksum`; N workers
multiply matcher memory by N; correctness across stripe boundaries needs an
overlap zone; merge step is an extra O(tokens) pass.

The signature index (`DeltaSignatureIndex` at
`crates/match/src/index/mod.rs:38`) is read-only inside `generate()` so it
can be shared across workers via `Arc<DeltaSignatureIndex>` with no
synchronisation. Only the per-worker rolling state and ring buffer differ.

### B: block-window pipelining

Keep one matcher, but split its phases across pipeline stages connected by
bounded queues:

```
   [stage 1: read]      [stage 2: roll]     [stage 3: verify]    [stage 4: emit]
    network ingest  --> rolling-hash    -->  strong-checksum  -->  token writer
                        advance              verify on tag hits

   crossbeam_queue::ArrayQueue between stages, one byte chunk per slot.
```

The existing SPSC channel at `crates/transfer/src/pipeline/spsc.rs` already
implements lock-free disconnection over `crossbeam_queue::ArrayQueue` and is
used by the network -> disk pipeline. Extending the same pattern to
intra-match staging is a natural next step.

Pros: byte-equivalent by construction (stages are functionally identical to
the serial loop, just split across threads). Hides latency of the strong-
checksum verify behind rolling-hash advance. No correctness risk at stripe
boundaries because there are no stripes.

Cons: only useful if at least one stage is heavy enough to justify
synchronisation overhead. The strong-checksum verify is the heaviest stage
when the rsum tag bit hits often, so this approach helps high-similarity
inputs more than mostly-literal inputs. Single-pipeline depth is bounded by
2-3 useful stages; speedup ceiling around 2-3x.

This approach is partially in place: see #1734 (decoupled disk commit thread
in `crates/transfer/src/pipeline/receiver.rs`) and #1407 (async signature
pre-computation in `crates/signature/src/async_gen.rs`). Extending pipelining
into `DeltaGenerator::generate` itself is the new work.

### C: SIMD batched verify

When multiple candidate matches fall in a small basis window (the
`lookup.get(&(sum1, sum2))` chain at `crates/match/src/index/mod.rs:166`
returns `>= 2` candidates), verify all candidate strong checksums in a single
SIMD batch instead of a serial loop.

```
   candidates = lookup.get(&(sum1, sum2))?;   // e.g. 4-16 entries
   batch_md5(window, &candidates);            // 4-way or 8-way SIMD compute
   compare_in_parallel(&digests, &candidates.strong);
```

Pros: byte-equivalent by construction (batch verify computes the same hashes
as today). Composes cleanly with serial matching. Targets duplicate-block
files (containers, VM images, ML datasets) where rsum chains are long.
Tracked under #1763 with an AVX-512 path proposed.

Cons: only kicks in on long rsum chains. For typical chains of length 1-2
the SIMD path is no faster than serial verify because of dispatch overhead.
The existing `find_match_slices_parallel` (rayon) at
`crates/match/src/index/mod.rs:199` covers chains of length >= 4; SIMD batch
would target chains of length 2-8 below the rayon threshold.

## Approach summary

| Approach | Speedup ceiling | Wire-compat by construction | Memory cost          | Activation cost |
|----------|------------------|------------------------------|----------------------|------------------|
| A        | up to N cores    | merge in basis order required | N x matcher state   | N threads + Arc   |
| B        | 2-3x             | yes                          | bounded queue depth  | 2-3 stage threads |
| C        | 2-8x on long rsum chains | yes                  | one batch buffer     | SIMD dispatch    |

A is the only approach that breaks the rolling-hash serial dependency. B and
C help around the edges of the same single-thread loop. For a 100 GB file
where most input does NOT match, A is the only path to a multi-core speedup;
for files with high-similarity input where most input does match, C carries
significant weight; for files with mid-similarity input where the strong-
checksum verify is the hot stage, B is competitive.

## Wire-compat invariant: token stream byte-identity

The COPY/LITERAL stream emitted by today's serial `DeltaGenerator::generate`
is fully determined by:

- the input byte sequence,
- the basis signature index (`DeltaSignatureIndex`, immutable for one file),
- the rolling-checksum advance order (left-to-right, one byte at a time),
- the `want_i` adjacent-block hint state at
  `crates/match/src/generator.rs:177` and `:259`.

For B (pipelining) the matcher is unchanged - stages just partition work in
time. The token stream is identical because rolling state, hint state, and
input byte order are preserved.

For C (SIMD batch verify) the matcher is unchanged - only the inner
`find_match_slices` candidate-loop swaps a serial verify for a SIMD verify
that returns the same answer. The token stream is identical.

For A (spatial split) the matcher state is split. Worker k starts with an
empty `RingBuffer` and an empty `RollingChecksum`, and reads input from
`input[k*stripe_size - block_size .. (k+1)*stripe_size + block_size)`. To
preserve the serial token sequence:

1. Each worker emits tokens tagged with the **input-stream offset** of the
   first byte of each token (LITERAL: offset of first literal byte; COPY:
   offset where the matched window starts).
2. The merge step concatenates the tagged token streams and **stable sort**
   by input-stream offset.
3. After sort, **dedup**: drop any COPY whose matched range is fully
   contained inside the matched range of an earlier COPY in basis order, and
   drop any LITERAL byte that is covered by a COPY's matched range.
4. The dedup is well defined because every byte in the input is covered by
   at most one COPY in the serial output, and any LITERAL produced by a
   worker that overlaps a sibling worker's COPY is, by definition, covered.

The resulting token stream is byte-identical to the serial stream because:

- The rolling-checksum search at offset `i` in the input depends only on
  `input[i-block_size .. i]` plus the immutable signature index. The overlap
  zone (`block_size` bytes ahead of each stripe boundary) gives every worker
  the same rolling-checksum state at its first emit position as the serial
  loop would have.
- Within a worker's region the matcher uses the same `want_i` state machine
  as the serial loop, so the choice of "match at i+1 via hint" vs "probe
  hash table" is identical to the serial loop.
- The merge dedup removes only redundant tokens that two workers produced
  for the same byte range; it does not invent or drop genuine matches.

A test plan section below makes this guarantee testable.

## Boundary correctness for spatial split

The spatial split must handle the case where a match block crosses a stripe
boundary. Let:

- `S` = stripe size in bytes.
- `B` = signature block length in bytes (`block_length` in
  `DeltaSignatureIndex` and `RingBuffer`).
- Worker `k` covers stripe `k`, with input range
  `[k*S - B, (k+1)*S + B)` for `k > 0`, and `[0, S + B)` for `k == 0`.

Each worker matches over its own range. A block centred at byte `j` matches
when `input[j .. j+B]` equals some basis block. Boundary cases:

1. `j + B < (k+1) * S`. The match is fully inside stripe `k` and only worker
   `k` produces it.
2. `j + B == (k+1) * S`. The match ends exactly at the boundary. Worker `k`
   produces it; worker `k+1` does not see it because its rolling state
   begins at `(k+1)*S - B` with an empty ring buffer that becomes full only
   at offset `(k+1)*S`. At that point the window is `input[(k+1)*S - B ..
   (k+1)*S]`, which is a different range.
3. `j` straddles the boundary, i.e. `(k+1)*S - B < j < (k+1)*S`. Both
   worker `k` and worker `k+1` see this match because both their ranges
   include `[j, j+B)`. Worker `k` covers up to `(k+1)*S + B`, which is `>=
   j + B`. Worker `k+1` covers from `(k+1)*S - B`, which is `<= j`. Both
   produce a COPY token tagged with input offset `j`.

Case 3 is the duplication case. The merge step de-duplicates by stable sort
on `(input_offset, token_kind, basis_index)` and then a single pass that
drops any token whose input range is fully contained in the previously
emitted token's input range. The cost is O(total tokens), one pass.

Pseudo-code for merge:

```
fn merge_stripe_tokens(streams: Vec<Vec<TaggedToken>>) -> Vec<DeltaToken> {
    // streams[k] is in ascending input_offset order by construction.
    let mut all: Vec<TaggedToken> = streams.into_iter().flatten().collect();
    all.sort_by_key(|t| (t.input_offset, t.kind_priority(), t.basis_index));
    let mut out = Vec::new();
    let mut last_end: u64 = 0;
    for t in all {
        if t.input_offset < last_end {
            // t is fully covered by an earlier emitted token (boundary dup).
            continue;
        }
        last_end = t.input_offset + t.input_len();
        out.push(t.into_delta_token());
    }
    out
}
```

`kind_priority` exists to give COPY priority over LITERAL when both start at
the same input offset, matching the serial loop's behaviour of preferring a
match over a literal flush. `basis_index` breaks ties between two workers
producing identical COPY tokens (same `input_offset`, same length, same
basis block index): stable sort keeps the first one and dedup drops the
duplicate.

The overlap zone is `B` bytes per worker. For 64 workers and `B = 64 KB`
that is 4 MB of redundant matching at boundaries - negligible vs a 100 GB
input.

## Threshold for activation

`DeltaGenerator::generate` should run serially below a file-size threshold
and dispatch parallel above. Existing thresholds in the receiver are
file-count thresholds (`DEFAULT_PARALLEL_THRESHOLD = 64` in
`crates/transfer/src/delta_pipeline.rs:42`, `DEFAULT_STAT_THRESHOLD = 64` in
`crates/transfer/src/parallel_io.rs:16`). For single-file work the relevant
threshold is by **file size**, not file count.

Proposed constant in `crates/transfer/src/delta_pipeline.rs` next to the
existing thresholds:

```rust
/// Minimum single-file size for intra-file parallel delta matching.
///
/// Below this size, the spawn cost of N stripe workers + the merge pass
/// exceeds the gain from parallel rolling-hash advance. At 64 MB the input
/// is large enough that even 4 workers see >= 16 MB each, well above the
/// per-stripe break-even threshold measured by `cargo bench --bench delta`.
pub const INTRA_FILE_PARALLEL_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;
```

Rationale for 64 MB:

- Below ~10 MB the input fits in the page cache; serial matching is
  memory-bound on a single core and parallel work just thrashes the cache.
- At 10-64 MB the rayon dispatch + Arc<DeltaSignatureIndex> + per-worker
  ring buffer setup is on the order of milliseconds; parallel speedup is
  marginal and noisy.
- At 64 MB and above the rolling-hash advance dominates total time and the
  setup cost is amortised. Microbenchmarks under #1763 will tune this
  number.

The threshold is a single constant, not a `clap` flag. The receiver picks
serial vs parallel based on `file_entry.length()` at the call site in
`crates/transfer/src/transfer_ops/streaming.rs::process_file_response_streaming`.

## Memory cost

Each parallel matcher needs:

- One `RingBuffer` of `block_length` bytes.
- One `RollingChecksum` of 16 B (`s1: u32`, `s2: u32`, `len: usize`).
- One `pending_literals` Vec, capacity `block_length`, expected steady-state
  occupancy `< block_length` (see `generator.rs:88`).
- One per-worker `tokens` Vec, expected size ~tokens_per_stripe.

At `block_length = 1 MB` (typical for files >= 16 GB; see
`signature/src/block_size.rs::calculate_block_length`), each worker uses
about `2 * block_length = 2 MB` plus a few KB of fixed state. With 64
workers that is **128 MB per file in flight**.

Trade-off vs serial matcher:

- Serial: one ring buffer + one checksum = 2 MB + 16 B per file.
- 4-way parallel: 8 MB + 64 B per file. Negligible.
- 16-way parallel: 32 MB per file.
- 64-way parallel: 128 MB per file.

For consumer machines (16 GB RAM, 8 cores) 16-way parallel is the practical
ceiling and 32 MB per file is acceptable. For server machines (256 GB RAM,
64 cores) 64-way parallel with 128 MB per file is fine, and the ceiling
shifts to thread contention rather than memory.

The `Arc<DeltaSignatureIndex>` is shared across workers so the largest
data structure (lookup map + tag table + block strong checksums) is
**not** duplicated. That is decisive: a 100 GB basis at `block_length = 1 MB`
has ~100 K `SignatureBlock` entries (~10 MB). Sharing this across 64
workers saves 630 MB.

## Interaction with existing parallelism

Intra-file parallelism is at a different scope from existing parallel paths:

- **`PARALLEL_STAT_THRESHOLD = 64`** (`crates/transfer/src/parallel_io.rs:16`)
  is filesystem metadata stat batching at the receiver generator.
  Independent.
- **`DEFAULT_PARALLEL_THRESHOLD = 64`** (`crates/transfer/src/delta_pipeline.rs:42`)
  dispatches inter-file work to a rayon work queue
  (`engine::concurrent_delta::work_queue`). Each work item is one file,
  and intra-file parallelism would run inside the work item. Composition
  rule: enable EITHER inter-file parallel OR intra-file parallel, not
  both, to bound thread count.
- **Parallel signature generation**
  (`crates/signature/src/parallel.rs:84`, `generate_file_signature_parallel`)
  is a one-shot computation when the basis is first read. Independent.
- **`find_match_slices_parallel`**
  (`crates/match/src/index/mod.rs:199`, threshold = 4 candidates) parallelises
  candidate verification within a single hash chain. Independent of intra-
  file parallelism: A's per-stripe matchers each call this routine on their
  own chains; B and C operate at a different layer.
- **Decoupled disk commit thread** (`crates/transfer/src/pipeline/receiver.rs`,
  `pipelined_receiver`). The disk-write side of the receiver already runs
  on a separate thread connected by an SPSC `ArrayQueue` (see
  `crates/transfer/src/pipeline/spsc.rs`). Intra-file parallelism feeds the
  same disk-commit thread with tokens; no change there.

Composition rule: when `file_size >= INTRA_FILE_PARALLEL_THRESHOLD_BYTES`,
the receiver MUST process the file with the intra-file parallel matcher and
MUST NOT dispatch sibling files via the inter-file `ParallelDeltaPipeline`
on the same rayon pool. Otherwise the rayon thread pool sees N stripe
workers per large file plus M sibling-file workers, each trying for cores,
and contention dominates. The simpler rule is: large file -> intra-file;
many small files -> inter-file; never both for the same batch.

## Risks

1. **Cache thrashing across NUMA.** At 64-way on a 2-socket box each worker
   reads its own stripe of the input plus the shared `Arc<DeltaSignatureIndex>`
   in the page cache. If the index is allocated on socket 0 and worker 32 is
   pinned to socket 1, every probe walks the inter-socket interconnect.
   Mitigation: pin workers to the socket where they will read the input
   (input is paged in by socket-local page faults if `madvise(MADV_RANDOM)` is
   used). NUMA-aware affinity is tracked as a follow-up TODO below.

2. **Memory pressure on small machines.** 64-way x 1 MB blocks = 128 MB per
   in-flight file. On an 8 GB Raspberry Pi or a CI runner with 4 GB this is
   fine for one file at a time but pathological for many. Mitigation: cap
   worker count by `min(num_cpus, available_memory / (8 * block_length))`.

3. **`ReorderBuffer` bloat.** When intra-file parallelism is active, each
   stripe produces a tagged token stream that the merge layer reorders.
   `ReorderBuffer` (`crates/engine/src/concurrent_delta/reorder.rs:65`)
   already implements ordered drain over arbitrary completion order. For
   intra-file use, the index space is per-stripe (0..N-1) not per-file, and
   the buffer is short-lived (one file). Risk: heap fragmentation if many
   large files run back-to-back. Mitigation: pool the `ReorderBuffer`
   instance across files in the same transfer.

4. **Golden byte test stability.** Tests in `crates/protocol/tests/golden/`
   record byte-exact wire dumps. Intra-file parallelism MUST produce the
   same wire bytes as serial matching. The merge dedup invariant
   (no two workers emit overlapping COPY ranges; LITERAL ranges fill the
   gaps) is the load-bearing guarantee. A property test must prove it
   on randomly chosen stripe counts and stripe sizes against the serial
   matcher; without that test the golden suite is the only line of
   defence and risks a flaky landing.

5. **Signature index race.** `DeltaSignatureIndex` is shared via `Arc`. The
   index is fully constructed before workers spawn and is read-only inside
   `generate()`. The `tag_table` and `lookup` fields on
   `crates/match/src/index/mod.rs:38-48` are `Vec<bool>` and
   `FxHashMap<(u16, u16), Vec<usize>>` respectively, both of which are
   `Sync` for read access. No locks needed. Risk: a future change adding
   interior mutability to the index would silently break parallelism.
   Mitigation: assert `DeltaSignatureIndex: Sync` in a `static_assertions`
   compile-time check at the matcher entry point.

6. **Adversarial inputs.** A pathologically constructed input could place
   matches such that worker k's match always crosses into worker k+1's
   stripe, doubling the matching work in the overlap zone. This is an O(N)
   factor at most, not asymptotic, and bounded by `B` bytes per boundary.
   The cost is a constant overhead, not a denial-of-service vector.

## Implementation sketch

The change splits cleanly across crate boundaries.

`crates/match/src/generator.rs`:

- New entry point `DeltaGenerator::generate_parallel<R: Read>` that takes a
  `seekable` reader (or memory-mapped buffer), spawns `N` rayon threads,
  hands each a stripe range, and collects tagged tokens.
- Existing `generate` stays as the serial fallback, called below the size
  threshold.
- Both paths share `DeltaGenerator::match_block` helpers; only the outer
  loop changes.

`crates/transfer/src/transfer_ops/streaming.rs`:

- At the call site of `apply_delta_stream` (around line 74), when the
  `FileEntry::length() >= INTRA_FILE_PARALLEL_THRESHOLD_BYTES` and the
  basis file is mmap-friendly, call into `generate_parallel`. Otherwise
  fall through to the current serial path.
- The receiver does not see tokens, only applied bytes; the parallel path
  still emits COPY/LITERAL through the same `DeltaApplicator` interface.

`crates/transfer/src/delta_pipeline.rs`:

- Add `INTRA_FILE_PARALLEL_THRESHOLD_BYTES` and a small helper
  `should_use_intra_file_parallel(file_size: u64) -> bool`.

`crates/engine/src/concurrent_delta/reorder.rs`:

- Reuse `ReorderBuffer` for the merge step, parameterised on
  `TaggedToken`.

`crates/transfer/src/pipeline/spsc.rs`:

- No change. The SPSC queue between network ingest and disk commit is
  unaffected.

Approach C lands first because it is local to
`find_match_slices_sequential` (`crates/match/src/index/mod.rs:178`); B
lands second by extending the existing decoupled receiver thread; A lands
last because it is the largest change and the only one that needs careful
boundary handling.

## Test surface

PRs MUST keep all tests in this list green and add new ones as noted.

Existing:

- `crates/match/tests/block_matching_accuracy.rs` (~20 correctness tests).
- `crates/match/tests/integration_tests.rs` (~70 correctness tests).
- `crates/match/src/index/tests.rs` (12 correctness tests including
  `find_match_bytes_uses_strong_checksum_for_collision`).
- `crates/checksums/tests/rolling_simd_parity.rs` (proptest, SIMD vs scalar).
- `crates/checksums/tests/rsync_rolling_compat.rs` (parity, upstream).
- `crates/protocol/tests/golden/` (wire-format byte goldens).
- `tools/ci/run_interop.sh` (full interop matrix).

New (one per follow-up PR):

- `crates/match/tests/intra_file_parallel_parity.rs`: proptest comparing
  tokens emitted by `generate` and `generate_parallel` for random inputs
  and random stripe counts. Must be byte-identical for every input.
- `crates/match/tests/intra_file_parallel_boundary.rs`: hand-constructed
  inputs placing matches exactly at, just before, and just after each
  stripe boundary, for stripe counts 2, 4, 16, 64.
- `crates/transfer/tests/large_file_intra_parallel.rs`: end-to-end test
  with a 256 MB synthetic file, dest = empty, compare wire dump.
- `crates/match/benches/intra_file_parallel.rs`: criterion benchmark over
  stripe counts {1, 2, 4, 8, 16, 32, 64} and file sizes {16 MB, 64 MB,
  256 MB, 1 GB}.

## Tracking (informational, NOT added to the persistent TODO list)

These four follow-up TODOs would land separately after this design note
merges. They are listed here as a roadmap; they are not added to the
persistent TODO list.

1. **Implementation.** Wire `generate_parallel` into
   `crates/match/src/generator.rs` with the spatial-split approach plus
   the merge dedup. Land approach C and B first as smaller wins; land A
   behind the new size threshold.
2. **Threshold tuning.** Microbenchmark
   `INTRA_FILE_PARALLEL_THRESHOLD_BYTES` on 4-core, 16-core, and 64-core
   machines. Pick the smallest threshold that keeps the parallel path no
   slower than serial for all measured points.
3. **Golden tests.** Add the two parity / boundary test files listed
   above and extend the protocol golden suite to cover at least one large-
   file fixture.
4. **NUMA-aware affinity.** On Linux multi-socket machines, pin stripe
   workers to NUMA nodes via `sched_setaffinity` and allocate per-worker
   buffers from node-local arenas. Gate behind `cfg(target_os = "linux")`
   with a no-op on macOS/Windows. Track measurable speedup before
   landing.

## Decision record

This note records the design space; the keep-or-revert decision for each
of A, B, C is recorded in the per-technique PR descriptions once
benchmarks land. The default for any path that does not show >= 1.5x
speedup on a 16-core machine for a 1 GB synthetic file is to revert.

## References

- `crates/transfer/src/receiver/transfer/pipeline.rs:38`
  `ReceiverContext::run_pipeline_loop_decoupled`
- `crates/transfer/src/receiver/transfer/pipeline.rs:299`
  call site of `process_file_response_streaming`
- `crates/transfer/src/transfer_ops/streaming.rs:74`
  `process_file_response_streaming<R: Read>`
- `crates/transfer/src/delta_apply/applicator.rs:436`
  `apply_delta_stream`
- `crates/match/src/generator.rs:81-300` `DeltaGenerator::generate`
- `crates/match/src/index/mod.rs:151` `find_match_slices`
- `crates/match/src/index/mod.rs:178` `find_match_slices_sequential`
- `crates/match/src/index/mod.rs:199` `find_match_slices_parallel`
- `crates/match/src/index/mod.rs:229` `check_block_match_slices` (`want_i` hint)
- `crates/match/src/ring_buffer.rs` `RingBuffer`
- `crates/checksums/src/rolling/checksum/mod.rs:333` `RollingChecksum::roll`
- `crates/signature/src/parallel.rs:84` `generate_file_signature_parallel`
- `crates/signature/src/block_size.rs` `calculate_block_length`
- `crates/transfer/src/delta_pipeline.rs:42`
  `DEFAULT_PARALLEL_THRESHOLD = 64`
- `crates/transfer/src/parallel_io.rs:16`
  `DEFAULT_STAT_THRESHOLD = 64`
- `crates/transfer/src/pipeline/spsc.rs` SPSC `ArrayQueue` channel
- `crates/engine/src/concurrent_delta/reorder.rs:65` `ReorderBuffer`
- `docs/design/zsync-inspired-matching.md` companion note for matching-
  level optimisations on the serial path
- Upstream rsync 3.4.1 `match.c` (sequential matcher reference)
