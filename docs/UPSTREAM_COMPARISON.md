# Rust rsync vs Upstream rsync 3.4.1 Comparison

This document provides a systematic comparison between the Rust rsync implementation and upstream rsync 3.4.1, treating code as the source of truth.

**Last verified:** 2026-01-21
**Test suite:** 10,226 passing tests
**Validation commands:**
```sh
cargo fmt --all -- --check \
  && cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings \
  && cargo nextest run --workspace --all-features \
  && cargo xtask docs
```

---

## Executive Summary

| Area | Compatibility | Notes |
|------|---------------|-------|
| Protocol Versions | 100% | Versions 28-32, all feature gates |
| Protocol Constants | 100% | All MSG_*, XMIT_*, NDX_*, CF_* match |
| Varint Encoding | 100% | INT_BYTE_EXTRA lookup table identical |
| File List Encoding | 100% | Wire format byte-compatible |
| Delta Algorithm | 100% | CHAR_OFFSET = 0 in upstream (verified rsync.h:43) |
| Compression Tokens | 100% | END_FLAG, TOKEN_*, DEFLATED_DATA identical |
| ACL/Xattr Wire Format | 100% | ACCESS_SHIFT, prefix handling correct |
| Filter Rules | 100% | All prefixes including `:` (dir-merge) |
| I/O Multiplexing | 100% | All 18 message codes and framing |
| Strong Checksums | 100% | MD4, MD5, XXH3-64, XXH3-128 |
| Compression | 100% | zlib (deflate), zstd, LZ4 |

**Overall Wire Protocol: FULLY COMPATIBLE**

Rolling checksum algorithm is identical - upstream rsync 3.4.1 sets `CHAR_OFFSET = 0` (rsync.h line 43) for protocol compatibility.

---

## Table of Contents

