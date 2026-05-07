# Parallel signature computation scaling profile (#1085)

Profile plan to characterise how the parallel signature path scales with
thread count, before tuning the dispatch threshold or worker cap.

## 1. Parallel signature path

Sources live in `crates/signature/src/`:

- `parallel.rs:84-161` - `generate_file_signature_parallel` reads every
  block into a `Vec<Vec<u8>>`, then drives `par_chunks(BATCH_SIZE = 16)`
  through `RollingDigest::from_bytes` and
  `SignatureAlgorithm::compute_truncated_batch` for SIMD-batched strong
  digests.
- `parallel.rs:172` - `PARALLEL_THRESHOLD_BYTES = 256 KiB` gates
  `generate_file_signature_auto` (`parallel.rs:207-229`).
- `block_size.rs`, `layout.rs` - block-size selection and layout maths
  feeding both sequential and parallel paths.

The SIMD batching landed under #1024 (rolling+strong batch APIs in
`checksums::strong`); this audit picks up where that one left off and
asks whether the rayon outer loop scales linearly on top of those SIMD
inner kernels.

## 2. Question

Does throughput scale linearly with worker thread count, or does it
saturate? Two competing hypotheses:

- **Compute-bound**: strong-digest cost dominates, so wall-time falls
  as `1/N` until cores run out.
- **Memory-bandwidth-bound**: every block is streamed once from RAM, so
  aggregate throughput plateaus once the LLC-to-DRAM bus is saturated -
  typically 6 to 10 cores on a single-socket x86 workstation.

Identifying the regime decides whether oversubscribing rayon helps
(compute-bound) or hurts via cache thrash and context switches
(bandwidth-bound).

## 3. Profile plan

Criterion bench (`crates/signature/benches/parallel_scaling.rs`, new):

- **File**: a single 100 MiB buffer of `/dev/urandom` data, held in a
  `Vec<u8>` so the harness measures compute, not I/O.
- **Block sizes**: 4 KiB, 8 KiB, 16 KiB - spanning typical layout
  outputs for files in the 1 MiB to 1 GiB range.
- **Thread counts**: 1, 2, 4, 8, 16 via
  `rayon::ThreadPoolBuilder::num_threads(n).build()` so each iteration
  uses an isolated pool.
- **Algorithms**: `Md5` (CPU-heavy) and `Xxh3` (memory-bound) to
  separate the two regimes.

Run inside the `rsync-profile` podman container, pinned with
`taskset -c 0-15`. Capture
`perf stat -e cycles,instructions,cache-misses,LLC-loads,LLC-load-misses`
per thread count to corroborate the criterion wall-time curve.

## 4. Suspected ceiling

Memory-bandwidth saturation around 8 threads on commodity hardware.
With Md5 the strong digest costs roughly 6 cycles per byte, so a 4 GHz
core processes about 670 MiB/s; eight cores demand 5.3 GiB/s, near
DDR4-3200 dual-channel limits. Xxh3 is roughly 4x faster per byte and
should hit the wall sooner, around 4 threads. Beyond the ceiling adding
threads burns scheduler cycles for no throughput gain.

## 5. Recommendation

- **Cap rayon threads** for the signature path at
  `min(num_cpus, MAX_SIGNATURE_THREADS)` once profiling locates the
  ceiling. Initial guess: `MAX_SIGNATURE_THREADS = 8`.
- **Document `PARALLEL_SIGNATURE_THRESHOLD`** as the canonical name for
  the env override mirroring `OC_RSYNC_PARALLEL_THRESHOLD` for delta
  dispatch, and rename `PARALLEL_THRESHOLD_BYTES` (`parallel.rs:172`)
  in lockstep. Today the 256 KiB constant is asserted only in
  `parallel_threshold_constant_is_reasonable` (`parallel.rs:437-443`)
  without empirical justification.

Decision rules:

| Observation | Action |
|-------------|--------|
| Md5 throughput scales linearly to 16 threads | Drop the cap; widen scaling tests to 32. |
| Md5 plateaus at 8 threads, Xxh3 at 4 | Set `MAX_SIGNATURE_THREADS = 8`; note regime in module docs. |
| 16-thread run regresses 8-thread by > 10% | Cap at 8 immediately and gate via env var. |
| Sub-linear below 4 threads at 4 KiB blocks | Raise `PARALLEL_SIGNATURE_THRESHOLD` to 1 MiB. |

Profile artifacts (criterion JSON, `perf stat` output, plot SVGs)
attach to #1085 once the bench lands.
