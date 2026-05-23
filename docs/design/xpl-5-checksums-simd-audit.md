# XPL-5: `crates/checksums/` SIMD platform-gate consistency audit

Audits the checksums crate for SIMD `cfg`-gate consistency, runtime
feature detection caching, and `unsafe` SAFETY-block completeness.
Continues the XPL-2 (kqueue) / XPL-3 (transfer) audit pattern.

Hazard catalogue (SIMD-specific) drawn from
`feedback_proactive_cross_platform.md` and the "Cross-Platform
Compilation" + "Performance" sections of CLAUDE.md:

- `#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]` blocks
  with stale imports on aarch64.
- `#[cfg(target_arch = "aarch64")]` blocks with stale imports on x86.
- Missing scalar fallback for architectures with no SIMD path
  (riscv, wasm, others).
- Runtime feature detection caches that re-check on every call.
- SAFETY block completeness on every unsafe SIMD intrinsic call.
- SIMD vs scalar parity tests present for each algorithm.
- AVX2, SSE2, AVX-512 alternate paths all gated and tested.
- NEON path gated on aarch64.

## Scope

64 `.rs` files under `crates/checksums/src/`. SIMD surfaces are
concentrated in:

- `src/simd_batch/` - batch MD4/MD5 dispatchers plus per-backend
  implementations (AVX-512, AVX2, SSE4.1, SSSE3, SSE2, NEON, WASM,
  scalar).
- `src/rolling/checksum/` - rolling-checksum dispatcher with x86
  (AVX2/SSE2) and aarch64 (NEON) accelerators.
- `src/cpu_features.rs` - process-global SIMD-level override consulted
  by every dispatcher.
- `src/simd_self_test.rs` - runtime SIMD-vs-scalar parity self-test
  used by CLI diagnostics.
- `src/simd_parity_tests.rs` - in-crate parity tests for MD4/MD5 batch
  paths (56 `#[test]` functions).
- `src/strong/sha256.rs` - exposes a hardware-detection helper for the
  RustCrypto-backed SHA-256 path.
- `src/strong/xxhash.rs`, `src/crc32c.rs` - external crates handle
  SIMD detection internally.

## Inventory

`cfg` predicate counts in `crates/checksums/src/`:

| Predicate                                          | Count |
|----------------------------------------------------|------:|
| `cfg(target_arch = "x86_64")`                      |    62 |
| `cfg(target_arch = "aarch64")`                     |    23 |
| `cfg(target_arch = "wasm32" + simd128)`            |     8 |
| `cfg(any(target_arch = "x86", "x86_64"))`          |     9 |
| `cfg(any(... "x86_64", "aarch64"))` (and `not`)    |     9 |
| `cfg(not(any(... x86_64, aarch64)))` (scalar bail) |     2 |

Runtime-detection cache call sites:

| Cache                                              | Site                                           |
|----------------------------------------------------|------------------------------------------------|
| `OnceLock<FeatureLevel>` (avx2/sse2)               | `rolling/checksum/x86.rs:89`                   |
| `OnceLock<bool>` (neon)                            | `rolling/checksum/neon.rs:73`                  |
| `OnceLock<Dispatcher>` (MD5 batch)                 | `simd_batch/md5_dispatcher.rs:571`             |
| `OnceLock<Md4Dispatcher>` (MD4 batch)              | `simd_batch/md4/mod.rs:387`                    |
| `OnceLock<Result<(), ()>>` (openssl)               | `strong/openssl_support.rs:15`                 |
| `AtomicU8` (process-global SIMD override)          | `cpu_features.rs:126`                          |

`Cargo.toml` platform-conditional dependency tree:

- `cfg(unix)` -> `md-5`, `sha1`, `sha2` with `asm` feature (assembly
  fallbacks via `sha2-asm`; pure-Rust backend auto-detects SHA-NI
  and aarch64 crypto extensions via `cpufeatures` regardless of this
  feature).
- `cfg(not(unix))` -> same crates without `asm` (NASM toolchain
  unavailable under MSVC).
- Always-compiled: `md4`, `xxhash-rust`, `xxh3`, `crc32c`, `digest`,
  `rayon`, `thiserror`, `fast_io`, `logging`. The `xxh3` crate carries
  its own runtime CPU detection (AVX2 on x86_64, NEON on aarch64).

