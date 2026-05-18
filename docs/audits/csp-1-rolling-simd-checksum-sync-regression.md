# CSP-1: rolling-checksum SIMD underperformance in checksum-sync mode

Tracking issue: oc-rsync task #2386. Branch: `docs/csp-1-rolling-simd-audit`.
Pure audit. No source changes. All file:line citations are repository-relative
for oc-rsync and `target/interop/upstream-src/rsync-3.4.1/<file>:<line>` for
upstream.

## 1. Problem statement

Users report that oc-rsync's `--checksum` (`-c`) mode runs 1.5-1.7x slower than
upstream rsync 3.4.1 against the same source tree on identical hardware, and
attribute the gap to the rolling-checksum SIMD path. This audit walks the
checksum-sync flow end-to-end, confirms which code paths the rolling-checksum
SIMD dispatcher actually engages, compares the per-iteration shape of our
AVX2 / SSE2 / NEON loops against upstream `simd-checksum-x86_64.cpp`, and
ranks the most likely contributors to the regression. No fix is proposed for
landing yet; the document closes with a concrete fix plan and a bench plan
designed to confirm the hypothesis.

## 2. Code path walk

`--checksum` enables two independent kinds of work, and the rolling-checksum
SIMD path is only on the second.

### 2.1 Whole-file digest for skip/transfer decision (no rolling)

Receiver-side, when `--checksum` is set:

- `crates/transfer/src/receiver/transfer/candidates.rs:128` builds
  `always_checksum = Some(<algorithm>)` and passes it through
  `quick_check_matches`.
- `crates/transfer/src/receiver/quick_check.rs:60` routes into
  `file_checksum_matches`
  (`crates/transfer/src/receiver/quick_check.rs:262-287`).
- That function opens the destination file, allocates a 64 KiB stack buffer,
  and feeds it to a strong-only `ChecksumVerifier` (MD5 / MD4 / XXH3 /
  XXH3-128). **No rolling-checksum work happens on this path.** Upstream's
  equivalent is `target/interop/upstream-src/rsync-3.4.1/checksum.c:402
  file_checksum()` and it is also strong-only.

Sender-side, `--checksum` also enables file-list-time digests through
`crates/transfer/src/generator/mod.rs:714-722`, which calls
`FileListWriter::with_always_checksum(factory.digest_length())`
(`crates/protocol/src/flist/write/mod.rs:285`). That helper only widens the
flist wire framing; the digest payload itself is computed elsewhere. The
digest computed here is strong, not rolling. So `--checksum` itself does not
directly invoke `RollingChecksum::update`.

### 2.2 Block signature + delta match (this is where rolling SIMD lives)

For every file the receiver actually still needs to fetch (size mismatch, or
strong digest mismatch on the destination), the receiver builds block
signatures over the **basis** file and the sender runs a sliding-window match.
`--checksum` materially increases the count of files driven through this
phase because it short-circuits the mtime+size quick-check (upstream
`target/interop/upstream-src/rsync-3.4.1/generator.c:626`), so any reported
"checksum-sync slowdown" is dominated by this phase rather than by the
whole-file digest.

Receiver path:

- `crates/transfer/src/receiver/basis.rs:198-219 generate_basis_signature` ->
  `engine::signature::generate_file_signature` ->
  `crates/signature/src/generation.rs:81 generate_file_signature`. That
  function reads each basis block into a reusable buffer
  (`crates/signature/src/generation.rs:126`) and calls
  `RollingDigest::from_bytes(chunk)`
  (`crates/signature/src/generation.rs:129`).
- `crates/checksums/src/rolling/digest.rs:57 from_bytes` constructs a fresh
  `RollingChecksum`, calls `update(bytes)`, and reads the digest. Each call is
  a single chunk of `block_len` (defaults to ~700-65536 bytes per layout).
- `RollingChecksum::update` (`crates/checksums/src/rolling/checksum/mod.rs:164`)
  funnels into `accumulate_chunk_dispatch`
  (`crates/checksums/src/rolling/checksum/mod.rs:520`), which calls
  `accumulate_chunk_arch` (`mod.rs:535` for aarch64 / `mod.rs:541` for x86).
  On aarch64 the dispatcher returns
  `neon::accumulate_chunk(...)` (`neon.rs:82`), which feature-checks then
  invokes `accumulate_chunk_neon_impl` (`neon.rs:94`). On x86_64 the dispatcher
  returns `x86::try_accumulate_chunk(...)` (`x86.rs:106`), which feature-checks
  then invokes either `accumulate_chunk_avx2` (`x86.rs:192`) or
  `accumulate_chunk_sse2` (`x86.rs:143`).