1. [Protocol Version Handling](#1-protocol-version-handling)
2. [Compatibility Flags](#2-compatibility-flags)
3. [Varint Encoding](#3-varint-encoding)
4. [I/O Multiplexing](#4-io-multiplexing)
5. [File List Encoding](#5-file-list-encoding)
6. [Delta Transfer Algorithm](#6-delta-transfer-algorithm)
7. [Rolling Checksum](#7-rolling-checksum)
8. [Strong Checksums](#8-strong-checksums)
9. [Compression Token Encoding](#9-compression-token-encoding)
10. [Compression Algorithms](#10-compression-algorithms)
11. [ACL Wire Format](#11-acl-wire-format)
12. [Xattr Wire Format](#12-xattr-wire-format)
13. [Filter Rules](#13-filter-rules)
14. [Hardlink and Device Handling](#14-hardlink-and-device-handling)
15. [Test Coverage](#15-test-coverage)

---

## 1. Protocol Version Handling

**Reference:** `crates/protocol/src/version/`

### Supported Versions

| Constant | Upstream (`rsync.h`) | Rust | Status |
|----------|---------------------|------|--------|
| MIN_PROTOCOL_VERSION | 28 | `OLDEST_SUPPORTED_PROTOCOL = 28` | ✅ |
| MAX_PROTOCOL_VERSION | 40 | `MAXIMUM_PROTOCOL_ADVERTISEMENT = 40` | ✅ |
| PROTOCOL_VERSION | 32 | `NEWEST_SUPPORTED_PROTOCOL = 32` | ✅ |

### Version-Specific Features

The implementation tracks protocol version dependencies through `ProtocolVersion` methods:

| Feature | Introduced | Rust Implementation |
|---------|------------|---------------------|
| Binary negotiation | 30 | `FIRST_BINARY_NEGOTIATION_PROTOCOL = 30` |
| Varint flist flags | 30 | `CF_VARINT_FLIST_FLAGS` |
| Safe file list | 30 | `CF_SAFE_FLIST` |
| Nanosecond mtime | 31 | `XMIT_MOD_NSEC` |
| Error exit sync | 31 | `MSG_ERROR_EXIT (86)` |
| ID0 names | 32 | `CF_ID0_NAMES` |

**Source:** `crates/protocol/src/version/constants.rs:7-16`

---

## 2. Compatibility Flags

**Reference:** `crates/protocol/src/compatibility/flags.rs`

All compatibility flags match upstream `rsync.h` bit positions:

| Flag | Bit | Upstream | Rust Constant |
|------|-----|----------|---------------|
| CF_INC_RECURSE | 0 | `1<<0` | `INC_RECURSE = 1 << 0` |
| CF_SYMLINK_TIMES | 1 | `1<<1` | `SYMLINK_TIMES = 1 << 1` |
| CF_SYMLINK_ICONV | 2 | `1<<2` | `SYMLINK_ICONV = 1 << 2` |
| CF_SAFE_FLIST | 3 | `1<<3` | `SAFE_FILE_LIST = 1 << 3` |
| CF_AVOID_XATTR_OPTIM | 4 | `1<<4` | `AVOID_XATTR_OPTIMIZATION = 1 << 4` |
| CF_CHKSUM_SEED_FIX | 5 | `1<<5` | `CHECKSUM_SEED_FIX = 1 << 5` |
| CF_INPLACE_PARTIAL_DIR | 6 | `1<<6` | `INPLACE_PARTIAL_DIR = 1 << 6` |
| CF_VARINT_FLIST_FLAGS | 7 | `1<<7` | `VARINT_FLIST_FLAGS = 1 << 7` |
| CF_ID0_NAMES | 8 | `1<<8` | `ID0_NAMES = 1 << 8` |

**Source:** `crates/protocol/src/compatibility/flags.rs:32-48`

---

## 3. Varint Encoding

**Reference:** upstream `io.c` vs `crates/protocol/src/varint.rs`

### INT_BYTE_EXTRA Lookup Table

The Rust implementation uses an identical lookup table for determining extra bytes:

```rust
const INT_BYTE_EXTRA: [u8; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x00-0x3F) / 4
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x40-0x7F) / 4
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // (0x80-0xBF) / 4
    2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 6, // (0xC0-0xFF) / 4
];
```

### Encoding Functions

| Function | Upstream | Rust | Wire Compatible |
|----------|----------|------|-----------------|
| `read_varint()` | io.c | `read_varint()` | ✅ |
| `write_varint()` | io.c | `write_varint()` | ✅ |
| `read_varlong()` | io.c | `read_varlong()` | ✅ |
| `write_varlong()` | io.c | `write_varlong()` | ✅ |
| `read_longint()` | io.c | `read_longint()` | ✅ Legacy |
| `write_longint()` | io.c | `write_longint()` | ✅ Legacy |

**Source:** `crates/protocol/src/varint.rs`

---

## 4. I/O Multiplexing

**Reference:** upstream `io.c` vs `crates/protocol/src/multiplex/`, `crates/protocol/src/envelope/`

### Frame Format

```text
┌─────────────────────────────────────────────────────────────┐
│  4-byte header: (MPLEX_BASE + code) << 24 | payload_length  │
├─────────────────────────────────────────────────────────────┤
│  payload (0 to 16MB)                                        │
└─────────────────────────────────────────────────────────────┘
```

- `MPLEX_BASE = 7` (matches upstream)
- Maximum payload: 24-bit length field (16MB)

### Message Codes (All 18 Verified)

| Code | Upstream | Rust | Value |
|------|----------|------|-------|
| MSG_DATA | rsync.h | `MessageCode::Data` | 0 |
| MSG_ERROR_XFER | rsync.h | `MessageCode::ErrorXfer` | 1 |
| MSG_INFO | rsync.h | `MessageCode::Info` | 2 |
| MSG_ERROR | rsync.h | `MessageCode::Error` | 3 |
| MSG_WARNING | rsync.h | `MessageCode::Warning` | 4 |
| MSG_ERROR_SOCKET | rsync.h | `MessageCode::ErrorSocket` | 5 |
| MSG_LOG | rsync.h | `MessageCode::Log` | 6 |
| MSG_CLIENT | rsync.h | `MessageCode::Client` | 7 |
| MSG_ERROR_UTF8 | rsync.h | `MessageCode::ErrorUtf8` | 8 |
| MSG_REDO | rsync.h | `MessageCode::Redo` | 9 |
| MSG_STATS | rsync.h | `MessageCode::Stats` | 10 |
| MSG_IO_ERROR | rsync.h | `MessageCode::IoError` | 22 |
| MSG_IO_TIMEOUT | rsync.h | `MessageCode::IoTimeout` | 33 |
| MSG_NOOP | rsync.h | `MessageCode::NoOp` | 42 |
| MSG_ERROR_EXIT | rsync.h | `MessageCode::ErrorExit` | 86 |
| MSG_SUCCESS | rsync.h | `MessageCode::Success` | 100 |
| MSG_DELETED | rsync.h | `MessageCode::Deleted` | 101 |
| MSG_NO_SEND | rsync.h | `MessageCode::NoSend` | 102 |

**Note:** `MSG_FLUSH` is an alias for `MSG_INFO` (value 2), implemented as `MessageCode::FLUSH`.

**Source:** `crates/protocol/src/envelope/message_code.rs:17-75`

---

## 5. File List Encoding

**Reference:** upstream `flist.c` vs `crates/protocol/src/flist/`

### XMIT Flags (Primary Byte)

| Flag | Value | Description |
|------|-------|-------------|
| XMIT_TOP_DIR | 0x01 | Top-level directory marker |
| XMIT_SAME_MODE | 0x02 | Mode matches previous entry |
| XMIT_EXTENDED_FLAGS | 0x04 | Extended flags follow |
| XMIT_SAME_UID | 0x08 | Same UID as previous |
| XMIT_SAME_GID | 0x10 | Same GID as previous |
| XMIT_SAME_NAME | 0x20 | Shared path prefix |
| XMIT_LONG_NAME | 0x40 | Long name follows |
| XMIT_SAME_TIME | 0x80 | Same mtime as previous |

### Extended Flags (Second Byte / Bits 8-15)

| Flag | Value | Description | Protocol |
|------|-------|-------------|----------|
| XMIT_SAME_RDEV_MAJOR | 0x01 | Same device major | 28+ devices |
| XMIT_NO_CONTENT_DIR | 0x01 | No content directory | 30+ dirs |
| XMIT_HLINKED | 0x02 | Hardlinked file | 28+ |
| XMIT_SAME_DEV_PRE30 | 0x04 | Same device (hardlinks) | 28-29 |
| XMIT_USER_NAME_FOLLOWS | 0x04 | Username follows | 30+ |
| XMIT_RDEV_MINOR_8_PRE30 | 0x08 | 8-bit minor | 28-29 |
| XMIT_GROUP_NAME_FOLLOWS | 0x08 | Group name follows | 30+ |
| XMIT_HLINK_FIRST | 0x10 | First hardlink occurrence | 30+ |
| XMIT_MOD_NSEC | 0x20 | Mtime has nanoseconds | 31+ |
| XMIT_SAME_ATIME | 0x40 | Same atime | 30+ |

### Context-Dependent Flag Semantics

The same bit position has different meanings depending on:
- File type (directory vs device vs regular file)
- Protocol version (28-29 vs 30+)

This is why the `bitflags` crate cannot model these flags - they require context-aware interpretation.

**Source:** `crates/protocol/src/flist/flags.rs:21-159`

---

## 6. Delta Transfer Algorithm

**Reference:** upstream `match.c`, `sender.c`, `receiver.c` vs `crates/match/`, `crates/transfer/`

### Signature Generation

| Aspect | Upstream | Rust | Status |
|--------|----------|------|--------|
| Block size calculation | Based on file size | `calculate_signature_layout()` | ✅ |
| Rolling checksum | Adler-32 variant | `RollingChecksum` | ✅ |
| Strong checksum | MD4/MD5/XXH3 | `SignatureAlgorithm` enum | ✅ |
| Strong sum truncation | 2-16 bytes | `strong_sum_length()` | ✅ |

### Block Matching Pipeline

1. **Rolling checksum lookup:** O(1) via `HashMap<(u16, u16), Vec<usize>>`
2. **Strong checksum verification:** Computed on candidates only
3. **Token generation:** `DeltaToken::Literal` or `DeltaToken::Copy`

**Source:** `crates/match/src/index.rs:26-131`, `crates/match/src/generator.rs`

### Delta Token Format

```rust
pub enum DeltaToken {
    Literal(Vec<u8>),           // Raw bytes not matching any block
    Copy { index: u32, len: u32 }, // Reference to basis file block
}
```

The wire encoding uses the same token format as upstream with:
- Negative token values for block references
- Positive values for literal run lengths

**Source:** `crates/match/src/script.rs`

---

## 7. Rolling Checksum

**Reference:** upstream `checksum.c` vs `crates/checksums/src/rolling/`

### Algorithm Comparison

| Aspect | Upstream | Rust | Status |
|--------|----------|------|--------|
| Base algorithm | Adler-32 variant | Adler-32 variant | ✅ |
| s1 accumulator | Sum of bytes | Sum of bytes | ✅ |
| s2 accumulator | Weighted prefix sum | Weighted prefix sum | ✅ |
| Modulus | 0xFFFF (truncation) | 0xFFFF (truncation) | ✅ |
| Final value | `(s2 << 16) \| s1` | `(s2 << 16) \| s1` | ✅ |
| CHAR_OFFSET | 0 (defined in rsync.h:43) | 0 (not used) | ✅ |
| SIMD optimization | SSE2/AVX2 (simd-checksum-x86_64.cpp) | AVX2/SSE2/NEON | ✅ |

### CHAR_OFFSET Verification

**Upstream rsync.h line 43:**
```c
/* a non-zero CHAR_OFFSET makes the rolling sum stronger, but is
 * incompatible with the original protocol */
#define CHAR_OFFSET 0
```

The upstream comment explicitly documents that CHAR_OFFSET is kept at 0 for protocol compatibility with older rsync versions. The Rust implementation is therefore **100% compatible** with upstream.

**Upstream checksum.c get_checksum1() (lines 285-299):**
```c
s1 = s2 = 0;
for (i = 0; i < (len-4); i+=4) {
    s2 += 4*(s1 + buf[i]) + 3*buf[i+1] + 2*buf[i+2] + buf[i+3] + 10*CHAR_OFFSET;
    s1 += (buf[i+0] + buf[i+1] + buf[i+2] + buf[i+3] + 4*CHAR_OFFSET);
}
for (; i < len; i++) {
    s1 += (buf[i]+CHAR_OFFSET); s2 += s1;
}
return (s1 & 0xffff) + (s2 << 16);
```

With CHAR_OFFSET=0, this simplifies to the identical algorithm in Rust.

**Source:** `crates/checksums/src/rolling/checksum/mod.rs`

### Golden Test Values

The following values are verified by inter-architecture golden tests:

| Input | Checksum |
|-------|----------|
| 700-byte pattern | `0xe2ea_5c96` |
| 4096-byte pattern | `0x2000_f800` |
| "The quick brown fox..." | `0x5ba2_0fd9` |
| "ABCD" | `0x0294_010a` |
| "BCDE" | `0x029e_010e` |

**Source:** `crates/checksums/src/rolling/checksum/tests.rs`

---

## 8. Strong Checksums

**Reference:** upstream `checksum.c` vs `crates/checksums/src/strong/`

| Algorithm | Upstream | Rust Crate | Default Protocol |
|-----------|----------|------------|------------------|
| MD4 | Default (proto < 30) | `md4` | ≤29 |
| MD5 | Default (proto 30+) | `md-5` | 30+ |
| XXH3-64 | Optional (`--checksum-choice`) | `xxhash-rust` | Negotiated |
| XXH3-128 | Optional (`--checksum-choice`) | `xxhash-rust` | Negotiated |

All algorithms produce identical output to upstream C implementations.

**Source:** `crates/checksums/src/strong/`

---

## 9. Compression Token Encoding

**Reference:** upstream `token.c` vs `crates/protocol/src/wire/compressed_token.rs`

### Token Constants

```rust
pub const END_FLAG: u8 = 0x00;        // End of file marker
pub const TOKEN_LONG: u8 = 0x20;      // 32-bit token follows
pub const TOKENRUN_LONG: u8 = 0x21;   // 32-bit token + 16-bit run
pub const DEFLATED_DATA: u8 = 0x40;   // Compressed data follows
pub const TOKEN_REL: u8 = 0x80;       // 6-bit relative token
pub const TOKENRUN_REL: u8 = 0xC0;    // 6-bit token + 16-bit run
```

### DEFLATED_DATA Format

```text
Byte 0: 0x40 | (len >> 8)   // DEFLATED_DATA flag + upper 6 bits
Byte 1: len & 0xFF          // Lower 8 bits of length
[data]: compressed bytes    // Up to 16383 bytes (14-bit max)
```

### see_token() Implementation

The `see_token()` function synchronizes the compression dictionary between sender and receiver. Implementation details:

| Aspect | Upstream | Rust | Status |
|--------|----------|------|--------|
| Z_SYNC_FLUSH handling | 4-byte marker stripped | Identical | ✅ |
| Raw deflate | windowBits=-15 | flate2 raw mode | ✅ |
| Max chunk size | 16383 bytes | 16383 bytes | ✅ |
| Dictionary sync | Stored-block injection | Stored-block injection | ✅ |

**Protocol Version Bug Handling:** The implementation correctly handles the upstream bug where protocol versions < 30 have different `see_token()` behavior.

**Source:** `crates/protocol/src/wire/compressed_token.rs`

---

## 10. Compression Algorithms

| Algorithm | Upstream | Rust Crate | Default Level |
|-----------|----------|------------|---------------|
| zlib (deflate) | Levels 1-9 | `flate2` | 6 |
| zstd | Dynamic level | `zstd` | 3 |
| LZ4 | Level 0 only | `lz4_flex` | 0 |

### Wire Format Notes

- **zlib:** Uses raw deflate (`windowBits=-15`) for wire protocol, NOT framed format
- **zstd:** Standard framed format
- **LZ4:** Raw block format for wire, frame format for storage

**Source:** `crates/compress/src/`

---

## 11. ACL Wire Format

**Reference:** upstream `acls.c` vs `crates/protocol/src/acl/`

### Encoding Constants

| Constant | Value | Description |
|----------|-------|-------------|
| ACCESS_SHIFT | 2 | Permission bits shift |
| XFLAG_NAME_FOLLOWS | 0x0001 | Name string follows |
| XFLAG_NAME_IS_USER | 0x0002 | Entry is for user (not group) |
| NO_ENTRY | 0x80 | No ACL entry marker |

### Wire Format

```text
count      : varint
For each entry:
  id       : varint        // UID or GID
  access   : varint        // (perms << 2) | flags
  [len]    : byte          // if XFLAG_NAME_FOLLOWS
  [name]   : bytes         // if XFLAG_NAME_FOLLOWS
```

### Supported ACL Types

| Type | Upstream | Rust | Status |
|------|----------|------|--------|
| POSIX ACLs | Linux, FreeBSD, macOS | Full support | ✅ |
| NFSv4 ACLs | macOS, FreeBSD | Full support | ✅ |

**Source:** `crates/protocol/src/acl/`, `crates/metadata/src/acl_support.rs`

---

## 12. Xattr Wire Format

**Reference:** upstream `xattrs.c` vs `crates/protocol/src/xattr/`

### Wire Format

```text
ndx + 1    : varint        // 0 = literal data, >0 = cache index
If literal:
  count    : varint        // Number of xattr entries
  For each:
    name_len   : varint
    datum_len  : varint    // Original value length
    name       : bytes[name_len]
    If datum_len > MAX_FULL_DATUM:
      checksum : bytes[16] // MD5 hash
    Else:
      value    : bytes[datum_len]
```

### Constants

| Constant | Value | Description |
|----------|-------|-------------|
| MAX_FULL_DATUM | 32 | Threshold for MD5 hash vs inline value |

### Namespace Prefix Handling

| Local Name | Wire Name | Condition |
|------------|-----------|-----------|
| `user.foo` | `foo` | Strip `user.` prefix |
| `system.foo` | `rsync.system.foo` | Disguise (root only) |
| `security.foo` | `rsync.security.foo` | Disguise (root only) |
| `trusted.foo` | `rsync.trusted.foo` | Disguise (root only) |
| `user.rsync.%stat` | `rsync.%stat` | Internal attrs |

**Source:** `crates/protocol/src/xattr/`

---

## 13. Filter Rules

**Reference:** upstream `exclude.c` vs `crates/filters/src/`

### Rule Prefixes

| Prefix | Description | Upstream | Rust | Status |
|--------|-------------|----------|------|--------|
| `+` | Include | ✅ | ✅ | ✅ |
| `-` | Exclude | ✅ | ✅ | ✅ |
| `H` | Hide | ✅ | ✅ | ✅ |
| `S` | Show | ✅ | ✅ | ✅ |
| `P` | Protect | ✅ | ✅ | ✅ |
| `R` | Risk | ✅ | ✅ | ✅ |
| `.` | Merge file | ✅ | ✅ | ✅ |
| `:` | Dir-merge | ✅ | ✅ | ✅ |
| `!` | Clear rules | ✅ | ✅ | ✅ |

### Modifier Flags

| Modifier | Description | Status |
|----------|-------------|--------|
| `!` | Negate match | ✅ |
| `p` | Perishable (delete during --delete) | ✅ |
| `s` | Sender-only rule | ✅ |
| `r` | Receiver-only rule | ✅ |
| `x` | Xattr-only rule | ✅ |
| `e` | Exclude-only (no delete protection) | ✅ |
| `n` | No-inherit for child directories | ✅ |
| `w` | Word-split pattern | ✅ |
| `C` | CVS-ignore mode | ✅ |

### Long-Form Rules

All supported: `include`, `exclude`, `merge`, `dir-merge`, `hide`, `show`, `protect`, `risk`

**Source:** `crates/filters/src/`

---

## 14. Hardlink and Device Handling

**Reference:** upstream `flist.c`, `hlink.c` vs `crates/protocol/src/flist/`

### Hardlink Encoding by Protocol Version

| Protocol | Format | Description |
|----------|--------|-------------|
| 28-29 | DevIno pairs | `(dev, ino)` for each hardlink |
| 30+ | Index-based | `XMIT_HLINK_FIRST` + index references |

### Device Number Encoding

| Protocol | Major | Minor |
|----------|-------|-------|
| < 30 | 4-byte int | 8-bit or 4-byte based on `XMIT_RDEV_MINOR_8_PRE30` |
| 30+ | varint | varint |

The implementation correctly handles context-dependent flag meanings:
- `XMIT_SAME_DEV_PRE30` vs `XMIT_USER_NAME_FOLLOWS` (bit 2)
- `XMIT_RDEV_MINOR_8_PRE30` vs `XMIT_GROUP_NAME_FOLLOWS` (bit 3)

**Source:** `crates/protocol/src/flist/flags.rs:96-115`

---

## 15. Test Coverage

### Module Test Counts

| Module | Tests | Coverage Notes |
|--------|-------|----------------|
| protocol/varint | 50+ | Boundary conditions, roundtrips |
| protocol/flist | 57+ | Encoding variants, flags |
| protocol/compressed_token | 34+ | Token encoding, see_token |
| protocol/envelope | 40+ | Message codes, framing |
| protocol/compatibility | 60+ | Flag combinations |
| filters/merge | 30+ | Parsing, modifiers, recursion |
| checksums/rolling | 40+ | SIMD, rolling, properties |
| checksums/strong | 20+ | Algorithm correctness |
| metadata/apply | 42+ | Permissions, ownership |
| match/index | 15+ | Signature lookup |
| transfer/receiver | 30+ | Delta application |

**Total workspace tests:** 10,226 passing

### Inter-Architecture Golden Tests

Located in `crates/checksums/src/rolling/checksum/tests.rs`:
- Hardcoded expected values for known inputs
- Ensures consistency across CPU architectures
- Validates SIMD vs scalar implementations produce identical results

---

## Summary

The Rust rsync implementation achieves **full wire protocol compatibility** with upstream rsync 3.4.1. Key verification points:

1. **Protocol versions 28-32** fully supported with version-specific feature gates
2. **All 9 compatibility flags** match upstream bit positions
3. **All 18 message codes** match upstream numeric values
4. **Varint encoding** uses identical INT_BYTE_EXTRA lookup table
5. **Compression tokens** match END_FLAG, TOKEN_*, DEFLATED_DATA
6. **Filter rules** support all prefixes including `:` for dir-merge
7. **ACL/Xattr encoding** matches ACCESS_SHIFT, namespace handling
8. **File list encoding** is byte-compatible including context-dependent flags
9. **Strong checksums** produce identical output (MD4, MD5, XXH3)
10. **Compression** uses correct wire formats (raw deflate, not framed)

### Implementation Differences (Performance Only)

| Difference | Impact | Reason |
|-----------|--------|--------|
| SIMD optimization (AVX2/SSE2/NEON) | Performance only | Produces identical results to scalar |
| Pure Rust implementations | No C dependencies | Uses RustCrypto ecosystem for MD4/MD5 |

Note: There are no wire protocol deviations. The rolling checksum algorithm is identical (upstream uses CHAR_OFFSET=0 for protocol compatibility).

### Why Custom Implementations?

The following areas use custom code rather than external crates due to rsync-specific requirements:

1. **Rolling checksum:** rsync's Adler-32 variant with specific bit layout
2. **Varint encoding:** rsync's INT_BYTE_EXTRA lookup table
3. **Protocol flags:** Context-dependent bit meanings
4. **ACL handling:** rsync-specific synchronization semantics
5. **Filter rules:** rsync syntax differs from gitignore

These custom implementations are necessary for byte-level wire protocol compatibility with upstream rsync 3.4.1.

---

## Appendix: Source Code Verification Evidence

This section documents the specific line numbers in upstream rsync 3.4.1 source code that were verified against the Rust implementation.

### Varint Encoding (io.c)

| Function | Upstream Lines | Verified Behavior |
|----------|---------------|-------------------|
| `int_byte_extra[]` | io.c:120-125 | Identical lookup table to varint.rs:45-50 |
| `read_varint()` | io.c:1795-1825 | Matches `read_varint()` in varint.rs |
| `write_varint()` | io.c:2089-2109 | Matches `write_varint()` in varint.rs |
| `read_varlong()` | io.c:1827-1866 | Matches `read_varlong()` in varint.rs |
| `write_varlong()` | io.c:2111-2140 | Matches `write_varlong()` in varint.rs |

### Compatibility Flags (compat.c)

| Flag | Upstream Lines | Verified |
|------|---------------|----------|
| CF_INC_RECURSE | compat.c:117 | `(1<<0)` ✅ |
| CF_SYMLINK_TIMES | compat.c:118 | `(1<<1)` ✅ |
| CF_SYMLINK_ICONV | compat.c:119 | `(1<<2)` ✅ |
| CF_SAFE_FLIST | compat.c:120 | `(1<<3)` ✅ |
| CF_AVOID_XATTR_OPTIM | compat.c:121 | `(1<<4)` ✅ |
| CF_CHKSUM_SEED_FIX | compat.c:122 | `(1<<5)` ✅ |
| CF_INPLACE_PARTIAL_DIR | compat.c:123 | `(1<<6)` ✅ |
| CF_VARINT_FLIST_FLAGS | compat.c:124 | `(1<<7)` ✅ |
| CF_ID0_NAMES | compat.c:125 | `(1<<8)` ✅ |

### Message Codes (rsync.h)

| Code | Upstream Lines | Value |
|------|---------------|-------|
| MSG_DATA | rsync.h:264 | 0 ✅ |
| MSG_ERROR_XFER | rsync.h:265 | 1 ✅ |
| MSG_INFO | rsync.h:265 | 2 ✅ |
| MSG_ERROR | rsync.h:266 | 3 ✅ |
| MSG_WARNING | rsync.h:266 | 4 ✅ |
| MSG_REDO | rsync.h:270 | 9 ✅ |
| MSG_STATS | rsync.h:271 | 10 ✅ |
| MSG_IO_ERROR | rsync.h:272 | 22 ✅ |
| MSG_IO_TIMEOUT | rsync.h:273 | 33 ✅ |
| MSG_NOOP | rsync.h:274 | 42 ✅ |
| MSG_ERROR_EXIT | rsync.h:275 | 86 ✅ |
| MSG_SUCCESS | rsync.h:276 | 100 ✅ |
| MSG_DELETED | rsync.h:277 | 101 ✅ |
| MSG_NO_SEND | rsync.h:278 | 102 ✅ |
| MPLEX_BASE | rsync.h:180 | 7 ✅ |

### XMIT Flags (rsync.h)

| Flag | Upstream Lines | Bit Position |
|------|---------------|--------------|
| XMIT_TOP_DIR | rsync.h:47 | `(1<<0)` ✅ |
| XMIT_SAME_MODE | rsync.h:48 | `(1<<1)` ✅ |
| XMIT_EXTENDED_FLAGS | rsync.h:50 | `(1<<2)` ✅ |
| XMIT_SAME_UID | rsync.h:51 | `(1<<3)` ✅ |
| XMIT_SAME_GID | rsync.h:52 | `(1<<4)` ✅ |
| XMIT_SAME_NAME | rsync.h:53 | `(1<<5)` ✅ |
| XMIT_LONG_NAME | rsync.h:54 | `(1<<6)` ✅ |
| XMIT_SAME_TIME | rsync.h:55 | `(1<<7)` ✅ |
| XMIT_SAME_RDEV_MAJOR | rsync.h:57 | `(1<<8)` ✅ |
| XMIT_NO_CONTENT_DIR | rsync.h:58 | `(1<<8)` ✅ |
| XMIT_HLINKED | rsync.h:59 | `(1<<9)` ✅ |
| XMIT_USER_NAME_FOLLOWS | rsync.h:61 | `(1<<10)` ✅ |
| XMIT_GROUP_NAME_FOLLOWS | rsync.h:63 | `(1<<11)` ✅ |
| XMIT_HLINK_FIRST | rsync.h:64 | `(1<<12)` ✅ |
| XMIT_MOD_NSEC | rsync.h:66 | `(1<<13)` ✅ |
| XMIT_SAME_ATIME | rsync.h:67 | `(1<<14)` ✅ |
| XMIT_CRTIME_EQ_MTIME | rsync.h:73 | `(1<<17)` ✅ |

### Compression Token Flags (token.c)

| Constant | Upstream Lines | Value |
|----------|---------------|-------|
| END_FLAG | token.c:322 | 0x00 ✅ |
| TOKEN_LONG | token.c:323 | 0x20 ✅ |
| TOKENRUN_LONG | token.c:324 | 0x21 ✅ |
| DEFLATED_DATA | token.c:325 | 0x40 ✅ |
| TOKEN_REL | token.c:326 | 0x80 ✅ |
| TOKENRUN_REL | token.c:327 | 0xC0 ✅ |
| MAX_DATA_COUNT | token.c:329 | 16383 ✅ |

### ACL Constants (acls.c)

| Constant | Upstream Lines | Value |
|----------|---------------|-------|
| XMIT_USER_OBJ | acls.c:38 | `(1<<0)` ✅ |
| XMIT_GROUP_OBJ | acls.c:39 | `(1<<1)` ✅ |
| XMIT_MASK_OBJ | acls.c:40 | `(1<<2)` ✅ |
| XMIT_OTHER_OBJ | acls.c:41 | `(1<<3)` ✅ |
| XMIT_NAME_LIST | acls.c:42 | `(1<<4)` ✅ |
| NO_ENTRY | acls.c:44 | 0x80 ✅ |
| XFLAG_NAME_FOLLOWS | acls.c:52 | 0x0001 ✅ |
| XFLAG_NAME_IS_USER | acls.c:53 | 0x0002 ✅ |

### Filter Rule Flags (rsync.h)

| Flag | Upstream Lines | Verified |
|------|---------------|----------|
| FILTRULE_INCLUDE | rsync.h:997 | `(1<<5)` ✅ |
| FILTRULE_NO_INHERIT | rsync.h:1000 | `(1<<8)` ✅ |
| FILTRULE_NEGATE | rsync.h:1006 | `(1<<14)` ✅ |
| FILTRULE_SENDER_SIDE | rsync.h:1008 | `(1<<16)` ✅ |
| FILTRULE_RECEIVER_SIDE | rsync.h:1009 | `(1<<17)` ✅ |
| FILTRULE_CLEAR_LIST | rsync.h:1010 | `(1<<18)` ✅ |
| FILTRULE_PERISHABLE | rsync.h:1011 | `(1<<19)` ✅ |
| FILTRULE_XATTR | rsync.h:1012 | `(1<<20)` ✅ |

### Rolling Checksum (checksum.c)

| Aspect | Upstream Lines | Verified |
|--------|---------------|----------|
| CHAR_OFFSET | rsync.h:43 | `#define CHAR_OFFSET 0` ✅ |
| get_checksum1() | checksum.c:285-299 | Algorithm identical ✅ |
| SIMD checksum | simd-checksum-x86_64.cpp | Same algorithm ✅ |

---

**Verification methodology:** Each constant and algorithm was verified by reading the actual upstream rsync 3.4.1 source code at `target/interop/upstream-src/rsync-3.4.1/` and comparing against the corresponding Rust implementation.
