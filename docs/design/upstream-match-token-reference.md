# Upstream match.c and token.c reference for delta work

This is the authoritative line-by-line reference for upstream rsync's
`match.c` and `token.c` as they apply to oc-rsync's delta pipeline.
Downstream tasks #1579 (real delta), #1581 (DeltaGenerator wiring), and
#1582 (integration test) should cite this document rather than re-deriving
the upstream semantics each time.

Surveyed sources (release tarballs in `target/interop/upstream-src/`):

- `rsync-3.4.2/match.c` (448 lines, identical content to `rsync-3.4.1/match.c`).
- `rsync-3.4.2/token.c` (1131 lines, diverges from `rsync-3.4.1/token.c`
  in a handful of guards documented in section 5).

oc-rsync counterparts cited inline by absolute path. All paths in this
document are relative to the repository root.

## 1. match.c structure

`match.c` is small (sub-450 lines) and centred on three pieces of state:
a hash table keyed by the rolling checksum, a sliding `offset` walk, and
a `last_match` cursor that marks how much of the sender file has been
encoded so far. The public entry is `match_sums()`.

### 1.1 The hash table and BIG_TABLE optimisation

- `match.c:45` defines `TRADITIONAL_TABLESIZE` as `1 << 16` (65 536). For
  small basis files this stays the table width.
- `match.c:50-53` defines the two hash macros:
  `SUM2HASH2(s1,s2) = ((s1) + (s2)) & 0xFFFF` for the traditional sized
  table, and `BIG_SUM2HASH(sum) = (sum) % tablesize` for the
  dynamically-sized variant.
- `match.c:55-88` `build_hash_table()` sizes the table for ~80% load on
  large files: `tablesize = (s->count/8) * 10 + 11`, bumped to
  `TRADITIONAL_TABLESIZE` when that yields a smaller width. The `+11`
  comment in the source explains that for the BIG path the value must be
  odd so a multiplicative `s2` can span the full range.
- `match.c:66-71` keeps a static `alloc_size` and only reallocates when
  the requested table is larger or more than 16 KiB smaller, avoiding
  thrash across files in one session.
- `match.c:73` zero-fills with `0xFF` (all bits set) so `chain` heads of
  `-1` mark empty buckets.
- `match.c:75-87` populates the table. Each block stores its predecessor
  bucket head in `s->sums[i].chain`, threading a singly-linked chain per
  bucket. The chain head moves to `i`. The two branches are the
  `TRADITIONAL_TABLESIZE` path using `SUM2HASH` and the BIG path using
  `BIG_SUM2HASH`. Only the bucket calculation differs.

oc-rsync analogues:

- `crates/matching/src/index/mod.rs:45` `TAG_TABLE_SIZE: usize = 1 << 16`
  reproduces `TRADITIONAL_TABLESIZE`, but as a tag-only short-circuit
  (boolean per `sum1`), not a chain head store.
- `crates/matching/src/index/mod.rs:60-77` keeps the tag table, a
  `CompactLookup` open-addressing slot table and a zsync-style `BitHash`
  prefilter. We do not implement the dynamic 80% load resize; the lookup
  table grows via `CompactLookup::with_capacity` once per signature
  (`crates/matching/src/index/builder.rs:71-86`). #2072 tracks the
  packed-key compaction work that would benefit from a BIG_TABLE-style
  dynamic resize.
- The cluster of `--debug=HASH` lifecycle emissions in
  `crates/matching/src/index/builder.rs:100-103` and
  `:139-145` mirrors upstream's `hashtable.c:45-53` and `:100-103`
  create / grow lines that share the BIG_TABLE allocator.

### 1.2 hash_search() lookup walk

- `match.c:140-345` `hash_search(f, s, buf, len)` is the main loop.
- `match.c:155` initialises `want_i = 0`, the next-block hint that
  encourages RLE-friendly adjacent matches.
- `match.c:162-164` reads the first window via `map_ptr(buf, 0, k)`
  where `k = MIN(len, blength)`. The block-aligned read sets up the
  initial `(s1, s2)` checksum at `match.c:166-168`.
- `match.c:172` resets `offset`, `aligned_offset`, and `aligned_i` to
  zero. The aligned tracking is for in-place mode (`updating_basis_file`).
- `match.c:174` computes `end = len + 1 - s->sums[s->count-1].len`. The
  `-last_block_len` term is important because the last basis block may
  be short. Upstream walks `offset` past `end-1` only when the trailing
  partial block cannot match, then drops to the matched-tail flush below.
- `match.c:181-341` is the do/while window walk. Each iteration computes
  one hash probe at `offset`. The body falls into three sections:
  hash-table lookup, post-match window jump, and rolling-checksum slide.

#### 1.2.1 Hash-chain probing

- `match.c:191-201` selects the bucket via `SUM2HASH2(s1, s2)` or
  `BIG_SUM2HASH(sum)`. The traditional path computes `sum` only after
  the probe to avoid a redundant assembly when the bucket is empty.
- `match.c:193-200` `goto null_hash` when `hash_table[hash_entry] < 0`
  (empty bucket). This jumps past the entire chain walk to the rolling
  update at `match.c:314`.
- `match.c:202` aliases `prev` to the chain head pointer so the bypass
  pruning at `match.c:211-215` can splice an entry out of the chain
  by writing `*prev = s->sums[i].chain`.
- `match.c:204` increments `hash_hits` once per bucket visited.
- `match.c:205-312` walks the chain via `i = s->sums[i].chain`.

#### 1.2.2 The (sum1, sum2) early reject

