# Delta wire-format - internal documentation

Tracking issue: oc-rsync task #2108.

## Scope

Documents the delta token wire encoding as implemented in this repository,
without referencing upstream C source. Companion to
`docs/audits/delta-wire-format-audit.md`, which performs byte-for-byte
upstream conformance review. This document describes only the byte layout of
each operation in the token stream, the two encoding families that coexist
(legacy 4-byte LE vs. internal varint), the cited file:line for every layout
element, and the test coverage that pins each shape.

Sources surveyed:

- `crates/protocol/src/wire/delta/mod.rs:29-43` - submodule exports
- `crates/protocol/src/wire/delta/int_encoding.rs:32-66` - 4-byte LE
  primitives (`write_int`, `read_int`)
- `crates/protocol/src/wire/delta/token.rs:42-215` - upstream-compatible token
  framing (literal, block match, end marker, whole-file)
- `crates/protocol/src/wire/delta/internal.rs:35-145` - internal opcode +
  varint framing (`write_delta_op`, `read_delta_op`, `write_delta`,
  `read_delta`)
- `crates/protocol/src/wire/delta/types.rs:4-46` - `CHUNK_SIZE`, `DeltaOp`
- `crates/protocol/src/wire/delta/tests.rs` - unit tests over both encodings
- `crates/protocol/src/wire/mod.rs:30-47` - public re-exports for
  `protocol::wire`
- `crates/protocol/tests/proptest_delta_roundtrip.rs` - property tests
- `crates/protocol/tests/proptest_delta_script_roundtrip.rs` - property tests
- `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs` - golden
  bytes for protocol 28 multiplexed delta-stats tail

## Two encoding families

The crate ships two distinct delta encodings that share the in-memory
`DeltaOp` enum (`crates/protocol/src/wire/delta/types.rs:27-46`):

1. **Token framing (legacy 4-byte LE).** This is the format used on the wire
   to interoperate with rsync 3.x peers. Implemented in
   `crates/protocol/src/wire/delta/token.rs`. Every token is exactly one
   `i32` written little-endian via
   `crates/protocol/src/wire/delta/int_encoding.rs:33-35`. Literal data
   bytes follow inline after a positive token. There is no opcode byte.

2. **Internal opcode + varint format.** A backward-compatibility shape used
   by older subsystems and within tests; not part of the wire protocol with
   external peers. Implemented in
   `crates/protocol/src/wire/delta/internal.rs`. Each operation begins with
   a single opcode byte, followed by varint-encoded scalar fields, and (for
   literals) inline data.

Both formats are exported through `protocol::wire`
(`crates/protocol/src/wire/mod.rs:30-47`).

## Token framing - byte layout (legacy 4-byte LE)

All multi-byte integers in this family are signed 32-bit little-endian,
written by `write_int` and read by `read_int`
(`crates/protocol/src/wire/delta/int_encoding.rs:33-35,62-65`):

```text
[byte0, byte1, byte2, byte3]   value = i32::from_le_bytes([b0,b1,b2,b3])
```

### LITERAL token

Layout (`crates/protocol/src/wire/delta/token.rs:42-52`):

```text
+----------------+--------------------+
| length (i32 LE)| length bytes raw   |
| > 0            | of literal data    |
+----------------+--------------------+
```

- The length field is the byte count of the literal payload that
  immediately follows.
- Literals are auto-chunked: if the source slice exceeds
  `CHUNK_SIZE = 32 * 1024` (`crates/protocol/src/wire/delta/types.rs:4`),
  `write_token_literal` emits multiple back-to-back `length+payload` records
  rather than a single oversize record (`token.rs:43-50`). Receivers
  reassemble by concatenation.
- Length 0 is never emitted (loop at `token.rs:43-50` skips); a zero in the
  length slot is reserved for the end marker.

### COPY token (block match)

Layout (`crates/protocol/src/wire/delta/token.rs:73-77`):

```text
+----------------+
| token (i32 LE) |
| < 0            |
+----------------+

token = -((block_index as i32) + 1)
```

- The token-stream COPY carries **only the block index**, not a length.
  The length to copy is implied by the block size negotiated earlier in
  `sum_head` (see `crates/transfer/src/delta_transfer.rs:100-108` data flow
  and the `read_signature` schema at
  `crates/protocol/src/wire/signature.rs:104-133`). The doc comment at
  `crates/protocol/src/wire/delta/token.rs:130-138` records this invariant.
