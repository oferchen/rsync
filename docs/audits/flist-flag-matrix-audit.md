# FLIST_* / XMIT_* flag matrix audit

Audit of every transmission flag (`XMIT_*`) used in the rsync flist wire
format, the protocol versions on which each bit is meaningful, and the state
of our encode/decode coverage relative to the upstream 3.4.1 reference.

Sources of truth:

- Upstream rsync 3.4.1 (protocol 32) headers and flist code.
- Our protocol crate flist module: `crates/protocol/src/flist/`.
- Our flist crate (live in-memory state): `crates/flist/src/`.

Upstream files referenced (relative to
`target/interop/upstream-src/rsync-3.4.1/`):

- `rsync.h` lines 47-73 - `XMIT_*` constants.
- `rsync.h` lines 77-100 - in-memory `FLAG_*` constants.
- `flist.c` lines 380-680 - `send_file_entry()`.
- `flist.c` lines 682-1050 - `recv_file_entry()`.

Repo paths in the cross-reference tables are written relative to the
workspace root.

## 1. Master matrix

Legend for the per-protocol cells:

- `E+D` - we encode and decode the bit on this protocol.
- `E` - we encode but do not decode (or skip the field on decode).
- `D` - we decode but do not encode (or skip on encode).
- `N/A` - the bit is not defined on that protocol version.
- `-` - the bit is reserved on that protocol; neither side touches it.

`XMIT_EXTENDED_FLAGS` is the gate bit that switches a single-byte flag
header into a two-byte header for protocols 28-32. With
`VARINT_FLIST_FLAGS` (compat flag `'V'`) negotiated, both bytes of the
extended flags collapse into a single varint and bit 17
(`XMIT_CRTIME_EQ_MTIME`) becomes addressable.

| Flag (XMIT_*) | Bit | Proto 28 | Proto 29 | Proto 30 | Proto 31 | Proto 32 |
|---|---|---|---|---|---|---|
| `TOP_DIR` | 0 | E+D | E+D | E+D | E+D | E+D |
| `SAME_MODE` | 1 | E+D | E+D | E+D | E+D | E+D |
| `SAME_RDEV_pre28` | 2 | N/A | N/A | N/A | N/A | N/A |
| `EXTENDED_FLAGS` | 2 | E+D | E+D | E+D | E+D | E+D |
| `SAME_UID` | 3 | E+D | E+D | E+D | E+D | E+D |
| `SAME_GID` | 4 | E+D | E+D | E+D | E+D | E+D |
| `SAME_NAME` | 5 | E+D | E+D | E+D | E+D | E+D |
| `LONG_NAME` | 6 | E+D | E+D | E+D | E+D | E+D |
| `SAME_TIME` | 7 | E+D | E+D | E+D | E+D | E+D |
| `SAME_RDEV_MAJOR` | 8 | E+D | E+D | E+D | E+D | E+D |
| `NO_CONTENT_DIR` | 8 | N/A | N/A | E+D | E+D | E+D |
| `HLINKED` | 9 | E+D | E+D | E+D | E+D | E+D |
| `SAME_DEV_pre30` | 10 | E+D | E+D | N/A | N/A | N/A |
| `USER_NAME_FOLLOWS` | 10 | N/A | N/A | E+D | E+D | E+D |
| `RDEV_MINOR_8_pre30` | 11 | E+D | E+D | N/A | N/A | N/A |
| `GROUP_NAME_FOLLOWS` | 11 | N/A | N/A | E+D | E+D | E+D |
| `HLINK_FIRST` | 12 | N/A | N/A | E+D | E+D | E+D |
| `IO_ERROR_ENDLIST` | 12 | N/A | N/A | N/A | E+D | E+D |
| `MOD_NSEC` | 13 | N/A | N/A | N/A | E+D | E+D |
| `SAME_ATIME` | 14 | E+D | E+D | E+D | E+D | E+D |
| `UNUSED_15` | 15 | - | - | - | - | - |
| `RESERVED_16` | 16 | - | - | - | - | - |
| `CRTIME_EQ_MTIME` | 17 | N/A | N/A | N/A | E+D | E+D |

