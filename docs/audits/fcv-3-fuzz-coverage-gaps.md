# FCV-3 - Protocol-parsing fuzz coverage gaps

Tracking issue: #2316. Companion to `docs/audits/fuzz-coverage-matrix.md`
(FCV-2 baseline matrix) and `docs/audits/fuzz-coverage-gap-followups.md`
(per-gap decomposition for the highest-priority gaps).

This audit re-walks every byte-parsing entry point that consumes untrusted
input from a network peer, a daemon config file, or a CLI string, and
classifies each against the currently committed fuzz target inventory.
The intent is to drive the FCV-4+ follow-up queue with a clean inventory
rather than re-derive it from prose.

## 1. Baseline - existing fuzz targets

Three cargo-fuzz workspaces ship targets today. They are excluded from the
root workspace so `libfuzzer-sys`'s nightly-only linker flags do not break
ordinary builds.

| Workspace | Targets | Counts |
|-----------|---------|--------|
| `fuzz/` (top-level) | `protocol_wire`, `multiplex_frame_parse`, `flist_entry_decode`, `simd_checksum_parity`, `decompressor_zlib`, `decompressor_zstd`, `filter_differential`, `filter_rules_vs_upstream` | 8 |
| `crates/protocol/fuzz/` | `fuzz_varint`, `varint_roundtrip`, `fuzz_delta`, `fuzz_multiplex_frame`, `multiplex_frame`, `fuzz_legacy_greeting`, `file_entry_roundtrip` | 7 |
| `crates/filters/fuzz/` | `fuzz_filter_parse`, `fuzz_filter_chain` | 2 |

Total: **17** targets. **2** run on the nightly schedule
(`filter-fuzzer-overnight.yml`); the other **15** run only on demand via
`tools/ci/run_filter_fuzz.sh`, `tools/ci/run_filter_differential_fuzz.sh`,
or direct `cargo +nightly fuzz run` invocation.

Per-target input shapes are catalogued in
`docs/audits/fuzz-coverage-matrix.md` Table 1; this audit treats that
inventory as ground truth and focuses on the gap classification.

## 2. Coverage classification table

Coverage labels:

- **COVERED** - the cited fuzz target drives this entry point with raw or
  structured input and asserts at minimum panic-freedom.
- **PARTIAL** - the entry point is reached, but only through a wrapper or
  with a restricted input shape; sibling decoders / driver helpers / state
  machines on the same surface are not exercised.
- **MISSING** - no fuzz target reaches this entry point at all.

### crates/protocol/src

