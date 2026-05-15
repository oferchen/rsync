# Compressed-stream token decoder parity vs rsync 3.4.2

Tracks task #2225. Audits oc-rsync's compressed-token wire decoder
against the fix introduced in upstream rsync 3.4.2 for the
compressed-stream token decoder: "Reject negative token values in the
compressed-stream token decoder; a negative value could cause callers
to misinterpret a missing data pointer as literal data."

## 1. Upstream 3.4.2 change

The fix lives in `token.c`. In 3.4.1 the absolute-token branch read a
32-bit token from the wire and returned `-1 - rx_token` to the caller
without validating the sign:

```c
/* 3.4.1 token.c:589-593 (recv_deflated_token) */
if (flag & TOKEN_REL) {
    rx_token += flag & 0x3f;
    flag >>= 6;
} else
    rx_token = read_int(f);
```

3.4.2 adds an explicit bounds check in all three deflated-stream
receivers (`recv_deflated_token` for `CPRES_ZLIB`,
`recv_compressed_token` for `CPRES_ZLIBX`/`SUPPORT_LZ4`, and
`recv_zstd_token` for `CPRES_ZSTD`). The same guard is repeated at each
site:

```c
/* 3.4.2 token.c:589-599 (recv_deflated_token); matching guards at
 * token.c:843-848 (recv_compressed_token) and
 * token.c:1012-1017 (recv_zstd_token). */
if (flag & TOKEN_REL) {
    rx_token += flag & 0x3f;
    flag >>= 6;
} else {
    rx_token = read_int(f);
    if (rx_token < 0) {
        rprintf(FERROR, "invalid token number in compressed stream\n");
        exit_cleanup(RERR_PROTOCOL);
    }
}
```

Failure path: `RERR_PROTOCOL` (exit code 12) with a fixed message on
`FERROR`. The check runs before `recv_*_token` returns, so the caller
in `receiver.c:315` never sees the malformed value.

Why the misinterpretation hazard exists upstream: `recv_token()`
multiplexes one `int32` return for three states. A positive return is
literal length, with `*data` pointing into the per-call decompress
buffer; a negative return is a block match (`-1 - rx_token`); zero is
EOF. If a peer sends `rx_token = -1`, the function returns
`-1 - (-1) = 0`, terminating the receive loop with the file partially
filled. If a peer sends `rx_token = -N` for `N >= 2`, the function
returns `N - 1`, which the receiver treats as a literal length and
copies `N - 1` bytes from whatever `*data` happens to reference (the
last block-match output pointer, freed buffer, or `NULL` depending on
state).

## 2. oc-rsync decoder sites audited

oc-rsync uses a tagged enum (`CompressedToken::{Literal, BlockMatch,
End}`) instead of upstream's signed-`int32` multiplexing, so the
"missing data pointer as literal data" misinterpretation cannot occur
by construction. The block-index field is still derived from a wire
`i32`, however, and an attacker-controlled negative wire value silently
wraps to a large `u32` block index.

### 2.1 `crates/protocol/src/wire/compressed_token/zlib_codec.rs:413`

`ZlibTokenDecoder::recv_token`, `TOKEN_LONG` branch. Reads 4 bytes,
decodes as `i32`, stores in `self.rx_token`, returns
`CompressedToken::BlockMatch(self.rx_token as u32)`. No sign check on
the wire value before the cast.

**Verdict: NEEDS FIX.** A negative wire value wraps to a large `u32`
block index. Downstream consumers
(`transfer/src/transfer_ops/token_loop.rs:157`,
`transfer/src/transfer_ops/response.rs:214`,
`transfer/src/receiver/transfer.rs:337`) catch out-of-range indices,
so memory safety is preserved. However, when the absolute token is
followed by a `TOKENRUN_LONG` run count, the decoder pre-increments
`self.rx_token` on each subsequent `BlockMatch` emission
(`zlib_codec.rs:315`). After enough increments, a negative starting
token wraps back into the valid block-index range, the bounds check
passes, and the receiver copies the wrong basis block. The final
file-level checksum still fails, so the attack is limited to forcing
re-transfers and corrupting partial output, but the decoder loses
upstream parity and the error appears at a later layer with a less
specific message.

### 2.2 `crates/protocol/src/wire/compressed_token/zstd_codec.rs:454`

`ZstdTokenDecoder::recv_token`, `TOKEN_LONG` branch. Identical shape
and identical hazard to 2.1. **Verdict: NEEDS FIX.**

### 2.3 `crates/protocol/src/wire/compressed_token/lz4_codec.rs:340`

`Lz4TokenDecoder::recv_token`, `TOKEN_LONG` branch. Identical shape
and identical hazard to 2.1. **Verdict: NEEDS FIX.**

### 2.4 `crates/transfer/src/token_reader.rs:130`

`PlainTokenReader::read_token` for the uncompressed path. Branches on
`token.cmp(&0)` explicitly; `Less` is interpreted as a block match
(`-(token + 1) as usize`), matching upstream's `recv_token` return
convention. There is no multiplexed pointer here - the caller receives
a typed `DeltaToken::BlockRef(usize)` and goes through the same
bounds-checking consumers. **Verdict: SAFE.**

### 2.5 `crates/protocol/src/wire/delta/token.rs:208`

`read_token` for plain wire delta. Returns `Option<i32>` and leaves
interpretation to the caller; no pointer multiplexing. **Verdict:
SAFE.**

### 2.6 `TOKEN_REL` accumulator branches

`zlib_codec.rs:402`, `zstd_codec.rs:443`, `lz4_codec.rs:329` accumulate
`rx_token += rel` where `rel` is the low 6 bits of the flag byte
(range 0..=63). The accumulator can only become negative after the
absolute-token branch has stored a malformed value. Adding the
absolute-token guard (2.1-2.3) is sufficient to close this path.

## 3. Divergence summary

| Site | Negative wire value rejected? |
|------|-------------------------------|
| upstream 3.4.1 `recv_*_token` | No |
| upstream 3.4.2 `recv_*_token` | Yes (`RERR_PROTOCOL`) |
| oc-rsync compressed decoders (before #2225) | No |
| oc-rsync compressed decoders (after #2225) | Yes (`io::ErrorKind::InvalidData`) |
| oc-rsync plain `PlainTokenReader::read_token` | N/A (explicit signed dispatch, no aliasing) |

## 4. Remediation in this PR

All three compressed codecs gain a sign check immediately after
`i32::from_le_bytes`, mirroring upstream 3.4.2:

```rust
self.rx_token = i32::from_le_bytes(buf);
if self.rx_token < 0 {
    return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid token number in compressed stream",
    ));
}
```

The message text matches upstream's `rprintf(FERROR, ...)` string
exactly so wire-protocol tests can grep for it across implementations.
The Rust error converts to exit code 12 (`RERR_PROTOCOL`) through the
existing `io::Error` -> `EngineError` mapping, matching upstream's
`exit_cleanup(RERR_PROTOCOL)` semantics.

A single regression test
(`crates/protocol/src/wire/compressed_token/tests.rs`) exercises the
new guard for zlib, zstd, and lz4 by feeding a hand-built byte sequence
that places `-1i32` after `TOKEN_LONG` and asserting the decoder
returns `InvalidData` with the upstream-compatible message.