Notes on the matrix:

- The pre-28 alias `SAME_RDEV_pre28` (bit 2) is listed for completeness; we
  only run protocols 28-32 on the wire, so the pre-28 meaning is N/A for
  every column. The constant is exported in our flags module so that
  golden-byte tests can document the historic encoding.
- Bits 16+ are only addressable when both peers negotiate
  `VARINT_FLIST_FLAGS`. Without that capability, the second byte of the
  flags header is the high byte of the wire and bit 16 / bit 17 cannot be
  expressed at all.
- `IO_ERROR_ENDLIST` is reachable on protocol 30 too when the upstream
  `'f'` compat flag is enabled, but our negotiated default does not enable
  it for protocol 30; the matrix reflects the default behaviour.

## 2. Per-flag explanation

Bits with stable single meanings:

- `TOP_DIR` (bit 0) - sender marks command-line argument directories so the
  receiver does not treat them as deletable hierarchies. Also abused as a
  filler to keep the first flag byte non-zero on protocols < 28 for
  non-directory entries (`flist.c:561`, our
  `crates/protocol/src/flist/write/encoding.rs:62`).
- `SAME_MODE` (bit 1) - mode unchanged from previous entry; suppresses the
  4-byte mode field.
- `SAME_UID` (bit 3) / `SAME_GID` (bit 4) - id unchanged or `--owner` /
  `--group` not in effect. Upstream sets the bit unconditionally when
  preservation is disabled to keep the wire compact.
- `SAME_NAME` (bit 5) - prefix length follows in a single byte; suffix only
  is then transmitted.
- `LONG_NAME` (bit 6) - suffix length uses varint30 instead of one byte.
- `SAME_TIME` (bit 7) - mtime unchanged; suppresses the time field.

Bits with version-dependent dual meanings:

- Bit 2 - `SAME_RDEV_pre28` for legacy protocols 20-27, `EXTENDED_FLAGS` for
  protocols 28+. We never speak <28 over the wire, so the `EXTENDED_FLAGS`
  semantics are the only ones reachable; the `pre28` constant is preserved
  for documentation and golden tests.
- Bit 8 - `SAME_RDEV_MAJOR` on devices/specials, `NO_CONTENT_DIR` on
  directories (protocol 30+). Disambiguation is by entry type. Upstream and
  our code gate the directory meaning on `S_ISDIR(mode)`.
- Bit 10 - `SAME_DEV_pre30` on hardlink dev/ino encoding for protocols
  28-29, `USER_NAME_FOLLOWS` for protocol 30+ when `inc_recurse` ships the
  owner string.
- Bit 11 - `RDEV_MINOR_8_pre30` on protocols 28-29 device records,
  `GROUP_NAME_FOLLOWS` for protocol 30+ id-name shipping.
- Bit 12 - `HLINK_FIRST` for the leader of a hardlink set on protocol 30+,
  re-purposed as `IO_ERROR_ENDLIST` on protocol 31+ to embed an error code
  in the end-of-list marker. Disambiguation is by context: only the
  combined two-byte sentinel `EXTENDED_FLAGS | (IO_ERROR_ENDLIST << 8)` is
  treated as the end marker; otherwise bit 12 is the hardlink leader bit.

Reserved bits:

- `UNUSED_15` (bit 15) and `RESERVED_16` (bit 16) - upstream documents these
  as reserved. We export them as constants but never read or write them.

## 3. Cross-reference: upstream vs ours

### 3.1 Constant declarations

