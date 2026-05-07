# MD4 / MD5 SIMD dispatch coverage matrix

Tracking issue: #1762.

This audit enumerates every MD4 and MD5 implementation that ships in
`crates/checksums/`, traces the runtime dispatch ladder for each digest, and
flags the SIMD tiers that are missing relative to the desired ladder
(AVX-512 -> AVX2 -> SSE4.1 -> SSSE3 -> SSE2 -> NEON -> scalar).

Last verified: 2026-05-07 against master @ `60e83fd96`. Sources cross-checked:

- `crates/checksums/src/strong/md4.rs` - streaming MD4 wrapper.
- `crates/checksums/src/strong/md5.rs` - streaming MD5 wrapper.
- `crates/checksums/src/strong/openssl_support.rs` - OpenSSL-backed hashers.
- `crates/checksums/src/simd_batch/md5_dispatcher.rs` - MD5 batch dispatcher.
- `crates/checksums/src/simd_batch/md5_simd/mod.rs` - MD5 SIMD backends.
- `crates/checksums/src/simd_batch/md4/mod.rs` - MD4 batch dispatcher.
- `crates/checksums/src/simd_batch/md4/simd/mod.rs` - MD4 SIMD backends.
- `crates/checksums/src/cpu_features.rs` - runtime SIMD-level override and
  `feature_allowed` gate consulted before CPUID detection.

