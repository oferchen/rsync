# zsync bithash prefilter: shape and sizing

Detail design for the bithash prefilter introduced as the first of the four
zsync-inspired pipeline optimizations enumerated in
`docs/design/zsync-inspired-matching.md`. This note establishes the
data-structure shape, the sizing formula, the precise insertion points in
`crates/match/src/index/`, the bit-selection function over the rolling
sum, the property-test invariant the implementation MUST honour, the
wire-compat invariants this change MUST NOT touch, and the cfg-gated
benchmark scaffolding plan.

This is a design note. No Rust code lands in this PR. Implementation is
tracked in #2060 / #2061; property tests in #2062; benchmark scaffolding
in #2063.

## Background and goal

Today, every rolling-hash advance probes
`DeltaSignatureIndex::find_match_slices()` with `(sum1, sum2)` against an
`FxHashMap<(u16, u16), Vec<usize>>`. The existing `tag_table` (1 KB,
indexed by `sum1`) rejects on the low 16 bits in O(1). For a basis of
`N` blocks distributed across the full 16-bit `sum1` space, the
`tag_table` saturates quickly: at `N >= ~10 K`, almost every `sum1`
value is `true`, and the `tag_table` rejects almost nothing. Every
non-matching position then pays the `FxHashMap::get` cost (cache-line
fetch, hash computation, bucket walk).

zsync's `librcksum` solves the same problem with an 8x-larger bit
array, the **bithash**. With a density of 1/8 random rsums set, the
bithash rejects ~7/8 of misses in O(1) before any hash-map probe. This
note translates that mechanism into oc-rsync's existing index without
touching the wire.

## 1. Bithash bit-array shape

### Allocation and mask

zsync sizes the bithash as a power-of-two bit array 8x the main rsum
hash table. From `librcksum/internal.h:83` of zsync 0.6.2:

    #define BITHASHBITS 3

and `hash.c:62-84` builds the table by selecting `i` such that the rsum
hash table holds `2^i` buckets approximating `4 * N`. The bithash mask
follows:

    bithashmask = (2 << (i + BITHASHBITS)) - 1

Concretely, if the rsum hash holds `2^i` buckets, the bithash holds
`2^(i + BITHASHBITS + 1) = 2^(i + 4)` bits, i.e. `2^(i+1)` bytes. That
is 16x the bucket count in bits, or **2 bytes of bithash per rsum
bucket**, or roughly **1 byte per signature block** at the canonical
`4*N` bucket sizing.

### Insertion (zsync `hash.c:101-102`)

    bithash[(h & bithashmask) >> 3] |= 1 << (h & 7);

`h` is the 32-bit rsum value. The low `log2(bithash_bits)` bits address
into the array; bottom 3 bits select the bit within the byte; the next
`log2(bithash_bits) - 3` bits select the byte.

### Probe (zsync `rsum.c:362-366`)

    if ((bithash[(h & z->bithashmask) >> 3] & (1 << (h & 7))) == 0) {
        return 0;  /* skip the bucket-chain walk */
    }

One-sided filter: false positives cost one wasted hash-table lookup;
false negatives are impossible by construction (every inserted block
sets exactly its bit, never clears it).

### Memory cost in oc-rsync

For our typical signature sizes (parent doc, "Sizing the bithash"
table):

| `N` (blocks) | Buckets `2^k` | Bithash bytes | Density set |
|--------------|---------------|---------------|-------------|
| 1 K          | 2^10          | ~1 KB         | <= 1/8      |
| 100 K        | 2^17          | ~256 KB       | <= 1/8      |
| 10 M         | 2^23          | ~16 MB        | <= 1/8      |

Rule of thumb: `bithash_bytes = 2^(i+1)` with `2^i >= 4*N`, so the
bithash needs `~ N` bytes. For very small basis files (`N < 1024`) we
clamp at a 1 KB minimum to keep the bit-selection mask stable.

### Cache behavior

At `N <= 4 K`, the bithash fits in 1 KB. At `N <= 64 K`, it fits in
~16 KB, comfortably inside L1d on x86_64 (32-48 KB typical) and
aarch64 (32-128 KB typical).

