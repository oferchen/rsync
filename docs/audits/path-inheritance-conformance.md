# Path Inheritance / Common-Prefix Compression Conformance Audit

Task: #2105

This audit reviews oc-rsync's file-list path inheritance (common-prefix
compression) against upstream rsync 3.4.1's `flist.c:send_file_entry()` and
`flist.c:recv_file_entry()`. It covers the encoder/decoder pair, the
`XMIT_SAME_NAME` flag, the leading-byte-count + suffix wire format, and edge
cases (empty prefix, full match, Unicode boundaries, protocol 28 vs 30+).

## 1. Source Map

| Concern | oc-rsync (Rust) | Upstream (C) |
|---|---|---|
| Common-prefix length | `crates/protocol/src/flist/state.rs::FileListCompressionState::calculate_name_prefix_len` | `flist.c:send_file_entry()` (`l1`/`l2` calculation) |
| Helper (free fn) | `crates/protocol/src/wire/file_entry/flags.rs::calculate_name_prefix_len` | same |
| Sender previous-name buffer | `flist/state.rs::FileListCompressionState::prev_name [u8; 4096]` | `flist.c` `static char lastname[MAXPATHLEN]` |
| Flag set | `flist/write/xflags.rs::calculate_basic_flags` (sets `XMIT_SAME_NAME` when `same_len > 0`) | `flist.c:send_file_entry()` flag block |
| Encoder | `flist/write/encoding.rs::write_name` | `flist.c:send_file_entry()` (`write_byte(f, l1); write_vstring/byte(f, fname+l1, l2)`) |
| Decoder | `flist/read/name.rs::read_name` | `flist.c:recv_file_entry()` (`l1 = read_byte(f); l2 = read_vstring/byte(f); ...`) |
| Long-name length codec | `codec/protocol/legacy.rs` (4-byte LE) and `codec/protocol/modern.rs` (varint) | `flist.c` via `read_varint30` / `write_varint30_int` |
| Constant `XMIT_SAME_NAME` | `flist/flags.rs:55-56` (`1 << 5`) | `rsync.h` `XMIT_SAME_NAME (1<<5)` |

`FileListCompressionState::prev_name` is a fixed `[u8; 4096]` array, exactly
matching upstream's `static char lastname[MAXPATHLEN]` model. Updates are
gated through `set_prev_name`, which truncates to `MAXPATHLEN` bytes -
preserving upstream's "writes beyond MAXPATHLEN are discarded" semantics.

## 2. The XMIT_SAME_NAME Flag (bit 5 / `0x20`)

`XMIT_SAME_NAME` is the trigger for prefix compression. Upstream and oc-rsync
both compute, for the current entry's name `fname`:

```
l1 = min(255, common-prefix-length(lastname, fname))
l2 = strlen(fname) - l1
```

The flag is set iff `l1 > 0`. oc-rsync's `calculate_basic_flags` (xflags.rs:120-122)
implements exactly this guard:

```rust
if same_len > 0 {
    xflags |= XMIT_SAME_NAME as u32;
}
```

The cap at 255 lives in `state.rs::calculate_name_prefix_len` (`.min(255)`),
matching upstream's single-byte width for `l1` on the wire.

After encoding (or decoding) an entry, the writer/reader copies the *current*
name into `prev_name` (`flist/state.rs::set_prev_name`, called from
`update_name`/`update`). On the read side the assignment is identical
(`flist/read/name.rs:96`: `self.state.update_name(&name)`). This matches
upstream's invariant that `lastname` always holds the most recent transmitted
filename, including the case where the entry is an abbreviated hardlink
follower (see `flist/write/mod.rs:447` - the abbreviated branch still calls
`update_name`).

Note on iconv: when `--iconv` is in effect, both sides update `prev_name`
with the *wire* (post-conversion on send, pre-conversion on receive) bytes.
Upstream `flist.c:1606-1621` does the same so prefix sharing operates over
identical byte streams on both ends. Comments at
`flist/write/mod.rs:377-382` and `flist/read/name.rs:108-113` cite this.

## 3. Wire Format - Leading Byte Count + Suffix

The on-wire layout for the name portion of a file entry is:

```
[ flags ]                         varint or 1-2 bytes (covers XMIT_SAME_NAME)
[ same_len : u8 ]                 only if XMIT_SAME_NAME (l1, 0..=255)
[ suffix_len ]                    1 byte if !XMIT_LONG_NAME, else codec-dependent
[ suffix bytes : suffix_len ]     name[same_len..]
```

Encoder (`flist/write/encoding.rs::write_name`, lines 78-97):

```rust
if xflags & (XMIT_SAME_NAME as u32) != 0 {
    writer.write_all(&[same_len as u8])?;
}
if xflags & (XMIT_LONG_NAME as u32) != 0 {
    self.codec.write_long_name_len(writer, suffix_len)?;
} else {
    writer.write_all(&[suffix_len as u8])?;
}
writer.write_all(&name[same_len..])
```

