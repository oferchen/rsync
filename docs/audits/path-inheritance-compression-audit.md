# Audit: path inheritance / common-prefix compression

Tracking: oc-rsync task #2105.

This audit walks the file-list common-prefix compression scheme that
rsync uses to shrink consecutive filenames on the wire, comparing the
upstream C implementation in
`target/interop/upstream-src/rsync-3.4.1/flist.c` against the Rust
implementation split between
`crates/protocol/src/flist/write/encoding.rs` (sender) and
`crates/protocol/src/flist/read/name.rs` (receiver). The goal is to
prove byte-for-byte parity, surface any latent divergences, and call out
the test-coverage gaps that would let a regression slip past CI.

## Scope

In scope:

- The shared-prefix length computation (`l1` upstream,
  `same_len` in Rust) and its 255-byte cap.
- The suffix length computation (`l2` upstream, `suffix_len` in Rust)
  and the 256-byte boundary that toggles `XMIT_LONG_NAME`.
- The wire layout written / read for `XMIT_SAME_NAME` and
  `XMIT_LONG_NAME`.
- The `lastname` static / `FileListCompressionState::prev_name` buffer
  and how it persists across INC_RECURSE flist segments.
- Edge cases: first entry, empty names, internal NULs, names exactly at
  the 255 / 256 byte boundaries, prefixes that exceed the previous
  name's length on the read side.

Out of scope:

- iconv conversion (covered by `docs/audits/iconv-pipeline.md` and the
  golden tests in `crates/protocol/tests/iconv_golden_bytes.rs`).
- Hardlink follower abbreviation (only the metadata path is altered;
  the name field is encoded with the exact same prefix scheme).
- Wire encoding for non-name fields (xflags layout, varint dispatch,
  rdev, mtime, etc.).

## 1. Wire format walkthrough

For each file entry the sender emits, in order, after the `xflags`
header:

```
[ xflags: 1-2 bytes (or varint) ]
[ same_len: u8         ] ; only if xflags & XMIT_SAME_NAME
[ suffix_len           ] ; u8 if !XMIT_LONG_NAME, varint30 if XMIT_LONG_NAME
[ name suffix bytes    ] ; suffix_len raw bytes, no terminator
```

The receiver reconstructs the full filename by concatenating
`prev_name[..same_len]` with `suffix_len` bytes pulled from the wire.
The reconstructed name is then stored back into the prev-name buffer so
the next entry can build on it.

### `same_len` byte (`XMIT_SAME_NAME`)

The sender computes `same_len` as the length of the longest common
byte prefix between the new filename and the previous filename, capped
at 255 (the maximum value that fits in a single byte). When `same_len`
is non-zero, `XMIT_SAME_NAME` (`0x20`) is set in `xflags` and a single
`u8` carries the prefix length.

Upstream sender (`flist.c:534-540`):

```c
for (l1 = 0;
     lastname[l1] && (fname[l1] == lastname[l1]) && (l1 < 255);
     l1++) {}
l2 = strlen(fname+l1);

if (l1 > 0)
    xflags |= XMIT_SAME_NAME;
if (l2 > 255)
    xflags |= XMIT_LONG_NAME;
```

```c
if (xflags & XMIT_SAME_NAME)
    write_byte(f, l1);
```

Rust sender
(`crates/protocol/src/flist/write/mod.rs:384` plus
`crates/protocol/src/flist/write/encoding.rs:78-97` and
`crates/protocol/src/flist/state.rs:171-178`):

```rust
let same_len = self.state.calculate_name_prefix_len(&name);
let suffix_len = name.len() - same_len;
let xflags = self.calculate_xflags(entry, same_len, suffix_len);
```

```rust
pub fn calculate_name_prefix_len(&self, name: &[u8]) -> usize {
    self.prev_name[..self.prev_name_len]
        .iter()
        .zip(name.iter())
        .take_while(|(a, b)| a == b)
        .count()
        .min(255)
}
```

```rust
if xflags & (XMIT_SAME_NAME as u32) != 0 {
    writer.write_all(&[same_len as u8])?;
}
```

The `XMIT_SAME_NAME` xflag is set in the basic-flags pass when
`same_len > 0`
(`crates/protocol/src/flist/write/xflags.rs:120-122`), matching
upstream's `if (l1 > 0)`.

### `suffix_len` and `XMIT_LONG_NAME` (varint30)

`suffix_len` is the number of bytes the wire must carry. When it fits
in a single byte (`<= 255`) the sender writes `[suffix_len: u8]`.
Otherwise `XMIT_LONG_NAME` (`0x40`) is set and the length is written via
`write_varint30`, which dispatches as follows:

