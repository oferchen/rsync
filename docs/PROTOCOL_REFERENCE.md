# Rsync Protocol Reference

This document provides a comprehensive technical reference for the rsync protocol,
covering wire formats, algorithms, and implementation details. It serves as the
authoritative specification for oc-rsync's protocol implementation.

---

## Table of Contents

1. [Protocol Version Negotiation](#1-protocol-version-negotiation)
2. [Compatibility Flags](#2-compatibility-flags)
3. [File List Wire Format](#3-file-list-wire-format)
4. [Delta Transfer Algorithm](#4-delta-transfer-algorithm)
5. [Checksum Negotiation](#5-checksum-negotiation)
6. [Multiplexed I/O Protocol](#6-multiplexed-io-protocol)
7. [Filter Rule Wire Format](#7-filter-rule-wire-format)
8. [Batch Mode Format](#8-batch-mode-format)
9. [Daemon Protocol](#9-daemon-protocol)
10. [Statistics Wire Format](#10-statistics-wire-format)
11. [Incremental Recursion](#11-incremental-recursion)
12. [Delete Modes Wire Format](#12-delete-modes-wire-format)
13. [Hard Link Wire Format](#13-hard-link-wire-format)
14. [Device/Special File Wire Format](#14-devicespecial-file-wire-format)
15. [Symlink Wire Format](#15-symlink-wire-format)
16. [ACL Wire Format](#16-acl-wire-format)
17. [Extended Attributes Wire Format](#17-extended-attributes-wire-format)
18. [File Attribute Preservation](#18-file-attribute-preservation)
19. [Compression Wire Format](#19-compression-wire-format)
20. [I/O Timeout Protocol](#20-io-timeout-protocol)
21. [Partial Transfer Protocol](#21-partial-transfer-protocol)
22. [Itemize Output Format](#22-itemize-output-format)
23. [Checksum Transfer Protocol](#23-checksum-transfer-protocol)
24. [Backup Wire Format](#24-backup-wire-format)
25. [Update/Skip Rules](#25-updateskip-rules)
26. [Safe File Writing](#26-safe-file-writing)
27. [Fuzzy Matching Algorithm](#27-fuzzy-matching-algorithm)
28. [I/O Buffer Management](#28-io-buffer-management)
29. [Sparse File Handling](#29-sparse-file-handling)
30. [Rolling Checksum Mathematics](#30-rolling-checksum-mathematics)
31. [Progress Output Format](#31-progress-output-format)
32. [Checksum Seed Protocol](#32-checksum-seed-protocol)
33. [File Ordering and Priority](#33-file-ordering-and-priority)
34. [Module Listing Protocol](#34-module-listing-protocol)
35. [Error Recovery Mechanisms](#35-error-recovery-mechanisms)
36. [Temp File Naming Convention](#36-temp-file-naming-convention)
37. [Symlink Safety Options](#37-symlink-safety-options)
38. [Delay Updates Protocol](#38-delay-updates-protocol)
39. [Fake Super Mode](#39-fake-super-mode)
40. [Character Encoding](#40-character-encoding)
41. [Daemon Exec Hooks](#41-daemon-exec-hooks)
42. [Remote Binary Options](#42-remote-binary-options)
43. [Implied Directories](#43-implied-directories)
44. [UID/GID Mapping](#44-uidgid-mapping)
45. [Daemon Socket Options](#45-daemon-socket-options)
46. [Whole File Transfer](#46-whole-file-transfer)
47. [Append Mode Protocol](#47-append-mode-protocol)
48. [Copy Dest/Link Dest Options](#48-copy-destlink-dest-options)
49. [Max/Min Size Filtering](#49-maxmin-size-filtering)
50. [Modify Window Comparison](#50-modify-window-comparison)

---

## 1. Protocol Version Negotiation

### 1.1 Version Exchange Sequence

The protocol version exchange occurs immediately after connection establishment.
Both sides send their maximum supported protocol version as a 4-byte little-endian
integer.

**Wire format:**

```
Client → Server: [protocol_version: u32 LE]
Server → Client: [protocol_version: u32 LE]
```

**Sequence:**

1. Client sends its maximum protocol version (e.g., 31)
2. Server receives and sends its maximum protocol version
3. Both sides use `min(client_version, server_version)` as the negotiated version
4. If negotiated version < 20, connection is rejected

**Example exchange (protocol 31):**

```
Client sends: 1f 00 00 00  (31 as LE u32)
Server sends: 1f 00 00 00  (31 as LE u32)
Negotiated: 31
```

### 1.2 Protocol Version Constants

| Version | Introduced | Key Features |
|---------|------------|--------------|
| 20 | rsync 2.3.0 | Minimum supported version |
| 21 | rsync 2.4.0 | 64-bit file sizes |
| 25 | rsync 2.5.0 | --delete-during |
| 26 | rsync 2.5.4 | --compress option |
| 27 | rsync 2.6.0 | --checksum-seed |
| 28 | rsync 2.6.4 | --filter option |
| 29 | rsync 2.6.9 | Incremental recursion |
| 30 | rsync 3.0.0 | Improved incremental, varint |
| 31 | rsync 3.1.0 | Checksum negotiation, iconv |
| 32 | rsync 3.2.0 | Zstd compression |

### 1.3 Version Downgrade Rules

When peers have different maximum versions, the lower version is used:

```rust
fn negotiate_version(local: u32, remote: u32) -> Result<u32, Error> {
    let negotiated = std::cmp::min(local, remote);
    if negotiated < PROTOCOL_MIN_VERSION {
        return Err(Error::ProtocolTooOld(negotiated));
    }
    Ok(negotiated)
}
```

**Constraints:**

- Protocol < 20: Connection refused
- Protocol 20-28: Legacy mode (no incremental recursion)
- Protocol 29+: Modern mode with incremental recursion support
- Protocol 30+: Varint encoding for file list

### 1.4 Sub-Protocol Version

Protocol 30+ includes a sub-protocol version for minor feature negotiation:

```
After main version exchange:
[sub_protocol: u8]  // Only if protocol >= 30
```

Sub-protocol values:
- 0: Base protocol 30 features
- 1: Enhanced checksum negotiation
- 2: Extended xattr handling

---

## 2. Compatibility Flags

### 2.1 Compat Flags Bitmask

After version negotiation, protocol 30+ exchanges an 8-bit compatibility flags
byte to enable/disable specific features:

```
[compat_flags: u8]
```

The sender transmits its capabilities; the receiver applies the intersection.

### 2.2 CF_INC_RECURSE (0x01)

**Purpose:** Enables incremental recursion for large directory trees.

**Behavior:**
- When set: File list is sent incrementally as directories are descended
- When clear: Complete file list sent before transfer begins
- Requires protocol >= 30

**Wire impact:**
- Multiple flist segments with `flist_eof` markers
- Directory entries trigger new segment generation

### 2.3 CF_SYMLINK_TIMES (0x02)

**Purpose:** Preserve modification times on symbolic links.

**Behavior:**
- When set: Symlink mtime included in file list entries
- When clear: Symlink mtime not transferred
- Requires `--links` or `-l` option

**Wire format addition:**
```
// After symlink target, if CF_SYMLINK_TIMES set:
[symlink_mtime: i32 LE]  // Seconds since epoch
```

### 2.4 CF_SYMLINK_ICONV (0x04)

**Purpose:** Apply character encoding conversion to symlink targets.

**Behavior:**
- When set: Symlink targets converted using `--iconv` setting
- When clear: Symlink targets transferred as raw bytes
- Only meaningful with `--iconv` option

### 2.5 CF_SAFE_FLIST (0x08)

**Purpose:** Use stricter validation for file list entries.

**Behavior:**
- When set: Reject file list entries with potentially dangerous paths
- Validates: No absolute paths, no `..` components, no leading slashes
- Provides defense against path traversal attacks

### 2.6 CF_AVOID_XATTR_OPTIM (0x10)

**Purpose:** Disable xattr transfer optimization.

**Behavior:**
- When set: Always send full xattr data (no deduplication)
- When clear: Use xattr index references for repeated values
- Used when receiver cannot handle xattr indices

### 2.7 CF_CHKSUM_SEED_ORDER (0x20)

**Purpose:** Sender transmits checksum seed before receiver.

**Behavior:**
- Protocol 30+: Sender sends seed first
- Protocol < 30: Receiver sends seed first
- Ensures deterministic seed exchange order

### 2.8 CF_INPLACE_PARTIAL (0x40)

**Purpose:** Combined `--inplace` and `--partial` handling.

**Behavior:**
- When set: Partial files updated in-place without temp files
- Enables resume of interrupted transfers with `--inplace`
- Affects temp file creation strategy

### 2.9 CF_VARINT_FLIST_FLAGS (0x80)

**Purpose:** Use variable-length encoding for file list flags.

**Behavior:**
- When set: XMIT_* flags encoded as varints
- When clear: Fixed 1-2 byte flag encoding
- Reduces wire size for simple file entries

---

## 3. File List Wire Format

### 3.1 XMIT_* Transmit Flags

Each file list entry begins with transmit flags indicating which fields follow:

**Primary flags (1 byte, or 2 bytes if XMIT_EXTENDED_FLAGS):**

| Flag | Value | Meaning |
|------|-------|---------|
| XMIT_TOP_DIR | 0x01 | Top-level directory marker |
| XMIT_SAME_MODE | 0x02 | Mode same as previous entry |
| XMIT_EXTENDED_FLAGS | 0x04 | Second flag byte follows |
| XMIT_SAME_UID | 0x08 | UID same as previous entry |
| XMIT_SAME_GID | 0x10 | GID same as previous entry |
| XMIT_SAME_NAME | 0x20 | Partial name inheritance |
| XMIT_LONG_NAME | 0x40 | Name length > 255 |
| XMIT_SAME_TIME | 0x80 | Mtime same as previous |

**Extended flags (second byte if XMIT_EXTENDED_FLAGS):**

| Flag | Value | Meaning |
|------|-------|---------|
| XMIT_SAME_RDEV_MAJOR | 0x01 | Device major same as previous |
| XMIT_HLINKED | 0x02 | Hard link to previous entry |
| XMIT_HLINK_FIRST | 0x04 | First file in hard link group |
| XMIT_IO_ERROR_ENDLIST | 0x08 | I/O error during traversal |
| XMIT_MOD_NSEC | 0x10 | Nanosecond mtime follows |
| XMIT_SAME_ATIME | 0x20 | Atime same as previous |
| XMIT_UNUSED_2 | 0x40 | Reserved |
| XMIT_UNUSED_3 | 0x80 | Reserved |

### 3.2 File Entry Structure

Each file entry follows this structure (fields conditional on flags):

```
[xmit_flags: u8 or u16]
[inherit_name_len: u8]        // If XMIT_SAME_NAME
[name_len: u8 or varint]      // Depends on XMIT_LONG_NAME
[name: bytes]                  // Only non-inherited portion
[file_size: varint or i64]    // Encoding depends on protocol
[mtime: i32 or i64]           // If not XMIT_SAME_TIME
[mode: u32]                   // If not XMIT_SAME_MODE
[uid: varint]                 // If not XMIT_SAME_UID and -o
[gid: varint]                 // If not XMIT_SAME_GID and -g
[rdev: varies]                // If device file
[symlink_target: bytes]       // If symlink
[checksum: bytes]             // If --checksum
```

### 3.3 Path Encoding with Name Inheritance

To reduce bandwidth, rsync uses path prefix inheritance:

**Example:**
```
Entry 1: "src/main.rs"       → Full name sent
Entry 2: "src/lib.rs"        → inherit=4, send "lib.rs" (inherits "src/")
Entry 3: "src/utils/mod.rs"  → inherit=4, send "utils/mod.rs"
```

**Wire encoding:**
```
// For entry with XMIT_SAME_NAME:
[inherit_len: u8]   // Bytes to inherit from previous name
[new_len: u8]       // Length of new suffix
[new_bytes: ...]    // The suffix bytes
```

### 3.4 File Size Encoding

File sizes use different encodings based on protocol version:

**Protocol < 30:**
```
[size_low: i32 LE]
// If size > 0x7FFFFFFF:
[size_high: i32 LE]
```

**Protocol 30+:**
```
[size: varint]  // Variable-length integer encoding
```

**Varint encoding:**
```rust
fn write_varint(mut value: u64, writer: &mut impl Write) -> io::Result<()> {
    while value >= 0x80 {
        writer.write_all(&[(value as u8) | 0x80])?;
        value >>= 7;
    }
    writer.write_all(&[value as u8])
}
```

### 3.5 Mtime Encoding

Modification time encoding varies by protocol:

**Protocol < 30:**
```
[mtime: i32 LE]  // Seconds since Unix epoch
```

**Protocol 30+ with XMIT_MOD_NSEC:**
```
[mtime_sec: varint]   // Seconds component
[mtime_nsec: varint]  // Nanoseconds component (0-999999999)
```

**Delta encoding (when not XMIT_SAME_TIME):**
```
// If |delta| < 0x7FFFFFFF, send as i32
// Otherwise, send full i64
```

### 3.6 Mode/Permissions Encoding

File mode (type + permissions) uses a 32-bit encoding:

```
[mode: u32 LE]

// Mode bits:
// S_IFMT   0xF000  File type mask
// S_IFREG  0x8000  Regular file
// S_IFDIR  0x4000  Directory
// S_IFLNK  0xA000  Symbolic link
// S_IFBLK  0x6000  Block device
// S_IFCHR  0x2000  Character device
// S_IFIFO  0x1000  FIFO
// S_IFSOCK 0xC000  Socket

// Permission bits (lower 12 bits):
// 0o7777 = setuid + setgid + sticky + rwxrwxrwx
```

### 3.7 UID/GID Encoding

User and group IDs use protocol-dependent encoding:

**Without `--numeric-ids`:**
```
// First occurrence of uid/gid:
[id: varint]
[name_len: u8]
[name: bytes]

// Subsequent with same id: just [id: varint]
```

**With `--numeric-ids`:**
```
[id: varint]  // No name mapping
```

### 3.8 Symlink Target Encoding

Symbolic link targets follow the name immediately:

```
// If S_IFLNK mode:
[target_len: varint]
[target: bytes]  // Raw symlink target path
```

### 3.9 Device Number Encoding

Block and character devices encode major/minor numbers:

**Protocol < 30:**
```
[rdev: u32 LE]  // Combined major/minor
```

**Protocol 30+:**
```
[rdev_major: varint]  // Unless XMIT_SAME_RDEV_MAJOR
[rdev_minor: varint]
```

### 3.10 File List Termination

The file list ends with a null flags byte:

```
[0x00]  // Signals end of file list
```

For incremental recursion (protocol 29+), intermediate `flist_eof` markers
appear between directory segments:

```
[NDX_FLIST_EOF: i32 = -1]  // End of current segment
// ... more entries in next segment ...
[0x00]  // Final termination
```

---

## 4. Delta Transfer Algorithm

### 4.1 Overview and Phases

The delta transfer uses a rolling checksum algorithm to identify matching
blocks between source and basis files, minimizing data transfer.

**Phases:**

1. **Signature Generation**: Receiver computes block checksums of basis file
2. **Signature Transfer**: Checksums sent to sender
3. **Delta Generation**: Sender finds matches, emits copy/literal tokens
4. **Delta Application**: Receiver reconstructs file from tokens

### 4.2 Signature Request/Response

When the sender needs to transfer a file that may match a basis on receiver:

**Sender → Receiver: File index request**
```
[file_ndx: i32]  // Index in file list
```

**Receiver → Sender: Signature data**
```
[sum_head: SumHead]         // Block parameters
[block_checksums: ...]      // Per-block checksums
```

### 4.3 Sum Head Structure

The signature header defines block parameters:

```
struct SumHead {
    count: i32,        // Number of blocks
    blength: i32,      // Block length in bytes
    s2length: i32,     // Strong checksum length (2-16)
    remainder: i32,    // Last block size (if < blength)
}
```

**Wire format:**
```
[count: i32 LE]
[blength: i32 LE]
[s2length: i32 LE]
[remainder: i32 LE]
```

**Block length calculation:**
```rust
fn calculate_block_length(file_size: u64) -> u32 {
    // Target ~1000 blocks, minimum 700 bytes, maximum 128KB
    let target_blocks = 1000;
    let block_len = (file_size / target_blocks) as u32;
    block_len.clamp(700, 131072)
}
```

### 4.4 Block Checksum List

For each block, two checksums are transmitted:

```
// For each of `count` blocks:
[rolling_checksum: u32 LE]    // Fast rolling checksum
[strong_checksum: bytes]      // Slow strong checksum (s2length bytes)
```

**Rolling checksum**: Adler32-like rolling hash (see Section 30)
**Strong checksum**: MD4/MD5/XXH depending on negotiation (see Section 5)

### 4.5 Delta Token Types

Delta data consists of three token types:

| Type | Encoding | Meaning |
|------|----------|---------|
| DATA | Positive length | Literal data follows |
| COPY | Negative token | Copy from basis file |
| END  | Zero | End of file delta |

### 4.6 Literal Data Encoding

Literal (non-matching) data is sent with length prefix:

**Short data (len < 0x7F):**
```
[len: u8]           // Length with high bit clear
[data: bytes]       // Literal bytes
```

**Long data (len >= 0x7F):**
```
[0x80 | (len >> 24): u8]   // Length high bits
[len & 0xFFFFFF: u24 LE]   // Length low bits
[data: bytes]               // Literal bytes
```

### 4.7 Copy Token Encoding

Copy tokens reference blocks in the basis file:

**Short match (block index < 64):**
```
[(-(block_idx + 1)): i8]  // Negative 1-byte token
```

**Long match:**
```
[0x80: u8]                    // Long match marker
[block_idx: i32 LE]           // Block index
[offset: i32 LE]              // Byte offset in file
[length: i32 LE]              // Bytes to copy
```

**Optimized encoding for sequential matches:**
```
// If match is next sequential block:
[MATCH_NEXT_BLOCK: i8 = -128]

// If match is same block:
[MATCH_SAME_BLOCK: i8 = -127]
```

### 4.8 Whole-File Checksum

After all delta tokens, a whole-file checksum verifies integrity:

```
[file_checksum: bytes]  // Length = s2length (from sum_head)
```

The receiver computes the checksum while applying deltas and compares:

```rust
fn verify_file(expected: &[u8], computed: &[u8]) -> Result<(), Error> {
    if expected != computed {
        Err(Error::ChecksumMismatch)
    } else {
        Ok(())
    }
}
```

---

## 5. Checksum Negotiation

### 5.1 Checksum Negotiation Exchange

Protocol 31+ negotiates checksum algorithms:

**Sender → Receiver:**
```
[checksum_list: NUL-terminated string]
// Example: "xxh3 xxh64 md5 md4\0"
```

**Receiver → Sender:**
```
[selected_checksum: NUL-terminated string]
// Example: "xxh3\0"
```

Priority order (strongest to weakest): xxh3, xxh64, md5, md4

### 5.2 MD4 Implementation

MD4 is the legacy checksum (protocol < 30):

- Block size: 64 bytes
- Output size: 16 bytes (128 bits)
- Truncated to `s2length` bytes (default: 2-16)

```rust
// MD4 is used for:
// - Strong block checksums (basis matching)
// - Whole-file verification
// - Seed-based variants for security
```

### 5.3 MD5 Implementation

MD5 provides improved collision resistance:

- Block size: 64 bytes
- Output size: 16 bytes (128 bits)
- Available in protocol 27+

### 5.4 XXH64 Implementation

XXH64 offers high-speed hashing:

- Output size: 8 bytes (64 bits)
- Significantly faster than MD4/MD5
- Available in protocol 31+

### 5.5 XXH3 Implementation

XXH3 is the newest, fastest option:

- Output size: 8 bytes (64 bits) or 16 bytes (128 bits)
- Optimized for modern CPUs (AVX2, NEON)
- Available in protocol 32+

### 5.6 Checksum Length by Protocol

| Protocol | Default | Max | Algorithms |
|----------|---------|-----|------------|
| 20-26 | 2 | 16 | MD4 |
| 27-30 | 2 | 16 | MD4, MD5 |
| 31 | 2 | 16 | MD4, MD5, XXH64 |
| 32+ | 2 | 16 | MD4, MD5, XXH64, XXH3 |

---

## 6. Multiplexed I/O Protocol

### 6.1 Multiplex Header Format

Multiplexed messages use a 4-byte header:

```
[tag: u8]              // Message type (7 bits) + continuation
[length: u24 LE]       // Payload length (max 16MB)
```

**Encoding:**
```rust
fn encode_header(tag: u8, length: usize) -> [u8; 4] {
    let len = length as u32;
    [
        (tag << 5) | ((len >> 16) as u8 & 0x1F),
        (len >> 8) as u8,
        len as u8,
        0  // Reserved
    ]
}
```

### 6.2 MSG_DATA (Tag 0)

Raw file data without multiplexing:

```
[MSG_DATA header]
[payload: bytes]
```

Used for: Delta tokens, file content, bulk transfers

### 6.3 MSG_INFO (Tag 1)

Informational messages for display:

```
[MSG_INFO header]
[message: bytes]  // UTF-8 text
```

Used for: Progress updates, file names, statistics

### 6.4 MSG_ERROR Tags

**MSG_ERROR (Tag 2)**: Non-fatal errors
```
[MSG_ERROR header]
[error_message: bytes]
```

**MSG_ERROR_XFER (Tag 3)**: File transfer errors
```
[MSG_ERROR_XFER header]
[file_index: i32]
[error_message: bytes]
```

**MSG_WARNING (Tag 4)**: Warnings
```
[MSG_WARNING header]
[warning_message: bytes]
```

### 6.5 MSG_LOG (Tag 5)

Log messages for `--log-file`:

```
[MSG_LOG header]
[log_level: u8]
[message: bytes]
```

### 6.6 MSG_IO_ERROR (Tag 22)

I/O error notification with flags:

```
[MSG_IO_ERROR header]
[error_flags: i32]
```

Flags indicate: partial transfer, redo needed, fatal error

### 6.7 MSG_NOOP (Tag 42)

Keep-alive message for idle connections:

```
[MSG_NOOP header]
// No payload
```

Sent every `--timeout/2` seconds to prevent connection drops.

### 6.8 MSG_DELETED (Tag 101)

File deletion notification:

```
[MSG_DELETED header]
[deleted_path: bytes]  // Path relative to transfer root
```

### 6.9 MSG_SUCCESS (Tag 100)

Successful file transfer confirmation:

```
[MSG_SUCCESS header]
[file_ndx: i32]  // Index of completed file
```

### 6.10 MSG_NO_SEND (Tag 102)

Skip file transfer (up-to-date):

```
[MSG_NO_SEND header]
[file_ndx: i32]  // Index to skip
```

### 6.11 Multiplex Activation

Multiplexing activates after initial handshake:

**Protocol < 30:**
- Activated after filter list exchange
- Both directions multiplexed simultaneously

**Protocol 30+:**
- Sender activates first (after compat flags)
- Receiver activates when ready to send data

```rust
// Activation sequence
fn activate_multiplex(protocol: u32, role: Role) {
    if protocol >= 30 {
        match role {
            Role::Sender => { /* Activate after sending compat flags */ }
            Role::Receiver => { /* Activate after receiving file list */ }
        }
    } else {
        // Activate after filter list for both roles
    }
}
```

---

## 7. Filter Rule Wire Format

### 7.1 Filter Prefix Characters

Filter rules use single-character prefixes:

| Prefix | Meaning |
|--------|---------|
| `-` | Exclude pattern |
| `+` | Include pattern |
| `.` | Merge file (per-directory) |
| `:` | Merge file (once) |
| `H` | Hide (affects sender) |
| `S` | Show (affects sender) |
| `P` | Protect (affects receiver) |
| `R` | Risk (affects receiver) |
| `!` | Clear all rules |

### 7.2 Filter Modifiers

Modifiers follow the prefix character:

| Modifier | Meaning |
|----------|---------|
| `/` | Pattern anchored to root |
| `!` | Negate pattern |
| `C` | CVS-style ignore |
| `s` | Sender-side rule |
| `r` | Receiver-side rule |
| `p` | Perishable (auto-cleanup) |
| `x` | Xattr filtering |

### 7.3 Filter Encoding on Wire

Filter rules are sent as length-prefixed strings:

```
[rule_count: i32]
// For each rule:
[rule_len: i32]
[rule_text: bytes]  // Includes prefix and modifiers
```

**Example:**
```
Rule: "- *.log"
Wire: 00 00 00 07  2d 20 2a 2e 6c 6f 67
      (length=7)   (-   *.log)
```

### 7.4 Merge File Handling

Merge directives load rules from files:

```
// Per-directory merge (re-read in each dir):
". .rsync-filter"

// One-time merge:
": /etc/rsync-rules"
```

**Wire format for merge:**
```
[RULE_MERGE: u8]
[path_len: i32]
[path: bytes]
```

### 7.5 Filter List Termination

Filter list ends with zero-length entry:

```
[0: i32]  // Zero length terminates list
```

---

## 8. Batch Mode Format

### 8.1 Batch File Header

Batch files store transfers for later replay:

```
// Header:
[magic: u32 = 0x52534E42]  // "RSNB"
[protocol_version: u32]
[checksum_seed: i32]
[file_count: i32]
```

### 8.2 Batch Operation Flow

**Creation (`--write-batch`):**
1. Perform normal transfer
2. Write delta operations to batch file
3. Write file list to `.sh` script

**Replay (`--read-batch`):**
1. Load batch file header
2. Apply delta operations without network

### 8.3 Batch Delta Storage

Each file's delta stored sequentially:

```
// Per-file in batch:
[file_ndx: i32]
[delta_len: i32]
[delta_tokens: bytes]
[file_checksum: bytes]
```

### 8.4 Batch Application

Batch replay uses local-only operations:

```bash
# Generated .sh file:
rsync --read-batch=batch_file /destination/
```

---

## 9. Daemon Protocol

### 9.1 @RSYNCD Greeting

Daemon connections begin with a greeting:

**Server → Client:**
```
@RSYNCD: <version>\n
```

**Example:**
```
@RSYNCD: 31.0\n
```

The version indicates maximum protocol supported.

### 9.2 Module Listing

Empty module name requests listing:

**Client → Server:**
```
\n  // Empty line requests module list
```

**Server → Client:**
```
module1    Description of module1\n
module2    Description of module2\n
@RSYNCD: EXIT\n
```

### 9.3 Module Selection

Client selects a module:

**Client → Server:**
```
modulename\n
```

**Server → Client (success):**
```
@RSYNCD: OK\n
```

**Server → Client (auth required):**
```
@RSYNCD: AUTHREQD <challenge>\n
```

### 9.4 Daemon Authentication

Challenge-response authentication:

**Challenge format:**
```
@RSYNCD: AUTHREQD <base64_challenge>\n
```

**Response format:**
```
<username> <base64_response>\n
```

**Response computation:**
```rust
fn compute_response(password: &str, challenge: &[u8]) -> String {
    let hash = md4(challenge, password.as_bytes());
    base64_encode(&hash)
}
```

### 9.5 Daemon Argument Passing

After authentication, client sends arguments:

```
--sender\n
--recursive\n
--verbose\n
path/to/files\n
\n  // Empty line ends arguments
```

### 9.6 Daemon Error Responses

Errors use `@ERROR` prefix:

```
@ERROR: <message>\n
// or
@ERROR: <code> <message>\n
```

**Common codes:**
- 0: Unknown error
- 1: Access denied
- 2: Module not found
- 3: Max connections reached

### 9.7 Server Modes

Two operational modes based on arguments:

**Server-Sender** (`--sender` present):
- Server sends files to client
- Client receives and writes files

**Server-Receiver** (no `--sender`):
- Client sends files to server
- Server receives and writes files

---

## 10. Statistics Wire Format

### 10.1 Statistics Structure

Transfer statistics exchanged at completion:

```
struct Stats {
    total_read: i64,
    total_written: i64,
    total_size: i64,
    flist_buildtime: i64,
    flist_xfertime: i64,
    num_files: i32,
    num_transferred_files: i32,
}
```

### 10.2 Read/Write Encoding

**Protocol < 30:**
```
[total_read: i64 LE]
[total_written: i64 LE]
```

**Protocol 30+:**
```
[total_read: varint]
[total_written: varint]
```

### 10.3 Size/Count Encoding

```
[total_size: varint]
[num_files: varint]
[num_transferred: varint]
```

### 10.4 Timing Encoding

Build/transfer times in microseconds:

```
[flist_buildtime: varint]
[flist_xfertime: varint]
```

---

## 11. Incremental Recursion

### 11.1 Overview

Protocol 29+ supports incremental file list transmission:

- File list sent as directories are entered
- Reduces memory for large transfers
- Requires CF_INC_RECURSE compat flag

### 11.2 Flist EOF Marker

Segments separated by EOF marker:

```
[NDX_FLIST_EOF: i32 = -1]
```

This signals end of current directory's entries.

### 11.3 Directory Expansion

When generator encounters a directory:

1. Send directory entry in current segment
2. Mark with XMIT_TOP_DIR
3. Queue directory for expansion
4. Emit new segment with contents
5. Send NDX_FLIST_EOF when done

### 11.4 NDX Values

File indices coordinate between processes:

| NDX Value | Meaning |
|-----------|---------|
| >= 0 | Valid file index |
| -1 | NDX_FLIST_EOF (segment end) |
| -2 | NDX_DONE (phase complete) |
| -3 | NDX_FLIST_OFFSET (segment base) |

---

## 12. Delete Modes Wire Format

### 12.1 Delete Mode Flags

Delete behavior controlled by flags:

| Flag | Wire Value | Behavior |
|------|------------|----------|
| DELETE_BEFORE | 0x01 | Delete before transfer |
| DELETE_DURING | 0x02 | Delete as dirs scanned |
| DELETE_AFTER | 0x04 | Delete after transfer |
| DELETE_DELAY | 0x08 | Delay delete, batch at end |
| DELETE_EXCLUDED | 0x10 | Delete excluded files too |

### 12.2 Delete-Before Phase

Entire delete list computed upfront:

```
// Generator sends delete list before file list:
[MSG_DELETED header]
[path: bytes]
// Repeat for each deletion
```

### 12.3 Delete-During Protocol

Deletions interleaved with transfers:

```
// Within file list transmission:
[XMIT_TOP_DIR entry]  // Enter directory
// ... file entries ...
[MSG_DELETED for extra files]
```

### 12.4 Delete-After Phase

Deletions sent after all transfers complete:

```
// After final NDX_DONE:
[MSG_DELETED header]
[path: bytes]
// Repeat for each deletion
```

### 12.5 Delete-Delay Protocol

Two-phase delete for safety:

1. Record deletions during transfer
2. Execute after successful completion
3. Skip if transfer fails

### 12.6 MSG_DELETED Format

```
[MSG_DELETED tag + length]
[relative_path: bytes]
```

---

## 13. Hard Link Wire Format

### 13.1 Hard Link Detection

Hard links detected by inode/device matching:

```rust
struct FileId {
    dev: u64,
    ino: u64,
}

fn is_hard_linked(a: &FileId, b: &FileId) -> bool {
    a.dev == b.dev && a.ino == b.ino
}
```

### 13.2 XMIT_HLINKED Flag

Links marked in transmit flags:

```
[xmit_flags with XMIT_HLINKED]
[link_ndx: i32]  // Index of first file in link group
```

### 13.3 Hard Link Index Encoding

**Protocol < 30:**
```
[prev_ndx: i32 LE]  // Absolute index
```

**Protocol 30+:**
```
[prev_ndx: varint]  // Relative to current index
```

### 13.4 Hard Link First Flag

First entry in group marked with XMIT_HLINK_FIRST:

```
[xmit_flags with XMIT_HLINK_FIRST]
// Full file data follows
```

Subsequent entries reference the first:
```
[xmit_flags with XMIT_HLINKED]
[link_ndx: referring to first]
// No file data (linked)
```

---

## 14. Device/Special File Wire Format

### 14.1 Device Encoding

Block/character devices encode major/minor:

**Protocol < 30:**
```
[rdev: u32 LE]
// major = rdev >> 8
// minor = rdev & 0xFF
```

**Protocol 30+:**
```
[rdev_major: varint]
[rdev_minor: varint]
```

### 14.2 Special File Flags

Special file types identified by mode:

| Type | Mode Mask | Encoding |
|------|-----------|----------|
| FIFO | S_IFIFO | No extra data |
| Socket | S_IFSOCK | No extra data |
| Block dev | S_IFBLK | rdev follows |
| Char dev | S_IFCHR | rdev follows |

### 14.3 --devices vs --specials

**`--devices` (`-D`):**
- Transfers block and character devices
- Requires root or CAP_MKNOD

**`--specials`:**
- Transfers FIFOs and sockets
- No special privileges needed

---

## 15. Symlink Wire Format

### 15.1 Symlink Target Encoding

Symlinks include target path:

```
[mode: u32 with S_IFLNK]
[target_len: varint]
[target_path: bytes]
```

### 15.2 --links Flag

`-l`/`--links` enables symlink transfer:

- Without: Symlinks followed, target content transferred
- With: Symlinks recreated on destination

### 15.3 Symlink Time Preservation

With CF_SYMLINK_TIMES:

```
// After target path:
[symlink_mtime: i32 LE]
[symlink_mtime_nsec: i32 LE]  // If XMIT_MOD_NSEC
```

---

## 16. ACL Wire Format

### 16.1 ACL Wire Format

ACLs transmitted when `-A`/`--acls` set:

```
[acl_count: varint]
// For each entry:
[acl_type: u8]      // USER, GROUP, OTHER, MASK
[acl_id: varint]    // uid/gid if applicable
[acl_perm: u8]      // rwx bits
```

### 16.2 ACL Entry Encoding

Entry types:
| Type | Value | ID Present |
|------|-------|------------|
| USER_OBJ | 0x01 | No |
| USER | 0x02 | Yes |
| GROUP_OBJ | 0x04 | No |
| GROUP | 0x08 | Yes |
| MASK | 0x10 | No |
| OTHER | 0x20 | No |

### 16.3 Default ACL Handling

Directory default ACLs sent separately:

```
[access_acl_count: varint]
[access_acl_entries: ...]
[default_acl_count: varint]
[default_acl_entries: ...]
```

### 16.4 ACL Deduplication

Repeated ACLs use index references:

```
[acl_index: varint]  // Reference to previous ACL
// If new:
[0xFF marker]
[full_acl_data]
```

---

## 17. Extended Attributes Wire Format

### 17.1 Xattr Namespace Handling

Namespaces prefixed to names:

| Namespace | Prefix | Requires |
|-----------|--------|----------|
| user | `user.` | No special permission |
| trusted | `trusted.` | CAP_SYS_ADMIN |
| security | `security.` | CAP_SYS_ADMIN |
| system | `system.` | Varies |

### 17.2 Xattr Wire Format

```
[xattr_count: varint]
// For each:
[name_len: varint]
[name: bytes]
[value_len: varint]
[value: bytes]
```

### 17.3 Xattr Deduplication

Similar to ACLs, xattr sets deduplicated:

```
[xattr_set_index: varint]  // 0 = new, N = reference
// If new:
[full_xattr_data]
```

---

## 18. File Attribute Preservation

### 18.1 --archive Flag Expansion

`-a`/`--archive` expands to:
```
-r  --recursive
-l  --links
-p  --perms
-t  --times
-g  --group
-o  --owner
-D  --devices --specials
```

### 18.2 Preservation Flag Interactions

| Flag | Implies | Conflicts |
|------|---------|-----------|
| -p | | |
| -A | -p | |
| -X | | |
| -o | | --no-owner |
| -g | | --no-group |

### 18.3 Numeric IDs vs Name Mapping

**`--numeric-ids`:**
- Transfer raw uid/gid values
- No name resolution

**Default:**
- Resolve uid/gid to names on sender
- Map names to uid/gid on receiver
- Fall back to numeric if name unknown

---

## 19. Compression Wire Format

### 19.1 Compression Negotiation

Protocol 31+ negotiates compression:

```
// Sender advertises:
[compress_list: NUL-terminated]
// "zstd zlib lz4\0"

// Receiver selects:
[selected: NUL-terminated]
// "zstd\0"
```

### 19.2 Zlib Token Compression

Default compression (protocol 26+):

```
// Compressed data block:
[DEFLATE_HEADER: u8]
[compressed_data: bytes]
[DEFLATE_TRAILER: u32]  // Adler32
```

### 19.3 Zstd Compression

Zstandard (protocol 32+):

```
[ZSTD_MAGIC: u32]
[frame_header: bytes]
[compressed_data: bytes]
```

### 19.4 LZ4 Compression

Fast compression option:

```
[block_size: i32]
[compressed_block: bytes]
```

### 19.5 Skip-Compress Patterns

Files matching patterns skip compression:

```
// Default skip-compress list:
7z ace avi bz2 deb gpg gz iso jpeg jpg lz lzma lzo
mp3 mp4 ogg png rar rpm rzip tbz tgz tlz txz xz z zip
```

---

## 20. I/O Timeout Protocol

### 20.1 Timeout Mechanics

`--timeout=SECONDS` enforces I/O deadlines:

```rust
fn read_with_timeout(reader: &mut R, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match reader.read_with_deadline(deadline) {
            Ok(data) => return Ok(data),
            Err(e) if e.kind() == TimedOut => {
                if Instant::now() >= deadline {
                    return Err(Error::Timeout);
                }
                // Send keepalive
                send_noop()?;
            }
            Err(e) => return Err(e),
        }
    }
}
```

### 20.2 Keepalive Protocol

MSG_NOOP prevents timeout during idle:

```
// Every timeout/2 seconds:
[MSG_NOOP header]  // Empty keepalive message
```

### 20.3 Timeout Error Handling

Timeout triggers connection abort:

```
rsync error: timeout in data send/receive (code 30)
```

Exit code 30 indicates timeout.

---

## 21. Partial Transfer Protocol

### 21.1 --partial Handling

Keeps partially transferred files:

- Normal: Delete incomplete files on error
- `--partial`: Retain for resume

### 21.2 --partial-dir Mechanics

Partial files moved to directory:

```
# During transfer:
.rsync-partial/filename.XXXXXX

# On completion:
mv .rsync-partial/filename.XXXXXX filename
```

### 21.3 Resume Protocol

Resume uses partial as basis:

1. Detect existing partial file
2. Use as basis for delta transfer
3. Apply only missing/changed blocks
4. Atomic rename on completion

---

## 22. Itemize Output Format

### 22.1 Format String

`-i`/`--itemize-changes` format:

```
YXcstpoguax  path/to/file
```

| Position | Meaning |
|----------|---------|
| Y | Update type |
| X | File type |
| c | Checksum differs |
| s | Size differs |
| t | Time differs |
| p | Permissions differ |
| o | Owner differs |
| g | Group differs |
| u | Reserved |
| a | ACL differs |
| x | Xattr differs |

### 22.2 Change Flags

Update type (position 0):
| Char | Meaning |
|------|---------|
| < | Sent |
| > | Received |
| c | Local change |
| h | Hard link |
| . | No change |
| * | Deleted |

File type (position 1):
| Char | Meaning |
|------|---------|
| f | File |
| d | Directory |
| L | Symlink |
| D | Device |
| S | Special |

### 22.3 --out-format Template

Custom output format:

| Variable | Meaning |
|----------|---------|
| %n | Filename |
| %L | Symlink target |
| %l | File length |
| %b | Bytes transferred |
| %i | Itemize string |
| %o | Operation |
| %f | Full path |

---

## 23. Checksum Transfer Protocol

### 23.1 File Checksum Flow

With `-c`/`--checksum`:

1. Sender computes whole-file checksums
2. Checksums included in file list
3. Receiver compares before transfer
4. Only differing files transferred

### 23.2 --checksum Behavior

```
// File list entry with --checksum:
[standard_fields]
[file_checksum: bytes]  // Same length as s2length
```

### 23.3 Checksum Caching

`.rsyncsums` cache file format:

```
# .rsyncsums
ALGO SIZE MTIME CHECKSUM FILENAME
md5 12345 1699999999 abc123def456 myfile.txt
```

---

## 24. Backup Wire Format

### 24.1 Backup Naming

`-b`/`--backup` renames existing files:

```
original.txt → original.txt~
```

With `--suffix`:
```
original.txt → original.txt.bak
```

### 24.2 --backup-dir Mechanics

Backup to separate directory:

```
--backup-dir=/backups

# Result:
/dest/file.txt (new)
/backups/file.txt (old)
```

### 24.3 Backup Suffix

Suffix applied to backup files:

```
--suffix=.YYYYMMDD
--suffix=~  (default)
```

---

## 25. Update/Skip Rules

### 25.1 --update Rules

`-u`/`--update` skip rules:

Skip if destination is newer:
```rust
fn should_skip(src: &Metadata, dst: &Metadata) -> bool {
    dst.mtime > src.mtime
}
```

### 25.2 --ignore-existing

Skip files that exist on destination:

```rust
fn should_skip(dst: &Path) -> bool {
    dst.exists()
}
```

### 25.3 --existing

Only update existing files:

```rust
fn should_transfer(dst: &Path) -> bool {
    dst.exists()
}
```

---

## 26. Safe File Writing

### 26.1 Temp File Creation

Files written to temp location:

```
.filename.XXXXXX  // Random suffix
```

### 26.2 Atomic Rename

Completed files atomically renamed:

```rust
fn safe_write(path: &Path, data: &[u8]) -> Result<()> {
    let temp = temp_path(path);
    write_all(&temp, data)?;
    rename(&temp, path)?;
    Ok(())
}
```

### 26.3 --inplace Behavior

Direct writes without temp file:

- Faster for large files
- Risk of corruption on failure
- Required for immutable destinations

---

## 27. Fuzzy Matching Algorithm

### 27.1 Algorithm Overview

`--fuzzy` finds similar basis files:

1. Search destination for similar names
2. Score by name similarity
3. Use best match as basis

### 27.2 Basis Selection

Similarity scoring:

```rust
fn similarity_score(name: &str, candidate: &str) -> u32 {
    let mut score = 0;

    // Exact prefix match
    let prefix_len = common_prefix_len(name, candidate);
    score += prefix_len * 10;

    // Exact suffix match
    let suffix_len = common_suffix_len(name, candidate);
    score += suffix_len * 10;

    // Same extension bonus
    if extension(name) == extension(candidate) {
        score += 50;
    }

    score
}
```

### 27.3 --fuzzy Variations

`-y` / `--fuzzy`: Search destination directory
`-yy`: Also search `--fuzzy-basis` dirs

---

## 28. I/O Buffer Management

### 28.1 Buffer Sizing

Default buffer sizes:

| Buffer | Size | Purpose |
|--------|------|---------|
| Read | 256KB | File reading |
| Write | 256KB | File writing |
| Match | 32KB | Delta matching |
| Socket | 64KB | Network I/O |

### 28.2 Block Size Selection

Block size for delta algorithm:

```rust
fn select_block_size(file_size: u64) -> u32 {
    // Target approximately 1000 blocks
    let size = (file_size / 1000) as u32;

    // Clamp to reasonable range
    size.clamp(700, 131072)
}
```

### 28.3 Buffer Strategy

Buffer reuse for efficiency:

```rust
struct BufferPool {
    buffers: Vec<Vec<u8>>,
    max_size: usize,
}

impl BufferPool {
    fn acquire(&mut self, size: usize) -> Vec<u8> {
        self.buffers.pop()
            .map(|mut b| { b.resize(size, 0); b })
            .unwrap_or_else(|| vec![0; size])
    }

    fn release(&mut self, buf: Vec<u8>) {
        if self.buffers.len() < self.max_size {
            self.buffers.push(buf);
        }
    }
}
```

---

## 29. Sparse File Handling

### 29.1 Sparse Detection

`-S`/`--sparse` creates sparse output:

```rust
fn is_zero_block(data: &[u8]) -> bool {
    data.iter().all(|&b| b == 0)
}
```

### 29.2 --sparse Behavior

Zero blocks become holes:

```rust
fn write_sparse(file: &mut File, data: &[u8]) -> Result<()> {
    if is_zero_block(data) {
        // Seek past zeros (creates hole)
        file.seek(SeekFrom::Current(data.len() as i64))?;
    } else {
        file.write_all(data)?;
    }
    Ok(())
}
```

### 29.3 Hole Punching

Use fallocate for existing holes:

```rust
#[cfg(target_os = "linux")]
fn punch_hole(file: &File, offset: u64, len: u64) -> Result<()> {
    use libc::{fallocate, FALLOC_FL_PUNCH_HOLE, FALLOC_FL_KEEP_SIZE};
    unsafe {
        fallocate(
            file.as_raw_fd(),
            FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
            offset as i64,
            len as i64,
        )
    };
    Ok(())
}
```

---

## 30. Rolling Checksum Mathematics

### 30.1 Algorithm

Adler32-variant rolling checksum:

```
s1 = Σ(data[i]) mod M
s2 = Σ((n - i) × data[i]) mod M

checksum = s1 + (s2 << 16)
```

Where M = 65521 (largest prime < 2^16)

### 30.2 S1/S2 Components

**S1**: Simple byte sum modulo M
**S2**: Position-weighted sum modulo M

```rust
fn compute_checksum(data: &[u8]) -> u32 {
    let mut s1: u32 = 0;
    let mut s2: u32 = 0;

    for (i, &byte) in data.iter().enumerate() {
        s1 = (s1 + byte as u32) % 65521;
        s2 = (s2 + (data.len() - i) as u32 * byte as u32) % 65521;
    }

    s1 + (s2 << 16)
}
```

### 30.3 Window Slide

Efficient rolling update:

```rust
fn roll_checksum(
    checksum: u32,
    old_byte: u8,
    new_byte: u8,
    block_len: usize,
) -> u32 {
    let mut s1 = checksum & 0xFFFF;
    let mut s2 = checksum >> 16;

    // Remove old byte contribution
    s1 = (s1 - old_byte as u32 + 65521) % 65521;
    s2 = (s2 - block_len as u32 * old_byte as u32 + 65521) % 65521;

    // Add new byte contribution
    s1 = (s1 + new_byte as u32) % 65521;
    s2 = (s2 + s1) % 65521;

    s1 + (s2 << 16)
}
```

---

## 31. Progress Output Format

### 31.1 Standard Format

Default progress output:

```
sending incremental file list
         32,768  50%    1.23MB/s    0:00:01
         65,536 100%    2.46MB/s    0:00:00 (xfer#1, to-check=42/100)
```

### 31.2 --info=progress2

Enhanced progress with totals:

```
          1.23G  45%    5.67MB/s    0:02:34 (xfr#123, ir-chk=456/789)
```

| Field | Meaning |
|-------|---------|
| 1.23G | Total bytes transferred |
| 45% | Overall progress |
| 5.67MB/s | Current transfer rate |
| 0:02:34 | Estimated time remaining |
| xfr#123 | Files transferred |
| ir-chk | Incremental check remaining |

### 31.3 Rate Calculation

Transfer rate smoothing:

```rust
fn calculate_rate(bytes: u64, duration: Duration) -> f64 {
    bytes as f64 / duration.as_secs_f64()
}

fn smooth_rate(current: f64, previous: f64) -> f64 {
    // Exponential moving average
    0.7 * current + 0.3 * previous
}
```

---

## 32. Checksum Seed Protocol

### 32.1 Seed Protocol

Random seed for checksum security:

```
// Protocol 30+:
Sender → Receiver: [checksum_seed: i32]

// Protocol < 30:
Receiver → Sender: [checksum_seed: i32]
```

### 32.2 Seed Timing

Seed exchanged early in handshake:

**Protocol 30+ (CF_CHKSUM_SEED_ORDER):**
1. Version exchange
2. Compat flags
3. Sender sends seed

**Protocol < 30:**
1. Version exchange
2. Receiver sends seed

### 32.3 --checksum-seed Option

Override random seed:

```bash
rsync --checksum-seed=12345 src/ dst/
```

Deterministic for testing.

---

## 33. File Ordering and Priority

### 33.1 Ordering Rules

File list ordering:

1. Directories before contents
2. Lexicographic within directories
3. Case-sensitive comparison

```rust
fn compare_entries(a: &Entry, b: &Entry) -> Ordering {
    let a_dir = a.is_dir();
    let b_dir = b.is_dir();

    match (a_dir, b_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a.name.cmp(&b.name),
    }
}
```

### 33.2 Priority Queue

Generator maintains priority queue:

- Larger files first (by default)
- Modified order with `--preallocate`

### 33.3 Sorted Transfer

`--no-inc-recursive` forces sorted:

- Complete file list before transfer
- Guaranteed order
- Higher memory usage

---

## 34. Module Listing Protocol

### 34.1 List Request

Empty module name triggers listing:

```
Client: \n
```

### 34.2 Response Format

Tab-separated name and description:

```
module1\tDescription text\n
module2\tAnother description\n
@RSYNCD: EXIT\n
```

### 34.3 Module Description

From `rsyncd.conf`:

```ini
[module1]
    path = /data/module1
    comment = Description text
```

---

## 35. Error Recovery Mechanisms

### 35.1 I/O Error Propagation

MSG_IO_ERROR carries error flags:

```
[MSG_IO_ERROR header]
[error_code: i32]
```

Error codes:
| Code | Meaning |
|------|---------|
| 1 | Vanished file |
| 2 | I/O error |
| 4 | Partial transfer |

### 35.2 MSG_IO_ERROR Format

```rust
fn send_io_error(writer: &mut W, code: i32) -> io::Result<()> {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&code.to_le_bytes());
    send_msg(writer, MSG_IO_ERROR, &buf)
}
```

### 35.3 File Redo Mechanism

Failed files retried:

1. Mark file for redo
2. Continue with remaining files
3. Retry marked files
4. Report persistent failures

### 35.4 Max Error Handling

`--max-delete=N` limits deletions:

```rust
fn can_delete(state: &mut State) -> bool {
    if state.delete_count >= state.max_delete {
        return false;
    }
    state.delete_count += 1;
    true
}
```

---

## 36. Temp File Naming Convention

### 36.1 Naming Pattern

Temp files use pattern:

```
.filename.XXXXXX
```

Where XXXXXX is 6 random characters.

### 36.2 --temp-dir Mechanics

Custom temp directory:

```bash
rsync --temp-dir=/fast-disk src/ dst/
```

Creates temps in specified directory, then renames across filesystems if needed.

### 36.3 Temp Cleanup

Cleanup on completion/failure:

```rust
fn cleanup_temp(temp_path: &Path) {
    if temp_path.exists() {
        let _ = std::fs::remove_file(temp_path);
    }
}
```

---

## 37. Symlink Safety Options

### 37.1 --safe-links

Ignore unsafe symlinks:

```rust
fn is_safe_link(target: &Path, base: &Path) -> bool {
    let resolved = base.join(target).canonicalize();
    resolved.map(|p| p.starts_with(base)).unwrap_or(false)
}
```

### 37.2 --munge-links

Transform symlinks for safety:

```
target → .symlink<target>
```

### 37.3 Security Checks

Default symlink validation:

- No absolute targets outside transfer root
- No `..` components escaping root
- Warning on unsafe links (without --copy-unsafe-links)

---

## 38. Delay Updates Protocol

### 38.1 --delay-updates Mechanics

Two-phase commit:

1. Write to `.~tmp~` prefix
2. Rename all at end

### 38.2 Temp Storage

Delayed files stored temporarily:

```
.~tmp~filename → filename
```

### 38.3 Commit Phase

Atomic bulk rename:

```rust
fn commit_delayed(pending: &[PathBuf]) -> Result<()> {
    for temp in pending {
        let final_path = strip_delay_prefix(temp);
        std::fs::rename(temp, final_path)?;
    }
    Ok(())
}
```

---

## 39. Fake Super Mode

### 39.1 --fake-super Mechanics

Store ownership in xattrs:

```
user.rsync.%stat → mode,uid,gid
```

### 39.2 Xattr Storage

Format:

```
100644 1000,1000
(mode uid,gid)
```

### 39.3 Compatibility

Interoperates with upstream rsync `--fake-super`.

---

## 40. Character Encoding

### 40.1 --iconv Option

Character set conversion:

```bash
rsync --iconv=UTF-8,LATIN1 src/ dst/
```

### 40.2 Encoding Negotiation

With CF_SYMLINK_ICONV:

- Filenames converted sender→receiver
- Symlink targets converted if flag set

### 40.3 Error Handling

Invalid sequences:

```rust
fn convert_name(name: &[u8], from: &str, to: &str) -> Result<String> {
    iconv::convert(name, from, to)
        .map_err(|e| Error::EncodingError(e))
}
```

---

## 41. Daemon Exec Hooks

### 41.1 Pre-Xfer Exec

Execute before transfer:

```ini
[module]
    pre-xfer exec = /usr/local/bin/pre-sync.sh
```

### 41.2 Post-Xfer Exec

Execute after transfer:

```ini
[module]
    post-xfer exec = /usr/local/bin/post-sync.sh
```

### 41.3 Environment Variables

Available to hooks:

| Variable | Value |
|----------|-------|
| RSYNC_MODULE_NAME | Module being accessed |
| RSYNC_MODULE_PATH | Module filesystem path |
| RSYNC_HOST_ADDR | Client IP address |
| RSYNC_HOST_NAME | Client hostname |
| RSYNC_USER_NAME | Authenticated user |
| RSYNC_PID | Daemon process ID |
| RSYNC_REQUEST | Requested path |
| RSYNC_ARG# | Transfer arguments |
| RSYNC_EXIT_STATUS | Exit code (post-xfer only) |

---

## 42. Remote Binary Options

### 42.1 --rsync-path

Specify remote binary:

```bash
rsync --rsync-path=/opt/rsync/bin/rsync src/ remote:dst/
```

### 42.2 --remote-option

Pass options to remote:

```bash
rsync -M--bwlimit=100 src/ remote:dst/
```

### 42.3 Binary Selection

Remote execution:

```rust
fn build_remote_command(opts: &Options) -> String {
    let binary = opts.rsync_path.as_deref().unwrap_or("rsync");
    let args = build_remote_args(opts);
    format!("{} {}", binary, args.join(" "))
}
```

---

## 43. Implied Directories

### 43.1 Directory Creation

Parent directories created as needed:

```
rsync src/deep/path/file.txt dst/
# Creates dst/deep/path/ if needed
```

### 43.2 --no-implied-dirs

Skip intermediate directory creation:

```bash
rsync --no-implied-dirs src/a/b/file dst/
# Only creates dst/file, not dst/a/b/
```

### 43.3 -R Relative Paths

`--relative` preserves path:

```bash
rsync -R /src/./deep/path/file.txt dst/
# Creates dst/deep/path/file.txt
```

---

## 44. UID/GID Mapping

### 44.1 --usermap Option

Map usernames/UIDs:

```bash
rsync --usermap=alice:bob,1000:2000 src/ dst/
```

### 44.2 --groupmap Option

Map group names/GIDs:

```bash
rsync --groupmap=staff:users src/ dst/
```

### 44.3 --chown Option

Force owner/group:

```bash
rsync --chown=www-data:www-data src/ dst/
```

### 44.4 Mapping Syntax

```
FROM:TO[,FROM:TO...]
*:NAME      # Map all to NAME
NAME:       # Map NAME to running user
:NAME       # Map to NAME (no source filter)
```

---

## 45. Daemon Socket Options

### 45.1 Socket Binding

Daemon listen configuration:

```ini
address = 0.0.0.0
port = 873
```

### 45.2 Listen Backlog

Connection queue depth:

```rust
const DEFAULT_BACKLOG: i32 = 5;

fn listen(socket: TcpListener, backlog: i32) {
    socket.listen(backlog);
}
```

### 45.3 Socket Options

TCP options applied:

| Option | Purpose |
|--------|---------|
| SO_REUSEADDR | Allow address reuse |
| TCP_NODELAY | Disable Nagle algorithm |
| SO_KEEPALIVE | Detect dead connections |

---

## 46. Whole File Transfer

### 46.1 --whole-file Behavior

Skip delta algorithm:

```rust
fn should_whole_file(opts: &Options, local: bool) -> bool {
    opts.whole_file || (local && !opts.no_whole_file)
}
```

### 46.2 Auto-Detection

Local transfers default to whole-file:

- Same filesystem
- No delta overhead benefit
- `--no-whole-file` to override

### 46.3 Tradeoffs

| Mode | CPU | Bandwidth | Use Case |
|------|-----|-----------|----------|
| Delta | High | Low | Remote/slow |
| Whole | Low | High | Local/fast |

---

## 47. Append Mode Protocol

### 47.1 --append Mode

Only transfer new data:

```rust
fn append_transfer(src: &File, dst: &mut File) -> Result<()> {
    let dst_size = dst.metadata()?.len();
    src.seek(SeekFrom::Start(dst_size))?;
    std::io::copy(src, dst)?;
    Ok(())
}
```

### 47.2 --append-verify

Verify existing data matches:

```rust
fn append_verify(src: &File, dst: &File) -> Result<bool> {
    let dst_size = dst.metadata()?.len();
    let src_checksum = checksum_range(src, 0, dst_size)?;
    let dst_checksum = checksum_range(dst, 0, dst_size)?;
    Ok(src_checksum == dst_checksum)
}
```

### 47.3 File Handling

Append conditions:

- Destination exists
- Destination smaller than source
- No truncation needed

---

## 48. Copy Dest/Link Dest Options

### 48.1 --copy-dest

Copy matching files from alternate:

```bash
rsync --copy-dest=/snapshots/yesterday src/ dst/
```

### 48.2 --link-dest

Hard link matching files:

```bash
rsync --link-dest=/snapshots/yesterday src/ dst/
```

### 48.3 --compare-dest

Only compare, don't copy/link:

```bash
rsync --compare-dest=/reference src/ dst/
```

### 48.4 Search Order

Multiple --*-dest options searched in order:

1. First --link-dest
2. Second --link-dest
3. ... etc.
4. Destination directory

---

## 49. Max/Min Size Filtering

### 49.1 --max-size

Skip files larger than threshold:

```bash
rsync --max-size=100M src/ dst/
```

### 49.2 --min-size

Skip files smaller than threshold:

```bash
rsync --min-size=1K src/ dst/
```

### 49.3 Size Suffixes

| Suffix | Multiplier |
|--------|------------|
| K | 1,024 |
| M | 1,048,576 |
| G | 1,073,741,824 |
| T | 1,099,511,627,776 |

---

## 50. Modify Window Comparison

### 50.1 --modify-window

Fuzzy time comparison:

```bash
rsync --modify-window=1 src/ dst/
```

### 50.2 FAT Filesystem

FAT uses 2-second resolution:

```bash
rsync --modify-window=2 src/ /mnt/usb/
```

### 50.3 Time Comparison

```rust
fn times_match(src: i64, dst: i64, window: i64) -> bool {
    (src - dst).abs() <= window
}
```

---

## Appendix A: Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Syntax/usage error |
| 2 | Protocol incompatibility |
| 3 | Errors selecting files |
| 4 | Requested action not supported |
| 5 | Error starting client-server protocol |
| 6 | Daemon unable to append to log-file |
| 10 | Error in socket I/O |
| 11 | Error in file I/O |
| 12 | Error in rsync protocol data stream |
| 13 | Errors with program diagnostics |
| 14 | Error in IPC code |
| 20 | Received SIGUSR1 or SIGINT |
| 21 | Some error returned by waitpid() |
| 22 | Error allocating core memory buffers |
| 23 | Partial transfer due to error |
| 24 | Partial transfer due to vanished source files |
| 25 | The --max-delete limit stopped deletions |
| 30 | Timeout in data send/receive |
| 35 | Timeout waiting for daemon connection |

---

## Appendix B: Protocol Version History

| Version | Rsync Version | Key Changes |
|---------|---------------|-------------|
| 20 | 2.3.0 | Minimum supported |
| 21 | 2.4.0 | 64-bit file sizes |
| 25 | 2.5.0 | --delete-during |
| 26 | 2.5.4 | Compression support |
| 27 | 2.6.0 | Checksum seed |
| 28 | 2.6.4 | Filter rules |
| 29 | 2.6.9 | Incremental recursion |
| 30 | 3.0.0 | Varint encoding, compat flags |
| 31 | 3.1.0 | Checksum negotiation, iconv |
| 32 | 3.2.0 | Zstd compression |

---

## Appendix C: References

- [rsync Technical Report](https://rsync.samba.org/tech_report/)
- [rsync(1) man page](https://download.samba.org/pub/rsync/rsync.1)
- [rsyncd.conf(5) man page](https://download.samba.org/pub/rsync/rsyncd.conf.5)
- [Upstream rsync source](https://github.com/RsyncProject/rsync)