Decoder (`flist/read/name.rs::read_name`, lines 35-99) consumes the fields
in the same order, then concatenates `prev_name[..same_len]` with the new
suffix into the output `name`. The decoder asserts
`same_len <= prev_name.len()` and rejects malformed inputs with
`InvalidData` ("same_len N exceeds previous name length M") - upstream
treats this as an unrecoverable protocol error too (file list corruption).

`XMIT_LONG_NAME` (suffix > 255 bytes) is orthogonal to `XMIT_SAME_NAME`.
The two flags can be set together: a long suffix following a shared prefix
encodes `[same_len:u8] + [varint suffix_len] + [suffix]` on protocol 30+,
or `[same_len:u8] + [4-byte LE suffix_len] + [suffix]` on protocol < 30
(see section 5).

## 4. Byte-for-Byte Parity for Adjacent Entries

The repository encodes byte-level expectations as golden tests rather than
hex blobs, capturing the exact field offsets and values upstream produces.
Selected goldens:

- `crates/protocol/tests/golden_protocol_v29_flist.rs::golden_v29_name_compression_byte_layout`
  asserts that for entries `dir/file1.txt` followed by `dir/file2.txt`
  (size 100, mode 0o644, same mtime), the second entry's bytes are exactly:

  ```
  flags  = 0xA2          (SAME_TIME|SAME_NAME|SAME_MODE)
  same_len  = 8          ("dir/file")
  suffix_len = 5
  suffix     = "2.txt"
  size       = 100 as i32 LE  (4 bytes)
  total      = 12 bytes
  ```

  This is the wire layout upstream emits for the same input.

- `golden_v29_multi_entry_name_compression` (same file, lines 388-457)
  drives three entries through `FileListWriter::write_entry` then
  `FileListReader::read_entry`, confirming both compression *and*
  round-trip equality for `dir/file1.txt`, `dir/file2.txt`, `dir/file3.txt`.

- `golden_protocol_v28_flist.rs::golden_v28_no_flag_compression`
  (lines 577-605) confirms the inverse: when names share no prefix and
  metadata diverges, `XMIT_SAME_NAME` is *not* set.

Property tests in `proptest_file_entry_roundtrip.rs` (around lines
1100-1130) generate batches of entries sharing a prefix and a single
mode/mtime/uid/gid, exercising `XMIT_SAME_NAME` together with the other
SAME_* flags through randomized inputs and asserting decode returns the
original names.

Wire-level interop (daemon push/pull against upstream 3.0.9, 3.1.3, 3.4.1)
is exercised by `tools/ci/run_interop.sh`. The interop matrix passing
guarantees real upstream binaries accept and produce byte-identical streams
for prefix-compressed file lists.

## 5. Edge Cases

### 5.1 Empty Common Prefix

`same_len == 0` skips the `XMIT_SAME_NAME` flag, so no `same_len` byte is
emitted (encoder line 86) and the decoder reads `same_len = 0` by default
(reader line 40-46). The full name is sent as the suffix. This is identical
to upstream's behavior at the very first entry (where `lastname` is empty)
and at any entry whose first byte differs from the previous name.

Tested: `flist/state.rs::calculate_name_prefix_len_empty_prev` (line 295)
and `golden_v28_no_flag_compression`.

### 5.2 Full Match (Zero-Byte Suffix)

When the new name equals the previous name byte-for-byte:

- `same_len = name.len()` (capped at 255)
- `suffix_len = 0`
- `XMIT_SAME_NAME` set; `XMIT_LONG_NAME` clear
- Wire bytes: `[ flags ] [ same_len ] [ 0 ] [ ]` (no suffix bytes)

Both encoder and decoder handle this without special-casing: `write_all`
with a zero-length slice is a no-op and `read_exact` with zero length
succeeds without consuming any bytes. The decoder still calls
`update_name` with the reconstructed `prev_name[..same_len]`, which is
the original previous name - consistent with upstream `lastname` retention.

A name longer than 255 bytes that exactly matches the previous name still
caps `same_len` at 255, leaving a non-empty suffix consisting of the tail
bytes - this matches upstream's single-byte `l1` width.

Tested: `state.rs::calculate_name_prefix_len_full_match` (line 321) and
`calculate_name_prefix_len_caps_at_255` (line 312).

### 5.3 Unicode Boundaries

Filenames on the wire are byte streams. Both upstream and oc-rsync compare
*bytes*, not codepoints, when computing the common prefix. Because UTF-8 is
self-synchronizing (no codepoint is a strict prefix of another at sub-byte
granularity), prefix matching at byte granularity always splits at a valid
codepoint boundary *for filenames that share an exact UTF-8 prefix*.