- Parallel signature generation lives in
  `crates/signature/src/parallel.rs:138-158` and also drives
  `RollingDigest::from_bytes(data)` per block on a rayon pool.

Sender path:

- `crates/matching/src/generator.rs:149` instantiates one
  `RollingChecksum::new()` per file.
- The matching loop fills the window byte-by-byte
  (`crates/matching/src/generator.rs:212-228`) via `roll` and `update_byte` -
  the SIMD path does **not** engage on these single-byte calls.
- After each successful match, the window is bulk-refilled and the checksum
  re-seeded from scratch via two slice updates
  (`crates/matching/src/generator.rs:343-373`):

  ```text
  window.clear();
  rolling.reset();
  ...
  rolling.update(s1);            // one full chunk
  if !s2.is_empty() {
      rolling.update(s2);
  }
  ```

  Each `update` call is one trip through `accumulate_chunk_dispatch`, with
  the chunk equal to the contiguous half of the ring buffer (sum length ==
  block_len). On a `--checksum` run against trees of mostly-matching files,
  this path executes once per matched block.

`--checksum` therefore hits the rolling SIMD path at three places:

1. Per-block `RollingDigest::from_bytes(chunk)` during basis signature
   generation (receiver side; sequential and parallel variants).
2. Per-match `rolling.update(s1)` plus optional `rolling.update(s2)` during
   the sender match loop, executed every time the window is re-seeded after a
   block match.
3. Per-file `RollingChecksum::new()` ring-buffer warm-up via `update_byte` and
   `roll` (scalar; SIMD never engages here because the input is one byte at a
   time).

## 3. SIMD dispatch audit

### 3.1 Feature detection actually engages

x86 (`crates/checksums/src/rolling/checksum/x86.rs:79-103`): CPUID is cached
in a `OnceLock<FeatureLevel>` (one-time `is_x86_feature_detected!("avx2")` /
`("sse2")`). Every call funnels through `effective_features()`
(`x86.rs:91-97`) which intersects the cached level with the CLI override read
via `feature_allowed` (`crates/checksums/src/cpu_features.rs:225`). That
helper performs a `OVERRIDE.load(Ordering::SeqCst)`
(`crates/checksums/src/cpu_features.rs:181-188`) on every call. On
x86_64 a SeqCst load lowers to a plain MOV (no fence), so the cost is small
in absolute terms, but it does happen **once per `try_accumulate_chunk` call**
- once per block during signature generation, and once per re-seed during the
sender match loop. AVX2 is selected when `chunk.len() >= 32 && features.avx2`
(`x86.rs:114`); otherwise SSE2 is tried at `chunk.len() >= 16`. Result: AVX2
**does engage** for every typical block_len (>= 32 bytes).

aarch64 (`crates/checksums/src/rolling/checksum/neon.rs:63-79`): cached in
`OnceLock<bool>`. Same per-call `OVERRIDE.load(SeqCst)` overhead via
`feature_allowed`. NEON is unconditionally available on aarch64, so the
dispatcher always enters `accumulate_chunk_neon_impl` for chunks >= 16 bytes.
Result: NEON **does engage**.

Other architectures fall through to scalar via
`crates/checksums/src/rolling/checksum/mod.rs:547-554`.

So the regression is not "SIMD never engages". The regression is in **how
much work the SIMD loop does per byte once it does engage**.

### 3.2 Per-iteration work, ours vs upstream

Upstream rsync's SIMD rolling-checksum lives in
`target/interop/upstream-src/rsync-3.4.1/simd-checksum-x86_64.cpp`. Three
shapes coexist behind GCC multi-versioned dispatch: `get_checksum1_avx2_64`
(`simd-checksum-x86_64.cpp:338`), `get_checksum1_ssse3_32`
(`simd-checksum-x86_64.cpp:113`), `get_checksum1_sse2_32`
(`simd-checksum-x86_64.cpp:218`). Upstream has no NEON path; on aarch64 it
uses the scalar loop at
`target/interop/upstream-src/rsync-3.4.1/checksum.c:285-300`.

Stride and per-iteration arithmetic:

| Path | Stride (bytes / iter) | s1/s2 accumulators kept in | Reduction frequency |
|------|----------------------:|----------------------------|---------------------|
| Upstream AVX2 (`simd-checksum-x86_64.cpp:338`) | **64** | `__m128i ss1`, `__m128i ss2` registers | once at end of stripe |
| Upstream SSSE3 (`simd-checksum-x86_64.cpp:113`) | **32** | `__m128i ss1`, `__m128i ss2` registers | once at end of stripe |
| Upstream SSE2 (`simd-checksum-x86_64.cpp:218`) | **32** | `__m128i ss1`, `__m128i ss2` registers | once at end of stripe |
| oc-rsync AVX2 (`x86.rs:192`) | **32** | scalar `u32 s1`, `u32 s2` | **every iteration** via `_mm256_storeu_si256` + scalar fold |
| oc-rsync SSE2 (`x86.rs:143`) | **16** | scalar `u32 s1`, `u32 s2` | **every iteration** via `_mm_storeu_si128` + scalar fold |
| oc-rsync NEON (`neon.rs:94`) | **16** | scalar `u32 s1`, `u32 s2` | **every iteration** via `vaddlvq_s16` x 4 |

The two-rows-down stride hit is the most visible difference. Upstream's AVX2
consumes 64 bytes per iteration with an explicit prefetch
(`simd-checksum-x86_64.cpp:410 _mm_prefetch(&buf[i + 160], _MM_HINT_T0)`).
Ours consumes 32. Upstream's SSE2 consumes 32 bytes per iteration via two
`maddubs_epi16` ops fused into one accumulator update. Ours consumes 16.

The keep-in-register difference is even more punishing. Upstream initialises
`ss1` and `ss2` once at the top of the function (`simd-checksum-x86_64.cpp:343-344`),
updates them in place every iteration (`ss2 = _mm_add_epi32(ss2,
_mm_slli_epi32(ss1, 5))` for stride 32, shift-6 for stride 64), and only
spills back to scalar `*ps1` / `*ps2` after the loop terminates
(`simd-checksum-x86_64.cpp:204-207`). Ours keeps `s1`, `s2` in a regular
`u32` and computes `s2 = s2.wrapping_add(block_prefix)` then
`s2 = s2.wrapping_add(s1.wrapping_mul(BLOCK_LEN as u32))` then `s1 =
s1.wrapping_add(block_sum)` after every SIMD batch
(`x86.rs:170-173`, `x86.rs:211-214`, `neon.rs:124-126`). Each block-prefix
calculation requires a full horizontal reduction (`_mm256_storeu_si256` +
8-lane scalar fold on AVX2, `_mm_storeu_si128` + 4-lane scalar fold on SSE2,
`vaddlvq_s16` x 4 on NEON) every iteration. Those reductions serialize
through the cross-lane shuffle / store / scalar-add chain.

Per-byte instruction sketch (rough, omitting register moves):

- Upstream SSSE3 32-byte iter (`simd-checksum-x86_64.cpp:127-201`):
  2 loads, 4 `maddubs_epi16`, ~10 vector adds and shifts, 1 `madd_epi16`,
  no horizontal stores.
- Our SSE2 16-byte iter (`x86.rs:158-175`):
  1 load, 1 `cmplt_epi8`, 2 unpack, 3 `madd_epi16`, 1 cross-lane store,
  4-lane scalar fold (3 adds), then 2 scalar `wrapping_add` + 1
  `wrapping_mul` to roll s1/s2 forward. Effective work per byte is at
  least 2x upstream.
- Upstream AVX2 64-byte iter (`simd-checksum-x86_64.cpp:353-415`): 4 lane
  loads + 1 `inserti128` pair, 4 `maddubs_epi16`, ~12 vector ops, 1
  prefetch, no horizontal stores.
- Our AVX2 32-byte iter (`x86.rs:205-216`): 1 wide load, then for both
  `sum_block_avx2` (`x86.rs:235-254`) and `prefix_sum_avx2` (`x86.rs:257-280`):
  2 castsi128/extracti128, 2 `cvtepi8_epi16` (sign extend), 2
  `madd_epi16`, 1 `add_epi32`, 1 cross-lane store, 8-lane scalar fold.
  Two such reductions per iteration. Effective work per byte is at least
  2x upstream.

