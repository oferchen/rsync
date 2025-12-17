# Protocol 30+ Capability Negotiation Status

## Overview

This document tracks the implementation status of Protocol 30+ capability negotiation
for oc-rsync, following the upstream rsync protocol specification.

## Completed ✅

### 1. Core Infrastructure (Commits: 857ea0e7, 68ba43e0, 2838f35f, db41d62e, 350c88dd, f9d22b2c)

- **Capability Negotiation Module** (`crates/protocol/src/negotiation/capabilities.rs`)
  - Implements server-side `negotiate_capabilities()` function
  - Exchanges checksum and compression algorithm preferences
  - Conditional on VARINT_FLIST_FLAGS ('v') capability
  - Supports: MD4, MD5, SHA1, XXH64, XXH128 for checksums
  - Supports: None, Zlib, ZlibX, LZ4, Zstd for compression

- **Compatibility Flags** (`crates/protocol/src/negotiation/compat_flags.rs`)
  - Complete mapping of all 9 flags (i, L, s, f, x, C, I, v, u)
  - INC_RECURSE, SYMLINK_TIMES, SYMLINK_ICONV, SAFE_FILE_LIST
  - AVOID_XATTR_OPTIMIZATION, CHECKSUM_SEED_FIX, INPLACE_PARTIAL_DIR
  - VARINT_FLIST_FLAGS, ID0_NAMES

### 2. Protocol Setup (Commit: 350c88dd)

- **Checksum Seed Generation** (`crates/core/src/server/setup.rs`)
  - Generated for ALL protocols (not just 30+)
  - Formula: `timestamp ^ (pid << 6)` matching upstream
  - Transmitted after compat flags exchange
  - Used for XXHash algorithm variants

- **SetupResult Structure**
  ```rust
  pub struct SetupResult {
      pub negotiated_algorithms: Option<NegotiationResult>,
      pub compat_flags: Option<CompatibilityFlags>,
      pub checksum_seed: i32,
  }
  ```

### 3. Handshake Integration (Commits: db41d62e, 350c88dd)

- **HandshakeResult Extended**
  - Added `negotiated_algorithms` field
  - Added `compat_flags` field
  - Added `checksum_seed` field
  - All fields populated by `setup_protocol()`

### 4. Checksum Algorithm Selection (Commit: f9d22b2c)

- **ReceiverContext** (`crates/core/src/server/receiver.rs`)
  - Stores negotiated algorithms and checksum seed
  - Uses negotiated checksum for signature generation
  - Fallback: negotiated → MD5 (protocol 30+) → MD4 (legacy)
  - XXHash variants use transmitted checksum seed

- **GeneratorContext** (`crates/core/src/server/generator.rs`)
  - Stores negotiated algorithms and checksum seed
  - Uses negotiated checksum for delta generation
  - Same fallback logic as receiver
  - XXHash seed support

- **Conversion Helper**
  ```rust
  fn checksum_algorithm_to_signature(
      algorithm: ChecksumAlgorithm,
      seed: i32,
  ) -> SignatureAlgorithm
  ```

## Partially Complete ⚠️

### Compression Algorithm Selection

- **Status**: Negotiation works and algorithm is stored, but not yet applied
- **What Works**:
  - Compression algorithm is negotiated during capability exchange
  - Result stored in `NegotiationResult.compression`
  - Available in role contexts via `negotiated_algorithms`
- **What's Missing**:
  - Server-side compression streams not yet implemented
  - `ServerWriter` only handles Plain→Multiplex, not compression
  - Need to create compression wrapper layer in stream stack
  - Need to wire negotiated algorithm to compression wrappers

### Compatibility Flags Usage

- **Status**: Flags are exchanged and stored, but not yet used for protocol behavior
- **What Works**:
  - Flags negotiated and stored in `HandshakeResult.compat_flags`
  - Available to role contexts
- **What's Missing**:
  - Flags not yet used to control protocol behaviors
  - Examples: INC_RECURSE for incremental recursion
  - CHECKSUM_SEED_FIX for seed order variations
  - VARINT_FLIST_FLAGS for file list encoding