| Flag | Upstream `rsync.h` | Our `crates/protocol/src/flist/flags.rs` |
|---|---|---|
| `TOP_DIR` | 47 | 21 |
| `SAME_MODE` | 48 | 26 |
| `SAME_RDEV_pre28` | 49 | 40 |
| `EXTENDED_FLAGS` | 50 | 33 |
| `SAME_UID` | 51 | 45 |
| `SAME_GID` | 52 | 50 |
| `SAME_NAME` | 53 | 56 |
| `LONG_NAME` | 54 | 62 |
| `SAME_TIME` | 55 | 67 |
| `SAME_RDEV_MAJOR` | 57 | 78 |
| `NO_CONTENT_DIR` | 58 | 85 |
| `HLINKED` | 59 | 90 |
| `SAME_DEV_pre30` | 60 | 97 |
| `USER_NAME_FOLLOWS` | 61 | 102 |
| `RDEV_MINOR_8_pre30` | 62 | 110 |
| `GROUP_NAME_FOLLOWS` | 63 | 115 |
| `HLINK_FIRST` | 64 | 121 |
| `IO_ERROR_ENDLIST` | 65 | 122 |
| `MOD_NSEC` | 66 | 127 |
| `SAME_ATIME` | 67 | 133 |
| `UNUSED_15` | 68 | 139 |
| `RESERVED_16` | 72 | 148 |
| `CRTIME_EQ_MTIME` | 73 | 155 |

### 3.2 Encode (sender) sites

`flist.c:send_file_entry()` runs from line 380 to 680. Our encoder is
split between
`crates/protocol/src/flist/write/xflags.rs` (xflag computation),
`crates/protocol/src/flist/write/encoding.rs` (flags header, name, rdev,
hardlink, end-of-list), and
`crates/protocol/src/flist/write/metadata.rs` (size, time, mode, atime,
uid/gid).

| Flag | Upstream encode | Our encode |
|---|---|---|
| `TOP_DIR` | `flist.c:413,415,419,553,561` | `write/xflags.rs:94`, `write/encoding.rs:47,62` |
| `SAME_MODE` | `flist.c:431` | `write/xflags.rs:98`, `write/metadata.rs:86` |
| `EXTENDED_FLAGS` | `flist.c:550,555` | `write/encoding.rs:39,51,361` |
| `SAME_UID` | `flist.c:464` | `write/xflags.rs:111`, `write/metadata.rs:126` |
| `SAME_GID` | `flist.c:474` | `write/xflags.rs:117`, `write/metadata.rs:151` |
| `SAME_NAME` | `flist.c:540,564` | `write/xflags.rs:121`, `write/encoding.rs:86` |
| `LONG_NAME` | `flist.c:542,566` | `write/xflags.rs:125`, `write/encoding.rs:60,90` |
| `SAME_TIME` | `flist.c:484,581` | `write/xflags.rs:102`, `write/metadata.rs:63` |
| `SAME_RDEV_MAJOR` | `flist.c:444,457,627` | `write/xflags.rs:155,162`, `write/encoding.rs:168` |
| `NO_CONTENT_DIR` | `flist.c:415,417` | `write/xflags.rs:276` |
| `HLINKED` | `flist.c:530` | `write/xflags.rs:190,199` |
| `SAME_DEV_pre30` | `flist.c:526,655` | `write/xflags.rs:201`, `write/encoding.rs:248` |
| `USER_NAME_FOLLOWS` | `flist.c:470,602` | `write/xflags.rs:225`, `write/metadata.rs:134` |
| `RDEV_MINOR_8_pre30` | `flist.c:448,459,631` | `write/xflags.rs:157,168`, `write/encoding.rs:175` |
| `GROUP_NAME_FOLLOWS` | `flist.c:480,614` | `write/xflags.rs:232`, `write/metadata.rs:159` |
| `HLINK_FIRST` | `flist.c:510,573` | `write/xflags.rs:192`, `write/encoding.rs:206` |
| `IO_ERROR_ENDLIST` | `flist.c` (end-of-list) | `write/encoding.rs:362` |
| `MOD_NSEC` | `flist.c:488,587` | `write/xflags.rs:259`, `write/metadata.rs:67` |
| `SAME_ATIME` | `flist.c:491,595` | `write/xflags.rs:246`, `write/metadata.rs:106` |
| `CRTIME_EQ_MTIME` | `flist.c:499,590` | `write/xflags.rs:255`, `write/metadata.rs:71` |

