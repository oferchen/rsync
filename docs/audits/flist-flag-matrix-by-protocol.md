# FLIST flag matrix by protocol (28/29/30/31/32)

Audit for task #2104. Cross-references the wire-level `XMIT_*`
transmission flags as they appear in upstream rsync 3.4.1
(`target/interop/upstream-src/rsync-3.4.1/rsync.h`) against the
oc-rsync implementation in `crates/protocol/src/flist/`. The goal is
a single, definitive table of which flag bits are valid on protocols
28, 29, 30, 31, and 32, where the encoding diverges, and where this
crate's reader/writer may mismatch upstream.

The task description references `FLIST_TOP_DIR`, `FLIST_HLINKED`,
`FLIST_SAME_NAME`, `FLIST_EXTENDED_FLAGS`, `FLIST_EXTRA_BLOCKS`, and
`FLIST_NEW_BUF`. Upstream does not define any of these as wire flags.
The wire bits are spelled `XMIT_*` (`rsync.h:47-73`) and that name is
used throughout this document. `FLIST_TEMP`, `FLIST_START`, and
`FLIST_LINEAR` exist in upstream `flist.c` but are in-memory growth
constants for `flist_new()`/`flist_expand()`, not wire flags. There is
no `FLIST_EXTRA_BLOCKS` or `FLIST_NEW_BUF` in the upstream tree, in
this crate, or in the protocol 32 wire spec; sections covering them
are documented as "not on the wire" rather than left ambiguous.

## 1. Source of truth

Upstream definitions live at `rsync.h:45-74` (XMIT bits) and
`compat.c:117-124` (CF capability flags that gate XMIT layout).

| Symbol | Bit | Upstream proto range (`rsync.h` comment) |
|---|---|---|
| `XMIT_TOP_DIR` | 0 | all |
| `XMIT_SAME_MODE` | 1 | all |
| `XMIT_SAME_RDEV_pre28` | 2 | 20-27 only |
| `XMIT_EXTENDED_FLAGS` | 2 | 28+ |
| `XMIT_SAME_UID` | 3 | all |
| `XMIT_SAME_GID` | 4 | all |
| `XMIT_SAME_NAME` | 5 | all |
| `XMIT_LONG_NAME` | 6 | all |
| `XMIT_SAME_TIME` | 7 | all |
| `XMIT_SAME_RDEV_MAJOR` | 8 | 28+ (devices only) |
| `XMIT_NO_CONTENT_DIR` | 8 | 30+ (dirs only) |
| `XMIT_HLINKED` | 9 | 28+ (non-dirs) |
| `XMIT_SAME_DEV_pre30` | 10 | 28-29 |
| `XMIT_USER_NAME_FOLLOWS` | 10 | 30+ |
| `XMIT_RDEV_MINOR_8_pre30` | 11 | 28-29 |
| `XMIT_GROUP_NAME_FOLLOWS` | 11 | 30+ |
| `XMIT_HLINK_FIRST` | 12 | 30+ (HLINKED non-dirs) |
| `XMIT_IO_ERROR_ENDLIST` | 12 | 31+ (with `XMIT_EXTENDED_FLAGS`); also 30 with CF flag `f` |
| `XMIT_MOD_NSEC` | 13 | 31+ |
| `XMIT_SAME_ATIME` | 14 | any (gated by `--atimes`) |
| `XMIT_UNUSED_15` | 15 | reserved |
| `XMIT_RESERVED_16` | 16 | varint only, future fileflags |
| `XMIT_CRTIME_EQ_MTIME` | 17 | varint only, gated by `--crtimes` |

oc-rsync mirrors these at `crates/protocol/src/flist/flags.rs:21-158`.
Bit-share collisions (2, 8, 10, 11, 12) are encoded as separate Rust
constants with distinct doc comments, and disambiguated at decode time
by the negotiated protocol version and entry type. Compat capability
flags live at `crates/protocol/src/compatibility/flags.rs:34-50` and
match `compat.c:117-124` 1:1 (`CF_INC_RECURSE`, `CF_SYMLINK_TIMES`,
`CF_SYMLINK_ICONV`, `CF_SAFE_FLIST`, `CF_AVOID_XATTR_OPTIM`,
`CF_CHKSUM_SEED_FIX`, `CF_INPLACE_PARTIAL_DIR`,
`CF_VARINT_FLIST_FLAGS`, `CF_ID0_NAMES`).