At `N = 10 M` (~16 MB), the bithash exceeds L2 on most cores. The
access pattern is **one byte per rolling-hash advance**, with addresses
driven by the rsum. Because the rsum changes by a small delta on each
`roll`, consecutive probes hit nearby cache lines for spatially local
input. Profiling under #2063 will quantify L2/L3 miss rate at extreme
sizes; pathological cases are capped at the parent doc's 16 MB
adversarial-input ceiling.

## 2. Sizing formula derivation

### zsync's 1/8 density bound

At insertion time, zsync sets one bit per block. With a uniformly
distributed rsum, the expected fraction of bits set after inserting
`N` blocks into a `B`-bit array is approximately
`1 - exp(-N/B) ~= N/B` for `N << B`. Choosing `B = 8 * N`:

- expected set fraction: `N / (8N) = 1/8`
- expected reject rate on random misses: `1 - 1/8 = 7/8 ~= 87.5%`

This is the canonical "rejecting ~7/8 of random rsums" claim in the
parent doc and in zsync's own commentary. Confirming the math: after
`N` insertions in `8N` bits, set bits are `<= N` (each insertion sets
at most one bit, possibly colliding), so the set fraction is bounded
by `1/8` with equality only when all inserted bits are distinct.

### Why BITHASHBITS=3 in oc-rsync

zsync chose `BITHASHBITS = 3` (giving the 1/8 density target) by
balancing memory against rejection rate. The function is non-linear:
doubling memory beyond 1/8 only buys you a fraction of the remaining
12.5% miss probability.

| BITHASHBITS | Density set | Reject rate | Memory factor |
|-------------|-------------|-------------|---------------|
| 0           | ~50%        | ~50%        | 1x            |
| 1           | ~25%        | ~75%        | 2x            |
| 2           | ~16.7%      | ~83.3%      | 4x            |
| **3**       | **~12.5%**  | **~87.5%**  | **8x**        |
| 4           | ~6.25%      | ~93.75%     | 16x           |
| 5           | ~3.125%     | ~96.875%    | 32x           |

oc-rsync starts at `BITHASHBITS = 3` for two reasons:

1. **Start with the zsync default.** It is the only setting empirically
   validated against real-world rsync workloads at zsync's scale, so
   it eliminates one degree of freedom from the bench-up exercise in
   #2063.
2. **Bench-driven upgrade in #2063.** If profiling shows the post-tag
   miss rate stays above ~12.5% (i.e. the bithash is the bottleneck
   rather than the strong-checksum verification), we will sweep
   `BITHASHBITS` in `{3, 4, 5}` in #2063's bench scaffold and lock the
   chosen value in #2084.

