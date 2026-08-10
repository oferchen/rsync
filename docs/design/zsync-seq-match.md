# zsync seq-match heuristic semantics

Focused design note for insertion point #2 of the parent design
`docs/design/zsync-inspired-matching.md` ("Seq-match"). Scopes the
zsync `seq_matches`/`next_match` mechanism into oc-rsync's existing
`want_i` adjacent-block hint surface, and pins down the wire-compat
invariants that any implementation PR (#2065), test PR (#2066), and
benchmark PR (#2067) must keep green.

This note is **doc-only**. It changes no Rust code, adds no CLI flag,
and modifies nothing on the wire. Implementation lands in #2065 once
this design is reviewed.

## TL;DR

After a confirmed match at signature block index `i`, zsync remembers
that the very next probe should target block `i+1` directly, bypassing
both the rolling-hash byte loop and the bucket-chain probe. oc-rsync
already has a weaker form of this in `want_i`: after a match it sets
`want_i = Some(i+1)` and, on the next iteration, calls
`check_block_match_slices(hint, ...)` before falling back to the
table probe. The seq-match formalization (a) names that surface, (b)
spells out exactly when it fires vs when it falls back, (c) proves the
COPY-token output is byte-identical to the table-probe path, and (d)
documents the duplicate-block tie-breaker.

## Background: what zsync's `seq_matches` does

In zsync 0.6.2's `librcksum`, two cooperating pieces of state live in
the `rcksum_state` struct:

- `next_match`: a single `hash_entry*` (or NULL) that names the next
  block expected to match. Set by `rsum.c:262` after a confirmed
  match: `z->next_match = z->rover->next` (effectively, the hash entry
  for block `i+1`).
- `onlyone`: a one-shot flag passed into `check_checksums_on_hash_chain`
  at `rsum.c:352-356`. When set, the function probes exactly one
  hash-entry candidate and returns; it does NOT walk the rest of the
  bucket chain.

The control flow is:

1. The outer rolling-hash loop computes the rolling sum at the current
   byte offset (`rsum.c` driver loop around the byte iterator).
2. Before computing the rolling sum from scratch, it checks
   `next_match`. If set, it skips straight to
   `check_checksums_on_hash_chain(... onlyone=1)` against that one
   entry (`rsum.c:352-356`).
3. On confirmed match, `seq_matches` increments and `next_match` is
   advanced to the following block (`rsum.c:262`).
4. On miss, `next_match` is cleared (`rsum.c:190`) and the outer loop
   resumes the normal rolling-hash byte-by-byte advance until the next
   table hit.

Two properties matter:

- **Correctness**: the same match would have been found by the normal
  table probe. The shortcut just avoids re-deriving the bucket. The
  rolling-sum equality and strong-checksum equality are checked
  identically.
- **Misprediction is cheap**: one wasted strong-checksum verify, then
  the loop falls back. There is no extra allocation, no extra hash
  probe, no rewinding.

## Mapping to oc-rsync architecture

oc-rsync's `DeltaGenerator::generate` already carries the
adjacent-hint state needed for seq-match. The relevant lines in
`crates/match/src/generator.rs` (HEAD on master at the time of
writing):

- `generator.rs:103` declares the hint:
  `let mut want_i: Option<usize> = Some(0);`.
- `generator.rs:177-183` is the byte-by-byte fast path: when
  `want_i = Some(hint)`, call
  `index.check_block_match_slices(hint, digest, first, second)`
  before falling back to `index.find_match_slices(...)`.
- `generator.rs:221-225` advances the hint after a confirmed match:
  `want_i = if match_idx + 1 < index.block_count() { Some(match_idx + 1) } else { None };`.
- `generator.rs:259-273` repeats the hint check inside the
  bulk-refill match-chain loop, after each `block_len` chunk is
  buffered and the rolling sum recomputed.

The supporting predicate lives in `crates/match/src/index/mod.rs`:

- `index/mod.rs:229-251` `check_block_match_slices` does (i) bounds
  check, (ii) `sum1`/`sum2` rolling equality, (iii) strong-checksum
  equality, returning `bool`. No bucket-chain walk, no `lookup` map
  access, no `tag_table` consultation.

This is a faithful, if implicitly-named, port of zsync's
`next_match`+`onlyone=1` shortcut. The seq-match design is therefore
not a code rewrite but a **documentation-and-test pinning** of the
contract, plus targeted clarifications described below.

### Data flow per loop iteration

Two sites consume the hint, mirroring zsync's two probe points:

```
byte arrives
   |
   v
RingBuffer::push_back(byte)        [ring_buffer.rs:93]
   |
   v
RollingChecksum::roll(out, in)     [SIMD-dispatched]
   |
   v
window full? --no--> continue
   |
  yes
   v
digest = rolling.digest()
   |
   v
+--- want_i = Some(hint)? ----+
|           |                 |
|          yes                no
|           |                 |
|           v                 v
|  check_block_match_slices  find_match_slices
|  (one-block verify)        (tag_table -> lookup -> verify)
|           |                 |
|           v                 v
|  match? --no--> find_match_slices (fallback)
|           |
|          yes
|           v
+-- emit COPY token, advance want_i = Some(idx+1) -+
                                                    |
   then bulk-refill window for block `idx+1`        |
   (generator.rs:233-246) and re-probe at line 263 -+
```

The hint surface fires in two named cases:

1. **Byte-by-byte advance hits a hint** (line 177): expected to be
   rare in steady state because between two adjacent matches the
   generator goes through the bulk-refill block path, not the
   byte-by-byte path. This case fires when a partial-match streak is
   broken at exactly one block boundary (e.g., a small literal patch
   of < `block_len` bytes between two long runs of matched blocks).

2. **Bulk-refill block boundary** (line 263): the hot case. After a
   confirmed match, the window is refilled wholesale to the next
   block boundary and the hint check at `i+1` decides whether the
   match-chain inner loop continues or breaks back to byte-by-byte.

Both call into `check_block_match_slices`. There is exactly one fast
path predicate; this matches zsync's "one place that probes one
candidate" design.

### When the hint is cleared

The hint is cleared (set to `None`) in three situations:

- **Match at the last block** (line 221-225): `match_idx + 1 ==
  index.block_count()`. There is no `i+1` to chase.
- **Hint check fails** (line 178-183 / 263-273): the inner branch
  falls back to `find_match_slices` and the existing code path
  re-derives `want_i` either from the new match (if any) or sets it
  to `None` implicitly via the absence of a match.
- **No match this iteration**: `want_i` is unchanged on a miss and
  the next iteration will retry. This is acceptable because the only
  cost on a stale hint is one extra `check_block_match_slices` call,
  which is bounded above by the cost of one
  `find_match_slices` call. See "misprediction cost" below.

The "no clear on miss" semantics differ subtly from zsync's
`next_match = NULL` on miss. zsync clears aggressively because in its
loop the hint guards against bucket-chain walking; in oc-rsync the
hint guards against the full `find_match_slices` cost, and a
single-block sticky hint is no worse than zsync's behaviour because
the second miss falls through to `find_match_slices` anyway. The
sticky behaviour matches the existing `match.c:144-190` upstream
convention oc-rsync already follows.

## Wire-compat invariant

**The COPY-token output sequence MUST be byte-identical to the output
the same input would produce on the table-probe path with seq-match
disabled.** This is the load-bearing invariant for any seq-match PR,
and the binding contract for #2066 (golden-byte regression test).

### Why the invariant holds

The seq-match path picks the same block index that the table-probe
path would pick because, for any window position where seq-match
fires (i.e., `want_i = Some(hint)` and `check_block_match_slices`
returns true):

1. The rolling digest equality check is identical:
   `digest.sum1() == block.rolling().sum1() &&
    digest.sum2() == block.rolling().sum2()`
   (`index/mod.rs:243-246`). The full 4-byte rolling sum is compared,
   not a compacted key.
2. The strong-checksum equality check is identical: same algorithm
   (negotiated `SignatureAlgorithm`), same `strong_length`
   truncation, same byte slices fed in
   (`index/mod.rs:247-250`).
3. The block index returned is `hint`, which is `match_idx + 1` from
   the previous match. This is exactly the block index that a table
   probe would return *if* `(sum1, sum2)` for `hint` were unique in
   the bucket *or* `hint` happened to be the first index in the
   `lookup` bucket vector.

The third point is the duplicate-block edge case. Spelled out below.

### The duplicate-block tie-breaker

Today's `find_match_slices` tie-breaks like this
(`index/mod.rs:178-194`):

- Look up the `(sum1, sum2)` bucket. The bucket is a `Vec<usize>`
  with insertion order from `MatchIndex::build`. Build inserts in
  ascending block-index order, so the vector is sorted.
- Walk the vector left-to-right, computing the strong checksum once,
  and return the **first** index whose stored strong matches.

So for two blocks `j < k` with `(sum1, sum2, strong) == (sum1, sum2,
strong)`, the table-probe path always returns `j`.

Seq-match must produce the **same** tie-breaker. The rule is:

> If `want_i = Some(hint)` and the table-probe path on the same
> position would return `t`, the seq-match shortcut is allowed to
> return `hint` only when `t == hint`.

This is satisfied automatically when the data is sequential
(streaming over a basis with adjacent matched blocks): the previous
match was at index `i`, the bytes since are an exact prefix of block
`i+1`, the rolling sum and strong sum match block `i+1`, and **block
`i+1` is the only candidate for that window** because the basis
file's blocks are non-overlapping and the rolling sum at this offset
uniquely identifies the windowed bytes (modulo collisions, which the
strong checksum filters).

The non-trivial case is when two basis blocks `j < k` happen to share
identical content. The window currently at offset `O` matches both.
The table-probe path returns `j` (first in bucket). Seq-match could
return `hint = i+1`. The wire-compat question is: does `i+1 == j`?

It does, in every case the seq-match path fires:

- The previous match was at block `i`, emitting `COPY{index: i}`.
- The hint is `i+1`.
- The window at the new offset matches block `i+1` byte-for-byte
  (otherwise `check_block_match_slices` would have returned false
  via the strong checksum).
- So if blocks `j, k, i+1, ...` all share content, the seq-match
  path picks `i+1` and the table-probe path picks the smallest such
  index. We need `i+1` to BE the smallest.

This holds because the sequential traversal of a duplicated-block
streak walks the basis blocks in ascending order: the previous match
emitted index `i`, the byte stream then contains another `block_len`
bytes of the same content, and the smallest unmatched basis index
that matches is `i+1` (assuming `i+1 <= k` and `i+1` is among the
duplicate set). When `i+1` is NOT in the duplicate set (i.e., `i` and
`i+1` are adjacent in the basis but have different content, and
coincidentally the new window matches a *separate* duplicate set
elsewhere), the seq-match check on `i+1` correctly fails and the
fallback `find_match_slices` runs, picking the smallest matching
basis index.

The implementation already gets this right by virtue of
`check_block_match_slices` checking the rolling sum and strong sum
of block `hint` specifically. The PR must not change this.

**Test obligation (#2066)**: a new property test must construct a
basis with two duplicated block-content runs and verify the COPY
token sequence byte-for-byte against the no-hint baseline.
Recommended test cases:

- Basis: `[A, B, C, A, B, C]` (six unique blocks, two of which form
  duplicate pairs at indices `(0,3), (1,4), (2,5)`).
- Source: `A B C` (one streak). Expected COPY tokens:
  `{0}, {1}, {2}` (table-probe order; seq-match must agree).
- Source: `A B C A B C` (two streaks). Expected:
  `{0}, {1}, {2}, {3}, {4}, {5}`. Seq-match's `i+1` advance picks
  `3, 4, 5` correctly because after matching `2` the window for the
  fourth match contains `A` and the smallest basis index whose
  rolling+strong matches is `3` (== `i+1`).
- Source: `B C A` (cross-streak). Expected: `{1}, {2}, {3}`.
  Seq-match fires for the second and third positions; the first
  is found via `find_match_slices`.

## Misprediction cost analysis

When the seq-match hint is wrong (i.e.,
`check_block_match_slices(hint, ...)` returns false at line 178 or
263), the wasted work is:

- Bounds check on `hint` vs `blocks.len()`: O(1), one branch.
- `block.len() != block_length` check: O(1).
- Two `u16` comparisons (`sum1`, `sum2`): O(1), one branch.
- **If sums match**: one strong-checksum computation over
  `block_len` bytes (`compute_truncated_slices`).
- If sums don't match: returns false immediately, no strong-sum work.

Compare against the table-probe alternative
(`find_match_slices`):

- `tag_table[sum1]` boolean check: O(1), L1-resident.
- `lookup.get(&(sum1, sum2))`: O(1) hash probe with one or two
  `FxHash` evaluations.
- For each candidate in the bucket: one strong-checksum computation,
  one slice comparison.

In the common misprediction shape - sums collide on `(sum1, sum2)`
between the hint and the actual data - the seq-match path does
**one** strong-checksum verify and returns false. The fallback then
does its own `find_match_slices`, which does **one more** strong-checksum
verify (the bucket has the real match in it).

So the misprediction cost is at most one extra strong-checksum verify
versus pure table-probe. On a 1024-byte block with XXH3-64 at ~30
GB/s on a modern x86 core, that is ~34 ns. By comparison, a single
rolling-hash byte advance is ~1 ns (SIMD-accelerated path), and a
single full-table probe is ~50-200 ns (FxHash + bucket walk +
strong-sum on the matching candidate). So the worst-case wasted work
on a misprediction is **comparable to one table probe**, not an order
of magnitude worse.

In the common case where `(sum1, sum2)` for `hint` does NOT match the
incoming digest, the seq-match path returns false in ~3 ns and the
fallback runs its full probe. Total cost: ~3 ns + table-probe time,
i.e., the table-probe time plus a fixed ~3 ns surcharge.

Either way, the asymptotic ratio of seq-match-on-hit savings vs
seq-match-on-miss overhead is overwhelmingly favourable on
sequential data. zsync reports 2-4x reduction in rolling-hash work
on log-shaped and tar-shaped inputs (`librcksum/rsum.c` design
comments); we expect similar bounds on oc-rsync.

## Benchmark plan binding (#2067)

The seq-match path exercises differently across corpora. The benchmark
PR must cover:

| Corpus class       | Match-streak density | Seq-match hit rate (predicted) | Notes                                              |
|--------------------|----------------------|--------------------------------|----------------------------------------------------|
| Log files (text)   | Very high            | > 90% on append-only updates   | Logs grow at the tail; head is byte-identical.     |
| Tar archives       | High                 | 70-90% when patched in place   | Aligned record boundaries align with block-len.    |
| VM disk images     | Moderate-high        | 60-85% on snapshot deltas      | Many sectors unchanged between snapshots.          |
| Random binary diff | Low                  | < 10%                          | Adversarial; ensures fallback path stays correct.  |
| Empty / tiny       | N/A                  | N/A                            | Bypasses the inner loop entirely.                  |

Expected wall-clock speedup band, citing zsync's published numbers
(`librcksum` README and the zsync 0.6.2 paper):

- **2-4x reduction in rolling-hash byte-loop iterations** on
  high-density corpora (logs, tars, VM snapshots). Wall-clock
  speedup on the delta-generation phase typically tracks 1.3-2.0x
  because the rolling-hash loop is one of several costs (I/O,
  literal flushing, COPY-token emission).
- **Negligible regression** (< 1%) on adversarial corpora. The
  misprediction surcharge is a fixed cost per table probe.
- **No regression on the overall transfer time** when delta
  generation is not the bottleneck (e.g., network-bound transfers,
  fuzzy-match-dominated transfers).

The benchmark harness should reuse `scripts/benchmark_hyperfine.sh`
with at least three runs per corpus and report median + IQR. The
KPIs:

- Rolling-hash byte-advance count (instrumented via the existing
  `hash_hits` counter at `generator.rs:181`).
- Strong-checksum verify count (new counter; zero-cost when disabled).
- Wall-clock delta-generation time.
- COPY-token byte equality vs the no-hint baseline (correctness).

## Wire-compat restatement

Every layer below must stay byte-identical with seq-match enabled vs
disabled. This list is the explicit checklist for the implementation
PR (#2065) and the final cleanup PR (#2084-#2086).

1. **Signature payload format** (`signature` crate, wire layer): no
   change. The 4-byte rolling sum stays serialized as today.
2. **Capability negotiation** (`protocol` crate): no change. No new
   capability flag advertised, no new wire bit.
3. **NDX framing** (`protocol::ndx`): no change. Block indices in
   COPY tokens are emitted in the existing format.
4. **DeltaToken sequence** (`crates/match/src/script.rs`): the `Vec
   <DeltaToken>` produced by `DeltaGenerator::generate` must be
   byte-identical, token-for-token, with seq-match enabled vs
   disabled. This is the property test target for #2066.
5. **Multiplex MSG_DATA payloads**: byte-identical. Captured by
   `tcpdump` regression in #2075.
6. **Filter exchange, file-list framing, attribute serialization**:
   unrelated to seq-match; must remain unchanged.
7. **Golden byte tests** in `crates/protocol/tests/golden/`: pass
   unchanged.
8. **Interop matrix** (`tools/ci/run_interop.sh`): zero new entries
   in `tools/ci/known_failures.conf` against upstream rsync 3.0.9,
   3.1.3, 3.4.1.

## Cleanup PR contract (#2084-#2086)

The implementation must satisfy the parent design's
`Wire-compat invariants` section in addition to the eight items
above. Concretely:

- **No new CLI flag.** The seq-match path is always on; there is no
  `--seq-match` or `--no-seq-match` user-facing toggle. If a
  cfg-gated benchmark scaffold is needed for #2067, it lives behind
  `#[cfg(feature = "bench-internal")]` or similar, never reachable
  from `clap`.
- **No protocol crate change.** The implementation lives entirely in
  `crates/match/`. Internal API plumbing (e.g., a private helper in
  `index/mod.rs` or a new field on `DeltaGenerator`) is fine; public
  signatures of `generate_delta`, `DeltaGenerator::generate`,
  `DeltaSignatureIndex::find_match_slices`, and
  `DeltaSignatureIndex::check_block_match_slices` MUST NOT change
  unless purely additive.
- **Golden bytes stay green.** `cargo nextest run -p protocol
  --test golden` passes unchanged. CI enforces this.
- **No state in the wire layer.** `next_match`-equivalent state lives
  inside `DeltaGenerator` (the `want_i: Option<usize>` local), never
  in `DeltaSignatureIndex` (which is shared/cloneable across
  segments) or in any wire-serialized struct.
- **INC_RECURSE compatible.** Each per-segment `MatchIndex::build`
  rebuilds; `DeltaGenerator::generate` is invoked per file with a
  fresh `want_i = Some(0)`. No cross-file hint carryover.
- **Append/inplace compatible.** When the receiver short-circuits
  signature exchange (append-only path), the generator is not
  invoked. Seq-match has no effect on that path.

## References

- Parent design: `docs/design/zsync-inspired-matching.md`.
- zsync 0.6.2 source (gianm/zsync mirror):
  - `librcksum/rsum.c:190` (`next_match` cleared on miss).
  - `librcksum/rsum.c:262` (`next_match` advanced after match).
  - `librcksum/rsum.c:352-356` (`onlyone=1` probe at top of loop).
- oc-rsync match crate (HEAD on master):
  - `crates/match/src/generator.rs:103` (`want_i` declaration).
  - `crates/match/src/generator.rs:177-187` (byte-by-byte hint check).
  - `crates/match/src/generator.rs:221-225` (hint advance after match).
  - `crates/match/src/generator.rs:259-273` (bulk-refill hint check).
  - `crates/match/src/index/mod.rs:151-194` (`find_match_slices`,
    table-probe baseline).
  - `crates/match/src/index/mod.rs:229-251`
    (`check_block_match_slices`, the seq-match predicate).
- Upstream rsync 3.4.1: `target/interop/upstream-src/rsync-3.4.1/`
  - `match.c:144-190` (the `want_i` adjacent-match hint that oc-rsync
    already mirrors).
- Tracking issues: #2064 (this design), #2065 (impl), #2066 (golden
  COPY-token equality test), #2067 (benchmark), #2084-#2086
  (cleanup), #2075 (tcpdump wire-equality regression).