## 2. Per-protocol flag validity

`Y` = bit is meaningful on this protocol per upstream `rsync.h`; `-`
= reserved or unaddressable; `(varint)` = only addressable when
`CF_VARINT_FLIST_FLAGS` is negotiated, which requires protocol >= 30
and capability `'v'` in the client info string
(`compat.c:729-732,741`).

| Bit | Symbol | 28 | 29 | 30 | 31 | 32 |
|---|---|---|---|---|---|---|
| 0 | `XMIT_TOP_DIR` | Y | Y | Y | Y | Y |
| 1 | `XMIT_SAME_MODE` | Y | Y | Y | Y | Y |
| 2 | `XMIT_EXTENDED_FLAGS` | Y | Y | Y | Y | Y |
| 3 | `XMIT_SAME_UID` | Y | Y | Y | Y | Y |
| 4 | `XMIT_SAME_GID` | Y | Y | Y | Y | Y |
| 5 | `XMIT_SAME_NAME` | Y | Y | Y | Y | Y |
| 6 | `XMIT_LONG_NAME` | Y | Y | Y | Y | Y |
| 7 | `XMIT_SAME_TIME` | Y | Y | Y | Y | Y |
| 8 | `XMIT_SAME_RDEV_MAJOR` (dev) | Y | Y | Y | Y | Y |
| 8 | `XMIT_NO_CONTENT_DIR` (dir) | - | - | Y | Y | Y |
| 9 | `XMIT_HLINKED` | Y | Y | Y | Y | Y |
| 10 | `XMIT_SAME_DEV_pre30` | Y | Y | - | - | - |
| 10 | `XMIT_USER_NAME_FOLLOWS` | - | - | Y | Y | Y |
| 11 | `XMIT_RDEV_MINOR_8_pre30` | Y | Y | - | - | - |
| 11 | `XMIT_GROUP_NAME_FOLLOWS` | - | - | Y | Y | Y |
| 12 | `XMIT_HLINK_FIRST` | - | - | Y | Y | Y |
| 12 | `XMIT_IO_ERROR_ENDLIST` | - | - | Y\* | Y | Y |
| 13 | `XMIT_MOD_NSEC` | - | - | - | Y | Y |
| 14 | `XMIT_SAME_ATIME` | Y | Y | Y | Y | Y |
| 15 | `XMIT_UNUSED_15` | - | - | - | - | - |
| 16 | `XMIT_RESERVED_16` | - | - | (varint) | (varint) | (varint) |
| 17 | `XMIT_CRTIME_EQ_MTIME` | - | - | (varint) | (varint) | (varint) |

\* On protocol 30, `XMIT_IO_ERROR_ENDLIST` is reachable only when the
peer advertises capability `'f'` in the client-info string, which
upstream sets via `CF_SAFE_FLIST` in `compat.c:719-720`. For protocol
31+ it is implicit (`compat.c:775` flips `use_safe_inc_flist` on for
proto >= 31 even without `CF_SAFE_FLIST`).

## 3. Encoding differences

### 3.1 Protocol 28-29: legacy two-byte header

Per upstream `flist.c:send_file_entry()` and the receiver branch at
`flist.c:recv_file_entry()`, on protocols 28 and 29 the flag word is:

1. One unsigned byte for bits 0-7 (the "primary" byte).
2. If bit 2 (`XMIT_EXTENDED_FLAGS`) is set, one additional unsigned
   byte for bits 8-15 (the "extended" byte).

