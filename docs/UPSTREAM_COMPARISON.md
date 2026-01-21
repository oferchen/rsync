# Rust rsync vs Upstream rsync 3.4.1 Comparison

This document provides a systematic comparison between the Rust rsync implementation and upstream rsync 3.4.1, treating code as the source of truth.

**Last verified:** 2026-01-21
**Test suite:** 10,285 passing tests
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
| Protocol Versions | 100%* | Versions 28-32 (upstream supports 20-40) |
| Protocol Constants | 100% | All MSG_*, XMIT_*, NDX_*, CF_* match |
| Varint Encoding | 100% | INT_BYTE_EXTRA lookup table identical |
| File List Encoding | 100% | Wire format byte-compatible |
| NDX Encoding | 100% | Legacy (4-byte LE) and modern (delta) |
| Delta Algorithm | 100% | CHAR_OFFSET = 0 in upstream (verified rsync.h:43) |
| Compression Tokens | 100% | END_FLAG, TOKEN_*, DEFLATED_DATA identical |
| ACL/Xattr Wire Format | 100% | ACCESS_SHIFT, prefix handling correct |
| Filter Rules | 100% | All prefixes including `:` (dir-merge) |
| I/O Multiplexing | 100% | All 18 message codes and framing |
| Strong Checksums | 100% | MD4, MD5, XXH3-64, XXH3-128 |
| Compression | 100% | zlib (deflate), zstd, LZ4 |
| Daemon Protocol | 100% | Greeting, authentication, module listing |
| Checksum Negotiation | 100% | Seed exchange, algorithm selection |
| Delete Handling | 100% | MSG_DELETED, NDX_DEL_STATS |
| Exit/Error Codes | 100% | All RERR_*, IOERR_* match |
| Time Handling | 100% | mtime, atime, crtime, nanoseconds |
| Device Handling | 100% | rdev encoding, protocol differences |

**Overall Wire Protocol: FULLY COMPATIBLE**

\* *Protocol versions 28-32 are fully compatible. Versions 20-27 are intentionally unsupported (15+ years old).*

---

## Table of Contents

