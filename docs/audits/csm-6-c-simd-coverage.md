# CSM-6 - `--checksum` SIMD-coverage audit

Date: 2026-05-24
Scope: read-only research, no `.rs` edits.
Tracked under: CSM-6 (parent CSM track: `--checksum` mode is ~1.5-1.7x slower
than upstream rsync 3.4.1; upstream issue #970).

## 1. Goal

CSM-4 (`docs/audits/csm-4-strong-checksum-upstream-parity.md`) ranked
`simd_batch` non-wiring as the second-largest contributor to the `--checksum`
slowdown ("~4-16x per-core for many-small-files trees"). CSM-6 inventories
that gap precisely: for every `-c` call site and every host architecture,
what hash backend, what lane width, and what dispatcher is actually used
today? The cross-tab below feeds CSM-7 (synthesis) and CSM-8 (the fix).

The audit is restricted to the **whole-file** strong digest used by `-c`
(`generator.c:617 quick_check_ok` -> `checksum.c:402 file_checksum`). It
also covers the receiver-side **per-chunk** verify step that BR-3i.c wired
up, because that path shares the same single-stream backends and the user
asked it to be inventoried alongside `-c`.

## 2. `simd_batch` inventory

`crates/checksums/src/simd_batch/` ships two batch dispatchers, both
runtime-detected via a process-wide `OnceLock`.

### 2.1 MD5 batch dispatcher

`crates/checksums/src/simd_batch/md5_dispatcher.rs::Dispatcher` and
`crates/checksums/src/simd_batch/md5_simd/`.

| Backend | Lanes | Source file | Runtime gate |
| --- | --- | --- | --- |
| AVX-512 | 16 | `md5_simd/avx512.rs` (46 KiB) | `avx512f && avx512bw` |
| AVX2 | 8 | `md5_simd/avx2.rs` (13 KiB) | `avx2` |
| SSE4.1 | 4 | `md5_simd/sse41.rs` (14 KiB) | `sse4.1` (blendv) |
| SSSE3 | 4 | `md5_simd/ssse3.rs` (14 KiB) | `ssse3` (pshufb) |
| SSE2 | 4 | `md5_simd/sse2.rs` (15 KiB) | always on `x86_64` |
| NEON | 4 | `md5_simd/neon.rs` (13 KiB) | always on `aarch64` |
| WASM SIMD | 4 | `md5_simd/wasm.rs` (11 KiB) | `wasm32 + simd128` |
| Scalar | 1 | `md5_scalar.rs` | portable fallback |

Public entry points: `simd_batch::digest_batch(&[T])` and the re-export
`checksums::strong::md5_digest_batch`. Selection is overridable via the CLI
`--simd` flag (`cpu_features::feature_allowed`).

### 2.2 MD4 batch dispatcher

`crates/checksums/src/simd_batch/md4/mod.rs::Md4Dispatcher` and
`crates/checksums/src/simd_batch/md4/simd/`.

| Backend | Lanes | Source file | Runtime gate |
| --- | --- | --- | --- |
| AVX-512 | 16 | `md4/simd/avx512.rs` | `avx512f && avx512bw` |
| AVX2 | 8 | `md4/simd/avx2.rs` | `avx2` |
| SSE2 | 4 | `md4/simd/sse2.rs` | always on `x86_64` (covers SSSE3 / SSE4.1 since MD4's simpler round functions do not benefit from `pshufb`/`blendv`) |
| NEON | 4 | `md4/simd/neon.rs` | always on `aarch64` |
| WASM SIMD | 4 | `md4/simd/wasm.rs` | `wasm32 + simd128` |
| Scalar | 1 | `md4/scalar.rs` | portable fallback |

Public entry points: `simd_batch::md4::digest_batch(&[T])` and the re-export
`checksums::strong::md4_digest_batch`.

### 2.3 XXH3 / XXH64 / SHA-family batch dispatchers

None. There is no `simd_batch::xxh3` and no `simd_batch::sha*`. The XXH3
crate already SIMD-accelerates its single-stream path, so the only multi-
buffer opportunity for `-c` workloads is MD4/MD5.

## 3. Production callers of `simd_batch`

| Call site | Algorithm path | Notes |
| --- | --- | --- |
| `crates/signature/src/algorithm.rs:179` | `md4_digest_batch(blocks)` | Per-**block** signature batch (sender's `sum_init`/`sum_update` analog), unseeded MD4 only |
| `crates/signature/src/algorithm.rs:192` | `md5_digest_batch(blocks)` | Per-**block** signature batch, unseeded MD5 only |

That is the entire production wiring of `simd_batch` today. It exists in
the per-block signature path only. **Neither `-c` consumer reaches it.**
Every other reference to `*_digest_batch` lives under
`crates/checksums/{benches,tests}` or `crates/checksums/src/comprehensive_tests.rs`.

## 4. `-c` call sites under audit

| # | Call site (`file_path:line`) | Function | What it hashes | Used by |
| --- | --- | --- | --- | --- |
| C1 | `crates/transfer/src/receiver/quick_check.rs:262` | `file_checksum_matches` | Destination basis file, whole-file digest | Receiver `-c` mode (`always_checksum`) via `quick_check_matches:60-67` |
| C2 | `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:148` | `hash_file_contents` | Both source and destination, whole-file digest, parallelised across pairs via rayon | Local-copy executor's `-c` mode prefetch (`prefetch_checksums:88`, `ChecksumCache::from_prefetch:292`) |
| C3 | `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:627` | `ParallelDeltaApplier::verify_chunk` | Per-chunk strong digest during delta-apply | Receiver-side parallel delta-apply (BR-3i.c, gated behind `parallel-receive-delta`; not the `-c` whole-file path but shares the same single-stream backend selection) |
| C4 | `crates/checksums/src/parallel/files.rs:22` | `hash_file_internal` | Whole-file digest with mmap-first I/O | **Dead in the `-c` path.** No production caller. Only invoked via the crate's own tests/benches. Documented here because CSM-4 cited it as a complementary path |

### 4.1 C1 - `quick_check.rs::file_checksum_matches`

```rust
let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
let mut buf = [0u8; 64 * 1024];
let mut remaining = file_size;
while remaining > 0 {
    let to_read = buf.len().min(remaining as usize);
    if file.read_exact(&mut buf[..to_read]).is_err() {
        return false;
    }
    hasher.update(&buf[..to_read]);
    remaining -= to_read as u64;
}
```

`ChecksumVerifier::for_algorithm` (`crates/transfer/src/delta_apply/checksum.rs:85`)
returns a single-file streaming hasher: `Md5(Md5::new())` -> `Md5Backend::new()`
-> `md5::Md5::new()` (pure-Rust `md-5` crate, scalar). The `simd_batch`
module is never reached. No batching across files. No mmap.

### 4.2 C2 - `parallel_checksum.rs::hash_file_contents`

```rust
SignatureAlgorithm::Md5 { seed_config } => {
    let mut hasher = Md5::with_seed(seed_config);
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    hasher.finalize().as_ref().to_vec()
}
```

Callers pass `Md5Seed::default() == Md5Seed::none()`
(`crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:349, 375, 401, 428, 461, 522`),
so no seed bytes are mixed - identical to upstream `file_checksum`. `Md5::with_seed`
goes through the same single-stream `md-5` crate backend as C1. The
*outer* loop in `prefetch_checksums:88` already uses `par_iter`, so each
file is hashed on a different rayon worker, but **within each worker the
hash is single-stream**. The SIMD batch dispatcher is never invoked, so
the AVX-512 / AVX2 / SSSE3 / SSE4.1 / NEON / WASM batch lanes are dormant
even when many files are queued.

`SignatureAlgorithm::Md4` and `Md4Seeded` follow the same pattern via
`Md4::new()` / `Md4::with_seed(seed)` -> pure-Rust `md4` crate, scalar.

### 4.3 C3 - `ParallelDeltaApplier::verify_chunk`

```rust
fn verify_chunk(
    strategy: &dyn ChecksumStrategy,
    chunk: DeltaChunk,
) -> Result<VerifiedChunk, ParallelApplyError> {
    let digest = strategy.compute(&chunk.data);
    ...
}
```

`Md5Strategy::compute` (`crates/checksums/src/strong/strategy/impls.rs:86`)
calls `Md5::digest_with_seed(self.seed, data)` which builds a fresh
single-stream `md-5` crate hasher and runs `update`+`finalize`. The
parallel-apply wrapper `apply_batch_parallel`
(`crates/engine/src/concurrent_delta/parallel_apply/batch.rs:45`) does run
each chunk's `verify_chunk` on a separate rayon worker via
`into_par_iter().map(|chunk| Self::verify_chunk(...))`, so verification
fans out across cores - but again each chunk is hashed single-stream. The
SIMD batch dispatcher is never reached.

This path is gated behind the `parallel-receive-delta` Cargo feature and is
not the default `-c` path, but listed here per the audit brief.

### 4.4 C4 - `parallel/files.rs::hash_file_internal`

```rust
if size <= config.max_memory_file_size {
    let mut data = Vec::with_capacity(size as usize);
    let mut reader = BufReader::with_capacity(config.buffer_size, file);
    reader.read_to_end(&mut data)?;
    return Ok((D::digest(&data), size));
}
if size >= MMAP_THRESHOLD {
    if let Ok(mmap) = MmapReader::open(path) {
        let _ = mmap.advise_sequential();
        return Ok((D::digest(mmap.as_slice()), size));
    }
}
```

`D::digest` -> `D::digest_with_seed(Default::default(), data)`
(`crates/checksums/src/strong/mod.rs:168`) -> same single-stream hasher
constructed via `D::with_seed` + `update` + `finalize`. The mmap path is
nicer for I/O than C1/C2 but the per-file hash is still single-stream and
no `simd_batch` call is reached. `hash_files_parallel` / `hash_files_parallel_with_config`
have zero non-test callers in the workspace.

## 5. Cross-tab: lane width per call site per host architecture

Each cell describes the active **per-call** lane width and hashing backend
on the named host. Lane width 1 means single-stream scalar / single-buffer
SIMD inside the `md-5` or `md4` crate (those crates internally use the
scalar reference implementation; they do not contain multi-buffer SIMD).

| Call site | x86_64 + AVX-512 | x86_64 + AVX2 | x86_64 + SSE4.1 | x86_64 + SSSE3 | x86_64 + SSE2 | aarch64 + NEON | wasm32 + simd128 | Other / Scalar host |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **C1** `file_checksum_matches` (MD5) | 1-lane `md-5` | 1-lane `md-5` | 1-lane `md-5` | 1-lane `md-5` | 1-lane `md-5` | 1-lane `md-5` | 1-lane `md-5` | 1-lane `md-5` |
| **C1** `file_checksum_matches` (MD4) | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` |
| **C1** `file_checksum_matches` (XXH3 / XXH64 / XXH128) | XXH3 SIMD inside `xxh3` crate (already AVX2) | as left | as left | as left | as left | XXH3 SIMD inside `xxh3` crate (NEON) | scalar | scalar |
| **C1** `file_checksum_matches` (SHA1) | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` |
| **C2** `hash_file_contents` (MD5) | 1-lane `md-5` (rayon **across files**, but 1-lane within each) | as left | as left | as left | as left | as left | as left | as left |
| **C2** `hash_file_contents` (MD4, seeded or unseeded) | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` | 1-lane `md4` |
| **C2** `hash_file_contents` (XXH64 / XXH3 / XXH3_128) | streaming `xxhash-rust` (~10-20% slower than reference) | as left | as left | as left | as left | as left | as left | as left |
| **C2** `hash_file_contents` (SHA1) | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` | 1-lane `sha1` |
| **C3** `verify_chunk` (MD5 strategy) | 1-lane `md-5` (rayon **across chunks**, but 1-lane within each) | as left | as left | as left | as left | as left | as left | as left |
| **C3** `verify_chunk` (XXH3 strategy) | XXH3 SIMD inside `xxh3` crate (AVX2) | as left | as left | as left | as left | XXH3 SIMD inside `xxh3` crate (NEON) | scalar | scalar |
| **C4** `hash_file_internal` (any `StrongDigest`) | 1-lane per impl (mmap I/O only) | as left | as left | as left | as left | as left | as left | as left |

For comparison, the per-block signature path:

| Call site | x86_64 + AVX-512 | x86_64 + AVX2 | x86_64 + SSE4.1 | x86_64 + SSSE3 | x86_64 + SSE2 | aarch64 + NEON | wasm32 + simd128 | Other / Scalar host |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `algorithm.rs:179` (`md4_digest_batch`) | **16-lane AVX-512** | **8-lane AVX2** | 4-lane SSE2 | 4-lane SSE2 | 4-lane SSE2 | **4-lane NEON** | **4-lane WASM** | 1-lane scalar |
| `algorithm.rs:192` (`md5_digest_batch`) | **16-lane AVX-512** | **8-lane AVX2** | **4-lane SSE4.1** | **4-lane SSSE3** | **4-lane SSE2** | **4-lane NEON** | **4-lane WASM** | 1-lane scalar |

The per-block path is the proof that the dispatchers work; the gap is
entirely in the four `-c` consumers above.

## 6. Call sites stuck at scalar / single-stream

Production sites (compiled into the default `--release` binary) where the
SIMD batch dispatcher is implemented but unwired:

1. `crates/transfer/src/receiver/quick_check.rs:262`
   `file_checksum_matches` - **C1**, receiver-side `-c` quick-check.
2. `crates/transfer/src/receiver/quick_check.rs:271`
   `ChecksumVerifier::for_algorithm(algorithm)` constructor used inside C1
   (single-stream hasher).
3. `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:148`
   `hash_file_contents` body covers MD4, MD4Seeded, MD5, SHA1, XXH64,
   XXH3, XXH3_128 - **C2**, local-copy executor `-c` prefetch (one
   `match` arm per algorithm, all single-stream).
4. `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:185`
   `Md5::with_seed(seed_config)` - the `seed_config` is always
   `Md5Seed::none()` for `-c` callers, so this is unseeded MD5 that
   `md5_digest_batch` could serve.
5. `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:627-647`
   `verify_chunk` - **C3**, hashes one `DeltaChunk` per call via
   `ChecksumStrategy::compute`. Wrapped by
   `crates/engine/src/concurrent_delta/parallel_apply/batch.rs:45-65`
   (`apply_batch_parallel`) which fans out across cores via rayon but
   never batches chunks onto SIMD lanes within a core. Gated behind
   `parallel-receive-delta`.
6. `crates/checksums/src/strong/strategy/impls.rs:86`
   `Md5Strategy::compute` - the strategy backend invoked by C3.
7. `crates/checksums/src/parallel/files.rs:22`
   `hash_file_internal` - **C4**, public but unused by any `-c` consumer.

**Total: 7 production sites, 4 of them in the actual `-c` path.** All of
C1, C2 (every algorithm arm), and the strategy-backed C3 are stuck at
1-lane on every host architecture, including AVX-512 / AVX2 / NEON hosts
where `simd_batch` would otherwise deliver 4-16x.

## 7. CSM-8 fix sketch (symbols only - no code in this audit)

### 7.1 Receiver-side `-c` (C1)

Today `quick_check_matches:60` decides per-file whether to fall through
into `file_checksum_matches`. To benefit from `simd_batch`, the receiver
must gather a window of candidate (`dest_path`, `expected`, `algorithm`,
`file_size`) tuples first, then dispatch them as a single batch.

Symbols to swap to:

- For MD5 (unseeded) -> `checksums::strong::md5_digest_batch` (re-export of
  `crates/checksums/src/simd_batch/mod.rs::digest_batch`), backed by
  `simd_batch::md5_dispatcher::global()`.
- For MD4 (unseeded; seeded MD4 cannot batch because of the
  `seed.to_le_bytes()` suffix, see `checksum.c:377-380`) -> `checksums::strong::md4_digest_batch`
  (re-export of `crates/checksums/src/simd_batch/md4/mod.rs::digest_batch`).
- For XXH64 / XXH3 / XXH3_128 / SHA1 -> remain single-stream (no batch
  dispatcher exists; XXH3 already SIMD inside its own crate). CSM-8 should
  not block on these.

The wiring change lives in the receiver loop that drives
`quick_check_matches`. The simplest shape is a new
`file_checksums_match_batch(paths: &[...], algorithm, expected_digests: &[...])`
helper inside `crates/transfer/src/receiver/quick_check.rs` that reads
each file's bytes (or mmap), then routes through `simd_batch` once N
inputs are queued. The batch size should match the active dispatcher's
lane count (`simd_batch::parallel_lanes()`), padded up to a multiple of
the lane width.

### 7.2 Local-copy `-c` prefetch (C2)

`prefetch_checksums` already fans out file pairs across rayon workers. The
fix is to swap the **inner** hashing call from
`Md5::with_seed(seed_config) + update + finalize` to a chunked batch
dispatch. Two reasonable shapes:

- **Per-worker batch.** Each rayon worker gathers up to N inputs (where N
  = `simd_batch::parallel_lanes()`) before calling `md5_digest_batch`.
  Requires restructuring `par_iter` over `pairs` into a chunked iterator.
- **Two-phase prefetch.** Phase A: read all source+destination bytes (in
  parallel, via `par_iter`) into `Vec<(PathBuf, Vec<u8>)>`. Phase B:
  single `md5_digest_batch(&all_source_bytes)` + `md5_digest_batch(&all_dest_bytes)`.
  Simpler but holds more memory; bounded by the existing
  `FileHashConfig::max_memory_file_size` (default 1 MiB) and a batch
  cap.

Symbols to swap to: same as 7.1 (`md5_digest_batch`, `md4_digest_batch`).
The dispatch should mirror `crates/signature/src/algorithm.rs:174
SignatureAlgorithm::compute_truncated_batch` which already handles the
"only unseeded MD4/MD5 batch; everything else falls back" decision tree.

### 7.3 Per-chunk verify (C3)

`ParallelDeltaApplier::apply_batch_parallel` already collects a
`Vec<DeltaChunk>` per call. The verify fan-out today is `par_iter().map(|chunk|
verify_chunk(...))` (line 58-60). To use `simd_batch`, group the chunks
by `ChecksumAlgorithmKind` and route MD5/MD4 groups through
`md5_digest_batch` / `md4_digest_batch` in lane-width-sized sub-batches,
then zip the resulting digests back into `VerifiedChunk { chunk, digest }`
in their original order. The strategy trait needs a new method like:

```text
trait ChecksumStrategy {
    fn compute_batch(&self, datas: &[&[u8]]) -> Vec<ChecksumDigest>;
}
```

with a default `compute_batch` that loops over `self.compute(data)` and
specialised `Md5Strategy::compute_batch` / `Md4Strategy::compute_batch`
that delegate to `simd_batch::digest_batch` / `simd_batch::md4::digest_batch`.
The seeded MD5 / MD4 strategies stay on the scalar path (the seed-prefix
or seed-suffix injection cannot batch with the current per-input shape
unless the seed is folded into the data buffer upfront, which is the
trick `crates/signature/src/algorithm.rs:198-206` already uses to fall
back).

CSM-8 should treat C3 as **lower priority than C1/C2** since the parallel-
apply path is feature-gated and not on by default.

### 7.4 C4 - leave alone

`hash_files_parallel` has zero production callers. If CSM-8 chooses to
introduce a shared "batched whole-file hash" helper, it could live here
and replace the body of both C1 and C2; otherwise leave it as a
test/bench utility.

## 8. Speedup estimate per file-size bucket

These ranges combine (a) the dispatcher's lane count on a given host,
(b) the per-lane throughput delta between `md-5`-crate scalar and the
multi-buffer SIMD path, and (c) the I/O ceiling for each bucket. They
are upper-bounded by CSM-4's "4-16x per-core on many-small-files trees".

| Bucket | File size | Dominant cost today | Expected `-c` speedup from C1+C2 batch routing on a typical AVX2 host | Notes |
| --- | --- | --- | --- | --- |
| Small | < 64 KiB | Per-file hasher construction + scalar MD5 finalize; I/O fits in one read | **6-10x** (close to the 8-lane AVX2 theoretical max; per-file constant overhead amortises away across a batch) | Trees of many small config / source files. Most-favourable bucket |
| Medium | 64 KiB - 4 MiB | Scalar MD5 compress over multiple 64 KiB reads, CPU-bound on the `md-5` crate | **3-5x** on AVX2, **5-7x** on AVX-512, **3-4x** on NEON | Most realistic everyday `-c` workload (source trees, photo libraries) |
| Large | > 4 MiB | I/O (`read_exact` into 64 KiB stack buffer) saturates a single thread before MD5 keeps up | **1.5-2x** on AVX2; SIMD batching wins less because (i) hashing one large file does not benefit from multi-buffer batching by itself, (ii) the per-file rayon worker is already busy. Larger wins come from C1's I/O changes (mmap) tracked separately under O1+O5 in CSM-4 | Use mmap (CSM-4 item O1+O5) on top to recover the rest |

On a SSSE3-only x86_64 host, multiply the small/medium gains by ~0.5
(4-lane vs 8-lane). On AVX-512 hosts, multiply by ~1.5-2x relative to the
AVX2 figures. Scalar hosts (non-x86_64, non-aarch64, non-wasm32) see no
batch gain at all; their lane width stays at 1.

These estimates are bounded by I/O. On a fast NVMe + warm page cache the
upper end of each range applies. On a cold cache or rotational disk, the
gains compress because the I/O ceiling falls below the CPU ceiling.

## 9. Confirmed non-gaps

- The `simd_batch` dispatchers themselves are fully wired and exercised by
  parity tests (`simd_parity_tests::md{4,5}_simd_parity`). No correctness
  work is needed in CSM-8.
- The per-block signature path (`crates/signature/src/algorithm.rs::compute_truncated_batch`)
  already routes through `simd_batch` correctly; CSM-8 should not touch it.
- `CpuFeature` runtime detection (`crates/checksums/src/cpu_features.rs`)
  is cached via `OnceLock` and respected by both dispatchers' `feature_allowed`
  guards. CSM-8 should not introduce its own detection.
- The XXH3 streaming gap (CSM-4 item 2.3) is unrelated to `simd_batch` and
  is tracked separately.
- The OpenSSL-backed MD5 gap (CSM-4 item O7) is unrelated to `simd_batch`
  and is the highest-ranked item overall; CSM-8 may or may not bundle it
  with the `simd_batch` wiring depending on the CSM-7 decision.

## 10. Next steps

- **CSM-7 (synthesis)**: combine this audit with CSM-4's ranking and pick
  the order in which CSM-8 lands the fixes. Recommended: C2 (local-copy
  `-c` prefetch) first because the prefetch already collects file pairs
  and the change is mechanical; C1 (receiver `-c`) second because it
  requires a new batch-collection helper in the receiver loop; C3 last
  (feature-gated, lower priority).
- **CSM-8 (implementation)**: implement the symbol swaps in 7.1-7.3,
  carrying the lane-width-aware sub-batching and the seeded-MD4/MD5
  fall-back exactly as `compute_truncated_batch` already does. Add
  parity-vs-scalar tests for every new call site, and add benchmarks
  covering the small/medium/large buckets defined in section 8.

## 11. References

### oc-rsync source

- `crates/checksums/src/simd_batch/mod.rs`
- `crates/checksums/src/simd_batch/md5_dispatcher.rs`
- `crates/checksums/src/simd_batch/md4/mod.rs`
- `crates/checksums/src/simd_batch/md5_simd/{avx2,avx512,sse2,sse41,ssse3,neon,wasm}.rs`
- `crates/checksums/src/simd_batch/md4/simd/{avx2,avx512,sse2,neon,wasm}.rs`
- `crates/checksums/src/strong/md5.rs`
- `crates/checksums/src/strong/md4.rs`
- `crates/checksums/src/strong/strategy/{trait_def.rs,impls.rs,selector.rs}`
- `crates/checksums/src/parallel/files.rs`
- `crates/signature/src/algorithm.rs`
- `crates/transfer/src/receiver/quick_check.rs`
- `crates/transfer/src/delta_apply/checksum.rs`
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs`
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs`
- `crates/engine/src/concurrent_delta/parallel_apply/batch.rs`

### Upstream rsync

- `target/interop/upstream-src/rsync-3.4.1/checksum.c` (`file_checksum`, `get_checksum2`)
- `target/interop/upstream-src/rsync-3.4.1/lib/md5.c`
- `target/interop/upstream-src/rsync-3.4.1/lib/mdfour.c`

### Companion audits

- `docs/audits/csm-4-strong-checksum-upstream-parity.md` (parent ranking)
- `docs/audits/checksum-mode-computation-cost.md` (CSP audit; mmap + buffer-pool tracks)
