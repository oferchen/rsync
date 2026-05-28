# WDF-1: Wire-Level Differential Fuzz Coverage Audit

Audit date: 2026-05-28

## 1. Fuzz Target Inventory

### 1.1 Top-Level Targets (`fuzz/fuzz_targets/`)

| # | Target | Type | Protocol Layer | Coverage Scope |
|---|--------|------|---------------|----------------|
| 1 | `protocol_wire` | Parser-only | Multiplex | `BorrowedMessageFrames` frame walker; no-panic on arbitrary bytes |
| 2 | `multiplex_frame_parse` | Parser + round-trip | Multiplex | `MessageHeader::decode`, `from_raw`, `BorrowedMessageFrames`; structured input verifies `decode`/`from_raw` agree and `encode_raw` round-trips |
| 3 | `simd_checksum_parity` | Behavioral parity | Checksums | SIMD vs scalar parity for `RollingChecksum`, `md5_digest_batch`, `md4_digest_batch` |
| 4 | `filter_differential` | Differential (vs upstream) | Filters | `FilterSet::allows` vs upstream rsync `--dry-run --out-format=I:%n`; spawns child process |
| 5 | `filter_rules_vs_upstream` | Differential (vs upstream) | Filters | `FilterSet::allows` vs upstream rsync `--list-only`; includes `!` clear directive |
| 6 | `filter_list_wire` | Parser-only | Filter wire | `read_filter_list` across protocol 28-32; no-panic |
| 7 | `flist_entry_decode` | Parser-only | File list | `FileListReader::read_entry` and `read_file_entry` with `Arbitrary`-driven preserve-flag matrix and protocol version selector |
| 8 | `incremental_flist` | Parser-only | File list (INC_RECURSE) | `StreamingFileList` state machine - decode, dependency tracking, finalize orphans |
| 9 | `varint_decode` | Parser + round-trip | Varint/varlong | All decode entry points + full round-trip for `varint`, `varlong`, `int`, `longint`, `varint30` |
| 10 | `ndx_codec` | Parser-only | NDX (file-index) | `NdxCodec::read_ndx` across protocol 28-32; stateful delta-encoded stream |
| 11 | `legacy_greeting` | Parser-only | Daemon greeting | All 6 `parse_legacy_daemon_greeting*` variants (byte + string) |
| 12 | `daemon_greeting` | Parser-only | Daemon greeting | `parse_legacy_daemon_greeting_bytes*` (3 variants); details accessors |
| 13 | `capability_flags` | Parser + round-trip | Compat flags | `CompatibilityFlags::read_from`, `decode_from_slice*`, `from_bits`; round-trip via `encode_to_vec`/`decode_from_slice` |
| 14 | `vstring` | Parser-only | Capability negotiation | `negotiate_capabilities` with `Arbitrary`-driven protocol, role, compression flags |
| 15 | `acl_xattr_wire` | Parser-only | ACL/xattr wire | `read_acl_definition`, `read_xattr_definitions`, `recv_xattr`, `recv_xattr_request`, `recv_xattr_values` |
| 16 | `decompressor_zlib` | Parser-only (+ expansion cap) | Compression | `CountingZlibDecoder`, `decompress_to_vec`; asserts 100x expansion ceiling |
| 17 | `decompressor_zstd` | Parser-only (+ expansion cap) | Compression | `CountingZstdDecoder`, `decompress_to_vec`; asserts 100x expansion ceiling |
| 18 | `batch_reader` | Parser-only | Batch file | `BatchReader::new`, `read_header`, `read_data`; temp-file materialization |
| 19 | `rsyncd_conf` | Parser-only | Daemon config | `RsyncdConfig::parse`; no-panic on arbitrary UTF-8 |
| 20 | `auth_response` | Parser-only | Auth | `verify_client_response` with fuzzed protocol selectors; `SecretsFile::parse` |
| 21 | `bwlimit` | Parser-only | CLI/config | `parse_bandwidth_argument`, `parse_bandwidth_limit`; no-panic on arbitrary UTF-8 |

