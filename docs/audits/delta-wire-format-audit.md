# Delta Wire-Format Conformance Audit

This audit reviews the delta token stream produced by `oc-rsync` against the
upstream rsync 3.4.1 reference implementation. The goal is byte-for-byte parity
with `match.c` (block matching, `matched()`, `hash_search()`) and `token.c`
(token framing, `simple_send_token`, `send_deflated_token`, `send_zstd_token`,
`send_compressed_token` for LZ4) for every supported protocol version (28-32).

Upstream source under audit: `target/interop/upstream-src/rsync-3.4.1/`.

Our source under audit:

- `crates/protocol/src/wire/delta/` (uncompressed token framing)
- `crates/protocol/src/wire/compressed_token/` (zlib/zstd/lz4 framing)
- `crates/match/src/` (block matching, delta script generation)
- `crates/engine/src/delta/` (re-export facade for the executor)
- `crates/transfer/src/receiver/wire.rs` (`SumHead`, signature blocks)
- `crates/signature/src/block_size.rs` (`SHORT_SUM_LENGTH`, `MAX_SUM_LENGTH`,
  block-size heuristic)

## 1. Token stream schema by protocol version

The delta token stream is produced by the sender after it has received the
`sum_head` and signature blocks from the generator. The schema is identical
across protocol versions for the **token framing** (uncompressed and
compressed) - only adjacent encodings (sum_head, signature blocks, file
checksum trailer) shift between versions. Upstream `match.c` and `token.c`
are version-agnostic within the supported range.

| Aspect                              | v28          | v29          | v30          | v31          | v32          |
|-------------------------------------|--------------|--------------|--------------|--------------|--------------|
| Uncompressed token format           | int32 LE     | int32 LE     | int32 LE     | int32 LE     | int32 LE     |
| Compressed token format             | flag-byte    | flag-byte    | flag-byte    | flag-byte    | flag-byte    |
| `sum_head.s2length` field present   | yes (>= 27)  | yes          | yes          | yes          | yes          |
| Block-size cap                      | 2^29         | 2^29         | 2^17         | 2^17         | 2^17         |
| Negotiable strong checksum          | MD4 only     | MD4 only     | MD5 default  | MD5/MD4/xxh* | MD5/MD4/xxh* |
| zlib `see_token` offset advance     | bug-compat   | bug-compat   | bug-compat   | fixed (v31+) | fixed        |
| LZ4 / ZSTD compression negotiable   | no           | no           | no           | yes          | yes          |
| INC_RECURSE flist (delta-orthogonal)| no           | no           | yes          | yes          | yes          |

The two version-conditional points inside `token.c` itself are:

- `token.c:473` and `token.c:656`: when feeding the basis-block payload back
  through the deflate/inflate engine, **only** advance the input cursor when
  `protocol_version >= 31`. Earlier versions deliberately re-feed the same
  bytes for a `toklen > 0xFFFF` block to remain bug-compatible. We mirror this
  in `crates/protocol/src/wire/compressed_token/zlib_codec.rs:131-133` and the
  matching `see_token` flow in the decoder.

The sum_head differences at version 27 and below (no `s2length` field) are
out of scope for this audit because oc-rsync's minimum negotiated version is
28; v28 handshake tests in `golden_protocol_v28_handshake.rs` enforce that
floor.

## 2. Token types

Upstream `token.c` exposes three logical token types on the wire, and one
out-of-band data envelope for compressed mode:

| Token type             | Uncompressed encoding (no `-z`)                 | Compressed encoding (`-z` / `--compress`)                                                              |
|------------------------|--------------------------------------------------|--------------------------------------------------------------------------------------------------------|
| LITERAL                | `write_int(n)` then `n` bytes (chunked at 32K)   | One or more `DEFLATED_DATA` frames carrying compressed payload, emitted before the next token         |
| COPY (block reference) | `write_int(-(token+1))`                          | Flag byte: `TOKEN_REL` (0x80 + rel) or `TOKEN_LONG` (0x20) with int32 LE absolute, optional run count  |
| END marker             | `write_int(0)` (i.e. `send_token(token=-1)`)     | Single `END_FLAG` byte (0x00)                                                                          |
| Compressed flush       | n/a                                              | Implicit: `Z_SYNC_FLUSH` / `ZSTD_e_flush` / per-call LZ4 boundary at every token boundary              |

Upstream cross-references:

- LITERAL framing: `token.c:simple_send_token()` lines 305-319, 308-313
  (chunking).
