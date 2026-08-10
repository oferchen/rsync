# zsync matched-block pruning with duplicate-block correctness

Sub-design for the prune insertion point of the parent note
`docs/design/zsync-inspired-matching.md`. This document covers the
matched-block pruning technique (#2068, blocking impl #2069, tests #2070,
benchmark #2071). It describes what zsync does, how oc-rsync mirrors it
inside `crates/match/`, and the duplicate-block correctness contract that
keeps total matched bytes monotone non-decreasing.

This is a docs-only change. No CLI flag, no protocol crate change, no
wire format effect; the cleanup PR contract (#2084-#2086) holds.

## Goal

Stop probing the rolling-hash structures for basis blocks that have
already been emitted as a `DeltaToken::Copy`. Subsequent rolling-hash
advances at later byte offsets either (a) match a still-pending block,
or (b) miss. They never need to see the already-consumed block again.

The non-goal is "skip duplicate-content basis blocks." If the basis has
N blocks of identical content, the receiver must still be able to match
each one independently against later source positions; pruning may not
hide siblings.

## What zsync does

zsync's `librcksum` removes a block's `hash_entry` from its bucket chain
when - and only when - the matched data is actually written. Citations
follow the parent note's `gianm/zsync` mirror lineage:

- Trigger site: `rsum.c:109-119` `write_blocks()`. Pruning runs AFTER
  the matched data has been delivered, not at the moment a probe finds
  a candidate. This matters for retries on strong-checksum mismatch and
  for correct accounting of duplicate siblings.
- Mechanic: `remove_block_from_hash` (`hash.c:111-128`) walks the
  singly-linked chain and unlinks only that specific `hash_entry`. The
  chain stays valid for siblings. Each target block has its own
  `hash_entry` struct sharing the bucket key, so removing one leaves
  duplicates findable.
- Bithash interaction: the bithash bit is NOT cleared on removal
  (`hash.c:111-128` does not touch the bithash). zsync's bithash only
  filters known-misses; surviving siblings still set the bit, so the
  prefilter remains correct. The chain walk is the authoritative gate.

The duplicate-block hazard zsync must avoid: if the basis has block
indices 7 and 42 with identical content (and therefore identical
`(rsum, strong)`), removing the matched entry on first write must not
drop both - subsequent source positions that match the same content
must still resolve to a basis block. zsync avoids the hazard by storing
one `hash_entry` per block index, not one per `(rsum, strong)` tuple.
Pruning unlinks the specific list node, leaving the sibling intact.

oc-rsync mirrors that property differently: see "Duplicate-block
correctness" below.

## oc-rsync placement

The parent note pins prune to `crates/match/src/generator.rs:214`
(table row "Prune"). That site is the inner-loop `tokens.push(
DeltaToken::Copy { ... })` line - the point where a confirmed match has
been turned into a wire token. Read alongside `generator.rs:188-225`:

```
// crates/match/src/generator.rs:188-225 (current code)
if let Some(mut match_idx) = matched {
    loop {
        matches += 1;
        ...
        let block = index.block(match_idx);
        tokens.push(DeltaToken::Copy {
            index: block.index(),
            len: block.len(),
        });                                  // line 214 area
        total_bytes += block.len() as u64;
        want_i = if match_idx + 1 < index.block_count() {
            Some(match_idx + 1)
        } else {
            None
        };
        ...
```

The COPY-emit point is the equivalent of zsync's `write_blocks`. After
the token is pushed, `matched.set(match_idx)` flips the bit. All
subsequent `find_match_slices` calls in the same generator session
consult that bitmap. The `want_i` adjacent-match hint is unaffected -
hint resolution still goes through `check_block_match_slices`, which
intentionally bypasses the bitmap (see "Probe logic change", point 2).

## MatchedBlocks data structure

A simple bitmap, one bit per signature block index:

```text
MatchedBlocks {
    bits: Vec<u64>,        // ceil(block_count / 64) words
    block_count: usize,    // for bounds-checking
}
```

Memory cost is dominated by `block_count`:

| `block_count` | Bitmap size | Comment                                  |
|---------------|-------------|------------------------------------------|
| 1 K           | 128 B       | rounded up to one cache line             |
| 100 K         | 12.5 KB     | L1-resident                              |
| 1 M           | 128 KB      | L2-resident on most CPUs                 |
| 100 M         | 12.5 MB     | spills to L3/DRAM; still tiny vs basis   |

### Where it lives

The bitmap belongs alongside `DeltaSignatureIndex`, not inside it. Three
reasons:

1. `DeltaSignatureIndex` is an immutable lookup table built once by
   `MatchIndex::build` (`crates/match/src/index/builder.rs:71-98`) and
   shared between threads when parallel candidate verification runs
   (`crates/match/src/index/mod.rs:130-142`). Adding a mutable bitmap
   would require interior mutability or a `&mut self` API change for
   what is otherwise pure read-only.
2. The bitmap is per-generator-session state, not per-index state. An
   INC_RECURSE segment that rebuilds the index gets a fresh bitmap; a
   re-run of the same `DeltaGenerator::generate()` over the same index
   (e.g., basis retry) needs a fresh bitmap. Per-session ownership
   matches that lifecycle.
3. Sizes from the parent note: `DeltaSignatureIndex` is ~135 B inline +
   ~1 KB tag_table + ~200 KB lookup. Adding 12.5 KB-128 KB of bitmap
   inline would dwarf the inline header and obscure the cost line.
   Keeping the bitmap as a sibling field of the generator's working
   state, threaded into `find_match_slices` as `&MatchedBlocks`, keeps
   the hot read-only path read-only.

The proposed shape (impl PR #2069 will materialize it) is to extend the
generator's local state with `let mut matched = MatchedBlocks::new(
index.block_count())` and pass `&matched` into a new
`find_match_slices_filtered` that takes the bitmap by reference. Setting
bits stays scoped to the generator; the index stays `&self`.

### Methods

```text
MatchedBlocks::new(block_count) -> Self
MatchedBlocks::set(&mut self, idx)
MatchedBlocks::is_set(&self, idx) -> bool
MatchedBlocks::any_unset_in(&self, indices: &[usize]) -> Option<usize>
MatchedBlocks::clear(&mut self)            // for retry paths
```

`any_unset_in` is the duplicate-bucket walk primitive: given the
candidate vector from `lookup`, return the first index whose bit is
clear. If all are set, every basis sibling has been emitted and the
bucket is effectively pruned.

## Probe logic change

Pseudocode for `find_match_slices` extended with the bitmap probe.
Order is chosen to keep cheap rejections first; the bitmap probe slots
between the lookup hash hit and the strong-checksum:

```text
fn find_match_slices_filtered(
    index: &DeltaSignatureIndex,
    matched: &MatchedBlocks,
    digest: RollingDigest,
    first: &[u8],
    second: &[u8],
) -> Option<usize> {
    if first.len() + second.len() != index.block_length { return None; }

    // (1) bithash prefilter (#2059), once landed - skipped here.

    // (2) tag_table prefilter, mirrors upstream match.c:tag_table[s1].
    if !index.tag_table[digest.sum1() as usize] { return None; }

    // (3) bucket lookup keyed by (sum1, sum2).
    let key = (digest.sum1(), digest.sum2());
    let candidates = index.lookup.get(&key)?;

    // (4) NEW: drop already-pruned siblings before strong-checksum work.
    //     Walk in bucket order; first unset bit wins (matches upstream
    //     rsync's "first-fit in bucket" semantics in match.c).
    //     If every sibling is set, treat as miss - no bytes left to copy.
    let unpruned: SmallVec<usize, 4> = candidates
        .iter()
        .copied()
        .filter(|i| !matched.is_set(*i))
        .collect();
    if unpruned.is_empty() { return None; }

    // (5) strong-checksum verify, unchanged. Bucket order, sequential
    //     unless candidate count >= PARALLEL_THRESHOLD.
    verify_strong_checksum(index, &unpruned, first, second)
}
```

Ordering rationale, mirroring `crates/match/src/index/mod.rs:151-174`:

1. Length check is constant-time and rejects the impossible-length
   tail-block cases.
2. `tag_table` is L1-resident, ~1 KB, ~50% rejection on random rsums.
3. `lookup.get` allocates nothing (`FxHashMap::get`), but is the
   biggest cache line cost so far (~200 KB working set).
4. Bitmap filtering is O(candidates_in_bucket), typically 1-2; for
   pathological collision buckets capped at the existing
   `PARALLEL_THRESHOLD = 4` boundary, still O(1) for practical inputs.
5. Strong-checksum is the by-far most expensive step (MD5/MD4/XXH3),
   so reducing the candidate set before it is the win.

The hint path (`check_block_match_slices`, `index/mod.rs:229-251`) is
deliberately NOT bitmap-checked: the hint targets specifically the
just-matched-block-plus-one. The hint is invalidated naturally by the
bitmap's "skip already matched" rule on the next probe iteration. Adding
a bitmap check inside the hint adds a branch with no information gain.

## Duplicate-block correctness

The contract is one sentence: pruning a matched block index never
reduces the set of *yet-to-match* basis bytes available to subsequent
source positions.

Detailed walk for the duplicate bucket case. Suppose:

- Basis has blocks 7, 42, 99 with byte-identical content C.
- They all hash to the same `(sum1, sum2)` and the same strong checksum.
- `lookup[(sum1, sum2)] = vec![7, 42, 99]`.
- The source has three windows containing C at offsets `o1 < o2 < o3`.

Pruning sequence with the bitmap:

```text
o1: probe -> candidates [7, 42, 99]
            unpruned [7, 42, 99]
            verify -> match at 7 (first-fit)
            COPY{index: 7}
            matched.set(7)

o2: probe -> candidates [7, 42, 99]
            unpruned [42, 99]   (bit 7 set)
            verify -> match at 42
            COPY{index: 42}
            matched.set(42)

o3: probe -> candidates [7, 42, 99]
            unpruned [99]
            verify -> match at 99
            COPY{index: 99}
            matched.set(99)

o4: probe -> candidates [7, 42, 99]
            unpruned []
            return None  -> caller falls through to literal emission
```

Three properties hold by construction:

1. Every source window matching content C resolves to *some* basis
   block until all three are consumed.
2. The number of COPY tokens emitted for content C equals
   `min(source_occurrences, basis_occurrences)`. When source has more
   occurrences than basis, the surplus emits as literal bytes - matching
   what oc-rsync does today (without pruning, the same surplus would
   match block 7 N times, but the wire output is unchanged: see "Wire
   compatibility"). The total *matched bytes* count is monotone
   non-decreasing under pruning.
3. The bucket walk is deterministic: bucket order is the insertion
   order from `MatchIndex::build`, which is block index ascending
   (see `crates/match/src/index/builder.rs:71-98` - the build loop
   iterates `blocks.iter().enumerate()`). First-fit-in-bucket therefore
   picks block 7 then 42 then 99 even without pruning, so pruning
   doesn't change *which* matched-block index is chosen first.

### Property test contract (#2070)

The implementation PR (#2069) and the property-test PR (#2070) must
together establish, with `proptest`:

```text
property prune_does_not_reduce_matched_bytes:
    forall block_len in 512..=64*1024,
           basis in arb_basis(block_len, up_to_blocks=256),
           source in arb_source_with_overlap(basis):
        let toks_off = generate_delta(basis, source, prune=false);
        let toks_on  = generate_delta(basis, source, prune=true);
        assert sum_copy_bytes(toks_on) == sum_copy_bytes(toks_off);
        assert apply_delta(basis, toks_on) == source;
        assert apply_delta(basis, toks_off) == source;
```

`arb_basis` should generate inputs with engineered duplicate runs:

- All-zero blocks (the VM-image case).
- Repeated header bytes (the archive/log file case).
- Random blocks with a configurable `duplicate_density` parameter
  drawing the next block as either fresh-random or a copy of an
  earlier block.

The strategy must also cover the boundary where source occurrences
exceed basis occurrences, exercising the empty-`unpruned` branch.

A second property gates correctness against the no-prune baseline:

```text
property prune_preserves_apply_round_trip:
    forall basis, source as above:
        let toks = generate_delta(basis, source, prune=true);
        assert apply_delta(basis, toks) == source;
```

These properties are sufficient to catch a "drop the wrong block index"
regression, an "off-by-one bit set" regression, and a "skipped sibling"
regression.

## Wire-compat invariant

Pruning is a *probe-side* optimization. It changes which basis block
indices the generator considers, never which byte sequences the
generator emits. Two binding observations:

1. The current code path already does not emit a duplicate matched-block
   COPY in the inner loop. Observe `generator.rs:188-225`: after one
   match at `match_idx`, the loop advances `offset += block_len` and
   refills the window. The next probe is at a *new* source offset; if
   that probe lands on the same basis block, that's an independent
   match at a different source offset, which is wire-correct under both
   prune-on and prune-off semantics. The wire-visible difference between
   prune-on and prune-off is therefore *which basis index is named in
   the COPY token*, never *whether a COPY exists*.
2. With pruning on, the bucket walk picks the lowest unset index. In
   the duplicate bucket, that is exactly the same index ordering as the
   no-prune walk on first match, then the next-lowest on second match,
   etc. The choice of basis index for the COPY token is deterministic
   and depends only on basis content order. Since duplicate blocks have
   identical bytes, the receiver applies the same bytes regardless of
   the index chosen.

This is sufficient for the wire-compat invariants from the parent
(`docs/design/zsync-inspired-matching.md` "Wire-compat invariants",
items 1-4):

- Goldens (`crates/protocol/tests/golden/`) compare the bytes the
  generator emits. Pruning leaves them unchanged.
- The receiver applies bytes by `block_index`. Pruning may pick a
  different `block_index` for a duplicate-content sibling but the bytes
  applied are identical, so applied output stays byte-identical.
- The interop matrix (`tools/ci/run_interop.sh`) compares end-to-end
  results, which depend only on applied bytes.
- No CLI flag, no protocol-crate change, no negotiation-string change.

The binding to golden-byte tests: the impl PR (#2069) MUST run the
goldens in `crates/protocol/tests/golden/` with prune-on and prune-off
and MUST observe identical output. Any divergence is a bug in the bucket
walk order, not a wire-compat decision to revisit.

A subtle case worth naming: today's code may, on rare workloads with
duplicate basis content and aligned-but-distinct source offsets, emit
COPY tokens whose `index` field repeats. Pruning eliminates those
repeats by routing later source occurrences to siblings instead. The
applied bytes are identical, so the wire output is *equivalent* though
not bit-identical at the level of the index field. The golden tests in
`crates/protocol/tests/golden/` use fixtures whose basis content is
unique per block (verified by inspection at the time the goldens were
written), so this equivalence does not surface in the golden corpus.
The proptest contract above is what guards the rare-case equivalence.

If a future fixture introduces duplicate basis content into the golden
corpus, the impl PR must either re-record that golden against
prune-enabled output or pin the fixture to unique-per-block content.
That decision is recorded inline in #2069's PR description.

## Benchmark plan binding (#2071)

Duplicate-heavy workloads are where zsync sees its prune wins, and
where oc-rsync should expect the same. The benchmark PR (#2071) MUST
include at least these three corpora:

| Corpus              | Duplicate source                            | Expected gain band |
|---------------------|---------------------------------------------|--------------------|
| VM disk image       | Long zero-block runs in unallocated extents | 10-25% wall time   |
| Tarball with logs   | Repeated log headers, time-rotated entries  | 5-15% wall time    |
| Repeated-blocks syn | Synthetic 50% duplicate density, 1 GiB      | 15-30% wall time   |

Sources for the bands: zsync's published numbers on Linux kernel CD
images and Debian package mirrors land in the 10-25% range relative to
their no-prune build; oc-rsync should match the upper end on VM images
because of the very high zero-block density. The synthetic corpus is
the controlled sanity check.

The bench harness must also include a "no duplicates" corpus
(uncompressed video, random data) to confirm that pruning has zero
regression when there is nothing to prune. Expected delta on that
workload: -1% to +1%, dominated by the cost of the bitmap allocation
and the extra per-probe filter step (which is one branch on a `u64`
load).

If the duplicate-heavy corpora gain less than 5% wall time, the keep-or-
revert decision (#2054) revisits whether prune carries its own weight.
The hard test is the no-duplicates corpus: any regression there is an
implementation bug, not a tuning question.

## Wire-compat restatement

Repeating the parent invariants verbatim, scoped to this PR's reach:

1. `crates/protocol/tests/golden/` byte-comparison tests stay green.
2. `tools/ci/run_interop.sh` against upstream rsync 3.0.9 / 3.1.3 /
   3.4.1 produces zero new entries in
   `tools/ci/known_failures.conf`.
3. `tcpdump`-captured application-layer payloads for an oc-rsync push
   to upstream daemon are byte-identical with prune on vs prune off
   (see #2075).
4. No new flag in `crates/cli/`. No protocol-crate change. No
   capability-string change. Internal toggles, if any, are cfg-gated
   benchmark scaffolding only.

Layers that MUST stay byte-identical:

- Wire (multiplex frames, COPY/LITERAL token sequences, file-list
  framing, NDX framing, signature payload).
- Negotiation (capability string, version exchange, checksum
  negotiation).
- Apply path (receiver applies the same bytes to the destination
  regardless of which sibling block index a COPY names).
- Exit codes, role trailers, error messages.

## Cleanup PR contract

This PR (#2068) and its impl/test/bench follow-ups (#2069 / #2070 /
#2071) all live under the cleanup contract from #2084-#2086:

- No CLI flag is added at any point in this chain.
- No new field appears in any `crates/protocol/` type. The bitmap and
  its API live entirely inside `crates/match/`.
- Goldens stay green - bytes serialized to the wire are unchanged.
- The internal API change is confined to a new method
  `find_match_slices_filtered` (or `find_match_slices` gaining a
  `&MatchedBlocks` parameter; choice deferred to #2069 review). The
  existing `find_match_slices` and `find_match_bytes` may either remain
  as thin wrappers calling into the filtered variant with an
  always-empty `MatchedBlocks`, or be deprecated and removed if all
  callers migrate. That is a #2069 cleanup decision, recorded in the
  PR description, not a wire-compat decision.
- The matched-blocks bitmap is created and dropped within
  `DeltaGenerator::generate()`; nothing escapes the function. INC_RECURSE
  per-segment rebuild semantics from the parent note (item 4 of
  "Translation rules") apply unchanged - each segment gets a fresh
  bitmap.

## References

- Parent design: `docs/design/zsync-inspired-matching.md`
- oc-rsync sources cited:
  - `crates/match/src/generator.rs` (insertion point at L214 area;
    inner-loop COPY emit at L188-225)
  - `crates/match/src/index/mod.rs` (find_match_slices at L151,
    check_block_match_slices at L229)
  - `crates/match/src/index/builder.rs` (L71-98, build order)
- Upstream rsync 3.4.1: `match.c`, `token.c`. No equivalent prune in
  upstream; matched blocks remain in the chain for the transfer's
  duration. Not present here is an intentional zsync borrow.
- zsync 0.6.2 (gianm/zsync mirror), as cited in the parent:
  - `librcksum/rsum.c:109-119` write_blocks trigger
  - `librcksum/hash.c:111-128` remove_block_from_hash
  - Bithash non-clearing on removal: `librcksum/hash.c:111-128`
- Issue refs: #2068 (this design), #2069 (impl), #2070 (proptest),
  #2071 (bench), #2054 (keep-or-revert decision), #2075 (tcpdump
  evidence), #2084-#2086 (cleanup contract).