### 1.2 Protocol-Crate Targets (`crates/protocol/fuzz/fuzz_targets/`)

| # | Target | Type | Protocol Layer | Coverage Scope |
|---|--------|------|---------------|----------------|
| 22 | `fuzz_varint` | Parser + round-trip + truncation | Varint/varlong | All decoders + boundary values (i32/i64 edges) + every truncated prefix; `read_varint30_int` legacy/modern |
| 23 | `varint_roundtrip` | Round-trip | Varint/varlong + stats | `write_*/read_*` for varint, varlong, varlong30, longint, int, varint30_int; `TransferStats` and `DeleteStats` round-trip |
| 24 | `fuzz_delta` | Parser-only | Delta wire | `read_token`, `read_delta`, `read_int`, `read_signature` |
| 25 | `fuzz_legacy_greeting` | Parser-only | Daemon greeting | Same parsers as #11 plus `parse_legacy_error_message*`, `parse_legacy_warning_message*`, `parse_legacy_daemon_message` |
| 26 | `fuzz_multiplex_frame` | Parser-only | Multiplex | `BorrowedMessageFrame::decode_from_slice`, `MessageHeader::decode`, `recv_msg`, `recv_msg_into` |
| 27 | `multiplex_frame_roundtrip` | Round-trip | Multiplex | `MessageFrame::new`/`encode_into_vec`/`decode_from_slice`; `encode_into_writer`/`recv_msg`; `BorrowedMessageFrame`; oversized payload rejection |
| 28 | `file_entry_roundtrip` | Round-trip | File list entry fields | Per-field encode/decode: flags, size, mtime, mode, uid, gid, name (prefix compression), symlink target, atime, crtime, checksum, end marker |
| 29 | `negotiation_prologue` | Parser-only | Negotiation sniffer | `NegotiationPrologueSniffer::observe_byte`, `observe`, `read_from` |

### 1.3 Filters-Crate Targets (`crates/filters/fuzz/fuzz_targets/`)

| # | Target | Type | Protocol Layer | Coverage Scope |
|---|--------|------|---------------|----------------|
| 30 | `fuzz_filter_parse` | Parser-only | Filter rules | `parse_rules` + `FilterSet::from_rules`; no-panic on arbitrary UTF-8 |
| 31 | `fuzz_filter_chain` | Behavioral | Filter evaluation | `FilterSet::allows`, `allows_deletion`, `allows_deletion_when_excluded_removed`, `excluded_dir_by_non_dir_rule` |

**Total: 31 fuzz targets across 3 fuzz workspaces.**

### 1.4 Classification Summary

| Category | Count | Targets |
|----------|-------|---------|
| Parser-only (decode, no-panic) | 17 | #1, #6, #7, #8, #10, #11, #12, #14, #15, #16, #17, #18, #19, #20, #21, #24, #25, #26, #29, #30 |
| Round-trip (encode then decode, assert equality) | 7 | #2, #9, #13, #22, #23, #27, #28 |
| Behavioral parity (SIMD vs scalar) | 2 | #3, #31 |
| Differential (vs upstream rsync binary) | 2 | #4, #5 |

## 2. Wire-Level Coverage Gap Matrix

Each row represents a protocol feature that appears on the wire. Columns indicate the highest level of fuzz coverage available.

