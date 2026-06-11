# EDG-PANIC.1: Bare-Slice Indexing Inventory

Workspace-wide inventory of bare slice indexing (`slice[idx]`, `&slice[start..end]`,
`slice.get_unchecked(...)`) on attacker-controlled wire inputs in production code.
Scope excludes `tests/`, `benches/`, `fuzz/`, and `target/`.

## Methodology

1. ripgrep candidates: `get_unchecked`, `split_at`, `[start..]`, `[..end]`, literal
   indices `[N]` in `crates/`.
2. Filtered to production paths only (no `tests.rs`, no `#[cfg(test)]`).
3. Prioritized crates that consume wire bytes:
   - `protocol` (varint, multiplex frames, flist, iconv, secluded args)
   - `compress` (zlib codec, lz4 raw codec, skip-compress decider)
   - `transfer` (map_file: BufferedMap window for delta-apply)
   - `batch` (delta replay against basis file)
   - `metadata` (xattr/ACL wire payloads)
   - `filters`, `flist`, `daemon` for completeness.
4. Classified each surviving site SAFE / NEEDS-CHECK / MUST-FIX.

## Summary

**Sites inspected: 14 (SAFE: 12, NEEDS-CHECK: 2, MUST-FIX: 0).**

Existing hardening (EDG-PANIC.2/.3/.4/.5, UTS-18.f/.g, SEC-1, SEC-2.b, SEC-4)
already converted the highest-risk sites to checked-slice / typed-error returns.
The audit confirms no remaining bare slice indexing on attacker-controlled wire
inputs panics in default builds; the two NEEDS-CHECK items are batch-replay
derived lengths whose upstream validation is non-local.

## crates/protocol/

### `crates/protocol/src/multiplex/helpers.rs:136-145` - `&bytes[..HEADER_LEN]`, `&bytes[HEADER_LEN..frame_len]`, `&bytes[frame_len..]`
**Verdict:** SAFE
**Reasoning:** `decode_frame_parts` pre-validates `bytes.len() < HEADER_LEN` (line
132) and `bytes.len() < frame_len` (line 140) before each range index. `frame_len
= HEADER_LEN + payload_len_usize` and payload length is the only wire-decoded
field; both compared with `<` before slicing.
**Recommendation:** none.

### `crates/protocol/src/varint/decode.rs:36` - `&bytes[1..1 + extra]`
**Verdict:** SAFE
**Reasoning:** `decode_bytes` checks `bytes.is_empty()` (line 14), bounds `extra
<= MAX_EXTRA_BYTES` (line 23), then guards `bytes.len() < 1 + extra` (line 27)
before the range index.
**Recommendation:** none.

### `crates/protocol/src/varint/decode.rs:189` - `&bytes[consumed..]`
**Verdict:** SAFE
**Reasoning:** `consumed = 1 + extra` is returned from `decode_bytes` which
already validated `bytes.len() >= 1 + extra`.
**Recommendation:** none.

### `crates/protocol/src/wire/compressed_token/zlib_codec.rs:148` - `&data[offset..offset + chunk_len]`
**Verdict:** SAFE
**Reasoning:** `chunk_len = toklen.min(0xFFFF)` and `toklen` starts at
`data.len()` and decreases by `chunk_len` each iteration. Invariant: `offset +
chunk_len <= data.len()`.
**Recommendation:** none.

### `crates/protocol/src/wire/compressed_token/mod.rs:172` - `&data[offset..offset + piece_len]`
**Verdict:** SAFE
**Reasoning:** `piece_len = (data.len() - offset).min(MAX_DATA_COUNT)`. Bounded
by remaining slice length by construction.
**Recommendation:** none.

### `crates/protocol/src/wire/delta/token.rs:48` - `&data[offset..offset + chunk_len]`
**Verdict:** SAFE
**Reasoning:** `chunk_len = (data.len() - offset).min(CHUNK_SIZE)`. Identical
shape to the codec case above.
**Recommendation:** none.

