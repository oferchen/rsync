# Init-time backend selection: eliminate hot-path branches

Tracking issue: oc-rsync task #2116.

Cross-reference: #2117 (PlatformBackend enum unification, merged) standardized
the policy enums. #2116 evaluates whether to push the same idea one layer
down, into function dispatch, by replacing per-call CPU-feature branches with
a single function pointer set at startup.

This document is design-only. No code lands in this PR.

## 1. Current dispatch patterns

Every site below was located by grepping for
`is_x86_feature_detected | is_aarch64_feature_detected | OnceLock` in
`crates/checksums/`, `crates/fast_io/`, and `crates/matching/`. The matching
crate has no runtime CPU-feature dispatch (no hits), so it is out of scope.

The surveyed sites fall into three categories:

### (a) Cached-bool branch per call

The CPU-feature probe is cached in a `OnceLock<bool>` (or a small struct of
bools), but the dispatch site reads the cache and branches on every call.

| Site | Hot path | Notes |
|------|----------|-------|
| `crates/checksums/src/rolling/checksum/x86.rs:79` (`FEATURES`) read at `:84`, `:85`, `:112`, `:114`, `:119` | `RollingChecksum::update` per chunk | `try_accumulate_chunk` re-reads `effective_features()` per call; two branches (`features.avx2`, `features.sse2`). |
| `crates/checksums/src/rolling/checksum/neon.rs:63` (`NEON_AVAILABLE`) read at `:67`, `:73`, `:83` | `RollingChecksum::update` per chunk | One branch in `accumulate_chunk`; if false, falls through to scalar. |
| `crates/checksums/src/strong/openssl_support.rs:15` (`DETECTED`) read at `:32`, `:37`, `:46` | `new_md4_hasher` / `new_md5_hasher` per `Md4`/`Md5` construction | Once per hasher, not per byte. Per-construction, not per-update. |
| `crates/fast_io/src/splice.rs:76` (`SPLICE_SUPPORTED`) read at `:106` via `is_splice_available()` | Zero-copy send path per transfer | Once per transfer attempt, not per byte. |
| `crates/fast_io/src/io_uring/linkat.rs:49`, `renameat2.rs:56`, `statx.rs:71`, `buffer_ring.rs:274` | Per syscall family | Probed once, returns bool. Branch happens once per submission, dwarfed by syscall cost. |

The two genuinely hot sites are the rolling-checksum dispatchers
(`x86::try_accumulate_chunk`, `neon::accumulate_chunk`). Everything else
is per-file or per-syscall and the branch cost is in the noise.

### (b) Cached-function-pointer call

The probe selects a `fn` pointer once and stores it. Subsequent calls go
through one indirect jump with no feature-test branches.

| Site | Pattern |
|------|---------|
| `crates/fast_io/src/zero_detect.rs:81` `type FindFn = fn(&[u8]) -> usize;` with `static DISPATCH: OnceLock<FindFn>` at `:83`; `select_impl` at `:90` builds the pointer. | Cached `fn` pointer, called per zero-run probe. The hot path is `find_first_nonzero(buf)` at `:50`, which does `dispatch()(buf)` - one `OnceLock::get_or_init` (one atomic load on the steady state), one indirect call. |
| `crates/checksums/src/simd_batch/md5_dispatcher.rs:571` `static DISPATCHER: OnceLock<Dispatcher>` returned by `global()` at `:569`. | Cached struct holding a `Backend` enum; dispatch is `match self.backend` (`:238` on) per batch, not per byte. Eight match arms compile down to a jump table. |
| `crates/checksums/src/simd_batch/md4/mod.rs:386` `static DISPATCHER: OnceLock<Md4Dispatcher>` returned by `md4_dispatcher()` at `:384`. | Same `Backend` enum + match pattern as MD5. |

`zero_detect.rs` is the only existing example of the pure function-pointer
pattern this task evaluates. `md5_dispatcher` / `md4` use the
"enum-tag + match" pattern, which the compiler lowers to a jump table after
the dispatcher is cached.

### (c) Static-dispatch (cfg-only)

The decision is `cfg`-gated, the branch is removed at compile time, the call
is a direct static call. No runtime cost beyond the function itself.

| Site | Pattern |
|------|---------|
| `crates/checksums/src/rolling/checksum/mod.rs:534-553` `accumulate_chunk_arch` has three `#[cfg(target_arch = ...)]` variants. | The dispatcher selects between `neon::accumulate_chunk`, `x86::try_accumulate_chunk`, or scalar `None` purely at compile time. |
| `crates/checksums/src/rolling/checksum/mod.rs:50-66` `simd_available_arch` - same shape. | Compile-time only. |
| `crates/checksums/src/strong/sha256.rs:142-158` `sha256_hardware_acceleration_available` - cfg-gated, probes CPUID each call but never cached. | NOT in a hot path; called from version-banner rendering. The lack of cache here is a minor wart but not a perf bug. |

## 2. Per-call overhead estimate for category (a)

The cost of a cached-bool branch under steady state:

- `OnceLock::get()` on the fast path is one acquire load and a null check
  (one branch). On x86-64 this is a single `mov` + `test` + `jne`.
- Reading the cached `FeatureLevel` struct (`x86.rs:83`) and testing two
  fields is two more `test` + branch pairs.
- Modern branch predictors saturate at near-100% accuracy on these branches
  once warm (the outcome never changes after the first call), so the
  predicted cost is roughly 1 cycle per branch on Zen 3 / Skylake-derived
  cores.

Per `RollingChecksum::update(chunk)` call we pay:

1. One `OnceLock` load + null check for `FEATURES` (`x86.rs:83`).
2. One branch on `features.avx2` (`x86.rs:114`).
3. If false, one branch on `features.sse2` (`x86.rs:119`).
4. One branch on `chunk.len() >= AVX2_BLOCK_LEN` (data-dependent, not
   feature-dependent - this branch survives any dispatch change).

NEON path is leaner: one `OnceLock` load + one branch (`neon.rs:83`).

A 1 GiB delta scan with a 1 KiB block size calls `accumulate_chunk` roughly
1M times. At ~3 predicted-branch cycles per call extra, that is ~3M cycles
or under 1 ms on a 3 GHz core - well under 0.01% of the wall-clock time the
checksum loop itself spends touching memory.

The benchmarks under `crates/checksums/benches/checksums_benchmark.rs` and
`crates/checksums/benches/parallel_benchmark.rs` will not move noticeably
on either implementation.

## 3. Function-pointer migration sketch

Two shapes are possible. Both keep the OnceLock; both remove the per-call
feature-bool branches.

### Shape A: raw `fn` pointer

```rust
type AccumFn = fn(u32, u32, usize, &[u8]) -> (u32, u32, usize);

static DISPATCH: OnceLock<AccumFn> = OnceLock::new();

fn dispatch() -> AccumFn {
    *DISPATCH.get_or_init(select_impl)
}

fn select_impl() -> AccumFn {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            return accumulate_chunk_avx2_entry;
        }
        if is_x86_feature_detected!("sse2") {
            return accumulate_chunk_sse2_entry;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return accumulate_chunk_neon_entry;
        }
    }
    accumulate_chunk_scalar_raw
}
```

This is the exact pattern `crates/fast_io/src/zero_detect.rs:81-109`
already uses. Calls become one indirect jump.

### Shape B: trait object

```rust
trait RollingAccum: Send + Sync {
    fn accumulate(&self, s1: u32, s2: u32, len: usize, chunk: &[u8])
        -> (u32, u32, usize);
}

static DISPATCH: OnceLock<&'static dyn RollingAccum> = OnceLock::new();
```

Trade-offs:

- `fn` pointer: single indirect call, no vtable, no `&self`. No state
  carried with the pointer; if the backend ever needs lookup tables, they
  have to live in `static`s.