| Protocol Feature | None | Decode-only | Round-trip | Differential | Notes |
|-----------------|------|------------|------------|-------------|-------|
| **Handshake / Negotiation** | | | | | |
| Legacy `@RSYNCD:` greeting | | X | | | #11, #12, #25 - 6+ parser variants |
| Negotiation prologue sniffer | | X | | | #29 - byte/bulk/reader paths |
| Capability flags exchange | | | X | | #13 - `from_bits`/`encode_to_vec`/`decode_from_slice` |
| Vstring negotiation | | X | | | #14 - drives `negotiate_capabilities` but does not compare against upstream |
| Checksum negotiation | X | | | | No fuzz target covers the checksum-seed exchange or negotiation protocol |
| Server args exchange | X | | | | No fuzz target covers server-args wire encoding |
| **Multiplex Layer** | | | | | |
| MSG_* frame header | | | X | | #2, #27 - structured round-trip + `decode`/`from_raw` parity |
| MSG_* frame walker | | X | | | #1, #26 - `BorrowedMessageFrames` iteration |
| `recv_msg`/`send_msg` pair | | | X | | #27 - encode via writer, decode via `recv_msg` |
| MSG_* frame vs upstream | X | | | | No differential comparison of multiplex framing against upstream |
| **Variable-Length Integers** | | | | | |
| varint (i32) | | | X | | #9, #22, #23 - streaming + in-memory round-trip |
| varlong (i64) | | | X | | #9, #22, #23 - all `min_bytes` widths |
| varlong30 | | | X | | #23 - separate round-trip |
| longint (legacy) | | | X | | #9, #22, #23 |
| int (fixed 4-byte) | | | X | | #9, #22, #23 |
| varint30_int (version-gated) | | | X | | #9, #22, #23 - both legacy and modern branches |
| varint vs upstream encoding | X | | | | No differential check that oc-rsync varints match upstream byte-for-byte |
| **File List** | | | | | |
| File entry decode (XMIT flags) | | X | | | #7 - `Arbitrary` preserve-flag matrix |
| File entry per-field round-trip | | | X | | #28 - flags, size, mtime, mode, uid, gid, name, symlink, atime, crtime, checksum |
| INC_RECURSE streaming state machine | | X | | | #8 - `StreamingFileList` decode + finalize |
| File entry encode vs upstream | X | | | | No comparison of encoded file-list bytes against upstream `send_file_entry()` |
| File-list sorting / ordering | X | | | | No fuzz coverage of sort order or cross-referencing |
| **NDX (File-Index) Codec** | | | | | |
| NDX decode (stateful delta) | | X | | | #10 - 5 protocol versions |
| NDX encode/decode round-trip | X | | | | Missing; `NdxCodec::write_ndx` not fuzzed |
| NDX vs upstream | X | | | | No differential comparison |
| **Delta Transfer** | | | | | |
| Delta token decode | | X | | | #24 - `read_token`, `read_delta` |
| Delta token encode/decode round-trip | X | | | | No `write_token`/`send_token` round-trip fuzz target |
| Signature block decode | | X | | | #24 - `read_signature` |
| Signature block round-trip | X | | | | `write_signature`/`read_signature` not exercised by fuzzer (unit tests exist) |
| Delta stream vs upstream | X | | | | No comparison of delta output against upstream for same basis+file |
| **Compression** | | | | | |
| Zlib decompress | | X | | | #16 - expansion cap enforced |
| Zstd decompress | | X | | | #17 - expansion cap enforced |
| Compressed token stream | X | | | | `compressed_token/{encoder,decoder}` - no fuzz target for the token-level compress/decompress cycle |
| Compress vs upstream | X | | | | No comparison of compressed wire bytes against upstream |
| **Filters** | | | | | |
| Filter rule parse | | X | | | #30 - no-panic |
| Filter chain evaluation | | | | X | #4, #5 - vs upstream rsync binary |
| Filter list wire decode | | X | | | #6 - `read_filter_list` across versions |
| Filter list wire encode | X | | | | `write_filter_list` not fuzzed |
| Filter list wire round-trip | X | | | | Missing encode/decode pair |
| **ACL / Xattr** | | | | | |
| ACL definition decode | | X | | | #15 - `read_acl_definition` |
| Xattr definition decode | | X | | | #15 - all five entry points |
| ACL encode/decode round-trip | X | | | | `send_acl_definition` not fuzzed |
| Xattr encode/decode round-trip | X | | | | `send_xattr*` not fuzzed |
| ACL/xattr vs upstream | X | | | | No wire comparison |
| **Statistics** | | | | | |
| TransferStats round-trip | | | X | | #23 - all protocol versions |
| DeleteStats round-trip | | | X | | #23 |
| Stats vs upstream | X | | | | No comparison of stats encoding |
| **Batch File** | | | | | |
| Batch header decode | | X | | | #18 - `read_header` + `read_data` |
| Batch header round-trip | X | | | | No `write_header`/`read_header` pair |
| **Auth** | | | | | |
| Auth response verify | | X | | | #20 - multi-algorithm response, secrets file parse |
| Auth challenge/response round-trip | X | | | | No encode/verify cycle fuzz target |
| **Daemon Config** | | | | | |
| rsyncd.conf parse | | X | | | #19 |
| **CLI Parsers** | | | | | |
| Bandwidth limit parse | | X | | | #21 |
| **Checksums** | | | | | |
| Rolling + strong SIMD parity | | | | | #3 - SIMD vs scalar (not differential vs upstream) |
| Checksum vs upstream | X | | | | No comparison of checksum output against upstream for same input |
| **ID List** | | | | | |
| uid/gid name mapping wire | X | | | | `idlist` module has no fuzz target |
| **Iconv** | | | | | |
| Character encoding conversion | X | | | | `iconv` module has no fuzz target |
| **End-to-End Protocol** | | | | | |
| Full handshake sequence | X | | | | No fuzz target drives a complete handshake exchange |
| Full file-list exchange | X | | | | No fuzz target drives sender->receiver file list |
| Full delta transfer | X | | | | No fuzz target drives a complete file transfer |
| Full session vs upstream | X | | | | No differential comparison of a complete transfer session |

