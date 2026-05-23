# RP28.a - Inventory of `protocol_version < 30` Code Paths

Audit-only document. No code changes. Foundational survey for the RP28 series, which
adds an automated rsync 2.x interop fixture (protocol 28 and 29).

## Scope

The interop matrix tests rsync 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2 - all of which advertise
protocol 30 or higher. The codebase still ships explicit gates at protocol 28 and 29
because:

- Upstream `rsync.h` defines `MIN_PROTOCOL_VERSION 20` and `OLD_PROTOCOL_VERSION 25`
  (see `target/interop/upstream-src/rsync-3.4.1/rsync.h:147-148`).
- This implementation pins the floor at protocol 28 via
  `OLDEST_SUPPORTED_PROTOCOL = 28`
  (see `crates/protocol/src/version/constants.rs:7`).
- The last rsync 2.x release (2.6.9, 2009) advertises protocol 29; rsync 1.x advertises
  protocol 28. A real 2.x peer therefore still triggers every gate enumerated below.

Reference for upstream `MIN_PROTOCOL_VERSION` semantics:

```
target/interop/upstream-src/rsync-3.4.1/rsync.h:147
target/interop/upstream-src/rsync-3.4.2/rsync.h:147
```

## Central Constants and Capability Helpers

These are the single source of truth that all gates ultimately derive from. They are
not themselves "branches" - listed for orientation.

| Symbol | File:Line | Value / Predicate |
|--------|-----------|-------------------|
| `OLDEST_SUPPORTED_PROTOCOL` | `crates/protocol/src/version/constants.rs:7` | `28` |
| `NEWEST_SUPPORTED_PROTOCOL` | `crates/protocol/src/version/constants.rs:9` | `32` |
| `FIRST_BINARY_NEGOTIATION_PROTOCOL` | `crates/protocol/src/version/constants.rs:11` | `30` |
| `MAXIMUM_PROTOCOL_ADVERTISEMENT` | `crates/protocol/src/version/constants.rs:16` | `40` |
| `ProtocolVersion::uses_binary_negotiation` | `crates/protocol/src/version/protocol_version/capabilities.rs:12` | `as_u8() >= 30` |
| `ProtocolVersion::uses_legacy_ascii_negotiation` | `.../capabilities.rs:19` | `as_u8() < 30` |
| `ProtocolVersion::uses_varint_encoding` | `.../capabilities.rs:31` | `as_u8() >= 30` |
| `ProtocolVersion::uses_fixed_encoding` | `.../capabilities.rs:39` | `as_u8() < 30` |
| `ProtocolVersion::supports_sender_receiver_modifiers` | `.../capabilities.rs:50` | `as_u8() >= 29` |
| `ProtocolVersion::supports_perishable_modifier` | `.../capabilities.rs:61` | `as_u8() >= 30` |
| `ProtocolVersion::uses_old_prefixes` | `.../capabilities.rs:75` | `as_u8() < 29` |
| `ProtocolVersion::supports_flist_times` | `.../capabilities.rs:86` | `as_u8() >= 29` |
| `ProtocolVersion::supports_iflags` | `.../capabilities.rs:99` | `as_u8() >= 29` |
| `ProtocolVersion::supports_multi_phase` | `.../capabilities.rs:112` | `as_u8() >= 29` |
| `ProtocolVersion::supports_extended_flags` | `.../capabilities.rs:120` | `as_u8() >= 28` |
| `ProtocolVersion::uses_varint_flist_flags` | `.../capabilities.rs:130` | `as_u8() >= 30` |
| `ProtocolVersion::uses_safe_file_list` | `.../capabilities.rs:142` | `as_u8() >= 30` |
| `ProtocolVersion::supports_generator_messages` | `.../capabilities.rs:189` | `as_u8() >= 30` |
| `ProtocolVersion::supports_inline_hardlinks` | `.../capabilities.rs:217` | `as_u8() >= 30` |
| `ProtocolVersion::supports_checksum_negotiation` | `.../capabilities.rs:257` | `as_u8() >= 30` |
| `ProtocolVersion::supports_inc_recurse` | `.../capabilities.rs:286` | `as_u8() >= 30` |

## Wire-Format Gates

### W1. NDX codec dispatch (4-byte LE vs delta-encoded varint)

- `crates/protocol/src/codec/ndx/codec.rs:88` - `LegacyNdxCodec::new` panics if
  `protocol_version >= 30`.
- `crates/protocol/src/codec/ndx/codec.rs:149` - `ModernNdxCodec::new` panics if
  `protocol_version < 30`.
- `crates/protocol/src/codec/ndx/codec.rs:294` - `NdxCodecEnum::new` dispatches:
  ```rust
  if protocol_version < 30 {
      Self::Legacy(LegacyNdxCodec::new(protocol_version))
  } else {
      Self::Modern(ModernNdxCodec::new(protocol_version))
  }
  ```

