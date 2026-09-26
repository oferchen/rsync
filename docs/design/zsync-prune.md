# zsync matched-block pruning (removed)

**Status: removed.** oc-rsync shipped a zsync-style matched-block prune
(#2068-#2071, ZSO-3) and later removed it because it changed the wire.
This note records what it did, why it was wrong, and what replaced it.

## What it did

After the sender emitted a `Copy` token for basis block `i`, two
structures retired that block for the rest of the file:

- a per-scan `MatchedBlocks` bitmap, consulted by every chain walk;
- a shared `consumed` bitset of `AtomicU64` words on
  `DeltaSignatureIndex` (`mark_consumed` / `is_consumed`).

A later source window with the same content could then only match a
not-yet-used duplicate sibling. With no sibling left it went out as
literal data. The idea came from zsync's `remove_block_from_hash`
(`librcksum/hash.c:111-128`), where a written target block is never
needed again.

## Why it was not wire-neutral

The design claimed the prune was an in-memory optimization with no
wire effect. That was false. rsync's sender must answer every source
window that carries a basis block's content. upstream `match.c`
`hash_search()` (lines 229-345) never retires a chain entry outside
`--inplace`, so a basis holding block C once answers N copies of C in
the source with N copy tokens.

With the prune, oc sent one copy token and N-1 literal runs. That
changed:

- the token stream and therefore the bytes on the wire;
- the `--stats` "Literal data" and "Matched data" values;
- the transfer size, most visibly on sparse or VM-like images, where
  many zero blocks in the source map onto a few zero blocks in the basis.

The parallel scan (`generate_chunked`) already ran with the prune off,
so the sequential and parallel paths also disagreed on such inputs.

## What replaced it

The prune, `MatchedBlocks`, the `consumed` bitset and the
`with_prune_matched` toggle are gone. The scan never writes the index,
so a basis block matches as often as its content appears, exactly as
upstream does.

The bucket chain order was also brought in line with upstream. upstream's
`build_hash_table()` head-inserts (`match.c:98-110`), so a chain walk
tries the highest block index first. oc used to walk in ascending order,
which picked a different sibling among duplicate-content blocks and so a
different `Copy` index on the wire. `CompactLookup::insert` now
head-inserts too.

The one retirement upstream does perform stays: under `--inplace` a
candidate whose basis offset precedes the write cursor is skipped
(`match.c:232-240`). oc implements the offset rule through
`DeltaGenerator::with_updating_basis_file`, but not yet the rest of
upstream's `updating_basis_file` handling: the `SUMFLG_SAME_OFFSET`
exemption, the preference for the block at the identical offset
(`match.c:278-289`), the zero-run re-alignment (`match.c:293-315`) and the
guarded `want_i` check (`match.c:321-333`). Those are tracked separately;
until they land, `--inplace` token streams can still differ from
upstream.

The bithash prefilter and the compact key are unaffected and remain
wire-neutral.

## Tests

- `crates/matching/tests/repeated_basis_block.rs` pins the exact
  per-block token sequence for a repeated block and for a zero-heavy
  image, and the sequential/parallel token equality.
- `crates/test-support/tests/upstream_delta_token_parity.rs` compares
  oc's sender wire stream byte for byte with upstream rsync on the same
  two fixtures.
