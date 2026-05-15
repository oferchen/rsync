# Upstream 3.4.2 parity: `get_checksum2()` MD4 `buf1` initialization

Tracking issue: #2123. Verified 2026-05-15 against `origin/master`.

## 1. Upstream change

`rsync-3.4.2/NEWS.md`:

> Fixed an uninitialized `buf1` on the first call to `get_checksum2()`
> in the MD4 path (fixes #673).

The patch (`checksum.c:get_checksum2`, MD4 branch) is a one-line
condition change at the `buf1` allocation guard:

- `rsync-3.4.1/checksum.c:369`:

  ```c
  if (len > len1) {
      if (buf1)
          free(buf1);
      buf1 = new_array(char, len+4);
      len1 = len;
  }
  ```

- `rsync-3.4.2/checksum.c:369`:

  ```c
  if (len > len1 || !buf1) {
      free(buf1);
      buf1 = new_array(char, len+4);
      len1 = len;
  }
  ```

`buf1` / `len1` are file-static. On the very first call, both are zero
(`NULL` / `0`). If the first call has `len == 0` (an empty trailing
block, e.g. a zero-length file matched against a zero-length basis) the
original `len > len1` test is false, allocation is skipped, and `buf1`
stays `NULL`. The follow-up `memcpy(buf1, buf, 0)` is undefined behaviour
per C11 7.24.1p2 even with a zero count, and the subsequent
`SIVAL(buf1, 0, checksum_seed)` (when `checksum_seed != 0`) writes
through a NULL pointer.

The fix forces an allocation whenever `buf1` is `NULL`, regardless of
the cached `len1`.

## 2. oc-rsync surface area

oc-rsync does not maintain a static heap-backed `buf1` accumulator.
The MD4 path streams the input slice and the optional 4-byte seed
directly into the digest backend, with no intermediate scratch buffer.

### 2.1 Strong-checksum entry point

- `crates/checksums/src/strong/md4.rs:169-178` -
  `Md4::digest_with_seed(seed, data)`:

  ```rust
  pub fn digest_with_seed(seed: i32, data: &[u8]) -> [u8; 16] {
      let mut hasher = Md4::new();
      hasher.update(data);
      // upstream: checksum.c:377-380 - SIVAL(buf1, len, checksum_seed) appends
      // the seed as a 32-bit little-endian value when seed != 0.
      if seed != 0 {
          hasher.update(&seed.to_le_bytes());
      }
      hasher.finalize()
  }
  ```

  The first `update(data)` is a no-op when `data.is_empty()`; the seed
  append still feeds 4 well-defined bytes into the digest. There is no
  pointer that can be `NULL`, no static cache, and no
  `MaybeUninit::uninit()` scratch.

### 2.2 Strategy dispatch (protocol < 30)

- `crates/checksums/src/strong/strategy/impls.rs:23-35` -
  `Md4Strategy::compute` calls `Md4::digest(data)` directly.
- `crates/signature/src/algorithm.rs:143-149` -
  signature generation routes seeded blocks through
  `Md4::digest_with_seed(seed, data)`.

### 2.3 Parallel/file-level hashing

- `crates/checksums/src/parallel/files.rs:232,239` -
  `D::digest_with_seed(seed, &data)` for in-memory and mmap paths.
- `crates/checksums/src/parallel/files.rs:244-253` -
  streaming fallback uses `D::with_seed(seed)` plus `hasher.update`
  over a `vec![0u8; buffer_size]` read buffer (zero-initialized).
- `crates/checksums/src/parallel/blocks.rs:146` -
  per-block hashing via `D::digest_with_seed`.

### 2.4 SIMD batch MD4

`crates/checksums/src/simd_batch/md4/{scalar.rs,simd/{avx2,avx512,neon,sse2,wasm}.rs}`
implement RFC 1320 padding for batch MD4. Every scratch buffer is
zero-initialized at construction:

| Site | Init pattern |
|------|--------------|
| `scalar.rs:50` | `let mut padded = [0u8; 128];` |
| `simd/avx2.rs:76` | `vec![0u8; individual_padded_len.max(64)]` |
| `simd/avx512.rs:69` | `vec![0u8; padded_len.max(64)]` |
| `simd/neon.rs` | `vec![0u8; ...]` |
| `simd/sse2.rs` | `vec![0u8; ...]` |
| `simd/wasm.rs` | `vec![0u8; ...]` |

The trailing `0x80` marker and 64-bit length suffix are written into
already-zeroed memory, so every byte fed into the MD4 compression
function has a defined value. No `MaybeUninit` is used anywhere under
`crates/checksums/src/` (`grep -rn 'MaybeUninit' crates/checksums/src/`
returns no hits).

## 3. Verdict per audited site

| Site | Equivalent of upstream `buf1`? | Zero-init guarantee | Verdict |
|------|--------------------------------|---------------------|---------|
| `Md4::digest_with_seed` | No scratch; streamed | N/A (slice passed by reference) | At parity |
| `Md4Strategy::compute` | Delegates to `Md4::digest` | N/A | At parity |
| `parallel::files` mmap/in-mem | Slice or `Vec` passed in full | N/A | At parity |
| `parallel::files` streaming | `vec![0u8; buffer_size]` | Zero-initialized | At parity |
| `parallel::blocks` | Slice borrowed | N/A | At parity |
| `simd_batch::md4::scalar` | `[0u8; 128]` padding | Zero-initialized | At parity |
| `simd_batch::md4::simd::*` | `vec![0u8; padded_len]` | Zero-initialized | At parity |

oc-rsync has no equivalent of the static `buf1` cache, so the 3.4.1
regression cannot reproduce. The "first call with empty data and a
non-zero seed" preconditions land on a streaming digester whose state
is fully initialized by `Md4::new()`.

## 4. Regression coverage

The empty-input, non-zero-seed case (the exact trigger that walks
through `memcpy(NULL, _, 0)` and `SIVAL(NULL, ...)` upstream) is locked
in by:

- `crates/checksums/src/strong/md4.rs::tests::md4_seeded_empty_input_matches_seed_only_digest`
  (new) - asserts that `Md4::digest_with_seed(seed, b"")` equals
  `Md4::digest(&seed.to_le_bytes())` for a non-zero seed, matching the
  upstream contract `memcpy(buf1, buf, 0); SIVAL(buf1, 0, seed)`.

Existing coverage retained:

- `md4_seeded_appends_seed_after_data` - non-empty buffer + non-zero
  seed.
- `md4_seeded_zero_seed_matches_unseeded` - zero seed short-circuit.
- `md4_seeded_negative_seed_is_le_two_complement` - signed seed wire
  encoding.
- `md4_empty_input_one_shot` / `md4_empty_input_streaming` (in
  `md4_tests.rs`) - empty input, no seed.

## 5. Conclusion

No production change required. The upstream 3.4.2 fix targets a
C-specific static-buffer lifecycle bug that has no analogue in the
Rust port. A regression test guards the exact failure mode (empty
input + non-zero seed) so future refactors cannot regress past the
upstream-3.4.2 contract.