### 3.3 Decode (receiver) sites

`flist.c:recv_file_entry()` runs from line 682 to 1110. Our decoder is
split between
`crates/protocol/src/flist/read/flags.rs` (flag header parsing and
end-of-list detection),
`crates/protocol/src/flist/read/metadata.rs` (size, time, mode, atime,
uid/gid), and
`crates/protocol/src/flist/read/extras.rs` (symlink, rdev, hardlink,
checksum).

| Flag | Upstream decode | Our decode |
|---|---|---|
| `TOP_DIR` | `flist.c:1086,1089,1091,1098` | (FileEntry flags) `read/mod.rs` via `flags.top_dir()` |
| `SAME_MODE` | `flist.c:864` | `read/metadata.rs:101` |
| `EXTENDED_FLAGS` | `flist.c:766` (in caller) | `read/flags.rs:101,127,155` |
| `SAME_UID` | `flist.c:880` | `read/metadata.rs:140`, `read_owner_id()` 230 |
| `SAME_GID` | `flist.c:891` | `read/metadata.rs:159`, `read_owner_id()` 230 |
| `SAME_NAME` | `flist.c:716` | (caller of `read_flags`) `read/name.rs` via `flags.same_name()` |
| `LONG_NAME` | `flist.c:719` | `read/name.rs` via `flags.long_name()` |
| `SAME_TIME` | `flist.c:828` | `read/metadata.rs:73` |
| `SAME_RDEV_MAJOR` | `flist.c:911` | `read/extras.rs:114` |
| `NO_CONTENT_DIR` | `flist.c:1085,1089` | `read/metadata.rs:173` |
| `HLINKED` | `flist.c:780,964,1030` | `read/extras.rs:162,200` |
| `SAME_DEV_pre30` | `flist.c:655` (mirror) | `read/extras.rs:209` |
| `USER_NAME_FOLLOWS` | `flist.c:885` | `read/metadata.rs:136`, `read_owner_id()` 242 |
| `RDEV_MINOR_8_pre30` | `flist.c:915` | `read/extras.rs:126` |
| `GROUP_NAME_FOLLOWS` | `flist.c:897` | `read/metadata.rs:155`, `read_owner_id()` 242 |
| `HLINK_FIRST` | `flist.c:780` (via `BITS_SETnUNSET`) | `read/extras.rs:167` |
| `IO_ERROR_ENDLIST` | `flist.c` end-of-list / safe flist | `read/flags.rs:148,155-169` |
| `MOD_NSEC` | `flist.c:841` | `read/metadata.rs:82` |
| `SAME_ATIME` | `flist.c:866` | `read/metadata.rs:116` |
| `CRTIME_EQ_MTIME` | `flist.c:851` | `read/metadata.rs:90` |

## 4. Compatibility risks

The audit did not find any flag where we encode but cannot decode, or
decode but cannot encode. The risks are concentrated around bit aliasing
and around bits 16+ in the varint encoding.

1. Bit-12 disambiguation. `HLINK_FIRST` and `IO_ERROR_ENDLIST` share the
   same bit. We disambiguate at decode time by treating the exact pair
   (primary == `EXTENDED_FLAGS`, extended == `IO_ERROR_ENDLIST`) as the
   end-of-list sentinel
   (`crates/protocol/src/flist/read/flags.rs:154-159`). On encode the same
   sentinel is the only path that emits bit 12 outside a real entry
   (`crates/protocol/src/flist/write/encoding.rs:362`). A regression that
   started emitting bit 12 with `EXTENDED_FLAGS` on a non-hardlinked entry
   would be parsed by upstream as the IO error marker. The
   `xflags::calculate_hardlink_flags()` gate (`is_dir == false &&
   protocol >= 30 && hardlink_idx().is_some()`) is the load-bearing check.