**Upstream**: `io.c:2246-2248` (`read_int(f)` for pre-30) and `io.c:2243-2287`
(`write_ndx`).

**Behaviour difference**: every NDX on the wire. Legacy = `i32_le`. Modern = single byte
`0x00` for `NDX_DONE` plus delta-encoded variable-length frames.

**Test coverage**: `crates/protocol/tests/ndx_codec_comprehensive.rs` and
`crates/protocol/src/codec/ndx/tests.rs:141` (`#[should_panic]` covers the codec gate).
Golden v28/v29 wire bytes exist in `crates/protocol/tests/golden_protocol_v28_wire.rs`
and `golden_protocol_v29_wire.rs`.

### W2. NDX_DONE goodbye sentinel

- `crates/protocol/src/codec/ndx/goodbye.rs:25` - `write_goodbye`:
  ```rust
  if protocol_version < 30 {
      writer.write_all(&NDX_DONE_LEGACY_BYTES)   // [0xFF, 0xFF, 0xFF, 0xFF]
  } else {
      writer.write_all(&[NDX_DONE_MODERN_BYTE])  // [0x00]
  }
  ```
- `crates/protocol/src/codec/ndx/goodbye.rs:49` - matching `read_goodbye`.

**Upstream**: `main.c:875-906` `read_final_goodbye()`.

**Behaviour difference**: 4 bytes vs 1 byte at end of every transfer.

**Test coverage**: `crates/protocol/tests/goodbye_handshake.rs`.

### W3. Varint integer fallback

- `crates/protocol/src/varint/encode.rs:146` - `write_varint30_int`:
  fixed 4-byte LE when `protocol_version < 30`, else `write_varint`.
- `crates/protocol/src/varint/decode.rs:174` - matching `read_varint30_int`.

**Upstream**: `io.h` inline `write_varint30()` / `read_varint30()`.

**Behaviour difference**: integer width on every protocol field that uses
`varint30`.

**Test coverage**: `crates/protocol/tests/varint_vstring_codec.rs`,
`proptest_varint_boundaries.rs`, `proptest_codec_roundtrips.rs:224`.

### W4. Protocol codec dispatch

- `crates/protocol/src/codec/protocol/dispatch.rs:72` - `create_protocol_codec`
  picks `LegacyProtocolCodec` for `protocol_version < 30`, otherwise
  `ModernProtocolCodec`.
- `crates/protocol/src/codec/protocol/mod.rs:71` - `ProtocolCodec::is_legacy`.

**Upstream**: implicit; the per-field dispatch (size, mtime, long-name length) is
inlined across `flist.c` and `io.c`.

**Behaviour difference**: composite - dictates `write_file_size`, `write_mtime`,
`write_long_name_len` field widths.

**Test coverage**: `crates/protocol/tests/protocol_version_compat.rs`,
`crates/protocol/tests/protocol_v28_v31_compat.rs`,
`crates/protocol/tests/protocol_v29_compat.rs`.

### W5. File entry: name length encoding

- `crates/protocol/src/wire/file_entry/encode.rs:193` - `encode_name` writes
  `write_varint(suffix_len)` for >= 30 else `(suffix_len as i32).to_le_bytes()`.
- `crates/protocol/src/wire/file_entry_decode/name.rs:59` - matching decode.

**Upstream**: `flist.c:send_file_entry()` lines 580-610;
`flist.c:recv_file_entry()` 800-850.

**Behaviour difference**: long-name length is varint vs fixed `i32` LE.

**Test coverage**: `crates/protocol/tests/proptest_file_entry_roundtrip.rs`,
`crates/protocol/tests/unicode_wire_format.rs`,
`crates/protocol/tests/symlink_target_encoding.rs`.

### W6. File entry: size encoding

- `crates/protocol/src/wire/file_entry/encode.rs:227` - `encode_size` writes
  `write_varlong(size, 3)` for >= 30 else `write_longint(size)`.
- `crates/protocol/src/wire/file_entry_decode/size.rs:36` - matching decode.

**Upstream**: `flist.c:send_file_entry()` line 580
`write_varlong30(f, F_LENGTH(file), 3)`.

**Behaviour difference**: 3+ byte varlong vs 4-byte longint (12 bytes for
> 32-bit values on pre-30).

**Test coverage**: `crates/protocol/tests/large_file_handling.rs`,
`crates/protocol/src/flist/write/tests/protocol_boundaries.rs:316`.

### W7. File entry: mtime encoding

- `crates/protocol/src/wire/file_entry/encode.rs:256` - `encode_mtime` writes
  `write_varlong(mtime, 4)` for >= 30 else `(mtime as i32).to_le_bytes()`.
- `crates/protocol/src/wire/file_entry_decode/timestamps.rs:49` - matching decode.

**Upstream**: `flist.c:send_file_entry()` lines 582-584.

**Behaviour difference**: varlong vs fixed 4-byte LE (Y2038 truncation on pre-30).

