# Zsync SIMD rolling-hash parity verification (task #2077)

Verification that the existing SIMD parity coverage for the rolling
checksum is still complete after the zsync-inspired matching work
(#2059-#2087) landed. The audit mirrors the inventory style used in
[`docs/audits/zsync-cleanup-audit.md`](zsync-cleanup-audit.md) (the
#2083/#2084/#2085 follow-up) and produces an explicit verdict from
file-level evidence; no code changes are proposed.

Companion documents:

- [`docs/audits/zsync-cleanup-audit.md`](zsync-cleanup-audit.md) -
  prior zsync follow-up verification.
- [`docs/audits/zsync-golden-byte-stability.md`](zsync-golden-byte-stability.md) -
  wire-byte stability proof for the four zsync optimizations.
- [`docs/design/zsync-inspired-matching.md`](../design/zsync-inspired-matching.md) -
  parent design note.

## 1. Inventory: SIMD rolling-hash paths

The rolling checksum (rsync's weak `rsum`, Adler-32 style) lives in
`crates/checksums/src/rolling/`. The architecture dispatch ladder is
described in the module doc-comment at
`crates/checksums/src/rolling/checksum/mod.rs:1-22`, and the runtime
selector is `accumulate_chunk_dispatch` at
`crates/checksums/src/rolling/checksum/mod.rs:519-530`.

| Path | Source | Key entry point | Block size |
|------|--------|-----------------|------------|
| AVX2 (x86 / x86_64) | `crates/checksums/src/rolling/checksum/x86.rs` | `accumulate_chunk_avx2` at `x86.rs:191-231` (dispatched via `try_accumulate_chunk` at `x86.rs:106-125`) | 32 bytes/iter |
| SSE2 (x86 / x86_64) | `crates/checksums/src/rolling/checksum/x86.rs` | `accumulate_chunk_sse2` at `x86.rs:142-185` | 16 bytes/iter |
| NEON (aarch64) | `crates/checksums/src/rolling/checksum/neon.rs` | `accumulate_chunk_neon_impl` at `neon.rs:93-139` (dispatched via `accumulate_chunk` at `neon.rs:81-91`) | 16 bytes/iter |
| Scalar fallback | `crates/checksums/src/rolling/checksum/mod.rs` | `accumulate_chunk_scalar_raw` at `mod.rs:567-599` | 4-byte unrolled loop |

The dispatch entry point common to every architecture is
`accumulate_chunk_dispatch` at
`crates/checksums/src/rolling/checksum/mod.rs:519-530`, which all callers
reach through `RollingChecksum::update` at `mod.rs:163-169`. Each SIMD
path tail-calls into `accumulate_chunk_scalar_raw` for any trailing
bytes that do not fill a full SIMD lane (AVX2 at `x86.rs:218-228`, SSE2
at `x86.rs:177-182`, NEON at `neon.rs:131-136`), so the scalar fallback
is on the wire for every input shorter than one full SIMD block.

The runtime feature gate that controls which SIMD variant is permitted
to engage is the `--simd` CLI override defined in
`crates/checksums/src/cpu_features.rs:105-238`. The x86 module reads
both CPUID and the CLI gate via `effective_features` at
`crates/checksums/src/rolling/checksum/x86.rs:90-97`; the NEON module
combines CPUID and the gate via `neon_enabled` at `neon.rs:71-74`.
Comments at `crates/checksums/src/cpu_features.rs:19, 50, 108, 114, 116`
explicitly call out the rolling checksum's use of each dispatch tier.

## 2. Inventory: parity tests

Two parity surfaces exist for the rolling checksum:

### 2a. Unit parity at the SIMD-implementation boundary

File: `crates/checksums/src/rolling/tests/checksum/simd.rs`. Mounted
into the test tree by `crates/checksums/src/rolling/tests/mod.rs:1-39`
and `crates/checksums/src/rolling/tests/checksum/mod.rs:1-5`. Each
SIMD variant has a dedicated test that calls the SIMD path's
`*_for_tests` helper and the scalar oracle
`accumulate_chunk_scalar_for_tests` over a fixed sweep of sizes and seed
states:

| SIMD variant | Test | Oracle | Probes |
|--------------|------|--------|--------|
| SSE2 | `sse2_accumulate_matches_scalar_reference` at `simd.rs:1-37` | `accumulate_chunk_scalar_for_tests` at `mod.rs:631-639` | 10 sizes (1..=4096, including 15/16/17 around the lane boundary) x 4 seed states |
| AVX2 | `avx2_accumulate_matches_scalar_reference` at `simd.rs:39-75` | `accumulate_chunk_scalar_for_tests` at `mod.rs:631-639` | 8 sizes (32..=4096, including 32/33/47/64/95 around the lane boundary) x 4 seed states |
| NEON | `neon_accumulate_matches_scalar_reference` at `simd.rs:77-109` | `accumulate_chunk_scalar_for_tests` at `mod.rs:631-639` | 10 sizes x 4 seed states |

The seed states include `(0, 0, 0)`, `(0x1234, 0x5678, 7)`,
`(0x0fff, 0x7fff, 1024)`, and the saturating-length edge
`(0xffff, 0xffff, usize::MAX - 32)` (see `simd.rs:12-17, 50-55, 84-89`).
The cross-architecture test helpers
(`accumulate_chunk_sse2_for_tests`, `accumulate_chunk_avx2_for_tests`,
`accumulate_chunk_neon_for_tests`) are defined at `x86.rs:321-338` and
`neon.rs:141-153`. Each is gated behind `#[cfg(test)]` so they cost
nothing in production builds.

The SSE2/AVX2 tests pre-check `is_x86_feature_detected!` (see
`simd.rs:4, 42`) and return early on hosts without the feature, so the
suite runs cleanly on machines that lack AVX2 (only the SSE2 test
runs) or both (the tests become no-ops). The NEON test is gated by
`#[cfg(target_arch = "aarch64")]` (`simd.rs:77`) and unconditionally
runs on aarch64 hosts because NEON is mandatory on the target.

### 2b. Property-based parity over the public API

File: `crates/checksums/src/rolling/tests/checksum/properties.rs`. Five
proptest cases exercise the `RollingChecksum` surface that delegates
into `accumulate_chunk_dispatch` (and therefore into every SIMD path):

- `rolling_update_matches_single_pass` at `properties.rs:8-23`: every
  chunked `update()` sequence must match a single-shot `update()` of
  the concatenated input. Inputs come from `chunked_sequences()` at
  `crates/checksums/src/rolling/tests/mod.rs:25-27` (1..=8 chunks of
  0..=64 bytes each).
- `rolling_matches_reference_for_random_windows` at
  `properties.rs:25-51`: cross-validates `update()` against itself for
  arbitrary slice prefixes and then rolls the window forward one byte
  at a time, requiring each rolled state to match a fresh `update()` of
  the same window. Inputs from `random_data_and_window()` at
  `tests/mod.rs:17-23`.
- `vectored_update_matches_chunked_input` at `properties.rs:53-68`:
  ensures `update_vectored` matches a sequence of `update()` calls,
  which exercises the scratch-buffer flush path at
  `mod.rs:613-629` that re-enters the SIMD dispatcher.
- `roll_many_matches_single_rolls_for_random_sequences` at
  `properties.rs:70-92` and the deterministic 4096-step companion at
  `properties.rs:108-151`: confirm the `roll_many` weighted-delta path
  matches per-byte `roll()`. Although `roll`/`roll_many` are scalar in
  the current implementation, they run after states that the SIMD
  `update()` produced, so any SIMD drift would surface as a roll
  mismatch on the next probe.
- `from_digest_round_trips` at `properties.rs:94-105`: digest
  serialization round-trip.

### 2c. Runtime self-test (release-time parity)

`crates/checksums/src/simd_self_test.rs` exposes
`run_simd_self_test()` (declared at `simd_self_test.rs:141-151`,
re-exported at `crates/checksums/src/lib.rs:441-445`) which is callable
from release diagnostics and CLI smoke runs. The rolling-checksum
helper `check_rolling` at `simd_self_test.rs:201-227` validates the
SIMD-engaging `RollingChecksum::update` against a fully independent
oracle built from a per-byte `update_byte` loop (see
`simd_self_test.rs:25-27` and `mod.rs:191-198`). `update_byte` never
enters the SIMD accumulator, so the two paths share no code.

Input sweep at `simd_self_test.rs:51-53` covers 19 sizes from 0 to
9217 bytes, including all SIMD lane boundaries and an explicitly
non-aligned 9 KiB probe. Three pattern shapes per size
(`simd_self_test.rs:159-185`) - all-zero, ascending modulo, and an LCG
byte stream - catch the sign-extension, lane-ordering, and
computation-drift failure modes typical of SIMD regressions.

### 2d. Composite parity through the dispatcher

`crates/checksums/src/comprehensive_tests.rs:760-877` exercises the
`RollingChecksum` public API end-to-end (per-byte vs slice, vectored
mixed sizes, large `roll_many` batches). The vectored path at
`comprehensive_tests.rs:834-877` flows through
`accumulate_chunk_dispatch` and therefore engages whichever SIMD
variant the host supports.

### 2e. Other parity-test sites

`crates/checksums/src/simd_parity_tests.rs` covers MD5/MD4/XXH3 batch
hashers only (see the module enumeration at `simd_parity_tests.rs:9,
408, 785, 1035, 1140` - all strong-checksum dispatchers). It does not
duplicate the rolling-checksum parity and was not intended to: the
rolling parity lives next to the rolling implementation in
`rolling/tests/checksum/simd.rs`, and the rolling self-test lives in
the cross-algorithm `simd_self_test.rs` module that
`simd_parity_tests.rs` is the test-only twin of.

| Parity surface | Rolling? | MD4? | MD5? | XXH3? |
|----------------|----------|------|------|-------|
| `rolling/tests/checksum/simd.rs` | yes (SSE2, AVX2, NEON) | no | no | no |
| `rolling/tests/checksum/properties.rs` | yes (dispatcher) | no | no | no |
| `simd_self_test.rs` | yes (dispatcher vs `update_byte` oracle) | yes | yes | no |
| `simd_parity_tests.rs` | no | yes | yes | yes |

Every SIMD rolling-hash variant is paired with at least one scalar
oracle; the AVX2 oracle is `accumulate_chunk_scalar_raw` directly, the
SSE2 and NEON oracles are the same scalar function, and the
`simd_self_test.rs` rolling probe uses the entirely independent
`update_byte` oracle.

## 3. Zsync-adjacent rolling-hash usage and coverage

The zsync-inspired matching optimizations live in
`crates/matching/`. The matcher invokes the rolling hash in exactly
three places, all in `crates/matching/src/generator.rs`:

| Site | Call | Path engaged |
|------|------|--------------|
| `crates/matching/src/generator.rs:227` | `rolling.update_byte(byte)` (initial window fill) | per-byte scalar (no SIMD) |
| `crates/matching/src/generator.rs:213` | `rolling.roll(outgoing_byte, byte)` (sliding window) | scalar O(1) roll |
| `crates/matching/src/generator.rs:370` and `:372` | `rolling.update(s1)` / `rolling.update(s2)` (bulk recompute after a Copy match) | `accumulate_chunk_dispatch` -> SIMD where available |

The bulk-recompute site at `generator.rs:370-373` is the only matcher
caller that engages the SIMD AVX2/SSE2/NEON paths. The upstream
reference (`match.c:303-308`) is cited in the comment at
`generator.rs:347-348`, and the call is reached every time
`hash_search` finds a match and refills the window. Coverage:

- `crates/matching/tests/integration_tests.rs:661-680` runs
  `DeltaGenerator::new().with_buffer_len(...)` across four buffer
  sizes (64, 1024, 32768, default) over a shared input, all of which
  cycle through the bulk-recompute site whenever a block matches.
- `crates/matching/tests/block_matching_accuracy.rs` (line counts at
  `rolling = RollingChecksum::new()` sites `:290, :308, :356, :372,
  :395`) drives sliding-window probes against the matching index using
  the same `RollingChecksum::update` API, so any AVX2/SSE2/NEON drift
  would surface as a missed or spurious match.
- `crates/matching/tests/sparse_match_fixture.rs` (#2080) and
  `crates/matching/tests/shifted_insertion_fixture.rs` (#2079) drive
  `DeltaGenerator::generate` against adversarial sparse-match and
  shifted-insertion targets. Both call into the SIMD-accelerated bulk
  recompute path whenever a Copy token is emitted.
- `crates/matching/src/index/sparse_match_tests.rs:205-220` and
  `:240-254` use `RollingChecksum::update` followed by a sliding
  `roll` loop to verify the bithash prefilter's rejection rate; the
  initial `update()` call goes through the SIMD dispatcher.

The matcher therefore has both unit-level (sparse_match_tests,
block_matching_accuracy) and integration-level (sparse_match_fixture,
shifted_insertion_fixture, integration_tests) coverage of the path
where it actually invokes the SIMD rolling hash. Any SIMD regression
would either be caught at the dispatcher boundary (parity tests in
`rolling/tests/checksum/simd.rs`) or surface as a block-match miss in
the zsync adversarial fixtures.

## 4. Recent zsync-related changes and their parity exposure

The recent zsync-track merges (master, since #3643) and whether they
touched the rolling implementation or its SIMD paths:

| PR | Subject | Touches `crates/checksums/src/rolling/`? | Touches `crates/checksums/src/simd_*`? | Parity tests would run? |
|----|---------|------------------------------------------|----------------------------------------|--------------------------|
| `3d0391d80` (#3737) | bithash prefilter to MatchIndex | no | no | n/a (matching-internal change) |
| `6122b5070` (#3751) | seq-match extend-run | no | no | n/a |
| `aa7eb8a45` (#3748) | matched-block pruning bitmap | no | no | n/a |
| `8e750a737` (#3657) | sparse-match adversarial fixture | no | no | n/a (test-only) |
| `cc82f7734` (#3656) | shifted-insertion fixture | no | no | n/a (test-only) |
| `6e47121fc` (#4164) | adversarial test pair (#2079/#2080) | no | no | n/a (test-only) |
| `db081d9b7` (#4177) | benches for prefilter/seq-match/prune (#2063/#2067/#2071) | no | no | n/a (bench-only) |
| `9eac1db99` (#4188) | compact-keys cache behavior bench (#2073) | no | no | n/a (bench-only) |
| `b292e0881` (#4192) | packed-key feasibility audit (#2072) | no | no | n/a (docs-only) |
| `9b7f94f7d` (#4169) | zsync cleanup audit (#2083/#2084/#2085) | no | no | n/a (docs-only) |
| `05bd88b89` (#4171) | interop verification audit (#2074) | no | no | n/a (docs-only) |
| `04e018196` (#2072) | packed-key feasibility audit (release branch peer) | no | no | n/a (docs-only) |

`git log --oneline --all -- crates/checksums/src/rolling/` returns no
zsync-tagged commit. The most recent rolling-checksum-touching commits
are `bb110d34e` (#3743, `--simd` CLI override) and `723a0a0c1` (#3560,
upstream `schar` sign-extension fix). Both predate the most recent
zsync wave on this branch but both shipped with the parity tests in
`rolling/tests/checksum/simd.rs` exercised by CI's
`cargo nextest run -p checksums --all-features`.

Tag-table search confirms no zsync work has reached the strong-hash
parity surface either:
`git log --oneline --all -- crates/checksums/src/simd_parity_tests.rs`
returns the same set of upstream-fix and refactor commits with no
zsync tag in any subject.

## 5. Verdict

**PASS - existing parity coverage is complete and unaffected by the
zsync-inspired matching work.**

Justification:

1. Every architecture variant that
   `accumulate_chunk_dispatch`
   (`crates/checksums/src/rolling/checksum/mod.rs:519-530`) can select
   has a dedicated SIMD-vs-scalar parity test in
   `crates/checksums/src/rolling/tests/checksum/simd.rs` (SSE2, AVX2,
   NEON), each driven against the same `accumulate_chunk_scalar_raw`
   oracle.
2. The dispatcher itself is exercised end-to-end by the proptest cases
   in `crates/checksums/src/rolling/tests/checksum/properties.rs` and
   the per-byte oracle in
   `crates/checksums/src/simd_self_test.rs:201-227`, the latter
   independent of `accumulate_chunk_scalar_raw`.
3. The zsync-adjacent matcher call site at
   `crates/matching/src/generator.rs:370-373` (the bulk recompute that
   engages SIMD) is exercised by the integration suites in
   `crates/matching/tests/integration_tests.rs`,
   `crates/matching/tests/block_matching_accuracy.rs`,
   `crates/matching/tests/sparse_match_fixture.rs`, and
   `crates/matching/tests/shifted_insertion_fixture.rs`.
4. None of the recent zsync merges (#3737, #3748, #3751, #3656, #3657,
   #4164, #4177, #4188, #4169, #4171, #4192, plus #2072 on the
   release branch peer) touched `crates/checksums/src/rolling/` or
   `crates/checksums/src/simd_*`. Each merge is confined to
   `crates/matching/` plus, for #3751, a single plumbing call in
   `crates/transfer/src/generator/delta.rs` (documented in
   [`zsync-cleanup-audit.md`](zsync-cleanup-audit.md) #2085). The
   rolling SIMD dispatch is therefore unchanged, and the existing
   parity tests would have caught any regression introduced through it.

## 6. Recommended follow-ups

None. The parity surface for the rolling hash is complete:

| Coverage dimension | Status |
|--------------------|--------|
| SSE2 vs scalar | covered (`rolling/tests/checksum/simd.rs:1-37`) |
| AVX2 vs scalar | covered (`rolling/tests/checksum/simd.rs:39-75`) |
| NEON vs scalar | covered (`rolling/tests/checksum/simd.rs:77-109`) |
| Dispatcher round-trip (incremental vs single-pass) | covered (`rolling/tests/checksum/properties.rs:8-23`) |
| Roll-window forward consistency | covered (`rolling/tests/checksum/properties.rs:25-51`) |
| Vectored input | covered (`rolling/tests/checksum/properties.rs:53-68`) |
| Independent oracle (release self-test) | covered (`simd_self_test.rs:201-227`) |
| Matcher-side SIMD invocation | covered (`crates/matching/tests/{integration,block_matching_accuracy,sparse_match_fixture,shifted_insertion_fixture}.rs`) |

No additional tests are required. Should a future zsync optimization
introduce a new rolling-hash invocation pattern (for example, a
sub-block recompute that bypasses `accumulate_chunk_dispatch`), this
audit should be re-run against the new code path.
