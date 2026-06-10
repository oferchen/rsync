# EDG-PANIC.1 - Bare Slice Indexing on Attacker-Controlled Inputs

Workspace grep audit of bare slice indexing patterns (`buf[i]`, `&buf[..n]`,
`&buf[a..b]`, `copy_within(a..b, _)`) on buffers whose offsets or lengths are
read from the wire. Scope: `crates/protocol/`, `crates/transfer/`,
`crates/checksums/`, `crates/compress/`, with focus on parsing paths the
sender/daemon cannot influence safely.

## Methodology

Greps used:

```sh
grep -rn 'buf\[' crates/ --include='*.rs' | grep -v test | grep -v '//'
grep -rn '\[\.\.[a-z_]*\]\|\[[a-z_]*\.\.\]' crates/ --include='*.rs' | grep -v test
grep -rn 'bytes\[\|data\[\|input\[\|payload\[' crates/ --include='*.rs' | grep -v test
grep -rn 'copy_within' crates/ --include='*.rs'
```

Each hit classified as:

- **Safe**: bounds-checked on prior line, or the slice/offset is a fixed-size
  array (`[u8; N]`), `min`-bounded, or derived from a length-prefixed read
  whose length is capped to a constant.
- **Suspect**: the offset or length is attacker-influenced and the indexing
  could panic with `range end index N out of range for slice of length M`.

## Findings

### Suspect (1 site, with a fix in flight)

| File:Line | Pattern | Classification | Recommended Fix |
| --- | --- | --- | --- |
| `crates/transfer/src/map_file/buffered.rs:144` | `self.buffer.copy_within(src_offset..src_offset + reuse_len, 0)` in `load_window` overlap branch | Suspect - `reuse_len` derived from prior window extent vs `aligned_start`; a malformed zlib delta stream can request a window whose `reuse_len` exceeds the resized buffer length at EOF tail | Already fixed in PR #5566 (`fix/uts-18-buffered-bounds-fail-loud`): wrap with `checked_add` + bounds check, surface `io::ErrorKind::InvalidData`. Confirmed via commit `6ce80f539`. |
| `crates/transfer/src/map_file/buffered.rs:185` | `&self.buffer[start..end]` in `map_ptr` after `load_window` resize | Suspect - `end = start + len` where `len` is delta-token-driven; `end` can exceed `self.buffer.len()` if `load_window` shrinks the buffer near EOF | Already fixed in PR #5566: replace with `self.buffer.get(start..end).ok_or_else(...)`. UTS-18.f regression site. |