**Test coverage**: `crates/protocol/tests/proptest_file_entry_roundtrip.rs`,
`crates/protocol/tests/golden_protocol_v28_flist.rs`,
`crates/protocol/tests/golden_protocol_v29_flist.rs`.

### W8. File entry: uid/gid encoding (plus optional name)

- `crates/protocol/src/wire/file_entry/encode.rs:323` - `encode_uid` varint vs
  fixed 4-byte LE.
- `crates/protocol/src/wire/file_entry/encode.rs:341` - `encode_gid` mirrors.
- `crates/protocol/src/wire/file_entry_decode/ownership.rs:82` - matching
  `decode_owner_id` ID branch.
- `crates/protocol/src/wire/file_entry_decode/ownership.rs:88` - matching
  optional `XMIT_USER_NAME_FOLLOWS` / `XMIT_GROUP_NAME_FOLLOWS` strings (only
  exist on protocol 30+; pre-30 reuses those bit positions for
  `XMIT_SAME_DEV_PRE30` and `XMIT_RDEV_MINOR_8_PRE30`).

**Upstream**: `flist.c:880-902`.

**Behaviour difference**: varint vs fixed `i32`, plus the optional length-prefixed
owner-name string field disappears entirely on pre-30.

**Test coverage**: `crates/protocol/src/flist/read/metadata.rs` is exercised by
`flist/read/tests`; v28/v29 goldens cover the absence of the name string.

### W9. File entry: rdev minor and same-rdev for protocol 28-29

- `crates/protocol/src/wire/file_entry/flags.rs:112` - `XMIT_RDEV_MINOR_8_PRE30`
  set if `(28..30).contains(&protocol_version) && rdev_minor <= 0xFF`.
- `crates/protocol/src/wire/file_entry/flags.rs:138` - protocol-30+ uses
  `XMIT_HLINKED` / `XMIT_HLINK_FIRST` instead.
- `crates/protocol/src/wire/file_entry/encode.rs:385` - `encode_rdev` writes
  `write_varint(minor)` for >= 30, byte or 4-byte fallback for 28-29.
- `crates/protocol/src/wire/file_entry_decode/device.rs:47` - matching decode.

**Upstream**: `flist.c:send_file_entry()` 640-680, `recv_file_entry()` 910-945.

**Behaviour difference**: device-file rdev minor is 1 byte, 4 bytes, or varint
depending on protocol and flag, AND the flag bit at position 16 has different
meanings on pre-30 vs 30+.

**Test coverage**: `crates/protocol/tests/device_file_encoding.rs`.

### W10. File entry: hardlink encoding model

- `crates/protocol/src/flist/write/encoding.rs:201` - `write_hardlink_idx`
  early-returns for `protocol_version < 30` (modern uses inline index).
- `crates/protocol/src/flist/write/encoding.rs:234` - `write_hardlink_dev_ino`
  guard `protocol < 30 && protocol >= 28` (pre-30 ships `(dev+1, ino)`).
- `crates/protocol/src/flist/read/extras.rs:158` - matching `read_hardlink_idx`.
- `crates/protocol/src/flist/read/extras.rs:194` - matching
  `read_hardlink_dev_ino`.
- `crates/protocol/src/flist/write/xflags.rs:188` - protocol-30+ sets
  `XMIT_HLINKED` / `XMIT_HLINK_FIRST` in xflags; lines 195-204 protocol-28-29 sets
  `XMIT_HLINKED` / `XMIT_SAME_DEV_PRE30`.
- `crates/transfer/src/generator/file_list/hardlinks.rs:69` - on protocol 30+
  clears temp `dev/ino` after the leader/follower index is assigned.
- `crates/transfer/src/receiver/file_list/hardlinks.rs:7` and 81 -
  `normalize_pre30_hardlinks` collapses received `(dev, ino)` pairs into
  `hardlink_idx` and `XMIT_HLINK_FIRST` semantics so downstream code is uniform.

**Upstream**: `flist.c:send_file_entry()` 530-595 and 670-690; `hlink.c:idev_find`.

**Behaviour difference**: complete change in wire model. Pre-30 transmits raw
`(dev+1, ino)` longints after the entry's symlink-target slot; 30+ transmits a
varint index into the file list (with sentinel `u32::MAX` for the leader). Wrong
gating = silent hardlink-deduplication failure.

**Test coverage**: `crates/transfer/src/receiver/file_list/hardlinks.rs` unit
tests (`normalize_pre30_*`); `crates/protocol/tests/golden_protocol_v28_flist.rs`;
`crates/protocol/tests/protocol_v29_compat.rs`.

### W11. File entry: same-time / same-mode / etc. xflags bit layout

- `crates/protocol/src/flist/write/xflags.rs:156-170` - pre-30 reuses bits 17
  (`XMIT_RDEV_MINOR_8_PRE30`) and 8 (`XMIT_SAME_DEV_PRE30`) that 30+ uses for
  `XMIT_USER_NAME_FOLLOWS` / `XMIT_GROUP_NAME_FOLLOWS`. Setting the wrong bit
  in the wrong protocol desynchronises the reader.