| Protocol version | Encoding             | Source                                  |
|-----------------:|----------------------|-----------------------------------------|
| `< 30`           | 4-byte little-endian | `io.h:write_varint30` -> `write_int`    |
| `>= 30`          | rsync varint         | `io.h:write_varint30` -> `write_varint` |

Rust mirrors this dispatch in
`crates/protocol/src/codec/protocol/legacy.rs:76-84` (proto < 30,
fixed `i32 LE`) and
`crates/protocol/src/codec/protocol/modern.rs:61-67` (proto >= 30,
`write_varint`). The free helper `write_varint30_int` in
`crates/protocol/src/varint/encode.rs:141-151` is the equivalent of
upstream's inline `write_varint30()`.

The receiver dispatches identically in
`crates/protocol/src/flist/read/name.rs:50-57`:

```rust
let suffix_len = if flags.long_name() {
    self.codec.read_long_name_len(reader)?
} else {
    let mut byte = [0u8; 1];
    reader.read_exact(&mut byte)?;
    byte[0] as usize
};
```

After reading the length, the suffix is appended to the inherited
prefix and the combined buffer is fed back into the per-stream state:

```rust
let mut name = Vec::with_capacity(same_len + suffix_len);
name.extend_from_slice(&self.state.prev_name()[..same_len]);
if suffix_len > 0 {
    let start = name.len();
    name.resize(start + suffix_len, 0);
    reader.read_exact(&mut name[start..])?;
}
self.state.update_name(&name);
```

## 2. Edge cases

### Empty names

Upstream rejects empty filenames at the sender side
(`flist.c:1873`). The Rust receiver enforces the same invariant as
defense in depth - after `read_name()` returns, the caller checks
`name.is_empty()` and surfaces `InvalidData` with
`"received file entry with zero-length filename"`
(`crates/protocol/src/flist/read/mod.rs:507-516`). This path is
covered by `read_entry_rejects_zero-length_filename` in
`crates/protocol/src/flist/read/tests.rs:1286-1307`. There is no
matching sender-side rejection: a caller that hands the writer a
zero-length name would emit `same_len=0`, `suffix_len=0`, and zero
suffix bytes, which the upstream receiver would also flag. Adding a
sender-side `debug_assert!(!name.is_empty())` would catch the
programming error earlier without changing the wire output.

### NUL bytes inside a name