- Encoding examples: `block 0 -> -1`, `block 1 -> -2`, `block 42 -> -43`.
- The receiver decodes via `read_token`
  (`crates/protocol/src/wire/delta/token.rs:208-215`); negative return
  values map back to `block_index = -(token + 1)`.

### End marker

Layout (`crates/protocol/src/wire/delta/token.rs:94-97`):

```text
+----------------+
|     0x00 x4    |   write_int(writer, 0)
+----------------+
```

- One `i32` of value `0`. `read_token` returns `Ok(None)` upon encountering
  it (`token.rs:208-214`), signalling end-of-stream.
- The whole-file shortcut writes only literal chunks plus this terminator
  (`crates/protocol/src/wire/delta/token.rs:115-118`,
  `write_whole_file_delta`).

### Stream composition (`write_token_stream`)

`crates/protocol/src/wire/delta/token.rs:158-170` emits, for each `DeltaOp`:

- `DeltaOp::Literal(data)` -> `write_token_literal` (auto-chunked).
- `DeltaOp::Copy { block_index, .. }` -> `write_token_block_match` (the
  `length` field on `Copy` is **discarded**, by design - block size is
  implied).

After the loop, a single `write_token_end` is appended.

## Internal opcode + varint format

This format is **not** the legacy wire shape. It exists for in-process
use cases where a self-describing encoding is preferable. Implemented in
`crates/protocol/src/wire/delta/internal.rs`. All length and index fields
use the protocol crate's varint codec (`crates/protocol/src/varint`).

### Per-operation framing

Layout (`crates/protocol/src/wire/delta/internal.rs:35-52`):

```text
LITERAL                          COPY
+------+----------+-----------+  +------+--------------+--------+
| 0x00 | len (vi) | len bytes |  | 0x01 | block_idx vi | len vi |
+------+----------+-----------+  +------+--------------+--------+
```

- `0x00` = LITERAL opcode (`internal.rs:38`). Followed by `write_varint`
  length (`internal.rs:39`), then `data.len()` raw bytes (`internal.rs:40`).
- `0x01` = COPY opcode (`internal.rs:46`). Followed by `block_index`
  varint (`internal.rs:47`), then `length` varint (`internal.rs:48`).
- Any other opcode byte is rejected as `InvalidData`
  (`internal.rs:90-93`).
- Negative literal lengths are rejected (`internal.rs:71-76`).

### Stream framing

Layout (`crates/protocol/src/wire/delta/internal.rs:109-115`):

```text
+--------------+---------+---------+ ... +---------+
| count (vi)   | op #0   | op #1   |     | op #N-1 |
+--------------+---------+---------+ ... +---------+
```

- The stream begins with a varint operation count
  (`internal.rs:110`).
- The reader caps `Vec::with_capacity` at `count.min(1024)` to prevent OOM
  on hostile input (`internal.rs:138`).
- Negative counts are rejected (`internal.rs:128-134`).

### Differences from token framing

| Aspect              | Token framing                       | Internal format                |
|---------------------|-------------------------------------|--------------------------------|
| Stream length cue   | `write_int(0)` end marker           | Leading varint `count`         |
| Per-op opcode       | None (sign of `i32` discriminates)  | Single byte (`0x00`/`0x01`)    |
| Length encoding     | i32 LE (4 bytes)                    | varint (1-5 bytes)             |
| COPY length         | Implicit (from `sum_head`)          | Explicit varint                |
| LITERAL chunking    | Auto at `CHUNK_SIZE = 32 KiB`       | One record per `Vec<u8>`       |
| Wire-compatible     | Yes (peer rsync)                    | No (in-process only)           |

## Public surface

Exposed from `protocol::wire`
(`crates/protocol/src/wire/mod.rs:30-47`):

- `read_int`, `write_int`, `CHUNK_SIZE`, `DeltaOp`
- Token framing: `read_token`, `write_token_literal`,
  `write_token_block_match`, `write_token_end`, `write_token_stream`,
  `write_whole_file_delta`
- Internal format: `read_delta`, `read_delta_op`, `write_delta`,
  `write_delta_op`

## Rolling-checksum sign-extension (PR #3560)

The delta token stream relies on the rolling checksum (rsum) to identify
matching blocks before COPY tokens can be emitted. PR #3560
(commit `723a0a0c1`) corrected a sign-extension bug that produced wrong
rsum values for any byte `>= 0x80`, breaking delta interop on binary basis
files. The fix is intact in this codebase:

- Scalar 4-byte unrolled loop:
  `crates/checksums/src/rolling/checksum/mod.rs:578-591` (each
  `block[i]` folded via `sign_extend_byte`).
- Trailing-bytes loop:
  `crates/checksums/src/rolling/checksum/mod.rs:593-596` (same helper).
- `sign_extend_byte` definition:
  `crates/checksums/src/rolling/checksum/mod.rs:603-606`
  (`((byte as i8) as i32) as u32`).
- Single-byte fast path (`update_byte`):
  `crates/checksums/src/rolling/checksum/mod.rs:191-198`.
- Sliding-window roll (`roll`):
  `crates/checksums/src/rolling/checksum/mod.rs:333-350`.
- Multi-byte roll (`roll_many`):
  `crates/checksums/src/rolling/checksum/mod.rs:410-413` (i128 sign-extend
  via `i128::from(out_b as i8)`).
- AVX2 path:
  `crates/checksums/src/rolling/checksum/x86.rs:222-235` uses
  `_mm256_cvtepi8_epi16` widening with `_mm256_madd_epi16`.
- SSE2 path:
  `crates/checksums/src/rolling/checksum/x86.rs:148-157` uses
  `_mm_cmplt_epi8` sign-mask plus `_mm_unpacklo/hi_epi8`.
- NEON path:
  `crates/checksums/src/rolling/checksum/neon.rs:97-105` uses
  `vreinterpretq_s8_u8` plus `vmovl_s8` widening.

Verification: every code site above retains an inline comment of the form
`// upstream: checksum.c... schar`, and every accumulation uses a signed
intermediate. Removing the sign-extension at any one of those sites would
reproduce the pre-#3560 rsum drift on bytes `>= 0x80`.

## Test coverage

### Unit tests (`crates/protocol/src/wire/delta/tests.rs`)

Internal opcode format:

- Roundtrips: `delta_op_roundtrip_literal` (line 8),
  `delta_op_roundtrip_copy` (line 20), `delta_stream_roundtrip_mixed_ops`
  (line 35), `delta_stream_empty` (line 62),
  `delta_stream_single_large_literal` (line 88).
- Error path: `delta_op_rejects_invalid_opcode` (line 74).

Token framing primitives:

- `write_int_roundtrip` (line 107), `write_int_little_endian` (line 119).
- LITERAL chunking: `write_token_literal_small` (line 126),
  `write_token_literal_chunked` (line 138).
- COPY: `write_token_block_match_encoding` (line 159).
- End marker: `write_token_end_is_zero` (line 174).
- Whole-file shortcut: `write_whole_file_delta_format` (line 181).
- Decoding: `read_token_parses_literals_and_blocks` (line 198),
  `write_token_stream_mixed_ops` (line 213).

`CHUNK_SIZE` boundaries (token framing):

- Sizes around the 32 KiB chunk seam:
  `delta_oversized_literal_exactly_chunk_size` (line 279),
  `delta_oversized_literal_one_byte_over_chunk_size` (line 293),
  `delta_oversized_literal_multiple_chunks` (line 316),
  `delta_oversized_literal_multiple_chunks_with_remainder` (line 336),
  `delta_oversized_literal_chunk_boundary_minus_one` (line 486),
  `chunk_boundary_exact_double_chunk_size` (line 605),
  `chunk_boundary_two_chunks_plus_one_byte` (line 629),
  `chunk_boundary_split_verification` (line 661),
  `chunk_boundary_off_by_one_before_boundary` (line 799),
  `chunk_boundary_off_by_one_after_boundary` (line 814).
- Reconstruction: `delta_oversized_literal_reconstruction` (line 366),
  `..._exact_multiple` (line 388),
  `delta_oversized_literal_via_whole_file` (line 451),
  `chunk_boundary_streaming_reconstruction` (line 844).
- Edge inputs: `delta_oversized_literal_empty` (line 465),
  `_single_byte` (line 474), `_very_large` (line 498),
  `_data_integrity` (line 515),
  `delta_stream_with_consecutive_oversized_literals` (line 561),
  `chunk_boundary_zero_filled_chunks` (line 892),
  `chunk_boundary_alternating_pattern_integrity` (line 871),
  `chunk_boundary_all_different_bytes` (line 905),
  `chunk_boundary_stress_test_many_operations` (line 924),
  `chunk_boundary_write_read_symmetry` (line 981).