- `crates/protocol/src/flist/write/xflags.rs:217` -
  `write_extended_xflags` skips entirely for `protocol < 30`.
- `crates/protocol/src/flist/write/xflags.rs:275` - `XMIT_NO_CONTENT_DIR` only
  for protocol 30+ on directories.

**Upstream**: `flist.c:send_file_entry()` 440-540 (xflags table).

**Behaviour difference**: identical bit number means different things between
pre-30 and 30+; mis-gating corrupts the decoder.

**Test coverage**: `crates/protocol/src/flist/write/tests/protocol_boundaries.rs`,
`crates/protocol/src/flist/flags.rs` accessor tests
(`flags_same_dev_pre30`, `flags_rdev_minor_8_pre30`).

### W12. File entry: varint vs 1-2 byte flag prelude

- `crates/protocol/src/version/protocol_version/capabilities.rs:130`
  (`uses_varint_flist_flags`) gates whether the per-entry flag word is varint
  (>= 30 with `COMPAT_VARINT_FLIST_FLAGS`) or 1-2 byte fixed (pre-30).
- Consumed by `crates/protocol/src/flist/write/encoding.rs:172` and
  `crates/protocol/src/flist/read/metadata.rs:172`.

**Upstream**: `flist.c:send_file_entry()` flag prelude; `compat.c:740`
`do_negotiated_strings`.

**Behaviour difference**: per-entry flag prelude varies between 1, 2, or 1+ varint
bytes.

**Test coverage**: `crates/protocol/tests/protocol_v30_compat.rs`,
`crates/protocol/tests/protocol_v29_compat.rs`.

### W13. Multi-byte length terminator for daemon argument stream

- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:54` -
  `b'\0'` for `protocol >= 30`, `b'\n'` for pre-30.
- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:166` -
  emits capability string only for `protocol >= 30`.
- `crates/daemon/src/daemon/sections/module_access/client_args.rs:25` - matching
  daemon-side reader picks NUL or newline terminator.

**Upstream**: `io.c:1292 read_args()`, `clientserver.c:348-349`.

**Behaviour difference**: terminator byte between every command-line argument
delivered to the daemon.

**Test coverage**: `crates/daemon/src/daemon/sections/module_access/` unit tests
and `crates/protocol/tests/daemon_negotiation.rs`.

### W14. `MSG_NO_SEND` is protocol 30+ only

- `crates/transfer/src/reader/multiplex.rs:84` (doc-only ref to upstream
  `sender.c:367-368`).
- `crates/transfer/src/writer/server.rs:189` (doc-only ref).

The actual gate is upstream-side; we only send `MSG_NO_SEND` when the connection
is multiplexed (which is itself implicit on protocol >= 30 server-side). Listed
for completeness; no in-tree branch is gated against `< 30` here.

### W15. Input-multiplex activation threshold (server-side)

- `crates/transfer/src/receiver/mod.rs:546` (doc-only ref to upstream
  `main.c:1167-1168`). The actual gate uses
  `supports_multiplex_io()` which is protocol >= 23 (client mode) but server
  mode requires >= 30 - currently encoded in `receiver/mod.rs` via the
  `client_mode` branch.

## Capability Negotiation Gates

### C1. Binary negotiation / compatibility-flags exchange

- `crates/transfer/src/setup/mod.rs:158` - the entire compat-flags + algorithm
  negotiation block is gated `if protocol.uses_binary_negotiation()`. For pre-30
  the fallback sets `checksum = MD4`, `compression = Zlib` (or none) with no
  vstring exchange.
- `crates/transfer/src/setup/types.rs:11`, `:14` -
  `SetupResult.negotiated_algorithms` and `.compat_flags` are `None` for pre-30.
- `crates/transfer/src/handshake.rs:38`, `:41` - mirror in `HandshakeResult`.

**Upstream**: `compat.c:setup_protocol()`; `compat.c:740 do_negotiated_strings`.

**Behaviour difference**: pre-30 does not perform vstring algorithm negotiation
and skips compat flag exchange entirely.

**Test coverage**: `crates/transfer/src/setup/tests.rs:1570`,
`crates/transfer/src/tests/multiplex_protocol_version.rs:210`,
`crates/transfer/src/tests/negotiated_algorithms.rs:137`.

### C2. Protocol restrictions: `--acls`, `--xattrs`, append, delete-default

- `crates/transfer/src/setup/restrictions.rs:89` - `if version < 30 {` block:
  - `append_mode == 1` adjusted to `2` (upstream compat.c:653-654).
  - `--acls` (non-local) rejected with upstream-format error.
  - `--xattrs` (non-local) rejected with upstream-format error.
