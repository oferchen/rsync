# zstd as a batch-compatible compression alternative

Tracking issue: oc-rsync task #1685.

## Summary

This audit asks whether zstd can serve as a batch-compatible compression
alternative for `--write-batch` and `--read-batch`. Conclusion: conditionally
yes for self-hosted batch round-trips, no for cross-tool interop with
upstream. zstd's framing is independent enough that a batch can replay
tokens without replaying compressor state, but the upstream batch header
records only `do_compression` (bit 8) without naming the algorithm, so a
zstd batch is indistinguishable from a zlib batch on the wire and would be
mis-decoded by upstream rsync.

## The batch + compression problem

Upstream rsync tees the pre-decompression wire bytes into the batch file
via `io.c:read_buf()` (cited in `crates/batch/src/reader/delta.rs:90`). The
batch header sets bit 8 (`do_compression`) when `--compress` was active, and
upstream `batch.c:check_batch_flags()` restores that flag on replay. The
header never records which algorithm was used: upstream `compat.c:194-195`
hard-codes `CPRES_ZLIB` for batch reads, and `compat.c:413-414` forces
`compress_choice = "zlib"` for batch writes. See
`crates/batch/src/replay.rs:45-62` for the codec enum and the comment block
that documents this exact upstream limitation.

zlib breaks batch replay because `Z_SYNC_FLUSH` deflate blocks share an
inflate dictionary across tokens. Upstream `token.c:see_deflate_token()`
feeds matched basis-block bytes into the inflate dictionary so the next
literal can resolve back-references that crossed a block boundary. Replay
must reproduce that dictionary feed in lockstep, which is why
`crates/batch/src/replay.rs:475-479` flips a `cpres_zlib` flag that drives
the per-token `see_token()` path at `replay.rs:807-824`. A batch replayed
without this synchronisation aborts with an "invalid distance" inflate
error.

oc-rsync side-steps the dictionary problem by capturing post-decompression
bytes (`crates/core/src/client/run/batch.rs:97-107` writes
`do_compression=false` regardless of `--compress`), keeping batches
self-decodable. PR #3202 (`845ab60bf`) briefly rejected `--write-batch +
--compress` at protocol 28-29; it was reverted in `d61ebdf5e` because
upstream never rejects that combination. The user-visible note today lives
in `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs:40-42`.

## zstd framing

zstd uses an explicit frame format with a magic number, a frame header,
self-delimited blocks, and an optional content checksum. Each frame is
independently decodable without prior decoder state. oc-rsync's
`ZstdTokenEncoder::flush()` (see `crates/compress/src/zstd.rs:78-93`)
materialises pending bytes and lets the receiver decompress incrementally,
matching upstream `token.c:send_zstd_token()`. PR #1115 / #3047
(`2b8d524c5`) aligned our per-token flush boundaries so each token's output
sits in a single `MAX_DATA_COUNT` buffer.

Crucially, `crates/batch/src/replay.rs:475-479` already records that "zstd
does not need dictionary sync (`see_token` is a noop)". That property is
what makes zstd batch-friendly: the replayer can decode each token without
a parallel `see_deflate_token()` feed, and dropping a token does not
poison subsequent frames. The auto-detection scaffolding in
`replay.rs:985-988` already tries zstd if zlib decode fails, anticipating
"hypothetical or patched upstream" zstd batches.

## Trade-offs

- Wire format. The batch body would carry zstd frames where the upstream
  spec assumes zlib `DEFLATED_DATA` blocks. Cross-tool replay with stock
  upstream rsync would silently mis-decode without the auto-detect probe
  we already ship.
- Protocol gating. `do_compression` only exists from protocol >= 29 in
  `crates/batch/src/format/flags.rs:34-41`. Protocol 28 has no bit at all
  for compression, so any zstd batch produced for a 28-only peer is
  unreadable by upstream regardless of algorithm.
- Header changes. The cleanest option is a new bit (or a small algorithm
  byte) in `BatchFlags`. That is a wire-format extension, which the
  project's "no wire-protocol features for niche perf" rule rejects. A
  safer alternative is to peek at the first frame's magic bytes (zstd
  `0xFD2FB528` vs zlib `0x78 0x9C/0xDA`) at replay time, the way
  `create_compressed_decoder` is already wired.
- Testing burden. Adds a {zlib, zstd} x {self, ours-read-upstream,
  upstream-read-ours} x protocol 28..32 matrix. Cells where upstream reads
  our zstd batches cannot pass without a patched upstream and should be
  encoded as KNOWN_FAILURES.

## Recommendation

Conditional Yes - permit zstd batches for self-round-trip only, gated
behind a flag, and never claim cross-tool interop. Sketch:

1. Extend `BatchWriter` to optionally select zstd when both
   `--write-batch` and `--compress=zstd` are set. Continue to write
   `do_compression=true` so existing oc-rsync replayers route through the
   compressed path; rely on the existing magic-byte probe in
   `crates/batch/src/replay.rs:985-988` to switch the decoder.
2. Add a self round-trip integration test (`crates/batch/tests/`) and an
   explicit interop KNOWN_FAILURE entry asserting that upstream rsync
   cannot read our zstd batches. Document the asymmetry in the existing
   "compressed batch limitation" docs note.
3. Keep zlib as the default for `--write-batch + --compress` to preserve
   the current upstream-compatible behaviour. Surface zstd via
   `--compress-choice=zstd` only, never as an implicit upgrade.

If even step 1 cost is judged too high relative to the niche benefit, the
alternative is to leave the current zlib-only batch path in place and rely
on the existing CLI help text that documents the protocol-28 restriction.

## References

- `crates/batch/src/replay.rs:45-62` - codec enum and upstream cite.
- `crates/batch/src/replay.rs:461-479` - `do_compression` decoder gating.
- `crates/batch/src/replay.rs:807-824` - per-token `see_token()` feed.
- `crates/batch/src/replay.rs:985-988` - zstd auto-detection probe.
- `crates/batch/src/format/flags.rs:34-41` - bit 8 do_compression.
- `crates/batch/src/reader/delta.rs:78-90` - upstream `io.c:read_buf()`.
- `crates/core/src/client/run/batch.rs:97-107` - oc-rsync sets bit 8 = false.
- `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs:40-42`.
- `crates/compress/src/zstd.rs:78-93` - per-token flush, cites `token.c`.
- Upstream `batch.c:59-76`, `compat.c:194-195`, `compat.c:413-414`,
  `token.c:send_deflated_token`, `token.c:send_zstd_token`.