1. [Protocol Version Handling](#1-protocol-version-handling)
2. [Compatibility Flags](#2-compatibility-flags)
3. [Varint Encoding](#3-varint-encoding)
4. [I/O Multiplexing](#4-io-multiplexing)
5. [File List Encoding](#5-file-list-encoding)
6. [NDX (File Index) Encoding](#6-ndx-file-index-encoding)
7. [Delta Transfer Algorithm](#7-delta-transfer-algorithm)
8. [Rolling Checksum](#8-rolling-checksum)
9. [Strong Checksums](#9-strong-checksums)
10. [Checksum Negotiation and Seed](#10-checksum-negotiation-and-seed)
11. [Compression Token Encoding](#11-compression-token-encoding)
12. [Compression Algorithms](#12-compression-algorithms)
13. [ACL Wire Format](#13-acl-wire-format)
14. [Xattr Wire Format](#14-xattr-wire-format)
15. [Filter Rules](#15-filter-rules)
16. [Delete Handling](#16-delete-handling)
17. [Time Handling](#17-time-handling)
18. [Device and Special File Handling](#18-device-and-special-file-handling)
19. [Daemon Protocol](#19-daemon-protocol)
20. [Bandwidth Limiting](#20-bandwidth-limiting)
21. [Sparse File Handling](#21-sparse-file-handling)
22. [Backup Handling](#22-backup-handling)
23. [Error and Exit Codes](#23-error-and-exit-codes)
24. [Incremental Recursion](#24-incremental-recursion)
25. [Test Coverage](#25-test-coverage)

---

## 1. Protocol Version Handling

**Reference:** `crates/protocol/src/version/`

### Supported Versions

| Constant | Upstream (`rsync.h`) | Rust | Status |
|----------|---------------------|------|--------|
| MIN_PROTOCOL_VERSION | 20 | `OLDEST_SUPPORTED_PROTOCOL = 28` | ⚠️ Intentional |
| MAX_PROTOCOL_VERSION | 40 | `MAXIMUM_PROTOCOL_ADVERTISEMENT = 40` | ✅ |
| PROTOCOL_VERSION | 32 | `NEWEST_SUPPORTED_PROTOCOL = 32` | ✅ |

> **Note:** The Rust implementation intentionally supports a narrower protocol range (28-32)
> compared to upstream's (20-40). Protocol versions 20-27 are over 15 years old and
> rarely encountered. This simplifies the codebase while maintaining compatibility with
> all modern rsync clients and servers.

### Version-Specific Features

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

The Rust implementation uses an identical lookup table:

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
| `read_varint()` | io.c:1795-1825 | `read_varint()` | ✅ |
| `write_varint()` | io.c:2089-2109 | `write_varint()` | ✅ |
| `read_varlong()` | io.c:1827-1866 | `read_varlong()` | ✅ |
| `write_varlong()` | io.c:2111-2140 | `write_varlong()` | ✅ |
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
| MSG_DATA | rsync.h:264 | `MessageCode::Data` | 0 |
| MSG_ERROR_XFER | rsync.h:265 | `MessageCode::ErrorXfer` | 1 |
| MSG_INFO | rsync.h:265 | `MessageCode::Info` | 2 |
| MSG_ERROR | rsync.h:266 | `MessageCode::Error` | 3 |
| MSG_WARNING | rsync.h:266 | `MessageCode::Warning` | 4 |
| MSG_ERROR_SOCKET | rsync.h | `MessageCode::ErrorSocket` | 5 |
| MSG_LOG | rsync.h | `MessageCode::Log` | 6 |
| MSG_CLIENT | rsync.h | `MessageCode::Client` | 7 |
| MSG_ERROR_UTF8 | rsync.h | `MessageCode::ErrorUtf8` | 8 |
| MSG_REDO | rsync.h:270 | `MessageCode::Redo` | 9 |
| MSG_STATS | rsync.h:271 | `MessageCode::Stats` | 10 |
| MSG_IO_ERROR | rsync.h:272 | `MessageCode::IoError` | 22 |
| MSG_IO_TIMEOUT | rsync.h:273 | `MessageCode::IoTimeout` | 33 |
| MSG_NOOP | rsync.h:274 | `MessageCode::NoOp` | 42 |
| MSG_ERROR_EXIT | rsync.h:275 | `MessageCode::ErrorExit` | 86 |
| MSG_SUCCESS | rsync.h:276 | `MessageCode::Success` | 100 |
| MSG_DELETED | rsync.h:277 | `MessageCode::Deleted` | 101 |
| MSG_NO_SEND | rsync.h:278 | `MessageCode::NoSend` | 102 |

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

### Extended Flags (Bits 8-17)

| Flag | Bit | Description | Context |
|------|-----|-------------|---------|
| XMIT_SAME_RDEV_MAJOR | 8 | Same device major | Devices |
| XMIT_NO_CONTENT_DIR | 8 | No content directory | Directories (30+) |
| XMIT_HLINKED | 9 | Hardlinked file | 28+ |
| XMIT_SAME_DEV_PRE30 | 10 | Same device (hardlinks) | 28-29 |
| XMIT_USER_NAME_FOLLOWS | 10 | Username follows | 30+ |
| XMIT_RDEV_MINOR_8_PRE30 | 11 | 8-bit minor | 28-29 |
| XMIT_GROUP_NAME_FOLLOWS | 11 | Group name follows | 30+ |
| XMIT_HLINK_FIRST | 12 | First hardlink | 30+ |
| XMIT_MOD_NSEC | 13 | Mtime has nanoseconds | 31+ |
| XMIT_SAME_ATIME | 14 | Same atime | 30+ |
| XMIT_CRTIME_EQ_MTIME | 17 | crtime equals mtime | 30+ |

**Source:** `crates/protocol/src/flist/flags.rs:21-159`

---

## 6. NDX (File Index) Encoding

**Reference:** upstream `io.c:2243-2318` vs `crates/protocol/src/codec/ndx.rs`

### NDX Constants

| Constant | Value | Upstream | Description |
|----------|-------|----------|-------------|
| NDX_DONE | -1 | rsync.h:285 | End of file requests |
| NDX_FLIST_EOF | -2 | rsync.h:286 | End of file list(s) |
| NDX_DEL_STATS | -3 | rsync.h:287 | Delete statistics marker |
| NDX_FLIST_OFFSET | -101 | rsync.h:288 | Incremental flist offset |

### Protocol Version Differences

| Aspect | Protocol < 30 | Protocol >= 30 |
|--------|---------------|----------------|
| Format | 4-byte LE signed int | Delta-encoded |
| State | Stateless | prev_positive, prev_negative |
| NDX_DONE | `[0xFF, 0xFF, 0xFF, 0xFF]` | `[0x00]` |
| Positive indices | 4 bytes always | 1-5 bytes delta |
| Negative indices | 4 bytes always | 0xFF prefix + delta |

### Modern Delta Encoding (Protocol 30+)

```text
Initial state: prev_positive = -1, prev_negative = 1

Encoding rules:
- NDX_DONE (-1): Single byte 0x00
- Positive: diff = ndx - prev_positive
  - diff in 1-253: single byte
  - diff in 0-32767: 3 bytes [0xFE, high, low]
  - larger: 5 bytes [0xFE, 0x80|high, b0, b1, b2]
- Negative (not -1): 0xFF prefix, then encode absolute value
```

**Source:** `crates/protocol/src/codec/ndx.rs:45-423`

---

## 7. Delta Transfer Algorithm

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

**Source:** `crates/match/src/index.rs:26-131`

---

## 8. Rolling Checksum

**Reference:** upstream `checksum.c` vs `crates/checksums/src/rolling/`

### Algorithm Comparison

| Aspect | Upstream | Rust | Status |
|--------|----------|------|--------|
| Base algorithm | Adler-32 variant | Adler-32 variant | ✅ |
| s1 accumulator | Sum of bytes | Sum of bytes | ✅ |
| s2 accumulator | Weighted prefix sum | Weighted prefix sum | ✅ |
| Modulus | 0xFFFF (truncation) | 0xFFFF (truncation) | ✅ |
| Final value | `(s2 << 16) \| s1` | `(s2 << 16) \| s1` | ✅ |
| CHAR_OFFSET | 0 (rsync.h:43) | 0 (not used) | ✅ |
| SIMD optimization | SSE2/AVX2 | AVX2/SSE2/NEON | ✅ |

### CHAR_OFFSET Verification

**Upstream rsync.h line 43:**
```c
/* a non-zero CHAR_OFFSET makes the rolling sum stronger, but is
 * incompatible with the original protocol */
#define CHAR_OFFSET 0
```

**Source:** `crates/checksums/src/rolling/checksum/mod.rs`

---

## 9. Strong Checksums

**Reference:** upstream `checksum.c` vs `crates/checksums/src/strong/`

| Algorithm | Upstream | Rust Crate | Default Protocol |
|-----------|----------|------------|------------------|
| MD4 | Default (proto < 30) | `md4` | ≤29 |
| MD5 | Default (proto 30+) | `md-5` | 30+ |
| XXH3-64 | Optional | `xxhash-rust` | Negotiated |
| XXH3-128 | Optional | `xxhash-rust` | Negotiated |

**Source:** `crates/checksums/src/strong/`

---

## 10. Checksum Negotiation and Seed

**Reference:** `crates/transfer/src/setup.rs`, `crates/protocol/src/negotiation/`

### Checksum Seed Format

- **Width:** 4 bytes little-endian signed i32
- **Direction:** Server → Client (unidirectional)
- **Generation:** `seed = timestamp ^ (pid << 6)`
- **Protocol:** All versions (28+)

### Algorithm Negotiation (Protocol 30+)

Both sides exchange supported algorithms using vstring format:

```text
Preference order: xxh128, xxh3, xxh64, md5, md4, sha1, none
```

### CF_CHKSUM_SEED_FIX Handling

| Mode | Seed Position | When Used |
|------|---------------|-----------|
| Legacy | After file data | Flag not set or protocol < 30 |
| Proper | Before file data | Flag set (protocol 30+) |

**Source:** `crates/transfer/src/setup.rs:446-469`, `crates/transfer/src/shared/checksum.rs:86-98`

---

## 11. Compression Token Encoding

**Reference:** upstream `token.c` vs `crates/protocol/src/wire/compressed_token.rs`

### Token Constants

| Constant | Value | Description |
|----------|-------|-------------|
| END_FLAG | 0x00 | End of file marker |
| TOKEN_LONG | 0x20 | 32-bit token follows |
| TOKENRUN_LONG | 0x21 | 32-bit token + 16-bit run |
| DEFLATED_DATA | 0x40 | Compressed data follows |
| TOKEN_REL | 0x80 | 6-bit relative token |
| TOKENRUN_REL | 0xC0 | 6-bit token + 16-bit run |

### DEFLATED_DATA Format

```text
Byte 0: 0x40 | (len >> 8)   // DEFLATED_DATA flag + upper 6 bits
Byte 1: len & 0xFF          // Lower 8 bits of length
[data]: compressed bytes    // Up to 16383 bytes (14-bit max)
```

**Source:** `crates/protocol/src/wire/compressed_token.rs`

---

## 12. Compression Algorithms

| Algorithm | Upstream | Rust Crate | Wire Format |
|-----------|----------|------------|-------------|
| zlib | Levels 1-9 | `flate2` | Raw deflate (windowBits=-15) |
| zstd | Dynamic level | `zstd` | Standard framed |
| LZ4 | Level 0 only | `lz4_flex` | Raw block |

**Source:** `crates/compress/src/`

---

## 13. ACL Wire Format

**Reference:** upstream `acls.c` vs `crates/protocol/src/acl/`

### Encoding Constants

| Constant | Value | Description |
|----------|-------|-------------|
| ACCESS_SHIFT | 2 | Permission bits shift |
| XFLAG_NAME_FOLLOWS | 0x0001 | Name string follows |
| XFLAG_NAME_IS_USER | 0x0002 | Entry is for user |
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

**Source:** `crates/protocol/src/acl/`

---

## 14. Xattr Wire Format

**Reference:** upstream `xattrs.c` vs `crates/protocol/src/xattr/`

### Wire Format

```text
ndx + 1    : varint        // 0 = literal data, >0 = cache index
If literal:
  count    : varint        // Number of xattr entries
  For each:
    name_len   : varint
    datum_len  : varint
    name       : bytes[name_len]
    If datum_len > 32:
      checksum : bytes[16] // MD5 hash
    Else:
      value    : bytes[datum_len]
```

### Namespace Prefix Handling

| Local Name | Wire Name | Condition |
|------------|-----------|-----------|
| `user.foo` | `foo` | Strip `user.` prefix |
| `system.foo` | `rsync.system.foo` | Disguise (root only) |
| `security.foo` | `rsync.security.foo` | Disguise (root only) |

**Source:** `crates/protocol/src/xattr/`

---

## 15. Filter Rules

**Reference:** upstream `exclude.c` vs `crates/filters/src/`

### Rule Prefixes

| Prefix | Description | Status |
|--------|-------------|--------|
| `+` | Include | ✅ |
| `-` | Exclude | ✅ |
| `H` | Hide | ✅ |
| `S` | Show | ✅ |
| `P` | Protect | ✅ |
| `R` | Risk | ✅ |
| `.` | Merge file | ✅ |
| `:` | Dir-merge | ✅ |
| `!` | Clear rules | ✅ |

### Modifier Flags

| Modifier | Description | Status |
|----------|-------------|--------|
| `!` | Negate match | ✅ |
| `p` | Perishable | ✅ |
| `s` | Sender-only | ✅ |
| `r` | Receiver-only | ✅ |
| `x` | Xattr-only | ✅ |
| `e` | Exclude-only | ✅ |
| `n` | No-inherit | ✅ |
| `w` | Word-split | ✅ |
| `C` | CVS-ignore | ✅ |

**Source:** `crates/filters/src/`

---

## 16. Delete Handling

**Reference:** `crates/transfer/src/generator.rs`, `crates/protocol/src/stats.rs`

### Delete Modes

| Mode | Flag | Behavior |
|------|------|----------|
| Before | `--delete-before` | Delete before transfer |
| During | `--delete` | Delete while processing |
| Delay | `--delete-delay` | Record, delete after |
| After | `--delete-after` | Delete after transfer |

### Wire Protocol

| Component | Format | Description |
|-----------|--------|-------------|
| MSG_DELETED | Message code 101 | Delete notification |
| NDX_DEL_STATS | NDX value -3 | Statistics marker |
| DeleteStats | 5 varints | files, dirs, symlinks, devices, specials |

**Source:** `crates/protocol/src/stats.rs:155-245`

---

## 17. Time Handling

**Reference:** `crates/protocol/src/flist/read.rs`, `crates/protocol/src/flist/write.rs`

### Time Fields

| Field | Encoding | Condition |
|-------|----------|-----------|
| mtime | varlong(4) | Always (unless XMIT_SAME_TIME) |
| nsec | varint | XMIT_MOD_NSEC (protocol 31+) |
| crtime | varlong(4) | --crtimes (unless XMIT_CRTIME_EQ_MTIME) |
| atime | varlong(4) | --atimes, non-dirs (unless XMIT_SAME_ATIME) |

### Wire Order (Metadata Fields)

1. mtime (if not same)
2. nsec (if protocol 31+ and set)
3. crtime (if preserving and not equal to mtime)
4. mode (if not same)
5. atime (if preserving, non-dir, not same)

**Source:** `crates/protocol/src/flist/read.rs:408-462`

---

## 18. Device and Special File Handling

**Reference:** `crates/protocol/src/flist/`, `crates/metadata/src/special.rs`

### Device Number Encoding

| Protocol | Major | Minor |
|----------|-------|-------|
| < 30 | varint30_int | 1 byte (if MINOR_8) or 4-byte LE |
| 30+ | varint30_int | varint |

### Special File Types

| Type | Mode Bits | Creation |
|------|-----------|----------|
| Block device | 0o060000 | `mknodat()` with S_IFBLK |
| Char device | 0o020000 | `mknodat()` with S_IFCHR |
| FIFO | 0o010000 | `mknodat()` with S_IFIFO or `mkfifo()` |
| Socket | 0o140000 | Not created (metadata only) |

### Protocol 31+ Change

Special files (FIFOs, sockets) no longer transmit dummy rdev in protocol 31+.

**Source:** `crates/metadata/src/special.rs:6-167`

---

## 19. Daemon Protocol

**Reference:** `crates/rsync_io/src/daemon/`, `crates/protocol/src/legacy/`

### Greeting Format

```text
@RSYNCD: <major>.<minor> [digest1 digest2 ...]
Example: @RSYNCD: 32.0 sha512 sha256 md5 md4
```

### Module Listing Protocol

```text
Client: #list\n
Server: @RSYNCD: MOTD Welcome message (optional)
Server: @RSYNCD: CAP 0x1f 0x2 (optional)
Server: @RSYNCD: OK
Server: module_name\tdescription
Server: @RSYNCD: EXIT
```

### Authentication Protocol

```text
Client: module_name\n
Server: @RSYNCD: AUTHREQD module_name <base64_challenge>
Client: username digest\n
Server: @RSYNCD: OK (success) or @ERROR: access denied (failure)
```

### Supported Digest Algorithms

| Digest | Base64 Length | Preference |
|--------|---------------|------------|
| SHA-512 | 86 bytes | Highest |
| SHA-256 | 43 bytes | High |
| SHA-1 | 27 bytes | Medium |
| MD5 | 22 bytes | Low |
| MD4 | 22 bytes | Lowest |

**Source:** `crates/core/src/auth/mod.rs:17-189`

---

## 20. Bandwidth Limiting

**Reference:** `crates/bandwidth/src/`

### Token Bucket Algorithm

```rust
struct BandwidthLimiter {
    limit_bytes: NonZeroU64,      // Rate in bytes/second
    write_max: usize,             // Max chunk before sleep
    burst_bytes: Option<NonZeroU64>,
    total_written: u128,          // Accumulated debt
}
```

### Algorithm

1. Each write adds to accumulated debt
2. Debt clamped to burst limit
3. Elapsed time repays debt: `allowed = elapsed_us * rate / 1_000_000`
4. Sleep calculated: `sleep_us = debt * 1_000_000 / rate`
5. Minimum sleep threshold: 100ms

### Parsing Format

```text
"8M"        → 8,388,608 bytes/sec
"1M:512K"   → rate=1,048,576, burst=524,288
"1.5m"      → 1,572,864 bytes/sec
"2048k+1"   → 2,097,153 bytes/sec
```

**Source:** `crates/bandwidth/src/limiter/core.rs:30-190`

---

## 21. Sparse File Handling

**Reference:** `crates/transfer/src/delta_apply.rs`

### Detection Algorithm

- Chunk-based processing (1024-byte chunks)
- Leading and trailing zero detection per chunk
- Zero runs accumulated, flushed as seeks

### Implementation

```rust
struct SparseWriteState {
    pending_zeros: u64,  // Accumulated zeros
}
```

- **Hole creation:** `Seek::seek(SeekFrom::Current(n))`
- **Final byte:** Always writes one byte to extend file size
- **Disabled when:** `--inplace` or `--preallocate` active

**Source:** `crates/transfer/src/delta_apply.rs:28-131`

---

## 22. Backup Handling

**Reference:** `crates/engine/src/local_copy/executor/file/backup.rs`

### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--backup` | off | Enable backups |
| `--backup-dir` | none | Separate backup directory |
| `--suffix` | `~` | Backup file suffix |

### Path Resolution

| Mode | Example |
|------|---------|
| No backup-dir | `/dest/file.txt` → `/dest/file.txt~` |
| Relative backup-dir | `--backup-dir backups` → `/dest/backups/file.txt~` |
| Absolute backup-dir | `--backup-dir /var/backups` → `/var/backups/file.txt~` |

**Source:** `crates/engine/src/local_copy/executor/file/backup.rs:8-78`

---

## 23. Error and Exit Codes

**Reference:** `crates/core/src/exit_code.rs`, `crates/transfer/src/generator.rs`

### Exit Codes (RERR_*)

| Code | Constant | Description |
|------|----------|-------------|
| 0 | RERR_OK | Success |
| 1 | RERR_SYNTAX | Syntax/usage error |
| 2 | RERR_PROTOCOL | Protocol incompatibility |
| 3 | RERR_FILESELECT | File selection error |
| 4 | RERR_UNSUPPORTED | Unsupported action |
| 5 | RERR_STARTCLIENT | Client-server startup error |
| 10 | RERR_SOCKETIO | Socket I/O error |
| 11 | RERR_FILEIO | File I/O error |
| 12 | RERR_STREAMIO | Stream I/O error |
| 13 | RERR_MESSAGEIO | Message I/O error |
| 14 | RERR_IPC | IPC error |
| 15 | RERR_CRASHED | Sibling crashed |
| 16 | RERR_TERMINATED | Sibling terminated |
| 20 | RERR_SIGNAL | Received signal |
| 22 | RERR_MALLOC | Memory allocation error |
| 23 | RERR_PARTIAL | Partial transfer |
| 24 | RERR_VANISHED | Files vanished |
| 25 | RERR_DEL_LIMIT | Delete limit exceeded |
| 30 | RERR_TIMEOUT | Timeout |
| 35 | RERR_CONTIMEOUT | Connection timeout |

### I/O Error Flags (IOERR_*)

| Flag | Bit | Description |
|------|-----|-------------|
| IOERR_GENERAL | 1 | General I/O error |
| IOERR_VANISHED | 2 | File vanished |
| IOERR_DEL_LIMIT | 4 | Delete limit exceeded |

**Source:** `crates/core/src/exit_code.rs:36-154`, `crates/transfer/src/generator.rs:69-73`

---

## 24. Incremental Recursion

**Reference:** `crates/protocol/src/compatibility/flags.rs`, `crates/transfer/src/generator.rs`

### Status

| Component | Status | Notes |
|-----------|--------|-------|
| CF_INC_RECURSE flag | ✅ Defined | Bit 0 |
| NDX_FLIST_OFFSET | ✅ Defined | -101 |
| Flag negotiation | ✅ Implemented | Exchanged but gated |
| Directory queue | ❌ Not implemented | Requires segmented flist |
| Segmented transmission | ❌ Not implemented | Single flist only |

**Current behavior:** `allow_inc_recurse=false` gates the feature. File lists are sent as a single batch.

**Source:** `crates/transfer/src/setup.rs` (search for `allow_inc_recurse`)

---

## 25. Test Coverage

### Module Test Counts

| Module | Tests | Coverage Notes |
|--------|-------|----------------|
| protocol/varint | 50+ | Boundary conditions, roundtrips |
| protocol/flist | 57+ | Encoding variants, flags |
| protocol/compressed_token | 45+ | Token encoding, see_token, errors |
| protocol/envelope | 40+ | Message codes, framing |
| protocol/compatibility | 60+ | Flag combinations |
| protocol/acl | 26+ | Encoding, caching, errors |
| filters/merge | 30+ | Parsing, modifiers, recursion |
| checksums/rolling | 40+ | SIMD, rolling, properties |
| checksums/strong | 20+ | Algorithm correctness |
| metadata/apply | 42+ | Permissions, ownership |
| match/index | 15+ | Signature lookup |
| transfer/receiver | 30+ | Delta application |
| compress/lz4 | 32+ | Compression, edge cases |

**Total workspace tests:** 10,285 passing

---

## Summary

The Rust rsync implementation achieves **full wire protocol compatibility** with upstream rsync 3.4.1:

1. **Protocol versions 28-32** fully supported with version-specific feature gates
2. **All compatibility flags** match upstream bit positions
3. **All 18 message codes** match upstream numeric values
4. **NDX encoding** supports both legacy and modern formats
5. **Varint encoding** uses identical INT_BYTE_EXTRA lookup table
6. **Compression tokens** match END_FLAG, TOKEN_*, DEFLATED_DATA
7. **Filter rules** support all prefixes including `:` for dir-merge
8. **ACL/Xattr encoding** matches ACCESS_SHIFT, namespace handling
9. **File list encoding** is byte-compatible including context-dependent flags
10. **Strong checksums** produce identical output (MD4, MD5, XXH3)
11. **Compression** uses correct wire formats (raw deflate, not framed)
12. **Daemon protocol** supports authentication and module listing
13. **All exit codes** match upstream RERR_* values

### Implementation Differences (Non-Protocol)

| Difference | Impact | Reason |
|-----------|--------|--------|
| SIMD optimization | Performance only | Identical results |
| Pure Rust crypto | No C dependencies | RustCrypto ecosystem |
| Incremental recursion | Not enabled | Gate set to false |

### Why Custom Implementations?

1. **Rolling checksum:** rsync's Adler-32 variant with specific bit layout
2. **Varint encoding:** rsync's INT_BYTE_EXTRA lookup table
3. **Protocol flags:** Context-dependent bit meanings
4. **ACL handling:** rsync-specific synchronization semantics
5. **Filter rules:** rsync syntax differs from gitignore

These custom implementations are necessary for byte-level wire protocol compatibility with upstream rsync 3.4.1.

---

## Appendix: Upstream Source References

### Varint Encoding (io.c)

| Function | Upstream Lines |
|----------|---------------|
| `int_byte_extra[]` | io.c:120-125 |
| `read_varint()` | io.c:1795-1825 |
| `write_varint()` | io.c:2089-2109 |
| `read_ndx()` | io.c:2289-2318 |
| `write_ndx()` | io.c:2243-2287 |

### Message Codes (rsync.h)

| Code | Upstream Lines |
|------|---------------|
| MSG_DATA through MSG_NO_SEND | rsync.h:264-278 |
| MPLEX_BASE | rsync.h:180 |

### XMIT Flags (rsync.h)

| Flag | Upstream Lines |
|------|---------------|
| XMIT_TOP_DIR through XMIT_SAME_TIME | rsync.h:47-55 |
| XMIT_SAME_RDEV_MAJOR through XMIT_CRTIME_EQ_MTIME | rsync.h:57-73 |

### Exit Codes (errcode.h)

| Code | Upstream |
|------|----------|
| RERR_OK through RERR_CONTIMEOUT | errcode.h:28-54 |

---

**Verification methodology:** Each constant and algorithm was verified by reading upstream rsync 3.4.1 source code and comparing against the corresponding Rust implementation.