### Property tests

- `crates/protocol/tests/proptest_delta_roundtrip.rs` covers both encodings:
  internal copy/literal/stream/empty (lines 53, 69, 88, 100); token
  literal/block-match/stream/empty/large (lines 121, 149, 241, 252, 318);
  determinism (lines 298, 308); zero-length and max-index edges (lines 271,
  283).
- `crates/protocol/tests/proptest_delta_script_roundtrip.rs` covers
  single-op and interleaved scripts (lines 99, 109, 122, 131).

### Golden bytes

- `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs` pins
  the protocol 28 multiplexed delta-stats trailer that follows the token
  stream.

## Coverage gaps and recommended further tests

The byte-layout invariants for the three operation types are well covered
by unit and property tests. The following gaps would strengthen confidence
in cross-version interop and the legacy/varint coexistence:

1. **No golden-byte fixture for the bare token stream itself.** The current
   golden tests pin the multiplexed protocol-28 trailer but not a raw
   `LITERAL + COPY + END` byte sequence. Add a fixture in
   `crates/protocol/tests/golden/` that asserts exact bytes for a
   hand-crafted stream (e.g. 5-byte literal, block-index 0 COPY, end
   marker -> `[0x05,0,0,0, b'h',b'e',b'l',b'l',b'o', 0xff,0xff,0xff,0xff,
   0,0,0,0]`). This catches accidental endian flips or framing reordering
   without depending on multiplex layering.

2. **Negative `i32` boundary on COPY.** `write_token_block_match` accepts a
   `u32` block_index. The maximum encodable index is `i32::MAX - 1`
   (becomes token `i32::MIN + 2`); indices at or beyond that produce
   wrap-around. Add a unit test that asserts the highest valid index
   roundtrips and that `read_token` correctly recovers
   `block_index = -(token+1)` for `token = i32::MIN + 1`.

3. **CHUNK_SIZE boundary on `write_whole_file_delta`.** The dedicated
   whole-file path is covered for empty and small inputs but not for inputs
   that straddle exactly `2 * CHUNK_SIZE` and `3 * CHUNK_SIZE + 1`. Adding
   asserts that the resulting wire layout has exactly `ceil(N/CHUNK_SIZE)`
   `length+payload` records followed by one terminator would cement the
   chunking contract.

4. **Internal-format fuzz on truncated streams.** `read_delta` correctly
   rejects negative counts but does not have a coverage test that verifies
   graceful `UnexpectedEof` propagation when the stream is truncated mid
   varint, mid opcode, or mid literal payload. A property test that takes a
   well-formed encoding and asserts `read_delta` returns
   `ErrorKind::UnexpectedEof` for every prefix length would close that gap.

5. **Cross-encoding equivalence.** A property test that encodes the same
   `Vec<DeltaOp>` via both `write_token_stream` and `write_delta`, decodes
   each, and asserts both produce the same logical sequence (modulo
   chunking of literals and the implicit-length COPY) would protect the
   shared `DeltaOp` data model from drift if either encoder changes.

6. **Rolling-checksum signed-byte regression guard at the wire boundary.**
   The checksum crate has parity tests for SIMD vs. scalar, but there is
   no test in the protocol or transfer crate that constructs a basis with
   bytes `>= 0x80`, runs delta generation against a modified source, and
   asserts that the resulting token stream contains COPY records (not just
   LITERAL records). Such a test would catch a future regression of the
   #3560 fix at the level it actually matters - delta matching - without
   depending on byte-level checksum parity.

## Summary

The delta wire format in this repository has two distinct shapes: a 4-byte
LE token stream that is wire-compatible with peer rsync (literals carry an
explicit length, COPY carries only a sign-encoded block index, end-of-stream
is `i32` zero), and an internal varint-tagged opcode format that is
self-delimiting. Byte layouts, public APIs, and unit/property coverage are
documented above with file:line citations. The PR #3560 sign-extension fix
to the rolling checksum is intact across scalar, AVX2, SSE2, NEON, and
roll/roll_many paths. Recommended additions focus on golden fixtures for the
bare token stream, boundary tests for COPY index limits and whole-file
chunking, truncation fuzz for the internal format, cross-encoding
equivalence, and a wire-level regression guard for the signed-byte rolling
checksum semantics.