The sign-extension path is the third structural penalty. Upstream relies on
`maddubs_epi16(set1_epi8(1), in8)` to perform the byte-to-signed-i16 widening
**inside the same instruction that performs the per-pair sum**. Ours
performs an explicit sign-extension (`_mm_cmplt_epi8` + `unpacklo_epi8` /
`unpackhi_epi8` on SSE2; `_mm256_cvtepi8_epi16` on AVX2; `vmovl_s8` on NEON)
before the multiply-add, costing an extra 1-3 instructions per iteration.

### 3.3 Other dispatch-side overheads

- `accumulate_chunk_dispatch` (`mod.rs:520`) returns
  `Option<(u32, u32, usize)>` from `accumulate_chunk_arch` and then masks
  the result via `mask_result` (`mod.rs:557`). The `Option` is always
  `Some(..)` on aarch64 (`mod.rs:536`) and is `Some(..)` on x86 whenever
  the chunk is >= 16 bytes (`x86.rs:114-122`). The `Option` wrap + match
  + tuple-destructure imposes a function-call boundary that the optimiser
  cannot inline through cleanly on x86 because `try_accumulate_chunk` is
  not `#[inline]` (`x86.rs:106-125`). The same observation applies to the
  `mask_result` call: the mask is required for `update_byte` callers but
  not for the SIMD path, which already keeps s1/s2 < 2^17 after each
  iteration; masking every dispatch wastes two ANDs of independent
  registers.
- `update_byte` (`mod.rs:191-198`) masks with `0xffff` on every byte and
  saturates `len` with `saturating_add(1)`. Upstream scalar
  (`checksum.c:296-298`) does neither and masks only once at the end of
  the loop. `update_byte` is on the sender match loop's per-byte fast
  path (`crates/matching/src/generator.rs:227`).
- `effective_features()` reads the CLI override (`SeqCst` atomic load)
  on every call. Not catastrophic in absolute terms, but it is one more
  load on the critical path of every dispatch.

## 4. Upstream comparison summary

The reductions upstream achieves and we miss:

- **Per-iteration stride**: upstream AVX2 = 64 bytes, ours = 32. Upstream
  SSE2 = 32 bytes, ours = 16. (`simd-checksum-x86_64.cpp:127`, `:232`, `:353`
  vs `x86.rs:158`, `:205`.)
- **Accumulator residency**: upstream keeps `ss1`/`ss2` in SSE registers
  across the whole stripe; ours horizontally reduces to scalar after every
  iteration (`simd-checksum-x86_64.cpp:343-207`,
  `simd-checksum-x86_64.cpp:223-311` vs `x86.rs:170-173`, `:211-214`,
  `neon.rs:124-126`).
- **Sign-extension fusion**: upstream's `maddubs_epi16(set1_epi8(1), in8)`
  pattern fuses widening with the per-pair sum; ours splits widening into
  its own step (`simd-checksum-x86_64.cpp:142-148` vs `x86.rs:163-166`).
- **Prefetch**: upstream issues `_mm_prefetch(&buf[i + 160], _MM_HINT_T0)`
  (`simd-checksum-x86_64.cpp:410`) at the AVX2 stride. We have none.
- **NEON**: upstream has none, so on aarch64 we are competing against
  upstream's scalar (`checksum.c:285-300`). Our NEON loop processes 16
  bytes per iteration with a `vaddlvq_s16` x 4 reduction every iteration.
  A scalar 4-byte unrolled loop with no horizontal reduction can be
  faster per byte once the cross-lane reduction cost is paid.
- **Per-byte mask**: `RollingChecksum::update_byte` masks every byte; the
  scalar accumulator path and upstream both mask only at the end.

## 5. Suspected root cause (ranked)

### H1 (highest): horizontal reduction after every SIMD iteration

Every SIMD iteration in `accumulate_chunk_sse2` / `accumulate_chunk_avx2` /
`accumulate_chunk_neon_impl` ends with a cross-lane store + scalar fold to
materialise the block sum and block prefix into a `u32`. Upstream keeps the
equivalent values in SSE registers (`ss1`, `ss2`) across the entire stripe
and only extracts once at the end. The horizontal reduction is the slowest
SIMD construct on every micro-architecture; doing it 64 / stride times per
KiB is the most likely single cause of the 1.5-1.7x gap. **Concrete evidence:**

- `x86.rs:170-173` reduces `block_sum` / `block_prefix` from `_mm256_storeu_si256`
  + scalar fold per iter (`sum_block_avx2` `x86.rs:235-254`, `prefix_sum_avx2`
  `x86.rs:257-280`).
