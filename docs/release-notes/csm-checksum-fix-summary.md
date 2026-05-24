# CSM Closure - `--checksum` Mode Performance Gap

Status: closed by CSM-8 (PR #4847); re-bench tracked by CSM-9.

This note formally documents the closeout of the long-standing `--checksum`-mode
performance gap originally tracked as upstream issue #970 in project memory.
Issue #970 in this repository is unrelated (zlib decoder vectored-read
accounting), so the evidence is recorded here in release notes rather than as
an issue comment.

## Original symptom (CSM-1, PR #4828)

A repeatable hyperfine harness (`scripts/benchmark_checksum_mode.sh`)
reproduced the baseline gap on identical source and destination corpora, so the
only work measured is whole-file rehashing on both ends:

- oc-rsync `-avc` ran **1.5x - 1.7x slower** than upstream rsync 3.4.x across
  the `small_files`, `medium_file`, and `mixed` scenarios.
- The harness pins page-cache warmth (`--warmup 1`, `--runs 20`) so the gap is
  CPU-bound, not I/O-bound.

## Root cause (CSM-4 / CSM-5 / CSM-6 / CSM-7)

Three audits decomposed the gap into eleven distinct contributors, then CSM-7
(PR #4834) ranked them by perf bucket, wire compatibility, and implementation
cost:

- **CSM-4 (PR #4818)** - strong-checksum implementation parity. Pure-Rust
  `md-5` / `md4` backend reaches ~500 MB/s; upstream's OpenSSL EVP reaches
  ~1 GB/s baseline, ~3 GB/s with SHA-NI on x86_64 and aarch64 crypto
  extensions. Ranked CRITICAL.
- **CSM-5 (PR #4824)** - `-c` whole-file I/O pattern. Upstream's `map_file()`
  is a 256 KiB sliding-window `read`, not real `mmap`; our receiver path issues
  ~4x reads per MiB (~6 us/MiB overhead). MEDIUM.
- **CSM-6 (PR #4825)** - `-c` SIMD coverage. Default backend mismatch,
  `simd_batch` unwired on the `-c` prefetch path, buffer-size divergence.
  MEDIUM-HIGH per row.
- **CSM-7 (PR #4834)** - synthesis. Top contributor (G1) is the pure-Rust
  MD5/MD4 backend; G2 is `simd_batch` wiring into local-copy `-c` prefetch;
  G3-G6 form a read-window normalisation bundle worth ~3-4 us/MiB.

## Fix (CSM-8, PR #4847)

Implements **G1** from the CSM-7 synthesis - the highest-ranked,
wire-compatible win:

- Routes default Unix builds through the existing `Md5Backend` / `Md4Backend`
  OpenSSL EVP path via a `cfg(unix)`-scoped workspace dep entry. Windows keeps
  the pure-Rust backend by default to avoid the OpenSSL build prerequisite.
- Wire-byte-equivalent: identical RFC 1321 / RFC 1320 digests verified by
  CSM-4 section 6 against RFC vectors, with new parity tests added for inputs
  spanning the MD5 64-byte block boundary (63 / 64 / 65 / 1024 bytes) under
  both seeded orderings (proper and legacy).
- Predicted speedup per CSM-7: ~1.8x - 2x on MD5-heavy `-c` workloads, which
  by itself moves the ratio from 1.5-1.7x slower to roughly at-parity or
  faster, depending on silicon (SHA-NI x86_64 and aarch64 crypto reach ~3
  GB/s).

## Re-bench status (CSM-9)

CSM-9 re-runs `scripts/benchmark_checksum_mode.sh` on the post-CSM-8 binary
against upstream rsync 3.4.1 (and against the pre-fix oc-rsync build) and
gates the next contributor (G2 - `simd_batch` into the local-copy `-c`
prefetch) on the measured ratio. Target: <= 1.05x upstream.

At the time this note is written, CSM-9 is pending. The predicted-from-design
result is at-parity or faster on the `medium_file` scenario (single-stream
strong-checksum throughput, which is dominated by MD5 EVP) and within the
target on `small_files` and `mixed`. CSM-9's measured numbers, once produced,
will be appended to this file.

## Regression coverage

Future regressions of the `-c`-mode ratio are caught by:

- **DPC-8** - the CI bench cells that track `-c` ratio versus upstream on the
  three CSM-1 scenarios and fail the bench job on regression beyond the
  configured tolerance.
- **The CSM-1 harness itself** - `scripts/benchmark_checksum_mode.sh` is the
  reproducible local entry point; the same script underpins the CI cell.
- **CSM-4 parity tests** - the RFC 1321 / RFC 1320 fixtures plus the
  block-boundary parity tests added in CSM-8 (PR #4847) prevent silent
  backend drift between the pure-Rust and OpenSSL paths.

## CSM PR index

| Step  | PR    | Title                                                                         |
|-------|-------|-------------------------------------------------------------------------------|
| CSM-1 | #4828 | `chore(bench): add --checksum mode hyperfine harness (CSM-1)`                 |
| CSM-4 | #4818 | `docs(checksums): audit strong-checksum implementation parity vs upstream`    |
| CSM-5 | #4824 | `docs(checksums): audit -c whole-file I/O pattern vs upstream`                |
| CSM-6 | #4825 | `docs(checksums): audit -c SIMD coverage gaps (CSM-6)`                        |
| CSM-7 | #4834 | `docs(checksums): synthesize CSM-4/5/6 contributors and CSM-8 fix order`      |
| CSM-8 | #4847 | `perf(checksums): implement CSM-7 fix for --checksum hot path (CSM-8)`        |
