# `--checksum` mode synchronisation regression diagnosis

Tracks task #970. Diagnoses the standing wall-clock regression in
`--checksum` (`-c`) mode against upstream rsync 3.4.1 and lays out the
measurement plan needed to close the gap.

## 1. Known regression

`--checksum` runs against trees of many small-to-medium regular files
trail upstream rsync 3.4.1 by a measurable margin on cold-cache
benchmarks. The receiver-side hot path was profiled in the prior
`docs/audits/checksum-mode-computation-cost.md` audit (task #1041).
Its findings still hold:

- Receiver `quick_check_matches -> file_checksum_matches`
  (`crates/transfer/src/receiver/quick_check.rs:225`) hashes files
  sequentially with a 64 KiB stack buffer, half upstream's 128 KiB
  CHUNK_SIZE feed and a quarter of `MAX_MAP_SIZE = 256 KiB`. The
  follow-up I/O-pattern audit (task #1043,
  `docs/audits/checksum-io-pattern-vs-upstream.md`) measured the
  consequence at 4x the `read()` syscall count per GiB.
- The sender flist builder
  (`crates/transfer/src/generator/file_list/entry.rs`) never calls an
  equivalent of upstream `flist.c:1412 file_checksum`, so wire-mode
  pulls emit zeroed digests and force the receiver into the delta
  path even when content matches. This is the largest single
  contributor to the regression on wire transfers.
- The local-copy executor
  (`crates/engine/src/local_copy/executor/directory/parallel_checksum.rs`)
  builds a fresh hasher per file and bypasses the
  `simd_batch::digest_batch` AVX2/NEON dispatcher and the
  `pipelined::DoubleBufferedReader` helper, both of which are wired
  for transfer-stage signatures only.

#1041 ranked four wire-compatible reductions (cache reuse, SIMD batch
MD5/MD4, mmap-first hashing, EVP/`openssl_support` engagement,
double-buffered I/O). None have shipped yet, so the regression
persists.

## 2. Likely causes

The regression decomposes into three orthogonal contributors. Each
needs its own measurement before a fix lands.

### 2.1 Per-file fixed overhead

Every regular file pays a fresh `fs::File::open`, a fresh
`ChecksumVerifier` algorithm dispatch, a stack zeroing of the 64 KiB
buffer, and a `Drop` close. None of this work is amortised across
files. On trees of millions of small files the fixed cost dominates
the per-byte cost. Upstream amortises some of this via static digest
contexts (`XXH3_state_t` reset per file) and `map_file` buffer reuse;
oc-rsync currently pays the full setup tax on each entry.

### 2.2 Unparallelised hashing on the receiver

`crates/transfer/src/receiver/transfer/candidates.rs:111` filters
candidates one file at a time. The `parallel_checksum::ChecksumCache`
helper that the local-copy executor uses to drive `rayon::par_iter`
hashing exists but is not invoked from the wire receiver. The
`simd_batch` dispatcher, which can hash 4-, 8-, or 16-file lanes per
core for MD4/MD5, is wired nowhere in the `--checksum` path. Result:
single-core hashing throughput caps the receiver well below NVMe read
throughput on hosts with > 4 cores.

### 2.3 Sender vs receiver direction mismatch

Upstream pays the cost twice (sender hashes during `make_file`,
receiver hashes during quick-check) and amortises by skipping the
delta path entirely on match. oc-rsync as sender emits zero digests,
so the receiver always falls into delta. As receiver against an
upstream sender, oc-rsync hashes once and saves work; as sender to an
upstream receiver, oc-rsync forces the upstream peer to retransfer.
The asymmetry produces direction-dependent regressions: pull from
upstream is roughly at parity, push to upstream is materially slower.
The same gap is visible in `crates/protocol/src/flist/write/encoding.rs:288`
where the writer falls back to writing `flist_csum_len` zero bytes
when the entry has no payload.

## 3. Algorithm selection vs upstream