- `crates/transfer/src/setup/restrictions.rs:114` - `delete_mode` default
  selects `delete_before` on pre-30 vs `delete_during` on 30+.

**Upstream**: `compat.c:652-668` and `compat.c:671-676`.

**Behaviour difference**: pre-30 peer cannot use `-A`/`-X` over the network and
defaults to a different deletion phase.

**Test coverage**: `crates/transfer/src/setup/restrictions.rs` unit tests
(`protocol_29_rejects_acls`, `protocol_29_rejects_xattrs`,
`protocol_28_rejects_fuzzy`, `protocol_28_rejects_basis_dir_with_inplace`,
`protocol_28_rejects_multiple_basis_dirs`, `protocol_28_rejects_prune_empty_dirs`,
`append_mode_1_becomes_2_below_protocol_30`,
`delete_defaults_to_delete_before_below_30`,
`delete_defaults_to_delete_during_at_30_plus`).
Also `crates/transfer/tests/acl_negotiation_missing_remote.rs:90`.

### C3. Protocol restrictions: `--fuzzy`, `--inplace+basis-dir`, multi `--*-dest`, `--prune-empty-dirs`

- `crates/transfer/src/setup/restrictions.rs:122` - `if version < 29 {` block
  rejects each of the four features above with upstream-format errors.

**Upstream**: `compat.c:678-709`.

**Behaviour difference**: pre-29 peer cannot use any of these features.

**Test coverage**: same module's unit tests.

### C4. `INC_RECURSE` is protocol 30+ only

- `crates/protocol/src/version/protocol_version/capabilities.rs:286`
  (`supports_inc_recurse`).
- Generator/receiver consumers in
  `crates/transfer/src/receiver/file_list/receive.rs:55-60` and
  `crates/transfer/src/generator/file_list/...` gate INC_RECURSE behaviour on
  the negotiated compat flag, which itself is unavailable below protocol 30.

**Upstream**: `compat.c:720 set_allow_inc_recurse()`.

**Behaviour difference**: pre-30 peer is forced into batch flist mode (no
streaming file list).

**Test coverage**: capability-string golden tests
(`crates/protocol/tests/protocol_v30_compat.rs`).

### C5. Daemon greeting digest list

- `crates/daemon/src/daemon/sections/greeting.rs:22` - daemon greeting omits the
  digest list when `version.as_u8() < 30`; pre-30 clients assume MD4 by convention
  (upstream `csprotocol.txt`).
- `crates/daemon/src/auth.rs:160`, `crates/daemon/src/daemon/sections/module_access/authentication.rs:30` -
  doc-only references to `compat.c:858 protocol_version >= 30 ? "md5" : "md4"`.

### C6. Daemon auth digest selection

- `crates/core/src/auth/mod.rs:178` - `default_legacy_digest` returns `Md5` for
  `>= 30`, `Md4` for `< 30`.
- `crates/core/src/auth/mod.rs:244` - `verify_daemon_auth_response` disambiguates
  MD4 vs MD5 by protocol version when the response length is ambiguous (both
  produce 22-character base64).

**Upstream**: `compat.c:858`.

**Behaviour difference**: silently picks the wrong hash family for the
challenge-response if mis-gated, breaking daemon authentication.

**Test coverage**: `crates/core/src/auth/mod.rs` includes tests, plus
`crates/core/tests/daemon_client_interop.rs:441` and `:754`.

### C7. Daemon client-argument terminator

See W13 (`arguments.rs:54`, `client_args.rs:25`). Listed under capability
negotiation as well because the daemon wire framing is part of the connection
setup.

## File-List Framing Gates

### F1. Sort comparator (pre-29 vs 29+)

- `crates/protocol/src/flist/sort.rs:20` (module doc), `:99` (function doc),
  `:227` (runtime branch). When `protocol_pre29` is true the comparator becomes a
  plain lexicographic byte comparison without file-before-directory semantics or
  implicit trailing `/`.
- `crates/transfer/src/receiver/file_list/receive.rs:102` -
  `let pre29 = self.protocol.as_u8() < 29;` consumer.
- `crates/batch/src/replay/mod.rs:124` - `let pre29 =
  reader.config().protocol_version < 29;` consumer.

**Upstream**: `flist.c:3223 protocol_version >= 29 ? t_PATH : t_ITEM`.

**Behaviour difference**: a mis-sorted file list produces NDX mismatches across
the entire transfer - silent data placement errors.

**Test coverage**: `crates/protocol/src/flist/sort.rs` unit tests
(`pre29_no_files_before_dirs`, `pre29_dot_first`, `pre29_sort_mixed_entries`,
`pre29_sort_order_golden`).

### F2. `io_error` flag after file list (pre-30 only)

- `crates/transfer/src/receiver/file_list/receive.rs:62-72` - reads a fixed
  4-byte `io_error` integer for pre-30; on 30+ the same information is delivered
  via `MSG_IO_ERROR` / `SAFE_FILE_LIST`.