- `neon.rs:124-126` calls `vaddlvq_s16` four times per iter
  (`vaddlvq_s16(high)`, `vaddlvq_s16(low)`,
  `vaddlvq_s16(weighted_high)`, `vaddlvq_s16(weighted_low)`).
- Upstream AVX2 reduces only at `simd-checksum-x86_64.cpp:389-396` after
  the stripe loop completes.

### H2: half-stride compared to upstream

Even if we keep accumulators in registers, our AVX2 stride is half of
upstream's. Each block of basis bytes traverses the loop 2x more often, which
multiplies whatever fixed per-iteration overhead remains. SSE2 has the same
2x stride deficit. **Concrete evidence:** `x86.rs:71`
(`AVX2_BLOCK_LEN: usize = 32`) and `x86.rs:70` (`SSE2_BLOCK_LEN: usize = 16`)
vs upstream's 64/32-byte loops.

### H3: per-call dispatch overhead amplified by short chunks

`accumulate_chunk_dispatch` is called once per `update(chunk)`. The signature
generator calls it once per block (chunk = block_len, typically 700-65536
bytes). The sender's match loop calls it once per re-seed (chunk = block_len
or two halves of block_len). On a `--checksum` run where most files match,
this dispatch fires for every block. Each dispatch carries:

1. `OVERRIDE.load(Ordering::SeqCst)` in `feature_allowed`.
2. `Option`-wrapped indirection through `try_accumulate_chunk`
   (`x86.rs:106-125`, not marked `#[inline]`).
3. `mask_result` (`mod.rs:557`) doing two ANDs that the SIMD path does not
   require.

In aggregate this is small per call, but on a tree of millions of small
files it pays a constant tax per file. **Concrete evidence:** missing
`#[inline]` on `try_accumulate_chunk`; `feature_allowed` (`cpu_features.rs:225`)
performs `OVERRIDE.load(SeqCst)`.

### H4: NEON-specific underperformance because aarch64 competes vs upstream scalar

Upstream has no NEON path and falls back to scalar on aarch64. Our NEON loop
processes 16 bytes per iteration with **four** `vaddlvq_s16` reductions per
iteration. On Apple Silicon and Graviton, `vaddlvq_s16` is in the 4-cycle
ballpark on the integer side, so four of them per 16 bytes (1 reduction per 4
bytes) approaches the throughput of the 4-byte scalar unrolled loop with no
reduction at all. **Concrete evidence:** `neon.rs:113-122` performs four
`vaddlvq_s16` reductions; scalar `accumulate_chunk_scalar_raw`
(`mod.rs:568-599`) does zero. Aarch64 users are the most likely to notice
the 1.5x gap because they are effectively running our slow NEON loop while
upstream runs its fastest scalar loop.

## 6. Fix plan (do not ship from this audit)

Three changes, ordered by expected payoff:

### F1 (largest expected win): keep s1/s2 in SIMD registers across the stripe

`crates/checksums/src/rolling/checksum/x86.rs:143-185` (SSE2),
`crates/checksums/src/rolling/checksum/x86.rs:192-231` (AVX2),
`crates/checksums/src/rolling/checksum/neon.rs:94-139` (NEON).

Refactor the inner loop to mirror upstream's pattern: broadcast the entry
`s1`/`s2` into vector lanes once, accumulate per-iteration sums and weighted
prefixes into vector registers, and only horizontally reduce after the
stripe terminates. For AVX2 this means an `__m128i ss1`, `__m128i ss2` pair
that absorbs each iteration via `_mm_add_epi32(ss2, _mm_slli_epi32(ss1, 5))`
(or `slli_epi32(ss1, 6)` if F2 is taken). For NEON the equivalent is an
`int32x4_t` pair updated via `vshlq_n_s32(ss1, 4)` plus `vaddq_s32`.

Expected speedup: 1.4-1.8x on the per-byte SIMD throughput, recovering the
bulk of the upstream gap.

### F2: widen the stride to 64 bytes (AVX2) and 32 bytes (SSE2)

Same files as F1. Process 64 bytes per AVX2 iter and 32 bytes per SSE2 iter
to match upstream. On NEON, widen to 32 bytes by processing two 128-bit
loads per iteration; the prefix-weight constants need to be split into two
8-lane halves.

Expected speedup: 1.15-1.3x on top of F1 (less if F1 already saturates
the back-end pipeline; more on Intel client cores where AVX2 has plenty of
back-end headroom).

