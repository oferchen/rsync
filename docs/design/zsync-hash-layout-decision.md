# ADR: zsync hash-layout - keep or drop compact keys

Status: **Deferred pending #2073 cachegrind data.**

This decision record covers the fourth zsync-inspired insertion point from
`docs/design/zsync-inspired-matching.md` - the in-memory layout of the
rolling-checksum lookup map in `crates/match/src/index/mod.rs`. The first
three techniques (bithash #2059, seq-match #2064, prune #2068) land first
and on their own merits; this ADR scopes the call on whether to follow
them with a fourth PR that swaps the lookup container itself.

## Context

The hot path in delta matching is, today, structured as:

```
crates/match/src/index/mod.rs:38   DeltaSignatureIndex { ... lookup: FxHashMap<(u16, u16), Vec<usize>>, ... }
crates/match/src/index/mod.rs:85   tag_table[digest.sum1() as usize]   // O(1) reject
crates/match/src/index/mod.rs:89   let key = (digest.sum1(), digest.sum2());
crates/match/src/index/mod.rs:90   let candidates = self.lookup.get(&key)?;
crates/match/src/index/mod.rs:99   self.find_match_sequential(candidates, window)
```

The `lookup` field is an FxHashMap. Per the parent design note, at 1000
blocks this footprint is roughly 200 KB:

- 60-100 KB of `SignatureBlock` entries on the `blocks` vector
- map overhead (FxHashMap buckets + per-key `Vec<usize>` headers + heap
  allocations for the candidate vectors)
- the constant 64 KB `tag_table` (one byte per `sum1` value, see
  `index/mod.rs:28`)

The `(u16, u16)` key is composed of the low 16 bits of `sum1` and the low
16 bits of `sum2`. The full 32-bit rolling sum is never used as the
in-memory key - upstream rsync truncates the same way (`match.c` tag
build). Strong-checksum verification (`find_match_sequential`,
`index/mod.rs:114-124`) is the authoritative gate on every probe; the
weak key only filters candidates.

zsync 0.6.2 (`librcksum/hash.c`) takes a different approach: the rsum
hash table is a packed open-addressed structure, and for small targets
the key itself is shrunk via `rsum_a_mask` (`librcksum/state.c:48`).
zsync's per-entry footprint is therefore ~6 bytes, not the ~24+ bytes
of an FxHashMap bucket plus heap-allocated `Vec<usize>`.

The cache-line-per-entry density differs by an order of magnitude.
Whether that translates to measurable end-to-end speedup on oc-rsync
workloads is the question this ADR scopes.

## Wire-compat invariant

This is a purely in-memory layout decision. It has **zero wire impact**
under any of the options below.

1. The serialized rolling-sum width is fixed at 4 bytes
   (`crates/protocol/src/...` golden tests in
   `crates/protocol/tests/golden/`). None of the options touches that.
2. The `SignatureBlock` payload (rolling sum + truncated strong checksum)
   is unchanged. The `blocks: Vec<SignatureBlock>` field stays
   byte-identical regardless of the lookup container.
3. The strong-checksum algorithm is determined by capability negotiation
   and is never short-circuited by the in-memory layout.
4. INC_RECURSE per-segment rebuilds (`crates/match/src/index/builder.rs`)
   work with whichever container is chosen; the rebuild cost is not on
   the wire.

Restated: every option preserves byte-identical output against upstream
rsync 3.0.9 / 3.1.3 / 3.4.1 in both directions, and golden-byte tests in
`crates/protocol/tests/golden/` stay green.

## Options considered

### Option A - Keep `FxHashMap<(u16, u16), Vec<usize>>`

- Code today, see `crates/match/src/index/mod.rs:44`.
- Footprint: ~200 KB at 1000 blocks (60-100 KB block payload + map
  overhead). FxHashMap is already the 2-5x faster variant per the
  module's own rustdoc (`index/mod.rs:7`).
- Per-hit cost: one hash, one bucket walk, one `Vec<usize>` indirection
  to a heap-allocated candidate list, then strong-checksum verify.
- Per-miss cost: one hash, one bucket walk to "not found." With the
  bithash gate from #2059 sitting in front, almost all misses never
  reach the FxHashMap probe at all.
- Pros: simple, well-tested (`crates/match/src/index/tests.rs` has 12
  correctness tests including duplicate-block and collision coverage),
  no new code, no migration risk.
- Cons: pointer chasing on every bucket walk; cache lines for buckets
  and for the `Vec<usize>` candidate list are typically not adjacent;
  heap fragmentation at high block counts.

### Option B - `Vec<(u32, u32)>` sorted by `rsum_low`, binary search

- Replace the FxHashMap with a single contiguous vector of
  `(packed_key, block_idx)` pairs. Build is one-shot - sort once at
  signature load. Probe is `partition_point` (binary search) plus a
  short forward scan to walk all slots with the same packed key.
- Footprint at N blocks: 8 bytes per entry plus duplicates. For 1000
  blocks that is ~8 KB; for 1 M blocks, ~8 MB. No per-key heap
  allocation, no bucket overhead, no `Vec<usize>` headers.
- Per-hit cost: `log2(N)` cache-friendly binary-search probes (each
  hitting a small number of cache lines) + a short linear walk through
  duplicates + strong-checksum verify.
- Per-miss cost: `log2(N)` cache-friendly probes; with bithash in
  front, this path is rare.
- Pros: dense, cache-friendly, zero map overhead; trivial to clone for
  INC_RECURSE per-segment rebuilds; deterministic memory profile.
- Cons: build cost is `O(N log N)` rather than `O(N)`; `log2(N)` probes
  cost more than one hash on average for `N` past ~16 K; insertion is
  `O(N)` (irrelevant for our load-once pattern).

### Option C - zsync-style open-addressed table

- Power-of-two-sized array of `(rsum_low, block_idx)` slots. Linear
  probing, sentinel-empty slot. Sized as `4 * N` rounded up to a power
  of two, mirroring zsync's `i` selection (`hash.c:62-84`). Per-entry
  ~6-8 bytes.
- Footprint at N blocks: ~6N bytes plus 4x oversize for load factor =
  ~24-32 bytes per occupied slot net of empty slots, but contiguous and
  prefetcher-friendly.
- Per-hit cost: one hash, expected ~1.0-1.5 cache-line touches under
  load factor 0.25, then strong-checksum verify.
- Per-miss cost: one hash, ~1 cache-line touch, then bucket-empty
  sentinel terminates the probe.
- Pros: matches zsync's documented behaviour; tightest cache profile
  of the three options on miss-heavy workloads (which dominate
  delta-rich transfers).
- Cons: a custom container; needs its own correctness tests for
  duplicate-block and collision behaviour to match the existing
  FxHashMap suite (`index/tests.rs:find_match_bytes_uses_strong_checksum_for_collision`);
  more code to maintain than Option B.

## Decision criteria

The call is benchmark-driven. The compact-keys prototype lands in
#2072, with cachegrind / `perf stat` measurements published in #2073.
The numbers from #2073 decide between the three options here:

- **Adopt Option B or Option C** if #2073 shows >15% L1-data miss
  reduction or >10% wall-clock speedup on the 100 MB-modified dataset
  used in #2082, holding bithash (#2063) and seq-match (#2067)
  benchmark fixtures constant.
- **Keep Option A** if #2073 shows <5% L1-data miss reduction AND
  <3% wall-clock change. The complexity of B or C is not justified
  for marginal gains, and Option A is what the test surface already
  validates.
- **Re-prototype** if #2073 lands in the 5-15% gain band. That signals
  the win is workload-dependent; #2082 large-file benchmarks should be
  inspected before committing to either container.

The parent design note (`docs/design/zsync-inspired-matching.md`)
already records the revert-on-low-gain discipline: "compact-keys
prototype (#2072) -> bench (#2073), revert if cache-miss gain < 5%."
This ADR aligns with that line.

## Cache-behaviour reasoning

Modern x86_64 and aarch64 cores typically expose:

- L1d: 32-64 KB, ~4 cycle hit latency, 64-byte line.
- L2: 256 KB - 8 MB, ~10-15 cycle hit latency.
- L3: 4-64 MB, ~30-50 cycle hit latency.

At small block counts (1 K - 100 K blocks):

- Option A's lookup at ~200 KB - 20 MB straddles L2 and L3. Bucket walk
  cost is dominated by pointer-chase latency to the heap-allocated
  `Vec<usize>` candidate list. Tag-table prefilter (1 KB) stays in L1.
- Option B's lookup at ~8 KB - 800 KB fits L1 to mid-L2. Binary search
  on a contiguous vector touches `log2(N)` cache lines, all prefetcher-
  friendly. Forward scan over duplicate-key slots is a single-line
  walk.
- Option C's lookup at ~32 KB - 3 MB fits L1 (small N) to L2 (medium
  N). Linear-probe walk touches ~1-2 cache lines per probe.

At large block counts (1 M blocks):

- Option A: ~12 MB lookup. L3-resident on most servers; bucket walk
  hits L3 latency every probe.
- Option B: ~8 MB lookup. Mid-L3-resident; binary search has ~20
  comparisons but the early ones reuse the same hot cache lines, so
  the effective miss rate is closer to `log2(N) - log2(line_density)`.
- Option C: ~6 MB lookup. Tightest profile; single-line probe hits
  L2 or L3 once, no chain walk.

Which option fits best depends on **whether the bucket-walk cost or
the single-probe cost dominates**. Two factors interact:

1. **Bithash gating (#2059).** With bithash in front, the FxHashMap
   probe in Option A is reached only on bithash hits. Bithash density
   is `1/8` (`internal.h:83`, `BITHASHBITS = 3`), so 87.5% of misses
   are rejected upstream of the lookup. This MUTES the cache-density
   advantage of Options B/C on miss-heavy paths - the lookup is rarely
   touched for misses anyway.

2. **Seq-match interaction (#2064).** After a confirmed match, the
   `next_match` hint probes `block_index + 1` directly (parent design
   note, "Sequential match heuristic" section). This is a single
   indexed access into the `blocks: Vec<SignatureBlock>`, independent
   of the `lookup` container. None of A/B/C affects seq-match.

The implication: compact keys' theoretical cache win is largest on
**hit-heavy** delta-matching workloads (high modification rate, many
short runs of unmatched-then-matched blocks), and smallest on
miss-heavy workloads where bithash already eliminates most probes.
This is exactly the regime where the prune optimization (#2068)
shrinks the candidate vectors over time, further reducing the bucket-
walk cost in Option A.

Net: at 1 K - 100 K blocks, Options B and C deliver a measurable but
modest cache improvement. At 1 M blocks, the gap widens, but cold-
cache effects on signature load also widen (Option A's heap-scattered
candidate vectors are slow to warm). The benchmark in #2073 is the
arbiter.

## Risk if dropped

The first three zsync optimizations are **independent and additive**
relative to compact keys:

- Bithash (#2059) inserts a new `BitHash` field on
  `DeltaSignatureIndex` (parent doc, insertion-points table, line
  165). It does not depend on the lookup container.
- Seq-match (#2064) hooks into `crates/match/src/generator.rs:177`
  via the existing `want_i` hint surface. It does not touch the
  lookup container.
- Prune (#2068) maintains a matched-block bitmap in
  `crates/match/src/generator.rs:214`. It interacts with the lookup
  container only in that pruned indices are skipped during the
  candidate walk; the container itself is unchanged.

If compact keys is dropped:

- The first three PRs land and deliver their measured speedups.
- The `lookup: FxHashMap<(u16, u16), Vec<usize>>` field stays on
  `DeltaSignatureIndex`. No code regresses.
- The decision can be revisited later when (a) larger-N benchmarks
  expose a new bottleneck, or (b) a future contributor profiles the
  matching pipeline on a workload not represented in #2081/#2082.

This decision is reversible. The parent design note's per-technique
PR plan already isolates the prototype to #2072 and the bench to
#2073 - reverting #2072 leaves the rest of the pipeline intact.

## Cleanup PR contract (#2084-#2086)

Per the parent design note's wire-compat invariants section:

1. No CLI flag. None of the options here introduces a `clap` argument.
   Internal toggles, if any (e.g. for benchmark scaffolding), stay
   behind cfg-gated `#[cfg(feature = "bench")]` modules and never reach
   `crates/cli/`.
2. No `crates/protocol` change. The protocol crate has no awareness of
   the in-memory lookup container; it serializes `SignatureBlock`
   payloads, which are byte-identical regardless of the container.
3. Golden bytes stay green. `crates/protocol/tests/golden/` is the
   canary; CI fails the PR if any byte changes. None of A/B/C affects
   the bytes.
4. Internal API plumbing only. The public surface of `match::index` is
   `DeltaSignatureIndex::find_match_bytes` and `find_match_slices`
   (`crates/match/src/index/mod.rs:78,151`). Whichever option lands
   keeps both signatures unchanged.
5. No internal toggle escapes into CLI. If a runtime-selectable
   container is desired during the bake-in window, the selector is
   compile-time only. Once the decision is final, the unused option is
   deleted, not gated behind a flag.
6. Tests in `crates/match/src/index/tests.rs` and
   `crates/match/tests/block_matching_accuracy.rs` and
   `crates/match/tests/integration_tests.rs` continue to pass. The
   correctness contract is unchanged. New tests added by #2072 cover
   the new container's duplicate-block and collision behaviour, but
   the existing 12 + ~70 + ~20 cases stay green without modification.

## Recommendation

**Defer pending #2073.** Land bithash, seq-match, and prune first; let
#2073 publish cachegrind / `perf stat` numbers on the 100 MB-modified
benchmark with all three of those optimizations active. Then revisit
this ADR with one of three concrete updates:

- **Status: Accepted (Option B)** if #2073 shows >15% L1-data miss
  reduction or >10% wall-clock speedup, AND Option B's binary search
  matches or beats Option C in the same benchmark.
- **Status: Accepted (Option C)** if Option C beats Option B by >5%
  wall-clock on #2082 large-file workloads, justifying the extra
  custom-container code.
- **Status: Rejected** if cache-miss gain is <5% AND wall-clock
  change is <3%. Option A's simplicity is the right default; the
  compact-keys prototype #2072 is reverted per the parent design
  note's revert clause.

Conditions for revisit outside the #2073 trigger:

- A future benchmark exposes a sustained >20% time fraction inside
  `lookup.get` on a representative workload.
- Block counts on real-world transfers routinely exceed 1 M, pushing
  the FxHashMap into L3 territory where the cache-density gap
  matters most.
- A new SIMD or prefetch optimization on the rolling-hash side shifts
  the bottleneck onto the lookup probe.

Until any of those triggers, the keep-Option-A default stands.
Compact keys is the lowest-priority of the four zsync techniques and
the only one where the benefit is uncertain a priori. The other three
are wins on paper and on zsync's own measurements; this one is not.

## References

- Parent design note: `docs/design/zsync-inspired-matching.md`
  - Insertion-points table (compact key row)
  - "Compact key (`rsum_a_mask`)" zsync source mapping
  - Per-technique PR plan with revert-if-low-gain clause
- oc-rsync source:
  - `crates/match/src/index/mod.rs:38` - `DeltaSignatureIndex` struct
  - `crates/match/src/index/mod.rs:44` - `lookup` field type
  - `crates/match/src/index/mod.rs:78,151` - public probe API
  - `crates/match/src/index/mod.rs:85,89-90` - tag-table + lookup probe
  - `crates/match/src/index/builder.rs` - INC_RECURSE per-segment build
  - `crates/match/src/index/tests.rs` - correctness tests
  - `crates/match/tests/block_matching_accuracy.rs`
  - `crates/match/tests/integration_tests.rs`
- zsync 0.6.2 source (gianm/zsync mirror):
  - `librcksum/hash.c:62-84` - hash-table sizing
  - `librcksum/hash.c:101-102` - bithash insertion (context for #2059)
  - `librcksum/state.c:48` - `rsum_a_mask` compact-key derivation
  - `librcksum/internal.h:83` - `BITHASHBITS` density constant
- Tracking issues: #2054 (this decision), #2072 (prototype),
  #2073 (cachegrind bench), #2082 (large-file bench), #2084-#2086
  (cleanup contract).
