# Audit: FLIST flag matrix per protocol 28-32

Closes #2104.

## Scope

Map every `XMIT_*` flag upstream rsync 3.4.1 transmits in the file-list stream
to the protocol versions where it applies, then verify oc-rsync's writer
(`crates/protocol/src/flist/write/`) and reader (`crates/protocol/src/flist/read/`)
honour the same gating. Source of truth:

- `target/interop/upstream-src/rsync-3.4.1/rsync.h:47-73` -- `XMIT_*` definitions.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:380-680` -- `send_file_entry()`.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:682-1020` -- `recv_file_entry()`.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:117-749` -- `CF_*` capability flags.

## 1. Upstream XMIT_* catalogue

| Bit | Macro | Width | Protocol gating |
|----:|-------|-------|-----------------|
| 0 | `XMIT_TOP_DIR` | primary | all |
| 1 | `XMIT_SAME_MODE` | primary | all |
| 2 | `XMIT_SAME_RDEV_pre28` | primary | 20-27 (devices) |
| 2 | `XMIT_EXTENDED_FLAGS` | primary | 28+ |
| 3 | `XMIT_SAME_UID` | primary | all |
| 4 | `XMIT_SAME_GID` | primary | all |
| 5 | `XMIT_SAME_NAME` | primary | all |
| 6 | `XMIT_LONG_NAME` | primary | all |
| 7 | `XMIT_SAME_TIME` | primary | all |
| 8 | `XMIT_SAME_RDEV_MAJOR` | extended | 28+ devices |
| 8 | `XMIT_NO_CONTENT_DIR` | extended | 30+ dirs |
| 9 | `XMIT_HLINKED` | extended | 28+ non-dirs |
| 10 | `XMIT_SAME_DEV_pre30` | extended | 28-29 |
| 10 | `XMIT_USER_NAME_FOLLOWS` | extended | 30+ |
| 11 | `XMIT_RDEV_MINOR_8_pre30` | extended | 28-29 |
| 11 | `XMIT_GROUP_NAME_FOLLOWS` | extended | 30+ |
| 12 | `XMIT_HLINK_FIRST` | extended | 30+ HLINKED |
| 12 | `XMIT_IO_ERROR_ENDLIST` | extended | 31+ end marker (also 30 with `f` compat) |
| 13 | `XMIT_MOD_NSEC` | extended | 31+ |
| 14 | `XMIT_SAME_ATIME` | extended | any (gated by `--atimes`) |
| 15 | `XMIT_UNUSED_15` | extended | reserved |
| 16 | `XMIT_RESERVED_16` | varint-only | 32+ (reserved) |
| 17 | `XMIT_CRTIME_EQ_MTIME` | varint-only | any (gated by `--crtimes`) |

Bits 16+ require `CF_VARINT_FLIST_FLAGS` (proto 32+) because the legacy
two-byte form has no room for them (`flist.c:549-558`).

## 2. Per-protocol flag set

| Protocol | Wire surface | Notable flags |
|---------:|--------------|---------------|
| 28 | 4-byte rsum, no INC_RECURSE, two-byte xflags (`EXTENDED_FLAGS|hi<<8`) | bits 0-11; `SAME_RDEV_MAJOR`, `HLINKED`, `SAME_DEV_pre30`, `RDEV_MINOR_8_pre30` |
| 29 | + multiplex (`MSG_*` framing) | same as 28 |
| 30 | + INC_RECURSE (`CF_INC_RECURSE`); no more `SAME_DEV_pre30`/`RDEV_MINOR_8_pre30` | adds `NO_CONTENT_DIR`, `USER_NAME_FOLLOWS`, `GROUP_NAME_FOLLOWS`, `HLINK_FIRST` |
| 31 | + safe-flist (`CF_SAFE_FLIST`), `CF_AVOID_XATTR_OPTIM` | adds `MOD_NSEC`, `IO_ERROR_ENDLIST` |
| 32 | + `CF_VARINT_FLIST_FLAGS`, `CF_CHKSUM_SEED_FIX`, `CF_INPLACE_PARTIAL_DIR`, `CF_ID0_NAMES` | unlocks bits 16-17 (`RESERVED_16`, `CRTIME_EQ_MTIME`); xflags now a single varint (`flist.c:549`) |

## 3. oc-rsync handling

Constants: `crates/protocol/src/flist/flags.rs:21-158` declare every upstream
`XMIT_*` bit, with the byte-collision aliases (`SAME_RDEV_PRE28` vs
`EXTENDED_FLAGS`; `SAME_RDEV_MAJOR` vs `NO_CONTENT_DIR`;
`SAME_DEV_PRE30`/`USER_NAME_FOLLOWS`; `RDEV_MINOR_8_PRE30`/`GROUP_NAME_FOLLOWS`;
`HLINK_FIRST`/`IO_ERROR_ENDLIST`) commented inline.

Encoders/decoders consult `use_varint_flags()` (`flist/write/xflags.rs:31`,
`flist/read/flags.rs:36`) which gates on `CompatibilityFlags::VARINT_FLIST_FLAGS`.
Behaviour matches upstream's `xfer_flags_as_varint` (compat.c:748):

| Flag | Writer site | Reader site | Proto gate |
|------|-------------|-------------|------------|
| `TOP_DIR`, `SAME_MODE`, `SAME_UID`, `SAME_GID`, `SAME_NAME`, `LONG_NAME`, `SAME_TIME` | `write/xflags.rs` | `read/flags.rs` | all |
| `EXTENDED_FLAGS` (sentinel) | `write/encoding.rs:35-43` | `read/flags.rs:57-96` | 28+ |
| `SAME_RDEV_MAJOR` / `NO_CONTENT_DIR` | `write/encoding.rs:136-173` | `read/metadata.rs:134-160` | 28+ / 30+ |
| `HLINKED`, `HLINK_FIRST` | `write/encoding.rs:191-210` | `read/extras.rs` | 28+ / 30+ |
| `SAME_DEV_pre30`, `RDEV_MINOR_8_pre30` | `write/encoding.rs:160-175` | `read/metadata.rs:153` | 28-29 only |
| `USER_NAME_FOLLOWS`, `GROUP_NAME_FOLLOWS` | `write/metadata.rs:120-150` | `read/metadata.rs:219-240` | 30+ |
| `MOD_NSEC` | `write/metadata.rs:65-72` | `read/metadata.rs:55-90` | 31+ |
| `IO_ERROR_ENDLIST` | `write/encoding.rs:340-365` | `read/flags.rs:141-170` | 31+ |
| `SAME_ATIME` | `write/metadata.rs:95-115` | `read/metadata.rs` | any (option-gated) |
| `CRTIME_EQ_MTIME` | `write/xflags.rs:249-257` | `read/flags.rs` (extended16) | 32+ varint only |

## 4. Gaps

- `XMIT_UNUSED_15` and `XMIT_RESERVED_16`: defined and round-tripped but never
  set by sender or interpreted by receiver. Matches upstream (reserved).
- INC_RECURSE sender bit ('i' in `-e.LsfxCIvu`): emitted only for receiver
  direction (`build_capability_string(!is_sender)` in
  `crates/protocol/src/setup.rs`). Upstream advertises symmetrically; oc-rsync's
  asymmetric emission is intentional pending sender-side validation (memory
  note: `INC_RECURSE` sender code exists but interop is not wired).
- No production handling of `XMIT_SAME_RDEV_pre28` (proto < 28). Acceptable
  because oc-rsync's minimum negotiated version is 28.
- `XMIT_HLINK_FIRST` reuses the same bit constant as `XMIT_IO_ERROR_ENDLIST`;
  disambiguation is contextual (HLINKED-set vs zero-flag sentinel) and matches
  upstream (`flist.c:606-614`).

No XMIT_* upstream sets that oc-rsync silently drops.

## 5. Verification

Existing golden coverage:

- `crates/protocol/tests/golden_protocol_v28_flist.rs` (#1605) -- proto 28
  sort + flag layout fixtures.
- `crates/protocol/tests/golden_protocol_v29_flist.rs` (#1741) -- proto 29
  regular file/directory/symlink entries, end-of-list marker, UID/GID
  encoding parity with upstream.
- `crates/protocol/tests/protocol_v30_compat.rs:198-297` -- v30 varint,
  `VARINT_FLIST_FLAGS`, `SAFE_FILE_LIST` capability gates.
- `crates/protocol/tests/protocol_v31_comprehensive.rs:37-179` -- v31
  varint-flist-flags, `CHKSUM_SEED_FIX`, `AVOID_XATTR_OPTIM`.
- `crates/protocol/tests/protocol_v32_compat.rs` -- v32 varint xflags,
  `CRTIME_EQ_MTIME`, `ID0_NAMES`, `INPLACE_PARTIAL_DIR`.
- `crates/protocol/tests/protocol_feature_gates.rs` and
  `protocol_interop_matrix.rs` -- cross-protocol capability matrix.

Recommended additions tracked separately:

- Round-trip golden for `IO_ERROR_ENDLIST` in proto 30 with the legacy `f`
  compat flag (currently only proto 31+ path is covered).
- Negative-case golden ensuring `SAME_DEV_pre30` is rejected when reading a
  proto 30+ stream (bit 10 must decode as `USER_NAME_FOLLOWS`).
