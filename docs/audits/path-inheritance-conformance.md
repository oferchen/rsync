# Path Inheritance / Common-Prefix Compression Conformance

Task: #2105

Audit of file-list path inheritance (the `XMIT_SAME_NAME` shared-prefix
compression) against upstream rsync 3.4.1.

## 1. Upstream Encoding (rsync 3.4.1 `flist.c`)

`send_file_entry()` (lines 399-570) uses a single static buffer
`static char lastname[MAXPATHLEN]` to track the previously transmitted
name. For each entry it computes:

```
for (l1 = 0; lastname[l1] && (fname[l1] == lastname[l1]) && (l1 < 255); l1++) {}
l2 = strlen(fname + l1);
if (l1 > 0)  xflags |= XMIT_SAME_NAME;   // bit 5, 0x20
if (l2 > 255) xflags |= XMIT_LONG_NAME;  // bit 6, 0x40
```

Wire layout for the name portion:

| Field | When emitted | Width |
|---|---|---|
| `l1` (shared prefix length) | iff `XMIT_SAME_NAME` | 1 byte (0..=255) |
| `l2` (suffix length) | always | 1 byte if `!XMIT_LONG_NAME`, else `varint30` (proto >= 30) or 4-byte LE (proto < 30) |
| suffix bytes | always | `l2` bytes from `fname + l1` |

After encoding, line 676 unconditionally refreshes the buffer:
`strlcpy(lastname, fname, MAXPATHLEN)`. `recv_file_entry()` (lines
697-736) mirrors the steps and rejects `l2 >= MAXPATHLEN - l1` as
flist corruption, then refreshes its own `lastname`.

## 2. oc-rsync Implementation

| Concern | File / Symbol |
|---|---|
| Prefix length (`l1`) | `crates/protocol/src/flist/state.rs::FileListCompressionState::calculate_name_prefix_len` (caps at 255) |
| Previous-name buffer | `flist/state.rs::prev_name: [u8; 4096]` (matches `MAXPATHLEN`) |
| Flag set | `flist/write/xflags.rs::calculate_basic_flags` (sets `XMIT_SAME_NAME` iff `same_len > 0`) |
| Encoder | `flist/write/encoding.rs::write_name` |
| Decoder | `flist/read/name.rs::read_name` |
| Constant | `flist/flags.rs::XMIT_SAME_NAME = 1 << 5` |
| Long-suffix codec | `codec/protocol/{legacy,modern}.rs` (4-byte LE vs varint30) |

The encoder emits `[same_len:u8]?[suffix_len:1|varint][suffix]`,
matching upstream byte for byte. The decoder reads in the same order
and rejects `same_len > prev_name.len()` with `InvalidData`.

## 3. Audit Findings

| Edge case | Status | Notes |
|---|---|---|
| `l1` off-by-one at directory boundary (`dir/a` -> `dir2/b`) | Conformant | byte-wise compare matches upstream's `lastname[l1] && (fname[l1] == lastname[l1])`; `/` is treated as any other byte. |
| Empty-name root entry (`""`) | Conformant | `same_len = 0` -> flag clear; encoder emits 0-length suffix; decoder accepts. |
| Multi-byte (UTF-8) char split mid-prefix | Conformant by design | Both implementations compare bytes, not codepoints. The suffix carries the remaining bytes verbatim, so round-trip output is byte-identical. Prefix sharing also operates over post-iconv bytes on send, pre-iconv bytes on receive (`flist/write/mod.rs:377`, `flist/read/name.rs:108`). |
| Path > 255 bytes total | Conformant | `l1` capped at 255 by upstream loop and oc-rsync's `.min(255)`; `l2` switches to `XMIT_LONG_NAME` codec when > 255. Combined `l1 + l2` may exceed 255. |
| `l1 + l2 >= MAXPATHLEN` | Conformant | Decoder bounds-checks `same_len <= prev_name.len()` and the suffix-bounded write into a fixed buffer, mirroring upstream's `l2 >= MAXPATHLEN - l1` rejection. |
| Hardlink follower abbreviated entries | Conformant | `flist/write/mod.rs:447` still calls `update_name`, matching upstream's unconditional `strlcpy` at line 676. |

## 4. Test Gaps

Existing coverage:

- Golden byte fixtures: `crates/protocol/tests/golden_protocol_v29_flist.rs::golden_v29_name_compression_byte_layout` (single adjacent pair) and `golden_v29_multi_entry_name_compression` (3-entry round-trip).
- Negative golden: `golden_protocol_v28_flist.rs::golden_v28_no_flag_compression`.
- Property: `proptest_file_entry_roundtrip.rs` randomized batches with shared prefixes.

Gaps:

1. No protocol-30 golden for `XMIT_SAME_NAME | XMIT_LONG_NAME` combined (varint30 suffix following `same_len` byte).
2. No protocol-32 golden for the same combination; only v28/v29 fixtures exist.
3. No fixture for `same_len == 255` (boundary saturation) or `prev_name` length exactly `MAXPATHLEN`.
4. No fixture exercising UTF-8 split mid-prefix despite the byte-wise design being load-bearing.
5. No empty-name (root) round-trip golden for any protocol.

## 5. Recommendation

Add a property test that:

1. Generates arbitrary `Vec<Vec<u8>>` filename batches (mixed UTF-8, ASCII, lengths up to 4096, includes empty and 255-byte segments).
2. For each protocol in `{28, 29, 30, 31, 32}`, encodes via `FileListWriter::write_entry` and decodes via `FileListReader::read_entry`.
3. Asserts: (a) decoded names equal originals byte-for-byte, (b) `same_len` matches `calculate_name_prefix_len(prev, curr)`, (c) `XMIT_SAME_NAME` set iff `same_len > 0`, (d) `XMIT_LONG_NAME` set iff `suffix_len > 255`.

Follow-on goldens for the combinations in section 4 should land in a
separate issue once the property test is in place.
