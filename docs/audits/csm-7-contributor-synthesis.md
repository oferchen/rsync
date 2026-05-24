# CSM-7 - `--checksum` contributor synthesis and CSM-8 fix order

Date: 2026-05-24
Scope: docs-only synthesis. No `.rs` edits.
Tracked under: CSM-7 (parent CSM track: `--checksum` mode is ~1.5-1.7x slower
than upstream rsync 3.4.1; upstream issue #970).
Inputs:
- CSM-4 - `docs/audits/csm-4-strong-checksum-upstream-parity.md`
- CSM-5 - `docs/audits/csm-5-c-io-pattern.md`
- CSM-6 - `docs/audits/csm-6-c-simd-coverage.md`
Bench harness: `scripts/benchmark_checksum_mode.sh` (CSM-1, PR #2764).
Pending inputs at synthesis time: CSM-2 (oc-rsync profile), CSM-3 (upstream
syscall comparison). Their absence is called out where it changes the call.

## 1. Goal

Pick a single ranked list of contributors across all three audits, recommend
the CSM-8 fix order, reconcile the one cross-audit conflict (CSM-4 vs CSM-5
on I/O severity), and identify the CSM-2/3 evidence required before CSM-9
can claim `<= 1.05x upstream`.

## 2. Unified contributor table

Rows are distinct gaps named in CSM-4, CSM-5, or CSM-6. Where the same gap
was named twice, the row carries both audit IDs. Perf-impact bucket reflects
the synthesis verdict, not any single audit's wording (see section 3 for the
one reconciliation).

| # | Gap | Source | Perf bucket | Wire-byte compat | Impl cost | CSM-8 fix sketch |
| --- | --- | --- | --- | --- | --- | --- |
| G1 | Default-build MD5/MD4 backend is pure-Rust `md-5`/`md4` (~500 MB/s); upstream default is OpenSSL EVP (~1 GB/s, ~3 GB/s with SHA-NI on supported CPUs) | CSM-4 O7 / 2.1 | CRITICAL | yes | M | Default-enable the `openssl` cargo feature on platforms that ship OpenSSL; or adopt an asm-accelerated pure-Rust MD5 backend |
| G2 | `simd_batch` MD5/MD4 dispatchers (4/8/16-lane AVX2/AVX-512/NEON) implemented but unwired from any `-c` consumer; all four `-c` sites (C1/C2/C3/C4) hash single-stream | CSM-4 O4 / CSM-6 C1+C2+C3 | HIGH (small/medium files), bounded by I/O at large | yes | M | Wire `md5_digest_batch` / `md4_digest_batch` into C2 (local-copy prefetch) and C1 (receiver quick-check) using `compute_truncated_batch`-style lane-aware sub-batching |
| G3 | Receiver `quick_check.rs::file_checksum_matches` reads via 64 KiB stack buffer (4x upstream's 256 KiB window); 16 reads/MiB vs upstream's 4 reads/MiB | CSM-4 O1 / CSM-5 F1 | MEDIUM (was HIGH; see section 3) | yes | S | Grow buffer to 256 KiB, source from a `BufferPool` rather than stack |
| G4 | Local-copy `parallel_checksum.rs::hash_file_contents` uses 128 KiB pooled buffer (2x upstream); 8 reads/MiB vs 4 reads/MiB | CSM-5 F2 | LOW | yes | S | Pass `buffer_size: 256 * 1024` through `GlobalBufferPoolConfig` when `-c` is active, or take a one-off 256 KiB pool inside the prefetch caller |
| G5 | Local-copy path issues an extra `fstat`/`statx` per file via `file.metadata()` in `compute_file_checksum` | CSM-5 F3 | LOW | yes | S | Drop the `metadata()` call; pass known size through `FilePair::source_size`/`destination_size` (already known to caller) |
| G6 | Receiver path lacks `BufferPool` reuse - zero-initialises a fresh 64 KiB stack frame per call | CSM-4 O2 / CSM-5 F8 | LOW | yes | S | Subsumed by G3 once the buffer moves to a pool |
| G7 | `DoubleBufferedReader` pipelined I/O exists but is not wired into `-c`; would overlap `read()` with hashing | CSM-4 O6 | MEDIUM (only relevant on pure-Rust MD5 where compute >= I/O; collapses once G1 lands) | yes | M | Wire `crates/checksums/src/pipelined::DoubleBufferedReader` into both `-c` consumers |
| G8 | Streaming XXH3 uses `xxhash-rust` (~10-20% slower on AVX2 than reference); one-shot already uses faster `xxh3` crate | CSM-4 2.3 / O8 | LOW (only files above `max_memory_file_size = 1 MiB` on XXH3-negotiated peers) | yes | M | Replace `xxhash-rust` streaming with a streaming wrapper over the `xxh3` crate or vendored libxxhash |
| G9 | Per-call `ChecksumVerifier` enum-dispatch and per-file fresh `Md5::new()` allocation; upstream uses `static md_context`/`XXH*_state_t` per process | CSM-4 O3 / 2.1 | LOW (<5%; in the noise vs G1/G2) | yes | M | Cache hashers per worker thread or thread-local-init; defer until G1/G2 land |
| G10 | `crates/checksums/src/parallel/files.rs::hash_file_internal` is mmap-capable but unwired (no `-c` consumer) | CSM-4 O5 / CSM-5 F9 / CSM-6 C4 | INFORMATIONAL / not a fix on its own | yes | n/a | Do not wire mmap into `-c`. Upstream deliberately avoids `mmap` for SIGBUS safety (`fileio.c:214-217`). Match upstream by growing the `read` window (G3/G4) |
| G11 | Per-chunk `ParallelDeltaApplier::verify_chunk` (BR-3i.c, gated behind `parallel-receive-delta`) uses single-stream MD5; rayon fans across chunks but each chunk stays 1-lane | CSM-6 C3 | LOW (feature-gated, off by default) | yes | M | Add `ChecksumStrategy::compute_batch`; specialise `Md5Strategy` / `Md4Strategy` to call `simd_batch::digest_batch` |

Items that the audits explicitly **excluded** from the perf gap (kept here so
future readers don't re-litigate them):

- MD5/MD4/XXH3/XXH64 byte-equivalence: confirmed; RFC vectors pass (CSM-4 6).
- `-c` seed threading: confirmed unseeded both sides (CSM-4 3).
- Real `mmap` adoption: rejected; matches upstream's deliberate `read`-window
  design (CSM-5 F4, CSM-6 4.4).
- `O_NOATIME`, `posix_fadvise(SEQUENTIAL)`, `madvise(SEQUENTIAL)`: upstream
  omits all three; no oc-rsync delta (CSM-5 F5, F7).
- io_uring for `-c`: rejected at this stage; upstream is blocking-`read`, and
  CSM-6 ranks `simd_batch` wiring as the higher-impact wiring change (CSM-5 8).
- SHA1/SHA256/SHA512 OpenSSL backend gap: out of scope; default `--checksum`
  negotiates MD5/MD4/XXH3, not SHA-family (CSM-4 2.5).

## 3. Reconciliation: CSM-4 "I/O HIGH" vs CSM-5 "I/O MEDIUM"

CSM-4 ranked I/O pattern (O1) as HIGH on the assumption that upstream
operates on a real `mmap` ("zero syscalls after `mmap`"). CSM-5 read
upstream's `fileio.c:214-217` and quantified the truth:

- Upstream's `map_file()` / `map_ptr()` is a **read-based sliding window**,
  not POSIX `mmap`. The name misleads.
- The window stride is 256 KiB (`MAX_MAP_SIZE`), so upstream issues 4
  reads/MiB, not zero.
- oc-rsync's receiver path issues 16 reads/MiB (64 KiB buffer); the
  local-copy path issues 8 reads/MiB (128 KiB buffer).
- Cost model: ~500 ns per `read(2)` from warm page cache. Receiver overhead
  is +12 reads/MiB = ~6 us/MiB; local-copy is +4 reads/MiB = ~2 us/MiB.
- On pure-Rust MD5 (~500 MB/s, ~2 ms/MiB), the 6 us is ~0.3% of total hash
  time per file. On OpenSSL EVP MD5 with SHA-NI (~3 GB/s, ~340 us/MiB), 6 us
  is ~1.8%.

**Verdict: adopt CSM-5's MEDIUM rating for G3.** CSM-4's CRITICAL/HIGH
framing was correct on the dominant contributor (G1) but overstated the I/O
ranking by assuming `mmap`. G3 is still worth fixing because the change is a
1-line buffer-size bump (S cost) and the savings stack with G1 once OpenSSL
EVP / SHA-NI lands; but G3 alone will not close the gap.

This reconciliation does NOT change CSM-4's top-of-list verdict (G1 remains
CRITICAL) or CSM-4's #2 rank for `simd_batch` (G2 remains HIGH for small/
medium trees). It only adjusts CSM-4's #3 ranking.

## 4. Recommended CSM-8 fix order

Ordering rules:

1. Land wire-byte-compatible wins first (all eleven entries qualify).
2. Front-load highest-impact / lowest-cost combinations.
3. Defer anything that requires CSM-2/3 profile evidence to confirm.
4. Bundle low-cost neighbours into the same change to amortise review.

**Recommended order:**

1. **G1 - default-enable OpenSSL EVP MD5/MD4 on platforms that ship OpenSSL.**
   Highest single contributor (~1.8-2x on MD5-heavy `-c`). Cost: M (Cargo
   feature wiring + build-matrix tweaks; OpenSSL crate already permitted from
   `checksums`). Lands first because every downstream item's impact is read
   off the OpenSSL baseline.
2. **G2 - wire `simd_batch` MD5/MD4 into C2 (local-copy `-c` prefetch).**
   Local-copy is the mechanical win: `prefetch_checksums` already collects
   `FilePair`s and runs `par_iter`. Inserting a chunked batch dispatcher
   that calls `md5_digest_batch` per lane-width-sized chunk is mostly a
   restructure of the existing loop. Expected: 3-7x per-core on AVX2/AVX-512
   small/medium files, bounded by I/O. Cost: M.
3. **G3 + G4 + G5 + G6 - I/O-window normalisation bundle.** Grow C1 to
   256 KiB pooled, grow C2 to 256 KiB, drop the C2 `metadata()` syscall,
   retire the per-call stack zero-init. All four are 1-3 line changes that
   share the same reviewer context (the two `-c` rehash sites). Expected:
   3-4 us/MiB recovered cumulatively. Cost: S each. Bundling matches the
   CSM-5 8 "ride-along" guidance.
4. **G2 - wire `simd_batch` MD5/MD4 into C1 (receiver `-c` quick-check).**
   Receiver path needs a new batch-collection helper (`file_checksums_match_batch`)
   that windows N candidates before dispatch. Higher impl cost than C2
   wiring because the receiver loop is sequential per-file today (CSM-6 7.1).
   Expected: same per-core multiplier as #2 but applies to the network
   path. Cost: M.
5. **G7 - pipelined I/O (`DoubleBufferedReader`) for both `-c` consumers.**
   Defer until after G1. If G1 lands and the digest backend reaches OpenSSL
   speed, pipelined-I/O wins shrink to ~5-10% (`I/O ~= compute` instead of
   `compute > I/O`). CSM-2 profile data is what confirms whether this is
   still worth the M cost. Mark this item "blocked on CSM-2".
6. **G8 - replace `xxhash-rust` streaming.** Only matters for XXH3-negotiated
   peers with files above `max_memory_file_size = 1 MiB`. CSM-2/3 will quantify
   how often this path actually fires in the benchmark trees. Cost: M.
7. **G9 - hasher caching / per-worker static contexts.** <5% expected; defer
   until measurement confirms it shows in the CSM-2 profile. Cost: M.
8. **G11 - `ChecksumStrategy::compute_batch` for the parallel-apply path.**
   Feature-gated (`parallel-receive-delta`), off by default. Land last. Cost: M.

**Not addressed by CSM-8** (per audit guidance):

- G10 - leave `parallel/files.rs::hash_file_internal` alone; do not wire mmap
  into `-c`. Match upstream by growing read windows.
- `mmap`-for-`-c` adoption - rejected at CSM-5 8.
- `io_uring`-for-`-c` adoption - deferred indefinitely (CSM-5 8); revisit
  only if CSM-9 cannot reach `<= 1.05x upstream` after items 1-4.

### 4.1 Top 3 (the report-back list)

1. **G1** - default-enable OpenSSL EVP MD5/MD4. CRITICAL, S/M cost,
   wire-compatible.
2. **G2 (C2 wiring)** - route local-copy `-c` prefetch through
   `md5_digest_batch` / `md4_digest_batch`. HIGH on small/medium trees,
   M cost.
3. **G3+G4+G5+G6 bundle** - normalise the `-c` read window to 256 KiB,
   drop the extra `metadata()` syscall, retire the stack zero-init.
   MEDIUM cumulatively, S per item.

## 5. What CSM-2/3 must confirm before CSM-9 claims `<= 1.05x upstream`

CSM-2 (oc-rsync profile via `perf` / `samply`) and CSM-3 (upstream syscall
comparison via `strace -c` / `dtruss`) are pending. The synthesis above is
defensible without them, but the following CSM-9 sign-off items require their
output:

- **C2.1 Compute-vs-I/O split.** What fraction of `-c` wall time is in
  `md5_compress` on the default oc-rsync build? CSM-7's claim that G1 is the
  dominant contributor rests on the assumption that MD5 compute dominates;
  if the profile shows >50% of time in `read`/page-fault handling, G3/G4 move
  up the list.
- **C2.2 Per-bucket profile.** Section 8 of CSM-6 gives expected `-c`
  speedups per file-size bucket (small/medium/large). CSM-2 must confirm
  the bucket mix in the benchmark trees and the actual hash:I/O ratio per
  bucket. Without this, the "3-5x on AVX2 for medium" estimate is theory.
- **C3.1 Syscall counts.** CSM-5 derived oc-rsync's `read()` count from the
  source. CSM-3 must verify it with `strace -c oc-rsync -c ...` against the
  same tree on the same host, and compare to `strace -c rsync -c ...`. The
  expected ratio is 4x (receiver path) or 2x (local-copy path); a different
  ratio invalidates the cost model.
- **C3.2 OpenSSL vs `md-5` per-call cost.** CSM-3 should run upstream against
  the same tree with `--openssl-md5=off` and with the default OpenSSL build;
  the delta between those runs is the empirical upper bound on G1's expected
  win. CSM-4 estimated 1.8-2x from published throughput tables; CSM-3 must
  confirm.
- **C3.3 Tree-scale syscall amortisation.** CSM-5 4.2 projects 3x syscall
  traffic for the receiver on 10k-file 1 MiB-average trees. CSM-3 must
  confirm that the per-file `open`/`close` cost does not dominate small-file
  buckets; if it does, the prefetch design in G2 needs to amortise opens
  too.

CSM-9 should not claim `<= 1.05x upstream` until C2.1, C2.2, C3.1, C3.2 are
all on disk. C3.3 is required only if the bench tree skews to small files.

## 6. Deferred / rejected items (one-sentence rationale)

- **Real POSIX `mmap` for `-c`** (CSM-4 O5, CSM-5 F9) - rejected; upstream
  deliberately avoids `mmap` for SIGBUS safety (`fileio.c:214-217`); matching
  upstream's sliding-window `read` is the cheaper, safer move.
- **`io_uring` `READ_FIXED` for `-c`** (CSM-5 8) - deferred; upstream uses
  blocking `read` here, and `simd_batch` wiring delivers more per unit of
  reviewer time than wiring `fast_io::IoUringReadFixed` into the rehash loop.
- **SHA-NI / SHA1 OpenSSL backend** (CSM-4 2.5) - out of scope; default
  `--checksum` negotiates MD5/MD4/XXH3, and `--checksum-choice=sha1` is a
  rare-enough flag that closing that gap is a separate workstream.
- **MD5-asm pure-Rust port** (CSM-4 2.1) - deferred; G1's OpenSSL EVP path
  already routes through the platform's tuned MD5 (and SHA-NI where the
  silicon supports it), so an asm port duplicates work G1 already covers.
