# UTS-18.k - Zlib codec and basis-window invariants

This document records the wire-byte and runtime invariants that govern the
delta-transfer codec stack: the upstream-compatible zlib token decoder
(`crates/protocol/src/wire/compressed_token/zlib_codec.rs`), the basis-window
mapper that backs every COPY token (`crates/transfer/src/map_file/buffered.rs`),
and the streaming raw-deflate encoder/decoder primitives in
`crates/compress/src/zlib/`. The invariants below are load-bearing for the
panic-freedom guarantees the receiver pipeline depends on; any change to the
codec, mapper, or compress crate that relaxes one of these invariants must be
accompanied by a new regression test in the same module.

Tracked under the UTS-18 series. Closes the documentation gap left open by
PRs #5566 / #5569 / #5588 (BufferedMap fail-loud, window clamp, never-shrink)
and PR #5535 / URV-1 (zlib obuf overflow on matched-block inserts).

## 1. Wire-byte invariants

The delta wire stream is a sequence of token frames. Each token references
either literal bytes (the sender's INSERT) or a position in the receiver's
basis file (the sender's COPY). The codec carries the literal payload; the
basis mapper materialises COPY ranges out of the receiver's local file. Both
sides share a single contract: the offsets the wire commits to must be
addressable, and any out-of-range request must surface as a typed
`io::Error`, never an abort.

### 1.1 INSERT tokens

- INSERT tokens carry literal bytes inline. For zlib mode, the literal is
  delivered as one or more `DEFLATED_DATA` frames whose 14-bit length field
  is bounded by `MAX_DATA_COUNT = 16383` per upstream `token.c:338`.
- A receiver must accept consecutive `DEFLATED_DATA` frames until a
  non-DEFLATED flag byte appears. The accumulator that joins them MUST cap
  the aggregate length so an unbounded chain cannot drive the decoder to
  OOM. The cap lives at
  `crates/protocol/src/wire/compressed_token/zlib_codec.rs::MAX_ACCUMULATED_COMPRESSED_BYTES`
  (64 MiB). Upstream's defence-in-depth equivalent was added in 3.4.3.
- The literal length the receiver writes out of `recv_token` is bounded by
  `CHUNK_SIZE`. Any larger decompressed run must be returned across
  multiple `CompressedToken::Literal` calls so a single allocation never
  exceeds `CHUNK_SIZE`.

### 1.2 COPY tokens and the basis window

- COPY tokens carry a 32-bit block index that the receiver translates into
  a `(offset, len)` request against the basis-file mapper. The mapper is
  `BufferedMap` in `crates/transfer/src/map_file/buffered.rs`.
- The basis window covers `[window_start, window_start + window_len)`. A
  COPY request `(offset, len)` is in-window iff
  `offset >= window_start && offset + len <= window_start + window_len`.
  Out-of-window requests trigger `load_window`.
- `window_len` is clamped at the single assignment site in `load_window`
  to `min(MAX_MAP_SIZE, file_size - window_start)`. The clamp is the only
  legitimate path that produces a window smaller than the configured
  maximum. This is the UTS-18.g.2 / PR #5569 invariant.
- `MAX_MAP_SIZE = 256 KiB` (mirrors upstream `MAX_MAP_SIZE` in
  `crates/transfer/src/constants.rs`). The window is aligned down to
  `MIN_MAP_SIZE = 1024` byte boundaries via `align_down()` so partial
  blocks at the head of the buffer remain addressable.
- Any COPY request that would address `offset + len > file_size` returns
  `io::ErrorKind::UnexpectedEof` from `map_ptr` without ever indexing into
  the buffer slice. This is the UTS-18.f / PR #5566 invariant.

### 1.3 END marker

- The encoder writes the `END_FLAG` byte (`0x00`) after the final token
  run. The decoder treats `END_FLAG` as a clean stream terminator and
  returns `CompressedToken::End`.
- A stream that ends mid-frame (no `END_FLAG`, or `END_FLAG` arriving while
  the run counter is non-zero) MUST surface an `io::Error` from
  `read_exact`. The decoder never silently terminates with pending state.

## 2. Runtime invariants

### 2.1 BufferedMap: fail-loud bounds (UTS-18.f / PR #5566)

- `map_ptr(offset, len)` returns `io::ErrorKind::UnexpectedEof` when
  `offset + len > file_size`. Saturation arithmetic on `offset + len`
  prevents the bounds-check itself from wrapping.
- `len == 0` is short-circuited to `Ok(&[])` before any range arithmetic
  so a zero-length call cannot underflow the slice index.
- Inside `load_window`, if the clamped `window_size` is still smaller than
  `offset_in_window + min_len`, the function returns
  `io::ErrorKind::UnexpectedEof` BEFORE the subsequent `copy_within` /
  `read_exact` calls. The two slice indexing operations that previously
  panicked (`copy_within` at the resize site and `&buffer[start..end]` in
  `map_ptr`) are now reachable only when the bounds check has succeeded.

### 2.2 BufferedMap: window clamp at the source (UTS-18.g.2 / PR #5569)

- `window_size` is computed once at the top of `load_window` as
  `(self.max_window as u64).min(remaining) as usize` where `remaining =
  size - aligned_start`. This is the only place `window_len` is derived.
  No downstream code in the receiver should re-clamp `window_len`; doing
  so would mask future regressions in the source clamp.
- The clamp keeps a small file (e.g., 48128 bytes) paired with the default
  `MAX_MAP_SIZE = 262144` window from generating a `window_len` larger
  than the file, which previously triggered the `copy_within` panic the
  guards in 2.1 now catch.

### 2.3 BufferedMap: monotonic buffer length (UTS-18.h / PR #5588)

- `self.buffer` grows but never shrinks during a session. The
  reload paths in `load_window` use `target_len = window_size.max(buffer.len())`
  so a reload that requests a smaller window than the prior allocation
  cannot truncate the bytes the overlap branch is about to relocate via
  `copy_within`. This mirrors upstream `fileio.c:236` `realloc_array`,
  which only grows the backing buffer.
- `window_len` continues to bound the valid region. Callers only read
  `&buffer[start..start + len]` after the `is_in_window` / `load_window`
  bounds checks, so a buffer that is allocated larger than `window_len`
  cannot leak stale tail bytes to a caller.

### 2.4 Zlib token decoder: counter and accumulator bounds (URV-1 / PR #5535 + 3.4.3 defence-in-depth)

- `rx_token` increments use `checked_add` for both the `TOKEN_REL` /
  `TOKEN_LONG` literal-flag path and the run-tail emission loop. A
  malicious sender that drives `rx_token` past `i32::MAX` receives
  `io::ErrorKind::InvalidData`; the decoder never wraps into the valid
  block-index range.
- Absolute `TOKEN_LONG` values that decode to negative `i32` (high bit
  set) are rejected with `io::ErrorKind::InvalidData`. This prevents a
  block index that wraps from `u32::MAX` back into the receiver's basis
  range, mirroring upstream's 3.4.2 hardening.
- Accumulated `DEFLATED_DATA` runs are capped at
  `MAX_ACCUMULATED_COMPRESSED_BYTES = 64 MiB` per token call. Hostile
  peers cannot drive the decoder to allocate without bound.

### 2.5 Zlib token encoder: matched-block insert obuf safety (URV-1 / PR #5535)

- `see_token` (the dictionary-sync path for CPRES_ZLIB) loops the
  underlying `compress()` call until the input chunk is fully consumed.
  Single-shot calls could leave a worst-case 0xFFFF-byte incompressible
  insert with unconsumed tail bytes, silently corrupting the receiver's
  dictionary on the next literal pass. The fix mirrors upstream's
  `send_deflated_token` flush handling at `token.c:367`.
- The encoder's `compress_buf` is sized for the worst-case stored-block
  expansion (`AVAIL_OUT_SIZE(0xFFFF) + 16`) so a single matched insert
  never trips `Z_BUF_ERROR` mid-stream.

## 3. Failure modes locked by regression tests

| Failure mode                                        | Regression site                                                                                                | PR    |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | ----- |
| `copy_within` panic on shrink-truncated buffer      | `crates/transfer/src/map_file/tests.rs::load_window_overlap_shrink_preserves_data`                              | #5588 |
| Slice index past EOF in `map_ptr`                   | `crates/transfer/src/map_file/tests.rs` (map_ptr out-of-range tests)                                            | #5566 |
| `window_len > file_size` from unclamped reload      | `crates/transfer/src/map_file/tests.rs` (window-clamp test)                                                     | #5569 |
| BufferedMap panic-freedom under arbitrary input     | `fuzz/fuzz_targets/buffered_map.rs` with `fuzz/corpus/buffered_map/`                                            | #5590 |
| Matched-block insert >= obuf size corrupts stream   | `crates/protocol/src/wire/compressed_token/tests.rs::see_token_large_incompressible_insert_no_overflow`         | #5535 |
| Decoder token-index overflow on hostile run         | `crates/protocol/src/wire/compressed_token/tests.rs` (`rx_token` overflow tests, 3.4.3 cap)                     | -     |
| Accumulated DEFLATED_DATA exceeds 64 MiB cap        | `crates/protocol/src/wire/compressed_token/tests.rs` (accumulator cap test, 3.4.3 cap)                          | -     |
| Streaming zlib decoder panic on arbitrary bytes     | `fuzz/fuzz_targets/decompressor_zlib.rs` + (new) `fuzz/fuzz_targets/zlib_token_decode.rs`                       | -     |
| Streaming zlib decoder panic on truncated header    | `crates/compress/src/zlib/tests.rs::malformed_deflate_header_returns_err`                                       | UTS-18.k |
| Streaming zlib decoder panic on truncated payload   | `crates/compress/src/zlib/tests.rs::truncated_payload_returns_err`                                              | UTS-18.k |

Every row in the table represents a concrete invariant covered by a single
named test. Future regressions surface as a named test failure rather than a
crash report from production.

## 4. Fuzz target rationale

The wire codec stack has two complementary fuzz targets:

1. `fuzz/fuzz_targets/buffered_map.rs` (PR #5590): drives `BufferedMap`
   through `MapStrategy::map_ptr` with arbitrary `(file_size, window_size,
   request_start, request_len)` parameter combinations. Locks the UTS-18.f /
   UTS-18.g / UTS-18.h panic class. Corpus seeds live under
   `fuzz/corpus/buffered_map/` and are documented in
   `docs/audits/edg-panic-4-buffered-fuzz.md`.

2. `fuzz/fuzz_targets/zlib_token_decode.rs` (new in this PR): drives the
   per-token zlib codec layer one level above `CountingZlibDecoder`. The
   target feeds arbitrary byte sequences to the streaming raw-deflate
   decoder and asserts panic-freedom across malformed `DEFLATED_DATA`
   headers, truncated payloads, and arbitrary flag-byte combinations.
   Complements `fuzz/fuzz_targets/decompressor_zlib.rs`, which covers the
   one-shot `decompress_to_vec` path and the streaming decoder's
   expansion-ratio guard, by exercising the same decoder under streaming
   read semantics.

Adversarial inputs from the upstream `compress-zlib-insert` testsuite
(matched-block inserts up to 64 KiB of incompressible bytes) must not panic
either fuzz target. The encoder-side counterpart is locked by the
`see_token_large_incompressible_insert_no_overflow` regression test
referenced in the table above.

## 5. Cross-references

- `crates/transfer/src/map_file/buffered.rs` - basis-window mapper.
- `crates/transfer/src/constants.rs` - `MAX_MAP_SIZE`, `MIN_MAP_SIZE`,
  `align_down()`.
- `crates/protocol/src/wire/compressed_token/zlib_codec.rs` - zlib token
  encoder/decoder, `MAX_ACCUMULATED_COMPRESSED_BYTES`.
- `crates/compress/src/zlib/` - streaming raw-deflate primitives
  (`CountingZlibEncoder`, `CountingZlibDecoder`, `compress_to_vec`,
  `decompress_to_vec`).
- `target/interop/upstream-src/rsync-3.4.4/token.c` - upstream
  `simple_recv_token`, `recv_deflated_token`, `send_deflated_token`.
- `target/interop/upstream-src/rsync-3.4.4/fileio.c` - upstream `map_ptr`
  / `realloc_array`.
- `docs/audits/edg-panic-4-buffered-fuzz.md` - EDG-PANIC.4 BufferedMap
  fuzz target and corpus rationale.
- `docs/audits/edg-panic-5-unwrap-on-slice.md` - sibling audit covering
  the broader `.unwrap()` / `.expect()` slice-driven panic class.
