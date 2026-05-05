# zsync-inspired matching pipeline optimizations

Internal acceleration of the rolling-checksum search and block-match pipeline
using techniques from zsync 0.6.2's `librcksum`. All optimizations here are
**purely in-memory** - they MUST NOT change anything serialized on the wire,
the protocol-32 negotiation, the signature payload format, or the
file-list / NDX framing.

## Goals and non-goals

- Reduce CPU on the hot path: skip rolling-hash work that cannot match,
  skip strong-checksum work on already-matched blocks, and skip table
  probes on confirmed-miss positions.
- Stay byte-identical on the wire against upstream rsync 3.0.9, 3.1.3,
  3.4.1 in both directions.
- Keep the SIMD parity invariant for the rolling Adler hash
  (AVX2/SSE2/NEON/scalar all produce the same digest).
- **Not** an attempt to add zsync transfer mode. No `.zsync` files, no
  HTTP range requests, no protocol extensions.

## Wire-compat invariants

The following must hold for every PR landing one of these techniques:

1. `crates/protocol/tests/golden/` byte-comparison tests pass unchanged.
2. `tools/ci/run_interop.sh` against upstream 3.0.9 / 3.1.3 / 3.4.1
   produces zero new entries in `tools/ci/known_failures.conf`.
3. `tcpdump`-captured application-layer payloads for an oc-rsync push
   to upstream daemon are byte-identical with the optimization on vs
   off (see #2075).
4. No new flag in `crates/cli/`. Internal toggles, if any, are cfg-gated
   benchmark scaffolding, never `clap` arguments. Decision recorded in
   #2054 before merge.

## Current matching pipeline

```
RECEIVER DATA ARRIVES
     |
     v
crates/transfer/src/receiver/transfer/pipeline.rs:run_pipeline_loop_decoupled()
     |
     v
crates/transfer/src/transfer_ops.rs:process_file_response_streaming()
     |
     v
crates/transfer/src/delta_apply.rs:apply_delta_to_stream()
     |
     v
DeltaGenerator::generate() [crates/match/src/generator.rs:81]
     |
     +- RingBuffer::push_back() [crates/match/src/ring_buffer.rs:93]
     |  (fills byte-by-byte window to block_length)
     |
     +- RollingChecksum::roll() [crates/checksums/src/rolling/checksum/mod.rs:333]
     |  (SIMD-dispatched x86 AVX2/SSE2 or aarch64 NEON, scalar fallback)
     |
     v
[want_i hint check, then tag_table prefilter] [generator.rs:177]
     |
     v
DeltaSignatureIndex::find_match_slices() [crates/match/src/index/mod.rs:151]
     |
     +- tag_table[sum1] -> bool   (1 KB, L1-resident)
     |
     +- lookup.get(&(sum1, sum2)) (FxHashMap<(u16,u16), Vec<usize>>)
     |
     +- strong-checksum verify (algorithm chosen by negotiation:
        MD4/MD5/XXH64/XXH3-64/XXH3-128/SHA1/SHA256)
```

The hot data structures:

- `RollingChecksum`: ~16 B (`s1: u32`, `s2: u32`, `len: usize`)
- `DeltaSignatureIndex`: ~135 B inline + tag_table ~1 KB + lookup
  ~200 KB typical (per 1000 blocks ~60-100 KB of `SignatureBlock`)
- `RingBuffer`: ~40 B + buffer of `block_length` bytes

## Insertion points

The four techniques hook in at four named locations. PRs MUST stay
within these scopes; touching anything else makes the change
wire-affecting risk-wise even if not bytes-wise.

| Technique     | File                                     | Line | What changes                                                                                |
|---------------|------------------------------------------|------|---------------------------------------------------------------------------------------------|
| Bithash       | `crates/match/src/index/mod.rs`          | 165  | new `BitHash` field; probe gates on `probably_present(rsum)` before `lookup.get`            |
| Seq-match     | `crates/match/src/generator.rs`          | 177  | after a confirmed match, try block `index+1` directly via the existing `want_i` hint surface|
| Prune         | `crates/match/src/generator.rs`          | 214  | mark matched block_index in a bitmap; probe skips matched bits unless duplicate exists      |
| Compact key   | `crates/match/src/index/mod.rs`          | 44   | swap `FxHashMap<(u16,u16), Vec<usize>>` for a packed `(rsum_low, block_idx)` layout         |

## zsync source mapping

zsync librcksum (gianm/zsync mirror, line-equivalent to 0.6.2):

### Bithash prefilter

- Sized 8x larger than the main rsum hash table:
  `bithashmask = (2 << (i + BITHASHBITS)) - 1` with `BITHASHBITS = 3`
  (`internal.h:83`).
- Insertion: `bithash[(h & bithashmask) >> 3] |= 1 << (h & 7)`
  (`hash.c:101-102`). Bloom-style with one hash function.
- Probe (`rsum.c:362-366`) tests the bithash bit BEFORE descending the
  bucket chain. Density bound: `N_blocks / bithash_size <= 1/8`,
  rejecting ~7/8 of random rsums in O(1).
- Memory: ~1 byte per block.

### Sequential match heuristic (`seq_matches`)

- After a confirmed match at index `i`, set `next_match` to point at
  block `i+1`'s hash entry (`rsum.c:262`).
- Top of next loop iteration (`rsum.c:352-356`) probes ONE entry
  directly with `onlyone=1`, skipping the rolling-hash byte loop.
- `next_match` is cleared on miss (`rsum.c:190`); a misprediction
  costs nothing beyond one wasted strong-checksum.

### Matched-block hash removal

- Triggered at `write_blocks` (`rsum.c:109-119`), AFTER the data has
  been written, not at first match.
- `remove_block_from_hash` (`hash.c:111-128`) walks the singly-linked
  chain unlinking only THAT specific entry.
- Duplicate-block correctness: each target block has its own
  `hash_entry` struct sharing a chain; removing one leaves siblings.
- The bithash bit is NOT cleared on removal (no reverse mapping); the
  chain lookup is the authoritative gate.

### Compact key (`rsum_a_mask`)

- For small target files, zsync shrinks the in-memory key:
  `mask = rsum_bytes < 3 ? 0 : rsum_bytes == 3 ? 0xff : 0xffff`
  (`state.c:48`).
- Stored AND probed with the same mask: weak-checksum equality is
  on the masked key only.
- Strong checksum (MD4 in zsync) still uses the full block bytes;
  rolling-key collisions are filtered by the strong stage.

## Translation rules

Wire-compat constrains every translation:

1. **rsync wire format is fixed** at 4 bytes of rolling sum. Do NOT
   change anything serialized. The full rolling sum stays in
   `SignatureBlock`. Only the in-memory hash key may be compacted.
2. **Strong-checksum algorithm** is determined by capability
   negotiation; the `ChecksumStrategySelector` governs it. Never
   change the verification step.
3. **Duplicate-block correctness:** when pruning, evict by
   `block_index` (the matched-blocks bitmap), not by the
   `(rsum, strong)` tuple. Two blocks with identical content occupy
   distinct `block_index` slots; pruning one leaves the other.
4. **INC_RECURSE segments:** state (matched-bitmap, bithash,
   `next_match` hint) lives inside `DeltaSignatureIndex`. Every
   per-segment `MatchIndex::build` rebuilds them from scratch, which
   is the existing pattern (`builder.rs:71-98`).
5. **Append/inplace:** the existing append path skips signature blocks
   entirely (`receiver/transfer/transfer.rs:206`). Optimizations must
   guard against missing signatures and cleanly degrade.
6. **Fuzzy matching** runs BEFORE delta computation. Optimizations may
   not change which basis file is selected; they only optimize delta
   within that basis.

## Test surface

PRs MUST keep all tests in this list green.
Inventory categorized by category in `docs/design/zsync-test-inventory.md`
(produced by #2056). Highlights:

- `crates/match/tests/block_matching_accuracy.rs` (~20 correctness tests)
- `crates/match/tests/integration_tests.rs` (~70 correctness tests)
- `crates/match/src/index/tests.rs` (12 correctness tests including
  `find_match_bytes_uses_strong_checksum_for_collision`)
- `crates/checksums/tests/rolling_simd_parity.rs` (proptest, SIMD vs scalar)
- `crates/checksums/tests/rsync_rolling_compat.rs` (parity, upstream)
- `crates/checksums/tests/rolling_signed_byte_regression.rs` (PR #3636,
  added as guard for the `i8` interpretation invariant before any
  bithash wiring lands)
- `crates/protocol/tests/golden/` (wire-format byte goldens)
- `tools/ci/run_interop.sh` (full interop matrix)

## Not present in upstream rsync

The audit of `match.c`, `token.c`, and `checksum.c` in upstream rsync
3.4.1 confirms that **none** of the four zsync techniques exist in
upstream:

- Bithash: absent. Upstream uses a simple `bool` tag table indexed by
  the low 16 bits of `s1` (`match.c:45-51`).
- `seq_matches`: absent. Upstream uses a `want_i` adjacent-block hint
  that only checks `i+1`, not multi-block sequential probing
  (`match.c:289-300`).
- Matched-block hash removal: absent. Upstream chains stay intact
  for the duration of the transfer.
- Compact key: absent. Upstream uses 32-bit (or modulo) keys; no key
  compression.

The design space is therefore fully open - no upstream constraint
blocks any of the four. The constraint is purely "do not change wire
bytes," which all four techniques honour by construction.

## Per-technique PR plan

Each technique lands as its own PR with the structure: design (this
note's section), implementation, property test, benchmark. The four
PRs are blocked-by ordered:

```
audits (#2055-#2058) [done]
   |
   v
this design note (#2053) [this PR]
   |
   +-> bithash design (#2059) -> impl (#2060,#2061) -> tests (#2062) -> bench (#2063)
   |       (gated by signed-byte fixture #2076 [PR #3636])
   |
   +-> seq-match design (#2064) -> impl (#2065) -> tests (#2066) -> bench (#2067)
   |
   +-> prune design (#2068) -> impl (#2069) -> tests (#2070) -> bench (#2071)
   |
   +-> compact-keys prototype (#2072) -> bench (#2073)
   |       (revert if cache-miss gain < 5%)
   |
   v
adversarial tests (#2078-#2080)
   |
   v
benchmarks medium/large (#2081, #2082)
   |
   v
final cleanup (#2084-#2087) and decision record (#2054)
```

## Sizing the bithash

For oc-rsync, recommended sizing as a function of block count `N`:

| N (blocks) | Buckets (`2^k`)     | Bithash bytes | False-positive rate |
|------------|---------------------|---------------|---------------------|
| 1 K        | 2^10                | 1 KB          | ~12.5%              |
| 100 K      | 2^17                | 16 KB         | ~12.5%              |
| 10 M       | 2^23                | 1 MB          | ~12.5%              |

Cap at 16 MB to prevent runaway allocation on adversarial inputs.
The `2^k` bucket count tracks `4 * N` rounded up to a power of two,
matching zsync's `i` selection at `hash.c:62-84`.

False-positive cost: one wasted bucket lookup. False-negative cost:
none; bithash is one-sided. Memory cost: ~1 byte per block, vs
~60-100 bytes for the existing `SignatureBlock` payload, so a
negligible fraction of total signature memory.

## Decision record

Decisions and benchmarks recorded in `docs/design/zsync-test-inventory.md`
and the per-technique PR descriptions. Final keep-or-revert decision
for each technique is recorded in #2054 once benchmarks land.

## References

- zsync 0.6.2 source (gianm/zsync mirror):
  - `librcksum/internal.h`, `hash.c`, `rsum.c`, `state.c`, `rcksum.h`
- Upstream rsync 3.4.1: `target/interop/upstream-src/rsync-3.4.1/`
  - `match.c`, `token.c`, `checksum.c`
- oc-rsync match crate: `crates/match/src/{generator.rs, optimized_search.rs,
  script.rs, ring_buffer.rs, index/{builder,mod}.rs}`
- oc-rsync rolling Adler: `crates/checksums/src/rolling/checksum/{mod,x86,neon}.rs`