A subtler case is two filenames whose UTF-8 encodings share the leading
bytes of a multi-byte codepoint but diverge mid-codepoint. The byte-level
comparison reports the matching prefix length as the index where the bytes
diverge, which can fall *inside* a codepoint. This is upstream behavior:
`flist.c` operates on `char *` with no codepoint awareness, and oc-rsync
matches it via the bytewise `take_while` in
`state.rs::calculate_name_prefix_len`. The reconstructed name on the
receiving side is byte-identical to the sender's original, so no codepoint
truncation occurs in practice; the field is purely a transport-level
deduplication and is opaque to filesystem semantics.

iconv interaction: with `--iconv` active, the prefix compression operates
on the wire encoding (post-`local_to_remote` on send, pre-`remote_to_local`
on receive). The receiver's `prev_name` deliberately holds wire bytes -
not the locally-decoded path - so prefix sharing remains stable across
iconv conversion. Upstream stores the raw wire `lastname` for the same
reason (`flist.c:738-754` runs iconv *after* `lastname` has already been
extended with the new name's wire bytes).

Tested: `unicode_wire_format.rs::wire_format_prefix_compression_unicode`
(CJK paths), `wire_format_prefix_compression_mixed` (CJK + ASCII), and
`wire_format_unicode_all_protocol_versions` cover protocols V28, V29,
V30, V31, V32 across ASCII, Latin-1 (`café`), CJK, supplementary plane
(`U+1F600`), Arabic, and combining-mark composed forms.

### 5.4 Protocol 28 vs 30+ Differences

The `XMIT_SAME_NAME` flag itself is unchanged across protocols 28-32: same
bit value (`0x20`), same trigger condition, same `same_len` u8 byte. The
differences are confined to two adjacent fields:

1. **Flags container.** Protocol < 28 uses one flag byte; protocol 28-31
   uses one or two flag bytes via `XMIT_EXTENDED_FLAGS`; protocol >= 32
   (or earlier with negotiated `VARINT_FLIST_FLAGS`) uses a single varint
   that may also carry the bit-16 `XMIT_CRTIME_EQ_MTIME`. See
   `flist/write/encoding.rs::write_flags`. `XMIT_SAME_NAME` always lives
   in byte 0, so prefix compression is independent of the flag-encoding
   mode.

2. **Long-name length width.** When `XMIT_LONG_NAME` is also set:
   - Protocol < 30 (`codec/protocol/legacy.rs`): `suffix_len` is a 4-byte
     little-endian `u32`.
   - Protocol >= 30 (`codec/protocol/modern.rs`): `suffix_len` is a varint
     (1-5 bytes, typically 1-2 for realistic names).

   This matches upstream's `read_varint30` / `write_varint30_int` dispatch.

The `same_len` byte remains a single `u8` in all protocols. There is no
protocol version at which oc-rsync (or upstream) widens it.

Tested: golden v28 and v29 flist tests for byte layouts; `protocol_v32_compat.rs`
covers varint flag mode with prefix compression as part of full-entry round-trips.

## 6. Findings

- Encoder and decoder are byte-for-byte symmetric and align with upstream
  `flist.c:send_file_entry` / `recv_file_entry` for all protocols 28-32.
- `XMIT_SAME_NAME` semantics, `same_len` width (u8, 0..=255), and field
  ordering are exact matches.
- Prefix length cap at 255 is enforced in
  `FileListCompressionState::calculate_name_prefix_len` and the free
  `wire::file_entry::calculate_name_prefix_len` helper.
- `prev_name` is a fixed `[u8; 4096]` mirroring upstream `lastname[MAXPATHLEN]`
  rather than a heap-allocated `Vec`, preserving upstream's per-entry zero-
  allocation invariant and truncation behavior.
- Edge cases (empty prefix, full match, long suffixes, iconv, Unicode) are
  covered by golden, property, and round-trip tests at the protocol crate
  level. Unicode codepoint-boundary handling is byte-equivalent to upstream
  (both ignore codepoint structure).
- Protocol 28 vs 30+ divergence is confined to flag-byte width and long-name
  length encoding; the prefix-compression machinery itself is invariant.

No corrective action required. The conformance posture is satisfied.

## 7. References

- `crates/protocol/src/flist/state.rs`
- `crates/protocol/src/flist/flags.rs`
- `crates/protocol/src/flist/write/xflags.rs`
- `crates/protocol/src/flist/write/encoding.rs`
- `crates/protocol/src/flist/write/mod.rs`
- `crates/protocol/src/flist/read/name.rs`
- `crates/protocol/src/flist/read/mod.rs`
- `crates/protocol/src/wire/file_entry/flags.rs`
- `crates/protocol/src/codec/protocol/legacy.rs`
- `crates/protocol/src/codec/protocol/modern.rs`
- `crates/protocol/tests/golden_protocol_v28_flist.rs`
- `crates/protocol/tests/golden_protocol_v29_flist.rs`
- `crates/protocol/tests/unicode_wire_format.rs`
- `crates/protocol/tests/proptest_file_entry_roundtrip.rs`
- Upstream: `flist.c:send_file_entry()`, `flist.c:recv_file_entry()`,
  `rsync.h` (`XMIT_SAME_NAME`, `MAXPATHLEN`).