Upstream's prefix loop is bounded by `lastname[l1] && (fname[l1] ==
lastname[l1])`. The first NUL in either string terminates the loop.
Filenames produced by the file system layer never contain NUL bytes,
but a malicious sender could construct one and feed it to a receiver.

The Rust prefix scan is a byte-slice comparison
(`take_while(|(a, b)| a == b)`); it does not treat NUL specially. The
two implementations therefore disagree on the prefix length only when
both strings happen to share an internal NUL byte at the same offset,
which is a degenerate input that upstream rejects elsewhere on the
pipeline. The divergence is wire-observable but unreachable from any
well-formed sender. Documenting it here so a future fuzzer / property
test can pin the behaviour to the upstream contract; the safest fix is
for the receiver to reject any name containing an interior `\0` byte
during cleaning, which would also harden the path against
double-byte / wide-char smuggling.

### 256-byte boundary triggering `XMIT_LONG_NAME`

The boundary is strictly `> 255` upstream (`if (l2 > 255) xflags |=
XMIT_LONG_NAME;`) and `> 255` in Rust
(`crates/protocol/src/flist/write/xflags.rs:124-126`). Concretely:

| `suffix_len` | `XMIT_LONG_NAME` | Length encoding |
|-------------:|:-----------------|:----------------|
| 0            | unset            | one `u8 = 0x00` |
| 1..=255      | unset            | one `u8`        |
| 256..        | set              | varint30        |

A 255-byte suffix uses the single-byte encoding (`0xFF`); 256 bytes
flips to varint30. The sender's single-byte path
(`writer.write_all(&[suffix_len as u8])?;` in
`encoding.rs:93`) is reachable only when `XMIT_LONG_NAME` is unset, so
the `as u8` truncation is safe by construction.

### Prefix exceeds the previous name's length (receiver only)

The receiver guards against a malformed sender that asks it to share
more bytes than were ever emitted:

```rust
if same_len > self.state.prev_name().len() {
    return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "same_len {} exceeds previous name length {}",
            same_len,
            self.state.prev_name().len()
        ),
    ));
}
```

(`crates/protocol/src/flist/read/name.rs:68-77`). This is stricter
than upstream, which performs only a `MAXPATHLEN` overflow check
(`flist.c:724-729`) and otherwise trusts the sender. The Rust check is
covered by `read_name_rejects_invalid_prefix_length`
(`crates/protocol/src/flist/read/tests.rs:582-604`). Behaviour is
hardening, not divergence: a benign sender will never trigger it.

### First entry

On the first entry of a session, upstream's static `lastname` array is
all-zero, so `lastname[0] == '\0'` and the prefix loop terminates at
`l1 = 0`. `XMIT_SAME_NAME` is therefore never set on the first entry.

Rust matches: `FileListCompressionState::new()` sets `prev_name_len =
0`, the slice `&prev_name[..0]` is empty, the `zip` iterator produces
zero pairs, and `same_len = 0`. The xflags pass leaves
`XMIT_SAME_NAME` clear. The new name is then stored via
`state.update_name(&name)` so the *second* entry can begin sharing.

## 3. `lastname` state across INC_RECURSE flist segments

In incremental recursion the file list is shipped as a sequence of
segments, each starting at a different `ndx_start`. Upstream uses
function-static `lastname` buffers in both `send_file_entry()`
(`flist.c:399`) and `recv_file_entry()` (`flist.c:697`). C statics
persist across calls, so a sub-list entry can - and routinely does -
share its prefix with the *last entry of the previous segment*. The
only segment-scoped state on the wire is the index base.

oc-rsync mirrors this:

- `FileListWriter::set_first_ndx(first_ndx)` (write/mod.rs:333)
  updates only the segment's wire index base.
- `FileListReader::reset_for_new_segment(ndx_start)` (read/mod.rs:421)
  is documented to update *only* `ndx_start` and explicitly preserves
  compression state. Its docstring spells the upstream contract out:
  "static variables for name compression, uid, gid, etc.... persist
  across `recv_file_list()` calls".
- A repository-wide grep for `state.reset()` inside the writer turns
  up nothing: the writer never clears its `FileListCompressionState`
  between segments.
- `state.reset()` exists (`state.rs:271-273`) but is only used in
  unit tests; the production code path never calls it during a live
  transfer.

Net effect: cross-segment prefix compression works end to end, exactly
as upstream does.

## 4. Worked byte-level example

Four entries, protocol >= 30, varint flags disabled (the typical
non-`VARINT_FLIST_FLAGS` path), preserving uid/gid set the
`XMIT_SAME_UID` / `XMIT_SAME_GID` shortcut; mode/mtime are constant so
`XMIT_SAME_MODE` and `XMIT_SAME_TIME` are also set. We focus on the
name portion of each entry. Bytes are shown in hex, ASCII alongside.

| `#` | `prev_name` | `name`                  | `same_len` | `suffix_len` | `xflags` (low byte) |
|----:|:------------|:------------------------|-----------:|-------------:|:--------------------|
| 0   | (empty)     | `src/main.rs`           | 0          | 11           | `0x..` no SAME_NAME |
| 1   | `src/main.rs` | `src/lib.rs`          | 4          | 7            | `XMIT_SAME_NAME`    |
| 2   | `src/lib.rs`  | `src/lib.rs.bak`      | 11         | 4            | `XMIT_SAME_NAME`    |
| 3   | `src/lib.rs.bak` | `tests/foo.rs`     | 0          | 13           | no SAME_NAME        |

Per-entry name field on the wire (the rest of `xflags`, mtime, mode,
uid, gid etc. are omitted for clarity):

Entry 0 (no shared prefix - first entry; `XMIT_SAME_NAME` clear):
```
0B 73 72 63 2F 6D 61 69 6E 2E 72 73       ; suffix_len=11, "src/main.rs"
```

Entry 1 (shares `"src/"`):
```
04 07 6C 69 62 2E 72 73                    ; same_len=4, suffix_len=7, "lib.rs"
```

Entry 2 (shares `"src/lib.rs"`):
```
0B 04 2E 62 61 6B                          ; same_len=11, suffix_len=4, ".bak"
```

Entry 3 (no shared prefix; `XMIT_SAME_NAME` clear):
```
0D 74 65 73 74 73 2F 66 6F 6F 2E 72 73     ; suffix_len=13, "tests/foo.rs"
```

Receiver reconstructs each name by concatenating the saved prefix with
the wire suffix and updating its own `prev_name` buffer:

```
state.prev_name() := "src/main.rs"      ; after entry 0
state.prev_name() := "src/lib.rs"       ; after entry 1
state.prev_name() := "src/lib.rs.bak"   ; after entry 2
state.prev_name() := "tests/foo.rs"     ; after entry 3
```

Total name-field bytes for these four entries: 12 + 8 + 6 + 13 = 39
bytes vs 11 + 11 + 14 + 13 = 49 bytes uncompressed; common-prefix
compression saves 10 bytes (~20%) on this small slice.

## 5. Discrepancies vs upstream

Two spots where the Rust implementation deviates from upstream
behaviour. Both are stricter (refuse malformed input) rather than
laxer; neither breaks interop with a well-formed peer.