- `match.c:218-219` rejects on `sum != s->sums[i].sum1` before any
  strong-checksum work. This is the principal cycle saver in the inner
  chain. `sum` is the packed 32-bit value `(s1 & 0xffff) | (s2 << 16)`.
- `match.c:221-224` then checks the block lengths match, since the
  trailing block may be short.
- `match.c:232-241` only computes the strong checksum
  (`get_checksum2()`) on the first survivor in the chain, caching the
  result in `done_csum2`. The comparison length is `s->s2length` which
  is the negotiated truncation (commonly 2 in phase 1, up to 16 in
  phase 2 redo).
- `match.c:238-241` increments `false_alarms` on strong-checksum
  mismatch and continues to the next chain entry.

The "why s1/s2 are mixed as they are" question: upstream packs `s1` in
the low 16 bits and `s2` in the high 16 bits because the rolling
algorithm naturally produces `s2` as an accumulator scaled by window
length. The two halves are independent under the rolling update
(`match.c:323-329`): on a one-byte slide, `s1` loses `map[0]` and gains
`map[k]`; `s2` loses `k * map[0]` and gains the new `s1`. Packing them
into a single `uint32` lets the table key, the bucket calculation, and
the chain reject all read one machine word. The
`SUM2HASH2(s1, s2) = (s1 + s2) & 0xFFFF` mixing folds the s2 bits into
the same 16-bit bucket so the chain length stays bounded even when one
half is degenerate.

oc-rsync analogues:

- `crates/checksums/src/rolling/digest.rs:178-207` packs
  `(s2 << 16) | s1` identically and exposes `sum1()`, `sum2()`, and the
  packed `value()` accessor used by `BitHash`.
- `crates/matching/src/index/mod.rs:146-150` is the tag table
  fast-reject (`tag_table[s1]`), upstream's first cycle saver.
- `crates/matching/src/index/mod.rs:153-156` adds the zsync-style
  `BitHash::contains(value)` prefilter mixing both halves before any
  hash probe, equivalent in spirit to the chain prune at
  `match.c:211-215` but probed without traversing the chain.
- `crates/matching/src/index/mod.rs:160-169` performs the strong
  verify (`SignatureAlgorithm::compute_truncated`) and the
  `strong.as_slice() == block.strong()` compare. The truncation length
  is `self.strong_length`, equivalent to upstream's `s->s2length`.

#### 1.2.3 The want_i adjacency hint

- `match.c:289-300` is the `check_want_i` branch. After a confirmed
  match at index `i`, upstream sets `want_i = i + 1` so the next probe
  prefers the immediately following basis block. The branch also
  swaps `i = want_i` when both rolling and strong checksums match
  there. This is the "RLL coder will be happy" optimisation - adjacent
  matches compress better in the token-run encoding (`token.c` section
  2.2 below).
- `match.c:301` unconditionally sets `want_i = i + 1` so a non-adjacent
  match still points the hint at the next basis block, preserving the
  property when the source skips ahead.

oc-rsync analogue:

- `crates/matching/src/generator.rs:163` initialises
  `want_i: Option<usize> = Some(0)`.
- `crates/matching/src/generator.rs:251-264` probes the hint first
  (`check_block_match_slices` at
  `crates/matching/src/index/mod.rs:240-263`), only falling back to
  `find_match_slices_filtered` on a hint miss. The hint check skips
  both the tag table and the hash probe.
- `crates/matching/src/generator.rs:337-341` advances `want_i` to
  `match_idx + 1` after each confirmed match. The chained adjacent-match
  loop at `:280-399` mirrors `match.c:303-310` window refill.

#### 1.2.4 Match window advance and post-match jump

- `match.c:303-310` is the "matched" hot path:
  - `matched(f, s, buf, offset, i)` emits the literal/token pair
    (section 1.3 below).
  - `offset += s->sums[i].len - 1`. The `-1` compensates for the `++offset`
    at the bottom of the do/while.
  - `k = MIN(blength, len-offset)` reads the next block-sized window via
    `map_ptr(buf, offset, k)` and recomputes `sum` from scratch with
    `get_checksum1((char *)map, k)`. The full recompute is necessary
    because the slide invariants break across an arbitrary block jump.
- `match.c:312` `break` exits the chain walk; control falls through to
  the iterator's `++offset` at the bottom.

oc-rsync analogue:

- `crates/matching/src/generator.rs:266-399` implements the same jump.
  After a match, the ring buffer is cleared
  (`crates/matching/src/generator.rs:343`), the next block-sized window
  is bulk-refilled (`:349-362`), and the rolling checksum is rebuilt
  via SIMD `RollingChecksum::update` on the two-slice view
  (`:369-373`).

#### 1.2.5 The rolling slide

- `match.c:314-340` is the `null_hash:` label and the byte-by-byte
  slide:
  - `backup = offset - last_match`, clamped to `>= 0`. Upstream reads
    one byte before `last_match` to keep the slide rolling across the
    boundary, accessed via `map_ptr(buf, offset - backup, ...)`.
  - `more = offset + k < len` is the EOF guard; when `more == 0`
    upstream decrements `k` instead of pulling a phantom byte.
  - `s1 -= map[0] + CHAR_OFFSET` and
    `s2 -= k * (map[0] + CHAR_OFFSET)` remove the leaving byte. The
    `k * map[0]` for `s2` is the closed-form contribution of the
    leaving byte across the entire window.
  - When `more`, the new byte `map[k]` is added: `s1 += map[k] +
    CHAR_OFFSET` and `s2 += s1`. The new-byte contribution is exactly
    the post-update `s1`.
  - `CHAR_OFFSET` is defined as `0` for protocol >= 27 (see
    `rsync.h:175`); oc-rsync inherits the protocol-32 default.

