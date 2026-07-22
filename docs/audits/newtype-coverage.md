# Newtype Coverage Audit

Tracking issue: oc-rsync task #2137. Branch: `docs/newtype-audit-2137`.
Related history: PR #1767 introduced `FileNdx`.

## Overview & decision question

This audit catalogues which of oc-rsync's protocol- and engine-level
identifiers, sizes, offsets, and digests are wrapped in dedicated newtypes
and which still flow through the codebase as raw integers or byte slices.
The decision question: where does primitive obsession still risk silent
mixing of unrelated quantities (file index vs. block index vs. byte
offset vs. file length, weak rolling sum vs. strong digest, seed vs.
flag word), and which of those sites are worth converting to newtypes
given current call-graph reach and migration cost?

The audit is scoped to the protocol/engine surface that touches wire
data and delta state (`crates/protocol`, `crates/engine`,
`crates/match`, `crates/checksums`, `crates/signature`, `crates/batch`).
Per-platform crates (`metadata`, `fast_io`) are out of scope; their
integers map to OS types (`uid_t`, `gid_t`, file descriptors) where
adding a Rust newtype carries little benefit over the existing OS type
aliases.

## Current newtype inventory

The grep `FileNdx|FileIndex|ProtocolVersion|BlockChecksum|FileChecksum`
across `crates/*/src/` is the seed; the broader sweep below adds every
newtype found while walking the relevant struct definitions.

| Newtype | Wrapped type | Defined in | Purpose | Coverage |
|---------|--------------|------------|---------|----------|
| `ProtocolVersion` | `NonZeroU8` | `crates/protocol/src/version/protocol_version/mod.rs:68` | Negotiated wire protocol number with `try_from(u8)` validation, `as_u8()`, `NEWEST`, `supported_range_bounds()`. | Universal: codec dispatch, daemon greeting, batch header, CLI parser, client config builder, transfer orchestration all consume `ProtocolVersion` rather than `u8`. |
| `FileNdx` | `u32` | `crates/engine/src/concurrent_delta/types.rs:25` | NDX values inside the concurrent delta pipeline. `Copy + Hash + Ord`, `repr(transparent)`, `From<u32>`, `Display`. | **Only the concurrent-delta pipeline.** All other "file index" call sites still use raw `i32`/`u32`. See gap analysis below. |
| `RollingDigest` | `{ s1: u16, s2: u16, len: usize }` | `crates/checksums/src/rolling/digest.rs:17` | Packed Adler32-style rolling sum with explicit window length. Serialises via `SIVALu`-equivalent path. | Universal: signature blocks, match indexes, scalar/SIMD parity tests all use `RollingDigest`. |
| `DigestBuf` | `[u8; MAX_DIGEST_LEN]` + `usize` | `crates/signature/src/algorithm.rs:16` | Stack-allocated strong-checksum bytes with length, replacing `Vec<u8>` on the hot path. | Used by `SignatureBlock::strong_digest`; receiver-side delta search still works in `Vec<u8>` (see `BlockEntry::strong_checksum` below). |
| `SignatureBlock` | composite | `crates/signature/src/block.rs:9` | Single block descriptor: `index: u64`, `rolling: RollingDigest`, `strong: DigestBuf`. | Universal in the sender's signature pipeline. |
| `SignatureLayout` | composite | `crates/signature/src/layout.rs:75` | `block_length: NonZeroU32`, `remainder: u32`, `block_count: u64`, `strong_sum_length: NonZeroU8`. | Universal in `signature::generate_*`. |
| `SignatureLayoutParams` | composite | `crates/signature/src/layout.rs:21` | Builder-side layout inputs (`file_length`, optional `forced_block_length: NonZeroU32`, `protocol`, `checksum_length: NonZeroU8`). | Single entry point to `calculate_signature_layout`. |
| `BlockChecksums<D>` | composite | `crates/checksums/src/pipelined/checksums.rs:20` | Per-block rolling+strong pair emitted from the pipelined generator. | Pipelined sender path only. |
| `DeltaWork` / `DeltaResult` | composite | `crates/engine/src/concurrent_delta/types.rs:65,310` | Carries `FileNdx`, paths, sizes across the worker pool. | Concurrent delta pipeline only. |
| `HardlinkEntry` | `{ first_ndx: u32 }` | `crates/protocol/src/flist/hardlink/types.rs:31` | Tracks the first NDX seen for a given dev/ino pair. | Wraps `u32`, but its public field is a raw `u32`. |
| `DevIno` (from `metadata`) | composite | `crates/metadata/src/...` | Device + inode pair. | Universal hardlink key. |

