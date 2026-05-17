# Fuzz coverage matrix and gap analysis

Tracking issues: #2314 (inventory), #2315 (gap analysis), #2316
(FCV-3 follow-up decomposition). Companion to
`docs/design/protocol-fuzzing-harness.md`,
`docs/design/wire-format-differential-fuzzer.md`,
`docs/design/differential-filter-fuzzer.md`, and the per-gap follow-up
breakdown in `docs/audits/fuzz-coverage-gap-followups.md`. This audit
catalogues every existing fuzz target in the workspace, maps the
protocol-parsing entry points that consume untrusted bytes, and ranks
the gaps so FCV-3+ has a clear queue.

## 1. Existing fuzz targets

Three cargo-fuzz workspaces live in the tree, each excluded from the root
workspace so `libfuzzer-sys`'s nightly-only linker flags do not break
ordinary builds:

- **`fuzz/`** - top-level harnesses for the highest-traffic attack
  surfaces.
- **`crates/protocol/fuzz/`** - per-crate harnesses for protocol wire
  parsers.
- **`crates/filters/fuzz/`** - per-crate harnesses for filter rule
  parsing and chain evaluation.

### Table 1: existing fuzz targets

| Target | Workspace | Subsystem | Input shape | Assertions | Last finding | CI schedule |
|--------|-----------|-----------|-------------|------------|--------------|-------------|
| `protocol_wire` | `fuzz/` | multiplex `BorrowedMessageFrames` iterator | raw `&[u8]` | panic-only | none recorded (no `fuzz/artifacts/`) | on-demand |
| `simd_checksum_parity` | `fuzz/` | rolling + MD4/MD5 batch SIMD vs scalar | raw `&[u8]` | byte-equal parity across all dispatchers and 17 lanes | none recorded | on-demand |
| `filter_differential` | `fuzz/` | `filters::FilterSet` decisions vs upstream 3.4.2 (`--dry-run --recursive --verbose --out-format=I:%n`) | `Arbitrary` (rules + path + dir flag) | verdict equality with upstream child | none recorded | **nightly 02:00 UTC** (`filter-fuzzer-overnight.yml`, 3600 s/target/run) |
| `filter_rules_vs_upstream` | `fuzz/` | `filters::FilterSet` vs upstream `--list-only`; adds `!` clear directive | `Arbitrary` (rules + path + dir flag) | verdict equality with upstream child | none recorded | **nightly 02:00 UTC** (same workflow) |
| `fuzz_varint` | `crates/protocol/fuzz/` | `read_varint`, `read_varlong`, `read_varlong30`, `decode_varint` | raw `&[u8]` | panic-only | none recorded | on-demand |
| `varint_roundtrip` | `crates/protocol/fuzz/` | varint/varlong/longint/varint30 + `TransferStats` + `DeleteStats` encode/decode | `Arbitrary` (typed values + raw bytes) | encode-then-decode equality, panic-only on raw bytes | none recorded | on-demand |
| `fuzz_delta` | `crates/protocol/fuzz/` | `wire::read_token`, `wire::read_delta`, `wire::read_int`, `wire::read_signature` | raw `&[u8]` | panic-only | none recorded | on-demand |
| `fuzz_multiplex_frame` | `crates/protocol/fuzz/` | `BorrowedMessageFrame::decode_from_slice`, `MessageHeader::decode`, `recv_msg`, `recv_msg_into` | raw `&[u8]` | panic-only | none recorded | on-demand |
| `multiplex_frame_roundtrip` | `crates/protocol/fuzz/` | `MessageFrame`/`BorrowedMessageFrame` encode-decode roundtrip + oversize rejection | `Arbitrary` (code selector + payload + raw bytes) | code + payload equality, oversize rejection, panic-only on raw bytes | none recorded | on-demand |
| `fuzz_legacy_greeting` | `crates/protocol/fuzz/` | `parse_legacy_daemon_greeting{,_bytes,_details,_owned}`, error/warning message parsers | raw `&[u8]` (plus UTF-8 string variants) | panic-only | none recorded | on-demand |
| `file_entry_roundtrip` | `crates/protocol/fuzz/` | `wire::file_entry::encode_*` + `wire::file_entry_decode::decode_*` (flags, size, mtime, mode, uid, gid, name, symlink, atime, crtime, checksum, end marker) | `Arbitrary` (typed field values + protocol selector 28-32) | per-field encode-decode equality | none recorded | on-demand |
| `fuzz_filter_parse` | `crates/filters/fuzz/` | `filters::parse_rules` + `FilterSet::from_rules` | raw `&[u8]` filtered to UTF-8 | panic-only | none recorded | on-demand (seed corpus 39 files under `corpus/filter_parse/`) |
| `fuzz_filter_chain` | `crates/filters/fuzz/` | `parse_rules` + `FilterSet::{allows,allows_deletion,allows_deletion_when_excluded_removed,excluded_dir_by_non_dir_rule,is_empty}` | `Arbitrary` (rule text + path entries) | panic-only across every decision method | none recorded (seed corpus 10 files under `corpus/filter_chain/`) | on-demand |