- COPY (uncompressed): `token.c:simple_send_token()` line 318
  (`write_int(f, -(token+1))`).
- END uncompressed: same path with `token == -1` -> wire value
  `-((-1)+1) = 0`. See `match.c:343` (`matched(f, s, buf, len, -1)`).
- COPY/END compressed: `token.c:321-329` flag byte definitions, plus the
  `TOKEN_LONG` / `TOKENRUN_LONG` / `TOKEN_REL` / `TOKENRUN_REL` flag bytes
  defined inline.

oc-rsync mirroring:

- LITERAL: `crates/protocol/src/wire/delta/token.rs:42-52`
  (`write_token_literal` chunks at `CHUNK_SIZE = 32 * 1024`,
  `crates/protocol/src/wire/delta/types.rs:4`).
- COPY: `crates/protocol/src/wire/delta/token.rs:74-77`
  (`write_token_block_match`).
- END: `crates/protocol/src/wire/delta/token.rs:95-97`
  (`write_token_end` writes int32 LE `0`).
- Compressed flag bytes: `crates/protocol/src/wire/compressed_token/mod.rs:58-90`
  (`END_FLAG`, `TOKEN_LONG`, `TOKENRUN_LONG`, `DEFLATED_DATA`, `TOKEN_REL`,
  `TOKENRUN_REL`).

## 3. Wire encoding per token type

### 3.1 Uncompressed path (`do_compression == CPRES_NONE`)

```text
LITERAL (n > 0)
  +------------+----------------+
  | int32 LE n | n raw bytes    |
  +------------+----------------+
  Repeated for each 32 KiB chunk if total > CHUNK_SIZE.

COPY (block_index = b)
  +-----------------------------+
  | int32 LE -(b+1)             |
  +-----------------------------+

END (token == -1 in upstream send_token)
  +-----------------------------+
  | int32 LE 0                  |
  +-----------------------------+
```

Upstream: `token.c:307-318`. oc-rsync: `crates/protocol/src/wire/delta/int_encoding.rs:33-35`
(little-endian via `i32::to_le_bytes`), then chunked literal emission in
`token.rs:42-52`.

### 3.2 Compressed path (zlib / zstd / lz4)

Each token boundary produces zero or more `DEFLATED_DATA` frames (the
compressed-literal envelope) followed by exactly one token byte (or run
header), or - at end of file - one `END_FLAG` byte. Upstream's `obuf` layout
is shared across the three algorithms; only the codec inside the envelope
differs.

```text
DEFLATED_DATA (compressed literal envelope, len <= 16383)
  +------------------------+----------------+--------------------+
  | 0x40 | (len >> 8)      | len & 0xFF     | len bytes payload  |
  +------------------------+----------------+--------------------+

TOKEN_REL (relative block ref, 0 <= rel <= 63, run length zero)
  +------------------------+
  | 0x80 | rel             |
  +------------------------+

TOKENRUN_REL (relative block ref + run count)
  +------------------------+----------------+----------------+
  | 0xC0 | rel             | run_lo         | run_hi         |
  +------------------------+----------------+----------------+

TOKEN_LONG (absolute 32-bit block ref, run length zero)
  +------+--------------------------------------+
  | 0x20 | int32 LE absolute_token              |
  +------+--------------------------------------+

TOKENRUN_LONG (absolute 32-bit block ref + run count)
  +------+--------------------------------------+----------------+----------------+
  | 0x21 | int32 LE absolute_token              | run_lo         | run_hi         |
  +------+--------------------------------------+----------------+----------------+

END_FLAG
  +------+
  | 0x00 |
  +------+
```

Mask layout (upstream `token.c:321-327`, mirrored
`crates/protocol/src/wire/compressed_token/mod.rs:58-90`):

| Constant         | Value | Purpose                                                |
|------------------|-------|--------------------------------------------------------|
| `END_FLAG`       | 0x00  | end of file                                            |
| `TOKEN_LONG`     | 0x20  | 32-bit absolute token follows                          |
| `TOKENRUN_LONG`  | 0x21  | 32-bit absolute token + 16-bit run count               |
| `DEFLATED_DATA`  | 0x40  | low 6 bits = len high byte (low byte read separately)  |
| `TOKEN_REL`      | 0x80  | low 6 bits = relative offset                           |
| `TOKENRUN_REL`   | 0xC0  | low 6 bits = relative offset, 16-bit run count follows |