Desired ladder (per task #1762):

```
AVX-512 -> AVX2 -> SSE4.1 -> SSSE3 -> SSE2 -> NEON -> scalar
```

## 1. MD4 implementation inventory

| Path | Symbol(s) | Description |
|------|-----------|-------------|
| `crates/checksums/src/strong/md4.rs` | `Md4`, `Md4Backend::OpenSsl`, `Md4Backend::Rust`, `Md4::digest`, `Md4::digest_with_seed`, `digest_batch` | Streaming MD4 hasher used by protocol < 30. Backend chosen at construction: OpenSSL (when the `openssl` feature is enabled and `MessageDigest::from_name("md4")` resolves) or pure-Rust `md4::Md4`. The free `digest_batch` re-exports the batch dispatcher. |
| `crates/checksums/src/strong/openssl_support.rs` | `new_md4_hasher` | Factory wrapping `openssl::hash::Hasher` with `MessageDigest::from_name("md4")`. Returns `None` when OpenSSL was built without the legacy provider. |
| `crates/checksums/src/simd_batch/md4/scalar.rs` | `digest` | Pure-Rust scalar reference (RFC 1320). Used by every fallback path and as parity oracle. |
| `crates/checksums/src/simd_batch/md4/simd/sse2.rs` | `digest_x4` | x86_64 SSE2 4-lane batch backend. |
| `crates/checksums/src/simd_batch/md4/simd/avx2.rs` | `digest_x8` | x86_64 AVX2 8-lane batch backend. |
| `crates/checksums/src/simd_batch/md4/simd/avx512.rs` | `digest_x16` | x86_64 AVX-512 (F + BW) 16-lane batch backend. |
| `crates/checksums/src/simd_batch/md4/simd/neon.rs` | `digest_x4` | aarch64 NEON 4-lane batch backend. |
| `crates/checksums/src/simd_batch/md4/simd/wasm.rs` | `digest_x4` | wasm32 SIMD-128 4-lane batch backend. |
| `crates/checksums/src/simd_batch/md4/mod.rs` | `Md4Dispatcher`, `digest_batch`, `digest`, `md4_dispatcher` | Runtime dispatcher; selects backend on first use via `OnceLock`. |

## 2. MD5 implementation inventory

| Path | Symbol(s) | Description |
|------|-----------|-------------|
| `crates/checksums/src/strong/md5.rs` | `Md5`, `Md5Seed`, `Md5Backend::OpenSsl`, `Md5Backend::Rust`, `Md5::digest`, `digest_batch` | Streaming MD5 hasher with seeded modes (`proper` for protocol 30+ and `legacy` for older ordering). Backend chosen at construction: OpenSSL (when `openssl` feature is enabled and detection succeeds) or pure-Rust `md5::Md5`. |
| `crates/checksums/src/strong/openssl_support.rs` | `new_md5_hasher`, `openssl_acceleration_available` | Factory wrapping `openssl::hash::Hasher` with `MessageDigest::md5()` plus a `OnceLock`-cached availability probe. |
| `crates/checksums/src/simd_batch/md5_scalar.rs` | `digest` | Pure-Rust scalar reference (RFC 1321). Used as parity oracle and single-input convenience path. |
| `crates/checksums/src/simd_batch/md5_simd/sse2.rs` | `digest_x4` | x86_64 SSE2 4-lane batch backend (baseline x86_64). |
| `crates/checksums/src/simd_batch/md5_simd/ssse3.rs` | `digest_x4` | x86_64 SSSE3 4-lane batch backend (uses `pshufb`). |
| `crates/checksums/src/simd_batch/md5_simd/sse41.rs` | `digest_x4` | x86_64 SSE4.1 4-lane batch backend (uses `blendv`). |
| `crates/checksums/src/simd_batch/md5_simd/avx2.rs` | `digest_x8` | x86_64 AVX2 8-lane batch backend. |
| `crates/checksums/src/simd_batch/md5_simd/avx512.rs` | `digest_x16` | x86_64 AVX-512 (F + BW) 16-lane batch backend. |
| `crates/checksums/src/simd_batch/md5_simd/neon.rs` | `digest_x4` | aarch64 NEON 4-lane batch backend. |
| `crates/checksums/src/simd_batch/md5_simd/wasm.rs` | `digest_x4` | wasm32 SIMD-128 4-lane batch backend. |
| `crates/checksums/src/simd_batch/md5_dispatcher.rs` | `Backend`, `Dispatcher`, `global` | Runtime dispatcher for MD5 batch hashing; selected on first use via `OnceLock`. |

## 3. Actual runtime dispatch order

### 3.1 Streaming `Md4` and `Md5` (`crates/checksums/src/strong/{md4,md5}.rs`)

The streaming hashers do not consult CPUID. The backend is decided in
`Md4Backend::new` / `Md5Backend::new`:

1. `#[cfg(feature = "openssl")]` -> call `openssl_support::new_md4_hasher`
   (resp. `new_md5_hasher`). Detection is gated by
   `openssl_acceleration_available`, which performs a one-shot probe in
   `OnceLock` and falls through if the OpenSSL build lacks the digest.
2. Fall back to the pure-Rust `md4::Md4` / `md5::Md5` crate. The pure-Rust
   crates use scalar code paths only.

No SIMD ladder exists for the streaming path. The streaming hashers are the
ones used for whole-file checksums (`checksum.c:file_checksum()` upstream)
and for `get_checksum2()` block strong checksums when the dispatcher is not
involved.

### 3.2 MD4 batch dispatcher (`crates/checksums/src/simd_batch/md4/mod.rs::detect_backend`)

```
1. has_avx512()  -> Backend::Avx512  (x86_64 + avx512f + avx512bw)
2. has_avx2()    -> Backend::Avx2    (x86_64 + avx2)
3. has_sse2()    -> Backend::Sse2    (x86_64; SSE2 is baseline)
4. has_neon()    -> Backend::Neon    (aarch64; mandatory)
5. has_wasm_simd() -> Backend::Wasm  (wasm32 + simd128)
6. Backend::Scalar
```

The MD4 dispatcher reuses the `Backend` enum from the MD5 dispatcher.
`Backend::Sse41` and `Backend::Ssse3` are unreachable through `detect_backend`,
but the `digest_batch` match arm `Backend::Sse41 | Backend::Ssse3 | Backend::Sse2`
maps all three to `digest_batch_sse2`. The MD4 dispatcher also does **not**
consult `cpu_features::feature_allowed`, so the `--simd` CLI override does
not constrain MD4 batch dispatch the way it constrains MD5 dispatch.

### 3.3 MD5 batch dispatcher (`crates/checksums/src/simd_batch/md5_dispatcher.rs::detect_backend`)

```
1. has_avx512() -> Backend::Avx512  (feature_allowed(Avx512) && x86_64 + avx512f + avx512bw)
2. has_avx2()   -> Backend::Avx2    (feature_allowed(Avx2)   && x86_64 + avx2)
3. has_sse41()  -> Backend::Sse41   (feature_allowed(Sse41)  && x86_64 + sse4.1)
4. has_ssse3()  -> Backend::Ssse3   (feature_allowed(Ssse3)  && x86_64 + ssse3)
5. has_sse2()   -> Backend::Sse2    (feature_allowed(Sse2)   && x86_64 baseline)
6. has_neon()   -> Backend::Neon    (feature_allowed(Neon)   && aarch64 mandatory)
7. has_wasm_simd() -> Backend::Wasm (wasm32 + simd128)
8. Backend::Scalar
```

Each tier first consults `cpu_features::feature_allowed` so the runtime
override (`--simd=auto|avx512|avx2|sse4|neon|none`) can pin dispatch below
what the host advertises. Note that `Backend::Wasm` is **not** gated by
`feature_allowed`; the override has no effect on the wasm path today.

## 4. Coverage gap matrix

Y = present. - = absent.

| Tier | MD5 batch dispatcher | MD4 batch dispatcher | Streaming `Md5` | Streaming `Md4` |
|------|:-------------------:|:--------------------:|:---------------:|:---------------:|
| AVX-512 (F + BW) | Y | Y | -                  | -                  |
| AVX2             | Y | Y | -                  | -                  |
| SSE4.1           | Y | - (folded into SSE2) | -            | -                  |
| SSSE3            | Y | - (folded into SSE2) | -            | -                  |
| SSE2             | Y | Y | -                  | -                  |
| NEON             | Y | Y | -                  | -                  |
| Scalar           | Y | Y | Y (pure-Rust `md5`) | Y (pure-Rust `md4`) |
| OpenSSL (off-ladder) | -               | -                    | Y (when feature) | Y (when feature) |
| WASM SIMD (off-ladder) | Y             | Y                    | -                  | -                  |

Findings:

- The MD5 batch dispatcher is the only path that satisfies the desired
  AVX-512 -> AVX2 -> SSE4.1 -> SSSE3 -> SSE2 -> NEON -> scalar ladder.
- The MD4 batch dispatcher collapses SSE4.1 and SSSE3 into the SSE2 backend,
  so any callers running on Nehalem-class or newer x86_64 hardware never see
  a `pshufb` / `blendv` lane optimisation for MD4. The dispatcher also
  ignores the `cpu_features::feature_allowed` override, breaking
  `--simd=sse4` parity with MD5.
- The streaming `Md4` / `Md5` hashers expose only OpenSSL or scalar
  pure-Rust. Whole-file MD5 (e.g. `--checksum`, `file_checksum()` parity)
  and protocol-level seeded MD5 / MD4 hashing run through the streaming
  path and therefore receive no in-tree SIMD acceleration. OpenSSL is the
  only acceleration available, and only when the build links against a
  legacy-provider-enabled OpenSSL.

## 5. Recommended follow-up tasks

| ID | Title | Goal |
|----|-------|------|
| md4-sse41-lane | Add SSE4.1 (`blendv`) MD4 batch backend | Mirror the MD5 SSE4.1 backend in `simd_batch/md4/simd/sse41.rs`, register `Backend::Sse41` in `Md4Dispatcher::detect_backend`, and dispatch it explicitly so the SSE4.1 tier is exercised on Nehalem-class hosts. |
| md4-ssse3-lane | Add SSSE3 (`pshufb`) MD4 batch backend | Add `simd_batch/md4/simd/ssse3.rs` mirroring the MD5 SSSE3 path, register `Backend::Ssse3` in MD4 detection, and split the existing `Sse41 \| Ssse3 \| Sse2` arm so each tier dispatches to its own backend. |
| md4-feature-allowed-gate | Honour `feature_allowed` in MD4 dispatch | Wire `cpu_features::feature_allowed(SimdFeature::*)` into every `Md4Dispatcher::has_*` probe so the `--simd` CLI override caps MD4 dispatch identically to MD5. |
| md5-streaming-batch-shim | SIMD-accelerate streaming `Md5` whole-file hashing | Route long-lived `Md5` updates through a chunked batch shim (or a single-lane SIMD fast path) when no seed is pending, so `file_checksum()` parity workloads see the same throughput as `md5_digest_batch`. |
| md4-streaming-batch-shim | SIMD-accelerate streaming `Md4` whole-file hashing | Same as above for `Md4`, used by protocol < 30 strong checksums and seeded MD4 paths. |
| md5-md4-wasm-override | Gate WASM SIMD by `feature_allowed` | Add a `SimdFeature::WasmSimd` (or reuse an existing tier) so `set_simd_override(SimdLevel::None)` also disables the wasm32 backends in both dispatchers. |
| dispatch-coverage-test | Add a parity test that pins each tier | For every backend (AVX-512, AVX2, SSE4.1, SSSE3, SSE2, NEON, Scalar) call `reset_simd_override_for_tests` to force the level, run a fixed RFC vector batch, and assert the resulting `Backend` matches the override. Catches future regressions where MD4 silently folds tiers. |
