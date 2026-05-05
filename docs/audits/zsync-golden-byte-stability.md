# Golden-byte stability audit for zsync-inspired matching

Tracking: oc-rsync task #2086. Gating evidence for the four zsync-inspired
optimizations (#2059-#2073) staying purely in-memory and never perturbing
the bytes oc-rsync emits on the wire. Companion design notes:

- [`docs/design/zsync-inspired-matching.md`](../design/zsync-inspired-matching.md)
- [`docs/design/zsync-bithash.md`](../design/zsync-bithash.md)
- [`docs/design/zsync-seq-match.md`](../design/zsync-seq-match.md)
- [`docs/design/zsync-prune.md`](../design/zsync-prune.md)
- [`docs/design/zsync-hash-layout-decision.md`](../design/zsync-hash-layout-decision.md)

## Summary

The four planned matching optimizations (bithash prefilter, seq-match
run-extension, matched-block pruning, compact key layout) all attach to
private state inside `crates/match/` and never reach the wire boundary.
Verdict: **PROVEN STABLE for three of four techniques (bithash, seq-match,
compact-keys). ONE GAP for prune: today's golden corpus uses
unique-per-block basis content, so the duplicate-basis-block pruning
contract is exercised only by `crates/engine/tests/delta_reconstruction_tests.rs::duplicate_delta_block_references` (apply-side) and `crates/match/tests/block_matching_accuracy.rs::multiple_identical_blocks_all_matched` (reconstruction-only). Neither pins the COPY-token sequence as bytes. A new wire-byte regression covering duplicate-basis content is required before #2069 (prune impl) lands.** See "Stability test recommendation" below.

The wire boundary in question is the `DeltaOp` token stream serialized by
`crates/protocol/src/wire/delta/token.rs::write_token_stream`, plus the
sum_head and signature-block payloads from
`crates/protocol/src/wire/signature.rs`. None of the four optimizations
touches either layer. The optimizations sit upstream of token emission, in
the `DeltaSignatureIndex` lookup (`crates/match/src/index/mod.rs:38-48`)
and the `DeltaGenerator` driver loop (`crates/match/src/generator.rs:81-321`).

The receiver side is untouched: `apply_delta`
(`crates/match/src/script.rs:105-171`) only reads `DeltaToken::Copy{index, len}`
and `DeltaToken::Literal(bytes)`. Optimizations that change which basis-block
index is named in a COPY token still produce identical applied bytes when
duplicate basis blocks share content, but the wire-emitted index field can
differ. The prune optimization is the only one with that observable
property; the other three pin the same index the table-probe path would
return.

## Inventory of golden byte tests on the match/delta path

The repository does not maintain a `crates/protocol/tests/golden/` directory
of binary fixture files. Instead, all golden-byte assertions are inline
`assert_eq!` calls in Rust test files that build expected byte arrays from
hand-traced upstream wire encodings. The match/delta pipeline is exercised
by these:

### Wire encoding goldens (sum_head, delta tokens)

- `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs:148-341`
  pins the simple-token wire format used by all protocol versions:
  - `golden_v28_delta_token_literal_exact_bytes` (line 149) - 4-byte LE
    length followed by raw payload.
  - `golden_v28_delta_token_block_match_exact_bytes` (line 167) - block
    index 0 encodes to `[0xFF, 0xFF, 0xFF, 0xFF]` (write_int(-1)).
  - `golden_v28_delta_token_block_match_index_42` (line 177) - block 42
    encodes to write_int(-43).
  - `golden_v28_delta_token_end_marker_exact_bytes` (line 189) -
    write_int(0) ends the stream.
  - `golden_v28_delta_whole_file_exact_bytes` (line 241) - whole-file
    delta = literal-data + end marker.
  - `golden_v28_delta_stream_mixed_ops_exact_bytes` (line 260) - mixed
    `[Literal "AB", Copy{block_index: 0, length: 1024}, Literal "C"]`
    pinned byte-for-byte against expected `[0x02, 0, 0, 0, 'A', 'B',
    0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0, 0, 0, 'C', 0, 0, 0, 0]`.
  - `golden_v28_delta_stream_roundtrip` (line 289) - reads each token
    back and re-derives block_index via `-(token+1)`.
  - `golden_v28_delta_empty_file` (line 330) - empty whole-file delta is
    a bare end marker.

  This file is the strongest pin on the wire-emit path. Any reordering of
  the COPY-token stream produced by `write_token_stream`
  (`crates/protocol/src/wire/delta/token.rs:158-170`) would surface as a
  golden-byte mismatch in `golden_v28_delta_stream_mixed_ops_exact_bytes`.
  Protocol coverage: applies to all versions 28-32 (the simple-token
  format is unchanged across versions).

- `crates/protocol/tests/golden_protocol_v29_wire.rs:275-306`
  - `golden_v29_sum_head_md4` - pins the 16-byte SumHead wire layout
    (count, blength, s2length, remainder) used to advertise signature
    block parameters before the token stream begins. Wire boundary: the
    sum_head lives in front of the signature blocks; no zsync optimization
    here can change it because the optimizations all consume the
    signatures, never produce them.

- `crates/protocol/tests/zlib_golden_bytes.rs`,
  `crates/protocol/tests/zstd_golden_bytes.rs`,
  `crates/protocol/tests/lz4_golden_bytes.rs`,
  `crates/protocol/tests/iconv_golden_bytes.rs`,
  `crates/protocol/tests/zstd_daemon_recv_golden.rs`,
  `crates/protocol/tests/zstd_interop_golden_bytes.rs` - codec-specific
  wire goldens. The CPRES_ZLIB path
  (`crates/transfer/src/generator/delta.rs:378-434`) feeds matched basis
  bytes to the encoder dictionary after each COPY; if a zsync optimization
  emitted a different basis index for a duplicate-content block the
  dictionary feed would still see the same bytes (siblings are
  byte-identical), so the deflate stream stays synchronized. Protocol
  coverage: zlib is gated >= 29, zstd / lz4 >= 31.

- `crates/protocol/tests/golden_handshakes.rs`,
  `crates/protocol/tests/golden_protocol_v28_handshake.rs` - pin the
  `@RSYNCD:` greeting and protocol-version exchange. No interaction with
  matching.

- `crates/protocol/tests/golden_protocol_v28_flist.rs`,
  `crates/protocol/tests/golden_protocol_v29_flist.rs`,
  `crates/protocol/tests/golden_protocol_v28_wire.rs`,
  `crates/protocol/tests/golden_protocol_v29_wire.rs` - file-list framing,
  device/symlink/hardlink encoding, transfer stats. Pre-delta phase, no
  matching interaction.

### Roundtrip property tests on the wire encoder

These do not pin specific bytes but assert that the bytes encoded for a
given `DeltaOp` sequence decode back to the same sequence:

- `crates/protocol/tests/proptest_delta_roundtrip.rs` - covers both
  `write_delta` / `read_delta` (internal opcode format) and
  `write_token_stream` / `read_token` (upstream wire format) over
  arbitrary `DeltaOp` sequences.
- `crates/protocol/tests/proptest_delta_script_roundtrip.rs` - covers
  interleaved Literal+Copy token sequences, large literals (CHUNK_SIZE
  splitting), copy-boundary values near `i32::MAX`, byte-level content
  preservation.

### Generator-side end-to-end goldens

These run the generator over real basis/source data and compare reconstructed
output (apply-side) byte-for-byte:

- `crates/match/tests/integration_tests.rs:116-540` - 70+ correctness
  cases covering uniform data, repetitive patterns, sparse islands,
  insertions/deletions/reorderings, partial blocks, matching at exact
  block boundaries, etc. Each case calls `verify_round_trip(basis, input)`
  which asserts `apply_delta(basis, generate_delta(input)) == input`.

- `crates/match/tests/block_matching_accuracy.rs:78-900` - 22 cases on
  rolling/strong checksum interaction, including the duplicate-content
  case `multiple_identical_blocks_all_matched` (line 195) and adversarial
  collision cases at line 465 and 584.

- `crates/match/tests/rsum_collision_fixture.rs` - hand-constructed
  collision pairs that share rolling digests but differ in content. This
  is the bithash safety property in fixture form: any prefilter that
  admits a colliding rolling key MUST still be caught by the strong-sum
  gate. Cited verbatim in `docs/design/zsync-bithash.md` as the
  prerequisite for #2059.

- `crates/match/src/index/tests.rs:244-291`
  (`find_match_bytes_uses_strong_checksum_for_collision`) - blocks 0 and
  2 share content (and rolling digest); block 1 distinct. Verifies the
  strong-checksum disambiguates and that a wrong-content window is
  rejected even when its rolling key collides with an indexed block.

- `crates/engine/tests/delta_reconstruction_tests.rs:75-790` - 12
  apply-side cases including
  `duplicate_delta_block_references` (line 181) which builds a script
  that copies block 0 three times. This pins the apply path's behaviour
  for repeated block indices but does not exercise the generator
  emitting that pattern.

- `crates/engine/tests/delta_transfer_strategy_integration.rs:51-220` -
  end-to-end strategy tests asserting per-token statistics on shared
  prefixes, unrelated basis, and basis-equals-source.

### What is not covered

There is no golden test that asserts the byte sequence emitted by the
generator over a basis with duplicate-content blocks. The closest are
`multiple_identical_blocks_all_matched`
(`crates/match/tests/block_matching_accuracy.rs:195-230`), which counts
COPY tokens but does not pin their `index` fields, and the wire pinning
at `golden_protocol_v28_mplex_delta_stats.rs:260-286`, whose fixture
basis is implicit (block 0 only). This is the gap the prune optimization
must close before #2069 lands.

## Trace from match output to wire

The full path from a matched block index to a wire byte:

```
DeltaSignatureIndex::find_match_slices(digest, first, second)
  crates/match/src/index/mod.rs:151-174
    -> tag_table[digest.sum1()] gate           (line 161)
    -> lookup.get(&(sum1, sum2))               (line 165-166)
    -> find_match_slices_sequential(...)       (line 173)
       -> compute_truncated_slices(strong)     (line 184-186)
       -> for &index in candidates: ...        (line 187-194)
          first match wins -> return Some(idx)
       |
       v
DeltaGenerator::generate                       (crates/match/src/generator.rs:81-321)
  -> on Some(match_idx):                       (line 188)
       flush pending literals                   (line 206-212)
       tokens.push(DeltaToken::Copy {           (line 215-218)
           index: block.index(),
           len: block.len(),
       })
       want_i = Some(match_idx + 1)             (line 221-225)
  -> on None: byte goes to pending_literals     (line 144)
  |
  v
DeltaScript::new(tokens, ...)                  (crates/match/src/script.rs:55-61)
  |
  v
script_to_wire_delta(script) -> Vec<DeltaOp>   (crates/transfer/src/generator/delta.rs:340-352)
  -> DeltaToken::Copy { index, len }           (line 346-349)
     -> DeltaOp::Copy {
            block_index: index as u32,
            length: len as u32,
        }
  |
  v
write_token_stream(writer, &ops)               (crates/protocol/src/wire/delta/token.rs:158-170)
  -> for op in ops:
       DeltaOp::Literal(data) -> write_token_literal     (line 161-163)
                                  4-byte LE len + raw bytes (line 42-52)
       DeltaOp::Copy { block_index, .. }
           -> write_token_block_match           (line 164-167)
              4-byte LE write_int(-(block_index + 1))    (line 73-77)
     write_token_end -> write_int(0)            (line 169)
  |
  v
multiplex frame                                 (crates/protocol/src/wire/...)
  -> MSG_DATA frame header + token bytes
```

### Where reordering or alternative-but-equivalent token streams could leak

Three potential leak points exist; only one matters here:

1. **Block-index field of a COPY token.** `DeltaToken::Copy::index`
   propagates to `DeltaOp::Copy::block_index` and then to write_int(-(idx+1)).
   If two basis blocks share content, the chosen index can change without
   altering the applied bytes. This is the prune-only concern (see
   "Risk areas" below).

2. **Literal-vs-COPY split.** A position that today produces a literal
   stream could in principle become a COPY when an optimization changes
   which candidates the prober considers. None of the four optimizations
   does this. Bithash and compact-keys preserve the candidate set
   (one-sided rejection / pure layout change). seq-match only fires after
   `check_block_match_slices` confirms a match against the same `(sum1,
   sum2, strong)` triple the table-probe would have used. Prune can only
   shrink the candidate set, never enlarge it; the strong-checksum gate
   determines whether the position becomes a COPY.

3. **CHUNK_SIZE-driven literal flushing.** `DeltaGenerator::generate`
   flushes pending_literals when they exceed `block_len + CHUNK_SIZE`
   (`crates/match/src/generator.rs:148-154`) and again on each match
   (line 206-212). Optimizations that change the timing of matches could
   in principle reshape the literal-chunk boundaries. But all four
   optimizations preserve the offset at which a match is detected (a match
   confirmed by strong-checksum at byte offset O is the same offset
   regardless of how the prober reaches the candidate), so flush boundaries
   stay aligned.

## Per-optimization invariants

### Bithash (#2060-#2063)

**Invariant:** every block the bithash accepts is still verified by
full rsum + strong checksum. The accepted-match set is a subset of the
no-bithash set; the bithash is a one-sided prefilter that REJECTS only.

**Where the invariant is enforced:**

- `crates/match/src/index/mod.rs:151-174` (`find_match_slices`): the
  bithash check, when added by #2060, will sit between the existing
  `tag_table` gate (line 161) and the `lookup.get` probe (line 165-166).
  The strong-checksum gate at lines 187-194
  (`find_match_slices_sequential`, calling `compute_truncated_slices`)
  remains the authoritative match-acceptance step.
- The structural property is captured by
  `crates/match/tests/rsum_collision_fixture.rs:151-188`
  (`find_match_rejects_low_byte_collision_window` and
  `find_match_rejects_high_byte_collision_window`): even when a colliding
  rolling key is admitted, the strong-sum disqualifies the wrong-content
  window. A bithash that admits the same colliding key inherits this
  guarantee for free.
- The proptest contract for the bithash itself is documented in
  `docs/design/zsync-bithash.md` section 5: for every inserted block,
  `BitHash::probably_present` MUST return true. Test target is
  `crates/match/tests/bithash_no_missed_match.rs` (to be added by #2062).

**Why wire output is unchanged:** the bithash never appears in a
serialized type. The `DeltaSignatureIndex` field listed in
`docs/design/zsync-bithash.md` section 3
(`crates/match/src/index/mod.rs:38-48`) is private to the crate. No
`crates/protocol/` type changes; no `Cargo.toml` capability flag; no
CLI flag.

### Seq-match (#2065-#2067)

**Invariant:** extended-run COPY tokens produce identical concatenated
bytes to the equivalent sequence of single-block COPY tokens. The
seq-match path is allowed to return `hint = match_idx + 1` only when
the table-probe path on the same window position would also return
`match_idx + 1`. The current implementation already enforces this via
`check_block_match_slices`
(`crates/match/src/index/mod.rs:229-251`), which performs the SAME
rolling-sum equality and strong-checksum equality checks the table-probe
path would perform.

**Where the invariant is enforced:**

- `crates/match/src/index/mod.rs:243-250`:
  `check_block_match_slices` compares `digest.sum1()/sum2()` against
  `block.rolling().sum1()/sum2()` and the truncated strong checksum
  against `block.strong()`. This is byte-identical to the gate run by
  `find_match_slices_sequential` (line 184-194).
- `crates/match/src/generator.rs:177-187` (byte-by-byte hint check) and
  `crates/match/src/generator.rs:259-273` (bulk-refill hint check) are
  the two firing sites; both fall back to `find_match_slices` on
  hint-miss without changing `want_i`.
- `apply_delta` round-trip is the byte-equality check:
  `crates/match/src/script.rs:105-171` reads each `DeltaToken::Copy
  { index, len }` and writes `len` bytes from basis offset `index *
  block_length`. A run of N adjacent COPY tokens vs a single COPY
  spanning N blocks produces the same output IFF each token's `index`
  matches the basis layout.
- The byte-equality property is exercised by
  `crates/match/tests/integration_tests.rs::all_matches_identical_files`
  (line 279) and
  `crates/match/src/generator.rs::generate_delta_finds_matching_blocks`
  (line 379), both of which run the generator on a basis-equals-source
  scenario and verify the reconstructed bytes match the input.

**Why wire output is unchanged:** the hint surface
(`want_i: Option<usize>`) lives in `DeltaGenerator::generate`'s local
scope (`crates/match/src/generator.rs:103`). It never escapes the
function. The duplicate-block tie-breaker analysis in
`docs/design/zsync-seq-match.md` section "The duplicate-block
tie-breaker" proves that for a sequential-traversal scenario the
seq-match `hint = i+1` is exactly the same index the no-hint
`find_match_slices` would have picked (smallest matching basis index),
because the basis blocks `0..i` have already been emitted as COPYs and
`i+1` is the next-smallest candidate when content matches.

### Matched-block pruning (#2069-#2071)

**Invariant:** removing already-matched basis blocks from the index
must respect duplicate-basis-block correctness. An input that
legitimately matches the same basis block content twice (because two
basis blocks share that content) MUST still emit two COPY tokens, one
per source occurrence, until basis siblings are exhausted.

**Where the invariant is checked today:**

- The apply-side duplicate-block correctness is pinned by
  `crates/engine/tests/delta_reconstruction_tests.rs:181-218`
  (`duplicate_delta_block_references`), which builds a script with
  three COPY tokens all referencing block 0 and verifies the reconstructed
  output is the basis content concatenated three times. This pins
  `apply_delta`'s behaviour, NOT the generator's.

- The generator-side duplicate-content scenario is touched by
  `crates/match/tests/block_matching_accuracy.rs:195-230`
  (`multiple_identical_blocks_all_matched`): basis is the same 700-byte
  pattern repeated 5 times, source is the same pattern repeated 3 times.
  The test asserts `copy_count >= 3` and `apply(script) == source`. It
  does NOT pin which basis indices are named in the COPY tokens.

- The collision/duplicate-content lookup correctness is pinned at
  `crates/match/src/index/tests.rs:244-291`
  (`find_match_bytes_uses_strong_checksum_for_collision`): two blocks
  with identical content, both in the lookup bucket; verifies that the
  strong-sum gate still resolves to a valid block index.

**Where the invariant is NOT yet pinned (the gap):**

- No existing test asserts the byte-for-byte token sequence emitted by
  `generate_delta` over a duplicate-basis-block input. Today the bucket
  walk is deterministic (block-index ascending, see
  `crates/match/src/index/builder.rs:23` - `for (index, block) in
  blocks.iter().enumerate()`). With prune ON, the bucket walk picks the
  lowest unset index. As argued in `docs/design/zsync-prune.md` section
  "Wire-compat invariant", the resulting COPY-token `index` fields are
  the same first-N-siblings-in-order even without prune, but only because
  today's pre-prune code does not re-emit the same basis index across a
  bucket. Confirming this empirically requires a wire-byte regression
  test described in "Stability test recommendation" below.

**Why wire output is byte-identical when basis content is unique
per block:** the COPY-token `index` field is uniquely determined by the
matched-block's basis offset; with non-duplicate content no two blocks
share a `(sum1, sum2, strong)` triple, so the bucket has at most one
candidate and prune cannot route a match to a different sibling.

### Compact-keys (#2072-#2073)

**Invariant:** lookup behaviour unchanged; only the in-memory layout of
`DeltaSignatureIndex::lookup` differs (see
`docs/design/zsync-hash-layout-decision.md` for the three options
considered). The set of candidate block indices for a given `(sum1,
sum2)` digest is the same under any container choice.

**Where the invariant is enforced:**

- `crates/match/src/index/mod.rs:78-100` (`find_match_bytes`) and
  `crates/match/src/index/mod.rs:151-174` (`find_match_slices`) are the
  two public probe entry points. Both delegate to
  `find_match_*_sequential` for strong-sum verification; that code is
  unchanged.
- The 12 correctness cases in `crates/match/src/index/tests.rs` (lines
  8-410) including `find_match_bytes_uses_strong_checksum_for_collision`
  (line 244) and `rebuild_reuses_allocation` (line 294) are the gate.
  They do not depend on the container choice; whichever option lands
  must keep them green without modification.
- The roundtrip-on-wire goldens listed in the "Inventory" section above
  are the wire-side gate. The `DeltaSignatureIndex::lookup` field never
  appears in a `crates/protocol/` type or in any wire-serialized payload.

**Why wire output is byte-identical:** the in-memory lookup is purely a
search accelerator. The `blocks: Vec<SignatureBlock>` field that holds
the wire-serializable signature payload is unchanged
(`crates/match/src/index/mod.rs:42`). The
`SignatureBlock` rolling+strong fields are byte-identical under any
container.

## Risk areas

### Tie-breaking when multiple basis blocks could match

**Today's behaviour (oc-rsync):** the lookup bucket
`crates/match/src/index/mod.rs:44`
(`FxHashMap<(u16, u16), Vec<usize>>`) is built by
`crates/match/src/index/builder.rs:23-34` (`populate_index`). The build
loop iterates `blocks.iter().enumerate()` so the bucket vector is in
ascending block-index order. `find_match_*_sequential`
(`crates/match/src/index/mod.rs:114-124` and lines 178-194) walks the
candidates left-to-right and returns the FIRST index whose strong
checksum matches. So for blocks `j < k` with equal `(sum1, sum2,
strong)`, the table-probe path always returns `j`.

**Upstream rsync (3.4.1):** the local mirror at
`target/interop/upstream-src/rsync-3.4.1/` is not present in this
worktree (the directory does not exist). The companion design notes
already cite upstream's behaviour:

- `docs/design/zsync-prune.md` "What zsync does" section states
  upstream's matched-block chain stays intact for the duration of the
  transfer (no prune), and the bucket walk is deterministic in
  block-index order.
- `docs/design/zsync-seq-match.md` "Mapping to oc-rsync architecture"
  cites upstream `match.c:144-190` as the `want_i` adjacent-match hint
  pattern oc-rsync already mirrors at `crates/match/src/generator.rs:103`
  and lines 177-187.
- `docs/design/zsync-inspired-matching.md` "Not present in upstream
  rsync" section confirms upstream uses the simple `bool` tag table at
  `match.c:45-51` and does not implement any of the four zsync
  techniques.

**Optimization-by-optimization risk:**

- **Bithash (#2060):** zero risk. One-sided rejection. Tie-breaking is
  unaffected because the bithash is consulted before the bucket walk,
  not during it.
- **Seq-match (#2065):** zero risk for sequential traversals. The
  hint-fires-only-when-`hint == lowest-matching-index` argument is
  proven in `docs/design/zsync-seq-match.md` "The duplicate-block
  tie-breaker" section. The hint is cleared on miss; on the next
  iteration the table-probe path runs and picks the smallest matching
  index.
- **Prune (#2069):** moderate risk. If the bucket has duplicates `[7,
  42, 99]` and source matches block 7 first, prune sets bit 7 and the
  next probe walks `[42, 99]`. The selected index is 42, NOT 7. Today's
  no-prune code on the same input would walk `[7, 42, 99]` and return 7
  again because the strong-sum matches. Both choices apply the same
  bytes (siblings are byte-identical), but the wire-emitted index is
  different. This is wire-equivalent (same applied output) but not
  bit-identical at the level of the index field.
- **Compact-keys (#2072):** zero risk for any of the three options
  considered (FxHashMap, sorted-vec binary search, open-addressed
  table). Each option preserves the bucket-order-by-block-index
  invariant established at build time
  (`crates/match/src/index/builder.rs:23-34`).

The prune risk is mitigated by `docs/design/zsync-prune.md` section
"Wire-compat invariant" which observes that the today's golden corpus
uses unique-per-block basis content (see "Inventory" gap above), so the
mismatch surface does not exist in the existing fixtures. Any future
fixture introducing duplicate basis content into a golden test would
need to be pinned against prune-enabled output.

### Adjacent edge cases

- **Append/inplace.** `crates/transfer/src/transfer_ops/request.rs`
  short-circuits signature-block emission for append mode. Generator is
  not invoked. None of the four optimizations runs in this path.
- **INC_RECURSE per-segment rebuild.** `MatchIndex::build` and
  `crates/match/src/index/builder.rs::rebuild` (line 80-98) reset the
  index between files. The bithash, prune bitmap, and seq-match `want_i`
  are all per-generator-session state; the rebuild semantics from the
  parent design (Translation rules item 4) preserve correctness.
- **Whole-file path.**
  `crates/protocol/src/wire/delta/token.rs:115-118`
  (`write_whole_file_delta`) bypasses signature exchange entirely. None
  of the optimizations applies; the wire is `Literal + end_marker` only.
- **Compressed-token path (CPRES_ZLIB).**
  `crates/transfer/src/generator/delta.rs:378-434`
  (`write_delta_with_compression`) feeds matched basis bytes to the
  encoder dictionary after each COPY. If prune routes a duplicate-content
  match to a sibling, the dictionary feed reads that sibling's bytes
  from the source file. Because siblings are byte-identical, the deflate
  stream sees the same bytes regardless of which index is named. Wire
  output stays byte-identical at the deflate level.

## Stability test recommendation

For each PR landing one of the optimizations, the following pinning
tests must run green:

| PR     | Tests that must stay green                                                                                |
|--------|-----------------------------------------------------------------------------------------------------------|
| #2060  | `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs::*` (full file, esp. `*_mixed_ops_*`)     |
|        | `crates/match/tests/rsum_collision_fixture.rs::*` (5 tests, the bithash safety property)                  |
|        | `crates/match/src/index/tests.rs::find_match_bytes_uses_strong_checksum_for_collision`                    |
|        | `crates/match/tests/integration_tests.rs::*` (~70 cases)                                                  |
|        | `crates/match/tests/block_matching_accuracy.rs::*` (22 cases)                                             |
|        | `crates/protocol/tests/proptest_delta_roundtrip.rs::*`                                                    |
|        | `crates/protocol/tests/proptest_delta_script_roundtrip.rs::*`                                             |
| #2065  | All of #2060's set, plus the duplicate-content cases (`multiple_identical_blocks_all_matched`)            |
| #2069  | All of #2065's set, **plus a NEW wire-byte test** (see below)                                             |
| #2072  | All of #2069's set                                                                                        |
| #2086  | Full re-run of all golden tests across all four optimizations active simultaneously                       |

### NEW golden test required for #2069 (prune)

The gap identified in this audit is that no test pins the byte sequence
emitted by `generate_delta` over a basis with duplicate-content blocks.
The recommendation is a new test, scoped to #2069's PR:

- Location: `crates/match/tests/duplicate_basis_block_token_sequence.rs`
  (new file).
- Shape: build a basis with three blocks of identical content
  `[A, A, A]` (each block of canonical block_length).
- Source: same content, two blocks worth (`AA`).
- Run: `generate_delta(source, basis_index)`.
- Pin: assert the COPY-token sequence is exactly
  `[Copy{index:0,len:N}, Copy{index:1,len:N}]` (with prune-enabled
  semantics) AND assert that the wire bytes produced by
  `script_to_wire_delta(...) -> write_token_stream(...)` decode back to
  the same `[block_index:0, block_index:1]` pair via `read_token`.
- Apply-side check: `apply_delta(basis_cursor, output, &index, &script)`
  produces `output == source`.

This pins three properties simultaneously: (1) prune routes consecutive
duplicate matches to consecutive sibling indices (0 -> 1, not 0 -> 0),
(2) the wire encoding of `block_index:1` is `write_int(-2)` =
`[0xFE, 0xFF, 0xFF, 0xFF]`, (3) apply-side reconstruction is correct.

A second case should cover `[A, B, A]` source against `[A, A]` basis:
the second `A` in the source must match basis block 1 under prune (with
block 0 already consumed), emitting `[Copy{0}, Literal{B}, Copy{1}]`.
This pins the empty-`unpruned` fallback when the `A` content has been
fully consumed but more occurrences arrive.

For #2086 ("final cleanup"), this test must run with all four
optimizations active simultaneously to confirm the combined behaviour.
The test is referenced explicitly by `docs/design/zsync-prune.md`
"Property test contract (#2070)" which already sketches the proptest
shape; this audit's recommendation is the deterministic golden-byte
companion to that proptest.

### Ongoing CI obligations

Each PR must additionally:

- Run the wire encoder roundtrip proptests
  (`proptest_delta_roundtrip.rs`, `proptest_delta_script_roundtrip.rs`)
  with at least 1024 iterations.
- Run `tools/ci/run_interop.sh` against upstream 3.0.9, 3.1.3, 3.4.1
  and produce zero new entries in `tools/ci/known_failures.conf`.
- For #2069 specifically: capture a tcpdump of an oc-rsync push of a
  duplicate-block-heavy fixture (e.g. a sparse VM image) to an upstream
  rsync daemon, with prune ON and OFF, and confirm wire-equivalence
  per the project's "no wire protocol features" rule. The infrastructure
  for this lives at `scripts/rsync-interop-server.sh` and the existing
  tcpdump audits
  (`docs/audits/tcpdump-daemon-filter-pull.md`,
  `docs/audits/tcpdump-daemon-proto28-29.md`).

## Conclusion

**PROVEN STABLE for bithash, seq-match, and compact-keys.** The wire
boundary is the `DeltaOp` token stream emitted by
`crates/protocol/src/wire/delta/token.rs::write_token_stream` plus the
sum_head pinned at
`crates/protocol/tests/golden_protocol_v29_wire.rs::golden_v29_sum_head_md4`
(line 275). Bithash adds a one-sided prefilter
(`crates/match/src/index/mod.rs:165` insertion point) whose accepted-set
is a subset of the no-bithash set, with the strong-sum gate
(`find_match_*_sequential` at `crates/match/src/index/mod.rs:114-124,
178-194`) as the unchanged authoritative match-acceptance step.
Seq-match's extension via `check_block_match_slices`
(`crates/match/src/index/mod.rs:229-251`) verifies the SAME
`(sum1, sum2, strong)` triple as the table-probe path, so the chosen
index field is provably equal to the no-hint path's choice.
Compact-keys is a pure layout change that preserves the
`(sum1, sum2) -> Vec<block_index>` mapping verified by the 12
correctness cases at `crates/match/src/index/tests.rs:8-410`.

**ONE GAP for prune.** Today's golden corpus does not include a
fixture that pins the wire bytes emitted by `generate_delta` over a
basis with duplicate-content blocks. The closest existing tests
(`crates/match/tests/block_matching_accuracy.rs::multiple_identical_blocks_all_matched`
at line 195;
`crates/engine/tests/delta_reconstruction_tests.rs::duplicate_delta_block_references`
at line 181;
`crates/match/src/index/tests.rs::find_match_bytes_uses_strong_checksum_for_collision`
at line 244) exercise the lookup-side correctness and the apply-side
reconstruction, but not the generator-emitted byte sequence. Before
#2069 (prune impl) lands, the new golden test described in "Stability
test recommendation" must be added. After that addition, all four
optimizations are gated by tests that pin both the in-memory candidate
set and the wire-emitted byte sequence, satisfying the parent design
note's wire-compat invariants
(`docs/design/zsync-inspired-matching.md` section "Wire-compat
invariants" items 1-4).