Total targets: **13** across 3 workspaces.

Total scheduled targets: **2** (both filter differential targets, nightly).
The remaining 11 targets run only on demand via
`tools/ci/run_filter_fuzz.sh`, `tools/ci/run_filter_differential_fuzz.sh`,
or direct `cargo +nightly fuzz run` invocation.

### Nightly coverage report (informational)

The `Fuzz Coverage Report` workflow (`.github/workflows/fuzz-coverage-report.yml`)
runs `cargo fuzz coverage <target> -- -max_total_time=300` for every target
in the three fuzz workspaces nightly at 03:30 UTC. Each run uploads a
`coverage/<target>.lcov` artifact with 30-day retention, and the GitHub step
summary lists per-target line-coverage percentages. The job is marked
`continue-on-error: true` while baseline thresholds stabilise; promote it to
a required check once each target has a documented minimum line-coverage
target and the lcov totals trend monotonically upward across consecutive
nightly runs.

Maturity signals:

- `fuzz/corpus/` and `fuzz/artifacts/` are absent at the top-level
  workspace - those targets rely entirely on coverage-guided generation
  with no seeded corpus.
- `crates/protocol/fuzz/corpus/` and `crates/protocol/fuzz/artifacts/`
  are absent - same story for the per-crate protocol targets.
- `crates/filters/fuzz/corpus/` is the only seeded corpus (49 files
  across `filter_parse/` and `filter_chain/`); no artifacts directory.
- No target has a recorded finding committed to the tree. The nightly
  workflow uploads `fuzz/artifacts/**` on failure and fails the job, so
  any historic find would have been triaged through the artifact path
  rather than checked in.

## 2. Protocol parsing entry points

Every public function below consumes bytes from an untrusted peer (network
peer, daemon config file, command-line arg, or filesystem). The "Fuzz
target" column lists the existing target that exercises the function, or
`NONE` when no target reaches it. Priority ratings:

- **P0** - critical attack surface. Reachable on every connection; bug
  here means remote DoS or worse. Must have an in-process panic target.
- **P1** - high-value. Reachable on most connections or controls
  resource sizing. Should have a structured-input roundtrip target.
- **P2** - medium. Reachable when an optional feature is enabled
  (xattr, ACL, iconv, files-from).
- **P3** - low. Local-only or constrained input.

### Table 2: protocol entry points