Tooling-related newtypes outside the wire path (`ExitCode`, `Verbosity`,
`DebugLevels`, etc.) exist and are well-covered; they are not part of
this audit.

## Gap 1 - `FileNdx` is local to the concurrent-delta pipeline

PR #1767 added `FileNdx` so the worker queue could not silently mix
file indices with the pipeline's `sequence: u64` reorder key. That goal
is met inside `crates/engine/src/concurrent_delta`. Outside that
module, the same conceptual value still flows as raw integers:

| Site | Current type | Notes |
|------|--------------|-------|
| `crates/engine/src/hardlink.rs:193,224,241,260,314` (`HardlinkTracker::register`, `resolve`, `is_hardlink_source`, `get_hardlink_target`, free `resolve` helper) | `i32` | Mirrors upstream's `int ndx`; values are sentinel-encoded (negative = "no link"), so a direct swap to `FileNdx` (`u32`) would also need to model the sentinel. |
| `crates/protocol/src/flist/hardlink/types.rs:31,39` (`HardlinkEntry::first_ndx`) | `u32` | Drop-in candidate for `FileNdx`. |
| `crates/protocol/src/flist/hardlink/table.rs:72,79,80` (`HardlinkTable::find_or_insert`, `HardlinkLookup::First(u32)`) | `u32` | Drop-in candidate. |
| `crates/protocol/src/flist/sort.rs:36` (`SortKey { index: u32, ... }`) | `u32` | NDX of the entry in the unsorted list. Drop-in candidate. |
| Wire NDX I/O (`io.c:read_ndx`/`write_ndx` analogues in `crates/protocol/src/wire/`) | varies | Read/write helpers currently return `i32`/`u32` directly. Wrapping at the codec boundary would propagate `FileNdx` through receiver and generator without a type-shift in between. |

The interesting friction point is `hardlink.rs`: upstream uses
`int file_index` and reserves negative values as sentinels
(`F_HLINK_NOT_FIRST`, etc.). A clean `FileNdx` migration would either
(a) switch to `Option<FileNdx>` + a small `enum HardlinkAction` (already
exists - `HardlinkAction::Transfer`/`LinkTo`), or (b) introduce a
separate `HardlinkRef(i32)` newtype that preserves the sentinel
semantics for wire compatibility and converts to `FileNdx` only when
non-negative.

## Gap 2 - `BlockEntry` carries four primitives

`crates/match/src/optimized_search.rs:47-56` defines

```rust
pub struct BlockEntry {
    pub index: u32,            // block index in the original file
    pub checksum: u32,         // weak rolling checksum (sum1 + sum2)
    pub strong_checksum: Vec<u8>,  // MD4/MD5 digest
    pub block_len: u32,        // block length in bytes
}
```

Every field is primitive obsession-prone:

- `index: u32` and `block_len: u32` have the same Rust type but very
  different units. `find_block_at_offset(file_offset / block_len)`-style
  arithmetic could be miscomputed.
- `checksum: u32` is the packed `(s2 << 16) | s1` form; the rest of the
  codebase uses `RollingDigest` for the same value. Two representations
  of the same concept invite drift (e.g., when SIMD code hashes blocks
  differently).
- `strong_checksum: Vec<u8>` is a heap allocation per block, while
  `SignatureBlock::strong` already uses the stack-allocated `DigestBuf`.
  The match crate is the primary delta hot path, and `Vec<u8>` here
  forces an allocation per signature block ingested - `DigestBuf` would
  remove it.

A `BlockEntry { index: BlockIndex, len: BlockSize, weak: RollingDigest, strong: DigestBuf }` rewrite is the highest-leverage single change in the audit.

## Gap 3 - block size and offset primitives

