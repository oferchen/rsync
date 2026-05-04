# Protocol 28-32 wire/capability interop matrix

Tracking issue: oc-rsync task #1908. Companion documents:
[`docs/interop/protocol-matrix.md`](../interop/protocol-matrix.md)
(operator-facing harness inventory),
[`docs/protocol-compatibility.md`](../protocol-compatibility.md)
(release-by-release compatibility summary),
[`docs/audits/zstd-batch-compatibility.md`](zstd-batch-compatibility.md)
(task #1685, batch-format gating),
[`docs/audits/tcpdump-daemon-filter-pull.md`](tcpdump-daemon-filter-pull.md)
(task #1697, filter wire on-the-wire).

Last verified: 2026-05-01 against master @ `83c8aa41`. Files spot-checked:
`crates/protocol/src/version/protocol_version/capabilities.rs`,
`crates/protocol/src/version/constants.rs`,
`crates/protocol/src/compatibility/flags.rs`,
`crates/protocol/src/compatibility/known.rs`,
`crates/protocol/src/codec/ndx/constants.rs`,
`crates/protocol/src/filters/wire.rs`,
`crates/protocol/src/wire/file_entry/constants.rs`,
`crates/protocol/src/flist/flags.rs`,
`crates/transfer/src/setup/capability.rs`,
`crates/batch/src/format/flags.rs`,
`tools/ci/known_failures.conf`,
`tools/ci/run_interop.sh`.

## Scope

This audit answers a single question for each negotiable feature in
oc-rsync: at which negotiated protocol version (28, 29, 30, 31, 32) is
the feature on the wire, and which Rust symbol gates it? It is the
flat lookup table for "what does protocol N support". It cross-cites
upstream `compat.c`, `io.c`, `flist.c`, and `exclude.c` so a reviewer
can confirm any cell in one click without re-reading the C source.

The matrix at [`docs/interop/protocol-matrix.md`](../interop/protocol-matrix.md)
covers the same territory but is organised around the CI harness
(scenarios x upstream binaries) and is the right reference for "what
does CI exercise". This file is organised around **the wire** -
encoding boundaries, capability characters, compat-flag bits - and is
the right reference for "where does the code branch on protocol".

This is a documentation-only audit. No Rust code changes are proposed
here. All proposals route through follow-up issues.

## TL;DR

- Protocol 30 is the single largest break in the rsync wire protocol.
  It introduces the binary handshake, varint encoding for indices and
  flist flags, the `CompatibilityFlags` exchange, codec/checksum
  vstring negotiation, inline hardlink dev/ino in the file list, and
  the ACL/xattr wire fields. Protocols 28 and 29 are legacy ASCII
  with fixed-width 4-byte LE integers; protocols 30, 31, 32 differ
  from 30 only by additive feature bits (`CF_VARINT_FLIST_FLAGS` was
  retro-introduced at 30 itself).
- Protocol 31 layers two changes on top of 30: `safe_file_list_always_enabled`
  (`compat.c:775`), the 3-way extended goodbye / `MSG_IO_TIMEOUT` path
  (`io.c:1684`), and `NDX_DEL_STATS` per-type deletion counts during
  the goodbye phase.
- Protocol 32 is **wire-equivalent** to 31 in every cell of this
  matrix except `--crtimes` semantics (`compat.c:750-753`); oc-rsync
  treats 32 as the primary advertised version and 31 as a documented
  downgrade target.
- The `i` capability character (`-e.LsfxCIvu`) only flips the
  `CF_INC_RECURSE` bit when the caller is **acting as receiver**:
  see `transfer/setup.rs` and #2569's note via
  `build_capability_string(!is_sender)`. INC_RECURSE for push
  transfers is gated off and not advertised. This is the most
  surprising version-gating finding documented in the project.
- rsync 3.0.9 advertises `PROTOCOL_VERSION = 30`, not 28, despite
  CI commentary in `tools/ci/known_failures.conf:81` calling it a
  "protocol 28" peer. The number 28 is the **minimum** it accepts.
  This was first flagged in PR #3503's audit at line 81-100 of
  [`docs/audits/tcpdump-daemon-filter-pull.md`](tcpdump-daemon-filter-pull.md)
  and is still open as of master `83c8aa41`. The CI logic itself is
  correct because it forces the protocol via `--protocol=N`, not via
  the advertised version.

## 1. Supported version range and constants

| Constant                              | Value | Source |
|---------------------------------------|-------|--------|
| `OLDEST_SUPPORTED_PROTOCOL`           | 28    | `crates/protocol/src/version/constants.rs:7` |
| `NEWEST_SUPPORTED_PROTOCOL`           | 32    | `crates/protocol/src/version/constants.rs:9` |
| `FIRST_BINARY_NEGOTIATION_PROTOCOL`   | 30    | `crates/protocol/src/version/constants.rs:11` |
| `MAXIMUM_PROTOCOL_ADVERTISEMENT`      | 40    | `crates/protocol/src/version/constants.rs:16` |
| `SUPPORTED_PROTOCOL_RANGE`            | 28..=32 | `crates/protocol/src/version/constants.rs:27` |

Mirrors upstream `target/interop/upstream-src/rsync-3.4.1/rsync.h:114`
(`PROTOCOL_VERSION 32`) and `rsync.h:147` (`MIN_PROTOCOL_VERSION 20`,
`MAX_PROTOCOL_VERSION 40`). oc-rsync clamps the lower bound to 28
because protocols 20-27 share the goodbye / multiplex semantics of 28
but differ in flist encoding details that have no observed in-the-wild
peer; the `MAX_PROTOCOL_VERSION` of 40 mirrors upstream's tolerance for
future negotiation.

## 2. Wire encoding format matrix

| Boundary                                            | 28 | 29 | 30 | 31 | 32 | Rust symbol (`crates/protocol/...`) | Upstream |
|-----------------------------------------------------|----|----|----|----|----|-------------------------------------|----------|
| Legacy ASCII `@RSYNCD:` daemon greeting             | Y  | Y  |    |    |    | `version/protocol_version/capabilities.rs:19` `uses_legacy_ascii_negotiation` | `compat.c:710` |
| Binary negotiation handshake                        |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:12` `uses_binary_negotiation` | `compat.c:710` |
| 4-byte LE `write_int` for NDX                       | Y  | Y  |    |    |    | `codec/ndx/constants.rs:38` `NDX_DONE_LEGACY_BYTES` | `io.c:2249-2251` |
| Varint NDX (`write_ndx`)                            |    |    | Y  | Y  | Y  | `codec/ndx/constants.rs:48` `NDX_DONE_MODERN_BYTE` | `io.c:2243-2287` |
| Fixed 1-2 byte flist flags                          | Y  | Y  |    |    |    | `version/protocol_version/capabilities.rs:39` `uses_fixed_encoding` | `flist.c:411` |
| Varint flist flags (`CF_VARINT_FLIST_FLAGS`)        |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:130` `uses_varint_flist_flags`; `compatibility/flags.rs:46` `VARINT_FLIST_FLAGS` | `compat.c:124,729-732` |
| `CompatibilityFlags` varint exchange                |    |    | Y  | Y  | Y  | `compatibility/flags.rs:156` `write_to` / `:173` `read_from` | `compat.c:738,740` |
| Pre-release `'V'` byte-encoded compat flags         | -  | -  | Y  | Y  | Y  | `transfer/src/setup/capability.rs:232` `client_has_pre_release_v_flag` | `compat.c:733-737` |

`uses_varint_encoding` (`capabilities.rs:31`) is the canonical predicate
for "does this version use varint over fixed integers"; it is the bit
that callers in flist, idlist, and codec layers branch on.

## 3. Capability character matrix (`-e.LsfxCIvu`)

The capability string is built by
`crates/transfer/src/setup/capability.rs:108` `build_capability_string()`.
The single source of truth is the `CAPABILITY_MAPPINGS` table at
`crates/transfer/src/setup/capability.rs:32-99`; both SSH invocation
and daemon-mode setup go through this builder.

| Char | Capability                       | Compat flag                  | Available    | Notes |
|------|----------------------------------|------------------------------|--------------|-------|
| `i`  | INC_RECURSE                      | `CF_INC_RECURSE`             | proto >= 30  | Only emitted by **receiver** direction; gated by `requires_inc_recurse=true` at `capability.rs:38`. Upstream `compat.c:712,720`. |
| `L`  | Set symlink times                | `CF_SYMLINK_TIMES`           | proto >= 30  | Unix only (`platform_ok=false` on Windows, `capability.rs:46`). Upstream `compat.c:714`. |
| `s`  | Symlink iconv translation        | `CF_SYMLINK_ICONV`           | proto >= 30  | Upstream `compat.c:717`. |
| `f`  | Safe file list                   | `CF_SAFE_FLIST`              | proto >= 30  | At proto >= 31 the bit is implicit (always-on). Upstream `compat.c:719,775`. |
| `x`  | Avoid xattr hardlink optimisation| `CF_AVOID_XATTR_OPTIM`       | proto >= 30  | Inverts the proto-31 default (`compat.c:746`). |
| `C`  | Checksum-seed-fix                | `CF_CHKSUM_SEED_FIX`         | proto >= 30  | Upstream `compat.c:723,747`. |
| `I`  | Inplace partial-dir              | `CF_INPLACE_PARTIAL_DIR`     | proto >= 30  | Upstream `compat.c:725-726,777`. |
| `v`  | Varint flist flags               | `CF_VARINT_FLIST_FLAGS`      | proto >= 30  | Drives `do_negotiated_strings = 1` (codec/checksum vstrings). Upstream `compat.c:729-732`. |
| `u`  | id0 names (uid/gid 0 names)      | `CF_ID0_NAMES`               | proto >= 30  | Upstream `compat.c:727,749`. |

The leading `.` in `-e.LsfxCIvu` is the version placeholder upstream uses
when `protocol_version != PROTOCOL_VERSION`; it is stripped in
`capability.rs:167-169` `parse_client_info`. The capability string is
inert at proto 28-29 (no compat-flag exchange occurs at all - upstream
`compat.c:710` gates the whole block on `protocol_version >= 30`).

## 4. Filter wire format matrix

| Filter capability                                                  | 28 | 29 | 30 | 31 | 32 | Rust symbol | Upstream |
|--------------------------------------------------------------------|----|----|----|----|----|-------------|----------|
| Old-style 1-char prefixes only (`+ `, `- `, `!`)                   | Y  |    |    |    |    | `version/protocol_version/capabilities.rs:75` `uses_old_prefixes`; `filters/wire.rs:254` `parse_wire_rule` | `exclude.c:1675`, `exclude.c:1119-1133` |
| Multi-char prefixes: `merge` (`.`), `dir-merge` (`:`), `protect`/`risk`, etc. |    | Y  | Y  | Y  | Y  | `filters/wire.rs:330` `parse_wire_rule_modern` | `exclude.c:1530` |
| Sender-side / receiver-side modifiers (`s`, `r`)                   |    | Y  | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:50` `supports_sender_receiver_modifiers`; `filters/wire.rs:84-86` | `exclude.c:1567-1571` |
| Perishable modifier (`p`)                                          |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:61` `supports_perishable_modifier`; `filters/wire.rs:88` | `exclude.c:1350` |

`RuleType` enum in `filters/wire.rs:8-23` enumerates every prefix
character. The 28-only branch (old prefixes) does not parse modifier
flags at all; modifier bits in `FilterRuleWireFormat` are silently
ignored when `protocol.uses_old_prefixes()` is true. The CI test for
`merge-filter` at `forced_proto <= 28` is a documented known failure
(`tools/ci/known_failures.conf:116-126`).

## 5. File-list encoding matrix

| Field / behaviour                                              | 28 | 29 | 30 | 31 | 32 | Rust symbol | Upstream |
|----------------------------------------------------------------|----|----|----|----|----|-------------|----------|
| `XMIT_HLINKED` flag and inline dev/ino in flist entry          |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:217` `supports_inline_hardlinks`; `flist/read/extras.rs:145` | `flist.c:411,505,829` |
| `XMIT_SAME_DEV_PRE30` (legacy hardlink dev compression)        | Y  | Y  |    |    |    | `flist/flags.rs:97`; `wire/file_entry/constants.rs:48` | `flist.c:436,461,623` |
| `XMIT_RDEV_MINOR_8_PRE30` (8-bit minor only)                   | Y  | Y  |    |    |    | `wire/file_entry/mod.rs:43` | `flist.c:447` |
| `XMIT_TOP_DIR` and extended xflags                             | Y  | Y  | Y  | Y  | Y  | `flist/mod.rs:52` `XMIT_TOP_DIR` | `flist.c:525,551` |
| Multi-phase transfer (`max_phase = 2`)                         |    | Y  | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:112` `supports_multi_phase` | `generator.c`, `receiver.c` |
| File list timing stats                                         |    | Y  | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:86` `supports_flist_times` | `main.c handle_stats()` |
| Iflags (2-byte `iflag_extra`) follow each NDX                  |    | Y  | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:99` `supports_iflags` | `sender.c:180-187` |
| Incremental recursion (`CF_INC_RECURSE`)                       |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:286` `supports_inc_recurse` | `compat.c:712,720` |
| Safe file list (`CF_SAFE_FLIST`) negotiable                    |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:142` `uses_safe_file_list` | `compat.c:719,775` |
| Safe file list always on (no negotiation needed)               |    |    |    | Y  | Y  | `version/protocol_version/capabilities.rs:151` `safe_file_list_always_enabled` | `compat.c:775` |
| `--crtimes` (creation time field on flist entry)               |    |    |    |    | Y  | (gated via varint flist flags) | `compat.c:750-753`, `flist.c:487` |

The 28-29 hardlink encoding uses two pre-30 xflags (`XMIT_SAME_DEV_PRE30`,
`XMIT_RDEV_MINOR_8_PRE30`) and emits dev/ino as a separate run-length
payload; upstream `flist.c:436-461` documents the branch. Protocol 30+
embeds the dev/ino pair inline as varints inside the flist entry, which
is what enables `CF_INC_RECURSE` (the receiver can deduplicate hardlinks
without buffering the full list).

## 6. Multiplex, goodbye, and stats matrix

| Behaviour                                                  | 28 | 29 | 30 | 31 | 32 | Rust symbol | Upstream |
|------------------------------------------------------------|----|----|----|----|----|-------------|----------|
| Multiplexed I/O (`MSG_*` envelope frames)                  | Y  | Y  | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:163` `supports_multiplex_io` (>=23) | `main.c:1304-1305` |
| `NDX_DONE` goodbye exchange                                | Y  | Y  | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:174` `supports_goodbye_exchange` (>=24) | `main.c:880-905` |
| Generator-to-sender messages over multiplex                |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:189` `supports_generator_messages` | `io.c need_messages_from_generator` |
| 3-way extended goodbye / `MSG_IO_TIMEOUT`                  |    |    |    | Y  | Y  | `version/protocol_version/capabilities.rs:202` `supports_extended_goodbye` | `main.c:880-905`, `io.c:1684` |
| `NDX_DEL_STATS` per-type deletion counts in goodbye phase  |    |    |    | Y  | Y  | `version/protocol_version/capabilities.rs:270` `supports_delete_stats`; `codec/ndx/constants.rs:21` `NDX_DEL_STATS = -3` | `main.c read_del_stats()` |
| Multiplex `OUT_MULTIPLEXED` for protocol < 31              | Y  | Y  | Y  |    |    | (envelope code) | `io.c:1227` |

`NDX_DEL_STATS` is encoded as five varints after the index sentinel
(files, dirs, symlinks, devices, specials). The receiver-side parser
lives in `crates/transfer/src/receiver/transfer/phases.rs` (per
`MEMORY.md` "Recent Completed Work" entry for v0.5.8 / PR #2570).

## 7. Compression and checksum negotiation matrix

| Negotiation                                              | 28 | 29 | 30 | 31 | 32 | Rust symbol | Upstream |
|----------------------------------------------------------|----|----|----|----|----|-------------|----------|
| Hardcoded zlib compression                               | Y  | Y  |    |    |    | (no symbol; pre-30 path) | `compat.c:383,556-564` |
| Vstring negotiation for codecs (zlibx, zstd, lz4)        |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:241` `preferred_compression` | `compat.c:100-112,556-564,729-732` |
| Hardcoded MD4 strong checksum                            | Y  | Y  |    |    |    | (pre-30 path; checksums crate fallback) | `compat.c:414,552,859` |
| MD5 default, negotiated MD4/XXH3/XXH128                  |    |    | Y  | Y  | Y  | `version/protocol_version/capabilities.rs:257` `supports_checksum_negotiation` | `compat.c:414,552`, `do_negotiated_strings` |
| `-e.LsfxCIvu` capability advertises checksum negotiation | -  | -  | Y  | Y  | Y  | `transfer/setup/capability.rs:108` (the `v` mapping enables vstrings) | `compat.c:729-732` |

Without the `v` capability character at proto >= 30, codec selection
silently falls back to zlib (`compat.c:383` `strlcpy(tmpbuf, "zlib", ...)`)
and the `compress-zstd` / `compress-lz4` interop scenarios are documented
known failures in `tools/ci/known_failures.conf:97-110`.

## 8. ACL/xattr wire format matrix

| Field                                                       | 28 | 29 | 30 | 31 | 32 | Rust symbol | Upstream |
|-------------------------------------------------------------|----|----|----|----|----|-------------|----------|
| ACL wire format (flist ACL fields, `--acls` / `-A`)         |    |    | Y  | Y  | Y  | `crates/protocol/src/acl/wire/encoding.rs` (whole module) | `compat.c:655-661` (hard-rejects at proto<30) |
| xattr wire format (flist xattr fields, `--xattrs` / `-X`)   |    |    | Y  | Y  | Y  | `crates/protocol/src/xattr/` (whole module) | `compat.c:662-668` (hard-rejects at proto<30) |
| `CF_AVOID_XATTR_OPTIM` (skip hardlink xattr dedup)          |    |    | Y  | Y  | Y  | `compatibility/flags.rs:40` `AVOID_XATTR_OPTIMIZATION` | `compat.c:721-722,746` |
| `want_xattr_optim` default-on (proto >= 31)                 |    |    |    | Y  | Y  | (consumed by xattr writer) | `compat.c:746` |

Upstream rejects `--acls` / `--xattrs` with `RERR_PROTOCOL` (exit 2) at
proto < 30. oc-rsync mirrors this exit-code mapping. CI tracks the
graceful-degradation path against rsync 3.0.9 in
`tools/ci/known_failures.conf:148-156`.

## 9. Batch file format compatibility matrix

| Batch flag                       | First proto | Bit  | Rust symbol                                | Upstream |
|----------------------------------|-------------|------|--------------------------------------------|----------|
| `recurse`                        | 28          | 0    | `crates/batch/src/format/flags.rs:18`      | `batch.c:60` |
| `preserve_uid` / `_gid` / `_links` / `_devices` | 28 | 1-4 | `batch/format/flags.rs:19-26`              | `batch.c:61-64` |
| `preserve_hard_links`            | 28          | 5    | `batch/format/flags.rs:28`                 | `batch.c:65` |
| `always_checksum`                | 28          | 6    | `batch/format/flags.rs:30`                 | `batch.c:66` |
| `xfer_dirs`                      | 29          | 7    | `batch/format/flags.rs:32`                 | `batch.c:67` |
| `do_compression`                 | 29          | 8    | `batch/format/flags.rs:34`                 | `batch.c:68`, `compat.c:194-195`, `compat.c:413-414` |
| `iconv`                          | 30          | 9    | `batch/format/flags.rs:42`                 | `batch.c:69` |
| `preserve_acls`                  | 30          | 10   | `batch/format/flags.rs:44`                 | `batch.c:70` |
| `preserve_xattrs`                | 30          | 11   | `batch/format/flags.rs:46`                 | `batch.c:71` |
| `inplace`                        | 30          | 12   | `batch/format/flags.rs:48`                 | `batch.c:72` |
| `append` / `append_verify`       | 30          | 13-14| `batch/format/flags.rs:50-53`              | `batch.c:73-74` |

Upstream's batch format does **not** record the negotiated codec
algorithm: `compat.c:194-195` hard-codes `CPRES_ZLIB` for batch reads
and `compat.c:413-414` forces `compress_choice = "zlib"` for batch
writes. oc-rsync side-steps this by writing `do_compression=false` and
storing post-decompression bytes (see
[`docs/audits/zstd-batch-compatibility.md`](zstd-batch-compatibility.md)
for the full analysis). At proto 28 there is no compression bit on the
batch wire at all, so any compressed batch produced for a 28-only peer
is unreadable by upstream regardless of algorithm.

## 10. Compatibility flag bitfield reference

The bitfield is declared in `crates/protocol/src/compatibility/flags.rs:32-48`
and named in `crates/protocol/src/compatibility/known.rs:14-42`.
Upstream definitions live at `compat.c:117-125`.

| Bit | Symbol                              | Name                       | Available    | Drives |
|-----|-------------------------------------|----------------------------|--------------|--------|
| 0   | `INC_RECURSE`                       | `CF_INC_RECURSE`           | proto >= 30  | Streaming flist + hardlink dedup |
| 1   | `SYMLINK_TIMES`                     | `CF_SYMLINK_TIMES`         | proto >= 30  | utimensat() on symlinks (Unix only) |
| 2   | `SYMLINK_ICONV`                     | `CF_SYMLINK_ICONV`         | proto >= 30  | `--iconv` on symlink targets |
| 3   | `SAFE_FILE_LIST`                    | `CF_SAFE_FLIST`            | proto >= 30  | Negotiable at 30, always-on at 31+ |
| 4   | `AVOID_XATTR_OPTIMIZATION`          | `CF_AVOID_XATTR_OPTIM`     | proto >= 30  | Disables hardlink xattr dedup |
| 5   | `CHECKSUM_SEED_FIX`                 | `CF_CHKSUM_SEED_FIX`       | proto >= 30  | MD5 seed-ordering fix |
| 6   | `INPLACE_PARTIAL_DIR`               | `CF_INPLACE_PARTIAL_DIR`   | proto >= 30  | `--inplace` + `--partial-dir` semantics |
| 7   | `VARINT_FLIST_FLAGS`                | `CF_VARINT_FLIST_FLAGS`    | proto >= 30  | Varint xflags + vstring negotiation |
| 8   | `ID0_NAMES`                         | `CF_ID0_NAMES`             | proto >= 30  | Send uid/gid 0 names on wire |

The wire format encodes the bitfield as a varint
(`compatibility/flags.rs:156` `write_to`), except in the pre-release
`'V'` compatibility mode where it is a single byte (`compat.c:736`,
`transfer/setup/capability.rs:232` `client_has_pre_release_v_flag`).
`without_unknown_bits` (`compatibility/flags.rs:110`) preserves
upstream's "tolerate but ignore future bits" behaviour for forward
compatibility.

## 11. CI coverage and known gaps

### 11.1 Versions tested

`tools/ci/run_interop.sh:53` declares `versions=(3.0.9 3.1.3 3.4.1)`.
The harness runs each scenario in two directions (push/pull) and
additionally pins the protocol via `--protocol=N` against 3.4.1 for
`N in {28,29,30,31,32}`. See
[`docs/interop/protocol-matrix.md`](../interop/protocol-matrix.md) section 3
for the full scenario inventory.

| Protocol | Tested via                                                                                |
|----------|-------------------------------------------------------------------------------------------|
| 28       | `--protocol=28` against rsync 3.4.1 (forced); legacy ASCII negotiation only               |
| 29       | `--protocol=29` against rsync 3.4.1 (forced); legacy ASCII negotiation only               |
| 30       | rsync 3.0.9 native, plus `--protocol=30` against 3.4.1; first binary handshake            |
| 31       | rsync 3.1.3 native, plus `--protocol=31` against 3.4.1                                    |
| 32       | rsync 3.4.1 native; oc-rsync's primary advertised version                                 |

### 11.2 In-tree protocol-version test files

| Test file                                       | Coverage |
|-------------------------------------------------|----------|
| `crates/protocol/tests/protocol_v27_compat.rs`  | Pre-28 boundary rejection |
| `crates/protocol/tests/protocol_v28_v31_compat.rs` | Cross-version flist xflags |
| `crates/protocol/tests/protocol_v29_compat.rs`  | Multi-char filter prefixes, iflags |
| `crates/protocol/tests/protocol_v30_compat.rs`  | First-binary handshake, varint NDX |
| `crates/protocol/tests/protocol_v31_comprehensive.rs` | Safe-flist always-on, NDX_DEL_STATS |
| `crates/protocol/tests/protocol_v32_compat.rs`  | Newest version baseline |
| `crates/protocol/tests/protocol_interop_matrix.rs` | All-versions negotiation matrix |
| `crates/protocol/tests/golden_protocol_v28_*.rs` (4 files) | Golden bytes for 4-byte LE encoding |
| `crates/protocol/tests/golden_protocol_v29_*.rs` (2 files) | Golden bytes for proto-29 flist |
| `crates/protocol/tests/protocol_feature_gates.rs` | Per-feature `>= N` predicates |

### 11.3 Documented known failures (per `tools/ci/known_failures.conf`)

Unconditional (`KNOWN_FAILURES`):

- `standalone:iconv-upstream` - daemon-mode `--iconv` not yet wired
  (#1916).
- `standalone:delta-stats` - oc-rsync delta engine not engaged in
  daemon mode.
- `standalone:upstream-compressed-batch-self-roundtrip` - upstream
  rsync 3.4.1 cannot read its own compressed batches
  (`token.c:608` inflate -3).

Protocol-gated (push direction only, `forced_proto <= 29`):

- `up:acls` - upstream `compat.c:655-661` hard-exits with `RERR_PROTOCOL`.
- `up:xattrs` - upstream `compat.c:662-668` hard-exits with `RERR_PROTOCOL`.
- `up:compress-zstd`, `up:compress-lz4` - no vstring negotiation at
  proto < 30 (`compat.c:383,556-564`).

Protocol-gated (push direction only, `forced_proto <= 28`):

- `up:merge-filter` - `exclude.c:1530` sets `legal_len=1` at proto < 29,
  rejecting the dir-merge `:` prefix.

### 11.4 Open documentation discrepancy (carried from PR #3503)

The comment at `tools/ci/known_failures.conf:81` reads:

> ```
> # versions (e.g., rsync 3.0.9 speaks protocol 28, rsync 3.1.3 speaks 30+).
> ```

PR #3503's audit at
[`docs/audits/tcpdump-daemon-filter-pull.md:383-405`](tcpdump-daemon-filter-pull.md)
flagged that rsync 3.0.9 actually advertises `PROTOCOL_VERSION = 30`,
not 28. The "28" tracks the **minimum** version 3.0.9 will accept on
the wire, not the version it speaks. The CI logic itself is sound
because the gating predicate is `forced_proto <= 29`, which is keyed
off the `--protocol=N` override rather than the advertised value.

This discrepancy is **still present** as of master `83c8aa41`. It is
out of scope for this audit (docs-only change, single comment line),
and a follow-up cleanup PR is the right place to fix it.

## 12. Surprising or non-obvious version-gating

For each entry below, the gating decision is non-obvious from the
protocol number alone and warrants comment when reading the code.

- **INC_RECURSE is not symmetric.** The `i` capability character is
  emitted only when oc-rsync acts as the **receiver**. Sender-direction
  invocations call `build_capability_string(false)` and omit `i`
  entirely, even at proto 32. This is keyed off
  `MEMORY.md::Capability string` and `transfer/setup/capability.rs:108`;
  the explicit `requires_inc_recurse` mapping at `capability.rs:38`
  enforces the asymmetry. Upstream `compat.c:712,720` is symmetric -
  oc-rsync is intentionally more conservative because INC_RECURSE
  push interop is not validated.
- **Safe file list bit lives in two places.**
  `CF_SAFE_FLIST` is a negotiable bit at protocol 30
  (`compatibility/flags.rs:38`) but is **always-on** at protocol 31+
  (`capabilities.rs:151` `safe_file_list_always_enabled`). Code that
  branches on the bit must also check the version; checking only the
  bit will under-report safe-flist mode at 31+.
- **Multiplex `OUT_MULTIPLEXED` is gated at protocol < 31.**
  `io.c:1227` reads `if (protocol_version < 31 && OUT_MULTIPLEXED)`.
  Above 31, the multiplex output channel is always considered open.
  This affects keep-alive frame timing.
- **The `v` capability bit drives codec negotiation.** Without `v` in
  the client's capability string, the server falls back to hardcoded
  `"zlib"` at `compat.c:383` even at proto 32. zstd and lz4 are
  unreachable without `v`, which is why the capability string in
  `MEMORY.md::Capability string` is the single source of truth.
- **The `'V'` capability is not the same as `'v'`.** Upstream once
  used uppercase `V` for a pre-release variant of varint flist flags
  that wrote compat flags as a single byte rather than a varint.
  Detection lives in `transfer/setup/capability.rs:232`. New code must
  not invent a similar collision.
- **MD4 -> MD5 boundary is at protocol 30, but MD5 is not the only
  option there.** Protocol 30+ supports negotiated MD4 / MD5 / XXH3 /
  XXH128 via `do_negotiated_strings` (`compat.c:729-732`). The default
  is MD5 only when the `v` capability is not set; with `v`, the
  configured preference (XXH3 in oc-rsync's default build) wins.

## 13. References

### oc-rsync sources cited

- `crates/protocol/src/version/constants.rs` - protocol range constants.
- `crates/protocol/src/version/mod.rs` - module surface re-exports.
- `crates/protocol/src/version/protocol_version/capabilities.rs` -
  per-version capability predicates and `ProtocolCapabilities` newtype.
- `crates/protocol/src/compatibility/flags.rs` - `CompatibilityFlags`
  bitfield, varint write/read, `without_unknown_bits` for forward
  compatibility.
- `crates/protocol/src/compatibility/known.rs` - `KnownCompatibilityFlag`
  enum and canonical `CF_*` name mapping.
- `crates/protocol/src/codec/ndx/constants.rs` - `NDX_DONE`,
  `NDX_FLIST_EOF`, `NDX_DEL_STATS`, legacy/modern wire bytes.
- `crates/protocol/src/filters/wire.rs` - `parse_wire_rule` with
  `uses_old_prefixes` branch, `RuleType` enum.
- `crates/protocol/src/wire/file_entry/constants.rs` and
  `crates/protocol/src/flist/flags.rs` - `XMIT_*` xflag bits including
  `XMIT_SAME_DEV_PRE30`, `XMIT_RDEV_MINOR_8_PRE30`, `XMIT_HLINKED`.
- `crates/transfer/src/setup/capability.rs` - `build_capability_string`,
  `CAPABILITY_MAPPINGS` table, `parse_client_info`,
  `client_has_pre_release_v_flag`.
- `crates/batch/src/format/flags.rs` - `BatchFlags` per-bit upstream
  cites for protocols 28-30.

### Upstream rsync 3.4.1 sources cited

Local copy at `target/interop/upstream-src/rsync-3.4.1/`. If missing,
fetch via `tools/ci/run_interop.sh` or `curl
https://download.samba.org/pub/rsync/src/rsync-3.4.1.tar.gz`.

- `rsync.h:114` `PROTOCOL_VERSION 32`, `:147` `MIN_PROTOCOL_VERSION 20`,
  `MAX_PROTOCOL_VERSION 40`, `:285-288` `NDX_DONE`/`NDX_FLIST_EOF`/
  `NDX_DEL_STATS`/`NDX_FLIST_OFFSET`.
- `compat.c:100-112` `valid_compressions_items[]`,
  `:117-125` `CF_*` macro definitions,
  `:194-195` batch read forces `CPRES_ZLIB`,
  `:383` `strlcpy(tmpbuf, "zlib", ...)` fallback,
  `:413-414` batch write forces `compress_choice = "zlib"`,
  `:552,859` MD4/MD5 default selection,
  `:556-564` codec vstring negotiation,
  `:655-668` ACL/xattr proto<30 hard-rejection,
  `:710-755` proto>=30 compat flag exchange,
  `:712,720` `set_allow_inc_recurse` and `CF_INC_RECURSE`,
  `:729-732` `do_negotiated_strings` gating on `'v'`,
  `:733-737` pre-release `'V'` byte-encoded path,
  `:746` `want_xattr_optim` proto>=31 default,
  `:750-753` `--crtimes` gating,
  `:775` `use_safe_inc_flist` proto>=31 always-on.
- `io.c:1227` `OUT_MULTIPLEXED` proto<31 branch,
  `:1684` `MSG_IO_TIMEOUT` proto>=31,
  `:1795` `read_varint`, `:2089` `write_varint`,
  `:2243-2287` `write_ndx`, `:2290-2299` `read_ndx`.
- `flist.c:411,505,829` `protocol_version >= 30` inline hardlink/dev
  encoding,
  `:436,461,623` `protocol_version < 28` legacy paths,
  `:447` `XMIT_RDEV_MINOR_8_PRE30` minor encoding,
  `:487` `NSEC_BUMP` proto>=31 high-resolution mtime,
  `:622` device-special proto<31 special-casing.
- `exclude.c:1119-1133` `XFLG_OLD_PREFIXES` parsing branch,
  `:1350` `FILTRULE_PERISHABLE` proto>=30 gate,
  `:1530` `legal_len` 1-vs-2 prefix length,
  `:1567-1571` `s`/`r` modifier proto>=29 gate,
  `:1675` `xflags = protocol_version >= 29 ? 0 : XFLG_OLD_PREFIXES`.
- `batch.c:59-76` `flag_ptr[]` array (per-flag mapping for stream
  flags), `:608` upstream batch inflate failure.
- `main.c:880-905` `read_final_goodbye` with proto>=31 extra round-trip,
  `:1304-1305` multiplex activation.
- `sender.c:180-187` `write_ndx_and_attrs` iflags proto>=29.

### Sibling oc-rsync documents

- [`docs/interop/protocol-matrix.md`](../interop/protocol-matrix.md) -
  CI scenario matrix; sections 1-3 list per-version test coverage.
- [`docs/protocol-compatibility.md`](../protocol-compatibility.md) -
  release-by-release prose summary.
- [`docs/audits/zstd-batch-compatibility.md`](zstd-batch-compatibility.md) -
  task #1685, why zstd batches cannot interop with upstream.
- [`docs/audits/tcpdump-daemon-filter-pull.md`](tcpdump-daemon-filter-pull.md) -
  task #1697, on-the-wire confirmation of filter rule encoding;
  contains the original 3.0.9 protocol-number discrepancy note at
  section 5.
