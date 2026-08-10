# AVX-512 VPDPBUSD path for the rolling checksum (#1835)

Tracking issue: oc-rsync task #1835.

This document is design-only. No code lands in this PR. The entry
points, type stubs, and dispatch sites named here are sketches that
follow the existing AVX2/SSE2 ladder shape so the rolling checksum
dispatcher keeps a single calling convention across vector widths.

Related code and design notes:

- `crates/checksums/src/rolling/checksum/x86.rs` - the existing AVX2
  (32 bytes/iter) and SSE2 (16 bytes/iter) ladder this design extends.
- `crates/checksums/src/rolling/checksum/mod.rs:50-66` - the
  `simd_available_arch()` dispatcher that selects between aarch64 NEON,
  x86 AVX2/SSE2, and the scalar fallback.
- `crates/checksums/src/rolling/checksum/mod.rs:561-606` -
  `accumulate_chunk_scalar_raw`, the byte loop that mirrors upstream
  `checksum.c:get_checksum1()` and is the canonical scalar reference.
- `crates/checksums/src/cpu_features.rs:106` - `SimdFeature::Avx512`
  override path used by the `--simd=<level>` CLI override (#1825).
- `crates/checksums/src/simd_parity_tests.rs` - SIMD vs scalar parity
  harness this path must extend.
- `docs/design/sve-rolling-checksum-aarch64.md` - sister design for the
  aarch64 wide-vector path; uses the same dispatcher contract.
- `docs/design/avx512-md4-md5-batch.md` - sibling AVX-512 design that
  established the project's CPUID detection convention.

## 1. Motivation

The rolling checksum (`get_checksum1` upstream, `RollingChecksum::update`
in oc-rsync) sits on the hot path of every delta transfer: the receiver
slides it byte-by-byte over the basis file, and the sender computes one
sum per block during signature emission. On x86_64 today the work is
done by `accumulate_chunk_avx2`
(`crates/checksums/src/rolling/checksum/x86.rs:192`) at a fixed 32 bytes
per iteration through `_mm256_madd_epi16` against sign-extended halves
of each 256-bit block.

VPDPBUSD ("vector packed dot product, unsigned-by-signed bytes,
dword-accumulate"), introduced with AVX-512-VNNI on Cascade Lake / Ice
Lake / Tiger Lake / Sapphire Rapids and on AMD Zen 4+, performs four
8x8-bit dot products per 32-bit lane in a single instruction:

    dst[i] = src1[i]
        + a[4*i+0] * b[4*i+0] + a[4*i+1] * b[4*i+1]
        + a[4*i+2] * b[4*i+2] + a[4*i+3] * b[4*i+3]

`a` is read as unsigned bytes, `b` as signed bytes, and the result is a
saturating 32-bit add into the existing accumulator lane. With a 512-bit
register that is 64 input bytes folded into 16 32-bit lanes per
instruction. uops.info reports a 1-cycle reciprocal throughput for
`vpdpbusd zmm, zmm, zmm` on Sapphire Rapids and Zen 4, so the bulk
accumulator can ingest 64 bytes per cycle - roughly 2x the AVX2 path's
32 bytes per iteration with two `vpmaddwd`s.

Reference background (Intel Optimization Reference Manual, "AVX-512
VNNI"; Intel Deep Learning Boost technical article at
`https://www.intel.com/content/www/us/en/developer/articles/technical/intel-deep-learning-boost.html`;
uops.info per-instruction latency tables) is included as future-
verification material; the implementation must measure on real hardware
before claiming the speedup.

## 2. Current state

The x86 ladder probes once at first use and caches a `FeatureLevel`
record in a `OnceLock`
(`crates/checksums/src/rolling/checksum/x86.rs:79-87`). The dispatcher
`try_accumulate_chunk` (`x86.rs:106`) tries AVX2 first, then SSE2, then
returns `None` so `accumulate_chunk_dispatch`
(`mod.rs:519`) falls back to `accumulate_chunk_scalar_raw`
(`mod.rs:568`).

Each SIMD path tail-calls into the next narrower path for trailing
bytes that do not fill a full SIMD lane, so byte-for-byte parity with
upstream is preserved at every input length. Parity is enforced by
`crates/checksums/src/simd_parity_tests.rs` and the per-arch test
modules under `crates/checksums/src/rolling/tests/`.

The CLI override `--simd=<level>` is plumbed through
`SimdFeature::Avx512` in `crates/checksums/src/cpu_features.rs:106`,
which means the dispatcher can already see an "AVX-512 allowed" signal
even though no AVX-512 path is currently consumed by the rolling
checksum.

## 3. Background: VPDPBUSD semantics for Adler-style sums

Upstream rsync's `get_checksum1` (`target/interop/upstream-src/rsync-3.4.1/checksum.c`)
is an Adler-32-shaped function but with two project-specific quirks
that the SIMD path must preserve:

- **No prime modulus.** rsync does not reduce modulo 65521 (the Adler-32
  prime). It uses 16-bit add wrap on `s1` and `s2`. The scalar reference
  in `mod.rs:568` masks to `0xffff` only on the public `value()`
  boundary, mirroring upstream's `get_checksum1` final mask.
- **Signed-byte interpretation.** Upstream casts `buf` to `schar *`, so
  each byte contributes in `-128..127`. The existing AVX2 path handles
  this with `_mm256_cvtepi8_epi16` before `_mm256_madd_epi16`
  (`x86.rs:233-280`). VPDPBUSD natively reads its first operand as
  *unsigned* bytes and second as *signed*, so the sign-extension trick
  has to be inverted (see Section 4.2).

`s1` is the running byte sum and `s2` is the running prefix-weighted
sum. For a chunk of length `N`:

    s1' = s1 + sum_{i=0..N-1} schar(buf[i])
    s2' = s2 + N * s1 + sum_{i=0..N-1} (N - i) * schar(buf[i])

The AVX2 path computes the second sum with a `_mm256_madd_epi16` against
a precomputed weight vector (`first_half_weights`, `second_half_weights`
in `x86.rs:199-203`). The VPDPBUSD path replaces both sums with single-
instruction dot products against vectors of `1`s (for `s1`) and the
descending weight ramp (for `s2`).

## 4. Design

### 4.1 Window size

64 bytes (one zmm-register chunk) per VPDPBUSD pair. That is the smallest
increment that uses the full 512-bit register and matches the
instruction's natural input width. Section 8 discusses 128-byte
unrolling.

### 4.2 Mathematical mapping

Two zmm accumulators, `s1_acc` and `s2_acc`, each a `__m512i` of 16
lanes of `i32`. Per 64-byte block `b[0..64]`:

1. Load `block = _mm512_loadu_si512(b)`. Bytes are unsigned in `block`.
2. Compute the unsigned-side correction. VPDPBUSD reads the *first*
   operand as unsigned, so we want to dot `block` (unsigned u8) against
   a signed-byte constant pattern. To honour the upstream `schar`
   interpretation we use the identity
   `schar(b) = u8(b) - 256 * (b >= 128)` and pre-compute a one-time
   correction from the high-bit count of each block. Concretely:

       s1_acc = vpdpbusd(s1_acc, block, ones_i8)

   yields `sum_{i} u8(b[i])`, then we subtract `256 * popcount(b[i] >= 128)`
   from the scalar tail to recover the signed-byte sum. Two cheap
   alternatives - in priority order to be benchmarked:

   - Flip the byte's top bit with `_mm512_xor_si512(block, _mm512_set1_epi8(0x80u8 as i8))`
     so the unsigned interpretation matches the signed one, then offset
     the accumulator by `64 * 128` per block at the tail.
   - Use VPDPBSSD on Zen 5 / Granite Rapids when both operands are
     signed; this is *not* on the baseline VNNI feature flag and would
     require a separate detection bit, so it is out of scope for the
     first cut.

3. Compute the weighted-prefix sum:

       s2_acc = vpdpbusd(s2_acc, block, weights_i8)

   where `weights_i8` is a precomputed `__m512i` packed with the
   descending ramp `[64, 63, 62, ..., 1]` cast to `i8`. Because `i8`
   tops out at 127, the full descending ramp fits without saturation.

4. Add the carry term `64 * s1` to `s2_acc` once per 64-byte block, the
   same way the AVX2 path adds `s1.wrapping_mul(AVX2_BLOCK_LEN as u32)`
   per iteration (`x86.rs:212`).

5. Reduce `s1_acc` and `s2_acc` to `u32` with
   `_mm512_reduce_add_epi32` at the end of the bulk loop. The reduction
   is one instruction on Sapphire Rapids and is not on the per-block
   hot path.

### 4.3 Tail handling

For chunks shorter than 64 bytes, fall through to AVX2 (32 bytes), then
SSE2 (16 bytes), then scalar - exactly as `accumulate_chunk_avx2`
delegates today (`x86.rs:218-228`). The dispatcher contract requires
each layer to consume its multiple of the SIMD width and pass the
remainder down.

### 4.4 Overflow analysis

The scalar reference accumulates into `u32` and only masks on the
public boundary. With 64-byte blocks, `s2_acc` lanes grow at most by
`64 * 127 = 8128` per block, so `i32` lanes will not overflow within
any reasonable chunk size. `_mm512_reduce_add_epi32` returns `i32`,
which we re-interpret as `u32` for the wrapping arithmetic, mirroring
the existing AVX2 reduction in `sum_block_avx2` (`x86.rs:233-254`).

### 4.5 Detection

The path is gated on three independent features:

    is_x86_feature_detected!("avx512f")   // 512-bit register file
    && is_x86_feature_detected!("avx512bw") // byte/word lanes (zmm)
    && is_x86_feature_detected!("avx512vnni") // vpdpbusd

`avx512vnni` alone is not sufficient: the spec allows VNNI as a
separate-feature CPU even when AVX-512F is absent (see Section 7.3 on
AVX2 VNNI). The detection is cached in the same `OnceLock` as the
existing AVX2/SSE2 features, extended to a four-field `FeatureLevel`.

## 5. Integration points

- **New file** `crates/checksums/src/rolling/checksum/x86_avx512_vnni.rs`
  containing `accumulate_chunk_avx512_vnni`,
  `accumulate_chunk_avx512_vnni_for_tests`, and the per-block helpers.
  Naming mirrors the existing `accumulate_chunk_avx2` /
  `accumulate_chunk_avx2_for_tests` pair in `x86.rs`.

- **Dispatcher edit** in
  `crates/checksums/src/rolling/checksum/x86.rs:106` adds a new arm at
  the top of `try_accumulate_chunk`:

      if chunk.len() >= AVX512_BLOCK_LEN && features.avx512_vnni {
          return Some(unsafe { accumulate_chunk_avx512_vnni(s1, s2, len, chunk) });
      }

  before the AVX2 arm. `AVX512_BLOCK_LEN = 64`.

- **Feature record** in the same file extends `FeatureLevel` with
  `avx512_vnni: bool` and the cache initialiser `cpu_features()` adds
  the three `is_x86_feature_detected!` calls. `effective_features()`
  intersects against
  `feature_allowed(SimdFeature::Avx512)` from
  `crates/checksums/src/cpu_features.rs`, so the existing `--simd=avx2`
  override (#1825) automatically forces the new path off.

- **Public reporter** `simd_acceleration_available()`
  (`mod.rs:46`) keeps its boolean shape; the version banner already
  surfaces "SIMD: yes" without naming the level.

- **Parity test** in `crates/checksums/src/simd_parity_tests.rs` adds
  `accumulate_chunk_avx512_vnni_for_tests` to the existing fan-out so
  every input length covered for AVX2 is also covered here.

## 6. Trade-offs

- **Narrower CPU population.** AVX-512-VNNI requires both `avx512bw`
  and `avx512vnni`. Detection is mandatory; on Skylake-X (no VNNI) and
  on every pre-Cascade Lake server CPU the path stays inactive and the
  AVX2 path is selected.
- **Frequency throttling.** Older AVX-512 implementations (Skylake-X)
  drop core frequency under heavy zmm pressure. Cascade Lake onwards
  closed most of that gap, and Sapphire Rapids / Ice Lake server are
  largely throttle-free for short bursts. Mitigation: gate the path
  the same way #1763 gated AVX-512 MD4/MD5, and report measurements
  per microarchitecture in the criterion bench.
- **Misalignment cost.** The rolling-checksum slide is not naturally
  64-byte aligned: it advances one byte at a time during
  `hash_search`. The bulk accumulator is only used for *block*
  signature emission and for the receiver's basis-file scan, both of
  which are aligned-by-construction (block boundaries). The single-
  byte slide path stays in `update_byte` (`mod.rs:191-198`) and does
  not enter SIMD.
- **Code-size / icache.** Adding a third x86 SIMD path widens the
  rolling-checksum object file. Measured impact on the AVX2 path is
  marginal because the new code lives in a separate function; the
  hot AVX2 path is unchanged.
- **Maintenance burden.** Three SIMD paths (AVX-512-VNNI, AVX2, SSE2)
  must stay in lockstep with the scalar reference. The parity harness
  in `crates/checksums/src/simd_parity_tests.rs` is the contract.

## 7. Implementation phases

### Phase 1 - VPDPBUSD-based bulk accumulator

Land `accumulate_chunk_avx512_vnni` and the helper kernels in the new
module. Wire `cpu_features()` to populate `avx512_vnni`. No dispatcher
change yet; the function is reachable only through the
`*_for_tests` shim and the parity harness.

### Phase 2 - Dispatcher integration

Add the AVX-512-VNNI arm to `try_accumulate_chunk` ahead of AVX2. Wire
`SimdFeature::Avx512` through `effective_features()` so `--simd=avx2`
forces the new path off. End-to-end interop tests run unchanged (the
output is byte-identical).

### Phase 3 - Parity tests + criterion bench

Extend `crates/checksums/src/simd_parity_tests.rs` to cover the new
function across:

- aligned and 1..63-offset inputs,
- power-of-two and odd lengths from 1 to 4 KiB,
- adversarial byte patterns (all-zero, all-0xFF, sign-flip
  alternation) chosen to surface VPDPBUSD signedness mistakes.

Add a criterion bench `crates/checksums/benches/rolling_avx512.rs`
mirroring the existing rolling-checksum bench. Report numbers per
microarchitecture: Cascade Lake, Ice Lake, Tiger Lake, Sapphire
Rapids, Zen 4.

### Phase 4 - SIMD fuzz target

Companion to #2103: add an `arbitrary`-driven differential fuzzer that
generates random byte vectors and asserts the AVX-512-VNNI accumulator
matches `accumulate_chunk_scalar_raw` bit-for-bit. The fuzzer should
also flip the live `SimdFeature` override mid-run to exercise the
path-switch boundary.

## 8. Open questions

- **64 bytes vs 128 bytes per loop.** Two zmm registers can be
  processed in parallel to hide the ~5-cycle dependency chain on
  `vpdpbusd`'s accumulator. Phase 3 benchmarks should compare the
  two unroll factors before committing to a window size in the
  dispatcher header.
- **Shared OnceLock with #1761 (CPUID detection).** The cpu_features
  module already has a unified `SimdFeature` enum; the rolling-
  checksum subsystem has its own `FeatureLevel`. #1761 proposes a
  single CPUID table for the whole crate. The decision is whether
  this design pre-empts that consolidation or feeds into it. The
  current proposal extends the local `FeatureLevel` and leaves the
  consolidation to #1761.
- **AVX2-VNNI (`vex.vnni`).** A subset of CPUs (Alder Lake E-cores in
  hybrid mode, Intel N-series) expose VPDPBUSD on ymm without the rest
  of AVX-512. Adding a fourth path widens hardware coverage but
  duplicates the bulk-accumulator logic at 32 bytes per iteration.
  Defer to a follow-up issue if and only if Phase 3 benchmarks show a
  meaningful win on representative E-core hardware.
- **VPDPBSSD on Zen 5 / Granite Rapids.** The signed-by-signed variant
  removes the unsigned-correction step from Section 4.2. It is gated on
  a separate `avx-vnni-int8` feature bit. Out of scope for the first
  cut; revisit when CPU population justifies a fifth detection arm.
- **Interaction with `--checksum-choice=xxh3`.** Strong-checksum choice
  is independent of rolling checksum; `xxh3` does not change the
  rolling-sum calling convention. Confirmed orthogonal, listed only to
  pre-empt the question.

## 9. Verification

Before this design ships as code, the implementer must:

1. Read `target/interop/upstream-src/rsync-3.4.1/checksum.c`
   `get_checksum1()` end-to-end to confirm the wrap-and-mask discipline
   matches the scalar reference in `mod.rs:561-606`. If the local
   upstream tree is missing, fetch it per the project README before
   landing Phase 1.
2. Run the existing `rolling::tests::checksum::simd` parity sweep with
   the new path enabled on at least one VNNI-capable CPU (Sapphire
   Rapids preferred) and one AVX2-only CPU (Skylake or Haswell) to
   confirm both dispatch arms.
3. Run the interop harness `tools/ci/run_interop.sh` against upstream
   3.0.9, 3.1.3, and 3.4.1 to confirm wire output is byte-identical
   with and without the new path engaged.

## 10. Out of scope

- Strong-checksum SIMD (covered by `docs/design/avx512-md4-md5-batch.md`).
- aarch64 SVE rolling-checksum work (covered by
  `docs/design/sve-rolling-checksum-aarch64.md`).
- A consolidated CPUID table across crates (#1761).
- AVX2-VNNI and VPDPBSSD follow-ups (Section 8).