oc-rsync analogues:

- `crates/matching/src/generator.rs:208-228` performs the same single
  byte rolling update via `RollingChecksum::roll(outgoing_byte, byte)`
  (SIMD-accelerated path) and `pending_literals.push(outgoing_byte)`,
  with the same outgoing-byte-becomes-literal semantics.
- The ring buffer at `crates/matching/src/ring_buffer.rs` replaces
  upstream's `backup`-byte trick. The two-slice `as_slices()` view
  lets us avoid the rotate and read directly without re-mapping the
  basis-buffer span.

### 1.3 End-of-block flush and matched()

- `match.c:91` defines the file-scoped `last_match`. Every literal flush
  and every token emit advances `last_match` so the next emission knows
  what range of source bytes still needs to ship.
- `match.c:105-137` `matched()` is the workhorse. The signature is
  `matched(f, s, buf, offset, i)` where:
  - `i >= 0` emits literal up to `offset` followed by a match token for
    block `i` of length `s->sums[i].len`.
  - `i == -1` emits the final literal up to `offset` and a 0-token
    end-marker (`write_int(f, 0)` via `send_token(token=-1)` at
    `token.c:317-319`).
  - `i == -2` emits only literal data, no token.
- `match.c:107` `n = offset - last_match` is the literal length. The
  bounding invariant is `n <= block_size`, since the slide flushes early
  via the bored-of-literals branch (section 1.4).
- `match.c:117` `send_token(f, i, buf, last_match, n, s->sums[i].len)`
  is the single emission point.
- `match.c:120-123` updates `stats.matched_data` and bumps `n` by the
  block length for the checksum loop.
- `match.c:125-128` runs `sum_update()` over the entire range
  `[last_match, last_match + n)` in `CHUNK_SIZE`-sized strides. This
  feeds both literal bytes and matched-block bytes through the whole
  file checksum (`sender_file_sum`).
- `match.c:130-133` updates `last_match`. For a match it jumps past the
  matched block; for a literal-only flush it just absorbs the literal
  prefix.

oc-rsync analogues:

- `crates/matching/src/generator.rs:293-308` flushes pending literals
  on every match, mirroring the `n > 0` literal-prefix emission.
- `crates/matching/src/generator.rs:412-417` drains any remaining
  ring-buffer bytes at EOF into a final literal token, mirroring
  `match.c:343` (`matched(f, s, buf, len, -1)`).
- The literal+match unified emit lives at
  `crates/transfer/src/generator/delta.rs:225-274` and
  `:406-459`, which is where the wire-layer `write_token_stream` /
  `CompressedTokenEncoder` send the script.
- The whole-file checksum runs over both literals and re-read matched
  bytes at `crates/transfer/src/generator/delta.rs:289-334`. Upstream
  cites are at `match.c:370`, `:383-385`, and `matched():131-135`.

### 1.4 The bored-of-literals early flush

- `match.c:339-340` shipped the comment "By matching early we avoid
  re-reading the data 3 times". When `backup >= s->blength + CHUNK_SIZE`
  and there is still more than `CHUNK_SIZE` left to scan, upstream calls
  `matched(f, s, buf, offset - s->blength, -2)` to flush the accumulated
  literal as a "literal-only" emission. The `-2` token tells
  `send_token` to skip the trailing token integer.
- The rationale: a long unmatched stretch otherwise forces the source
  region to be read three times - once by the rolling-checksum slide,
  once by `sum_update()` in `matched()`, and once by the literal write
  path inside `send_token`. The early flush bounds the resident span.

oc-rsync analogue:

- `crates/matching/src/generator.rs:33-34` defines `CHUNK_SIZE: usize =
  32 * 1024` with the upstream cite.
- `crates/matching/src/generator.rs:218-225` flushes when
  `pending_literals.len() >= block_len + CHUNK_SIZE`, mirroring the
  `backup >= s->blength + CHUNK_SIZE` guard. The `total_bytes` and
  `literal_bytes` counters are bumped on the flush path.

### 1.5 match_sums() and the matches summary

- `match.c:362-437` `match_sums(f, s, buf, len)` is the public entry.
- `match.c:364-368` zeros `last_match`, `false_alarms`, `hash_hits`,
  `matches`, and `data_transfer` for the file.
- `match.c:370` `sum_init(xfer_sum_nni, checksum_seed)` initialises the
  whole-file checksum (xxh3 / md5 / md4 depending on negotiation).
- `match.c:372-391` is the `append_mode` shortcut. When appending, the
  receiver already has a verified prefix; the sender just runs
  `sum_update()` over the basis prefix (`append_mode == 2` rechecksums,
  `append_mode == 1` trusts it). `s->count = 0` then forces the
  no-blocks branch below.
- `match.c:393-402` is the normal path: `build_hash_table(s)` then
  `hash_search(f, s, buf, len)`.
- `match.c:403-409` is the whole-file path for `s->count == 0` (no
  basis or appended-only): walk the file in `CHUNK_SIZE` strides,
  emitting `matched(f, s, buf, j, -2)` literal flushes, then one final
  `matched(f, s, buf, len, -1)` end marker.
- `match.c:411` `sum_end(sender_file_sum)` finalises the whole-file
  hash.
- `match.c:416-422` corrupts the whole-file checksum on `buf->status
  != 0` (read error) so the receiver rejects the file.
- `match.c:426` `write_buf(f, sender_file_sum, xfer_sum_len)` emits the
  whole-file checksum after the end-marker token.
- `match.c:429-431` emits the per-file `false_alarms=N hash_hits=N
  matches=N` debug line.
