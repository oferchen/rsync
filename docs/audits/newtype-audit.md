# Newtype Audit: Protocol / Checksum / Index Primitives

Tracks issue #2137. Surveys raw primitive types (`u32`, `i32`, `Vec<u8>`,
`[u8; N]`) circulating across crate boundaries that could be replaced with
purpose-built newtypes to prevent confusion at call sites and document
invariants at the type level.

## 1. Existing Newtypes

| Newtype | Location | Status |
| --- | --- | --- |
| `FileNdx(u32)` | `crates/engine/src/concurrent_delta/types.rs:25` | Done (#1767), 47 references; `repr(transparent)` with `From<u32>` and `Display` |
| `ProtocolVersion(NonZeroU8)` | `crates/protocol/src/version/protocol_version/mod.rs:68` | Done; ~3,400 references, range-validated via `from_supported` / `from_peer_advertisement` |
| `ChecksumDigest` | `crates/checksums/src/strong/strategy/digest.rs:14` | Fixed-capacity (64-byte) buffer, no algorithm tag |
| `RollingDigest` | `crates/checksums/src/rolling/digest.rs:17` | Adler-32-style sum1+sum2 wrapper |
| `DigestBuf` | `crates/signature/src/algorithm.rs:16` | Variable-length signature digest |

## 2. Raw Primitives Still in Public APIs

`rg "pub fn .*\(.*: u32\)" crates/protocol/ crates/transfer/ crates/flist/`
returns 56 hits. Filtering for identifier-shaped parameters surfaces the
recurring confusables.

| Concern | Sample call sites | Count |
| --- | --- | --- |
| `protocol_version: u32` / `i32` field or arg | `protocol/src/state/{phases,error}.rs`, `protocol/src/wire/compressed_token/*.rs`, `batch/src/{lib,format/*}.rs`, `branding/src/*` | 21 |
| `protocol_version: u8` | `protocol/src/wire/file_entry/encode.rs`, decoders, accessors | 57 |
| `version: u32` / `i32` (negotiation, daemon) | `protocol/src/state/{typestate,dynamic}.rs:set_protocol_version` | 25 |
| `ndx: u32` / `i32` (file index in transfer pipeline) | `transfer/src/pipeline/{job,pending,async_pipeline}.rs`, `transfer/src/transfer_ops/request.rs` | 66 |
| `block_index: u32` / `block_idx: usize` | `protocol/src/wire/delta/{token,types}.rs`, `transfer/src/delta_apply/applicator.rs`, `match/src/optimized_search.rs`, all `compressed_token` codecs | 13 |
| `strong_checksum: Vec<u8>` | `match/src/optimized_search.rs:53` (`BlockEntry`), `protocol/src/wire/signature.rs:53` (`SumHead`), `protocol/src/flist/entry/accessors.rs:392` (`set_checksum`) | 9 |
| `digest: Vec<u8>` (parallel checksum result) | `engine/src/local_copy/executor/directory/parallel_checksum.rs:38` | 1 |

## 3. Candidate Newtypes

### `ProtocolVersion` re-use (highest leverage, lowest cost)
The strongly typed wrapper already exists; the work is migration. Replace raw
`protocol_version: u32` / `i32` / `u8` parameters with `ProtocolVersion`,
either by reference or by `Copy` value. Two storage widths (`u32` for branding
manifest, `u8` for wire encoders) mean the migration must add `as_u32` /
`as_u8` accessors at the boundaries; `ProtocolVersion::as_u8` already exists.

### `BlockIndex(u32)`
Wraps the block ordinal sent in `TOKEN_BLOCK_MATCH` frames and stored in
`BlockEntry`. Add `repr(transparent)`, `From<u32>`, `Display`, plus a
`fn as_usize(self) -> usize` for hash-table indexing. Prevents accidental
swap with `block_len`, `block_count`, or rolling-checksum `u32`s.

### `StrongChecksum<const N: usize>` or `StrongChecksum(SmallVec<[u8; 16]>)`
Tags a strong block-checksum byte string with its source algorithm. A const
generic (`StrongChecksum<16>` for MD4/MD5, `StrongChecksum<8>` for XXH64,
`StrongChecksum<16>` for XXH128) keeps the hot path heap-free; alternately a
`SmallVec` keeps a single non-generic type for collections like
`Vec<BlockEntry>`. Either form replaces the `Vec<u8>` field on `BlockEntry`
and the `strong_sum: Vec<u8>` on `SumHead`.

### `FileChecksum { algorithm: ChecksumAlgorithm, bytes: SmallVec<[u8; 32]> }`
Wraps the per-file digest exchanged in flist entries
(`FileEntry::set_checksum`) and parallel directory checksums. Carrying the
algorithm at the type level lets receivers reject mismatches without trusting
external state and removes a class of bugs where MD4 bytes are compared
against MD5 bytes.

### `FileNdx` extension
Already exists in `engine`. Promote it to `protocol` (or a shared `core`
module) and use it across `transfer/src/pipeline/*.rs`, where 66 raw
`ndx: i32` / `u32` parameters still flow. `i32` is needed only for the
sentinel values (`NDX_DONE = -1`, `NDX_DEL_STATS`, `NDX_FLIST_OFFSET`); model
those as a `FlistNdx` enum (`Done`, `DelStats`, `FlistOffset(i32)`,
`File(FileNdx)`).

## 4. Benefits

* **Confusion prevention.** A `u32` may be a protocol version, a file
 index, a block index, a UID, a hardlink index, an ACL slot, or an XATTR slot
 (every entry in section 2 lives in the same crate). Newtypes turn silent
 mis-wirings into compile errors.
* **Algorithm clarity.** `FileChecksum` and `StrongChecksum` carry the
 algorithm tag with the bytes. Today, `flist/entry::set_checksum(Vec<u8>)`
 trusts the caller to pass digest bytes matching the negotiated algorithm.
* **Range validation.** `ProtocolVersion::from_supported` rejects out-of-range
 advertisements at the boundary; raw `u32` parameters defer that check or
 skip it entirely.
* **Documentation.** Type signatures replace prose: `block_match(BlockIndex)`
 reads correctly without comments; `block_match(u32, u32)` does not.
* **Tooling.** `clippy::implicit_hasher` and `clippy::needless_pass_by_value`
 lints behave better against named types than `u32` aliases.

## 5. Migration Cost & Priority

Migration is **high-cost** because every raw integer crosses crate
boundaries. Prioritize by surface area and confusion risk:

| Priority | Change | Effort | Risk reduction |
| --- | --- | --- | --- |
| P0 | Replace `protocol_version: i32` / `u32` / `u8` with `ProtocolVersion` across `protocol`, `batch`, `branding`, `transfer` (~103 hits) | Medium - existing type, mechanical refactor, but spans 4 crates | High - eliminates the most-confused `u32` in the codebase |
| P1 | Introduce `BlockIndex(u32)` in `protocol::wire::delta` and propagate through `match` and `transfer::delta_apply` | Low - 13 call sites in 6 files | Medium - prevents block-index/block-length swaps on hot delta path |
| P1 | Promote `FileNdx` to `protocol` and replace `ndx: i32` / `u32` in `transfer::pipeline` (66 hits) | Medium - sentinel handling needs `FlistNdx` enum | High - file-list indices flow through the entire transfer state machine |
| P2 | Add `StrongChecksum` newtype, replace `Vec<u8>` on `BlockEntry`, `SumHead`, signature wire types | Medium - touches wire encoders and hot match path; performance must be re-benchmarked | Medium - most call sites already check `.len()`, so confusion risk is lower |
| P2 | `FileChecksum { algorithm, bytes }` for flist + parallel directory digest | Medium - changes `FileEntry` accessor signatures (public API break) | Medium - prevents algorithm/bytes mismatches |

## Recommendation

Execute P0 (`ProtocolVersion` migration) immediately - the type already
exists and the change is mechanical. Schedule P1 (`BlockIndex` plus
`FileNdx` promotion) for the next refactor window; the public API churn is
contained to two crates. Defer P2 (`StrongChecksum`, `FileChecksum`) until a
major-version bump because changing `set_checksum(Vec<u8>)` and the wire
`SumHead::strong_sum` field shape breaks downstream embedders of the
`protocol` crate.
