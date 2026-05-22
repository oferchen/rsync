# Fuzz corpus gap analysis (FCV-18)

Tracking issue: FCV-18 (#2664). Reads on top of FCV-17
(`docs/audits/fuzz-corpus-inventory.md`). Drives the seed-generation
work in FCV-19 (#2665).

## Scope and threshold

Subjective under-seeded threshold for this audit:

- Fewer than 20 seed files in the target's corpus directory, or
- No corpus directory at all, or
- Seeds present but no coverage of the boundary classes catalogued
  below (oversize length, embedded NUL, UTF-8 boundary characters,
  truncation mid-field, recursive nesting limits, negative integers).

Every one of the 21 targets in the top-level `fuzz/` workspace fails
this threshold today. The largest single seeded corpus is `bwlimit`
with 4 seeds; the median is 1 seed; 8 targets have no on-disk corpus
at all. This document enumerates the highest-leverage seed categories
per target and lists concrete byte sequences (in hex) that FCV-19 can
ship verbatim.

## Per-target gap analysis

### `acl_xattr_wire`

- **Current**: 1 seed of 1 byte (`00`). Reaches the empty-ACL branch only.
- **Gaps**: boundary varint lengths in the four ACL permission slots;
  embedded NUL inside xattr names; UTF-8 boundary names; truncation
  between count varint and per-entry varints; maximum named-id list
  depth; abbreviated xattr entries (16-byte MD5 stand-in); request-list
  delta-gap > `i32::MAX`.
- **Seeds**: empty `00`; literal `user.foo=bar` `01 09 75 73 65 72 2e 66
  6f 6f 00 03 62 61 72`; full-perm ACL `00 07 07 07 07 00`; abbreviated
  xattr w/ MD5 placeholder `01 09 75 73 65 72 2e 66 6f 6f 00 10 00 00 00
  00 00 00 00 00 00 00 00 00 00 00 00 00 00`.

### `auth_response`

- **Current**: 1 seed of 33 bytes (`alice ...\n`); only the secrets
  parser is exercised on a benign input.
- **Gaps**: boundary digest lengths MD4 (16), MD5 (16), SHA-1 (20),
  SHA-256 (32), SHA-512 (64); embedded NUL inside username; UTF-8
  boundary characters in passwords; truncation mid-base64; empty and
  comment-only lines; CRLF line endings; protocol selectors 0 and 255.
- **Seeds**: empty `00`; MD5-length verify selector `00 1f 70 61 73 73
  77 6f 72 64 63 68 61 6c 6c 65 6e 67 65 64 65 61 64 62 65 65 66 64 65
  61 64 62 65 65 66`; secrets file w/ CRLF and comment `01 23 20 63 6f
  6d 6d 65 6e 74 0d 0a 75 73 65 72 3a 73 65 63 72 65 74 0d 0a`.

### `batch_reader`

- **Current**: 2 seeds (13 B each), neither carrying the upstream batch
  magic.
- **Gaps**: valid upstream magic (`b04147ce` LE per `batch.c:
  write_batch_open`); protocol-version boundary 28-32 split between
  pre-30 and post-30 codec arms; stream-flag bit layouts (zero,
  all-ones, single-bit walk); truncation at every header offset; valid
  header followed by garbage body; checksum-seed `i32::MIN`/`i32::MAX`/
  `0xFFFFFFFF`.
- **Seeds**: valid proto-31 header w/ empty body `b0 41 47 ce 1f 00 00
  00 00 00 00 00 00 00 00 00`; proto-28 boundary `b0 41 47 ce 1c 00 00
  00 00 00 00 00 00 00 00 00`; magic only `b0 41 47 ce`.

### `bwlimit`

- **Current**: 4 seeds (`0`, `100k`, `1.5m`, `1g`). Best-seeded corpus
  in the tree but still misses boundary parsing.
- **Gaps**: negative values; full suffix alphabet `b`/`k`/`K`/`m`/`M`/
  `g`/`G`/`t`/`T`; decimal/exponent edges (`1.5e9`, `.5`, `1.`);
  RATE:BURST colon-split (only `parse_bandwidth_limit`); whitespace and
  trailing garbage; embedded NUL; UTF-8 multibyte suffix letters.
- **Seeds**: `-100k` `2d 31 30 30 6b`; `1.5m:500k` `31 2e 35 6d 3a 35
  30 30 6b`; `1g x` `31 67 20 78`.

### `capability_flags`

- **Current**: 1 seed (`LsfxCIvu`) covers the identifier `from_str`
  path only.
- **Gaps**: multi-byte varint w/ continuation bits; boundary bit
  patterns for `from_bits` (`0`, `u32::MAX`, single-bit walk); truncated
  varint at first continuation byte; negotiation prologue strings beyond
  `LsfxCIvu` (legacy `@RSYNCD:` prefix, empty, w/ whitespace); unknown
  `CF_*` identifier; mixed-case prologue.
- **Seeds**: round-trip zero bits `03 00 00 00 00`; all-ones `03 ff ff
  ff ff`; truncated varint `00 ff`; unknown ident `01 43 46 5f 55 4e 4b
  4e 4f 57 4e`.

### `daemon_greeting`

- **Current**: 1 seed (`@RSYNCD: 32.0\n`); only proto-32 path reached.
- **Gaps**: every supported version (28-32); subprotocol digit
  boundaries (`0`, `1`, max digits); digest-list variants (e.g.
  `@RSYNCD: 32 md5 sha512\n`); embedded NUL before `\n`; CRLF; truncated
  banner; UTF-8 boundary in digest list; oversized version number.
- **Seeds**: proto-30 w/ subprotocol `40 52 53 59 4e 43 44 3a 20 33 30
  2e 35 0a`; w/ digest list `40 52 53 59 4e 43 44 3a 20 33 32 20 6d 64
  35 20 73 68 61 35 31 32 0a`; truncated `40 52 53 59 4e 43 44 3a 20 33
  32`.

### `decompressor_zlib`

- **Current**: no seeds on disk.
- **Gaps**: empty deflate stream (fixed-Huffman, BFINAL=1); stored
  block at lengths 0, 1, 65535 (max LEN), 65536 (overflow);
  dynamic-Huffman block w/ literal and length codes; truncated
  mid-Huffman-table; zip-bomb candidate (>100x expansion to trip ratio
  assert); invalid block-type 3; premature EOF (BFINAL=0 + truncation).
- **Seeds**: empty fixed-Huffman `03 00`; empty stored block `01 00 00
  ff ff`; one-byte stored `A` `01 01 00 fe ff 41`; truncated header
  `78`.

### `decompressor_zstd`

- **Current**: no seeds on disk.
- **Gaps**: valid magic + empty frame; skippable frame (magic `50-5f 2a
  4d 18` + 4-byte size); block-size boundaries (1 B, 128 KiB max raw);
  multi-frame stream; truncation at every header offset; dictionary-ID
  flag w/ no dict content; window-size > 2 GiB; zip-bomb >100x
  expansion.
- **Seeds**: magic only `28 b5 2f fd`; empty frame `28 b5 2f fd 00 00
  00 00 00 00`; skippable `50 2a 4d 18 00 00 00 00`; invalid magic `00
  00 00 00 00 00 00 00`.

### `filter_differential`

- **Current**: no seeds on disk. Differential target depends on an
  external `rsync` binary; libFuzzer drives `Arbitrary` rule generation
  but seeds accelerate divergence-hunting.
- **Gaps**: anchored vs unanchored (`/foo` vs `foo`); directory-only
  (`foo/`); perishable (`-! pattern`); per-directory merge (`. .rsync-filter`);
  wildcard depth (`**/foo`, `foo/**/bar`); modifier stacks (`R`/`s`/
  `r`/`p`); empty vs all-exclude rule lists.
- **Seeds**: target consumes `Arbitrary` structs, not raw bytes; defer
  concrete byte hex to FCV-19 via `cargo fuzz tmin` over libFuzzer
  bootstrap discovery.

### `filter_rules_vs_upstream`

- **Current**: no seeds on disk. Same shape as `filter_differential`
  plus the `!` clear directive.
- **Gaps**: `!` at every list position (start/middle/end/duplicated);
  mixed `+`/`-`/`!` sequences across protocol-version boundaries;
  CR-terminated rule lines; empty pattern after prefix; tab-separated
  prefix/pattern.
- **Seeds**: see `filter_differential`; defer to FCV-19 mining.

### `flist_entry_decode`

- **Current**: no seeds on disk.
- **Gaps**: XMIT flag bit walk (each `XMIT_*` bit in isolation); RLE
  suffix lengths 0, 1, 255, 256 (1-byte vs 2-byte prefix boundary);
  zero-byte and 4096-byte maximum names; negative size/mtime varints;
  hardlink reference to an unseen index; ACL/xattr index reference w/o
  preceding definition; every mode-bit file type (REG/DIR/LNK/BLK/CHR/
  FIFO/SOCK); INC_RECURSE empty segment after `XMIT_TOP_DIR`.
- **Seeds**: empty flist `00`; regular file `a` (proto 28) `01 01 61 00
  00 00 00 00 00 00 00 00 a4 81 00 00`; symlink `l`->`t` `01 01 6c 00
  00 00 00 00 00 00 00 00 ff a1 00 00 01 74`.

### `incremental_flist`

- **Current**: 1 seed of 1 byte (`00`); empty state machine only.
- **Gaps**: out-of-order child entry (child before parent); orphan
  finalization (parent never sent); INC_RECURSE multi-segment w/
  `XMIT_TOP_DIR` boundaries; cycle in parent-dependency graph; max-depth
  parent chain; mixed dir/file/symlink entries in dependency order;
  empty segment followed by non-empty segment in INC_RECURSE mode.
- **Seeds**: two-entry tree `00 01 04 72 6f 6f 74 00 00 00 00 00 00 00
  00 00 ed 41 00 00 00`; orphan child `00 01 05 6f 72 70 68 2f 00 00 00
  00 00 00 00 00 00 a4 81 00 00 00`.

### `legacy_greeting`

- **Current**: 1 seed (`@RSYNCD: 32.0\n`).
- **Gaps**: same matrix as `daemon_greeting` (six entry points share
  the same grammar) plus invalid UTF-8 in digest list (byte parsers
  accept, string parsers reject); mixed-case `@rsyncd:`; leading
  whitespace; CR-only line ending.
- **Seeds**: mixed-case prefix `40 72 73 79 6e 63 64 3a 20 33 32 0a`;
  CR-only `40 52 53 59 4e 43 44 3a 20 33 32 0d`; invalid UTF-8 digest
  `40 52 53 59 4e 43 44 3a 20 33 32 20 ff fe 0a`.

### `multiplex_frame_parse`

- **Current**: no seeds on disk.
- **Gaps**: each `MSG_*` code (DATA, ERROR_XFER, INFO, ERROR, WARNING,
  LOG, CLIENT, STATS, DELETED, NO_SEND, DONE, FLIST, NOOP,
  ERROR_SOCKET, ERROR_UTF8, IO_ERROR, DEL_STATS); tag byte below
  `MPLEX_BASE`; tag byte above max known code; payload-length boundary
  0/1/4/`i32::MAX`/`u32::MAX`; multi-frame stream w/ zero-byte payloads;
  header followed by short payload.
- **Seeds**: `MSG_DATA` empty `00 00 00 07`; `MSG_ERROR_XFER` 4-byte `04
  00 00 08 de ad be ef`; below-base tag `00 00 00 06`; oversize payload
  announcement `ff ff ff 0f`.

### `ndx_codec`

- **Current**: 1 seed (`01 00 00 00`, single positive NDX).
- **Gaps**: multi-byte 0xFE prefix (legacy 4-byte extension); 0xFF
  prefix (modern signed-extension marker); negative NDX (negative
  accumulator); delta-encoded sequence crossing positive/negative
  boundary; zero NDX (end-of-list); truncation mid-extension at
  0xFE/0xFF; `i32::MIN`/`i32::MAX`.
- **Seeds**: zero NDX `00`; -1 `ff ff ff ff`; 0xFE extension `fe 00 00
  00 01`; truncated 0xFF `ff 01`.

### `protocol_wire`

- **Current**: no seeds on disk.
- **Gaps**: same matrix as `multiplex_frame_parse` (this target also
  walks `BorrowedMessageFrames`) plus empty stream; sub-header stream
  (3 bytes); two valid frames back-to-back; valid frame + garbage;
  garbage + valid frame.
- **Seeds**: empty (zero bytes); single valid empty frame `00 00 00
  07`; two empty frames `00 00 00 07 00 00 00 07`; truncated `00 00
  00`.

### `rsyncd_conf`

- **Current**: 1 seed of 132 bytes (one minimal valid `[public]` module
  config).
- **Gaps**: empty file; comment-only file; global-only parameters (no
  `[module]`); multiple modules w/ overlapping names; boolean edges
  (`yes`/`no`/`true`/`false`/`1`/`0` plus invalid); list parameter w/
  leading/trailing/empty commas; CRLF; UTF-8 boundary in module name;
  section header w/o closing bracket; key/value split on tab vs space
  vs `=`.
- **Seeds**: empty (zero bytes); comment-only `23 20 68 65 6c 6c 6f
  0a`; broken section header `5b 62 72 6f 6b 65 6e 0a`; CRLF `70 6f 72
  74 20 3d 20 38 37 33 0d 0a`.

### `simd_checksum_parity`

- **Current**: no seeds on disk.
- **Gaps**: SIMD lane boundary lengths 4/8/16/32/64 (SSE2/AVX2/AVX-512
  widths); lane-boundary+1 lengths 5/9/17/33/65 for remainder handling;
  MD4/MD5 block boundary (64 B) plus partial second block; all-zero
  input; all-`0xFF` input (rolling sum overflow); sawtooth `0x00 0x01
  0x02 ...` for stripe misalignment; empty input.
- **Seeds**: empty (zero bytes); 64 zeros (`00`x64); 65 `0xFF`
  (`ff`x65); 33-byte sawtooth `00 01 02 ... 20`.

### `varint_decode`

- **Current**: 1 seed of 33 bytes.
- **Gaps**: boundary varint values 0/1/`i32::MAX-1`/`i32::MAX`/
  `i32::MIN`/-1; boundary varlong values plus `i64::MAX`/`i64::MIN`;
  `min_bytes` at every value 1..=8; truncated varint at every
  continuation byte; oversize varint (more bytes than spec); negative
  `int_value` in `write_int`; round-trip parity at the proto-30 varint30
  boundary.
- **Seeds**: zero `00`; truncated continuation `ff ff ff`; 5-byte
  max-positive `f0 ff ff ff 7f`; negative int `ff ff ff ff`.

### `vstring`

- **Current**: 1 seed (`06 03 6d 64 35`, selector + 3-byte `"md5"`).
- **Gaps**: empty vstring; single-byte length boundary 0/1/127/128
  (1-byte vs 2-byte prefix switch); two-byte length boundary
  256/16383/16384/`u16::MAX`; embedded NUL inside body; invalid UTF-8
  payload; truncated length prefix; selector matrix coverage across
  every proto 28-32 x do_negotiation x send_compression x is_daemon x
  is_server.
- **Seeds**: empty `00 00`; 128-byte length boundary `00 80 01 41`;
  invalid UTF-8 `00 02 ff fe`; legacy proto arm `03 00`.

## Gap aggregation

| Gap category | Targets affected |
|--------------|------------------|
| **Boundary lengths (0, 1, MAX-1, MAX, MAX+1)** | every target except `simd_checksum_parity` (where lane-boundary lengths play the same role) |
| **Embedded NUL** | `acl_xattr_wire`, `auth_response`, `bwlimit`, `daemon_greeting`, `legacy_greeting`, `rsyncd_conf`, `vstring` |
| **UTF-8 boundary characters** | `auth_response`, `bwlimit`, `daemon_greeting`, `legacy_greeting`, `rsyncd_conf`, `vstring`, `flist_entry_decode` |
| **Truncation mid-field** | every parser target |
| **Maximum nesting / recursion** | `acl_xattr_wire`, `flist_entry_decode`, `incremental_flist`, `rsyncd_conf`, `filter_differential`, `filter_rules_vs_upstream` |
| **Negative integers** | `varint_decode`, `bwlimit`, `ndx_codec`, `flist_entry_decode`, `batch_reader` |
| **Zero-length strings / lists** | `acl_xattr_wire`, `filter_list_wire`, `flist_entry_decode`, `incremental_flist`, `rsyncd_conf`, `vstring` |
| **Protocol version branch** (28 vs 30+) | `filter_list_wire`, `flist_entry_decode`, `incremental_flist`, `ndx_codec`, `varint_decode`, `vstring`, `batch_reader` |

The largest single gap is **truncation mid-field**: every parser
target accepts an arbitrary byte stream and must surface truncation as
`io::Result::Err` rather than panic, but no corpus directory currently
seeds the truncation matrix. FCV-19 should prioritise generating one
truncation seed per field boundary in each parser, derived from the
valid seeds proposed above by lopping off the trailing bytes one at a
time.

## Recommendations for FCV-19

1. Commit the concrete byte sequences listed above as
   `fuzz/corpus/<target>/seed_<descriptor>` files.
2. For differential filter targets, bootstrap via libFuzzer
   `-runs=10000`, minimise with `cargo fuzz tmin`, then commit.
3. For decompressors, capture 50-100 fixtures from upstream test
   suites (`zlib/test/*.gz`, `zstd/tests/`) and minimise.
4. Re-run the FCV-17 inventory after FCV-19 lands. Target >= 20 seeds
   per non-differential target.
5. Wire `cargo fuzz coverage` (FCV-20) as the verification metric and
   gate it in CI once a per-target line-coverage baseline is in place.
