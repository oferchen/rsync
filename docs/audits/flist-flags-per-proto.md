# FLIST_* / XMIT_* per-protocol-version drilldown

Companion to `docs/audits/flist-flag-matrix-audit.md` (PR #3769). The
prior audit lays out the full master matrix of every `XMIT_*` bit and
its presence on protocols 28-32. This document narrows the focus to
*per-version variation* - the points where the same flag means
different things, occupies different bit positions, or requires a
different wire layout depending on the negotiated protocol number.

Source-of-truth for the cross-references is the in-tree code only:

- `crates/protocol/src/flist/flags.rs` - flag constants and `FileFlags`.
- `crates/protocol/src/flist/write/xflags.rs` - sender flag computation.
- `crates/protocol/src/flist/write/encoding.rs` - flag header writer.
- `crates/protocol/src/flist/write/metadata.rs` - field gating on encode.
- `crates/protocol/src/flist/read/flags.rs` - flag header reader.
- `crates/protocol/src/flist/read/metadata.rs` - field gating on decode.
- `crates/protocol/src/version/protocol_version/capabilities.rs` -
  protocol version capability predicates.

Repo paths in this document are written relative to the workspace
root.

## 1. Flag-header wire layout per protocol

The header that carries the flags themselves changes shape with the
negotiated protocol version and the `VARINT_FLIST_FLAGS` (`'V'`) compat
flag. Until the receiver knows the layout, no flag bit can be read at
all.

| Protocol | `VARINT_FLIST_FLAGS` | Header on the wire | Reachable bits | Source |
|---|---|---|---|---|
| 28 | n/a (proto < 30) | 1 byte; if bit 2 set, a 2nd byte follows | 0-15 | `write/encoding.rs:44-55`, `read/flags.rs:101-107` |
| 29 | n/a (proto < 30) | 1 byte; if bit 2 set, a 2nd byte follows | 0-15 | `write/encoding.rs:44-55`, `read/flags.rs:101-107` |
| 30 | not negotiated | 1 byte; if bit 2 set, a 2nd byte follows | 0-15 | `write/encoding.rs:44-55`, `read/flags.rs:101-107` |
| 30 | negotiated | single varint | 0-23 | `write/encoding.rs:35-43`, `read/flags.rs:59-65` |
| 31 | not negotiated | 1 byte; if bit 2 set, a 2nd byte follows | 0-15 | `write/encoding.rs:44-55`, `read/flags.rs:101-107` |
| 31 | negotiated | single varint | 0-23 | `write/encoding.rs:35-43`, `read/flags.rs:59-65` |
| 32 | not negotiated | 1 byte; if bit 2 set, a 2nd byte follows | 0-15 | `write/encoding.rs:44-55`, `read/flags.rs:101-107` |
| 32 | negotiated | single varint | 0-23 | `write/encoding.rs:35-43`, `read/flags.rs:59-65` |

Consequences:

- `XMIT_CRTIME_EQ_MTIME` (bit 17) is only addressable when the varint
  layout is on the wire, i.e. protocol 30+ AND `VARINT_FLIST_FLAGS`
  negotiated. The encoder gates on this combination at
  `write/xflags.rs:254` (`use_varint_flags() && preserve.crtimes && ...`).
- `XMIT_RESERVED_16` (bit 16) shares the same 2-byte gating but is
  never set anywhere in our code - confirmed by absence of any
  assignment in `write/xflags.rs` and `write/encoding.rs`.
- For protocol 30 with `VARINT_FLIST_FLAGS` *not* negotiated the
  server falls back to the legacy two-byte header; bits 16+ are silently
  unreachable on that wire. Callers that try to set bit 17 on this path
  would desync the receiver.

### 1.1 Zero-flag substitution per layout

A zero flag value would collide with the end-of-list marker. The
substitution rule differs per layout:

| Layout | Substitution | Source |
|---|---|---|
| Varint (proto 30+ with VARINT_FLIST_FLAGS) | xflags := `XMIT_EXTENDED_FLAGS` (0x04) | `write/encoding.rs:38-43` |
| Two-byte header, `is_dir == false` | OR-in `XMIT_TOP_DIR` (0x01) | `write/encoding.rs:46-48` |
| Two-byte header, `is_dir == true` | (no substitution; falls through to the high-byte check) | `write/encoding.rs:50-55` |
| Single-byte header (proto < 28), `is_dir == true` | OR-in `XMIT_LONG_NAME` (0x40) | `write/encoding.rs:58-63` |
| Single-byte header (proto < 28), `is_dir == false` | OR-in `XMIT_TOP_DIR` (0x01) | `write/encoding.rs:58-63` |

The single-byte branch is dead code in our negotiated wire surface
(we never speak < 28) but is preserved verbatim against upstream for
golden-byte parity.

## 2. End-of-list marker variation per protocol

The terminator that closes the flist stream changes byte form across
protocols and across the safe-file-list capability:

| Protocol | Safe file list | Marker on the wire | Source |
|---|---|---|---|
| 28 | n/a | single zero byte | `write/encoding.rs:368` |
| 29 | n/a | single zero byte | `write/encoding.rs:368` |
| 30 (no `'f'`/no `'V'`) | off | single zero byte | `write/encoding.rs:368` |
| 30 (with `'f'` capability, no `'V'`) | on | `[0x04, 0x10]` (XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST) + varint error | `write/encoding.rs:358-365` |
| 30 (with `'V'`) | on (varint) | varint(0) + varint(error) | `write/encoding.rs:352-355` |
| 31 (any) | always on | varint(0) + varint(error) when `'V'`; else 2-byte sentinel + varint(error) | `read/flags.rs:43-47` |
| 32 (any) | always on | varint(0) + varint(error) when `'V'`; else 2-byte sentinel + varint(error) | `read/flags.rs:43-47` |

The `safe_file_list_always_enabled()` predicate
(`version/protocol_version/capabilities.rs:151-153`) returns true for
protocol >= 31 unconditionally. This is the only case in the matrix
where the capability is implied rather than negotiated.

## 3. Bit-by-bit per-protocol meaning

The bits below are the ones whose interpretation actually changes
across the negotiated protocols 28-32. Bits not listed (0-1, 3-7, 9,
13-14) keep one meaning across all five versions.

### 3.1 Bit 2 - `EXTENDED_FLAGS` vs `SAME_RDEV_pre28`

| Protocol | Meaning | Encode site | Decode site |
|---|---|---|---|
| 28 | `XMIT_EXTENDED_FLAGS` (gates 2nd byte) | `write/encoding.rs:51` | `read/flags.rs:101` |
| 29 | `XMIT_EXTENDED_FLAGS` | `write/encoding.rs:51` | `read/flags.rs:101` |
| 30 | `XMIT_EXTENDED_FLAGS` | `write/encoding.rs:51` | `read/flags.rs:101` |
| 31 | `XMIT_EXTENDED_FLAGS` | `write/encoding.rs:51` | `read/flags.rs:101` |
| 32 | `XMIT_EXTENDED_FLAGS` | `write/encoding.rs:51` | `read/flags.rs:101` |

The `pre28` alias is exported in `flist/flags.rs:40` for documentation
parity but no code path on our negotiated wires consumes it. The
`supports_extended_flags()` predicate
(`version/protocol_version/capabilities.rs:120-122`) is `true` for
every protocol we run, so the gate at `write/encoding.rs:44` always
takes the modern branch.

### 3.2 Bit 8 - `SAME_RDEV_MAJOR` vs `NO_CONTENT_DIR`

Bit 8 is the first extended-flag bit (i.e. bit 0 of the 2nd byte, or
bits 8 of the varint). Disambiguation is by entry type, not by
protocol version, but the directory meaning only exists on protocol >=
30:

| Protocol | Entry type | Meaning | Source |
|---|---|---|---|
| 28-32 | device / special | `XMIT_SAME_RDEV_MAJOR` | encode `write/xflags.rs:155-168`; decode `read/extras.rs:114` |
| 28-29 | directory | unused (bit cleared) | not set on encode |
| 30-32 | directory | `XMIT_NO_CONTENT_DIR` | encode `write/xflags.rs:275-277`; decode `read/metadata.rs:172-176` |

The encoder gate is `entry.is_dir() && self.protocol.as_u8() >= 30`
(`write/xflags.rs:275`); the decoder gate is
`is_dir && self.protocol.as_u8() >= 30` (`read/metadata.rs:172`).
A misnegotiated protocol 30+ talking to a 29 peer would mistake a
directory's `NO_CONTENT_DIR` for a device's `SAME_RDEV_MAJOR`.

### 3.3 Bit 9 - `HLINKED`

Identical meaning across 28-32, but the *encode condition* differs:

| Protocol | Set when | Source |
|---|---|---|
| 28 | `preserve.hard_links && !is_dir && entry.hardlink_dev().is_some()` | `write/xflags.rs:195-204` |
| 29 | same as 28 | `write/xflags.rs:195-204` |
| 30 | `preserve.hard_links && !is_dir && entry.hardlink_idx().is_some()` | `write/xflags.rs:188-194` |
| 31 | same as 30 | `write/xflags.rs:188-194` |
| 32 | same as 30 | `write/xflags.rs:188-194` |

Protocol 28-29 attaches hardlink groups by `(dev, ino)` pairs;
protocol 30+ uses a per-list index. The flag bit position is constant
but the trailing field bytes change form.

### 3.4 Bit 10 - `SAME_DEV_pre30` vs `USER_NAME_FOLLOWS`

| Protocol | Meaning | Set on encode | Read on decode |
|---|---|---|---|
| 28 | `XMIT_SAME_DEV_pre30` | `write/xflags.rs:201` (when prev hardlink dev matches) | `read/extras.rs:209` (skip `dev` field) |
| 29 | `XMIT_SAME_DEV_pre30` | `write/xflags.rs:201` | `read/extras.rs:209` |
| 30 | `XMIT_USER_NAME_FOLLOWS` | `write/xflags.rs:225` (when uid name present and not SAME_UID) | `read/metadata.rs:136` (`uid_name_follows` gated on `protocol >= 30`) |
| 31 | `XMIT_USER_NAME_FOLLOWS` | `write/xflags.rs:225` | `read/metadata.rs:136` |
| 32 | `XMIT_USER_NAME_FOLLOWS` | `write/xflags.rs:225` | `read/metadata.rs:136` |

The version gate is duplicated on both sides: encode checks
`self.protocol.as_u8() < 30` early-return at
`write/xflags.rs:217-219`; decode AND-s in
`self.protocol.as_u8() >= 30` at `read/metadata.rs:136`. Either side
relaxing the gate would mis-parse uid bytes as a hardlink-dev sentinel
or vice versa.

### 3.5 Bit 11 - `RDEV_MINOR_8_pre30` vs `GROUP_NAME_FOLLOWS`

Symmetric to bit 10:

| Protocol | Meaning | Set on encode | Read on decode |
|---|---|---|---|
| 28 | `XMIT_RDEV_MINOR_8_pre30` | `write/xflags.rs:157, 168` (when minor <= 0xFF, or always for specials) | `read/extras.rs:126` (read 1 byte instead of 4) |
| 29 | `XMIT_RDEV_MINOR_8_pre30` | `write/xflags.rs:157, 168` | `read/extras.rs:126` |
| 30 | `XMIT_GROUP_NAME_FOLLOWS` | `write/xflags.rs:232` | `read/metadata.rs:155` |
| 31 | `XMIT_GROUP_NAME_FOLLOWS` | `write/xflags.rs:232` | `read/metadata.rs:155` |
| 32 | `XMIT_GROUP_NAME_FOLLOWS` | `write/xflags.rs:232` | `read/metadata.rs:155` |

Encode-side version gate is the same range check
`protocol >= 28 && protocol < 30` at `write/xflags.rs:156, 165`. For
protocol >= 30 the whole block is skipped.

### 3.6 Bit 12 - `HLINK_FIRST` vs `IO_ERROR_ENDLIST`

| Protocol | Context | Meaning | Source |
|---|---|---|---|
| 28-29 | any | unused (bit not assigned) | n/a |
| 30 | regular entry, non-dir, hardlink leader | `XMIT_HLINK_FIRST` | `write/xflags.rs:191-193`; `read/extras.rs:167` |
| 30 | end-of-list with `'f'` capability | `XMIT_IO_ERROR_ENDLIST` (sentinel `0x04, 0x10`) | `write/encoding.rs:361-363`; `read/flags.rs:148-169` |
| 31-32 | regular entry | `XMIT_HLINK_FIRST` | same as proto 30 |
| 31-32 | end-of-list (always safe-flist) | `XMIT_IO_ERROR_ENDLIST` | `read/flags.rs:148-169` |

The disambiguation key is the *combined two-byte sentinel*
`primary == XMIT_EXTENDED_FLAGS && extended == XMIT_IO_ERROR_ENDLIST`
(`read/flags.rs:154-159`). Any other appearance of bit 12 with the
extended-flags marker is a hardlink leader. Symmetrically, the
encoder only emits bit 12 outside a hardlink group via the explicit
`write_end` path at `write/encoding.rs:361-363`.

### 3.7 Bit 13 - `MOD_NSEC`

Identical position across 28-32, but only the protocol >= 31 encoder
ever sets it:

| Protocol | Encode behavior | Decode behavior |
|---|---|---|
| 28 | not set (`write/xflags.rs:258`: `protocol >= 31`) | bit ignored - `read/metadata.rs:67` reads nsec only when set |
| 29 | not set | bit ignored |
| 30 | not set | bit ignored |
| 31 | set when `entry.mtime_nsec() != 0` | nsec varint read |
| 32 | set when `entry.mtime_nsec() != 0` | nsec varint read |

The encode gate is the only thing keeping a 30-only peer from seeing
an unexpected nsec field. There is no decode-side protocol gate; the
flag bit alone drives whether nsec is read. A buggy encoder that sets
bit 13 on protocol 30 would inject a varint that the receiver tries
to read as nsec, desyncing the next entry.

### 3.8 Bit 14 - `SAME_ATIME`

Position fixed; gated by `--atimes` (`preserve.atimes`). The
preservation flag is itself protocol-gated higher up (`--atimes`
emits a `'A'` capability in negotiation), so on a protocol where
`'A'` was rejected the bit will never appear.

| Protocol | Reachable when | Source |
|---|---|---|
| 28-32 | `preserve.atimes && !is_dir && atime == prev_atime` | `write/xflags.rs:245-247`; `read/metadata.rs:115-127` |

Also: protocol >= 32 reads/writes an additional atime nanoseconds
varint when `same_atime` is *not* set
(`read/metadata.rs:120-124`, paralleled in `write/metadata.rs` -
the same mechanism as `MOD_NSEC` but for atime).

### 3.9 Bit 17 - `CRTIME_EQ_MTIME`

Reachable only with the varint header layout and `--crtimes`:

| Protocol | `VARINT_FLIST_FLAGS` | `--crtimes` | Reachable | Source |
|---|---|---|---|---|
| 28-29 | n/a | any | no | bit position not on wire |
| 30 | not negotiated | any | no | bit position not on wire |
| 30 | negotiated | off | no | encoder gate at `write/xflags.rs:254` |
| 30 | negotiated | on | yes (when `crtime == mtime`) | `write/xflags.rs:254-256`; `read/metadata.rs` via `flags.crtime_eq_mtime()` |
| 31 | same as 30 |  |  |  |
| 32 | same as 30 |  |  |  |

The `use_varint_flags() && preserve.crtimes` conjunction is the only
encode site. Without the varint guard the byte that carries bit 17 is
not on the wire, so the encoder would skip crtime while the decoder
still expected it - a silent corruption guard documented at
`write/xflags.rs:249-253`.

## 4. Per-version field-presence checklist

Cross-cuts the bit-by-bit table above into a per-protocol order of
fields written by `write_file_entry()` (encoder) and read by
`read_file_entry()` (decoder).

### 4.1 Protocol 28

Header: 1 byte (bits 0-7), optional 2nd byte (bits 8-15) when bit 2
set. End marker: single zero byte. Bits 16+ unreachable.

| Field | Gate | Bit |
|---|---|---|
| Flags primary | always | 0-7 |
| Flags extended | bit 2 set | 8-15 |
| Name same-len | `SAME_NAME` | 5 |
| Name length (1 byte vs varint30) | `LONG_NAME` | 6 |
| Name suffix | always | n/a |
| Size | always | n/a |
| Mtime | not `SAME_TIME` | 7 |
| Mode | not `SAME_MODE` | 1 |
| Uid | preserve_uid && not `SAME_UID` | 3 |
| Gid | preserve_gid && not `SAME_GID` | 4 |
| Rdev major | device/special && not `SAME_RDEV_MAJOR` | 8 |
| Rdev minor | device/special; 1 byte if `RDEV_MINOR_8_pre30`, 4 bytes else | 11 |
| Hardlink dev | hardlink && not `SAME_DEV_pre30` | 10 |
| Hardlink ino | `HLINKED` | 9 |
| Symlink target | symlink | n/a |

Notes: hardlink encoding uses `(dev, ino)` longints; user/group names
never travel on the wire.

### 4.2 Protocol 29

Identical to 28 in field presence and gates. The differences are in
neighbouring subsystems (iflags, multi-phase, sender/receiver
modifier filters) - not flist flag bits.

### 4.3 Protocol 30 (no `VARINT_FLIST_FLAGS`)

Header still 1+1 bytes. End marker: single zero byte unless `'f'`
capability is on. Bits 16+ unreachable.

| Field | Gate | Bit |
|---|---|---|
| Flags primary | always | 0-7 |
| Flags extended | bit 2 set | 8-15 |
| Name same-len | `SAME_NAME` | 5 |
| Name length | `LONG_NAME` (varint30) | 6 |
| Name suffix | always | n/a |
| Size | always | n/a |
| Mtime | not `SAME_TIME` | 7 |
| Mode | not `SAME_MODE` | 1 |
| Atime | preserve_atimes && !dir && not `SAME_ATIME` | 14 |
| Uid | preserve_uid && not `SAME_UID` | 3 |
| User name | preserve_uid && not `SAME_UID` && `USER_NAME_FOLLOWS` | 10 |
| Gid | preserve_gid && not `SAME_GID` | 4 |
| Group name | preserve_gid && not `SAME_GID` && `GROUP_NAME_FOLLOWS` | 11 |
| Rdev major | device/special && not `SAME_RDEV_MAJOR` | 8 |
| Rdev minor | device/special; varint30 (no pre30 8-bit form) | n/a |
| Hardlink ndx | hardlink, varint; leader emits no idx | 9 / 12 |
| Symlink target | symlink | n/a |
| `NO_CONTENT_DIR` flag | dir | 8 |

Bit-10 / bit-11 now mean user/group name follows. Bit 12 is the
hardlink-leader bit. End-of-list is still a single zero byte unless
the `'f'` capability is negotiated, in which case the two-byte
sentinel is used.

### 4.4 Protocol 30 (with `VARINT_FLIST_FLAGS`)

Same field set as 4.3, but the header is one varint and bits 16+ are
addressable. The end marker is `varint(0) + varint(error_code)`.
`CRTIME_EQ_MTIME` (bit 17) is the only newly reachable bit.

### 4.5 Protocol 31

Adds `MOD_NSEC` (bit 13) and unconditional safe file list. Header
layout identical to 30 (varint when `'V'` negotiated, else 1+1
bytes). End marker is always the safe-flist sentinel + error varint
when an error is reported, else the protocol-appropriate zero
encoding.

| Field | New vs proto 30 |
|---|---|
| Mtime nanoseconds varint | written when `MOD_NSEC` set (bit 13) |
| End-of-list error code | always shipped (safe-flist mandatory) |

### 4.6 Protocol 32

Adds atime nanoseconds varint (read at `read/metadata.rs:120-124`).
No new flag bits; the existing `SAME_ATIME` interacts with the new
nsec field exactly the same way `SAME_TIME` interacts with the proto
31 mtime nsec field.

## 5. Negotiation-side gating

The flag matrix is meaningful only after the protocol version and
compat-flag set is locked in. The relevant predicates that the flist
code consults at runtime:

| Predicate | Defined in | Used in |
|---|---|---|
| `supports_extended_flags()` (proto >= 28) | `version/protocol_version/capabilities.rs:120-122` | `write/encoding.rs:44` |
| `uses_varint_encoding()` (proto >= 30) | `version/protocol_version/capabilities.rs:31-33` | `read/extras.rs:241`, `read/name.rs:50` |
| `uses_fixed_encoding()` (proto < 30) | `version/protocol_version/capabilities.rs:39-41` | `read/metadata.rs:143, 162` |
| `safe_file_list_always_enabled()` (proto >= 31) | `version/protocol_version/capabilities.rs:151-153` | `read/flags.rs:46` |
| `CompatibilityFlags::VARINT_FLIST_FLAGS` (`'V'`) | `compatibility/flags.rs:48` | `write/mod.rs:160`, `read/flags.rs:38` |
| `CompatibilityFlags::SAFE_FILE_LIST` (`'f'`) | `compatibility/flags.rs` | `read/flags.rs:43` |

Two protocol-version checks remain inline rather than going through
the predicate API - both in `write/xflags.rs`:

- `self.protocol.as_u8() >= 28 && self.protocol.as_u8() < 30`
  (lines 156, 165) - pre-30 device-record encoding.
- `self.protocol.as_u8() >= 30` (line 188) - hardlink-by-index gate.
- `self.protocol.as_u8() < 30` (line 217) - early-return for the
  user/group name follow encoder.
- `self.protocol.as_u8() >= 30` (line 275) - directory `NO_CONTENT_DIR`.
- `self.protocol.as_u8() >= 31` (line 258) - `MOD_NSEC` set.
- `self.protocol.as_u8() < 31` (line 145) - special-files rdev gate.

Folding these into named predicates (`supports_pre30_hardlink_dev()`,
`supports_mod_nsec()`, `supports_no_content_dir()`) is a follow-up
opportunity. None of them are bugs; the inline checks all match the
semantics of upstream. The risk is only one of drift if a future
predicate rename misses the inline copies.

## 6. Findings beyond PR #3769

PR #3769 established that all 23 `XMIT_*` bits round-trip on
protocols 28-32. The per-version drilldown surfaces four observations
that the master matrix glosses over:

1. The flag-header layout itself is per-protocol/per-capability and
   gates which bits are even reachable. The varint layout is the only
   way bit 17 (`CRTIME_EQ_MTIME`) reaches the wire; the audit's
   `E+D` cell on protocol 30-32 implicitly assumes `'V'` negotiated.
2. The end-of-list marker varies in three forms depending on
   protocol and `safe_file_list`. A misnegotiated peer that reads the
   wrong terminator will see a spurious entry rather than a clean
   close.
3. Bits 9 (`HLINKED`) and 12 (`HLINK_FIRST`) keep their *position* but
   change their *encode trigger* at the proto-29/30 boundary - the
   matrix marks both as `E+D` for every protocol, but the field that
   follows them on the wire differs. This is the largest semantic
   shift inside the flag block.
4. Several protocol-version checks are inline `as_u8()` comparisons
   rather than capability predicates. They are correct but bypass the
   centralized capability layer; consolidating them would make the
   per-version semantics easier to audit in the future.
