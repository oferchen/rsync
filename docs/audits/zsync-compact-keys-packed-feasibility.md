# Zsync compact-keys packed-layout feasibility (#2072)

**Verdict: Defer. The current `CompactLookup` layout already realizes the
zsync-style compact-keys optimization. A further packing into a 4-byte slot
is not feasible without restricting block counts below realistic workload
sizes.**

This audit closes the prototype scope of #2072 with a no-op decision.
Companion documents:

- [`docs/design/zsync-hash-layout-decision.md`](../design/zsync-hash-layout-decision.md) -
  ADR enumerating Options A (FxHashMap), B (sorted Vec), C (open-addressed).
- [`docs/design/zsync-inspired-matching.md`](../design/zsync-inspired-matching.md) -
  parent design note describing the `rsum_a_mask` compact-key idea from
  zsync 0.6.2 (`librcksum/state.c:48`).
- [`crates/matching/benches/compact_keys_cache.rs`](../../crates/matching/benches/compact_keys_cache.rs) -
  existing `bench-internal` harness (#2073) that already produces the
  cache-behaviour evidence the ADR's adopt-or-revert thresholds reference.

## Current layout (post-Option-C migration)

The hot probe path lives in
[`crates/matching/src/index/compact_lookup.rs`](../../crates/matching/src/index/compact_lookup.rs).
The lookup container is now a flat open-addressed hash table with Robin
Hood linear probing:

```rust
pub(super) struct CompactLookup {
    slots: Vec<u64>,
    mask: u32,
    len: u32,
}
```

Each `u64` slot packs:

- **High 32 bits**: `(sum2 << 16) | sum1`, the full rolling-checksum key.
- **Low 32 bits**: basis block index (`u32`).

The empty sentinel is `u32::MAX` in the high half. Load factor is capped at
25% (`with_capacity` rounds up to `4 * n_entries` next power of two), so the
linear probe walk almost never crosses a single 64-byte cache line in
isolation. Eight slots fit per cache line.

The probe sequence inside
[`crates/matching/src/index/mod.rs::find_match_bytes_filtered`](../../crates/matching/src/index/mod.rs)
is:

1. `tag_table[sum1]` - one byte, table sits at 64 KiB in L1.
2. `bithash.contains(rsum)` - one byte per indexed block, sized ~`8 * n`
   bits with `BITHASHBITS = 3` (mirrors `librcksum/internal.h:83`).
3. `lookup.find_all(sum1, sum2)` - the Robin Hood walk above.
4. Strong-checksum verify on the candidate block.

Steps 1 and 2 are the upstream `tag_table` and zsync's bithash prefilter.
They reject the overwhelming majority of probe positions before the
`CompactLookup` walk runs at all.

## What #2072 originally proposed

The parent design note describes zsync's `rsum_a_mask` technique
(`librcksum/state.c:48`):

```
mask = rsum_bytes < 3 ? 0
     : rsum_bytes == 3 ? 0xff
     : 0xffff;
```

Translated to oc-rsync's terms, the densest realistic variant is a 4-byte
slot:

```text
[ rsum_low: u16 | block_idx: u16 ]
```

This would put 16 slots per 64-byte cache line, halving the lookup-table
footprint and (in principle) doubling the per-line probe throughput on
cache-resident workloads.

## Why the 4-byte variant is not feasible

The block index does not fit in 16 bits for realistic transfers.
`signature::block_size::calculate_checksum_count` returns a `u64`, and the
canonical block count is `file_size / block_length` rounded up:

| File size | Block length | Block count |
|-----------|--------------|-------------|
| 16 MiB | 256 B (`DEFAULT_BLOCK_SIZE` lower bound) | 65 536 |
| 1 GiB | ~32 KiB (sqrt heuristic, `generator.c:sum_sizes_sqroot()`) | ~32 K |
| 100 GiB | 128 KiB (`MAX_BLOCK_SIZE_V30`) | ~800 K |
| 1 TiB | 128 KiB | ~8 M |

A 16-bit `block_idx` field saturates at 65 535. Every 100 GiB transfer
already exceeds that by an order of magnitude, and the protocol-allowed
ceiling (`u32::MAX` blocks via the `u64` checksum count) is six orders of
magnitude above the proposed field width. Truncating the field would
require either:

1. **Rejecting large transfers at index time** - a regression against
   upstream rsync, which uses native pointer-width indices throughout
   `match.c`.
2. **Multi-level indirection** (e.g., 16-bit slot pointing into a fallback
   `Vec<u32>` for blocks past 65 535) - reintroduces the pointer chase
   that the Option-C migration explicitly eliminated.
3. **Bit-stealing splits** like 20/12 or 24/8 - same problem, smaller
   numbers; only postpones the cliff to ~1 GiB or ~256 MiB worst case.

None of these clear the wire-compat-neutral bar set in the ADR
(`zsync-hash-layout-decision.md` section "Wire-compat invariant"): they
either change observable behaviour or reintroduce the indirection the
optimization was supposed to eliminate.

## Why even a hypothetical 4-byte slot would not help much

Even if every transfer fit a 16-bit `block_idx`, the cache-behaviour
ceiling on the lookup table itself is already low:

1. **Prefilter gating dominates the probe rate.** Per
   `docs/design/zsync-bithash.md` section 7, the bithash rejection rate
   is ~87.5% (`BITHASHBITS = 3`). The tag-table rejects another large
   slice. The `CompactLookup` walk is reached for a small minority of
   target positions; halving its footprint affects only that minority.
2. **8 slots per line is already prefetcher-friendly.** Linear probing
   on a sub-25%-load table keeps the expected probe chain inside one
   cache line; the second line is touched in a vanishing minority of
   probes. The bench harness comment block in
   `crates/matching/benches/compact_keys_cache.rs:67-90` records the
   adopt-or-revert threshold: scattered-probe cache-miss rate at the
   `llc` or `ram` size must be `>= 2x` the sequential rate to justify a
   denser layout. With prefilter gating in front, that ratio is
   structurally bounded below.
3. **Robin Hood termination is one branch.** The `find_all` iterator
   terminates on either an empty slot or a probe-distance violation.
   Halving the slot width does not change the branch count.
4. **The strong-checksum verify dominates the hit path.** Every confirmed
   candidate runs MD4/MD5/XXH3 over the block, which is orders of
   magnitude more expensive than the lookup walk itself. The lookup is
   not the bottleneck on hit-heavy workloads either.

## What the existing #2073 bench proves

The `compact_keys_cache.rs` harness already exercises the current 8-byte
layout across L1 / L2 / LLC / RAM tiers using the `bench-internal`
accessors `lookup_capacity`, `lookup_bytes`, and `lookup_probe`. The
adopt-or-revert thresholds documented in the bench's leading comment
block hold for both the current 8-byte layout and any hypothetical
denser variant:

> **Favourable** (justify packing): scattered-probe cache-miss rate at
> the `llc` or `ram` size is `>= 2x` the sequential rate, and total
> probes-per-second on the scattered case is bottlenecked by memory
> bandwidth.
>
> **Unfavourable** (defer #2072): scattered and sequential cache-miss
> rates stay within ~20% across all sizes, or the `ram` case is
> bottlenecked by something other than memory traffic.

The current `CompactLookup` is the densest layout that supports the full
`u32` block-index space; the bench is the gate for any future denser
variant if `u32` block indices ever stop being a workload constraint.

## Recommendation

Mark #2072 closed with the following resolution:

1. **No code change.** The `CompactLookup` struct in
   `crates/matching/src/index/compact_lookup.rs` already realizes the
   Option-C layout from
   `docs/design/zsync-hash-layout-decision.md`, with the densest slot
   width that the workload supports.
2. **No bench change.** The `compact_keys_cache.rs` harness already
   measures the current layout's cache behaviour and the adopt-or-revert
   thresholds it documents apply unchanged.
3. **Revisit triggers** (unchanged from the ADR):
   - Block counts on real-world transfers routinely exceed `u32::MAX`
     (would force a wider, not narrower, field).
   - A future SIMD / prefetch optimization on the rolling-hash side
     shifts the bottleneck onto the lookup probe.
   - The bithash and tag-table prefilters are deprecated or reworked in
     a way that exposes the lookup walk to the full probe stream.

None of those triggers are active on the current roadmap, so the
prototype scope of #2072 closes here.

## Wire-compat invariant

This audit changes no code. The on-wire bytes, the `SignatureBlock`
payload, the `tag_table` semantics, the `BitHash` semantics, and the
`CompactLookup` semantics are all unchanged. Golden-byte tests in
`crates/protocol/tests/golden/` remain green by construction.

## References

- ADR: [`docs/design/zsync-hash-layout-decision.md`](../design/zsync-hash-layout-decision.md)
- Parent design note: [`docs/design/zsync-inspired-matching.md`](../design/zsync-inspired-matching.md)
- Current implementation:
  - [`crates/matching/src/index/compact_lookup.rs`](../../crates/matching/src/index/compact_lookup.rs)
  - [`crates/matching/src/index/mod.rs`](../../crates/matching/src/index/mod.rs)
  - [`crates/matching/src/index/builder.rs`](../../crates/matching/src/index/builder.rs)
  - [`crates/matching/src/index/bithash.rs`](../../crates/matching/src/index/bithash.rs)
- Existing cache bench: [`crates/matching/benches/compact_keys_cache.rs`](../../crates/matching/benches/compact_keys_cache.rs)
- Block sizing: [`crates/signature/src/block_size.rs`](../../crates/signature/src/block_size.rs)
- zsync 0.6.2 source (gianm/zsync mirror):
  - `librcksum/state.c:48` - `rsum_a_mask` derivation
  - `librcksum/hash.c:62-84` - hash-table sizing
  - `librcksum/internal.h:83` - `BITHASHBITS` density
- Tracking issues: #2054 (parent ADR), #2072 (this audit), #2073 (cache
  bench), #2082 (large-file bench).