Cross-references task #1042 (algorithm selection vs upstream). The
`ChecksumFactory::from_negotiation` path and the strong-digest
backends in `crates/checksums/src/strong/{md4,md5,xxhash}.rs` decide
which algorithm runs for `--checksum` based on the negotiated
checksum-choice plus compat flags. Two upstream-divergent choices
matter for the regression:

- Upstream prefers `file_sum_evp_md` when `USE_OPENSSL` is set,
  picking up AES-NI / SHA-NI / ARMv8 Crypto Extensions for MD5 and
  SHA family digests. oc-rsync routes through pure-Rust `Md5` even
  when `crates/checksums/src/strong/openssl_support.rs` is compiled
  in. Per-byte hashing is correspondingly slower on hosts where
  hardware extensions are present.
- xxh3/xxh128 dispatch from `xxhash-rust` matches upstream's xxHash
  vendoring, but oc-rsync builds a fresh `Xxh3` instance per file
  rather than resetting a static context. Init cost is small but
  visible in the per-file fixed overhead bucket.

#1042 should land alongside the work prompted by this diagnosis so
algorithm cost and per-file overhead can be moved together.

## 4. Recommendation

Measure first, then fix. The two regressions identified above are
plausible but not yet quantified at scale on this codebase. Land the
following before changing any production code:

### 4.1 Criterion microbench

Add a `crates/checksums/benches/checksum_mode_per_file.rs` Criterion
suite that exercises:

- Per-file fixed cost: fixture of N empty files vs N x 4 KiB files
  vs N x 64 KiB files, single-threaded receiver path.
- Per-byte cost: 1 MiB, 16 MiB, 256 MiB single-file runs across
  MD4 / MD5 / XXH3 / XXH128, with and without
  `openssl_support` engaged.
- Direction asymmetry: sender flist build path (currently the no-op
  zero-fill) vs receiver quick-check path, on the same fixture.

The bench runs locally per developer and in the existing
`benchmark.yml` workflow. Any subsequent fix is gated on Criterion
deltas remaining within noise on the unaffected scenarios.

### 4.2 Flamegraph at 10K and 100K files

Run `oc-rsync --checksum -avr src/ dst/` against synthetic trees of
10K and 100K small files (mean size 4 KiB) and 1K x 16 MiB files,
under `cargo flamegraph` (Linux container `rsync-profile`). Capture
both directions: oc-rsync sender + upstream receiver, and oc-rsync
receiver + upstream sender. Compare against an upstream-only baseline
captured in the same container.

The flamegraph should expose:

- Time spent in `file_checksum_matches` vs `Md5::update` vs `read`.
- Whether sender-side `make_file` work is missing entirely (expected,
  per #1041 section 2).
- Whether `BufferPool` contention shows up at 100K files.
- Whether the receiver's single-thread hashing saturates one core
  while peers idle.

Once both views are in hand, prioritise the #1041 reductions by
measured cost rather than by guess. Re-audit when the sender flist
hash population from #1041 lands, since it changes the wire shape and
invalidates the current receiver-only flamegraphs.

## References

- `docs/audits/checksum-mode-computation-cost.md` (#1041)
- `docs/audits/checksum-io-pattern-vs-upstream.md` (#1043)
- `crates/transfer/src/receiver/quick_check.rs`
- `crates/transfer/src/receiver/transfer/candidates.rs`
- `crates/transfer/src/generator/file_list/entry.rs`
- `crates/transfer/src/generator/mod.rs`
- `crates/protocol/src/flist/write/encoding.rs`
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs`
- `crates/checksums/src/parallel/files.rs`
- `crates/checksums/src/simd_batch/mod.rs`
- `crates/checksums/src/pipelined/mod.rs`
- `crates/checksums/src/strong/openssl_support.rs`
- `target/interop/upstream-src/rsync-3.4.1/checksum.c`
- `target/interop/upstream-src/rsync-3.4.1/flist.c`