PR #5566 is currently OPEN against master. Once merged, both suspect sites
return typed `io::Result::Err` instead of aborting the process. The negative
test PR for EDG-PANIC.3 (this audit's companion) stacks on PR #5566.

### Safe by length-prefix or fixed-size array (representative sample)

| File:Line | Pattern | Why Safe |
| --- | --- | --- |
| `crates/protocol/src/envelope/header.rs:38` | `encoded.copy_from_slice(&bytes[..HEADER_LEN])` | Bounds-checked at line 31 (`bytes.len() < HEADER_LEN` returns `Err`). |
| `crates/protocol/src/flist/read/metadata.rs:245` | `let len = len_buf[0] as usize;` then `vec![0u8; len]` + `read_exact` | `len_buf` is `[u8; 1]`, fixed-size. `len` capped at 255. No bare slice indexing on the wire payload. |
| `crates/protocol/src/flist/read/name.rs:107` | `reader.read_exact(&mut name[start..])` after `name.resize(start + suffix_len, 0)` | Length validated against `MAXPATHLEN` at line 90 before allocation. |
| `crates/protocol/src/wire/file_entry_decode/name.rs:96` | `name.extend_from_slice(&prev_name[..prefix_len])` | `prefix_len = same_len.min(prev_name.len())`, cannot exceed source length. |
| `crates/protocol/src/wire/file_entry_decode/ownership.rs:104` | `let len = len_buf[0] as usize;` + read | `len_buf` is `[u8; 1]`; cap of 255. |
| `crates/protocol/src/wire/compressed_token/lz4_codec.rs:323` | `&self.decompress_buf[..decompressed_len].to_vec()` | `decompressed_len` is the LZ4 library return value (`lz4_flex::block::decompress_into`); library contract: return <= buffer length. `decompress_buf` sized to `lz4_flex::block::get_maximum_output_size(CHUNK_SIZE)` at construction. |
| `crates/protocol/src/wire/compressed_token/zlib_codec.rs:252` | `if self.flush_buf[len - 4..] == [...]` | Guarded by `self.flush_buf.len() >= 4` at line 250. |
| `crates/protocol/src/wire/compressed_token/zlib_codec.rs:458` | `self.decompress_buf[..chunk_len].to_vec()` | `chunk_len = self.decompress_buf.len().min(CHUNK_SIZE)`. |
| `crates/protocol/src/wire/compressed_token/mod.rs:172` | `writer.write_all(&data[offset..offset + piece_len])` | `piece_len = (data.len() - offset).min(MAX_DATA_COUNT)` in loop bounded by `offset < data.len()`. |
| `crates/protocol/src/varint/decode.rs:36` | `buf[..extra].copy_from_slice(&bytes[1..1 + extra])` | `extra > MAX_EXTRA_BYTES` rejected at line 23; `bytes.len() < 1 + extra` rejected at line 27; `buf` is `[u8; 5]` and `extra <= 4`. |
| `crates/protocol/src/varint/encode.rs:23,29,33` | `bytes[count]` indexing | Encoding-only, no attacker input; `count` bounded `1..=4` by `[u8; 5]`. |
| `crates/protocol/src/flist/read/flags.rs:62-103` | `buf[0]` after `read_exact(&mut buf)` where `buf: [u8; 1]` | Fixed-size arrays. |
| `crates/transfer/src/reader/multiplex.rs:327` | `buf[..to_copy].copy_from_slice(&self.buffer[self.pos..self.pos + to_copy])` | `to_copy = available.min(buf.len())` where `available = self.buffer.len() - self.pos`. |
| `crates/transfer/src/reader/multiplex.rs:168-208` | Fixed `[u8; 4]` parse for `MSG_IO_ERROR`/`MSG_REDO`/`MSG_NO_SEND` | Guarded by `if self.buffer.len() == 4`. |
| `crates/transfer/src/receiver/transfer/sync.rs:474` | `expected_buf[..checksum_len]` | `checksum_len = checksum_verifier.digest_len()` returns `const fn` value `<= ChecksumVerifier::MAX_DIGEST_LEN` (20) by enum match; `expected_buf: [u8; MAX_DIGEST_LEN]`. |
| `crates/transfer/src/delta_apply/checksum.rs:144` | `digest_len() -> usize` enum match | Returns 1, 8, 16, or 20 - never exceeds `MAX_DIGEST_LEN = 20`. |
| `crates/compress/src/lz4/raw.rs:225,236` | `[input[0], input[1]]`, `&input[HEADER_SIZE..total_input]` | `input.len() < HEADER_SIZE` and `< total_input` rejected at lines 218 and 229. |
| `crates/compress/src/skip_compress/decider.rs:257` | `&data[8..12]` | Guarded by `data.len() >= 12` at line 256. |
| `crates/compress/src/skip_compress/magic.rs:35` | `&data[self.offset..self.offset + self.bytes.len()]` | `data.len() < self.offset + self.bytes.len()` returns `false` at line 32. `offset` is hard-coded in `KNOWN_SIGNATURES` table. |
| `crates/checksums/src/parallel/mod.rs:409` | `&data[start..end]` | Test code; `end = (start + block_size).min(data.len())`. |
| `crates/checksums/src/strong/sha*.rs` | `hasher.update(&input[..mid])`, `hasher.update(&input[mid..])` | Test code; `mid` chosen explicitly within `input.len()`. |

## Summary

- Files greped: ~314 indexing sites across the four target crates (after
  excluding tests, comments, and asserts).
- Suspect sites flagged: **1** logical site, **2** lines.
  - `crates/transfer/src/map_file/buffered.rs:144` (`copy_within` overlap)
  - `crates/transfer/src/map_file/buffered.rs:185` (`&buf[start..end]` final
    slice)
  - Both bare-slice paths in the same module; fix lives in PR #5566
    (`fix/uts-18-buffered-bounds-fail-loud`), commit `6ce80f539`.

All other reviewed sites are either bounds-checked on the prior line, derive
from `[u8; N]` fixed arrays whose length is bounded by the type system, use
`min(...)` to clamp into a known-bounded range, or come from library APIs
with a documented length contract.

## Follow-Up Items

Filed as future EDG-PANIC tasks (audit-only enumeration; not opened here):

1. EDG-PANIC.4 - Add an `lz4_flex::block::decompress_into` length sanity
   check inline in `lz4_codec.rs` to defend against a library regression
   that could violate the buffer-bound contract.
2. EDG-PANIC.5 - Audit `crates/engine/src/concurrent_delta/` and
   `crates/engine/src/delta/` for the same pattern (out of scope for
   EDG-PANIC.1's protocol/transfer/checksums/compress focus).
3. EDG-PANIC.6 - Document the convention "wire-derived offsets must use
   `.get(range)` not `[range]`" in `docs/style/parsing.md` once those
   sites are converged.
4. EDG-PANIC.7 - Run `cargo-fuzz` against `BufferedMap::map_ptr` with the
   UTS-18 regression corpus from `crates/transfer/fuzz/corpus/` (already
   landed in commit `6d273b4ef`).

## Related Work

- PR #5566 (`fix(transfer): turn buffered map_file out-of-range panic into Err`):
  open against master; replaces the two suspect sites with typed
  `InvalidData` errors.
- Commit `6d273b4ef` (`test(fuzz): add buffered_map fuzz target + UTS-18
  regression corpus`): fuzz coverage for the same code path.
- Commit `05b0964dd` (`fix(transfer): never shrink BufferedMap buffer to
  avoid overlap data loss`): related stability fix for the same buffer.