A primary byte of `0x00` is the end-of-list sentinel. Bits 16-23
(`XMIT_RESERVED_16`, `XMIT_CRTIME_EQ_MTIME`) cannot be transmitted -
there is no third byte and no varint capability on these protocols.
oc-rsync implements this branch in
`crates/protocol/src/flist/read/flags.rs:101-107`:

```text
} else if self.protocol.as_u8() >= 28 && (flags_value as u8 & XMIT_EXTENDED_FLAGS) != 0 {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    (buf[0], 0u8)
}
```

The protocol-version check at `read/flags.rs:101` is the single guard
preventing protocol 27-and-earlier peers from being misread; on 28+
the extended byte is read whenever bit 2 is set.

### 3.2 Protocol 30: gated varint

Protocol 30 keeps the legacy two-byte layout *unless* both peers
advertise `'v'` in the client-info string and the server replies with
`CF_VARINT_FLIST_FLAGS` in the compat-flags varint
(`compat.c:729-732`). When that happens, the header collapses into a
single varint that carries bits 0-23 in one read. Bits 16-17 become
addressable, enabling `XMIT_CRTIME_EQ_MTIME` for `--crtimes`.

oc-rsync gates this at `crates/protocol/src/flist/read/flags.rs:36-39`
(`use_varint_flags()` checks `CompatibilityFlags::VARINT_FLIST_FLAGS`)
and at `read/flags.rs:59-65`:

```text
let flags_value = if use_varint {
    read_varint(reader)?
} else {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    buf[0] as i32
};
```

### 3.3 Protocol 31-32: varint preferred, safe flist always on

Protocol 31 and 32 behave identically to protocol 30 with respect to
flag-header layout: legacy two-byte by default, varint when
`CF_VARINT_FLIST_FLAGS` is negotiated. The protocol bumps that matter
for the flag matrix are:

- `XMIT_MOD_NSEC` (bit 13) becomes legal for the first time on
  protocol 31, allowing nanosecond mtimes.
- `use_safe_inc_flist` is unconditional on protocol >= 31
  (`compat.c:775`), so `XMIT_IO_ERROR_ENDLIST` (bit 12 with bit 2 set)
  is a recognised mid-stream error sentinel even without
  `CF_SAFE_FLIST`. oc-rsync mirrors this at
  `crates/protocol/src/flist/read/flags.rs:43-47`:

```text
self.compat_flags
    .is_some_and(|f| f.contains(CompatibilityFlags::SAFE_FILE_LIST))
    || self.protocol.safe_file_list_always_enabled()
```

Protocol 32 introduces no new flag bits and no new layout. The
upstream constant `PROTOCOL_VERSION 32` (`rsync.h:114`) is the
current maximum; `SUBPROTOCOL_VERSION 0` (`rsync.h:128`) signals a
final release.

### 3.4 End-of-list sentinel

On legacy encoding, primary byte `0x00` ends the list. On varint
encoding, a varint of `0` is followed by a second varint giving an
I/O error code (zero means a clean end). oc-rsync encodes both forms
at `crates/protocol/src/flist/read/flags.rs:75-91`. Mid-stream error
sentinels in safe-flist mode are detected at
`read/flags.rs:148-170` against the upstream sentinel
`(XMIT_EXTENDED_FLAGS | (XMIT_IO_ERROR_ENDLIST << 8))`.

## 4. oc-rsync handling