The selected value lives as `pub(super) const BITHASH_BITS: u32 = 3;`
in the eventual implementation (#2060). It is **not** a CLI flag and
**not** a runtime knob; see #2084.

## 3. Insertion-point binding

The parent doc's insertion table names two binding points:

| File                                  | Line | Role                                                              |
|---------------------------------------|------|-------------------------------------------------------------------|
| `crates/match/src/index/mod.rs`       | 165  | Probe call site - `BitHash::probably_present(rsum)` gate          |
| `crates/match/src/index/mod.rs`       | 44   | Field declaration on `DeltaSignatureIndex`                        |
| `crates/match/src/index/builder.rs`   | ~30  | Build call site - `bithash.insert(rsum)` per indexed block        |

The parent doc cites `mod.rs:44` for the field declaration; the
builder call site lives in `index/builder.rs::populate_index`, used by
both `from_signature` and `rebuild`.

### Build site (builder.rs)

The `populate_index` helper in `crates/match/src/index/builder.rs`
already iterates every indexable `SignatureBlock`, computing
`block.rolling()` and inserting into both the `tag_table` and the
`lookup`. The bithash insertion attaches to the same loop:

    let digest = block.rolling();
    tag_table[digest.sum1() as usize] = true;
    bithash.insert(digest);                     // NEW
    lookup.entry((digest.sum1(), digest.sum2()))
        .or_default()
        .push(index);

The call is O(1), branchless, and adds one cache-line touch per block.
At 10 M blocks, the build-time overhead is bounded by the bithash
write set (~16 MB of writes) and is negligible relative to the existing
strong-checksum digesting that produced each `SignatureBlock` in the
first place.

`from_signature` and `rebuild` both go through `populate_index`. The
INC_RECURSE per-segment rebuild path inherits the bithash for free,
satisfying parent-doc translation rule 4.

### Probe site (mod.rs)

The probe gate is wired into both `find_match_bytes` (line 78-100) and
`find_match_slices` (line 151-174). The conceptual insertion in
`find_match_slices` (where the parent doc cites line 165):

    if !self.tag_table[digest.sum1() as usize] {
        return None;
    }
    if !self.bithash.probably_present(digest) {  // NEW
        return None;
    }
    let key = (digest.sum1(), digest.sum2());
    let candidates = self.lookup.get(&key)?;

Order matters:

- The `tag_table` check stays first: it is 1 KB, hot in L1, and rejects
  on `sum1` alone in one branchless comparison.
- The bithash check goes second: it is larger (potentially L2/L3 at
  very large `N`) but mixes both `sum1` and `sum2`, so it rejects the
  vast majority of `sum1`-only collisions.
- The `lookup.get` walk goes last: it is the most expensive of the
  three (FxHash + bucket walk + cache-cold `Vec` deref).

`check_block_match_slices` (the `want_i` adjacent-match path,
mod.rs:228-251) **does not** consult the bithash. That path verifies a
specific `block_index` directly via `block.rolling()` equality, not via
the hash table. Adding the bithash check there would only insert a
spurious extra branch.

### Field declaration

The new field on `DeltaSignatureIndex` (mod.rs:38-48):

    pub struct DeltaSignatureIndex {
        block_length: usize,
        strong_length: usize,
        algorithm: SignatureAlgorithm,
        blocks: Vec<SignatureBlock>,
        lookup: FxHashMap<(u16, u16), Vec<usize>>,
        tag_table: Vec<bool>,
        bithash: BitHash,                        // NEW
    }

`BitHash` is a sibling type in `crates/match/src/index/bithash.rs`,
private to the crate. It exposes only `new(block_count: usize)`,
`insert(&mut self, RollingDigest)`, and `probably_present(&self,
RollingDigest) -> bool`.

## 4. Hash mixing decision

The 32-bit rsum is `value() = (s2 << 16) | s1`, packed by
`crates/checksums/src/rolling/digest.rs:178-180`. Bit selection for the
bithash MUST drive from a value that:

1. Mixes both `s1` and `s2`. Driving from `s1` alone wastes the
   8x-larger array, since the existing `tag_table` already filters on
   `s1`'s low 16 bits.
2. Stays consistent with the **signed-byte** Adler interpretation
   confirmed by `723a0a0c1` (PR #3560), pinned by
   `crates/checksums/tests/rolling_signed_byte_regression.rs`.
3. Is computable in `const`-friendly ways without reaching into the
   SIMD dispatch path.

### Selected function

    fn bithash_h(digest: RollingDigest) -> u32 {
        digest.value()  // (s2 as u32) << 16 | (s1 as u32)
    }

    fn bithash_byte(h: u32, mask: u32) -> usize {
        ((h & mask) >> 3) as usize
    }

    fn bithash_bit(h: u32) -> u8 {
        1u8 << (h & 7)
    }

This is a literal port of zsync's `hash.c:101-102` and `rsum.c:362-366`
expressions, with `h` taken from `RollingDigest::value()` rather than
zsync's locally maintained `h` integer.

### Bit width and sign safety

The signed-byte fix in PR #3560 lives **inside** the rolling-hash
accumulator: each contributing byte is interpreted as `(byte as i8)
as i32` per upstream `checksum.c:285`. Accumulators sum into `i32`/`u32`
and mask to 16 bits at digest emission. By the time `value()` returns,
the signed interpretation has been applied; the bithash sees only the
final masked `u16` halves.

`rolling_signed_byte_regression.rs` (PR #3636) pins concrete
expectations: `[0x80; 16]` yields `s1 = 0xF800, s2 = 0xBC00`, so
`value() = 0xBC00_F800`. The bithash addresses byte
`(0xBC00_F800 & mask) >> 3`. Section 5's invariant binds insert and
probe to the same `value()` function, so a build-time-inserted bit is
always present at probe-time regardless of dispatch path (scalar, AVX2,
SSE2, NEON all produce identical digests per parent-doc invariant).

### Why `value()`, not `s1 ^ s2` or other mixing

- `s1 ^ s2` loses information on collisions of the form
  `s1' = s1 ^ x, s2' = s2 ^ x`; `value()` keeps both halves orthogonal.
- `value().wrapping_mul(MIX)` (avalanche-style mixers) diverges from
  zsync's exact mask shape and risks re-introducing dependence on the
  `s1` low bits the `tag_table` already filtered.
- `value().rotate_right(k)` for `k != 0` is equivalent up to a constant
  bucket remapping and adds no information; keep `k = 0` for fidelity.

## 5. Property-test contract

The bithash is **one-sided**. Its only correctness invariant:

> For every `(rsum, basis-byte-window)` pair actually inserted into the
> index at build time, `BitHash::probably_present(rsum)` MUST return
> `true` at probe time.

A violation would be a missed match, which would silently degrade to
literal-byte transfer, hiding under successful interop tests but
costing real bandwidth.

### Proptest strategy sketch

To be implemented in #2062 in
`crates/match/src/index/tests.rs` (or a new
`crates/match/tests/bithash_no_missed_match.rs`).

    proptest! {
        #[test]
        fn bithash_never_misses_inserted_block(
            blocks in proptest::collection::vec(any::<[u8; 1024]>(), 1..=512),
        ) {
            // Build the index from `blocks`, populating both lookup
            // and bithash via the production builder.
            let index = build_index_from_block_bytes(&blocks);

            for window in &blocks {
                let mut rolling = RollingChecksum::new();
                rolling.update(window);
                let digest = rolling.digest();

                // The contract: an inserted block MUST satisfy the
                // bithash gate.
                prop_assert!(index.bithash().probably_present(digest));

                // And, end-to-end, the index MUST find the match.
                prop_assert!(index.find_match_bytes(digest, window).is_some());
            }
        }
    }

A second test pins the **density bound** (deterministic, no proptest):

    #[test]
    fn bithash_density_bounded_by_one_eighth() {
        // Insert N random rsums into a bithash sized for N blocks.
        // set-bit count <= N (each insert sets one bit; collisions
        // can only reduce the count).
        // set-bit count / total-bits <= 1/8 + epsilon.
    }

The signed-byte fixture (`rolling_signed_byte_regression.rs`) is a
**prerequisite**: any rolling-hash regression violating
`(byte as i8) as i32` would change the inserted `value()` and silently
break the bithash invariant. Per the parent doc, #2059 is gated by
that fixture being green.

## 6. Wire-compat restatement

The bithash is purely in-memory. It MUST NOT alter any of the following
layers; each layer remains byte-identical to the pre-bithash baseline
and to upstream rsync 3.0.9 / 3.1.3 / 3.4.1.

1. **Signature payload.** The `SignatureBlock` rolling+strong checksum
   wire serialization is governed by `signature` and `protocol`. Field
   layouts, lengths, and orderings stay unchanged. The bithash never
   leaves `DeltaSignatureIndex`.
2. **NDX framing.** Sender-side block-index encoding (NDX_*, varint,
   negative-token framing) is untouched. The bithash gates only
   receiver-side delta search.
3. **Capability negotiation.** `build_capability_string()` in
   `core/src/client/setup.rs` is untouched. No new flag in the SSH
   capability string.
4. **Protocol-32 handshake.** Greeting, version negotiation, multiplex
   `MSG_*` frames - none are touched.
5. **Golden bytes.** `crates/protocol/tests/golden/` byte-comparison
   tests pass unchanged.
6. **tcpdump replay.** `tcpdump`-captured application-layer payloads
   for an oc-rsync push to upstream daemon are byte-identical with the
   bithash on vs off (interop verification under #2075).
7. **CLI surface.** No new `clap` argument, no new env var that affects
   wire output. Internal toggles are cfg-gated benchmark scaffolding
   only (#2084).
8. **Interop matrix.** `tools/ci/run_interop.sh` against upstream
   3.0.9 / 3.1.3 / 3.4.1 produces zero new entries in
   `tools/ci/known_failures.conf`.

The only observable effect is reduced CPU time on the receiver during
delta application, with the same delta-script bytes flowing on the
wire.

## 7. Bench scaffolding plan

Bench scaffolding lands in #2063 and is **cfg-gated**, never wired into
the CLI per #2084.

### What gets measured

Two quantities determine whether the bithash is pulling its weight:

1. **Rejection rate.** Of all probes that pass the `tag_table` gate,
   what fraction does the bithash reject? Formally:

       reject_rate = bithash_rejections / (bithash_rejections + bithash_hits)

   Target: `>= 0.85` for typical rsync workloads (parent doc's 7/8
   bound). Below `0.50`: the bithash adds branches without filtering
   enough; revert.

2. **End-to-end CPU delta.** Wall-clock time on `crates/match` micro-bench
   and on the existing `scripts/benchmark.sh` workloads, comparing
   bithash-on vs bithash-off. Target: `>= 5%` total delta-application
   time saved on workloads with >= 100 K blocks.

### Counters and harness

Cfg-gated `AtomicU64` counters (`probes`, `rejections`) live on
`DeltaSignatureIndex` behind `#[cfg(feature = "bench-bithash")]`. A
new bench at `crates/match/benches/bithash_rejection.rs` (added under
#2063) drives synthetic basis-vs-target pairs at `N` in
`{1_000, 10_000, 100_000, 1_000_000}` and prints
`reject_rate / probes / wall_time_us`.

The feature flag is **internal**: declared in `crates/match/Cargo.toml`
but never propagated to workspace `default-features`, the CLI `features`
table, or the daemon. CI does not run the bench. There is no way to
turn it on from a release `oc-rsync` invocation.

### Decision flow

1. #2060 / #2061 land the implementation behind no feature gate;
   `BitHash` is always built and always probed.
2. #2062 lands proptest property tests (no feature gate).
3. #2063 lands the bench harness behind `bench-bithash`.
4. Bench results feed #2084's decision record. If reject rate is
   `>= 0.85` AND CPU savings `>= 5%`, the implementation stays. If
   either fails, the implementation is reverted and the design moves
   to "shelved" alongside `parallel_chunks_design.md`.
5. The `bench-bithash` feature flag is **deleted** at #2087 cleanup
   regardless of outcome. No feature flag survives into release builds.

## References

### zsync 0.6.2 (gianm/zsync mirror, librcksum/)

- `internal.h:83` - `BITHASHBITS` definition
- `hash.c:62-84` - `i` selection for hash table sizing
- `hash.c:101-102` - bithash insertion expression
- `rsum.c:362-366` - bithash probe expression

### oc-rsync source

- `crates/match/src/index/mod.rs:38-48` - `DeltaSignatureIndex` field
  declarations (bithash field added here)
- `crates/match/src/index/mod.rs:78-100` - `find_match_bytes` (probe gate
  goes after `tag_table` check)
- `crates/match/src/index/mod.rs:151-174` - `find_match_slices` (probe
  gate goes after `tag_table` check, parent doc citation point)
- `crates/match/src/index/mod.rs:228-251` - `check_block_match_slices`
  (intentionally NOT gated on bithash)
- `crates/match/src/index/builder.rs:16-36` - `populate_index` helper
  (bithash insertion call site)
- `crates/match/src/index/builder.rs:71-98` - `rebuild` (per-segment
  reuse for INC_RECURSE)
- `crates/checksums/src/rolling/digest.rs:175-208` - `value`, `sum1`,
  `sum2` accessors driving the bithash mixing function
- `crates/checksums/tests/rolling_signed_byte_regression.rs` - PR #3636
  fixture pinning the signed-byte invariant the bithash builds on

### Parent design and parity guards

- `docs/design/zsync-inspired-matching.md` - parent design note
  enumerating the four techniques and wire-compat invariants
- `crates/checksums/tests/rolling_simd_parity.rs` - SIMD-vs-scalar
  parity proptest (must stay green)
- `crates/protocol/tests/golden/` - wire-format byte goldens (must stay
  green)
- `tools/ci/run_interop.sh` - upstream 3.0.9 / 3.1.3 / 3.4.1 matrix
  (must stay green)

### Tracking

- #2059 - this design note (this PR)
- #2060, #2061 - bithash implementation (blocked by this)
- #2062 - bithash property tests (blocked by #2060/#2061)
- #2063 - bithash bench scaffolding (blocked by #2060/#2061)
- #2076 / PR #3636 - signed-byte regression fixture (gating prerequisite,
  already merged)
- #2084 - keep-or-revert decision record
- #2087 - cfg-gate cleanup, removes `bench-bithash` feature