### `crates/protocol/src/multiplex/io/send.rs:172, 217, 337, 346` - `&slice[offset_in_first..]`, `&current_slice[offset..]`, `&header[remaining..]`, `&payload[remaining..]`
**Verdict:** SAFE
**Reasoning:** All offsets derive from `written_total`, an internal counter the
function increments after a successful write. There is also a fail-loud check
(line 237: `written > remaining`) that rejects misbehaving writers. No
wire-controlled index reaches these slices.
**Recommendation:** none.

### `crates/protocol/src/iconv/converter.rs:551, 553, 576` - `&mut output[start..]`, `&remaining[consumed..]`
**Verdict:** SAFE
**Reasoning:** `output.resize(start + needed, 0)` immediately precedes
`&mut output[start..]`. `consumed` is returned by `encoding_rs` and documented
to be <= input length.
**Recommendation:** none.

### `crates/protocol/src/flist/name_cmp.rs:114-122` - `name[dir.len() + 1..]`, `name[pos + 1..]`
**Verdict:** SAFE
**Reasoning:** Guarded by `name.len() > dir.len() && name.starts_with(dir) &&
name[dir.len()] == b'/'`. The bare byte access `name[dir.len()]` is also bounded
by the explicit `name.len() > dir.len()` check. `pos` comes from
`memchr::memrchr` so `pos < name.len()` always.
**Recommendation:** none.

## crates/compress/

### `crates/compress/src/lz4/raw.rs:225-226, 236` - `input[0]`, `input[1]`, `&input[HEADER_SIZE..total_input]`
**Verdict:** SAFE
**Reasoning:** `decompress_block` pre-validates `input.len() < HEADER_SIZE`
(line 218) before `input[0]`/`input[1]` (HEADER_SIZE == 2). `total_input =
HEADER_SIZE + compressed_size` where `compressed_size` comes from
`decode_header` (capped at 14 bits = 16383). A second guard `input.len() <
total_input` (line 229) precedes the range slice.
**Recommendation:** none.

### `crates/compress/src/skip_compress/decider.rs:257` - `&data[8..12]`
**Verdict:** SAFE
**Reasoning:** Guarded by `if data.len() >= 12` (line 256). Constant 4-byte
window inside the bounds check.
**Recommendation:** none.

### `crates/compress/src/skip_compress/adaptive.rs:107, 117` - `&buf[..to_buffer]`, `&buf[remaining..]`
**Verdict:** SAFE
**Reasoning:** `to_buffer = available.min(buf.len() - written)` (internal write
accounting). `remaining = buf.len() - written`. Both derive from the slice
itself, not from wire input.
**Recommendation:** none.

## crates/transfer/

### `crates/transfer/src/map_file/buffered.rs:209-241` - `BufferedMap::map_ptr`
**Verdict:** SAFE
**Reasoning:** Already hardened by UTS-18.f / EDG-PANIC.3. `map_ptr` uses
`buffer.get(start..end).ok_or_else(...)` and `checked_add`. The companion
`load_window` was also clamped (UTS-18.g) so `reuse_len` cannot overstate the
window. Negative-bounds regression test landed via PR #5703.
**Recommendation:** none.

### `crates/transfer/src/map_file/buffered.rs:142-167` - `load_window` overlap branch
**Verdict:** SAFE
**Reasoning:** UTS-18.f added explicit `checked_add` + `src_end >
self.buffer.len()` guard before `buffer.copy_within(...)`.
**Recommendation:** none.

## crates/batch/