- **`hash_file_internal` as shared `-c` body** (CSM-6 7.4) - optional;
  consider only if the C1/C2 wiring (#2 and #4 above) ends up duplicating
  enough code to justify a shared helper.
- **CSUM_MD4_OLD / BUSTED / ARCHAIC parity** (CSM-4 3.3) - rejected; these
  fire only on protocol 27-29 per-block paths, never on `-c` whole-file.
- **`O_NOATIME` / `posix_fadvise(SEQUENTIAL)` / `madvise(SEQUENTIAL)`**
  (CSM-5 F5, F7) - rejected; upstream omits all three, so adopting them
  would be a divergence rather than a parity fix.

## 7. References

### CSM inputs

- `docs/audits/csm-4-strong-checksum-upstream-parity.md`
- `docs/audits/csm-5-c-io-pattern.md`
- `docs/audits/csm-6-c-simd-coverage.md`
- `scripts/benchmark_checksum_mode.sh` (CSM-1, PR #2764)

### oc-rsync sites named in the synthesis

- `crates/transfer/src/receiver/quick_check.rs:262` (C1, G3, G6)
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:148`
  (C2, G4, G5)
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:133`
  (G5 `compute_file_checksum`)
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:627` (C3, G11)
- `crates/checksums/src/simd_batch/mod.rs` (G2 dispatcher)
- `crates/checksums/src/simd_batch/md4/mod.rs` (G2 dispatcher)
- `crates/checksums/src/strong/openssl_support.rs` (G1)
- `crates/checksums/src/pipelined/mod.rs` (G7)
- `crates/checksums/src/parallel/files.rs:22` (G10; intentionally unwired)

### Upstream rsync

- `target/interop/upstream-src/rsync-3.4.1/checksum.c:402` `file_checksum`
- `target/interop/upstream-src/rsync-3.4.1/fileio.c:214-315` `map_file` /
  `map_ptr`
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:158-159` `CHUNK_SIZE`,
  `MAX_MAP_SIZE`