Block-related quantities flow as plain integers in many places:

| Site | Type used | Conceptual unit |
|------|-----------|-----------------|
| `crates/signature/src/block_size.rs` (`calculate_block_length`, `calculate_checksum_count`) | `u32`, `u64` | Block length (bytes), block count (blocks). |
| `crates/protocol/src/wire/signature.rs:107` (`block_length: u32`) | `u32` | Block size in bytes. |
| `crates/batch/src/replay.rs:198,201,217,815,816,828` | `u32`/`usize` | Block index and `block_index * block_length` offset arithmetic. |
| `crates/engine/src/local_copy/debug_*` trace helpers (`trace_delta_apply_match`, `trace_match_hit`, `trace_match_false_alarm`, `record_match`, `record_literal`, etc.) | `usize`, `u64`, `u32` | `block_index`, file `offset`, span `length`, weak checksum. |
| `crates/match/src/script.rs:15-25` (`DeltaToken::Copy { index: u64, len: usize }`) | `u64`/`usize` | Block index + byte length. |
| `crates/protocol/src/wire/delta/token.rs:74` (`write_token_block_match(writer, block_index: u32)`) | `u32` | Block index over the wire. |
| `crates/protocol/src/wire/compressed_token/lz4_codec.rs:109,290,339,351` (`CompressedToken::BlockMatch(u32)`) | `u32` | Block index over the wire. |

The only place that already encapsulates block sizing is
`SignatureLayout` (and even there the public accessors return raw
`NonZeroU32`/`u32`/`u64`).

## Gap 4 - checksum seed is a bare `i32`

`checksum_seed` flows through the entire transfer:

- `crates/batch/src/format/header.rs:31,38,72,103` - serialised as `i32`
  on the wire (upstream `io.c:2449 write_int(batch_fd, checksum_seed)`).
- `crates/batch/src/lib.rs:380,429,468,472` - public field `pub checksum_seed: i32`, builder method `with_checksum_seed(seed: i32)`.
- `crates/batch/src/reader/mod.rs:114` - `self.config.checksum_seed = header.checksum_seed`.
- `crates/checksums/src/strong/md4.rs:138-265` - `digest_with_seed(seed: i32, ...)`.

The seed is structurally identical to a 32-bit field word but
semantically a salt fed through `SIVAL(buf1, len, checksum_seed)`. A
`ChecksumSeed(i32)` wrapper is cheap and prevents accidents like
swapping seed and `compat_flags` (also `i32`) in the batch header
constructor `BatchHeader::new(protocol_version: i32, checksum_seed: i32)`.

## Gap 5 - `i32`/`u64` file-size signedness drift

`crates/protocol/src/codec/protocol/{legacy,modern,mod,dispatch}.rs`
exposes `write_file_size(&self, writer, size: i64)` and the rest of the
code path is `u64` on `FileSignature::total_bytes`, `SignatureLayout::file_size`. The signedness flip happens at the codec boundary and is
fine, but a `FileSize(i64)` (with a `try_from(u64)` constructor that
clamps at `i64::MAX`, mirroring `SignatureLayoutError::FileTooLarge`)
would let us drop the manual overflow check at `layout.rs:176` and
several `as i64`/`as u64` casts further out.

## Recommended additional newtypes

In priority order, with the rough migration cost in primitive replacement
sites (counted by the grep evidence above; not literal LoC):