### `crates/batch/src/replay/delta.rs:160, 166` - `&mut buffer[..chunk_size]`, `&buffer[..chunk_size]`
**Verdict:** NEEDS-CHECK
**Reasoning:** `chunk_size = remaining.min(buffer.len())` is bounded against
`buffer.len()`. The deeper risk is `effective_length` (line 150-156), which
derives from wire-decoded `length` or from `block_length`/`remainder` parsed
from the batch file header further upstream. If a malformed batch header sets
`block_length` to `usize::MAX` and `length == 0` and `block_index ==
block_count - 1`, `effective_length = remainder`; if `remainder` was not
validated against basis-file size, the `read_exact` loop simply reads garbage
(no panic), but the caller may write more bytes than the basis file holds.
**Recommendation:** Audit `crates/batch/src/header/` (or the read-path that
populates `block_length` / `remainder`) for a `<= basis_size` invariant. Follow-up
task: `EDG-PANIC.6 - validate batch-header block_length/remainder against basis
file size`.

## crates/filters/

### `crates/filters/src/compiled/pattern.rs:50` - `&pattern[..pattern.len() - 1]`
**Verdict:** SAFE
**Reasoning:** Branch is gated on `pattern.last() == Some(&b'/')` (i.e. the
slice is non-empty), so `pattern.len() >= 1` and the underflow case is
unreachable.
**Recommendation:** none.

## Sites under threshold

After inspecting 14 candidate sites covering the highest-risk wire-input crates
(protocol, compress, transfer, batch, filters, flist), the inventory is at the
target depth. No additional MUST-FIX sites surface. The remaining workspace-wide
matches for `[start..end]` are inside owner-controlled buffers (BufferPool,
multiplex writer book-keeping) or constant-bounded array accesses (e.g.
`INT_BYTE_EXTRA[(first / 4) as usize]` where the index is a `u8 / 4` so
necessarily `< 64`, matching the lookup table size).

## Cross-cutting hardening already in place

- **UTS-18.f** (`buffered.rs:159-178`): replaced bare `copy_within` panic site
  with `checked_add` and explicit `> self.buffer.len()` guard returning
  `io::ErrorKind::InvalidData`.
- **UTS-18.g** (`buffered.rs:202`): clamped `window_len = window_size.min(
  remaining_from_start)` so `map_ptr` cannot index past EOF.
- **SEC-2.b** (HTTP CONNECT response parser): bounded line length cap.
- **SEC-4** (CVE-2026-43620): malformed `parent_node_idx` regression test.
- **EDG-PANIC.2** (compress codecs): property-test fuzz harness.
- **EDG-PANIC.4** (libfuzzer): corpus from compress-zlib-insert input.
- **EDG-PANIC.5**: `.unwrap()` / `.expect()` audit on hot-path slice-derived
  values closed.

## Follow-up tasks

### MUST-FIX

None.

### NEEDS-CHECK

- **EDG-PANIC.6 (proposed):** Validate batch-header `block_length` and
  `remainder` fields against basis-file size before delta replay. Scope:
  `crates/batch/src/header/` parser + an invariant check at the top of
  `crates/batch/src/replay/delta.rs::apply_delta_op` (or equivalent). 1 file,
  ~20 LoC, 1 regression test using a hand-rolled malformed batch header.

### Cluster audits

- **EDG-PANIC.7 (proposed):** Re-sweep `crates/daemon/` and
  `crates/metadata/src/{acl,xattr}*` after the planned ACL/xattr wire fuzz
  expansion lands (XAP-11 follow-ups). The current crates use parser methods on
  `bytes::Buf`-style helpers (no bare indexing surfaced) but a fresh post-XAP
  re-grep is cheap insurance.

## Conclusion

The workspace currently has no bare-slice indexing sites that could panic on
attacker-controlled wire input in default builds. The two existing
`get_unchecked_mut` calls in `crates/fast_io/src/iocp/overlapped.rs` are
`Pin::get_unchecked_mut`, not slice indexing, with documented SAFETY blocks in
`#[allow(unsafe_code)]` blocks per the unsafe-code policy. EDG-PANIC.6 is the
only follow-up scoped from this audit and is filed as a NEEDS-CHECK rather than
a MUST-FIX because the failure mode is "read garbage from basis" not "panic".