### 2.1 Coverage Summary

| Coverage Level | Features Covered | Features Missing |
|---------------|-----------------|-----------------|
| Differential (vs upstream) | 1 (filter evaluation) | 18+ |
| Round-trip (encode/decode) | 10 | 11 |
| Decode-only (no-panic) | 20 | 6 |
| No coverage | - | 6 (checksum negotiation, server args, id-list wire, iconv, compressed token stream, full session) |

## 3. Priority Ranking for New Differential Fuzz Targets

Priority is determined by: (1) protocol attack surface breadth, (2) semantic divergence risk, (3) implementation complexity, (4) existing coverage gap severity.

### P0 - Critical (WDF-2, WDF-3)

**WDF-2: File-list entry differential fuzzing**

- **Gap**: File-list encoding is the most complex wire format in rsync (XMIT flags, run-length path compression, varint sizes, uid/gid mapping, optional ACL/xattr indexes). Round-trip fuzz exists (#28) but no comparison against upstream `send_file_entry()`/`recv_file_entry()`.
- **Divergence risk**: High. Any byte-level difference in file-list encoding causes interop failure. The preserve-flag matrix creates a combinatorial explosion that unit tests cannot cover.
- **Approach**: Generate structured `FileEntry` values, encode with oc-rsync, then compare the raw bytes against what upstream rsync 3.4.1 would produce for the same entry. Requires a small C shim around upstream's `send_file_entry()` compiled as a shared library, or a byte-capture harness that records upstream's wire output.
- **Complexity**: Medium-high. Requires building upstream rsync's file-list encoder as a callable function.

**WDF-3: Multiplex frame differential fuzzing**

- **Gap**: Multiplex framing round-trip exists (#2, #27) but no comparison against upstream. `MSG_DATA`, `MSG_ERROR`, `MSG_INFO` frames are the transport envelope for every wire interaction.
- **Divergence risk**: Medium-high. Header byte layout is simple (4 bytes LE, tag + length) but the `MPLEX_BASE` constant, payload-length ceiling, and unknown-code handling must match upstream exactly.
- **Approach**: Encode a `MessageFrame` with oc-rsync, decode with upstream's `read_buf()`/`mplex_read()` (or vice versa), assert payload equality.
- **Complexity**: Medium. Upstream's multiplex code is self-contained in `io.c`.

### P1 - High (WDF-4)

**WDF-4: Varint/varlong differential fuzzing**

- **Gap**: Round-trip coverage is excellent (#9, #22, #23) but never compared against upstream. Varints appear in every wire message, so a silent encoding divergence would propagate everywhere.
- **Divergence risk**: Medium. The encoding is well-defined but the boundary between 1-byte, 2-byte, and 5-byte forms differs between legacy and modern protocols.
- **Approach**: Encode a value with oc-rsync's `write_varint`/`write_varlong`, decode with upstream's `read_varint()`/`read_varlong()` (compiled as a C shim), assert equality. Reverse direction too.
- **Complexity**: Low-medium. Upstream varint code is ~50 lines in `io.c`.

### P2 - Medium (WDF-5)

**WDF-5: Delta token stream differential fuzzing**

- **Gap**: Decode-only (#24) with no round-trip and no upstream comparison. Delta tokens (literal data + copy commands + end-of-file marker) are the core of file reconstruction. A divergence here causes silent data corruption.
- **Divergence risk**: High for correctness, medium for likelihood (the format is simpler than file lists).
- **Approach**: Given a basis file and a modified file, generate signatures and delta tokens with both oc-rsync and upstream, compare the reconstructed output byte-for-byte. This is an end-to-end differential test of the delta pipeline rather than a parser fuzz target.
- **Complexity**: High. Requires orchestrating both implementations through the full signature-delta-reconstruct pipeline.

### P3 - Lower (WDF-6)

**WDF-6: Compressed token stream differential fuzzing**

- **Gap**: Raw decompressor fuzz exists (#16, #17) but the rsync-specific compressed-token framing (`compressed_token/{encoder,decoder}`) has no fuzz target at all. The compressed-token format wraps deflate/zstd inside rsync's own framing with block boundaries and flush semantics that differ from raw deflate.
- **Divergence risk**: Medium. Compression bugs typically manifest as transfer failures (checksum mismatch) rather than silent corruption.
- **Approach**: Compress a token stream with oc-rsync's `CompressedTokenEncoder`, decompress with upstream's `recv_deflated_token()`, compare output.
- **Complexity**: Medium-high. Upstream's compressed token handling in `token.c` is entangled with the I/O layer.

### 3.1 Additional Gaps (Not Prioritized for WDF-2..6)

These gaps are real but lower priority because they are either narrow attack surfaces or already covered by interop tests:

| Gap | Why Deferred |
|-----|-------------|
| NDX codec round-trip + differential | Narrow codec; interop tests cover the integration path |
| ACL/xattr encode round-trip | Encode paths exist but are harder to exercise without platform ACL support |
| Filter list wire encode | Narrow surface; filter evaluation differential covers the semantic layer |
| Checksum negotiation | Stateful multi-step exchange; better covered by integration tests |
| Server args exchange | Deterministic string formatting; low divergence risk |
| Batch file round-trip | Administrative surface; not network-facing |
| ID list wire format | Covered by interop tests implicitly |
| Iconv | Narrow; charset conversion relies on system iconv |
| Full session differential | Aspirational; requires process-level orchestration |

## 4. Infrastructure Requirements for Differential Fuzzing Harness

### 4.1 Current State

The project already has two differential fuzz targets (`filter_differential`, `filter_rules_vs_upstream`) that establish the pattern:

1. Build oc-rsync's in-process result.
2. Shell out to an upstream `rsync` binary.
3. Compare verdicts.
4. Panic on divergence (libFuzzer records the crash artifact).

This pattern works for behavioral comparisons (filter verdicts) but cannot compare wire-level byte encodings because upstream rsync does not expose its encoder/decoder as a library API.

### 4.2 Required Infrastructure

**Option A: C shim library (recommended for WDF-2..4)**

Build a minimal shared library from upstream rsync 3.4.1 source that exposes:

- `fuzz_write_varint(int32_t val, uint8_t *buf, size_t *len)` - wraps `write_varint()`
- `fuzz_read_varint(const uint8_t *buf, size_t len, int32_t *out)` - wraps `read_varint()`
- `fuzz_write_varlong(int64_t val, int min_bytes, uint8_t *buf, size_t *len)` - wraps `write_varlong()`
- `fuzz_send_file_entry(/* structured entry */, uint8_t *buf, size_t *len)` - wraps `send_file_entry()`
- `fuzz_encode_mplex_header(int code, size_t len, uint8_t *buf)` - wraps the 4-byte header encoder

The shim is compiled from `target/interop/upstream-src/rsync-3.4.1/` and linked into the fuzz binary via `cc` build script. The Rust fuzz target calls both oc-rsync's encoder and the C shim, then asserts byte equality.

**Advantages**: In-process comparison at full fuzzer throughput (100K+ exec/sec). No child process spawn overhead.

**Disadvantages**: Requires maintaining a C build alongside the Rust fuzzer. Upstream rsync's functions have internal state (static variables in `io.c`) that must be reset between calls.

**Option B: Process-based wire capture (recommended for WDF-5..6)**

For end-to-end delta and compression comparisons:

1. Run upstream rsync daemon in a subprocess.
2. Capture wire bytes via a transparent proxy or `tshark`.
3. Run oc-rsync against the same daemon with the same source tree.
4. Compare captured wire bytes (modulo known non-determinism: checksum seed, timestamps).

The existing `wire_equivalence_tcpdump.rs` integration test follows this pattern but is not a libFuzzer target. Adapting it for fuzzing requires:

- A deterministic mode that fixes the checksum seed and timestamp.
- A lightweight proxy that captures and replays wire bytes.
- A corpus of source-tree configurations that drive different protocol paths.

**Advantages**: Tests the complete protocol stack. Catches interaction bugs.

**Disadvantages**: Low throughput (~10-50 exec/sec). Non-determinism from OS timestamps, TCP buffering. Requires Linux with `tshark`.

**Option C: Golden-byte snapshot testing (complement to A and B)**

Expand the existing golden-byte test suite (`crates/protocol/tests/golden_*.rs`) with tcpdump-captured wire bytes from upstream rsync sessions. Each golden test asserts that oc-rsync produces identical bytes for a known input.

This is not fuzzing per se, but it provides deterministic regression coverage for specific wire interactions that fuzzing cannot easily capture (multi-step handshakes, session-level state).

### 4.3 Build System Requirements

| Requirement | Status |
|-------------|--------|
| Upstream rsync 3.4.1 source available | Available at `target/interop/upstream-src/rsync-3.4.1/` or fetched by `tools/ci/run_interop.sh` |
| C compiler for shim library | Available on all CI platforms (`cc` crate) |
| Nightly Rust for `cargo-fuzz` | Already required by existing fuzz targets |
| `libfuzzer-sys` dependency | Already in `fuzz/Cargo.toml` |
| `arbitrary` derive for structured inputs | Already in `fuzz/Cargo.toml` |
| Upstream rsync binary for process-based tests | Discovered by existing `upstream_binary()` helper |
| `tshark`/`tcpdump` for wire capture | Linux-only; already gated in `wire_equivalence_tcpdump.rs` |

### 4.4 Recommended Implementation Order

1. **WDF-2 (file-list entry)**: Build C shim for `send_file_entry()`. Highest divergence risk, highest value.
2. **WDF-4 (varint/varlong)**: Add varint/varlong functions to the same C shim. Low incremental cost.
3. **WDF-3 (multiplex frame)**: Add multiplex header encoding to the C shim. Low incremental cost.
4. **WDF-5 (delta tokens)**: Process-based differential using upstream rsync binary. Higher complexity.
5. **WDF-6 (compressed tokens)**: Extend WDF-5 with compression enabled. Highest complexity.

### 4.5 Existing Assets to Leverage

| Asset | Location | Relevance |
|-------|----------|-----------|
| Upstream C source | `target/interop/upstream-src/rsync-3.4.1/` | Build shim from `io.c`, `flist.c`, `token.c` |
| `upstream_binary()` helper | `fuzz/fuzz_targets/filter_differential.rs` | Reuse for process-based targets |
| Wire-equivalence tcpdump test | `crates/protocol/tests/wire_equivalence_tcpdump.rs` | Adapt capture harness |
| Golden-byte tests | `crates/protocol/tests/golden_*.rs` | Expand with upstream captures |
| Interop test harness | `tools/ci/run_interop.sh` | Provides built upstream binaries |
| File-entry round-trip fuzz | `crates/protocol/fuzz/fuzz_targets/file_entry_roundtrip.rs` | Extend with C shim comparison |