2. Bit-8 disambiguation. `SAME_RDEV_MAJOR` and `NO_CONTENT_DIR` are gated
   on entry type. Encode side gates the directory meaning on
   `entry.is_dir()` (`write/xflags.rs:275`); decode side gates on the
   reconstructed mode in `is_dir` (`read/metadata.rs:172`). Reading the
   wire as the wrong type would mis-flag a directory as having content or a
   device as same-major.

3. Bit-10 / bit-11 protocol switch. `SAME_DEV_pre30` /
   `RDEV_MINOR_8_pre30` overlap with `USER_NAME_FOLLOWS` /
   `GROUP_NAME_FOLLOWS` at bits 10 and 11. The protocol-version gate is
   load-bearing; both encoder and decoder check `protocol >= 30` before
   applying the new meaning (`write/xflags.rs:217,225,232`,
   `read/metadata.rs:136,155`). A negotiation bug that leaves the runtime
   protocol at 30+ while the peer thinks it is 29 would corrupt the entry
   stream silently.

4. `CRTIME_EQ_MTIME` only exists with `VARINT_FLIST_FLAGS`. Our encoder
   refuses to set the bit unless varint flags are negotiated
   (`write/xflags.rs:254`). Without that guard, the byte that carries bit
   17 is simply not on the wire, so the receiver would still read crtime
   while we skipped it on send. The check is the only thing keeping the
   encode side from desyncing on protocol 31 with the legacy two-byte flag
   header.

5. Pre-28 fields are dead on the wire. `SAME_RDEV_pre28` is a public
   constant but no encoder or decoder path actually consumes it - we have
   never spoken protocol 27 or earlier. Keeping the constant is documented
   as supporting golden-byte tests; deleting it would not change runtime
   behaviour.

6. `UNUSED_15` and `RESERVED_16` are exported but neither read nor written.
   Setting them on the wire would be silently passed through by varint
   decoding into `FileFlags::extended` / `extended16` and ignored. This
   matches upstream which also ignores them.

## 5. Test coverage per flag

Test files surveyed:

- `crates/protocol/src/flist/flags.rs` - module-level unit tests covering
  bit values, dual-meaning aliases, accessor methods, and round-trip
  encoding.
- `crates/protocol/src/flist/read/tests.rs` and
  `crates/protocol/src/flist/write/tests.rs` - encoder/decoder unit tests.
- `crates/protocol/tests/golden_protocol_v28_flist.rs`,
  `golden_protocol_v29_flist.rs`,
  `golden_protocol_v28_wire.rs`,
  `golden_protocol_v29_wire.rs` - byte-for-byte goldens.
- `crates/protocol/tests/protocol_v31_comprehensive.rs` - protocol 31
  flag-driven scenarios.
- `crates/protocol/tests/proptest_file_entry_roundtrip.rs` and
  `proptest_wire_format_fuzz.rs` - property tests covering arbitrary
  flag combinations.
- `crates/protocol/tests/iconv_golden_bytes.rs` - flags interaction with
  iconv.
- `crates/protocol/tests/device_file_encoding.rs` - device rdev flag
  combinations.

Per-flag coverage status:

| Flag | Unit (flags.rs) | Encode/decode unit | Golden | Property | Notes |
|---|---|---|---|---|---|
| `TOP_DIR` | yes (`flags_top_dir`) | yes (write_tests) | yes (v29) | yes | Includes "non-zero filler" path on protocol < 28 |
| `SAME_MODE` | yes | yes (`calculate_xflags_mode_comparison`) | yes (v29 same-time test) | yes | Mode round-trip covered |
| `EXTENDED_FLAGS` | yes (alias) | yes (varint and two-byte paths) | yes | yes | Both encoding modes tested |
| `SAME_UID` | yes | yes (write/read round-trip) | yes (v29 0x18 byte) | yes |  |
| `SAME_GID` | yes | yes | yes (v29 0x18 byte) | yes |  |
| `SAME_NAME` | yes | yes (`calculate_xflags_name_compression`) | yes (v29 prefix test) | yes |  |
| `LONG_NAME` | yes (`flags_long_name`) | yes (suffix > 255) | yes | yes |  |
| `SAME_TIME` | yes | yes | yes (v29 same-time) | yes |  |
| `SAME_RDEV_MAJOR` | yes | yes (`flags_same_high_rdev`) | yes (device_file_encoding) | yes |  |
| `NO_CONTENT_DIR` | yes (`flags_no_content_dir`) | yes (round-trip dir without content) | partial (no dedicated golden) | yes |  |
| `HLINKED` | yes | yes (round-trip + follower) | yes (v29) | yes |  |
| `SAME_DEV_pre30` | yes (`flags_same_dev_pre30`) | yes (v28 hardlink path) | yes (v28 flist) | yes |  |
| `USER_NAME_FOLLOWS` | yes (`flags_user_name_follows`) | yes (round-trip) | partial | yes | No dedicated golden, covered via round-trip |
| `RDEV_MINOR_8_pre30` | yes | yes | yes (device_file_encoding) | yes |  |
| `GROUP_NAME_FOLLOWS` | yes (`flags_group_name_follows`) | yes (round-trip) | partial | yes | Same gap as user-name |
| `HLINK_FIRST` | yes (`flags_hlink_first`) | yes (`is_abbreviated_follower_helper`) | yes (v29) | yes |  |
| `IO_ERROR_ENDLIST` | yes (`flags_io_error_endlist`, alias) | yes (write_end + read_flags error) | yes (v31 comprehensive) | yes | Both safe-flist negotiation paths exercised |
| `MOD_NSEC` | yes (`flags_mod_nsec`) | yes (round-trip + nsec writer) | yes (v31 comprehensive) | yes |  |
| `SAME_ATIME` | yes (`flags_same_atime`) | yes (round-trip atime) | partial | yes |  |
| `UNUSED_15` | yes (constant value test) | none | none | none | Reserved; no semantic test possible |
| `RESERVED_16` | yes (constant value test) | none | none | none | Reserved; no semantic test possible |
| `CRTIME_EQ_MTIME` | yes (`flags_crtime_eq_mtime`) | yes (round-trip + varint gate) | partial | yes | Needs `VARINT_FLIST_FLAGS` |

Coverage gaps to consider:

- No dedicated byte-level golden for `USER_NAME_FOLLOWS`,
  `GROUP_NAME_FOLLOWS`, `NO_CONTENT_DIR`, `SAME_ATIME`, or
  `CRTIME_EQ_MTIME`. They are covered indirectly by round-trip tests but
  a captured upstream byte stream would harden interop.
- `UNUSED_15` and `RESERVED_16` have no behavioural assertion that we
  ignore them on read. Adding a test that injects bit 15 or bit 16 into a
  varint flag value and asserts the entry decodes unchanged would close
  the gap.
- The pre-28 alias `SAME_RDEV_pre28` is only covered by the constant
  identity test (`xmit_same_rdev_pre28_same_as_extended_flags`). No wire
  test exercises the protocol < 28 path; this is acceptable because we do
  not negotiate those protocols.

## 6. Findings summary

- All 23 documented `XMIT_*` bits are accounted for in our flag module,
  with constants, accessors, and round-trip helpers.
- For every protocol version we negotiate (28-32) every meaningful bit is
  encoded and decoded; there are no encode-only or decode-only flags.
- The dual-meaning bits (2, 8, 10, 11, 12) are disambiguated at the same
  point as upstream - protocol version, entry mode, or sentinel context -
  and our regression tests pin those gates.
- The remaining risk surface is concentrated in negotiation: incorrect
  protocol numbers or a missed `VARINT_FLIST_FLAGS` capability would
  silently misalign the wire. The encoder guards on
  `self.protocol.as_u8()` and `self.use_varint_flags()` in every relevant
  helper.
- Test coverage is strongest for the flags whose bytes change file size or
  type and weakest for purely advisory bits (`SAME_ATIME`, name-follows
  variants, `CRTIME_EQ_MTIME`). Adding byte-level goldens for these would
  bring coverage to parity with the rest of the matrix without changing
  runtime code.