`MAX_DATA_COUNT = 16383` (upstream `token.c:329`, ours
`compressed_token/mod.rs:96`). Upstream's `OBUF_SIZE` is `MAX_DATA_COUNT + 2`
to hold the envelope header (`token.c:350-354`). Run counts are 16-bit
little-endian (upstream lines 396-397, 595-596; ours line 238-239 in zlib
encoder, decoder line 408-409).

Run encoding heuristic at the encoder (upstream `token.c:384-400` for zlib,
mirrored exactly in our zlib (224-243), zstd (243-263) and lz4 (211-231)
encoders):

```text
r = run_start - last_run_end   // gap from prior run end
n = last_token  - run_start    // tokens accumulated in this run
if 0 <= r <= 63:
    flag = (n == 0 ? TOKEN_REL : TOKENRUN_REL) + r
else:
    flag = (n == 0 ? TOKEN_LONG : TOKENRUN_LONG)
    write int32 LE run_start
if n != 0:
    write byte n & 0xFF
    write byte (n >> 8) & 0xFF
```

The break condition (`nb != 0 || token != last_token + 1
|| token >= run_start + 65536`, upstream line 384) is replicated at
`zlib_codec.rs:91`, `zstd_codec.rs:126`, and `lz4_codec.rs:122`.

## 4. Block-size negotiation (`block_size`, `s2length`)

The generator computes `block_size` and `s2length` per file via the
square-root heuristic at `generator.c:sum_sizes_sqroot()` (line 690), then
serialises a four-int sum_head:

```text
sum_head wire layout (upstream io.c:write_sum_head, line 1997-2009)
  +---------------------------------------------------------------+
  | int32 LE count       (number of blocks)                       |
  +---------------------------------------------------------------+
  | int32 LE blength     (block length in bytes)                  |
  +---------------------------------------------------------------+
  | int32 LE s2length    (only if protocol_version >= 27)         |
  +---------------------------------------------------------------+
  | int32 LE remainder   (size of final partial block, 0 if none) |
  +---------------------------------------------------------------+
```

The four signed 32-bit fields are written in network byte order? No, they
are little-endian (upstream `io.c:write_int()`). `read_sum_head()` validates:

- `count >= 0` (line 1969).
- `0 <= blength <= max_blength` where `max_blength` is `OLD_MAX_BLOCK_SIZE`
  (`1<<29`) for `protocol_version < 30`, else `MAX_BLOCK_SIZE` (`1<<17`),
  upstream `io.c:1967`.
- `0 <= s2length <= xfer_sum_len` (line 1981).
- `0 <= remainder <= blength` (line 1987).

oc-rsync conformance:

- `SumHead` struct: `crates/transfer/src/receiver/wire.rs:33-42` carries
  exactly the four fields.
- Wire layout: `wire.rs:111-117` writes `count`, `blength`, `s2length`,
  `remainder` as 4-byte LE integers in that order, unconditionally for
  protocol >= 28 (we never negotiate below 28, so the v27 omission of
  `s2length` is irrelevant).
- Block-length cap: `crates/signature/src/block_size.rs:140-146` clamps to
  `MAX_BLOCK_SIZE_OLD = 1<<29` for protocol < 30 and `MAX_BLOCK_SIZE_V30 =
  1<<17` for >= 30, matching upstream's split at version 30.
- Square-root heuristic: `block_size.rs:291-331` mirrors
  `generator.c:sum_sizes_sqroot()` lines 690-740, including the multiple-of-8
  rounding and `DEFAULT_BLOCK_SIZE = 700` floor for files <= 700^2 bytes.

After the sum_head, signature blocks are streamed:

```text
For i in 0 .. count:
  +---------------------------------+----------------------------+
  | int32 LE rolling_sum (sum1)     | s2length bytes strong_sum  |
  +---------------------------------+----------------------------+
```