| Entry point | File:fn | Coverage | Recommended target |
|---|---|---|---|
| Multiplex frame header decoder | `multiplex/frame.rs:191` (`MessageFrame::decode_from_slice`) | COVERED | `fuzz_multiplex_frame`, `multiplex_frame`, `multiplex_frame_parse` |
| Multiplex borrowed-frame decoder | `multiplex/borrowed.rs:72` (`BorrowedMessageFrame::decode_from_slice`) | COVERED | `protocol_wire`, `fuzz_multiplex_frame`, `multiplex_frame_parse` |
| Multiplex blocking reader | `multiplex/io/recv.rs:14,31` (`recv_msg`, `recv_msg_into`) | COVERED | `fuzz_multiplex_frame`, `multiplex_frame` |
| Envelope header parse | `envelope/header.rs:30` (`EnvelopeHeader::decode`) | COVERED | `multiplex_frame_parse` (re-decodes the same 4-byte LE envelope) |
| Varint decoders | `varint/decode.rs` (`read_varint`, `read_varlong`, `read_varlong30`, `read_longint`, `decode_varint`, `read_varint30_int`) | COVERED | `fuzz_varint`, `varint_roundtrip` |
| Delta token reader | `wire/delta/token.rs:208` (`read_token`) | COVERED | `fuzz_delta` |
| Delta op stream reader | `wire/delta/internal.rs:64,127` (`read_delta_op`, `read_delta`) | COVERED | `fuzz_delta` |
| Delta int (4-byte LE) | `wire/delta/int_encoding.rs:62` (`read_int`) | COVERED | `fuzz_delta` |
| Delta signature header | `wire/signature.rs:166` (`read_signature`) | COVERED | `fuzz_delta` |
| Compressed token receiver | `wire/compressed_token/decoder.rs:114` (`CompressedTokenDecoder::recv_token`) | MISSING | new in-process target seeded with valid token framing under each codec selector |
| File entry flags decode | `wire/file_entry_decode/flags.rs:47,135` (`decode_flags`, `decode_end_marker`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry size decode | `wire/file_entry_decode/size.rs:35` (`decode_size`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry mtime decode | `wire/file_entry_decode/timestamps.rs:41` (`decode_mtime`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry mtime_nsec decode | `wire/file_entry_decode/timestamps.rs:65` (`decode_mtime_nsec`) | PARTIAL | extend `file_entry_roundtrip` to roundtrip `mtime_nsec`; reached transitively by `flist_entry_decode` |
| File entry atime decode | `wire/file_entry_decode/timestamps.rs:88` (`decode_atime`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry crtime decode | `wire/file_entry_decode/timestamps.rs:107` (`decode_crtime`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry mode decode | `wire/file_entry_decode/mode.rs:28` (`decode_mode`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry uid / gid decode | `wire/file_entry_decode/ownership.rs:24,50` (`decode_uid`, `decode_gid`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry rdev decode | `wire/file_entry_decode/device.rs:28` (`decode_rdev`) | PARTIAL | extend `file_entry_roundtrip` to roundtrip `rdev`; reached transitively by `flist_entry_decode` |
| File entry checksum decode | `wire/file_entry_decode/checksum.rs:19` (`decode_checksum`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry symlink decode | `wire/file_entry_decode/symlink.rs:28` (`decode_symlink_target`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry name decode | `wire/file_entry_decode/name.rs:44` (`decode_name`) | COVERED | `file_entry_roundtrip`, `flist_entry_decode` |
| File entry hardlink idx decode | `wire/file_entry_decode/hardlink.rs:22` (`decode_hardlink_idx`) | PARTIAL | extend `file_entry_roundtrip` to roundtrip `hardlink_idx` |
| File entry hardlink dev_ino decode | `wire/file_entry_decode/hardlink.rs:49` (`decode_hardlink_dev_ino`) | PARTIAL | extend `file_entry_roundtrip` to roundtrip `hardlink_dev_ino` |
| File entry stream | `flist/read/mod.rs:469,491,756` (`FileListReader::read_entry{_with_flist}`, `read_file_entry`) | COVERED | `flist_entry_decode` |
| Incremental file list stream | `flist/incremental/streaming.rs:57,86` (`StreamingFileList::next_ready`, `read_all`) | MISSING | new target wrapping a `StreamingFileList` around `flist_entry_decode`'s structured input; assert `ready_count() + pending_count()` invariant |
| Negotiation prologue sniffer (per-call) | `negotiation/sniffer/observe.rs:12,65,78` (`observe`, `observe_byte`, `read_from`) | MISSING | FCV-7 - in-process raw-byte target across all three drivers with `reset()` idempotence assertion |
| Negotiation prologue detector | `negotiation/detector/observe.rs:24,99` (`observe`, `observe_byte`) | MISSING | fold into the FCV-7 target; same threat model |
| Legacy daemon line reader | `negotiation/sniffer/legacy.rs:16,81,91` (`read_legacy_daemon_line`, `read_and_parse_legacy_daemon_greeting{,_details}`) | PARTIAL | FCV-9 - lift `fuzz_legacy_greeting` to drive the reader-side helpers through an in-memory `Cursor` |
| Legacy greeting / message parsers (byte) | `legacy/bytes.rs:37-90` (`parse_legacy_daemon_message_bytes`, `parse_legacy_error_message_bytes`, `parse_legacy_warning_message_bytes`, `parse_legacy_daemon_greeting_bytes{,_details,_owned}`) | COVERED | `fuzz_legacy_greeting` |
| Legacy greeting / message parsers (str) | `legacy/greeting/parse.rs:14,39,47` and `legacy/lines.rs:71,173,183` | COVERED | `fuzz_legacy_greeting` (UTF-8 branch) |
| Capability vstring read | `negotiation/capabilities/negotiate.rs:493` (`read_vstring`) | MISSING | FCV-8 - structured `Arbitrary` driving `negotiate_capabilities{,_with_override}` for protocols 28-32 in client and server roles |
| Capability negotiation driver | `negotiation/capabilities/negotiate.rs:121,163` (`negotiate_capabilities`, `negotiate_capabilities_with_override`) | MISSING | covered by the FCV-8 target above |
| Capability algorithm name parse | `negotiation/capabilities/algorithms.rs:87,139` (`*::parse(&str)`) | MISSING | small in-process target on `&str`; reachable via the vstring stream the FCV-8 target already drives, so fold in |
| Protocol version from peer | `version/protocol_version/mod.rs:375` (`ProtocolVersion::from_peer_advertisement`) | COVERED | reached transitively by `fuzz_legacy_greeting`, `multiplex_frame_parse` |
| Compatibility flags decode | `compatibility/flags.rs:175,184,212` (`read_from`, `decode_from_slice{,_mut}`) | MISSING | FCV-10 - raw-byte target driving both reader and slice paths |
| Filter list wire | `filters/wire.rs:181` (`read_filter_list`) | MISSING | FCV-11 - structured `{protocol, payload}` target with a 16 MiB per-rule length cap |
| Filter rule wire prefix builder | `filters/prefix.rs:24` (`build_rule_prefix`) | COVERED | exercised whenever `read_filter_list` accepts a rule; FCV-11 covers it on the read side |
| Index codec (ndx) | `codec/ndx/state.rs:97` (`NdxState::read_ndx`) | MISSING | FCV-12 - structured `{protocol, payload}` target reusing a single `NdxState` across calls |
| Index codec (goodbye) | `codec/ndx/goodbye.rs:48` (`read_goodbye`) | MISSING | fold into FCV-12 |
| Idlist (uid/gid name table) | `idlist/mod.rs:232,256` (`IdList::read`, `IdList::read_with_kind`) | MISSING | FCV-14 - structured `{protocol_version, id0_names, kind, payload}` target |
| Transfer stats read | `stats/transfer.rs:219` (`TransferStats::read_from`) | COVERED | `varint_roundtrip` |
| Delete stats read | `stats/delete.rs:91` (`DeleteStats::read_from`) | COVERED | `varint_roundtrip` |
| ACL definition wire | `acl/definition/wire.rs:48` (`read_acl_definition`) | MISSING | new target; P2 follow-up |
| ACL recv path | `acl/wire/recv.rs:27,67,118,210` (`recv_ida_entries`, `recv_rsync_acl`, `recv_acl`, `receive_acl_cached`) | MISSING | fold into the ACL target above; covers ida and cache-driven paths |
| Xattr definitions / send / request / values | `xattr/wire/decode.rs:52,118,171,208` (`read_xattr_definitions`, `recv_xattr`, `recv_xattr_request`, `recv_xattr_values`) | MISSING | new target; P2 follow-up |
| Xattr cache receive | `xattr/cache.rs:114` (`XattrCache::receive_xattr`) | MISSING | fold into xattr target above |
| `--files-from` forward path | `files_from.rs:59` (`forward_files_from`) | MISSING | new target; P2 follow-up |
| `--files-from` stream parser | `files_from.rs:162` (`read_files_from_stream`) | MISSING | fold into the files-from target above |
| Secluded args receiver | `secluded_args.rs:114` (`recv_secluded_args`) | MISSING | new target; reached pre-arg-parse on every protect-args connection (P1) |

### crates/filters/src

| Entry point | File:fn | Coverage | Recommended target |
|---|---|---|---|
| Filter rule text grammar | `merge/parse.rs:38` (`parse_rules`) | COVERED | `fuzz_filter_parse`, `fuzz_filter_chain`, `filter_differential`, `filter_rules_vs_upstream` |
| Filter chain evaluator | `set.rs` via `FilterSet::{allows,allows_deletion,allows_deletion_when_excluded_removed,excluded_dir_by_non_dir_rule,is_empty}` | COVERED | `fuzz_filter_chain`, `filter_differential`, `filter_rules_vs_upstream` |

### crates/checksums/src

| Entry point | File:fn | Coverage | Recommended target |
|---|---|---|---|
| Rolling checksum SIMD vs scalar | `rolling/mod.rs` (`RollingChecksum::update`) | COVERED | `simd_checksum_parity` |
| Rolling digest one-shot | `rolling/digest.rs:57` (`RollingDigest::from_bytes`) | COVERED | reached transitively by `simd_checksum_parity` |
| MD4 / MD5 batch dispatch | `strong/{md4,md5}_batch.rs` via `md4_digest_batch`, `md5_digest_batch` | COVERED | `simd_checksum_parity` |
| Strong checksum algorithm name parse | `strong/strategy/kind.rs:83` (`Kind::from_name`) | MISSING | small in-process target on `&str`; reachable via daemon and protocol negotiation strings |
| CPU feature CLI parse | `cpu_features.rs:66` (`*::parse_cli`) | MISSING | CLI-string-only, P3 (low priority - not on the network path) |

### crates/compress/src

| Entry point | File:fn | Coverage | Recommended target |
|---|---|---|---|
| Zlib (raw deflate) one-shot | `zlib/helpers.rs:17` (`decompress_to_vec`) | COVERED | `decompressor_zlib` |
| Zlib (raw deflate) streaming | `zlib/decoder.rs` (`CountingZlibDecoder`) | COVERED | `decompressor_zlib` |
| Zstd one-shot | `zstd.rs:237` (`decompress_to_vec`) | COVERED | `decompressor_zstd` |
| Zstd streaming | `zstd.rs` (`CountingZstdDecoder`) | COVERED | `decompressor_zstd` |
| Zstd decompress-into | `zstd.rs:245` (`decompress_into`) | PARTIAL | `decompressor_zstd` exercises the streaming and one-shot helpers but not the pre-allocated `decompress_into` sink - bounds-check arithmetic diverges from `decompress_to_vec` |
| LZ4 raw block | `lz4/raw.rs:217,253` (`decompress_block`, `decompress_block_to_vec`) | MISSING | new target; reachable whenever the peer selects LZ4 (matches FCV-15 in the gap-followups doc) |
| LZ4 frame | `lz4/frame.rs:182,190` (`decompress_to_vec`, `decompress_into`) | MISSING | fold into the LZ4 target above |
| Skip-compress suffix list | `skip_compress/decider.rs:95` (`parse_skip_compress_list`) | MISSING | CLI string parser; P3 |

### crates/batch/src

| Entry point | File:fn | Coverage | Recommended target |
|---|---|---|---|
| Batch header read | `format/header.rs:84` (`BatchHeader::read_from`) | MISSING | new target with structured `{flags, payload}`; reachable from `--read-batch` against an attacker-supplied file |
| Batch flags decode | `format/flags.rs:59,163` (`from_bitmap`, `read_raw`) | MISSING | fold into the batch-header target above |
| Batch file entry read | `format/file_entry.rs:74` (`read_from`) | MISSING | fold into the batch-header target above |
| Batch stats read | `format/stats.rs:48` (`read_from`) | MISSING | fold into the batch-header target above |
| Batch reader (header, data, exact) | `reader/mod.rs:92,128,148` (`BatchReader::{read_header, read_data, read_exact}`) | MISSING | fold into the batch-header target above |
| Batch flist read | `reader/flist.rs:24,76,218` (`read_file_entry`, `read_protocol_flist`, `read_incremental_flist_segment`) | MISSING | reached transitively by `flist_entry_decode` once `BatchReader` is wired; treat as the FCV-13 incremental-flist target's batch sibling |
| Batch delta read | `reader/delta.rs:30,91,136` (`read_file_delta_tokens`, `read_compressed_delta_tokens`, `read_all_delta_ops`) | MISSING | reached transitively by `fuzz_delta` once `BatchReader` is wired |

### crates/daemon/src

| Entry point | File:fn | Coverage | Recommended target |
|---|---|---|---|
| `@RSYNCD:` greeting / capabilities reader | `daemon/sections/greeting.rs:49` (`read_trimmed_line`) and downstream `parse_legacy_daemon_message` | PARTIAL | `fuzz_legacy_greeting` covers the message-string parser but not the reader; covered for free once FCV-9 lands |
| Secrets file parse | `auth.rs:269,324` (`SecretsFile::parse`, `from_file`) | MISSING | CLI / filesystem string parser; reached on every authenticated module; P2 |
| Daemon auth challenge generator | `auth.rs:175` (`ChallengeGenerator::generate`) | COVERED | not a parser - emits hashed output only |
| Daemon auth response verifier | `core/src/auth/mod.rs:230` (`verify_daemon_auth_response`) | MISSING | small in-process target on `(secret, challenge, response, version)`; constant-time-compare path must never panic on adversarial response lengths |
| `rsyncd.conf` parser | `rsyncd_config/mod.rs:82` (`RsyncdConfig::parse`) and `rsyncd_config/parser.rs:28` (`Parser::parse`) | MISSING | new target on UTF-8 strings; reachable through admin-edited config files (P2) |
| Daemon include / merge parser | `daemon/sections/config_parsing/include_merge.rs` | MISSING | fold into the rsyncd_conf target above; the macro / module-section interactions are the interesting branches |

### Cross-cutting CLI string parsers (reachable via argv)

| Entry point | File:fn | Coverage | Recommended target |
|---|---|---|---|
| `--bwlimit` parser | `bandwidth/src/parse/components.rs:244` (`BandwidthLimitComponents::from_str`) | MISSING | small in-process string target; P3 |
| `--iconv` parser | `core/src/client/config/iconv.rs:25` (`IconvSpec::parse`) | MISSING | small in-process string target; P2 (reachable via daemon module-section `charset = ...`) |

## 3. Totals

| Coverage label | Count |
|---|---|
| COVERED | 36 |
| PARTIAL | 7 |
| MISSING | 37 |
| **Total entry points audited** | **80** |

`PARTIAL` rows fall into two groups:

1. Field decoders that are exercised end-to-end by `flist_entry_decode`
   but not yet present in the per-field `file_entry_roundtrip` matrix
   (`mtime_nsec`, `rdev`, `hardlink_idx`, `hardlink_dev_ino`). Folding
   them into the existing roundtrip target is a small in-place extension
   and is the only remaining work to flip Table 2 row 7 of
   `fuzz-coverage-matrix.md` from "partial" to "covered".
2. Reader-driven wrappers around pure parsers that the pure-parser
   targets already cover (`read_legacy_daemon_line`,
   `BatchReader::read_*`, `decompress_into`). These flip to COVERED as
   their per-gap follow-ups land - FCV-9 for the legacy reader, the
   batch-header target for `BatchReader`, and an in-place extension to
   `decompressor_zstd` for `decompress_into`.

## 4. Priority recommendations

Ranked by attack-surface exposure. Pre-auth gaps come first because they
are reachable by any peer that opens a TCP socket, before any
authentication or framing context exists.

### Pre-authentication (most exposed)

These parsers run before the daemon has decided whether the peer is even
permitted to speak. A panic here is unauthenticated remote DoS.

1. **`negotiation/sniffer/observe.rs` + `negotiation/detector/observe.rs`** -
   classify the very first byte of every connection. MISSING. Tracked
   as FCV-7 in `fuzz-coverage-gap-followups.md`.
2. **`negotiation/sniffer/legacy.rs` reader helpers** - drive the
   `@RSYNCD:` line parse against a `Cursor`. PARTIAL. Tracked as FCV-9.
3. **`negotiation/capabilities/negotiate.rs::read_vstring`** - vstring
   exchange runs before any framing or encryption. MISSING. Tracked as
   FCV-8.
4. **`compatibility/flags.rs::{read_from, decode_from_slice}`** - one
   byte but gates every later codec switch (varint30, longints,
   INC_RECURSE). MISSING. Tracked as FCV-10.
5. **`negotiation/capabilities/algorithms.rs::*::parse`** - the digest /
   compression name strings carried inside the vstring stream. MISSING.
   Fold into the FCV-8 target.
6. **`checksums/src/strong/strategy/kind.rs::Kind::from_name`** - same
   threat model as the algorithm parsers above. MISSING.

### Post-authentication, pre-transfer (peer is now trusted-ish but every
field is still attacker-controlled)

7. **`filters/wire.rs::read_filter_list`** - wire-format filter chain
   shipped by the sender. MISSING. Tracked as FCV-11.
8. **`codec/ndx/state.rs::read_ndx` + `codec/ndx/goodbye.rs::read_goodbye`** -
   gate every post-flist message. MISSING. Tracked as FCV-12.
9. **`flist/incremental/streaming.rs::next_ready`/`read_all`** - state
   machine on top of `read_file_entry`. MISSING. Tracked as FCV-13.
10. **`idlist/mod.rs::read{,_with_kind}`** - uid/gid name table arrives
    unauthenticated from the daemon. MISSING. Tracked as FCV-14.
11. **`secluded_args.rs::recv_secluded_args`** - sits ahead of
    server-side arg parsing for any `--protect-args` / `--secluded-args`
    connection. MISSING. New gap (no FCV slot yet).
12. **`compress/src/{lz4/raw,lz4/frame}.rs`** + **`compress/src/zstd.rs::decompress_into`** -
    compressed input is fully untrusted. zlib and zstd one-shot/stream
    are COVERED; LZ4 paths and the `decompress_into` sink are MISSING /
    PARTIAL. Tracked as FCV-15 (LZ4) with `decompress_into` as a small
    in-place extension to `decompressor_zstd`.
13. **`wire/compressed_token/decoder.rs::recv_token`** - sits between
    the multiplex frame and the delta token decoder. MISSING.
14. **`core/src/auth/mod.rs::verify_daemon_auth_response`** - reachable
    pre-auth with any peer-supplied `(challenge, response)` pair; the
    constant-time compare path must survive adversarial length and
    digest-name combinations. MISSING.

### Authenticated (admin / local input)

These parsers run only after the peer has authenticated, or against
strings supplied by the operator on the CLI or in a config file. The
attack surface is real but narrower.

15. **`acl/definition/wire.rs::read_acl_definition`** + **`acl/wire/recv.rs::*`** -
    only reachable when `-A` is negotiated. MISSING.
16. **`xattr/wire/decode.rs::*`** + **`xattr/cache.rs::receive_xattr`** -
    only reachable when `-X` is negotiated. MISSING.
17. **`files_from.rs::{forward_files_from, read_files_from_stream}`** -
    only reachable when `--files-from` is supplied. MISSING.
18. **`batch/format/*` + `batch/reader/*`** - only reachable through
    `--read-batch <file>`. The file is operator-supplied but completely
    attacker-controlled in shared-tenancy setups. MISSING.
19. **`daemon/auth.rs::SecretsFile::parse`** + **`daemon/rsyncd_config/*::parse`** +
    **`daemon/sections/config_parsing/include_merge.rs`** -
    administrator-edited files on the daemon host. MISSING.
20. **`core/src/client/config/iconv.rs::IconvSpec::parse`** -
    `--iconv=<spec>` CLI string; reachable from daemon module sections
    too. MISSING.
21. **`bandwidth/src/parse/components.rs::BandwidthLimitComponents::from_str`** +
    **`compress/src/skip_compress/decider.rs::parse_skip_compress_list`** +
    **`checksums/src/cpu_features.rs::*::parse_cli`** - CLI-only string
    parsers. MISSING. Lowest priority (P3).

### Recommended initial filing batch (matches `fuzz-coverage-gap-followups.md`)

The five highest-leverage P0 / P1 gaps already have detailed task slots
in `docs/audits/fuzz-coverage-gap-followups.md`: FCV-7 through FCV-11.
This audit endorses that ordering and recommends two additions before
the batch ships:

- **In-place extension to `file_entry_roundtrip`** to fold in the four
  outstanding PARTIAL field decoders (`mtime_nsec`, `rdev`,
  `hardlink_idx`, `hardlink_dev_ino`). One PR, no new target.
- **In-place extension to `decompressor_zstd`** to drive
  `decompress_into` against the same coverage-guided inputs. One PR, no
  new target.

The remaining MISSING rows above are tracked here so the FCV queue does
not have to re-derive them.