### F3: drop per-call dispatch overhead

`crates/checksums/src/rolling/checksum/x86.rs:99-125`,
`crates/checksums/src/rolling/checksum/neon.rs:65-79`,
`crates/checksums/src/rolling/checksum/mod.rs:520-559`,
`crates/checksums/src/rolling/checksum/mod.rs:191-198`.

- Mark `try_accumulate_chunk` and `neon::accumulate_chunk` `#[inline]`.
- Cache `effective_features()` in a second `OnceLock<FeatureLevel>` (the
  CLI override is one-shot per process, so a one-time intersection is
  legal).
- Hoist the `mask_result` call out of the SIMD path; the in-register
  accumulators already stay below 2^17 and the final `(s1 << 16) | s2`
  pack happens at the public `value()` boundary.
- Match upstream by removing the per-byte `& 0xffff` in `update_byte`;
  the public `value()` path already masks via `mask_result` /
  `(self.s2 << 16) | self.s1`.

Expected speedup: 1.05-1.10x on workloads dominated by signature
generation over many small basis files (the per-call dispatch tax
amortises poorly there).

Combined upper bound for F1+F2+F3 is in the 1.7-2.2x range on the
rolling-checksum micro-benchmark, which should close the 1.5-1.7x
observed regression at the workload level.

## 7. Bench plan

Add to `crates/checksums/benches/checksums_benchmark.rs` (the existing
Criterion suite) a new group exercising the exact shape of the
checksum-sync hot path:

```text
group: rolling_checksum_simd_throughput
  inputs: chunk sizes 256, 512, 1024, 4096, 16384, 65536, 262144
  metrics: bytes per second (Throughput::Bytes)
  scenarios:
    - update_fresh_simd: one RollingChecksum::new(); update(chunk); value()
    - reseed_after_match: RollingChecksum::new(); update(block_a); reset();
        update(block_b); value()   (mirrors generator.rs:343-373)
    - signature_block_loop: for each of N blocks: RollingDigest::from_bytes(chunk)
        (mirrors generation.rs:129)
```

For F1/F2 patch verification, also add per-iteration `update_byte` and
`roll` benches to confirm the scalar paths do not regress.

The existing `crates/matching/benches/profiling_analysis.rs:300
bench_rolling_checksum_detailed` already covers the per-byte and roll
shapes; add a sibling `bench_rolling_checksum_simd_stripe` next to it that
forces the SIMD path with chunk sizes >= 1024.

CI gate: the `benchmark.yml` workflow already runs Criterion in the
`benchmark` job. The fix PR should attach the before/after Criterion deltas
showing >= 1.4x improvement on AVX2 (`x86_64-unknown-linux-gnu` runners)
and on NEON (`aarch64-apple-darwin` and `aarch64-unknown-linux-gnu`
runners). Scalar-only paths must regress by < 2% (within noise).

After the F1+F2+F3 patches land, re-run the workload-level benchmark
(`scripts/benchmark.sh --mode checksum`) against upstream 3.4.1 in the
`rsync-profile` Linux container to confirm the wall-clock 1.5-1.7x gap
closes.

## 8. References

- `crates/checksums/src/rolling/checksum/mod.rs`
- `crates/checksums/src/rolling/checksum/x86.rs`
- `crates/checksums/src/rolling/checksum/neon.rs`
- `crates/checksums/src/rolling/digest.rs`
- `crates/checksums/src/cpu_features.rs`
- `crates/signature/src/generation.rs`
- `crates/signature/src/parallel.rs`
- `crates/matching/src/generator.rs`
- `crates/transfer/src/receiver/basis.rs`
- `crates/transfer/src/receiver/quick_check.rs`
- `crates/transfer/src/receiver/transfer/candidates.rs`
- `crates/transfer/src/generator/mod.rs`
- `crates/checksums/benches/checksums_benchmark.rs`
- `crates/matching/benches/profiling_analysis.rs`
- `target/interop/upstream-src/rsync-3.4.1/checksum.c`
- `target/interop/upstream-src/rsync-3.4.1/simd-checksum-x86_64.cpp`
- `target/interop/upstream-src/rsync-3.4.1/match.c`
- `docs/audits/checksum-sync-regression-diagnosis.md`
- `docs/audits/checksum-mode-computation-cost.md`
- `docs/audits/delta-sender-parallel-rolling-hash.md`
- `docs/audits/sve-rolling-checksum-feasibility.md`