- `match.c:433-436` accumulates into the per-session totals.
- `match.c:439-448` `match_report()` emits the aggregated totals at
  `--debug=DELTASUM,1` level.

oc-rsync analogues:

- `crates/matching/src/generator.rs:420-438` emits the same
  `false_alarms`, `hash_hits`, `matches`, and `delta:` summary lines
  via `debug_log!(Deltasum, ...)`.
- `crates/transfer/src/generator/delta.rs:225-264` is the whole-file
  literal-only path, using `write_token_end(writer)` at `:263` to
  terminate. Upstream cite is `match.c:404-408`.
- `append_mode` is wired at `crates/transfer/src/setup/restrictions.rs:39`
  and `:91` (`compat.c:653-654` adjustment); the
  rechecksum loop has no direct analogue yet (`crates/matching/` does
  not branch on append). Receiver-side prefix handling lives in
  `crates/transfer/src/receiver/transfer.rs`. #1579 owns wiring the
  sender-side `append_mode == 2` rechecksum.

## 2. token.c structure

`token.c` is the wire layer for delta streams. It is much larger than
`match.c` (1131 lines in 3.4.2) because it carries the compression
codecs.

### 2.1 send_token() and recv_token() dispatch

- `token.c:1053-1077` `send_token(f, token, buf, offset, n, toklen)`
  switches on `do_compression`:
  - `CPRES_NONE` -> `simple_send_token()` (`token.c:306-320`).
  - `CPRES_ZLIB` / `CPRES_ZLIBX` -> `send_deflated_token()`
    (`token.c:358-486`).
  - `CPRES_ZSTD` -> `send_zstd_token()` (`token.c:684-784`).
  - `CPRES_LZ4` -> `send_compressed_token()` (`token.c:895-967`).
- `token.c:1085-1104` `recv_token(f, data)` is the receiver-side dual:
  - `CPRES_NONE` -> `simple_recv_token()` (`token.c:282-303`).
  - `CPRES_ZLIB` / `CPRES_ZLIBX` -> `recv_deflated_token()`
    (`token.c:501-631`).
  - `CPRES_ZSTD` -> `recv_zstd_token()` (`token.c:788-892`).
  - `CPRES_LZ4` -> `recv_compressed_token()` (`token.c:969-1045`).
- `token.c:1109-1131` `see_token(data, toklen)` is the receiver-side
  dictionary-sync hook used to feed matched-block bytes into the
  decompressor's history. Only `CPRES_ZLIB` actually feeds the
  dictionary; `CPRES_ZLIBX` deliberately skips it (the X stands for
  "no transfer dictionary").

oc-rsync analogues:

- The Strategy-pattern dispatch lives at
  `crates/transfer/src/token_reader.rs:80-110` (`TokenReader::new`)
  and `:130-151` (`TokenReader::read_token`).
- The encoder side dispatches through
  `crates/protocol/src/wire/compressed_token/encoder.rs:120-156`
  (`CompressedTokenEncoder::send_literal`, `send_block_match`,
  `finish`, `see_token`, `reset`).
- The simple (uncompressed) path lives at
  `crates/protocol/src/wire/delta/token.rs:42-216`. The
  `CompressedTokenEncoder` is constructed in
  `crates/transfer/src/generator/delta.rs:39-56` and selected per
  negotiated `CompressionAlgorithm`.
- The receiver-side `see_token` hook is at
  `crates/transfer/src/token_reader.rs:168-173` and used after each
  `BlockRef` in the transfer loop at
  `crates/transfer/src/transfer_ops/token_loop.rs:181`.

### 2.2 simple_send_token: the literal/match tag scheme

- `token.c:306-320` `simple_send_token(f, token, buf, offset, n)`:
  - When `n > 0`, walks the literal in `CHUNK_SIZE` strides (32 KiB),
    emitting `write_int(f, n1)` followed by `write_buf(f, ..., n1)`.
    This is the positive-integer literal frame.
  - When `token != -2`, emits `write_int(f, -(token+1))` for the match
    token: `token == 0` becomes `-1`, `token == 1` becomes `-2`, etc.
  - When `token == -1` (end-of-file sentinel from `matched()` at
    `match.c:343`), `-(token+1) == 0`; the lone `write_int(f, 0)` is
    the end-of-stream marker.
  - When `token == -2` (bored-of-literals flush from `match.c:339-340`),
    no integer is emitted - only the literal chunk(s).
- `simple_recv_token` (`token.c:282-303`) is the receiver dual: read
  one `int`; if `<= 0` return it (caller interprets `0` as EOF and
  negative values as `-(token+1)` block references); if `> 0` interpret
  as residue length and stream up to `CHUNK_SIZE` bytes at a time via
  `read_buf`.

This is the "tag scheme":

| Wire value | Meaning |
|---|---|
| `> 0` | Literal byte count, that many raw bytes follow. |
| `0` | End-of-stream sentinel. |
| `< 0` | Block reference, basis block index is `-(value + 1)`. |

oc-rsync analogues:

- `crates/protocol/src/wire/delta/token.rs:42-52` `write_token_literal`
  matches `simple_send_token`'s positive-integer literal frame
  including the `CHUNK_SIZE` chunking at `:46`.
- `crates/protocol/src/wire/delta/token.rs:73-77`
  `write_token_block_match` emits `-(block_index + 1)`, matching
  upstream's `-(token+1)` encoding exactly.
- `crates/protocol/src/wire/delta/token.rs:94-97` `write_token_end`
  writes `write_int(0)`, the end sentinel.
