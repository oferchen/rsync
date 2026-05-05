# zsync adversarial sparse-match test fixture

Fixture design note for issue #2080. Defines the **extreme** sparse-match
fixture: a basis where only one block in every 25600 blocks matches
against the target stream (one match per 100 MB at a 4 KiB block size).
The fixture is the worst-case probe-density adversary against the
matching pipeline in `crates/match/`, and is the principal evidence
gate for the bithash prefilter (#2061) before it lands in release
configuration.

Docs-only PR. The fixture itself lands as
`crates/match/tests/sparse_match_extreme_fixture.rs` under a follow-up
PR; this note pins the parameters, invariants, and pass/fail thresholds.

Sibling: #2079 lands a smaller-scale sparse-match fixture
(`crates/match/tests/sparse_match_fixture.rs`, in flight) with `K` in
`{0, 1, 2}` over basis lengths up to 16 MB. This fixture is a separate,
larger-scale adversary tuned for the bithash false-positive rate
threshold, not general matching accuracy. #2071 (duplicate-block bench)
is unrelated and out of scope.

## 1. The sparse-match adversary

### Shape

A pair `(basis, target)` where:

- `basis` is exactly 100 MiB of deterministic pseudo-random bytes,
  signature-indexed at block size `B` in `{700, 4096, 65536}`. At
  `B = 4096` the basis holds exactly `25600` blocks.
- `target` is the same length as `basis` (`100 MiB`).
- Exactly **one** basis block recurs verbatim in `target` at the
  block-aligned offset `0`. Every other source byte is drawn from a
  byte-disjoint range that cannot match any basis block at any sliding
  offset, regardless of alignment.

The match density is therefore `1 / 25600` at `B = 4096`, i.e. one
match per 25600 indexed blocks, or one match per 100 MiB of source.

### Why one match, not zero

A zero-match fixture (`K = 0` in #2079) is already covered for the
match-accuracy contract. The extreme fixture's load-bearing property is
that **the bithash MUST keep the one real match findable** while
rejecting everything else. A zero-match fixture cannot distinguish a
correct bithash from one that returns `false` unconditionally.

### Adversary against bithash

The bithash prefilter (#2059, design `docs/design/zsync-bithash.md`)
inserts one bit per indexed block into a bit array sized at `~8N` bits.
On a 100 MiB basis with `B = 4096`, `N = 25600` and the bithash holds
approximately `262144` bits (`32 KiB`).

The hot loop in `crates/match/src/generator.rs:177-187` probes the
index at every byte offset where the rolling window is full. Over a
100 MiB target the loop runs roughly `1.04 * 10^8` probes. Almost all
must be rejected; only one position should reach the strong-checksum
verify (the planted block at offset 0). The existing `tag_table`
saturates near 100% set on this scale (`docs/design/zsync-bithash.md`
lines 19-32), so without bithash every probe pays the `FxHashMap::get`
cost. With bithash, a correctly sized prefilter rejects ~7/8 in O(1).

## 2. Why it matters

Three pipeline weaknesses surface only at this scale.

### 2.1 Bithash false-rejects at scale

The bithash invariant from `docs/design/zsync-bithash.md` section 5 is
one-sided: every inserted block MUST satisfy `probably_present` at probe
time. A single false-reject is a missed match, which silently degrades
to literal-byte transfer with no observable failure on typical fixtures
(everything still works, just slower). The signed-byte regression PR
#3636 pinned one corner case (`(byte as i8) as i32` in the rolling
accumulator). Other corner cases - SIMD vs scalar digest divergence at
particular byte distributions, rotate/shift sign bugs in the bithash
mixing function - would manifest only when the fixture has very few
real matches against very many probes.

The extreme fixture exposes the failure mode by construction: if the
bithash false-rejects the one real match, `matched_bytes == 0`, which
violates the invariant from section 4 below. With one real match per
`10^8` probes, even a tiny per-probe false-reject probability (one in a
million) would not be detected - the fixture deliberately drives the
ratio the other way.

### 2.2 Hash table degenerate buckets

The lookup map `FxHashMap<(u16, u16), Vec<usize>>` at
`crates/match/src/index/mod.rs:44` is keyed on `(sum1, sum2)`. With
25600 blocks distributed uniformly across the 32-bit `(sum1, sum2)`
space, the expected per-bucket occupancy is well under 1. But adversarial
basis content - or a cryptographically weak deterministic generator -
could cluster blocks into a handful of buckets, degrading
`find_match_slices` (`crates/match/src/index/mod.rs:151-194`) to linear
scan. The fixture's basis bytes use splitmix64 (Stafford variant 13,
already used in `crates/match/tests/sparse_match_fixture.rs`); the
target bytes use the same generator with a XOR-perturbed seed and a
high-bit forcing mask. Both are uniform on their byte ranges and
collide-resistant in practice across the block sizes in scope.

### 2.3 Rolling-hash hot path under near-constant rejection

`crates/match/src/generator.rs:125-285` is the byte-by-byte rolling
loop. At `~10^8` iterations the per-iteration cost dominates the
delta-generation wall clock. Quadratic-shaped regressions are invisible
at the 64 KiB and 1 MiB scales of #2079 and become obvious here. The
fixture acts as a tripwire for those regressions independent of the
bithash work.

## 3. Fixture parameters

### Basis size

| Parameter         | Value                       | Rationale                                                                  |
|-------------------|-----------------------------|----------------------------------------------------------------------------|
| `basis_len`       | `100 * 1024 * 1024` bytes   | Issue #2080 specifies 100 MB. MiB chosen for clean block-count arithmetic. |
| `target_len`      | `basis_len`                 | Disjoint byte ranges; equal length keeps the rolling loop running to EOF.  |
| `planted_blocks`  | `1`                         | Exactly one block-aligned match at offset 0. Single-match invariant.       |
| `planted_offset`  | `0`                         | Block-aligned. The generator skips by `block_len` after match, so no       |
|                   |                             | sliding-window probe inside the planted region (`generator.rs:227-246`).   |
| `strong_length`   | `8` bytes                   | Matches `STRONG_LEN` in `crates/match/tests/sparse_match_fixture.rs:68`.   |
| `protocol`        | `ProtocolVersion::NEWEST`   | Same as the sibling fixture; maximum negotiated capabilities.              |
| `algorithm`       | MD5(no-seed) and XXH3-64    | Both verified in #2079; bithash is algorithm-independent.                  |

### Block size sweep

The fixture runs once per block size in `{700, 4096, 65536}`:

| `block_size` | `block_count` | Density (one match per N blocks) | Matches per 100 MiB |
|--------------|---------------|----------------------------------|---------------------|
| `700`        | `149797`      | `1 / 149797`                     | `1`                 |
| `4096`       | `25600`       | `1 / 25600`                      | `1`                 |
| `65536`      | `1600`        | `1 / 1600`                       | `1`                 |

Justification:

- **700**: pre-protocol-30 default block size for very small files; also
  the block size used in `crates/match/tests/rsum_collision_fixture.rs:46`,
  ensuring symmetric coverage.
- **4096**: the canonical issue-#2080 target; matches typical Linux
  page size and the average rsync block-size heuristic for 100 MiB
  files.
- **65536**: stresses the opposite extreme - few blocks, very large
  bithash buckets-per-block ratio, much smaller bithash array. Probes
  per match remain `~10^8 / 1600`, large enough to detect false-rejects.

The block-size sweep also pins the bithash sizing-formula sensitivity
(`docs/design/zsync-bithash.md` section 1, "Sizing formula derivation")
across two orders of magnitude in `N`.

## 4. Expected output

The matching invariant is exact equality:

> `matched_bytes == planted_blocks * block_size == 1 * block_size == block_size`

Concretely:

| `block_size` | `matched_bytes` | `literal_bytes`              | `copy_token_count` | `total_bytes`         |
|--------------|-----------------|------------------------------|--------------------|-----------------------|
| `700`        | `700`           | `100 * 1024 * 1024 - 700`    | `1`                | `100 * 1024 * 1024`   |
| `4096`       | `4096`          | `100 * 1024 * 1024 - 4096`   | `1`                | `100 * 1024 * 1024`   |
| `65536`      | `65536`         | `100 * 1024 * 1024 - 65536`  | `1`                | `100 * 1024 * 1024`   |

The sole `DeltaToken::Copy` token must reference `block.index() == 0`
with `block.len() == block_size`. The literal payload covers every
target byte outside the planted region.

These checks mirror `assert_sparse_match_invariants` at
`crates/match/tests/sparse_match_fixture.rs:207-234`.

## 5. Bithash interaction (#2061)

The bithash prefilter is the primary subject under test. The fixture
encodes two bithash-specific contracts.

### 5.1 No false-reject of the planted match

The planted block at target offset 0 sets exactly one bit in the
bithash at insertion time
(`crates/match/src/index/builder.rs:16-36`, future `bithash.insert(...)`
call). At probe time, when the rolling window first fills with the
planted bytes, the digest must address that same bit. Per the bithash
invariant in `docs/design/zsync-bithash.md` section 5:

> For every `(rsum, basis-byte-window)` pair actually inserted into the
> index at build time, `BitHash::probably_present(rsum)` MUST return
> `true` at probe time.

If the invariant breaks, `matched_bytes == 0` and the fixture fails the
invariant in section 4 above. The fixture is the largest-scale
empirical check on this property in the repository.

### 5.2 Prefilter rejection rate >= 99.99%

Approximately `10^8` probes pass the `tag_table` gate at
`crates/match/src/index/mod.rs:161` (the `tag_table` saturates near 100%
set on a 25600-block basis - one of the motivations for bithash in the
parent doc). Of those, exactly one should reach the strong-checksum
verify. Allowing a generous margin for `(sum1, sum2)` collisions with
the planted block:

- Expected reject rate at the bithash gate (insertion point #2061,
  `crates/match/src/index/mod.rs:165`): `>= 99.99%`.
- Equivalently, false-positive surrender to the lookup map: `<= 1` per
  `10^4` post-tag candidates, far below the `~12.5%` worst case of
  parent-doc table line "BITHASHBITS = 3".

The threshold is intentionally **stricter** than the parent doc's
`>= 0.85` reject-rate target (`docs/design/zsync-bithash.md` section 7).
The parent-doc target is for typical workloads where blocks recur at
realistic frequencies. This fixture is specifically the adversarial
case where the bithash should be near-perfect because the basis content
distribution is uniform on a byte-disjoint range, so almost every
candidate `(sum1, sum2)` is genuinely new and the bithash bit at that
address is unset.

If `reject_rate < 0.9999` on this fixture, either the bithash sizing
is too small (`docs/design/zsync-bithash.md` section 2 "Sizing formula
derivation"), the mixing function (`bithash_h`) is dropping entropy,
or the basis generator is producing accidental rsum clusters. All
three are bugs the fixture must flag.

### 5.3 What the fixture does NOT test

- **Hot-path CPU savings.** Belongs in #2063 bench scaffolding.
- **Compact-key compatibility (#2072).** Reshapes the lookup but not
  the bithash; covered by #2073.
- **Seq-match (#2064).** Only one match, so the `want_i` path fires at
  most once and is irrelevant to the rejection-rate metric.

## 6. Tooling: deterministic basis-vs-target generator

The fixture requires a generator that **guarantees exactly N matches at
known offsets**, byte-disjoint elsewhere. The sibling fixture's
splitmix64 generator at `crates/match/tests/sparse_match_fixture.rs:81-104`
satisfies the requirement:

- `basis_byte(offset)`: splitmix64 of `offset`, masked to `[0, 0x7f]`.
- `source_non_planted_byte(offset)`: splitmix64 of `offset XOR
  0xa3a3_a3a3_a3a3_a3a3`, OR'd with `0x80` (forces high bit).

Properties carried over: byte-range disjointness (basis in `[0, 0x7f]`,
source non-planted in `[0x80, 0xff]`); full `2^64` period; pure
deterministic function of offset, reproducible across architectures and
SIMD dispatches.

The single planted block is copied verbatim from `basis[0..block_size]`
into `target[0..block_size]`. The fixture extends `build_sparse_pair`
from `crates/match/tests/sparse_match_fixture.rs:111-131` to support
`basis_len = 100 * 1024 * 1024`. The existing implementation already
takes `basis_len` as a parameter, so the extension is parameter-only.

## 7. Test harness

### 7.1 Decision: extension OR criterion bench?

**Both, split by concern.** A new integration test
`crates/match/tests/sparse_match_extreme_fixture.rs`, gated with
`#[ignore]` (default off, opt-in via `cargo nextest run --run-ignored
only`), asserts the section 4 invariants. A cfg-gated bench harness
`crates/match/benches/sparse_match_extreme.rs` under #2063's
`bench-bithash` feature records `reject_rate` and wall time and
asserts the section 5.2 threshold.

Rationale: a 100 MiB fixture runs for seconds-to-minutes per
`(algorithm, block_size)` combination, so `#[ignore]` mirrors the
`sparse_match_16mb_block1024` precedent at
`crates/match/tests/sparse_match_fixture.rs:299-311`. Correctness lives
in `tests/`, reachable from default tooling. Bithash counters require
`bench-bithash` per `docs/design/zsync-bithash.md` section 7, so the
rate-threshold check stays under the feature gate to keep release
builds untouched.

### 7.2 Where the fixture lives

- `crates/match/tests/sparse_match_extreme_fixture.rs` - integration
  test, `#[ignore]` by default, asserts section 4 invariants. No
  bithash-specific assertion (those need the counter feature gate).
- `crates/match/benches/sparse_match_extreme.rs` - criterion bench
  under `#[cfg(feature = "bench-bithash")]`. Records `reject_rate`,
  `probes`, and `wall_time_ms` per `(algorithm, block_size)` pair.
  Asserts `reject_rate >= 0.9999` from section 5.2.

### 7.3 Wiring against existing helpers

The extreme fixture imports from the sibling fixture's helper module
once #2079 lands. To keep the helpers reachable, #2079 must expose
`build_sparse_pair`, `build_index`, `run_pipeline`,
`assert_sparse_match_invariants`, `md5_algo`, and `xxh3_algo` as
`pub(crate)` from a shared `tests/common/` module. If #2079 lands
without that refactor, this PR (the extreme fixture impl PR) does the
extraction first.

## 8. Pass/fail thresholds

The fixture passes iff **all** of the following hold for every
`(algorithm, block_size)` combination in scope:

| # | Threshold                                                 | Source                                                     |
|---|-----------------------------------------------------------|------------------------------------------------------------|
| 1 | `matched_bytes == block_size` (one block, exactly)        | section 4                                                  |
| 2 | `literal_bytes == 100 * 1024 * 1024 - block_size`         | section 4                                                  |
| 3 | `total_bytes == 100 * 1024 * 1024`                        | section 4                                                  |
| 4 | `copy_token_count == 1`                                   | section 4                                                  |
| 5 | The single copy token references `block.index() == 0`     | section 4                                                  |
| 6 | bithash `reject_rate >= 0.9999` (post-tag candidates)     | section 5.2; `bench-bithash` feature, parent section 7     |
| 7 | bithash `probes > 10^7` (sanity, ensures the loop ran)    | section 5.2 derived; same feature gate                     |
| 8 | wall time within `5x` of the 1 MiB sibling fixture        | scaling tripwire; not a hard CI gate                       |

Item 8 is reported, not gated, because runner variance exceeds the
threshold on shared CI; it lives in the bench output for trend
analysis.

Items 1-5 run under default `cargo nextest run --run-ignored only` and
are unconditional. Items 6-7 run only when the `bench-bithash` feature
is on. Item 8 is reporting only.

## 9. Open questions

1. **Should the basis size scale with `block_size`?** At `B = 65536`,
   `N = 1600`, and the bithash holds only `~16384` bits (`2 KiB`),
   a much smaller adversary than the `B = 4096` case. Resolution
   proposal: keep `basis_len` fixed at 100 MiB per #2080; revisit in
   #2084's decision record.

2. **Algorithm matrix.** MD5 and XXH3-64 cover the matrix the sibling
   fixture #2079 uses. The bithash mixes only the rolling digest, so
   strong-algorithm choice is orthogonal. Proposal: keep MD5+XXH3-64
   and document in the fixture rustdoc that strong-algorithm choice
   does not affect the bithash threshold.

3. **Bithash off-by-one in the planted region.** The planted block
   covers `[0, block_size)`. The generator skips forward by
   `block_size` after the match
   (`crates/match/src/generator.rs:227-246`). At offset `block_size`
   the source is non-planted, so byte-disjointness holds for every
   subsequent rolling window. Called out for reviewer attention.

4. **Sparse-match with `--inplace`.** The append/inplace path skips
   signature exchange entirely (parent doc rule 5). Out of scope.

5. **CI gating policy.** `#[ignore]` keeps the fixture off the default
   matrix. A nightly `--run-ignored only` job is bounded in cost and
   reasonable; resolution deferred to #2084.

## 10. References

### oc-rsync source

- `crates/match/src/index/mod.rs:38-48` - `DeltaSignatureIndex` struct;
  bithash field declaration (#2061).
- `crates/match/src/index/mod.rs:151-194` - `find_match_slices`; probe
  hot path the bithash gates.
- `crates/match/src/index/mod.rs:161` - `tag_table` check (saturates).
- `crates/match/src/index/mod.rs:165` - bithash probe insertion (#2061).
- `crates/match/src/index/mod.rs:228-251` - `check_block_match_slices`
  (intentionally NOT bithash-gated).
- `crates/match/src/index/builder.rs:16-36` - `populate_index`; bithash
  insertion call site (#2060).
- `crates/match/src/generator.rs:103` - `want_i` declaration.
- `crates/match/src/generator.rs:177-187` - byte-by-byte hint check;
  the `~10^8` probe loop.
- `crates/match/src/generator.rs:221-225` - hint advance after match.
- `crates/match/src/generator.rs:227-246` - bulk-refill window after
  match (skips planted region at scale).
- `crates/match/tests/sparse_match_fixture.rs:81-104` - splitmix64
  byte generator reused here.
- `crates/match/tests/sparse_match_fixture.rs:111-131` -
  `build_sparse_pair`; extended for 100 MiB.
- `crates/match/tests/sparse_match_fixture.rs:207-234` -
  `assert_sparse_match_invariants`; reused for section 4.
- `crates/match/tests/sparse_match_fixture.rs:299-311` - `#[ignore]`
  pattern this fixture mirrors.
- `crates/match/tests/rsum_collision_fixture.rs:46` - precedent for
  `block_size = 700`.
- `crates/match/benches/delta_matching_benchmark.rs` - criterion
  harness pattern reused for the bithash counter bench.

### Design context

- `docs/design/zsync-inspired-matching.md` - parent design (#2053).
- `docs/design/zsync-bithash.md` - bithash design (#2059); section 5
  pins the one-sided invariant; section 7 the bench scaffolding plan.
- `docs/design/zsync-seq-match.md` - seq-match (#2064); out of scope.
- `docs/design/zsync-prune.md` - prune (#2068); orthogonal.

### Tracking

- #2080 - this fixture design note.
- #2079 - sibling sparse-match fixture (in flight).
- #2071 - duplicate-block bench (different concern; pending).
- #2061 - bithash impl probe-site insertion.
- #2063 - bithash bench scaffolding; consumes this fixture's data.
- #2084 - keep-or-revert decision record.