- `crates/transfer/src/generator/protocol_io.rs:67-87` - matching
  `send_io_error_flag`.

**Upstream**: `flist.c:2517-2518` (sender), `flist.c:2738-2742` (receiver).

**Behaviour difference**: 4 extra bytes on the wire on pre-30; their absence on
30+ is signalled out-of-band.

**Test coverage**: `crates/transfer/src/receiver/tests/file_list/proto_io_error.rs`
(includes a protocol-29 case at `:50`),
`crates/transfer/src/generator/tests.rs:1362` and `:1388`.

### F3. iflags after NDX (29+)

- `crates/transfer/src/receiver/wire.rs:229`, `:340` - `if protocol_version >= 29`
  reads 2-byte shortint of iflags after each NDX; pre-29 defaults to
  `ITEM_TRANSFER`.
- `crates/transfer/src/generator/item_flags.rs:149` - matching `ItemFlags::read`.
- `crates/transfer/src/receiver/transfer/sync.rs:151` (doc-only).

**Upstream**: `sender.c:180-187 write_ndx_and_attrs()`.

**Behaviour difference**: 2 bytes per file index, controlling itemize output and
transfer skipping logic.

**Test coverage**: `crates/transfer/src/tests/multiplex_protocol_version.rs:210`,
`crates/protocol/tests/protocol_v28_v31_compat.rs`.

### F4. Multi-phase transfer (29+)

- `crates/transfer/src/generator/transfer/transfer_loop.rs:54-60` - `max_phase`
  is 2 on protocol >= 29 (via `supports_iflags()`), 1 on pre-29.
- `crates/transfer/src/generator/transfer/goodbye.rs:22-41` (doc) - pre-29
  reads goodbye with `read_int()`; 29+ uses `read_ndx_and_attrs()`. (Both routes
  collapse to the protocol-30 dispatch in this codebase via `read_goodbye`.)

**Upstream**: `sender.c:210 max_phase = protocol_version >= 29 ? 2 : 1`.

**Behaviour difference**: pre-29 does not perform the second `SUM_LENGTH = 16`
re-check phase; failed files are not redone with a stronger checksum.

**Test coverage**: `crates/protocol/tests/protocol_v29_compat.rs`.

### F5. Flist build/xfer timing stats (29+)

- `crates/transfer/src/generator/timing.rs:9` (doc).
- `crates/transfer/src/generator/transfer/stats.rs:21` (doc; the runtime branch
  is in `TransferStats::write_to` against protocol).
- `crates/batch/src/format/stats.rs:37` and `:52` - 4-byte flist build/xfer time
  fields appear only on protocol >= 29.

**Upstream**: `main.c:347-357 handle_stats()`.

**Behaviour difference**: two extra varlong fields at end of transfer.

**Test coverage**: `crates/batch/src/format/stats.rs` round-trip tests,
`crates/protocol/tests/protocol_v29_compat.rs`.

## Compression Gates

### Z1. zlib chunk advance differs at protocol 31, but not pre-30

- `crates/protocol/src/wire/compressed_token/zlib_codec.rs:131` - inner
  `protocol_version >= 31` is unrelated to RP28.a (listed for context).

### Z2. Compression negotiation skipped pre-30

- `crates/transfer/src/setup/mod.rs:158-173` - pre-30 path forces
  `CompressionAlgorithm::Zlib` (or `None`) without vstring exchange.
- `crates/compress/src/strategy/profile.rs:17` (doc) - `ProtocolCompressionProfile`
  table.

**Upstream**: `compat.c:194-195 CPRES_ZLIB` (default for pre-30);
`compat.c:100-112 valid_compressions_items[]`.

**Behaviour difference**: pre-30 peer only ever speaks zlib; no zstd/lz4/zlibx
negotiation.

**Test coverage**: `crates/transfer/src/tests/negotiated_algorithms.rs`,
`crates/protocol/tests/zlib_golden_bytes.rs`.

### Z3. Batch `do_compression` bit (29+)

- `crates/batch/src/format/flags.rs:69`, `:112` - `do_compression` flag (bit 8)
  read/written only for protocol >= 29.

**Upstream**: `batch.c:68 &do_compression`.

**Behaviour difference**: pre-29 batch headers do not encode the
`do_compression` flag.

**Test coverage**: `crates/batch/src/format/flags.rs` round-trip tests.

## Checksum Gates

### S1. Strong checksum algorithm default (MD4 vs MD5)

- `crates/checksums/src/strong/strategy/selector.rs:42` -
  `ChecksumStrategySelector::for_protocol_version`: MD5 for `>= 30`, MD4 for
  `< 30`.
- `crates/checksums/src/strong/strategy/selector.rs:59` - same selector with
  seed-order parameter.
- `crates/checksums/src/strong/strategy/kind.rs:15` (doc) - notes MD4 is the
  legacy default for pre-30.
