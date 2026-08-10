# zsync adversarial shifted-insertion test fixture

Adversarial regression fixture for the rolling-checksum match path.
Pins behaviour against the canonical zsync-style insertion adversary:
insert `N` bytes at offset `M` in a basis, then confirm the rolling
window re-syncs at every block boundary past the insertion. Acts as
the public-API guard for the four pipeline optimizations enumerated
in `docs/design/zsync-inspired-matching.md` (#2053, completed):
bithash (#2059-#2063), seq-match (#2064-#2067), prune (#2068-#2071),
and compact-keys (#2072-#2073).

This is a doc-only note. The on-disk fixture already lives at
`crates/match/tests/shifted_insertion_fixture.rs` and is referenced
throughout for line-level binding. Sibling adversarial fixtures:
the rsum-collision fixture from #2078 at
`crates/match/tests/rsum_collision_fixture.rs`, and the upcoming
sparse-match fixture in #2080.

## 1. The zsync-style insertion adversary

The classical rsync adversary against the rolling-checksum match path
is a single contiguous insertion: take a basis `B` of length
`basis_len`, choose an offset `M in [0, basis_len]` and a count
`N >= 1`, and construct a source `S` with

    S[..M]         == B[..M]
    S[M..M+N]      == filler bytes that never collide with any block in B
    S[M+N..M+N+R]  == B[M..M+R]   where R = basis_len - M

`S.len() == basis_len + N`. Every byte after offset `M+N` in `S` is
shifted by exactly `+N` bytes relative to `B`. The matcher must:

- Match every basis block in `B[..M]` at identical offsets.
- Re-synchronize the rolling window past `[M, M+N)` so it can match
  every basis block in `B[M..]`. The window must slide one byte at a
  time across the inserted region until its content aligns again.
- Spill the inserted `N` bytes as `Literal` tokens; they are not
  reachable from any basis block by construction.

If the matcher fails to re-sync, every shifted basis block becomes a
literal, blowing the wire payload up by `~basis_len - M`. This is
the class of bug the bithash, seq-match, prune, and compact-keys PRs
risk introducing: each gates the lookup on a weak-checksum-derived
prefilter that, if buggy, can deny a real match.

## 2. Why this matters

Upstream rsync's key strength is precisely this: shifted insertions
never devolve into a wholesale retransmit. The point of the rolling
Adler-32 in `match.c:hash_search()` is to slide one byte at a time
and re-acquire the block alignment. Any optimization that touches the
gate between the rolling digest and the strong-checksum verify -
which all four zsync-inspired optimizations do - must demonstrate
that the re-sync property still holds.

PR #3560 (commit `723a0a0c1`) is an instance. Before #3560, every
byte `>= 0x80` made the rolling digest diverge from upstream by a
known delta; the strong-checksum gate rejected every window on a
high-byte basis and supposed matches fell through to literals. The
visible failure was "matched-bytes counter always zero on binary
basis files", a silent regression caught only via `--stats`
divergence. PR #3560 is pinned by
`crates/checksums/tests/rolling_signed_byte_regression.rs` (#2076,
completed via PR #3636).

The shifted-insertion fixture is the next layer up: a regression in
the delta-search pipeline (not just the rolling-hash math) that
silently drops a match. It catches sign-extension regressions on
high-byte basis files (#2076 class), bithash false negatives
(#2061-#2063 class), `want_i` hint off-by-one errors, rolling-window
reset bugs in `crates/match/src/generator.rs:194-280`, and
strong-checksum truncation bugs at
`crates/match/src/index/mod.rs:151-174`.

## 3. Fixture parameters

The fixture sweeps a four-dimensional matrix `(block_size, M, N,
algorithm)`. Concrete values pinned in
`crates/match/tests/shifted_insertion_fixture.rs:62-67`:

    BASIS_BLOCKS  = 8
    BLOCK_SIZES   = [700, 1024, 4096]
    INSERT_LENS   = [1, 7, 31, 1024]

Basis size `basis_len = BASIS_BLOCKS * block_size`. Eight blocks
exercises both basis ends with enough mid-stride blocks for the
generator's `want_i` hint to fire repeatedly.

### Block sizes

- `700`: upstream's default for small files at protocol 32.
- `1024`: power-of-two boundary that SIMD fast paths special-case.
- `4096`: stresses the bulk-refill loop in
  `crates/match/src/generator.rs:233-246`.

### Insertion offset M

- `M = 0`: prepend. Pinned by
  `prepend_aligned_insertion_preserves_full_basis_match` at
  `crates/match/tests/shifted_insertion_fixture.rs:296-329`.
- `M = block_size / 2`: mid-block straddle.
- `M = block_size`: clean block boundary; aligned `M` and aligned
  `N` must produce `Copy(prefix) + Literal(N) + Copy(suffix)`.
- `M = 2 * block_size + 1`: near-end, deliberately mid-block.
- `M = basis_len - 1`: tail-edge. Pinned by
  `tail_edge_insertion_round_trips` at lines 336-354.

### Insertion length N

- `N = 1`: per-byte rolling re-sync; most likely to expose `want_i`
  off-by-one.
- `N = 7`: sub-word; alignment-independent re-sync.
- `N = 31`: sub-block but past most SIMD lane widths (one byte short
  of AVX2's 32-byte lane).
- `N = 1024`: one full block run; aligned at `block_size = 1024`.

The aligned matrix runs at lines 250-268; the unaligned matrix at
lines 275-291.

### Strong-checksum coverage

MD5 (`Md5Seed::none()`) and XXH3-64 at lines 143-150. MD4 is covered
at lines 373-384 because the strategy selector at
`crates/checksums/src/strong/strategy/selector.rs` only emits MD4
for protocol < 30. SHA-1 / SHA-256 / XXH3-128 / XXH64 share the same
`find_match_slices` gate so MD5 + XXH3-64 is sufficient.

## 4. Expected match output

### Aligned regime: `M % block_size == 0` AND `N % block_size == 0`

Token sequence MUST be exactly:

    Copy(0) Copy(1) ... Copy(M_blocks-1)
    Literal(N_bytes)
    Copy(M_blocks) ... Copy(BASIS_BLOCKS-1)

with `script.copy_bytes() == basis_len` exactly and
`script.literal_bytes() == N` exactly. No block is lost. Pinned by
`assert_aligned_insert_shape` at
`crates/match/tests/shifted_insertion_fixture.rs:156-202`. COPY
indices form a contiguous prefix `0..M_blocks` then a contiguous
suffix `M_blocks..BASIS_BLOCKS`.

### Unaligned regime

Boundary drift can lose at most ONE block; whatever basis block
straddles `[M, M+N+block_size)` cannot match in full:

    matched_bytes >= basis_len - block_size
    matched_bytes <= basis_len
    literal_bytes >= N

Pinned by `assert_unaligned_insert_bounds` at lines 207-244. The
lower bound `basis_len - block_size` is the canonical upstream
guarantee: exactly one mid-stride block falls out as literal in the
worst case.

The deeper invariant is that `apply_delta(basis, script)` reproduces
`source` byte-for-byte. Asserted for every matrix cell via
`assert_round_trip` at lines 124-137. Round-trip failure is the
literal definition of a wrong delta.

## 5. Comparison oracle

### Layer 1: round-trip reconstruction

`apply_delta(basis_cursor, output, &index, &script)` MUST produce
`output == source`. This is the wire-compat correctness gate;
checked unconditionally for every matrix cell at lines 130-136.

### Layer 2: upstream rsync `--stats` cross-check

For aligned matrix cells, upstream rsync running the same basis and
source MUST report the same matched-bytes count via `--stats`. The
harness lives in `tools/ci/run_interop.sh` under the `delta-stats`
workload class:

    rsync --stats -avr --no-whole-file basis_dir/ source_dir/

The "matched data" counter in the `--stats` block is upstream's
version of `script.copy_bytes()`. Aligned cells must match exactly
`basis_len`; unaligned cells must report `>= basis_len - block_size`.

The `delta-stats` workload was previously in
`tools/ci/known_failures.conf` for the high-byte basis case; PR
#3560 fixed the rolling-hash sign-extension and the workload was
removed (commit `60e83fd96`, `chore(ci): remove
standalone:delta-stats from KNOWN_FAILURES`). Re-introducing a
sign-extension bug would manifest as a `delta-stats` divergence on
this fixture's matrix cells.

The fixture does not invoke upstream rsync inline; that is the job
of `tools/ci/run_interop.sh`. The fixture's assertions and the
interop harness's `delta-stats` comparison are duals.

## 6. Test harness location

The fixture lives at
`crates/match/tests/shifted_insertion_fixture.rs` (385 lines, on
master). Siblings in `crates/match/tests/`:

- `rsum_collision_fixture.rs` - rsum-collision fixture from #2078,
  exercising the strong-checksum gate against constructed digest
  collisions. Dual to this fixture, which exercises the rolling
  digest's re-sync property.
- `block_matching_accuracy.rs`, `integration_tests.rs` - point
  correctness tests for the existing match path.
- `fuzzy_level_tests.rs` - fuzzy-match level validation, orthogonal
  to the rolling-hash gate.

The shifted-insertion fixture is the only one with a parametric
matrix; the others are point fixtures. New adversarial cases land
in this file until the matrix grows past the maintainability budget.
The MD4 single-shot at lines 373-384 is the template for single-shot
pins without reworking the matrix.

## 7. CI integration

The fixture runs under `cargo nextest run -p matching --all-features`
in the standard nextest matrix on every PR. No new workflow.

The SIMD-parity proptest at
`crates/checksums/tests/rolling_simd_parity.rs` (#2077) is a
prerequisite gate: any rolling-hash divergence between scalar, AVX2,
SSE2, and NEON would silently corrupt the rolling digest fed into
this fixture. The chain is

    rolling_simd_parity.rs (#2077)
       |
       v
    rolling_signed_byte_regression.rs (#2076)
       |
       v
    rsum_collision_fixture.rs (#2078)
       |
       v
    shifted_insertion_fixture.rs (#2079, this fixture)
       |
       v
    sparse_match_fixture (#2080)

A failure at any earlier step poisons the fixtures downstream. The
chain is the end-to-end seal: rolling digest correct on every
dispatch path, high-byte semantics match upstream, strong-checksum
gate rejects weak collisions, rolling window re-syncs across
insertions.

CI ordering inside `tools/ci/run_interop.sh` does not need to change:
the unit-level fixture runs in nextest (early), and the
`delta-stats` interop workload runs later in the same job.

## 8. Failure modes the test must catch

### 8.1 Signed-byte regression (#2076 class)

Reintroduction of the unsigned-byte interpretation in
`crates/checksums/src/rolling/checksum/mod.rs:332-350` or in the SIMD
dispatch paths at `crates/checksums/src/rolling/checksum/x86.rs` and
`crates/checksums/src/rolling/checksum/neon.rs`. Symptom: digests
diverge from upstream on any basis byte `>= 0x80`. Caught because
`make_basis` at lines 71-75 produces ~50% bytes `>= 0x80`; the
strong-checksum gate rejects every supposed match and
`script.copy_bytes()` collapses to `0`.

### 8.2 Off-by-one in seek

A regression in `apply_delta` at `crates/match/src/script.rs:105`
that seeks to `block_index * block_length +/- 1`. Symptom: round-trip
reconstruction fails at every aligned-insertion cell. Caught via the
unconditional `assert_round_trip`.

### 8.3 Bithash false negative (#2061-#2063 class)

A future bithash at `crates/match/src/index/mod.rs:165` (the cited
insertion point in `docs/design/zsync-inspired-matching.md`)
returning `probably_present(rsum) == false` for a digest actually
inserted at build time. Symptom: a basis block whose digest collides
into a not-set bithash bit gets dropped, manifesting as a missing
COPY token in the aligned cells. The aligned-shape assertion at
lines 186-201 enforces the exact COPY-index sequence; a missing
block fails the check immediately, pinpointing
`(block_size, M, N, algorithm)`.

### 8.4 want_i hint off-by-one

A regression in `want_i` at
`crates/match/src/generator.rs:99-103, 221-225, 259-273` setting
`want_i = Some(match_idx)` (no +1) or `Some(match_idx + 2)`.
Symptom: aligned inserts lose every block past the insertion; the
unaligned regime still passes (hash-table fallback) but the aligned
regime fails the COPY-index sequence check at lines 186-201.

### 8.5 Rolling-window reset after match

A regression in the bulk-refill loop at
`crates/match/src/generator.rs:227-258` that fails to clear rolling
state, fails to refill the window, or computes the post-match digest
from a partially-filled window. Symptom: the next block boundary's
digest is wrong; the round-trip fails. The sweep across `block_size
in [700, 1024, 4096]` hits the refill loop at multiple alignments.

### 8.6 Strong-checksum truncation drift

A regression in `find_match_slices` at
`crates/match/src/index/mod.rs:151-174` that compares full-length
strong checksums against truncated stored ones (or vice versa).
Symptom: every supposed match fails the verify; the fixture
collapses to all-literal and the aligned COPY-byte assertion fails.

### 8.7 Two-slice ring-buffer wrap bug

A regression in `RingBuffer::as_slices` at
`crates/match/src/ring_buffer.rs` or its consumer at
`crates/match/src/generator.rs:176, 253-257` returning slices in
reverse order, double-counting the wrap, or skipping a byte at the
wrap. Symptom: rolling digests are correct but the strong checksum
sees a permuted window; the gate rejects every match. Same
observable symptom as 8.6, different root cause; both caught.

## 9. Open questions

### 9.1 Pathological N

`N` exceeding the basis size would stress the `pending_literals`
flush path at `crates/match/src/generator.rs:148-154` (the
`block_len + CHUNK_SIZE` threshold). Matrix caps at `N = 1024`;
expanding multiplies matrix size. Tracked for #2079 follow-up.

### 9.2 Symmetric deletion adversary

Deletion: remove `N` bytes at offset `M`, producing `len(S) ==
basis_len - N`. Re-sync is the same up to a sign change. Absent;
candidate for #2080 or a follow-up.

### 9.3 Lift aligned cells into the interop harness

Whether the matrix cells should be lifted into
`tools/ci/run_interop.sh` as named `delta-stats` workloads is open.
For: every aligned cell is a hard byte-count equality with upstream.
Against: 96 cells * 3 upstream versions = 288 interop runs, which
extends matrix test runtime materially.

### 9.4 SIMD lane-boundary corner cases

No explicit cell at `N = 32` (AVX2 lane) or `N = 16` (SSE2/NEON
lane). The parity proptest at
`crates/checksums/tests/rolling_simd_parity.rs` already exercises
SIMD lane boundaries; duplicating may be redundant.

### 9.5 INC_RECURSE per-segment rebuilds

Each INC_RECURSE segment rebuilds `DeltaSignatureIndex` from scratch
(per `crates/match/src/index/builder.rs:71-98`). The fixture builds
a single index per cell, so the per-segment rebuild is not
exercised. Folding into this fixture vs splitting into a separate
fixture is open.

## References

### oc-rsync source

- `crates/match/tests/shifted_insertion_fixture.rs` - the fixture,
  385 lines, on master.
- `crates/match/tests/rsum_collision_fixture.rs` - sibling fixture
  from #2078.
- `crates/match/src/generator.rs:81-322` - matching pipeline driver;
  `want_i` hint at 99-103, 221-225, 259-273; bulk-refill at 227-258;
  literal flush at 148-154.
- `crates/match/src/index/mod.rs:151-174` - `find_match_slices`,
  strong-checksum gate.
- `crates/match/src/index/mod.rs:38-48` - `DeltaSignatureIndex`
  layout; future bithash field at line 44 per parent doc.
- `crates/match/src/script.rs:14-98` - `DeltaToken`, `DeltaScript`,
  `copy_bytes`, `literal_bytes`.
- `crates/match/src/ring_buffer.rs` - two-slice window targeted by
  failure mode 8.7.
- `crates/checksums/src/rolling/checksum/mod.rs:303-350` - `roll`;
  signed-byte cast at 336-338 fixed by PR #3560.
- `crates/checksums/tests/rolling_signed_byte_regression.rs` -
  #2076, prerequisite high-byte fixture.
- `crates/checksums/tests/rolling_simd_parity.rs` - #2077,
  prerequisite SIMD parity proptest.

### Upstream rsync 3.4.1

- `target/interop/upstream-src/rsync-3.4.1/match.c:140-345` -
  `hash_search`, the upstream rolling-hash matcher.
- `target/interop/upstream-src/rsync-3.4.1/checksum.c:285` -
  signed-byte (`schar *buf`) cast mirrored by PR #3560.

### Parent design and tracking

- `docs/design/zsync-inspired-matching.md` - parent design (#2053).
- `docs/design/zsync-bithash.md` - bithash detail design (#2059).
- #2076 - signed-byte regression fixture (PR #3636).
- #2077 - SIMD parity proptest (prerequisite gate).
- #2078 - rsum-collision fixture (sibling).
- #2079 - this design note (this PR); #2080 - sparse-match (sibling).
- #3560 - rolling-checksum sign-extension fix (`723a0a0c1`).