| Concern | Upstream | oc-rsync site | Status |
|---|---|---|---|
| Primary byte read | `flist.c:recv_file_entry()` | `read/flags.rs:60-65` | matches |
| Extended byte read on proto >= 28 with bit 2 | `flist.c:recv_file_entry()` | `read/flags.rs:101-107` | matches |
| Varint flag header when `CF_VARINT_FLIST_FLAGS` | `compat.c:741`, `flist.c` | `read/flags.rs:36-39,59-65,96-100` | matches |
| EOL sentinel (legacy) | primary == 0 | `read/flags.rs:75-91` | matches |
| EOL sentinel (varint) | primary == 0 then error code | `read/flags.rs:76-90` | matches |
| Mid-stream error sentinel (safe-flist) | `flist.c:recv_file_entry()` | `read/flags.rs:148-170` | matches |
| Safe-flist auto-on for proto >= 31 | `compat.c:775` | `read/flags.rs:43-47` | matches via `safe_file_list_always_enabled()` |
| INC_RECURSE state machine | `flist.c:recv_file_list()`, `io.c:read_a_msg()` | `crates/protocol/src/flist/incremental/mod.rs:80-138`, `incremental/streaming.rs` | matches; receiver-side validated, sender-side wired but interop-gated |
| `XMIT_HLINK_FIRST` on protocol 30+ | `rsync.h:64` | `flist.c:hlink.c match_hard_links()`, `flags.rs:331-337` (`set_hlink_first`) | matches |
| `XMIT_NO_CONTENT_DIR` on protocol 30+ dirs | `rsync.h:58` | `flags.rs:357-360` | matches |
| `XMIT_MOD_NSEC` on protocol 31+ | `rsync.h:66` | `flags.rs:292-296` | matches |
| `XMIT_CRTIME_EQ_MTIME` (varint only) | `rsync.h:73` | `flags.rs:384-388` | matches |
| `XMIT_RESERVED_16` | `rsync.h:72` | `flags.rs:148` (constant only, never set) | matches |

The full `FileFlags` struct at `flist/flags.rs:160-389` exposes
typed accessors for every bit listed above. The varint-aware encoder
on the sender side is at `crates/protocol/src/flist/write/encoding.rs`
and `write/xflags.rs`; field gating on encode is at
`write/metadata.rs` (covered separately by
`docs/audits/flist-flags-per-proto.md`).

## 5. INC_RECURSE interactions

Incremental recursion (`CF_INC_RECURSE`, bit 0 of compat flags;
`compat.c:117,712,745`) is orthogonal to flag *layout* but interacts
with several flag *semantics*:

- **Per-segment file lists.** With INC_RECURSE, `flist.c:2117` writes
  `NDX_FLIST_OFFSET - dir_ndx` to begin a new segment, and
  `flist.c:2541,2172` write `NDX_FLIST_EOF` between segments. These
  are wire indices, not XMIT bits; oc-rsync handles them in
  `crates/protocol/src/flist/incremental/streaming.rs`. There is no
  `FLIST_EXTRA_BLOCKS` or `FLIST_NEW_BUF` upstream - those names
  appear only in this task's description and are not part of the wire
  protocol.
- **`XMIT_HLINK_FIRST` (bit 12).** With INC_RECURSE, hardlink leaders
  must be re-emitted in sorted order across segments because the
  wire order no longer reflects readdir order. Upstream
  `hlink.c:match_hard_links()` reassigns `FLAG_HLINK_FIRST` after
  sorting; oc-rsync does the same via `FileFlags::set_hlink_first` at
  `flist/flags.rs:331-337` and the hardlink table at
  `flist/hardlink/table.rs`.
- **`XMIT_NO_CONTENT_DIR` (bit 8 for dirs).** Used on protocol 30+ to
  mark directories that arrive in the parent segment but whose
  contents will appear in a later segment. Without this bit the
  receiver would either descend prematurely or treat the dir as
  empty. oc-rsync exposes this via `FileFlags::no_content_dir` at
  `flist/flags.rs:357-360`.
- **`XMIT_IO_ERROR_ENDLIST` (bit 12 with `XMIT_EXTENDED_FLAGS`).**
  Required for safe mid-segment error reporting once segments stream.
  Always-on for protocol >= 31; protocol 30 needs `CF_SAFE_FLIST`.
- **`CF_VARINT_FLIST_FLAGS` independence.** INC_RECURSE does not
  imply varint flags. A protocol 30 INC_RECURSE pair without `'v'`
  capability still uses the two-byte legacy header, just split across
  multiple segments. oc-rsync's reader handles both shapes per
  segment.

## 6. Gaps and potential mismatches

The audit found no missing-bit gaps. Items below are areas to watch:

1. **Bit 2 disambiguation on protocol 27 and earlier.** The crate
   defines `XMIT_SAME_RDEV_PRE28` (`flags.rs:40`) for completeness
   but the reader at `read/flags.rs:101` treats bit 2 as
   `XMIT_EXTENDED_FLAGS` for any `protocol >= 28`. Protocols < 28
   are out of scope for oc-rsync (negotiation rejects them). No fix
   needed; document the rejection path.
2. **Bit-share by entry type at bit 8.** `XMIT_SAME_RDEV_MAJOR` and
   `XMIT_NO_CONTENT_DIR` share bit 8. Disambiguation requires the
   entry's file-type, which is encoded via the `mode` field decoded
   later in the entry. The reader at `read/metadata.rs` must use the
   final mode to decide which interpretation applies; verify this
   covers all device/dir corner cases under property tests.
3. **`XMIT_IO_ERROR_ENDLIST` on protocol 30 without `CF_SAFE_FLIST`.**
   Upstream returns `RERR_PROTOCOL` (`flist.c:recv_file_entry()`)
   when an unrecognised flag pair is read. oc-rsync's
   `check_error_marker` at `read/flags.rs:148-170` already returns
   `InvalidData` in this case; confirm exit-code mapping yields
   protocol error (1:1 with upstream).
4. **`XMIT_RESERVED_16`.** Defined at `flags.rs:148` but never set on
   the writer side and never gated on the reader side. Adding any
   future fileflags bit must also update `write/xflags.rs` and
   `write/metadata.rs`.
5. **INC_RECURSE sender for push transfers.** Receiver-side is
   validated end-to-end; sender-side is implemented but interop-gated
   via `build_capability_string(!is_sender)` in `setup.rs`. This is
   a behavioural gap, not a flag-matrix gap - the wire-flag handling
   itself is symmetric.
6. **`XMIT_SAME_ATIME` (bit 14) is reachable on every protocol but
   only valid when `--atimes` is in effect.** Upstream gates the
   write at `flist.c:send_file_entry()`; oc-rsync gates it in
   `flist/write/xflags.rs`. Verify the reader rejects the bit when
   `--atimes` was not requested - the comment at `flags.rs:131-133`
   says "restricted by command-line option" but the reader does not
   currently enforce this. Document or add a guard.
7. **`XMIT_CRTIME_EQ_MTIME` (bit 17) requires both protocol >= 30 and
   `CF_VARINT_FLIST_FLAGS`.** Upstream errors out at `compat.c:750`
   if `--crtimes` is requested without varint flags. oc-rsync
   enforces this at the sender (`write/xflags.rs:254`); confirm the
   client-side option parser also rejects the combination so the
   receiver is never asked to honour an unreachable bit.

## 7. Summary

- The wire-level flag set is `XMIT_*` (`rsync.h:47-73`); upstream has
  no `FLIST_TOP_DIR`/`FLIST_HLINKED`/`FLIST_SAME_NAME`/`FLIST_EXTRA_BLOCKS`/`FLIST_NEW_BUF` symbols. The `FLIST_*` family in
  `flist.c` covers in-memory growth constants only.
- All XMIT bits 0-15 are valid on protocols 28-32; bits 16-17
  require protocol 30+ with `CF_VARINT_FLIST_FLAGS`.
- Bit-share collisions at positions 2, 8, 10, 11, 12 are protocol- or
  entry-type-dependent and are the highest-risk audit points.
- oc-rsync's reader (`flist/read/flags.rs`) and incremental processor
  (`flist/incremental/mod.rs`) match upstream semantics. The
  remaining items in section 6 are observability and validation gaps,
  not wire-format bugs.

References: `target/interop/upstream-src/rsync-3.4.1/rsync.h:45-128`,
`target/interop/upstream-src/rsync-3.4.1/compat.c:117-775`,
`crates/protocol/src/flist/flags.rs:1-617`,
`crates/protocol/src/flist/read/flags.rs:1-171`,
`crates/protocol/src/flist/incremental/mod.rs:1-604`,
`crates/protocol/src/compatibility/flags.rs:34-63`.