## Protocol Flow

```
Client connects
     ↓
Binary version handshake (for SSH mode)
or @RSYNCD negotiation (for daemon mode)
     ↓
[If protocol >= 30 and not compat_exchanged]
  ↓
  Write compat flags (varint)
  ↓
  [If VARINT_FLIST_FLAGS capability present]
    ↓
    Negotiate checksums (send/receive lists)
    ↓
    Negotiate compression (send/receive lists)
  ↓
  Send checksum seed (4 bytes, little-endian)
     ↓
Multiplex activation
     ↓
Transfer begins (uses negotiated checksum)
```

## Test Results

- **Formatting**: ✅ `cargo fmt --all -- --check` PASSED
- **Linting**: ✅ `cargo clippy` PASSED
- **Tests**: ✅ 3319/3321 tests passing
  - 2 pre-existing failures (unrelated to this work):
    - `core server::generator::tests::build_and_send_round_trip`
    - `protocol flist::write::tests::write_then_read_round_trip`

## Implementation Notes

### Checksum Seed

The checksum seed is generated and sent for ALL protocols, not just Protocol 30+.
This matches upstream behavior (compat.c:750). The seed is used by XXHash variants
to initialize their hash state.

### Algorithm Fallback

The checksum selection follows a clear precedence:

1. **Negotiated** (Protocol 30+ with 'v' capability): Use client's selected algorithm
2. **Protocol 30+ default** (no negotiation): Use MD5
3. **Legacy default** (Protocol < 30): Use MD4

### Dead Code Warnings

The `compat_flags` fields in `ReceiverContext` and `GeneratorContext` are marked
with `#[allow(dead_code)]` because they're not yet used to control protocol
behaviors. This is intentional - the infrastructure is in place for future use.

## Future Work

### Short Term

1. **Add integration tests** for negotiated algorithm usage
   - Test that MD5/SHA1/XXH64/XXH128 are actually used when negotiated
   - Test fallback behavior for protocols < 30
   - Test checksum seed propagation to XXHash

2. **Implement compatibility flags usage**
   - Use INC_RECURSE flag for incremental recursion mode
   - Use CHECKSUM_SEED_FIX for seed order handling
   - Use other flags for their intended protocol behaviors

### Medium Term

3. **Implement server-side compression**
   - Create `CompressedWriter` and `CompressedReader` wrappers
   - Integrate into server stream stack (Plain → Multiplex → Compress)
   - Wire negotiated compression algorithm to wrappers
   - Handle compression lifecycle (init, write, flush, finish)

4. **Golden handshake test files**
   - Generate reference handshake captures for protocols 28-32
   - Add regression tests against golden files
   - Validate wire format matches upstream rsync

### Long Term

5. **Client-side implementation**
   - Implement client side of capability negotiation
   - Support client requesting specific algorithms
   - End-to-end negotiation tests

6. **Performance testing**
   - Benchmark different checksum algorithms
   - Benchmark compression algorithms
   - Document performance characteristics

## References

- Upstream rsync source: `compat.c` (protocol setup and negotiation)
- Upstream rsync source: `generator.c` (signature generation)
- Upstream rsync source: `main.c` (server startup and stream setup)
- This implementation: `crates/protocol/src/negotiation/capabilities.rs`
- This implementation: `crates/core/src/server/setup.rs`

## Changelog

- **2025-01-XX**: Implemented negotiated checksum algorithm selection (f9d22b2c)
- **2025-01-XX**: Added checksum seed to protocol setup (350c88dd)
- **2025-01-XX**: Wired negotiated algorithms to role contexts (db41d62e)
- **2025-01-XX**: Made capability negotiation conditional on 'v' flag (2838f35f)
- **2025-01-XX**: Added Protocol 30+ capability negotiation (68ba43e0)
- **2025-01-XX**: Created negotiation module infrastructure (857ea0e7)