- `crates/protocol/src/wire/delta/token.rs:208-215` `read_token` parses
  the same tag scheme. The receiver-side `TokenReader::Plain` arm at
  `crates/transfer/src/token_reader.rs:132-143` implements the
  three-way ordering on the signed int and yields the typed
  `DeltaToken::{End, Literal(Pending(n)), BlockRef(idx)}` variants.

### 2.3 send_deflated_token: compression interleaving

When `-z` (zlib) is in effect, the literal payload is compressed and
the token framing changes to a single-byte flag stream with the bit
patterns at `token.c:321-329`:

```c
#define END_FLAG        0           /* that's all folks */
#define TOKEN_LONG      0x20        /* followed by 32-bit token number */
#define TOKENRUN_LONG   0x21        /* ditto with 16-bit run count */
#define DEFLATED_DATA   0x40        /* + 6-bit high len, then low len byte */
#define TOKEN_REL       0x80        /* + 6-bit relative token number */
#define TOKENRUN_REL    0xc0        /* ditto with 16-bit run count */
```

`MAX_DATA_COUNT = 16383` (`token.c:330`) is the 14-bit literal-payload
length cap.

- `token.c:358-486` `send_deflated_token`:
  - `token.c:364-382` on `last_token == -1`, initialises the
    `z_stream` via `deflateInit2()` with `windowBits = -15` (raw
    deflate, no zlib header) and `Z_DEFAULT_STRATEGY`. The `obuf` is
    sized to `MAX(MAX_DATA_COUNT+2, AVAIL_OUT_SIZE(CHUNK_SIZE))` from
    the `#if` ladder at `token.c:351-355`.
  - `token.c:383-401` token-run encoding. The encoder accumulates
    consecutive matches into a "run" started at `run_start`. A run is
    flushed when:
    - A literal arrives (`nb != 0`).
    - A non-adjacent token arrives (`token != last_token + 1`).
    - The run length would exceed 65 535 (`token >= run_start + 65536`).
    On flush, upstream writes one of four tag variants based on the
    relative offset `r = run_start - last_run_end` and run length
    `n = last_token - run_start`:
    - `r in [0, 63]` and `n == 0`: `TOKEN_REL | r` (single byte).
    - `r in [0, 63]` and `n > 0`: `TOKENRUN_REL | r` + 2-byte run.
    - `r > 63` and `n == 0`: `TOKEN_LONG` + 4-byte absolute token.
    - `r > 63` and `n > 0`: `TOKENRUN_LONG` + 4-byte absolute + 2-byte
      run.
    This is the rationale for the `want_i` hint in `match.c` (section
    1.2.3): adjacent matches compress into a single `TOKEN_REL`
    or short `TOKENRUN_REL` byte rather than a 5- or 7-byte
    long form.
  - `token.c:405-459` is the deflate loop. The loop pulls input in
    `CHUNK_SIZE` strides, feeds it to `deflate()` with `Z_NO_FLUSH`,
    and writes `DEFLATED_DATA | (n >> 8)` + low byte + payload bytes
    when `obuf` fills. End-of-file flushes with `Z_SYNC_FLUSH`. The
    last 4 bytes (the `0, 0, ff, ff` sync trailer) are kept in `obuf`
    and moved to the front of the next output to avoid duplication.
  - `token.c:461-463` writes `END_FLAG` at EOF (`token == -1`).
  - `token.c:464-485` is the dictionary-sync block. After every match
    when `do_compression == CPRES_ZLIB`, the matched basis-block bytes
    are fed back into the encoder via `deflate(tx_strm, Z_INSERT_ONLY)`
    so the receiver's inflate history stays in sync. `CPRES_ZLIBX`
    intentionally skips this step.
  - `token.c:474-475` advances the `offset` only when
    `protocol_version >= 31`, working around the
    "data-duplicating bug" in older protocols where the same range was
    inserted twice.
- `token.c:501-631` `recv_deflated_token` is the receiver-side state
  machine over `recv_state` (`r_init`, `r_idle`, `r_running`,
  `r_inflating`, `r_inflated`). It reads one flag byte, dispatches on
  the top two bits:
  - `0xC0` mask `DEFLATED_DATA`: inflate the next payload.
  - `END_FLAG`: return 0 and reset to `r_init`.
  - `TOKEN_REL`: relative offset in low 6 bits, then optional 2-byte
    run count if `flag & 1`.
  - Anything else: absolute 4-byte token follows.

oc-rsync analogues:

- The wire constants are mirrored at
  `crates/protocol/src/wire/compressed_token/mod.rs:56-102`.
- The zlib encoder run logic lives at
  `crates/protocol/src/wire/compressed_token/zlib_codec.rs:88-105`
  (the run accumulator) and `:225-242` (the four-variant flush). The
  decode mirror is in the same file at `:398-413`.
- `CompressedTokenEncoder::see_token` is wired from
  `crates/transfer/src/generator/delta.rs:444-454` after each
  `DeltaOp::Copy`, mirroring `token.c:463-485`.
- The receiver-side see-token hook is at
  `crates/transfer/src/transfer_ops/token_loop.rs:181`, cited as
  `token.c:631` (`see_deflate_token`).

### 2.4 The OP_END boundary

The "OP_END" boundary, as the task brief calls it, is the
end-of-stream marker. There is no `OP_END` symbol in the source; the
two encodings are:

- **Plain (no compression):** the literal/match tag scheme writes a
  bare `write_int(0)` as the terminator. `simple_send_token`
  derives it from `token == -1` (`token.c:317-319`):
  `write_int(f, -((-1)+1)) == write_int(f, 0)`.