Unsafe surface (54 unsafe blocks total, 51 SAFETY comments):

- 14 files contain `unsafe`, all in SIMD intrinsic call paths.
- 51 of 54 unsafe blocks carry a `// SAFETY:` comment naming the CPU
  feature precondition.
- 3 unsafe blocks live in `#[cfg(test)]` test-helper functions and lack
  SAFETY comments; see C-3 below.

Parity test coverage:

- Rolling checksum: SSE2/AVX2/NEON parity tests gated by arch
  (`rolling/tests/checksum/simd.rs:5,42,80`) and a proptest sweep.
- MD5 batch: 19 `#[test]` functions in `simd_parity_tests::md5_simd_parity`
  cover RFC 1321 vectors, lane boundaries, partial batches, large
  inputs (100 KiB), random data, and active-backend probes.
- MD4 batch: 19 `#[test]` functions in `simd_parity_tests::md4_simd_parity`
  cover the equivalent surface against RFC 1320 vectors.
- Runtime self-test: `simd_self_test::run_simd_self_test()`
  cross-validates every dispatcher against an independent scalar
  reference (`Md4`/`Md5` from `strong`, byte-loop reference for
  rolling). Exercised by `simd_self_test::tests::self_test_passes_on_host`.

## Methodology

1. Read `Cargo.toml` to map the platform-conditional dependency tree.
2. Grep for every `target_arch`, `target_feature`, `is_x86_feature_detected!`,
   `is_aarch64_feature_detected!`, `unsafe`, and `OnceLock` reference
   in `src/`.
3. Walk every SIMD entry-point file (dispatchers + per-backend impls)
   end to end. Cross-check that `pub mod` declarations in parent
   `mod.rs` and `use` statements at the call site share matching
   gates.
4. Confirm each unsafe SIMD call has a SAFETY comment naming the CPU
   feature it relies on.
5. Confirm each algorithm has a scalar reference and a parity test
   gated to the relevant architecture.

## Findings

Hazard counts: **0 CI-fatal / 2 Warning / 24 Clean.**

SAFETY block completeness: **51 of 54 unsafe blocks documented = 94.4%**.
The 3 missing blocks are in `#[cfg(test)]` helper wrappers; see W-1.

### CI-fatal hazards

**None found.** Every SIMD path that uses architecture-specific
intrinsics or runtime feature macros is gated by a matching
`#[cfg(target_arch = ...)]` predicate and exposes a scalar fallback for
architectures outside the gate. The `Linux musl (stable)`, `Windows
(stable)`, `macOS (stable)` matrices traverse x86_64 plus aarch64 (Apple
Silicon and `arm64` Linux runners); both are tier-1 architectures and
hit the gated SIMD code paths. Tier-2/3 architectures (`riscv64`,
`powerpc64`, etc.) fall through to `accumulate_chunk_arch` returning
`None`, then to `accumulate_chunk_scalar_raw` (rolling) or
`Backend::Scalar` (batch MD4/MD5), with the SIMD modules omitted
entirely.

### Warning hazards (cheap-to-fix or document-only)

#### W-1. Three test-helper `unsafe { ... }` blocks lack SAFETY comments.

`crates/checksums/src/rolling/checksum/x86.rs:406`,
`crates/checksums/src/rolling/checksum/x86.rs:416`,
`crates/checksums/src/rolling/checksum/neon.rs:193`.

All three sites are inside `#[cfg(test)]` helper wrappers
(`accumulate_chunk_sse2_for_tests`, `accumulate_chunk_avx2_for_tests`,
`accumulate_chunk_neon_for_tests`) that exist solely so the parity
proptests can exercise the unsafe inner function. The SSE2/AVX2 helpers
are reached only from tests that already gate themselves on
`is_x86_feature_detected!`; the NEON helper short-circuits to scalar
unless `neon_available()` returns true.

Risk is purely cosmetic: a future contributor calling these helpers
from new test code without re-checking the feature gate could trigger
an illegal-instruction fault on a CPU lacking the relevant extension.
Adding three short SAFETY comments removes the inconsistency without
changing behaviour. Captured for the W-2 fix below in the same PR.

#### W-2. `md5_simd/mod.rs:31-32` declares `pub mod wasm` but the parent excludes wasm32.