- `crates/transfer/src/shared/checksum.rs:74` and `:202` (docs) - default
  fallback when negotiation is absent.
- `crates/transfer/src/delta_config.rs:44` (doc).

**Upstream**: `checksum.c` - protocol version determines default; see also
`compat.c:858`.

**Behaviour difference**: hash family changes from MD4 (16 bytes) to MD5
(16 bytes). Same length, totally different bits - silent corruption if
mis-gated.

**Test coverage**: `crates/checksums/src/strong/strategy/tests.rs:271`, `:547`;
`crates/transfer/src/receiver/tests/delta_apply.rs:211`, `:306`;
`crates/transfer/src/tests/negotiated_algorithms.rs:137`.

### S2. MD4 SIMD batch path

- `crates/checksums/src/simd_batch/md4/mod.rs:8` (doc) - module exists
  specifically for upstream rsync protocol versions < 30.

**Test coverage**: `crates/checksums/src/simd_batch/md4` unit tests.

### S3. Strong-sum seed ordering for legacy MD4

- `crates/transfer/src/transfer_ops/streaming.rs:101` (doc) - the per-file
  verifier replacement re-seeds based on the algorithm; matters for pre-30 MD4
  which uses upstream's legacy seed-prepend convention.

### S4. Signature block-length max varies at protocol 30

- `crates/signature/src/block_size.rs:140` - `calculate_block_length` clamps to
  `MAX_BLOCK_SIZE_OLD` for `< 30`, `MAX_BLOCK_SIZE_V30` for `>= 30`.
- `crates/signature/src/block_size.rs:296` - `derive_block_length_sqrt` uses the
  same cap inside the heuristic.
- `crates/signature/src/layout.rs:185`, `:236` - duplicate cap inside the
  `SignatureLayout` builder.

**Upstream**: `generator.c:sum_sizes_sqroot()` lines 725-738.

**Behaviour difference**: pre-30 caps block size at a smaller value, producing
finer-grained signature blocks for large files.

**Test coverage**: `crates/signature/src/block_size.rs` doc-tests and
`crates/signature/src/layout.rs` unit tests.

## Filter / Rule-Encoding Gates

### R1. Filter rule `legal_len` and old-prefix mode (pre-29)

- `crates/protocol/src/codec/protocol/mod.rs:153-180` - `legal_len`,
  `supports_sender_receiver_modifiers`, `supports_perishable_modifier`,
  `uses_old_prefixes` all reference upstream `exclude.c:1530`,
  `exclude.c:1567-1571`, `exclude.c:1350`, `exclude.c:1675`.
- `crates/protocol/src/version/protocol_version/capabilities.rs:50`, `:61`,
  `:75` - same gates exposed on `ProtocolVersion`.

**Upstream**: `exclude.c:1530`, `:1567-1571`, `:1350`, `:1675`.

**Behaviour difference**: pre-29 filter parser caps modifier length at 1 and
accepts a different prefix grammar; perishable modifier `p` is unrecognised pre-30.

**Test coverage**: `crates/protocol/tests/filter_legal_len_parity.rs`.

### R2. Batch filter-rule injection switch (29+)

- `crates/batch/src/script.rs:81` - protocol >= 29 writes
  `--filter=._-`; pre-29 writes `--exclude-from=-`.

**Upstream**: `batch.c:262-267`.

**Behaviour difference**: batch script format changes the read-batch argument.

**Test coverage**: `crates/batch/src/script.rs` unit tests.

## Other Gates

### O1. INC_RECURSE-related capability-string emission

- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:166`
  - emits the `-e` capability string only when `protocol.as_u8() >= 30`.

See C4.

### O2. Signature special-rdev for FIFO/socket entries pre-31

- `crates/protocol/src/flist/write/xflags.rs:144` and
  `crates/protocol/src/flist/read/extras.rs:108` - the `needs_rdev` predicate
  for special files is gated on `protocol < 31`, not 30. Listed for orientation
  because it changes behaviour for the same set of peers under test (pre-30
  peers definitely take the `< 31` branch).

### O3. Receiver iconv reorder suppression (operates regardless of protocol)

- `crates/transfer/src/receiver/file_list/receive.rs:102-115` - hardlink
  `(dev, ino)` normalisation runs only when `self.protocol.as_u8() < 30 &&
  self.config.flags.hard_links`. Listed under W10 as well.

## Summary Table

Severity scheme:

- **HIGH** = silent data loss or transfer corruption if the gate is wrong.
- **MED** = compat/feature surfaces a user-visible error or skipped capability.
- **LOW** = cosmetic-only field (e.g. an unused statistic, a doc-only ref).

| Area | Items | HIGH | MED | LOW |
|------|-------|------|-----|-----|
| Wire-format | W1, W2, W3, W4, W5, W6, W7, W8, W9, W10, W11, W12, W13, W14, W15 | 12 | 1 | 2 |
| Capability negotiation | C1, C2, C3, C4, C5, C6, C7 | 2 | 5 | 0 |
| Flist framing | F1, F2, F3, F4, F5 | 3 | 1 | 1 |
| Compression | Z1, Z2, Z3 | 0 | 2 | 1 |
| Checksum | S1, S2, S3, S4 | 2 | 1 | 1 |
| Filter / rule encoding | R1, R2 | 0 | 2 | 0 |
| Other | O1, O2, O3 | 0 | 1 | 2 |
| **Totals** | **39** | **19** | **13** | **7** |

### Severity rationale per item

- HIGH: W1, W2, W3, W4, W5, W6, W7, W8, W9, W10, W11, W12, F1, F2, F3, S1, S4,
  C1, C6 - each silently corrupts the wire stream or produces a different
  destination file if mis-gated.
- MED: C2, C3, C4, C5, C7, F4, Z2, Z3, S2, R1, R2, F5, W13, O2 - missing or
  incorrect gates produce visible errors, dropped features, or sub-optimal but
  still-correct behaviour.
- LOW: W14, W15, Z1, S3, O1, O3, plus the central constants. Mostly doc-only
  references or already-uniformly-handled.

### Raw grep totals (informational)

| Pattern | Hits |
|---------|------|
| `protocol_version < 30` (src + tests) | 16 |
| `protocol.as_u8() < 30` (src + tests) | 11 |
| `protocol.as_u8() >= 30` (src + tests) | 23 |
| `protocol_version >= 30` (src + tests) | 37 |
| `protocol_version >= 29` (src) | 9 |
| `protocol_version < 29` / `as_u8() < 29` | 4 |
| `(28..30)` / `(28..=29)` | 1 (`flags.rs:112`) |
| `is_protocol_28` | 0 (helper does not exist) |
| `PROTOCOL_VERSION_28` / `_29` | 0 (helpers do not exist) |

The 39-item inventory above is the de-duplicated list of behavioural branches.
Raw grep counts include doc references, test fixtures, and parallel
encode/decode pairs that share the same behaviour.

## Test-Coverage Gap Analysis

Branches with explicit pre-30 / pre-29 unit-test or golden-byte coverage in the
workspace:

- W1, W2, W3, W4, W5, W6, W7, W8, W9, W10, W11, W12, W13, F1, F2, F3, F4, F5,
  Z2, Z3, S1, S4, C1, C2, C3, C6, R1, R2.

Branches whose only coverage is upstream-fidelity comments or doc-tests (no
black-box wire assertion against a real pre-30 peer):

- W14, W15, C4, C5, C7, S2, S3, Z1, O1, O2, O3.

The RP28 follow-up tasks should add an automated rsync 2.6.9 / older interop
fixture so each HIGH-severity branch is exercised against a live peer that
naturally takes the pre-30 path, not just synthetic byte-level golden tests.

## Upstream References (Local Paths)

- `target/interop/upstream-src/rsync-3.4.1/rsync.h:147-149` - `MIN_PROTOCOL_VERSION`,
  `OLD_PROTOCOL_VERSION`, `MAX_PROTOCOL_VERSION`.
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:114` - `PROTOCOL_VERSION 32`.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:652-668` - protocol < 30 restrictions.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:678-709` - protocol < 29 restrictions.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:720` - `set_allow_inc_recurse`.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:740` - `do_negotiated_strings`.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:858` - MD5 vs MD4 default.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:580-610`, `:670-690`, `:800-902`,
  `:910-945`, `:2517-2518`, `:2736-2742`, `:3223` - file-list framing branches.
- `target/interop/upstream-src/rsync-3.4.1/io.c:1292`, `:1618-1627`, `:2243-2287`,
  `:2446-2449` - argument terminator, MSG_NO_SEND, NDX codec, batch header.
- `target/interop/upstream-src/rsync-3.4.1/exclude.c:1350`, `:1530`, `:1567-1571`,
  `:1675` - filter rule gating.
- `target/interop/upstream-src/rsync-3.4.1/sender.c:180-187`, `:210`, `:367-368`
  - iflags, max_phase, MSG_NO_SEND.
- `target/interop/upstream-src/rsync-3.4.1/main.c:347-357`, `:875-906`,
  `:1167-1168`, `:1342-1343` - stats, goodbye, multiplex activation.
- `target/interop/upstream-src/rsync-3.4.1/batch.c:59-76`, `:113`, `:118`,
  `:258-298` - batch header, flag table, script generation.
- `target/interop/upstream-src/rsync-3.4.1/generator.c:725-738` -
  `sum_sizes_sqroot`.
- `target/interop/upstream-src/rsync-3.4.1/csprotocol.txt` - daemon greeting
  digest list convention.

Upstream `MIN_PROTOCOL_VERSION 20` documents the floor below which upstream
itself refuses to interoperate; this implementation's tighter `28` floor is
encoded in `crates/protocol/src/version/constants.rs:7` with the matching
upstream-fidelity guard tests in the same file.