- **Compressed (zlib / zstd / lz4):** the flag-byte stream writes the
  one-byte `END_FLAG (0x00)` (`token.c:461-463`, `token.c:780-783`,
  `token.c:963-966`). The receiver's `r_idle` state interprets it,
  resets `recv_state = r_init`, and returns `0`
  (`token.c:583-586`, `:833-836`, `:1001-1004`).

In both formats, after the end sentinel the sender writes the
whole-file checksum (`match.c:426`, `write_buf(f, sender_file_sum,
xfer_sum_len)`).

oc-rsync analogues:

- Plain: `crates/protocol/src/wire/delta/token.rs:94-97`
  (`write_token_end` -> `write_int(0)`).
- Compressed: `crates/protocol/src/wire/compressed_token/mod.rs:58`
  (`END_FLAG: u8 = 0x00`) and emitted by each codec's `finish` method
  (`zlib_codec.rs:106`, the zstd and lz4 codecs sit alongside it).

## 3. Mapping table: upstream -> oc-rsync

| Upstream function (`file:lines`) | oc-rsync analogue (`crate/file:lines`) | Notes |
|---|---|---|
| `match.c:55-88` `build_hash_table()` | `crates/matching/src/index/builder.rs:17-105` `populate_index` + `from_signature_with_role` | We do not auto-resize for 80% load; #2072 tracks the packed-key compaction. |
| `match.c:105-137` `matched()` | `crates/matching/src/generator.rs:293-399` literal-flush + match-emit path | The whole-file `sum_update()` happens in `crates/transfer/src/generator/delta.rs:289-334`. |
| `match.c:140-345` `hash_search()` | `crates/matching/src/generator.rs:141-441` `DeltaGenerator::generate` | Includes tag-table + bithash prefilter for the (sum1, sum2) early reject. |
| `match.c:155, 289-301` `want_i` adjacency hint | `crates/matching/src/generator.rs:163, 251-264, 337-341, 375-392` + `crates/matching/src/index/mod.rs:240-263` `check_block_match_slices` | Hint deliberately bypasses the matched-block bitmap; see `docs/design/zsync-prune.md`. |
| `match.c:191-202` bucket lookup | `crates/matching/src/index/compact_lookup.rs` `CompactLookup::find_all` | Open-addressing Robin Hood probe over an 8-byte slot. |
| `match.c:218-240` chain reject + strong verify | `crates/matching/src/index/mod.rs:146-169` `find_match_bytes_filtered` | Tag table -> bithash -> strong-checksum order. |
| `match.c:303-310` post-match window jump | `crates/matching/src/generator.rs:343-373` ring clear + bulk refill + SIMD `update` | Uses two-slice `as_slices()` view to avoid rotation. |
| `match.c:314-340` rolling slide + early flush | `crates/matching/src/generator.rs:208-228` + `:218-225` | `CHUNK_SIZE = 32 KiB` matches `rsync.h:158`. |
| `match.c:362-437` `match_sums()` | `crates/matching/src/generator.rs:141-441` `DeltaGenerator::generate` + `crates/transfer/src/generator/delta.rs:195-274` whole-file path | `append_mode == 2` rechecksum loop has no direct sender-side port yet (#1579). |
| `match.c:439-448` `match_report()` | `crates/matching/src/generator.rs:429-438` `debug_log!(Deltasum, 1, ...)` | Per-session totals not yet aggregated; emitted per-file. |
| `token.c:282-303` `simple_recv_token()` | `crates/transfer/src/token_reader.rs:130-151` `TokenReader::Plain` arm | |
| `token.c:306-320` `simple_send_token()` | `crates/protocol/src/wire/delta/token.rs:42-216` | Includes 32 KiB literal chunking. |
| `token.c:358-486` `send_deflated_token()` | `crates/protocol/src/wire/compressed_token/zlib_codec.rs:60-242` | Run accumulator at `:88-105`, four-variant flush at `:225-242`. |
| `token.c:464-485` zlib dictionary sync | `crates/transfer/src/generator/delta.rs:444-454` `encoder.see_token(&see_buf)?` | Source bytes re-read from disk to feed the encoder dictionary. |
| `token.c:501-631` `recv_deflated_token()` | `crates/protocol/src/wire/compressed_token/zlib_codec.rs:330-500` (decoder half) + `crates/transfer/src/token_reader.rs:145-150` | State machine over flag bytes; `END_FLAG` -> `DeltaToken::End`. |
| `token.c:637-676` `see_deflate_token()` | `crates/transfer/src/token_reader.rs:168-173` `TokenReader::see_token` + `crates/transfer/src/transfer_ops/token_loop.rs:181` | Receiver-side dictionary feed. |
| `token.c:684-784` `send_zstd_token()` | `crates/protocol/src/wire/compressed_token/zstd_codec.rs` (feature `zstd`) | Same flag-byte framing as zlib. |
| `token.c:788-892` `recv_zstd_token()` | `crates/protocol/src/wire/compressed_token/zstd_codec.rs` (feature `zstd`) | |
| `token.c:895-967` `send_compressed_token()` (LZ4) | `crates/protocol/src/wire/compressed_token/lz4_codec.rs` (feature `lz4`) | |
| `token.c:969-1045` `recv_compressed_token()` (LZ4) | `crates/protocol/src/wire/compressed_token/lz4_codec.rs` (feature `lz4`) | |
| `token.c:1053-1077` `send_token()` dispatch | `crates/protocol/src/wire/compressed_token/encoder.rs:120-156` | Strategy-pattern enum dispatch on negotiated `CompressionAlgorithm`. |
| `token.c:1085-1104` `recv_token()` dispatch | `crates/transfer/src/token_reader.rs:80-110` `TokenReader::new` + `:130-151` `read_token` | |
| `token.c:1109-1131` `see_token()` dispatch | `crates/transfer/src/token_reader.rs:168-173` | |

## 4. Wire-format invariants

These invariants must be preserved by every change to oc-rsync's
delta pipeline. Each cites the upstream source so deviations are
caught.

### 4.1 Plain delta stream (`CPRES_NONE`)

- Tokens are 4-byte little-endian `int32`. `write_int` is at
  `io.c:write_int()` (cited at
  `crates/protocol/src/wire/delta/mod.rs:20`).
- `value > 0`: literal of `value` bytes follows verbatim
  (`token.c:307-314`). Literals longer than `CHUNK_SIZE = 32768`
  (`rsync.h:158`) are split into multiple positive-int + payload
  pieces.
- `value == 0`: end-of-stream sentinel (`token.c:317-319` with
  `token == -1`; `match.c:343`).
- `value < 0`: block match at basis index `-(value + 1)`
  (`token.c:319`). The block length is implicit from the signature
  header; no length is encoded per-match.
- Whole-file transfers still terminate with `write_int(0)`
  (`match.c:407-408`).
- After the end sentinel, the sender writes `xfer_sum_len` bytes of
  whole-file checksum (`match.c:426`).

### 4.2 Compressed delta stream (`CPRES_ZLIB`, `ZLIBX`, `ZSTD`, `LZ4`)

- One flag byte per record (`token.c:321-329`):
  - `0x00` `END_FLAG`: end of stream.
  - `0x20` `TOKEN_LONG`: 4-byte absolute basis-block index follows
    (little-endian).
  - `0x21` `TOKENRUN_LONG`: 4-byte absolute index + 2-byte run count.
  - `0x40 | (n >> 8)` `DEFLATED_DATA`: 14-bit literal length where
    the next byte holds the low 8 bits. Compressed payload follows.
  - `0x80 | r` `TOKEN_REL`: relative offset `r in [0, 63]` from the
    last token.
  - `0xC0 | r` `TOKENRUN_REL`: relative offset + 2-byte run count.
- Literal payloads cap at `MAX_DATA_COUNT = 16383`
  (`token.c:330`). Larger literals split into multiple
  `DEFLATED_DATA` records.
- The deflate stream is raw (no zlib header), `windowBits = -15`,
  `Z_DEFAULT_STRATEGY` (`token.c:370-375`). `Z_SYNC_FLUSH` is used at
  record boundaries; the 4-byte `0, 0, ff, ff` sync trailer is held in
  the encoder's output buffer until the next record (`token.c:421-432`).
- For `CPRES_ZLIB` (not `CPRES_ZLIBX`), matched basis-block bytes are
  fed to the encoder dictionary via `Z_INSERT_ONLY` after each match
  (`token.c:464-485`). The receiver runs the symmetric
  `see_deflate_token()` (`token.c:637-676`).
- Whole-file checksum still follows the `END_FLAG` byte
  (`match.c:426`).

### 4.3 Common invariants

- `CHAR_OFFSET = 0` for protocol >= 27 (`rsync.h`). oc-rsync targets
  protocol 32 so the rolling hash never adds an offset
  (`crates/checksums/src/rolling/`).
- The strong checksum truncation length `s2length` is set by the
  generator based on phase (2 in phase 1, up to `MAX_SUM_LENGTH = 16`
  in phase 2 redo). See `signature/block_size.rs` for the
  `SHORT_SUM_LENGTH` constant in oc-rsync.
- `last_block_len` (the trailing partial block) participates in
  matching but only at the file end; `match.c:174` derives `end = len + 1
  - s->sums[s->count-1].len` so the slide does not run off the buffer.
- Match tokens carry an implicit length: the block length is
  `s->sums[i].len`, known to both sides from the signature header.
  This is why oc-rsync's `DeltaOp::Copy { block_index, length }`
  always sets `length` to the canonical block length when round-tripping
  to wire (`crates/transfer/src/generator/delta.rs:355-379`).

## 5. Edge cases worth porting

### 5.1 3.4.2-only `recv_token` hardening

The only behavioural divergence between 3.4.1 and 3.4.2 in these
files is a clutch of input-validation guards in the receiver. Each
checks that an absolute token index read from the wire is non-negative,
exiting via `RERR_PROTOCOL` on a malformed peer. These appear in all
three compressed receivers:

- `token.c:594-598` `recv_deflated_token`: guard after
  `rx_token = read_int(f)`.
- `token.c:843-847` `recv_zstd_token`: same guard.
- `token.c:1013-1017` `recv_compressed_token` (LZ4): same guard.

These are pure receiver-side defences against a hostile sender;
they do not change the wire format. oc-rsync should mirror them in
`crates/protocol/src/wire/compressed_token/{zlib,zstd,lz4}_codec.rs`
when porting the receiver, returning
`io::ErrorKind::InvalidData`. The current oc-rsync decoder
(`crates/protocol/src/wire/compressed_token/zlib_codec.rs:413`)
already rejects on `flag & 0xE0 == TOKEN_LONG` malformed sequences,
but should add a `rx_token < 0` check after the `read_int`.

### 5.2 zstd flush loop fix in 3.4.2

`token.c:737-779` in 3.4.2 replaces the 3.4.1 zstd flush loop with a
single-buffer-per-iteration shape:

- 3.4.1 conditionally reset `zstd_out_buff.size` only when zero, then
  emitted a record whenever the buffer filled or a flush was
  requested. This could send half-empty records on partial output.
- 3.4.2 always resets `zstd_out_buff.size = MAX_DATA_COUNT` before each
  `ZSTD_compressStream2` call, and uses an explicit `finished` flag
  derived from `flush == ZSTD_e_flush ? (r == 0) :
  (zstd_in_buff.pos == zstd_in_buff.size)` to terminate the loop.

oc-rsync's zstd codec
(`crates/protocol/src/wire/compressed_token/zstd_codec.rs`) should
match the 3.4.2 shape. Any port targeting 3.4.1 must still emit
records that decompress under the 3.4.1 receiver, which means the
3.4.2 loop is the safer target.

### 5.3 zstd `nbWorkers` parameter in 3.4.2

`token.c:701` adds
`ZSTD_CCtx_setParameter(zstd_cctx, ZSTD_c_nbWorkers,
do_compression_threads)`. This is a build-side knob (the
`do_compression_threads` global is wired through the CLI parser); it
does not change the wire format. oc-rsync may add this when wiring
multi-threaded zstd compression but should keep the default to
single-threaded for byte-for-byte interop with 3.4.1.

### 5.4 Zero-length match handling

- `match.c:107` uses `int32 n = (int32)(offset - last_match)` with the
  invariant `n <= block_size`. `n == 0` is legal and corresponds to a
  match that immediately follows the last emission; in this case
  `simple_send_token` skips the literal frame entirely (the `n > 0`
  guard at `token.c:308`).
- For `i == -1` (end sentinel), `n` can still be zero when the file
  ends exactly on the last match. The end-marker `write_int(0)` is
  always emitted regardless.

oc-rsync's `crates/matching/src/generator.rs:293-308` handles this
correctly: the `if !pending_literals.is_empty()` guard skips the
literal flush when no bytes are pending, and the EOF drain at
`:412-417` only pushes when there are leftover ring-buffer bytes.

### 5.5 End-of-file alignment

- `match.c:174` `end = len + 1 - s->sums[s->count-1].len` derives the
  loop bound so the slide cannot read past the basis-file image. The
  `+1` compensates for the loop's `++offset` at the bottom of the
  do/while at `match.c:341`.
- `match.c:343-344` emits the trailing `matched(f, s, buf, len, -1)`
  end-marker plus a `map_ptr(buf, len-1, 1)` reference to ensure the
  final byte is faulted in (relevant for the memory-map backend).
- `match.c:222-224` rejects mismatches on `l != s->sums[i].len` when
  the candidate is a short trailing block - the rolling sum may
  match, but the partial block has a different canonical length.

oc-rsync's `crates/matching/src/index/mod.rs:142-144` and `:203-205`
enforce the same length contract by rejecting when `window.len() !=
self.block_length`. The fuzzy-match path
(`crates/matching/src/fuzzy/`) handles short trailing blocks via
fuzzy scoring rather than exact match, since the basis file may
present a differently-sized trailer.

### 5.6 Read-error checksum poisoning

- `match.c:416-422` mutates `sender_file_sum` when `buf->status != 0`:
  all bits are set, and if that happens to equal a valid checksum,
  the last 0 bit is flipped to 1. This guarantees the receiver
  rejects the file as corrupt without requiring an out-of-band
  error frame.

This pattern has no direct oc-rsync port. The current sender path
returns an `io::Error` instead, which propagates up through the
transfer state machine. #1579 should decide whether to mirror the
poison-checksum trick for interop edge cases where the legacy
receiver expects a checksum frame even on read failure.

### 5.7 In-place mode chunk re-alignment

- `match.c:246-287` handles `updating_basis_file` (in-place mode).
  After a match, upstream walks `aligned_offset` forward in
  `blength`-sized strides and prefers a match at the boundary to
  enable the seek-back trick that prevents the sender from
  overwriting bytes it has not yet read.
- `match.c:262-282` handles the zero-run re-alignment: when a
  zero-rolling-sum match lands off-boundary, the sender backs up to
  the alignment point and verifies the match there to avoid emitting
  literal data covering already-correct zeros.
- `match.c:284` sets the `SUMFLG_SAME_OFFSET` flag so a later probe
  knows this index has been used at its native position.

oc-rsync currently has no in-place sender mode. The matching crate
does not branch on `updating_basis_file`, and `SUMFLG_SAME_OFFSET`
has no analogue. #1579 owns scoping whether in-place support is in
the first delta cut or follows in a later PR. When that work lands,
the touch points are `crates/matching/src/index/mod.rs` (the bitmap
filter would need a `same_offset` variant) and
`crates/matching/src/generator.rs` (the post-match window jump
needs the alignment fixup).

## 6. References

Upstream source tree: `target/interop/upstream-src/rsync-3.4.2/`.

- `match.c` (448 lines).
- `token.c` (1131 lines).
- `rsync.h` for `CHUNK_SIZE`, `CHAR_OFFSET`, `MAX_DIGEST_LEN`.
- `io.c:write_int()` / `read_int()` for the 4-byte LE integer
  primitives.
- `checksum.c:sum_init()` / `sum_update()` / `sum_end()` for the
  whole-file checksum lifecycle invoked from `match.c`.

oc-rsync companion docs:

- `docs/design/zsync-inspired-matching.md` for the wire-compat
  invariants the matching crate enforces.
- `docs/design/zsync-prune.md` for the matched-block bitmap (the
  duplicate-block correctness contract that interacts with the
  upstream chain prune at `match.c:211-215`).
- `docs/design/zsync-seq-match.md` for the seq-match coalescing the
  generator emits (a strict superset of upstream's `want_i` hint).
- `docs/design/zsync-bithash.md` for the bithash prefilter sitting
  between the tag table and the strong-checksum verify.