`crates/checksums/src/simd_batch/md5_simd/mod.rs:31`:

```rust
#[cfg(target_arch = "wasm32")]
pub mod wasm;
```

The parent `crates/checksums/src/simd_batch/mod.rs:19` gates the
`md5_simd` module on `#[cfg(any(target_arch = "x86_64", target_arch =
"aarch64"))]`, so the `wasm` child declaration is unreachable. The
asymmetry with the MD4 sibling
(`crates/checksums/src/simd_batch/md4/mod.rs:34-39`, which includes
`wasm32` in the parent gate) means MD5 currently has no WASM SIMD
backend even though `md5_simd/wasm.rs` exists. The dispatcher silently
falls through to scalar on `wasm32` via `Backend::Wasm =>
Self::digest_batch_scalar(inputs)` (`md5_dispatcher.rs:304-307`), so
behaviour is correct - just suboptimal for WASM SIMD targets.

Not CI-fatal (every Tier-1 CI matrix entry compiles cleanly). Two
straightforward repair options:

1. Match the MD4 pattern and add `target_arch = "wasm32"` to the
   parent gate in `simd_batch/mod.rs:19`, exposing the existing WASM
   backend.
2. Delete `md5_simd/wasm.rs` and the unreachable child declaration so
   the WASM path is uniformly absent.

Choosing (1) preserves the existing implementation; choosing (2)
deletes dead code. Either keeps MD4 and MD5 symmetric. Out of scope
for this audit (cross-platform fix only; no Tier-1 CI matrix runs
wasm32). Captured in `CCL-26` for follow-up.

### Clean (correctly gated, no action needed)

The following files were inspected end to end and confirmed clean. Each
either pairs `#[cfg(target_arch = X)]` / `#[cfg(not(target_arch = X))]`
blocks with matching shapes, gates the entire module when every item
inside is architecture-specific, or routes through a `cfg`-aware shim
function with a scalar fallback for non-matching architectures.

- `Cargo.toml` - `md-5`/`sha1`/`sha2` `asm` feature only on `cfg(unix)`;
  pure-Rust backend handles SHA-NI/aarch64-crypto detection regardless.
- `lib.rs` - no platform-conditional code at the crate root; re-exports
  point at properly-gated submodules.
- `cpu_features.rs` - `AtomicU8` override is platform-neutral; mapping
  table `feature_allowed()` correctly returns `false` for an SSE override
  asking for Neon and vice-versa.
- `simd_batch/mod.rs` - `md5_simd` gated to x86_64+aarch64, `md4` always
  compiled (scalar fallback inside).
- `simd_batch/md5_dispatcher.rs` - every `Backend` arm pairs
  `#[cfg(target_arch = X)]` with `#[cfg(not(target_arch = X))]` falling
  back to scalar; `has_avx512`/`has_avx2`/`has_sse41`/`has_ssse3`/
  `has_sse2`/`has_neon`/`has_wasm_simd` all gated. Cached via
  `OnceLock<Dispatcher>` at `:571`. SAFETY comment on every unsafe call
  site.
- `simd_batch/md4/mod.rs` - parallels `md5_dispatcher.rs` with the same
  pattern. `Backend::Sse41 | Backend::Ssse3 | Backend::Sse2` collapses
  to a single SSE2 implementation per the module doc rationale.
  `OnceLock<Md4Dispatcher>` at `:387`.
- `simd_batch/md4/simd/mod.rs` - per-backend submodules each gated
  individually (`x86_64` for sse2/avx2/avx512, `aarch64` for neon,
  `wasm32` for wasm).
- `simd_batch/md4/simd/{sse2,avx2,neon,avx512,wasm}.rs` - each carries
  the matching `#[cfg(target_arch = X)]` on the intrinsic `use`
  statement, `#[target_feature(enable = X)]` on the unsafe entry
  point, a Safety section in the rustdoc naming the required feature,
  and SAFETY comments on every unsafe call site in the test module.
- `simd_batch/md5_simd/mod.rs` - x86_64 backends and aarch64 NEON
  correctly declared; see W-2 for the wasm asymmetry.
- `simd_batch/md5_simd/{sse2,ssse3,sse41,avx2,neon}.rs` - same shape
  as the MD4 SIMD impls; each unsafe entry point carries the matching
  `#[target_feature]` attribute, a Safety rustdoc section, and SAFETY
  comments on test call sites.
