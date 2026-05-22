# Fuzz corpus inventory (FCV-17)

Tracking issue: FCV-17 (#2663). Companion to FCV-18
(`docs/audits/fuzz-corpus-gaps.md`).

Scope: the top-level `fuzz/` cargo-fuzz workspace. Per-crate fuzz
workspaces under `crates/protocol/fuzz/` and `crates/filters/fuzz/` are
catalogued in `docs/audits/fuzz-coverage-matrix.md` and are out of scope
for this inventory.

This document enumerates every fuzz target shipped in `fuzz/`, points at
the `fuzz_target!` macro that defines it, locates the seed corpus on
disk, reports the seed count and total seed byte budget, and names the
parser the target drives. The inventory is the input to FCV-18 (gap
analysis) and FCV-19 (new seed generation).

## Workspace summary

- Targets defined in `fuzz/Cargo.toml`: **21**
- Targets with a populated `fuzz/corpus/<name>/` directory: **13**
- Targets with no seed corpus on disk: **8**
- Total seed files across all corpora: **17**
- Total seed bytes across all corpora: **296**

The corpus is therefore extremely shallow: 8 of 21 targets ship with no
seeds at all, and the median populated target carries a single seed.
libFuzzer can still discover coverage from scratch on an empty corpus,
but seeded targets converge orders of magnitude faster on the boundary
conditions that matter to a protocol parser.

## Inventory table

Columns:

- **Target**: cargo-fuzz binary name (matches `[[bin]]` in `fuzz/Cargo.toml`).
- **Source**: `fuzz_target!` macro location.
- **Corpus**: seed directory and `seed_count / total_bytes` snapshot.
- **Parser under test**: public entry point(s) the harness drives, with
  upstream rsync 3.4.1 citation where the parser mirrors C source.
- **Failure mode**: how the parser is expected to surface malformed input.

| Target | Source | Corpus | Parser under test | Failure mode |
|--------|--------|--------|-------------------|--------------|
| `acl_xattr_wire` | `fuzz/fuzz_targets/acl_xattr_wire.rs:44` | `fuzz/corpus/acl_xattr_wire/` (1 / 1 B) | `protocol::acl::read_acl_definition` (upstream `acls.c:recv_rsync_acl()`), `protocol::xattr::read_xattr_definitions` + `recv_xattr` + `recv_xattr_request` + `recv_xattr_values` (upstream `xattrs.c:receive_xattr()` / `recv_xattr*()`) | `io::Result` from every entry point; panic = crash artifact |
| `auth_response` | `fuzz/fuzz_targets/auth_response.rs:30` | `fuzz/corpus/auth_response/` (1 / 33 B) | `daemon::auth::verify_client_response` (length-disambiguated MD4/MD5/SHA-1/SHA-256/SHA-512), `daemon::auth::SecretsFile::parse` (admin secrets file) | `Result`-returning verifier and parser; daemon never panics pre-auth |
| `batch_reader` | `fuzz/fuzz_targets/batch_reader.rs:28` | `fuzz/corpus/batch_reader/` (2 / 26 B) | `batch::BatchReader::new` + `read_header` + `read_data` (upstream `batch.c`) | `io::Result` on every read; panic = admin-DoS finding |
| `bwlimit` | `fuzz/fuzz_targets/bwlimit.rs:34` | `fuzz/corpus/bwlimit/` (4 / 11 B) | `bandwidth::parse_bandwidth_argument` + `bandwidth::parse_bandwidth_limit` (upstream `util2.c:parse_size_arg()`) | `Result<_, _>` from both parsers |
| `capability_flags` | `fuzz/fuzz_targets/capability_flags.rs:43` | `fuzz/corpus/capability_flags/` (1 / 8 B) | `protocol::CompatibilityFlags::{read_from, decode_from_slice, decode_from_slice_mut, from_bits, encode_to_vec}`, `protocol::KnownCompatibilityFlag::from_str`, `protocol::detect_negotiation_prologue`, `protocol::NegotiationPrologue::from_str` | `io::Result`/`Result`/`Option`; round-trip arm asserts encoder/decoder symmetry |
| `daemon_greeting` | `fuzz/fuzz_targets/daemon_greeting.rs:32` | `fuzz/corpus/daemon_greeting/` (1 / 14 B) | `protocol::parse_legacy_daemon_greeting_bytes` + `..._details` + `..._owned` | `Result<_, _>`; pre-auth surface, panic = remote DoS |
| `decompressor_zlib` | `fuzz/fuzz_targets/decompressor_zlib.rs:37` | (no corpus directory) | `compress::zlib::CountingZlibDecoder` (streaming raw deflate, upstream `deflateInit2(..., -MAX_WBITS, ...)`), `compress::zlib::decompress_to_vec` (one-shot) | `io::Result` from decoder; 100x expansion ratio asserted; panic = crash |
| `decompressor_zstd` | `fuzz/fuzz_targets/decompressor_zstd.rs:36` | (no corpus directory) | `compress::zstd::CountingZstdDecoder` (streaming), `compress::zstd::decompress_to_vec` (one-shot) | `io::Result` from decoder; 100x expansion ratio asserted; panic = crash |
| `filter_differential` | `fuzz/fuzz_targets/filter_differential.rs:526` | (no corpus directory) | `filters::FilterSet` decisions vs upstream `rsync --dry-run --recursive --verbose --out-format=I:%n` child process | differential panic on verdict divergence with upstream; otherwise `Result` |
| `filter_list_wire` | `fuzz/fuzz_targets/filter_list_wire.rs:26` | `fuzz/corpus/filter_list_wire/` (1 / 4 B) | `protocol::filters::wire::read_filter_list` (upstream `exclude.c:recv_filter_list()`), exercised at protocol versions 28-32 | `io::Result` per version |
| `filter_rules_vs_upstream` | `fuzz/fuzz_targets/filter_rules_vs_upstream.rs:344` | (no corpus directory) | `filters::FilterSet` decisions vs upstream `rsync --list-only` child process; includes `!` clear directive | differential panic on verdict divergence; otherwise `Result` |
| `flist_entry_decode` | `fuzz/fuzz_targets/flist_entry_decode.rs:67` | (no corpus directory) | `protocol::flist::FileListReader::read_entry` + `protocol::flist::read_file_entry` (upstream `flist.c:recv_file_entry()`), under legacy (V28) and INC_RECURSE (V30 + `CF_INC_RECURSE`) modes with randomised preserve-flag matrix | `io::Result` from streaming reader; panic = post-auth crash |
| `incremental_flist` | `fuzz/fuzz_targets/incremental_flist.rs:65` | `fuzz/corpus/incremental_flist/` (1 / 1 B) | `protocol::flist::StreamingFileList::next_ready` and `IncrementalFileList::finalize`, exercised under legacy and INC_RECURSE modes | `io::Result`; finalize must always succeed even on empty state |
| `legacy_greeting` | `fuzz/fuzz_targets/legacy_greeting.rs:38` | `fuzz/corpus/legacy_greeting/` (1 / 14 B) | `protocol::parse_legacy_daemon_greeting{,_bytes,_bytes_details,_bytes_owned,_details,_owned}` (six entry points) | `Result<_, _>` per entry point; pre-auth surface |
| `multiplex_frame_parse` | `fuzz/fuzz_targets/multiplex_frame_parse.rs:39` | (no corpus directory) | `protocol::MessageHeader::decode` + `from_raw`, `protocol::BorrowedMessageFrames` walker; structured `(raw u32, trailing Vec<u8>)` arm round-trips both decode entry points | `io::Result`; structured arm panics on decode disagreement |
| `ndx_codec` | `fuzz/fuzz_targets/ndx_codec.rs:28` | `fuzz/corpus/ndx_codec/` (1 / 4 B) | `protocol::codec::NdxCodec::read_ndx` via `create_ndx_codec(version)` for versions 28-32 (upstream `io.c:read_ndx()`) | `io::Result`; truncation expected at EOF |
| `protocol_wire` | `fuzz/fuzz_targets/protocol_wire.rs:19` | (no corpus directory) | `protocol::BorrowedMessageFrames` iterator (frame walker) | `io::Result` per frame; panic = pre-auth remote DoS |
| `rsyncd_conf` | `fuzz/fuzz_targets/rsyncd_conf.rs:27` | `fuzz/corpus/rsyncd_conf/` (1 / 132 B) | `daemon::rsyncd_config::RsyncdConfig::parse` (line-oriented `[module]` parser) | `Result<_, _>`; admin-driven, panic = startup DoS on reload |
| `simd_checksum_parity` | `fuzz/fuzz_targets/simd_checksum_parity.rs:39` | (no corpus directory) | `checksums::RollingChecksum::update`, `checksums::strong::md5_digest_batch`, `checksums::strong::md4_digest_batch` against per-byte scalar reference and RustCrypto MD4/MD5 | differential panic on SIMD vs scalar byte mismatch |
| `varint_decode` | `fuzz/fuzz_targets/varint_decode.rs:51` | `fuzz/corpus/varint_decode/` (1 / 33 B) | `protocol::{decode_varint, read_varint, read_varlong, read_longint, read_int, read_varint30_int}` plus matching encoders for round-trip assertions | `io::Result` per decoder; round-trip arm panics on encoder/decoder asymmetry |
| `vstring` | `fuzz/fuzz_targets/vstring.rs:40` | `fuzz/corpus/vstring/` (1 / 5 B) | `protocol::negotiate_capabilities` (drives `read_vstring` via the public entry point) under protocol versions 28-32 with role and compression toggles | `io::Result`; pre-auth path |

## Corpus seed catalogue

For each populated corpus, the seed file(s), byte size, and raw hex
content. This is the source-of-truth snapshot FCV-18 uses to decide
which corpora are under-seeded.

| Target | Seed file | Bytes | Hex | Notes |
|--------|-----------|-------|-----|-------|
| `acl_xattr_wire` | `seed_basic` | 1 | `00` | Single zero byte. Misses every ACL/xattr varint path. |
| `auth_response` | `seed_basic` | 33 | `616c69636520...5a67 0a` | `alice dGVzdHBlcnNvbnNlcGFkZGluZw\n` style secrets line. |
| `batch_reader` | `seed_basic` | 13 | `00000000 1f000000 0000000000` | Zero-magic header skeleton. |
| `batch_reader` | `seed_v31` | 13 | `3030303030...300a` | ASCII placeholder, not a real batch header. |
| `bwlimit` | `seed_0` | 1 | `30` | `"0"`. |
| `bwlimit` | `seed_100k` | 4 | `3130306b` | `"100k"`. |
| `bwlimit` | `seed_1_5m` | 4 | `312e356d` | `"1.5m"`. |
| `bwlimit` | `seed_1g` | 2 | `3167` | `"1g"`. |
| `capability_flags` | `seed_basic` | 8 | `4c73667843497675` | `"LsfxCIvu"` capability identifier string. |
| `daemon_greeting` | `seed_basic` | 14 | `4052...2033322e30 0a` | `"@RSYNCD: 32.0\n"`. |
| `filter_list_wire` | `seed_basic` | 4 | `00000000` | Empty-list terminator only. |
| `incremental_flist` | `seed_basic` | 1 | `00` | Single zero byte. |
| `legacy_greeting` | `seed_v32` | 14 | `4052...2033322e30 0a` | `"@RSYNCD: 32.0\n"`. |
| `ndx_codec` | `seed_basic` | 4 | `01000000` | Single positive 4-byte NDX. |
| `rsyncd_conf` | `seed_basic` | 132 | minimal `port`/`log file`/`[public]` config | One valid 6-line config. |
| `varint_decode` | `seed_basic` | 33 | `42 000102...1f` | One non-zero leading byte plus 32 byte ramp. |
| `vstring` | `seed_basic` | 5 | `06 036d6435` | One-byte selector then a 3-byte `"md5"` vstring. |

## Targets with no on-disk seeds

These directories do not exist under `fuzz/corpus/`. libFuzzer will
create them on first run, but coverage convergence is materially slower.
FCV-18 prioritises these eight for seed generation in FCV-19.

| Target | Why a seed corpus is high-priority |
|--------|------------------------------------|
| `decompressor_zlib` | Raw deflate has dense decode tables; structured seeds (empty frame, single literal, fixed-Huffman, dynamic-Huffman, BFINAL=1 boundaries) help libFuzzer reach the decode-state-machine branches. |
| `decompressor_zstd` | Zstd magic + frame header parsing is the first gate. Without seeds, libFuzzer spends most exec budget rejecting non-magic inputs. |
| `filter_differential` | Differential target depends on an external `rsync` binary. Seeds are not strictly required for the differential property, but seed rules covering anchored/unanchored, dir-only, perishable, and modifier interactions accelerate divergence-finding. |
| `filter_rules_vs_upstream` | Same as `filter_differential`. The `!` clear-directive arm benefits from seeds that include `!` in different positions. |
| `flist_entry_decode` | The decoder branches on XMIT flag bits in the first byte. A minimal valid entry per protocol version (V28, V30) bootstraps both arms cheaply. |
| `multiplex_frame_parse` | A handful of canonical 4-byte headers (`MSG_DATA`, `MSG_ERROR_XFER`, `MSG_INFO`, `MSG_DONE`, `MPLEX_BASE` boundaries) immediately reach the validation matrix. |
| `protocol_wire` | Same as `multiplex_frame_parse` plus multi-frame stream samples. |
| `simd_checksum_parity` | Parity assertions trigger on byte-level divergence. Seeds at SIMD lane boundaries (4, 8, 16, 32, 64 bytes) and odd remainders (5, 7, 17, 33) reach lane-tail handling fastest. |

## Methodology

Counts were generated from a clean checkout by walking
`fuzz/corpus/<target>/`, counting regular files and summing byte sizes.
The `fuzz_target!` macro line numbers were read directly out of each
`fuzz/fuzz_targets/<name>.rs` source. Parser entry points were
identified by following the `use` statements at the top of each fuzz
target file and the function calls inside the macro body.

This document is a point-in-time snapshot. Re-run the inventory
whenever a target is added (FCV-3 follow-up tasks may add more) or when
FCV-19 lands new seeds.