| Newtype | Wraps | Why | Cost (sites to touch) | Risk |
|---------|-------|-----|------------------------|------|
| `BlockIndex(u32)` | `u32` | Disambiguates block index from block length and from `FileNdx`; flows through `BlockEntry`, `DeltaToken::Copy`, wire `BlockMatch`, batch replay arithmetic, all `debug_*` tracers. | ~40 sites across `match`, `engine`, `batch`, `protocol`. | Low. The wire format is already `u32`; we only change Rust types. |
| `BlockSize(NonZeroU32)` | `NonZeroU32` | Removes the `index * block_size` ambiguity in batch replay and trace code; `NonZeroU32` matches `SignatureLayout::block_length`. | ~25 sites; mostly `signature::block_size` + `batch::replay` + a few `match` accessors. | Low. `SignatureLayout` already uses `NonZeroU32`. |
| `BlockOffset(u64)` | `u64` | Distinguishes file byte offset (`u64`) from block index (`u32`) on the receiver-side delta path. Currently both flow as ints to the same trace helpers. | ~20 sites in `engine::local_copy::debug_*` and `batch::replay`. | Low. |
| `ChecksumSeed(i32)` | `i32` | Stops accidental swap with `compat_flags`/`protocol_version` in batch header builder; documents wire layout. | ~12 sites in `batch` + `checksums::strong::md4`. | Trivial. |
| `BlockEntry` rewrite (use `BlockIndex` + `BlockSize` + `RollingDigest` + `DigestBuf`) | n/a | Single biggest semantic + perf win (drops `Vec<u8>` allocation per block). | One struct definition, ~10 callers in `match`. | Low protocol-wise; medium because the receiver hot path is sensitive to allocator behaviour and SIMD layout. Bench before/after. |
| `FileNdx` extension across `protocol::flist::hardlink`, `protocol::flist::sort`, wire NDX codec | `u32`/`i32` (sentinel) | Completes PR #1767 outside the concurrent-delta crate. The wire-codec boundary is the natural type-shift point. | ~15 sites; the `hardlink.rs` `i32` sentinel sites need either `Option<FileNdx>` or a sibling `HardlinkRef(i32)`. | Low for the `u32` sites; medium for hardlink sentinels (must preserve upstream-equivalent encoding). |
| `FileSize(i64)` | `i64` | Centralises the `u64`-to-`i64` signedness flip and the `i64::MAX` clamp. | ~10 sites at the wire boundary. | Low. |
| `BlockCount(u64)` (with `i32::MAX` invariant) | `u64` | Encodes the existing `BlockCountOverflow` invariant into the type. | ~6 sites. | Trivial. |

## Migration cost - rough order of magnitude

| Tier | Newtypes | Estimated touch (Rust files) | Estimated diff lines | Risk |
|------|----------|------------------------------|----------------------|------|
| Tier 1 (cheap, defensive) | `ChecksumSeed`, `BlockCount`, `FileSize` | 6-8 files | 80-120 net additions | Trivial; pure type-tightening at the boundary. |
| Tier 2 (highest leverage) | `BlockIndex`, `BlockSize`, `BlockOffset`, `BlockEntry` rewrite | 10-15 files in `match`, `engine`, `batch`, `protocol::wire` | 300-450 net additions | Low. Bench `match` and `batch::replay` afterwards. |
| Tier 3 (completes PR #1767) | `FileNdx` propagation, hardlink wrapper(s) | 8-12 files in `protocol::flist`, `engine::hardlink` | 150-220 net additions | Medium for hardlink sentinels. |

Total estimated effort: roughly 25-35 source files, 500-800 net diff
lines, and one round of micro-benchmarks across the delta pipeline. The
work splits cleanly into three follow-up PRs by tier.

## Out-of-scope items

- `metadata` crate `Uid`/`Gid` newtypes - already thin wrappers over OS
  types; not part of the wire path.
- `BatchHeader::protocol_version: i32` (vs. `ProtocolVersion`). Upstream
  serialises this as a signed 32-bit "magic + version" combo
  (`io.c:2438-2455`); converting requires a bespoke
  `BatchProtocolField` rather than reusing `ProtocolVersion`.
- `RollingDigest`'s `len: usize` - the only consumer is the digest
  itself; wrapping is not warranted.
- ACL `access_ndx: u32` (`crates/metadata/src/acl_*.rs`) - this is an
  ACL-internal index, unrelated to the file-list NDX above.

## Action items

1. Open separate tracking issues for Tier 1, Tier 2, Tier 3.
2. Tier 2 must include a `match`-crate microbench before/after, since
   `BlockEntry` is on the receiver hot path.
3. Tier 3 must propose explicit semantics for hardlink NDX sentinels
   before code change (option type vs. sibling newtype).
4. Once `BlockIndex`/`BlockSize` land, the `debug_*` trace helpers
   should switch to those types so `--debug=DELTASUM` output is
   self-documenting at the call site.