- `simd_batch/md5_simd/avx512.rs` - uses inline `asm!` rather than
  intrinsics (AVX-512 intrinsics are nightly-only), so the function
  is unsafe and gated by `#[cfg(target_arch = "x86_64")]`. A symmetric
  `#[cfg(not(target_arch = "x86_64"))]` stub at `:1114-1118` provides
  the scalar fallback. Tests at `:1136,1182,1245` skip with
  `eprintln!` when CPU lacks AVX-512F+AVX-512BW. SPL-31 left this
  file undecomposed (1292 LoC); the audit does not reopen that.
- `simd_batch/md5_scalar.rs` - reference implementation, no
  platform-conditional code.
- `rolling/checksum/mod.rs` - dispatcher uses three-way `cfg`:
  `aarch64` -> `neon::accumulate_chunk`, `x86`/`x86_64` ->
  `x86::try_accumulate_chunk`, else -> `None` falling to
  `accumulate_chunk_scalar_raw`. `simd_available_arch()` mirrors the
  same three-way gate and returns `false` for unsupported architectures
  so `simd_acceleration_available()` is well-defined everywhere.
- `rolling/checksum/x86.rs` - `use core::arch::x86::{...}` and
  `use core::arch::x86_64::{...}` each gated separately so the same
  intrinsic list works for both 32-bit and 64-bit x86. `FeatureLevel`
  cached in `OnceLock` at `:89`; `cpu_features()` returns the cached
  value, `effective_features()` intersects with the runtime override.
  Both unsafe call sites at `:126,131` carry SAFETY comments.
- `rolling/checksum/neon.rs` - aarch64-only module (gated at
  parent `:26`); `NEON_AVAILABLE: OnceLock<bool>` at `:73`;
  `neon_enabled()` AND-combines runtime detection with override.
  Unsafe call site at `:100` carries a SAFETY comment.
- `rolling/checksum/tests.rs:597` - `x86_cpu_feature_detection_is_cached`
  test gated to `x86`/`x86_64`; verifies `cpu_features_cached_for_tests()`
  returns true after `load_cpu_features_for_tests()` runs.
- `rolling/tests/checksum/simd.rs` - SSE2/AVX2 tests gated to
  `x86`/`x86_64`, NEON test gated to `aarch64`, each short-circuiting
  with `is_x86_feature_detected!`/`is_aarch64_feature_detected!` when
  the runtime CPU does not advertise the feature.
- `simd_self_test.rs` - architecture-agnostic dispatcher exercise;
  `rolling_backend_tag()` uses `cfg!()` macros (not `#[cfg]`) so the
  diagnostic string compiles on every platform.
- `simd_parity_tests.rs` - test-only module gated by `#[cfg(test)]` at
  `lib.rs:371`. No per-arch gating needed because dispatchers handle
  fallback internally.
- `strong/sha256.rs` - `sha256_hardware_acceleration_available()`
  pairs `target_arch = "x86_64"`, `target_arch = "aarch64"`, and
  `not(any(...))` arms each returning a bool; tests at `:215,226,237`
  mirror the same three-way split.
- `strong/xxhash.rs`, `crc32c.rs` - SIMD detection delegated to the
  `xxh3`, `xxhash-rust`, and `crc32c` crates, which carry their own
  cached runtime probes.
- `pipelined/`, `parallel/` - no SIMD intrinsics; rayon-based
  parallelism is platform-neutral.

## Repairs included in this PR

W-1 is fixed in the same PR (three short SAFETY comments). W-2 is
documented for future cleanup and not touched (would either expose a
new WASM backend or remove dead code; out of scope for an audit-doc
PR).

## Out of scope

- **SPL-31** decomposition of `md5_simd/avx512.rs` (1292 LoC). The
  file is correctly gated and tested; LoC limits were retired per
  `feedback_loc_limits.md` (2026-05-18) and the file is read top-to-
  bottom by the one runtime self-test that needs it.
- **CCL-26** (proposed): reconcile the MD5 WASM SIMD asymmetry.
- Performance changes to the dispatcher hot path.
- Adding new SIMD backends (AVX-512 BMI, AArch64 SVE).
