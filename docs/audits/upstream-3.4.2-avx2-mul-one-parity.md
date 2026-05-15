# Upstream 3.4.2 parity: AVX2 `get_checksum1` `mul_one` initialization

Tracking issue: #2222. Verified 2026-05-15 against `origin/master`.

## 1. Upstream change

Upstream rsync 3.4.2 fixed an uninitialized-vector bug in the AVX2 fast path
for `get_checksum1`. The 3.4.1 source declared `__m256i mul_one;` and then
used the (still-uninitialized) register as both operands of
`_mm256_cmpeq_epi16(mul_one, mul_one)` to derive an all-ones lane mask, which
it then folded with `_mm256_abs_epi8` to obtain `[1, 1, ..., 1]`. Reading
`mul_one` before any write is undefined behaviour; modern compilers are free
to leave the register holding stale or poisoned bits, producing a wrong
checksum on the first iteration.

The 3.4.2 fix replaces the self-compare trick with a single explicit
broadcast:

```cpp
// upstream: simd-checksum-x86_64.cpp get_checksum1_avx2_64() @ line 350
__m256i mul_one = _mm256_set1_epi8(1);
```

Diff window (3.4.1 -> 3.4.2 in
`target/interop/upstream-src/rsync-3.4.2/simd-checksum-x86_64.cpp`):

```
-        __m256i mul_one;
-            mul_one = _mm256_abs_epi8(_mm256_cmpeq_epi16(mul_one,mul_one));
+        __m256i mul_one = _mm256_set1_epi8(1);
```

3.4.2 also adds a `TEST_SIMD_CHECKSUM1` harness in the same file that
cross-checks `default_1` against SSE2, SSSE3, and AVX2 over a matrix of
aligned/unaligned buffers and assorted sizes.

## 2. oc-rsync AVX2 site: already at parity

oc-rsync's AVX2 accumulator is a from-scratch Rust port; it does not mirror
the upstream `mul_one` self-compare construct. Every `__m256i` register used
inside the hot loop is produced by an explicit initializer before its first
read.

File: `crates/checksums/src/rolling/checksum/x86.rs`

Loop preamble (lines 197-203):

```rust
unsafe fn accumulate_chunk_avx2(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    mut chunk: &[u8],
) -> (u32, u32, usize) {
    let ones = _mm256_set1_epi16(1);
    let first_half_weights = _mm256_set_epi16(
        17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    );
    let second_half_weights =
        _mm256_set_epi16(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);
```

`ones` is the analogue of upstream's `mul_one` (a vector of ones used as the
right operand of `_mm256_madd_epi16` to widen-and-sum i16 lanes into i32).
It is materialized via `_mm256_set1_epi16(1)` - the same defensive pattern
3.4.2 adopted, just one lane width up. There is no path on which the
register is read before this initializer executes.

The block-load (`_mm256_loadu_si256`) and per-iteration intermediates
(`block_sum`, `block_prefix`) are all produced by initializers immediately
before use. No `MaybeUninit`, `mem::zeroed`, or `assume_init` constructs
appear anywhere in the checksums crate.

## 3. Other SIMD initialization sites reviewed

The audit also covered every SIMD source file that declares `__m256i` /
`__m128i` / `__m512i` / `uint8x16_t` locals, to confirm none replicate the
self-compare anti-pattern:

| File | Verdict | Rationale |
| --- | --- | --- |
| `crates/checksums/src/rolling/checksum/x86.rs` | SAFE | All `__m256i` / `__m128i` produced by `_mm256_set*` / `_mm_set*` / loads before first read. |
| `crates/checksums/src/rolling/checksum/neon.rs` | SAFE | Aarch64 NEON path; vectors built via `vdupq_n_*` / `vld1q_*`. |
| `crates/checksums/src/simd_batch/md4/simd/avx2.rs` | SAFE | MD4 state/message constants set via `_mm256_set1_epi32` / `_mm256_loadu_si256`. |
| `crates/checksums/src/simd_batch/md5_simd/avx2.rs` | SAFE | MD5 lane state initialized from scalar IVs broadcast through `_mm256_set1_epi32`. |
| `crates/checksums/src/simd_batch/md5_simd/avx512.rs` | SAFE | Same pattern as the AVX2 lane-parallel MD5 implementation. |

A workspace-wide grep for `MaybeUninit`, `mem::zeroed`, and `assume_init`
inside `crates/checksums/` returns zero hits, confirming no Rust port of the
upstream bug.

## 4. Test coverage

Parity tests already lock the AVX2 path to the scalar reference and would
catch any regression equivalent to the 3.4.1 bug (which would manifest as a
wrong checksum on the first 64-byte block):

- `crates/checksums/src/rolling/tests/checksum/simd.rs::avx2_accumulate_matches_scalar_reference`
  - Iterates sizes `{32, 33, 47, 64, 95, 128, 1024, 4096}` against four
    `(s1, s2, len)` seed triples and compares `accumulate_chunk_avx2`
    against the scalar reference byte-for-byte.
- `crates/checksums/src/simd_parity_tests.rs` - lane-parallel MD4/MD5 SIMD
  parity, exercising AVX2/AVX-512 against the scalar implementations.

No new test is required: the upstream bug, if mirrored, would produce a
mismatch on the very first AVX2 iteration (32-byte chunk), which the
existing harness already exercises.

## 5. Conclusion

oc-rsync's AVX2 `get_checksum1` accumulator does not reproduce the
3.4.1 uninitialized-`mul_one` bug. The equivalent constant (`ones`) is
materialized via `_mm256_set1_epi16(1)` before any read, matching the
spirit of the 3.4.2 fix (`_mm256_set1_epi8(1)`). No production code change
is required and existing parity tests guard against future regression.