1. **Receiver rejects oversized `same_len`**
   (`read/name.rs:68-77`). Upstream only checks `l2 >= MAXPATHLEN -
   l1` (`flist.c:724`); it does not validate `l1` independently. A
   crafted stream where the sender claims `same_len > prev_name_len`
   would return `InvalidData` from oc-rsync but be silently accepted
   by upstream (which would copy garbage from its `lastname[]`
   tail). This is hardening that should be retained.

2. **Prefix loop ignores NUL bytes** (`state.rs:171-178`). Upstream
   stops at the first `\0` in `lastname`; Rust compares raw bytes.
   The divergence is unreachable for filenames produced by the OS but
   distinguishable for hand-crafted inputs containing internal NULs.
   Adding an interior-NUL rejection in `clean_and_validate_name`
   would close the gap without altering the wire contract.

No discrepancy was found in:

- The 255-byte cap on `same_len` (`min(255)` mirrors `(l1 < 255)`).
- The 256-byte threshold for `XMIT_LONG_NAME` (`> 255` matches
  upstream).
- The varint30 dispatch for the suffix length on protocols 30+ vs
  the fixed 4-byte `i32 LE` on earlier protocols.
- INC_RECURSE prefix sharing across segments (state preserved in
  both directions).
- First-entry handling (no `XMIT_SAME_NAME` because the buffer is
  empty / zero-filled in both implementations).
- Storage of the post-iconv (wire-form) name into the prev-name
  buffer so subsequent prefix matches operate on the same byte
  representation upstream does.

## 6. Test coverage gaps

Existing coverage:

- `name_prefix_compression_max_255_bytes` (write/tests.rs:1547)
  exercises the 255-cap with a 300-byte common prefix.
- `calculate_xflags_name_compression` (write/tests.rs:156) exercises
  the `same_len > 0` and `suffix_len > 255` flag-emission paths.
- A 300-byte `XMIT_LONG_NAME` round trip lives at
  write/tests.rs:1818.
- `read_name_rejects_invalid_prefix_length` and
  `truncated_long_name_varint` cover the two reader-side error
  paths.
- `read_entry_rejects_zero-length_filename` (read/tests.rs:1286)
  covers receiver rejection of empty names.

Gaps that should be filled:

1. **Exact 255 / 256 boundary tests for `suffix_len`**. Existing
   tests cap the prefix at 255 or use a 300-byte name; nothing
   pins the *transition* from single-byte length to varint30 at
   exactly 255 -> 256. A round-trip test with `suffix_len == 255`
   (single-byte path) and `suffix_len == 256`
   (`XMIT_LONG_NAME` path) would lock the boundary.

2. **`same_len == 255` with `prev_name.len() == 256+`**. The cap
   is unit-tested but not in a round-trip with a *different*
   suffix on the second entry; the existing test reuses the same
   long prefix for both entries. A test where prev = "a" * 300 and
   new = "a" * 254 + "X" exercises the 254-byte cap without
   short-circuiting on whole-name equality.

3. **Cross-INC_RECURSE-segment prefix sharing**. There is no test
   that calls `reset_for_new_segment(...)` between two entries and
   asserts that `XMIT_SAME_NAME` still fires across the boundary
   (with the second entry's `same_len` referring to the first
   entry's name). This is the contract called out in the
   `reset_for_new_segment` docstring but unverified in CI.

4. **Sender-side first-entry assertion**. No test pins that the
   very first entry of a freshly constructed `FileListWriter`
   never emits `XMIT_SAME_NAME`, even when its name happens to
   match the all-zero `prev_name` buffer (a name like `b"\0"` is
   degenerate but worth pinning to avoid surprises).

5. **NUL-byte semantics**. There is no test that probes how the
   prefix loop handles names containing internal NULs. A
   property-style test asserting that the receiver rejects names
   containing `\0` (recommended fix) would also document the
   intent.

6. **Varint30 length round-trip for protocol 28/29**. The fixed
   4-byte `i32 LE` path through `legacy::write_long_name_len`
   has unit coverage in
   `crates/protocol/src/codec/protocol/tests.rs` but no end-to-end
   round trip via `FileListWriter` / `FileListReader` for a long
   name on protocol 29. The interop golden tests in
   `crates/protocol/tests/golden_protocol_v29_flist.rs` should
   grow a long-name fixture.

7. **Same-name short-circuit when names are byte-identical**.
   Identical consecutive names should produce
   `same_len == name.len() (capped 255)` and `suffix_len == 0`,
   writing only the prefix length and a single zero byte for the
   suffix. This is reachable in real workloads (think hardlink
   followers in the same directory) and worth a dedicated test.

Filling gaps 1-3 closes the most impactful holes: they cover the
exact upstream wire contract that the implementation is supposed to
match, and they would catch any future refactor that accidentally
changes either the boundary condition or the cross-segment state
preservation.