- `dyn` trait object: vtable lookup (one extra load) plus an indirect
  call. Carries state cleanly. Easier to extend with new ops (the
  PlatformBackend approach from #2117).

For the rolling checksum the impls are stateless and uniform-shaped, so
the `fn` pointer wins on overhead. For PlatformBackend-style aggregates
(multiple ops per backend), the trait object is the right shape.

The `md5_dispatcher` / `md4` "enum tag + match" pattern is a third option
already in use. After warm-up the match lowers to a jump table, so it is
roughly equivalent to a `fn` pointer in steady-state branch behaviour and
keeps the dispatch decision visible in the source.

## 4. Risk: function pointers defeat inlining

The rolling-checksum scalar fallback is hot enough that the compiler
already inlines `accumulate_chunk_scalar_raw` into the dispatcher in
release mode. Switching to a `fn` pointer through `OnceLock`:

- Forces an indirect call that the compiler cannot devirtualize unless
  it proves the cell is initialised to a known function (it cannot, in
  general).
- Defeats inlining of the scalar path. The scalar `accumulate_chunk_scalar_raw`
  body is 4 unrolled byte ops per loop iteration
  (`crates/checksums/src/rolling/checksum/mod.rs:578-591`); the function
  call overhead becomes measurable when the chunk is small.
- Defeats inlining of `update_byte` (already a separate path at
  `crates/checksums/src/rolling/checksum/mod.rs:191-198`).

For SIMD paths the body is large (AVX2 32-byte-per-iter loop with prologue
and epilogue), the call overhead is negligible relative to the work, and
the indirect call wins by removing two branches per chunk.

For scalar fallback on hardware that has no SIMD (rare for x86_64 and
aarch64 in 2026, common for `wasm32` without `simd128`), the `fn` pointer
shape regresses. Mitigation: keep the scalar path direct-call by making
the cached pointer wrap "SIMD or scalar" rather than "SIMD only":

```rust
#[inline]
fn accumulate(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> (u32, u32, usize) {
    match dispatch() {
        Some(simd_fn) => simd_fn(s1, s2, len, chunk),
        None => accumulate_chunk_scalar_raw(s1, s2, len, chunk),
    }
}
```

This keeps the scalar path direct (and inlinable) at the cost of one
extra `Option` branch on SIMD-capable hardware. Net: identical to today,
because today's hot path also tests one bool before reaching the SIMD
implementation.

## 5. Recommendation: reject for rolling checksum, adopt selectively elsewhere

**Reject** the migration for the rolling checksum (`x86.rs`, `neon.rs`).
Reasons:

1. The branch cost is sub-1-ms per GiB scanned. It is invisible at the
   benchmark level.
2. The `fn` pointer regresses the scalar fallback for SIMD-less targets
   without a redesigned wrapper that pays back the saving.
3. The existing OnceLock-cached bool pattern is already the textbook
   answer for "rare-update, dense-read" SIMD probes. It compiles to a
   cmov-friendly load + test + branch on every architecture we ship.
4. The `effective_features()` indirection at
   `crates/checksums/src/rolling/checksum/x86.rs:91-97` exists because the
   CLI override (`SimdLevel`) can be set at startup. A `fn` pointer cached
   at first use would freeze the override - the override must be installed
   before the cache fills. We already document that contract for
   `set_simd_override` (`crates/checksums/src/cpu_features.rs:153-177`),
   so this is solvable, but it removes the late-binding flexibility today's
   tests rely on (`reset_simd_override_for_tests` at `:196`,
   `clear_simd_override_for_tests` at `:202`).

**Adopt** for the following subsystems:

- `crates/checksums/src/strong/sha256.rs:142` -
  `sha256_hardware_acceleration_available` currently re-probes CPUID on
  every call. Wrap in a `OnceLock<bool>` to match the rest of the crate.
  This is a one-line fix and orthogonal to the fn-pointer question.
- `crates/fast_io/src/zero_detect.rs` already uses the fn-pointer pattern
  and is the model implementation. No change needed; keep as canonical
  example.
- New SIMD subsystems (e.g. AVX-512 batch hashing rollouts under #1763 and
  the AVX-512 VPDPBUSD Adler-32 work in `avx512-vpdpbusd-adler32.md`)
  should follow the `md5_dispatcher` enum-tag pattern: it keeps the
  dispatch table visible, avoids the late-binding override problem, and
  the match-to-jumptable lowering is as fast as a `fn` pointer.

**Reject everywhere else.** The io_uring opcode probes
(`linkat.rs`, `renameat2.rs`, `statx.rs`, `buffer_ring.rs`) and the splice
probe (`splice.rs`) cache one bool that is consulted once per syscall
family. The branch is a rounding error against the syscall itself.

The OpenSSL detection (`openssl_support.rs`) caches one bool consulted
once per hasher construction, which is once per signature batch, not per
byte. No change warranted.

## 6. Benchmarks that would catch a regression

If anyone revisits this and tries the migration anyway, the following
existing benches must show no regression before any change lands:

| Bench | File | Catches |
|-------|------|---------|
| `bench_rolling_checksum`, `bench_rolling_checksum_roll` | `crates/checksums/benches/checksums_benchmark.rs` | Per-chunk and per-byte rolling-checksum throughput. The migration's biggest risk surface. |
| MD4 multibuffer sweep (task #4189) | `crates/checksums/benches/md4_multibuffer_benchmark.rs` | Lane-width crossover; catches any dispatcher regression at small N where call overhead dominates. |
| MD5 multibuffer sweep | `crates/checksums/benches/md5_multibuffer_benchmark.rs` | Same as above for MD5. |
| `framing_overhead_benchmark` | `crates/checksums/benches/framing_overhead_benchmark.rs` | End-to-end signature pipeline including dispatcher cost. |
| `parallel_benchmark` | `crates/checksums/benches/parallel_benchmark.rs` | Multi-thread dispatcher contention (the cached `OnceLock` is read by every worker). |
| `pipelined_benchmark` | `crates/checksums/benches/pipelined_benchmark.rs` | Generator-side delta computation that drives rolling-checksum updates. |
| `io_optimizations` | `crates/fast_io/benches/io_optimizations.rs` | Zero-detect dispatch site (`find_first_nonzero`). Confirms the existing fn-pointer pattern is not on the critical path. |

Commit the baseline numbers before any speculative change. A migration
that does not move these numbers is not worth the loss of late-binding
override flexibility.

## 7. Summary

Today's pattern is correct for the rolling-checksum hot path. The
function-pointer migration is a textbook idea that does not pay off on
this codebase because:

1. The branch cost is already negligible (sub-1-ms per GiB).
2. The CLI SIMD override demands late binding the fn-pointer cache would
   freeze.
3. The pure scalar fallback regresses without an extra Option wrapper that
   buys back the supposed saving.

Apply the discipline elsewhere only where it is free: add a OnceLock to
`sha256_hardware_acceleration_available`, follow the
`md5_dispatcher`-style enum-tag dispatch for new SIMD subsystems, and
keep `zero_detect.rs` as the reference implementation for cases where a
true fn-pointer is the right tool.