Upstream sender side reads via `sender.c:receive_sums()` lines 100-122; we
write via `crates/transfer/src/receiver/wire.rs:406-424`
(`write_signature_blocks`). The rolling sum is `int32 LE`, the strong sum is
zero-padded or truncated to exactly `s2length` bytes (matching upstream
`match.c:395` block transmission and `sender.c:102`'s `read_buf(f,
sum2_at(s, i), s->s2length)`).

Note that the per-block strong sum length sent on the wire is the
generator-chosen `s2length` (between 2 and 16), **not** the full digest
length of MD4/MD5/xxh3 - upstream truncates at write, we truncate at write.

## 5. Phase-aware checksum lengths

Upstream defines two constants in `rsync.h:714-715`:

```c
#define SUM_LENGTH       16
#define SHORT_SUM_LENGTH 2
```

The receiver toggles `csum_length` between the two values across the two
delta-transfer phases:

- **Phase 1** (initial pass): `csum_length = SHORT_SUM_LENGTH` (= 2). Smaller
  signatures, more false-alarm matches expected. Upstream:
  `generator.c:2188 csum_length = SHORT_SUM_LENGTH;`.
- **Phase 2** (redo pass): `csum_length = SUM_LENGTH` (= 16). Full
  collision-resistance for files that failed phase 1's whole-file MD4/MD5
  trailer. Upstream is set elsewhere in `generator.c` near the redo handler
  (the constant is referenced as `SUM_LENGTH` and stored in `csum_length`).

`generator.c:sum_sizes_sqroot()` then derives the actual on-wire `s2length`
via:

```c
b = BLOCKSUM_BIAS;
while ((l >>= 1) != 0) b += 2;
while ((c >>= 1) != 0 && b > 0) b--;
s2length = (b + 1 - 32 + 7) / 8;
s2length = MAX(s2length, csum_length);
s2length = MIN(s2length, SUM_LENGTH);
```

oc-rsync conformance:

- Constants: `crates/signature/src/block_size.rs:76` (`SHORT_SUM_LENGTH = 2`)
  and `:82` (`MAX_SUM_LENGTH = 16`). Test
  `sum_length_constants_match_upstream` at `block_size.rs:580-586` enforces
  the values match upstream's `rsync.h:714-715`.
- Heuristic: `layout.rs::derive_strong_sum_length` reproduces the bias loop,
  with the phase-2 short-circuit `if checksum_length == SUM_LENGTH` returning
  the negotiated digest width (not an unconditional `MAX_SUM_LENGTH`),
  preserving the redo-pass invariant while also applying the
  `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)` cap from
  `generator.c:705`.
- Phase distinction: `crates/transfer/src/receiver/mod.rs:75` and `:84`
  document the `csum_length = SHORT_SUM_LENGTH` and `csum_length =
  SUM_LENGTH` upstream call-sites at `generator.c:2157` and `generator.c:2163`
  respectively.
- Tests:
  - `sum_length_derive_strong_phase1_is_dynamic` (`layout.rs`) confirms phase 1
    produces a value `>= SHORT_SUM_LENGTH <= SUM_LENGTH`.
  - `sum_length_derive_strong_phase2_redo_returns_max` (`layout.rs`) confirms
    phase 2 returns the full negotiated digest width for any file/block size.
  - `sum_length_phase_toggle_produces_different_layouts` (`layout.rs`)
    confirms the phase 1 value differs from phase 2 for the same file.
  - `phase1_strong_sum_capped_by_narrow_transfer_digest` and
    `phase2_strong_sum_capped_by_narrow_transfer_digest` (`layout.rs`) confirm
    the `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)` cap.
  - `specific_upstream_compatibility_values` (`block_size.rs`) pins the
    block-length output for several known file sizes against
    upstream-derived values (1 MiB -> 1024, 10 MiB -> 3232, 100 MiB ->
    10240, 1 GiB -> 32768).

## 6. End-of-file marker encoding

Upstream produces the end marker via two cooperating call-sites:

- `match.c:343` calls `matched(f, s, buf, len, -1)`.
- `match.c:matched()` line 117 calls `send_token(f, i, ...)` with `i == -1`.
- For uncompressed mode, `simple_send_token` line 318 writes
  `write_int(f, -((-1)+1)) = write_int(f, 0)` - a four-byte LE zero.
- For compressed modes, `send_deflated_token` line 462, `send_zstd_token`
  line 774, and `send_compressed_token` (LZ4) line 952 all write a single
  `END_FLAG` (0x00) byte after flushing.

oc-rsync conformance:

- Uncompressed: `crates/protocol/src/wire/delta/token.rs:95-97` writes
  `write_int(0)`. Reader: `token.rs:208-215` (`read_token` returns
  `Ok(None)` when the int32 LE token is 0).
- Compressed (zlib): `compressed_token/zlib_codec.rs:101-109`
  flushes pending literals and run, then writes `&[END_FLAG]`.
- Compressed (zstd): `compressed_token/zstd_codec.rs:144-156` same pattern.
- Compressed (lz4): `compressed_token/lz4_codec.rs:139-147` same pattern.

After the END marker, upstream writes the **whole-file checksum trailer**
(`match.c:426 write_buf(f, sender_file_sum, xfer_sum_len)`). This is part of
the framing the receiver must drain after recognising END but it is not part
of the token stream itself; it is handled in
`crates/transfer/src/receiver/transfer.rs` and is out of scope for this
delta-stream audit.

## 7. Per-token zlib flush boundary

Upstream `token.c:send_deflated_token()` (lines 357-485) compresses literal
data with `Z_NO_FLUSH` until either the input is exhausted **or** the next
token arrives, at which point it forces `Z_SYNC_FLUSH` (line 434). The
flushed output ends with the standard four-byte sync trailer
`{0x00, 0x00, 0xFF, 0xFF}`, which upstream **strips** from the wire (lines
442-449) and then the receiver re-injects (lines 575-579) before calling
`inflate(Z_SYNC_FLUSH)`.

oc-rsync conformance:

- Strip on send:
  `crates/protocol/src/wire/compressed_token/zlib_codec.rs:177-212`
  (`sync_flush`). Lines 199-204 explicitly trim the trailing
  `0x00 0x00 0xFF 0xFF` sequence before emitting the `DEFLATED_DATA` frame.
- Reinject on receive: `zlib_codec.rs:351-353`
  (`self.compressed_input_buf.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF])`)
  prior to feeding into `Decompress::decompress(... FlushDecompress::Sync)`.
- Per-token flush sequencing: `zlib_codec.rs:80-99` (`send_block_match`)
  calls `flush_all_literals` -> `sync_flush` before writing the run header,
  matching upstream's `nb == 0 -> flush = Z_SYNC_FLUSH; deflate(...)`
  ordering at `token.c:433-435`.
- `see_token` history feed: `zlib_codec.rs:115-136` (encoder) and
  `:444-491` (decoder). These mirror upstream `see_deflate_token`
  (`token.c:631-670`), including the `protocol_version >= 31` cursor
  advance fix at `zlib_codec.rs:131-133` and the stored-block fake-header
  pattern at `zlib_codec.rs:455-465`.

This per-token zlib flush parity was the recent fix referenced in the task
brief. The encoder produces one `Z_SYNC_FLUSH` per token boundary, with the
sync trailer stripped, and the decoder drains all consecutive
`DEFLATED_DATA` frames before reinjecting the trailer. Coverage:

- `golden_zlib_sync_marker_stripped_from_output`
  (`crates/protocol/tests/zlib_golden_bytes.rs:226-249`).
- `golden_zlib_raw_deflate_no_zlib_header`
  (`zlib_golden_bytes.rs:250-292`) confirms the wire is raw deflate with
  no zlib header.
- `golden_zlib_deflated_data_header_exact_bytes`
  (`zlib_golden_bytes.rs:187-225`).

## 8. Per-token zstd flush

Upstream `send_zstd_token()` (lines 678-776) uses `ZSTD_e_continue` while
feeding literal bytes and switches to `ZSTD_e_flush` at every token boundary
(line 741). Output is accumulated in `obuf` (`MAX_DATA_COUNT + 2` bytes) and
emitted as a single `DEFLATED_DATA` frame whenever the buffer fills or the
flush operation reports completion (lines 755-762).

A critical upstream property: the zstd compression context (`zstd_cctx`) is
**created once at line 686 and never reset** for the duration of the
session. Files share a single continuous zstd stream; only the run-encoding
state is reset between files (lines 700-703).

oc-rsync conformance:

- Encoder: `crates/protocol/src/wire/compressed_token/zstd_codec.rs:49-264`.
- Continuous stream: `zstd_codec.rs:96-103` `reset()` clears only
  `last_token`, `run_start`, `last_run_end`, `flush_pending`, and the
  `literal_buf`. The `ZstdRawEncoder` field (`encoder`) is preserved across
  calls. Comment at `:88-95` cites `token.c:700-703`.
- Per-token flush: `zstd_codec.rs:173-226` (`compress_and_flush`). After
  feeding all input with `encoder.run` (continue), it loops calling
  `encoder.flush` until `remaining == 0`, writing every full or partial
  output buffer as a `DEFLATED_DATA` frame. Comment at `:204-206` cites
  `token.c:740-743`.
- Buffer fill triggers DEFLATED_DATA: `zstd_codec.rs:185-202`. Matches
  upstream's `if (zstd_out_buff.pos == zstd_out_buff.size || flush ==
  ZSTD_e_flush)` (line 755).
- Decoder continuous-stream parity: `zstd_codec.rs:266-477`. The DCtx is
  preserved across `reset()` calls (`:325-332`).
- Coverage:
  - `zstd_flush_produces_single_deflated_data_block_for_small_input`
    (`zstd_codec.rs:606-652`) verifies a small literal compresses into
    exactly one `DEFLATED_DATA` block.
  - `zstd_large_literal_splits_into_max_data_count_blocks`
    (`zstd_codec.rs:725-803`) verifies multiple full-buffer DEFLATED_DATA
    blocks are produced for incompressible 500 KiB input.
  - `zstd_continuous_stream_across_files` (`zstd_codec.rs:813-846`).
  - `zstd_deflated_data_header_matches_upstream`
    (`zstd_codec.rs:887-920`).

## 9. Per-token LZ4 frame flush

LZ4 has no streaming flush concept upstream - each call to
`send_compressed_token` (lines 881-954) compresses its literal payload with
`LZ4_compress_default` in `MIN(nb, MAX_DATA_COUNT)`-sized chunks (line 927).
Each chunk becomes one `DEFLATED_DATA` block on the wire (lines 938-941).
The encoder retries with halved input if the compressed output exceeds
`MAX_DATA_COUNT` (line 930).

The receiver decompresses each `DEFLATED_DATA` frame independently with
`LZ4_decompress_safe` (line 1008); there is no persistent LZ4 state on
either side.

oc-rsync conformance:

- Encoder: `crates/protocol/src/wire/compressed_token/lz4_codec.rs:46-232`.
  - Eager flush during `send_literal`: `lz4_codec.rs:93-100` compresses any
    accumulated buffer >= `MAX_DATA_COUNT` immediately so whole-file
    transfers don't buffer the entire file before emitting the first
    DEFLATED_DATA (matching upstream's per-call compression model).
  - Halve-and-retry loop: `lz4_codec.rs:182-209`. Line 197 (`available_in
    /= 2`) cites upstream `token.c:930`.
  - Single `DEFLATED_DATA` per chunk: `lz4_codec.rs:188-191` writes the
    header then the compressed bytes. Cites upstream `token.c:938-941`.
- Decoder: `lz4_codec.rs:240-364`. Each frame decompressed via
  `block::decompress_into` (line 307), no persistent state between frames.
- Coverage:
  - `lz4_flush_produces_single_deflated_data_block_for_small_input`
    (`lz4_codec.rs:803-846`).
  - `lz4_large_literal_splits_into_max_data_count_blocks`
    (`lz4_codec.rs:554-628`).
  - `lz4_send_literal_eagerly_emits_deflated_data`
    (`lz4_codec.rs:855-894`) verifies eager flush behaviour.
  - `lz4_incremental_literal_flush_for_whole_file_transfer`
    (`lz4_codec.rs:902-938`).
  - `lz4_deflated_data_header_matches_upstream`
    (`lz4_codec.rs:735-768`).

## 10. Discrepancies

Comparing upstream `match.c` and `token.c` against the oc-rsync sources,
this audit found the following divergences. All are accounted for and
either match upstream exactly or document a deliberate-and-equivalent
choice.

### 10.1 No discrepancies in token framing

The flag-byte mask, run encoding, `DEFLATED_DATA` header layout, and
END_FLAG are byte-identical to upstream. Run-coalescing logic at
`zlib_codec.rs:91`, `zstd_codec.rs:126`, `lz4_codec.rs:122` matches the
upstream break condition in `token.c:384`, `token.c:706`, `token.c:899`
respectively.

### 10.2 No discrepancies in `s2length` truncation

`crates/transfer/src/receiver/wire.rs:417-421`
zero-pads or truncates the strong sum to exactly `s2length` bytes before
write. Upstream `sender.c:102` reads exactly `s2length` bytes per block.
Identical wire footprint.

### 10.3 Internal opcode-based delta format coexists with the upstream
format

`crates/protocol/src/wire/delta/internal.rs` provides an alternative
opcode-based format used by older internal serialisation. It is **not** on
the wire to upstream rsync peers - all wire-bound delta streams flow
through `token.rs` and `compressed_token/`. The internal format is
documented as "for backward compatibility with earlier versions of this
implementation" at `delta/types.rs:8-14`. No discrepancy on the wire.

### 10.4 `crates/protocol/src/wire/signature.rs` is an orthogonal codec

That file uses **varint** encoding for `block_count`/`block_length`/
`strong_sum_length`. It is a separate, self-contained signature codec
provided for in-process consumers and is **not** the wire format used to
talk to upstream rsync. The upstream-compatible sum_head goes through
`crates/transfer/src/receiver/wire.rs::SumHead` (four 32-bit LE integers,
no varints) which is exercised by the protocol golden tests
`golden_v28_sum_head_md4` and `golden_v29_sum_head_md4`. This dual
existence is a documentation hazard rather than a wire bug; flagging it
here so future contributors do not mistakenly route upstream traffic
through `wire/signature.rs`.

### 10.5 `protocol_version >= 31` zlib `see_token` cursor advance

This is upstream's bug-compat boundary at `token.c:473` and `token.c:656`.
Below v31, the basis-block payload is fed back into the deflate stream
without advancing the input cursor (preserving the data-duplicating bug).
At v31 and above, the cursor advances. We mirror this at
`zlib_codec.rs:131-133` (encoder) and the decoder side feeds via the
stored-block trampoline at `zlib_codec.rs:455-465`. This is not a
discrepancy - it is upstream parity.

### 10.6 `END_FLAG = 0x00` is unambiguous

A `flag == 0x00` byte is unambiguously the END marker because every other
valid flag has at least one set bit:

- `TOKEN_REL` (0x80) and `TOKENRUN_REL` (0xC0) carry bit 7.
- `DEFLATED_DATA` (0x40) carries bit 6.
- `TOKEN_LONG` (0x20) and `TOKENRUN_LONG` (0x21) carry bit 5.

Only `END_FLAG` is byte zero. Our decoders test in upstream order:
`(flag & 0xC0) == DEFLATED_DATA` first, then `flag == END_FLAG`, then
`flag & TOKEN_REL`, then `flag & 0xE0 == TOKEN_LONG`. This sequence
mirrors `token.c:582`, `:825`, and `:988` for the three compression
modes. No discrepancy.

## 11. Golden-byte test coverage

### 11.1 By protocol version

| Version | Sum_head golden                                              | Notes                                                |
|---------|---------------------------------------------------------------|------------------------------------------------------|
| 28      | `golden_v28_sum_head_md4` in `golden_protocol_v28_wire.rs:520`| Pinned bytes for count/blength/s2length/remainder    |
| 29      | `golden_v29_sum_head_md4` in `golden_protocol_v29_wire.rs:248`| Same struct, MD4 strong sum                          |
| 30      | covered by `protocol_v30_compat.rs`                           | Block-size cap shifts to 1<<17                       |
| 31      | covered by `protocol_v31_comprehensive.rs`                    | xattr/INC_RECURSE wire orthogonal to delta           |
| 32      | covered by `protocol_v32_compat.rs`                           | No on-wire delta change vs v31                       |

The token stream itself is version-invariant (`match.c` and `token.c` do
not branch on `protocol_version` for token framing, only for the v31 zlib
cursor advance which we test via `zlib_codec.rs::see_token` parity).
Property tests exercise the full int32-LE range across all versions:

- `crates/protocol/tests/proptest_delta_roundtrip.rs::token_literal_roundtrip`
  (line 121) covers literal sizes 1..=65536.
- `proptest_delta_roundtrip.rs::token_block_match_roundtrip` (line 149)
  covers block indices `0..i32::MAX - 1`.
- `proptest_delta_script_roundtrip.rs` covers full
  literal+copy+end stream round-trips.

### 11.2 By compression algorithm

| Algorithm | Golden test file                                       | Test count | Coverage                                                                                               |
|-----------|--------------------------------------------------------|------------|--------------------------------------------------------------------------------------------------------|
| zlib      | `crates/protocol/tests/zlib_golden_bytes.rs`           | 44         | END_FLAG, TOKEN_LONG, TOKENRUN_LONG, TOKEN_REL all offsets 0/5/63/64, TOKENRUN_REL, mixed lit+match, sync marker stripped, raw deflate (no zlib header), DEFLATED_DATA header exact bytes, run >= 256 |
| zstd      | `crates/protocol/tests/zstd_golden_bytes.rs`           | 30         | DEFLATED_DATA header, single-block flush, multi-block split at MAX_DATA_COUNT, continuous stream across files, interleaved literal+match, block_match without literals |
| lz4       | `crates/protocol/tests/lz4_golden_bytes.rs`            | 34         | DEFLATED_DATA header, single-block flush, eager send_literal flush, multi-block split, halve-and-retry, run-encoding equivalence with zlib/zstd                                                       |
| zstd interop | `crates/protocol/tests/zstd_interop_golden_bytes.rs`| 26         | Captured upstream-produced byte sequences for zstd compressed streams                                  |
| zstd daemon  | `crates/protocol/tests/zstd_daemon_recv_golden.rs`  | 16         | Daemon-side recv path with zstd                                                                         |
| compressed token unit | `crates/protocol/src/wire/compressed_token/tests.rs` | -    | Encoder/decoder unit-level round-trips for zlib/zstd/lz4                                                |

### 11.3 Match.c hash search coverage

The block matching algorithm in upstream `match.c:hash_search()` (lines
140-345) is mirrored by `crates/match/src/generator.rs` and the
`MatchedBlocks` index at `crates/match/src/index.rs`. Tests:

- `crates/match/src/script.rs:184-444` covers `apply_delta` for
  literal-only, copy-only, multi-literal, and multi-copy scripts.
- The fuzzy basis-file matcher (`crates/match/src/fuzzy/`) exists
  for `--fuzzy` mode. Wire-format orthogonal.
- Property tests in
  `crates/protocol/tests/proptest_delta_script_roundtrip.rs` exercise
  the full token-stream encode/decode cycle.

### 11.4 Sum_head round-trip coverage

`crates/signature/src/block_size.rs` carries the only non-trivial
heuristic (square-root + bit-rounding). Tests at lines 333-648:

- `constants_match_upstream` (line 337-343) pins `DEFAULT_BLOCK_SIZE
  = 700`, `MAX_BLOCK_SIZE_V30 = 131_072`, `MAX_BLOCK_SIZE_OLD =
  536_870_912`.
- `specific_upstream_compatibility_values` (line 633-648) pins block
  lengths for 1 MiB, 10 MiB, 100 MiB, 1 GiB.
- `block_length_is_multiple_of_8` (line 437-453) confirms upstream's
  rounding convention.
- `protocol_version_affects_maximum` (line 399-414) asserts the v30
  switch from 1<<29 to 1<<17.

## 12. Summary

The delta token stream is **byte-equivalent** to upstream rsync 3.4.1 for
all protocol versions oc-rsync supports (28, 29, 30, 31, 32) across all
four `do_compression` modes (`CPRES_NONE`, `CPRES_ZLIB` / `CPRES_ZLIBX`,
`CPRES_ZSTD`, `CPRES_LZ4`).

Confirmed parity points:

- LITERAL chunked at 32 KiB (`CHUNK_SIZE`, `rsync.h:158`).
- COPY encoded as `int32 LE -(token+1)` uncompressed; flag-byte +
  optional run count compressed.
- END encoded as `int32 LE 0` uncompressed; single `END_FLAG` byte
  compressed.
- DEFLATED_DATA header packs 14-bit length across two bytes
  (`MAX_DATA_COUNT = 16383`).
- zlib `Z_SYNC_FLUSH` per token boundary, sync trailer
  `{0x00, 0x00, 0xFF, 0xFF}` stripped on send, reinjected on receive.
- zstd `ZSTD_e_flush` per token boundary, single continuous CCtx/DCtx
  across the session.
- lz4 per-call compression in `MAX_DATA_COUNT`-capped chunks with halve-
  and-retry on overflow.
- sum_head four 32-bit LE integers, with `s2length` field present on
  protocol >= 27 (always present in our supported range).
- `s2length` derived via the BLOCKSUM_BIAS heuristic, clamped to
  `[csum_length, SUM_LENGTH]` where `csum_length = SHORT_SUM_LENGTH` in
  phase 1 and `csum_length = SUM_LENGTH` in phase 2 redo.

Discrepancy ledger: zero on-wire divergences. Two structural notes:

1. The orthogonal `crates/protocol/src/wire/signature.rs` codec (varint-
   based) is documented as not wire-bound to upstream peers; flagged in
   section 10.4 to prevent future misrouting.
2. The internal opcode-based delta format
   (`crates/protocol/src/wire/delta/internal.rs`) coexists with the
   upstream-compatible token format and is documented as legacy-internal
   in `delta/types.rs:8-14`.

Both notes are documentation guardrails, not interoperability defects.