| Subsystem | Entry point (`file:line`) | Fuzz target | Priority gap |
|-----------|---------------------------|-------------|--------------|
| Multiplex frame iterator | `crates/protocol/src/multiplex/borrowed.rs:72` (`BorrowedMessageFrame::decode_from_slice`) | `protocol_wire`, `fuzz_multiplex_frame`, `multiplex_frame_roundtrip` | covered (P0) |
| Multiplex `MessageFrame` | `crates/protocol/src/multiplex/frame.rs:191` (`MessageFrame::decode_from_slice`) | `fuzz_multiplex_frame`, `multiplex_frame_roundtrip` | covered (P0) |
| Varint decode | `crates/protocol/src/varint/decode.rs:59,93,133,151,159,170,187` | `fuzz_varint`, `varint_roundtrip` | covered (P0) |
| Delta token / op stream | `crates/protocol/src/wire/delta/token.rs:208`, `wire/delta/internal.rs:64,127` | `fuzz_delta` | covered (P0) |
| Delta signature header | `crates/protocol/src/wire/signature.rs:166` (`read_signature`) | `fuzz_delta` | covered (P0) |
| Delta int encoding | `crates/protocol/src/wire/delta/int_encoding.rs:62` (`read_int`) | `fuzz_delta` | covered (P0) |
| File entry field decoders (flags, size, mtime, mtime_nsec, atime, crtime, mode, uid, gid, rdev, checksum, symlink, hardlink_idx, hardlink_dev_ino, name, end marker) | `crates/protocol/src/wire/file_entry_decode/{flags,size,timestamps,mode,ownership,device,checksum,symlink,hardlink,name}.rs` | `file_entry_roundtrip` (flags, size, mtime, mode, uid, gid, name, symlink, atime, crtime, checksum, end marker) | **partial (P0)**: `mtime_nsec`, `rdev`, `hardlink_idx`, `hardlink_dev_ino` not roundtripped |
| File entry stream | `crates/protocol/src/flist/read/mod.rs:469,491,756` (`read_entry`, `read_entry_with_flist`, `read_file_entry`) | NONE | **P0 GAP** |
| Incremental file list stream | `crates/protocol/src/flist/incremental/streaming.rs:57,86` (`next_ready`, `read_all`) | NONE | **P1 GAP** |
| Legacy daemon greeting + log/warn lines | `crates/protocol/src/legacy/{greeting/parse.rs:14,39,47,bytes.rs:37-90,lines.rs:71,173,183}`, `negotiation/sniffer/legacy.rs:16,81,91` | `fuzz_legacy_greeting` (parse only; sniffer paths not reached) | **partial (P0)**: `read_legacy_daemon_line`, `read_and_parse_legacy_daemon_greeting{,_details}` not exercised |
| Negotiation prologue sniffer | `crates/protocol/src/negotiation/sniffer/observe.rs:65,78` (`observe_byte`, `read_from`) | NONE | **P0 GAP** |
| Capability negotiation (vstring exchange) | `crates/protocol/src/negotiation/capabilities/negotiate.rs:121,163` | NONE | **P0 GAP** |
| Compatibility flags byte | `crates/protocol/src/compatibility/flags.rs:175,184,212` | NONE | **P1 GAP** |
| Filter list wire format | `crates/protocol/src/filters/wire.rs:181` (`read_filter_list`) | NONE | **P1 GAP** |
| Filter rule grammar (text) | `crates/filters/src/parse.rs` via `filters::parse_rules` | `fuzz_filter_parse`, `fuzz_filter_chain`, `filter_differential`, `filter_rules_vs_upstream` | covered (P0) |
| Filter chain evaluation | `crates/filters/src/set.rs` via `FilterSet::allows*` | `fuzz_filter_chain`, both differential targets | covered (P0) |
| Index codec (ndx) | `crates/protocol/src/codec/ndx/state.rs:97`, `codec/ndx/goodbye.rs:48` | NONE | **P1 GAP** |
| Idlist (uid/gid name table) | `crates/protocol/src/idlist/mod.rs:232,256` (`read`, `read_with_kind`) | NONE | **P1 GAP** |
| Transfer stats | `crates/protocol/src/stats/transfer.rs:219` (`TransferStats::read_from`) | `varint_roundtrip` (typed roundtrip + raw bytes) | covered (P1) |
| Delete stats | `crates/protocol/src/stats/delete.rs:91` (`DeleteStats::read_from`) | `varint_roundtrip` (typed roundtrip + raw bytes) | covered (P1) |
| ACL wire decoder | `crates/protocol/src/acl/definition/wire.rs:48` (`read_acl_definition`) | NONE | **P2 GAP** |
| Xattr wire decoders | `crates/protocol/src/xattr/wire/decode.rs:52,118,171,208` (`read_xattr_definitions`, `recv_xattr`, `recv_xattr_request`, `recv_xattr_values`) | NONE | **P2 GAP** |
| `--files-from` stream parser | `crates/protocol/src/files_from.rs:59,162` (`forward_files_from`, `read_files_from_stream`) | NONE | **P2 GAP** |
| Iconv spec parser | `crates/core/src/client/config/iconv.rs:25` (`IconvSpec::parse` / `from_str`) | NONE | **P2 GAP** |
| Daemon config (`rsyncd.conf`) parser | `crates/daemon/src/rsyncd_config/mod.rs:82` (`RsyncdConfig::parse`) | NONE | **P2 GAP** |
| Bandwidth limit string parser | `crates/bandwidth/src/parse/components.rs` (`BwLimit::parse`) | NONE | **P3 GAP** |
| Skip-compress suffix list | `crates/compress/src/skip_compress/decider.rs:95` (`parse_skip_compress_list`) | NONE | **P3 GAP** |
| Compression decoders (zlib/lz4/zstd) | `crates/compress/src/{zlib/helpers.rs:17,lz4/raw.rs:217,253,lz4/frame.rs:182,190,zstd.rs:237,245}` | NONE | **P1 GAP** |
| Strong checksum batch dispatcher (SIMD parity) | `crates/checksums/src/strong/{md4,md5}_batch.rs` | `simd_checksum_parity` | covered (P1) |
| Rolling checksum (SIMD parity) | `crates/checksums/src/rolling/mod.rs` | `simd_checksum_parity` | covered (P1) |

## 3. Identified gaps and recommended priority order

Gap-by-gap priority queue for FCV-3+:

1. **`flist_entry_decode`** (P0). The file-list entry stream is the
   single largest untrusted-input surface after the multiplex frame
   header. `file_entry_roundtrip` exercises individual field codecs but
   never assembles a full `FileEntry` via `read_entry` /
   `read_entry_with_flist` / `read_file_entry`, so the state-machine
   that sequences field decodes against XMIT flag bits is unfuzzed.
   Required: in-process target feeding raw bytes through
   `read_file_entry` with every supported protocol (28-32) and every
   transmit-flag combination the previous entry's flags can express.
2. **`negotiation_prologue`** (P0). Every connection passes through
   `NegotiationPrologueSniffer::observe_byte` before any other parser
   runs. A panic here is a pre-authentication remote DoS. Required:
   raw-byte target that feeds arbitrary slices byte-by-byte through
   `observe_byte` and asserts the prologue classifier never panics.
3. **`capability_vstring`** (P0). `negotiate_capabilities` reads
   vstrings from the peer before any compression or checksum context
   exists - any panic in vstring parsing is unauthenticated remote DoS.
   Required: in-process target that pre-stages arbitrary bytes as the
   peer's vstring stream and drives both `negotiate_capabilities` and
   `negotiate_capabilities_with_override` for protocols 28-32 in client
   and server roles.
4. **`legacy_sniffer`** (P0 fill-in). `fuzz_legacy_greeting` covers the
   pure parsers; the reader-driven sniffer (`read_legacy_daemon_line`,
   `read_and_parse_legacy_daemon_greeting{,_details}`) is unfuzzed.
   Lift `fuzz_legacy_greeting` to also drive the reader paths through
   an in-memory `Cursor`.
5. **`compat_flags`** (P1). One-byte decode, but
   `CompatibilityFlags::decode_from_slice{,_mut}` and `read_from` are
   the gate for every subsequent codec switch (varint30, longints,
   incremental flist). A miscount here corrupts every later parse.
6. **`filter_list_wire`** (P1). `read_filter_list` decodes the
   wire-serialised filter chain that the sender ships to the receiver.
   Distinct surface from the text-grammar parser already covered by
   `fuzz_filter_parse`.
7. **`ndx_codec`** (P1). `NdxState::read_ndx` and the goodbye codec
   gate the post-flist phase of every transfer.
8. **`incremental_flist`** (P1). `IncrementalFileList::read_all` /
   `next_ready` sit on top of `read_file_entry` and add a state
   machine for sub-list segments and parent-dir signalling.
9. **`idlist`** (P1). `IdList::read` / `read_with_kind` consume an
   untrusted uid/gid name table.
10. **`compress_decoders`** (P1). `compress::{zlib,lz4,zstd}::
    decompress_to_vec` consume framed bytes the peer chose. Even when
    the underlying RustCrypto/zstd-rs implementation is panic-free, our
    wrappers compute output bounds and call `Vec::with_capacity` -
    a bad length prefix is a memory-blowup vector.
11. **`acl_wire`** (P2). `read_acl_definition`.
12. **`xattr_wire`** (P2). `read_xattr_definitions`, `recv_xattr`,
    `recv_xattr_request`, `recv_xattr_values`.
13. **`files_from`** (P2). `read_files_from_stream` + the forwarding
    path.
14. **`iconv_spec`** (P2). `IconvSpec::parse` accepts user-supplied
    strings but is reachable from `--iconv=<spec>` on the command line.
15. **`rsyncd_conf`** (P2). Config file parser; only reachable through
    the trusted local filesystem, but it is human-edited and the
    grammar has macro / module-section interactions worth exploring.
16. **`bwlimit_parse`** (P3). CLI string parser only.
17. **`skip_compress_list`** (P3). CLI string parser only.

The `file_entry_roundtrip` target also has internal gaps within an
already-covered subsystem: `mtime_nsec`, `rdev`, `hardlink_idx`, and
`hardlink_dev_ino` codecs are not roundtripped. Folding them in is a
small, in-place extension - not a new target - but should land before
FCV-3 to avoid leaving the matrix half-green.

## 4. Recommendation: top three targets to add first

1. **`flist_entry_decode`** - the highest-volume untrusted input on
   every transfer. Highest panic-risk surface with no current coverage.
2. **`negotiation_prologue`** - pre-authentication entry point reached
   on the very first byte of every connection. Cheap in-process target,
   immediate DoS hardening.
3. **`capability_vstring`** - the vstring decoder behind
   `negotiate_capabilities`. Reachable before any encryption / framing
   context exists; a panic here is unauthenticated remote DoS.

All three are in-process targets with sub-millisecond exec budgets,
suitable for inclusion in the existing nightly fuzz workflow without
budget pressure.
