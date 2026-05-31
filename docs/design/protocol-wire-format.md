# Protocol Wire Format Reference

This document describes the rsync protocol wire format as implemented in oc-rsync,
targeting wire-compatibility with upstream rsync 3.4.x (protocol versions 28-32).

## 1. Protocol Negotiation

### Daemon Greeting

The daemon sends an ASCII greeting line before the binary multiplex stream begins:

```
@RSYNCD: <version>.<subversion>\n
```

Examples (exact bytes):

| Protocol | Greeting bytes |
|----------|---------------|
| 32 | `40 52 53 59 4E 43 44 3A 20 33 32 2E 30 0A` (`@RSYNCD: 32.0\n`) |
| 31 | `40 52 53 59 4E 43 44 3A 20 33 31 2E 30 0A` (`@RSYNCD: 31.0\n`) |
| 29 | `40 52 53 59 4E 43 44 3A 20 32 39 2E 30 0A` (`@RSYNCD: 29.0\n`) |

### Version Exchange

Both peers exchange their greeting. The negotiated protocol version is `min(local, remote)`.
Protocol 32 peers also advertise supported digest algorithms after the version:

```
@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n
```

### Module Selection

After the greeting exchange, the client sends the module name as a bare ASCII line
terminated by `\n`. The daemon responds with either:

- `@RSYNCD: OK\n` - module accepted, transition to binary stream
- `@RSYNCD: AUTHREQD <challenge>\n` - authentication required
- `@ERROR: <message>\n` - module rejected

## 2. Multiplexing

Once the connection transitions to binary mode, all data flows through a multiplexed
frame format. Each frame has a 4-byte little-endian header followed by payload.

### Header Format

```
Byte 0-2: payload length (24-bit LE, max 0x00FFFFFF = 16,777,215 bytes)
Byte 3:   tag = MPLEX_BASE (7) + message_code
```

The 32-bit header value is: `(tag << 24) | payload_length`

### Message Codes

| Code | Name | Wire tag | Purpose |
|------|------|----------|---------|
| 0 | MSG_DATA | 7 (0x07) | File/protocol data |
| 1 | MSG_ERROR_XFER | 8 (0x08) | Fatal transfer error |
| 2 | MSG_INFO | 9 (0x09) | Informational message |
| 3 | MSG_ERROR | 10 (0x0A) | Non-fatal error |
| 4 | MSG_WARNING | 11 (0x0B) | Warning message |
| 9 | MSG_REDO | 16 (0x10) | Reprocess file index |
| 10 | MSG_STATS | 17 (0x11) | Transfer statistics |
| 42 | MSG_NOOP | 49 (0x31) | Keepalive heartbeat |
| 100 | MSG_SUCCESS | 107 (0x6B) | File updated successfully |
| 101 | MSG_DELETED | 108 (0x6C) | File deleted |
| 102 | MSG_NO_SEND | 109 (0x6D) | Sender could not open file |

### Byte Examples

```
MSG_DATA, 100 bytes payload:
  [0x64, 0x00, 0x00, 0x07]   (7 << 24) | 100

MSG_INFO, 50 bytes payload:
  [0x32, 0x00, 0x00, 0x09]   (9 << 24) | 50

MSG_ERROR, 0 bytes payload:
  [0x00, 0x00, 0x00, 0x0A]   (10 << 24) | 0

MSG_DATA, max payload (16,777,215 bytes):
  [0xFF, 0xFF, 0xFF, 0x07]   (7 << 24) | 0x00FFFFFF
```

## 3. Variable-Length Integer Encoding (varint)

Protocol 30+ uses variable-length integers extensively. The first byte's high bits
indicate how many extra bytes follow:

| First byte pattern | Extra bytes | Total bytes | Value range |
|-------------------|-------------|-------------|-------------|
| `0xxxxxxx` | 0 | 1 | 0 - 127 |
| `10xxxxxx` | 1 | 2 | 0 - 16,383 |
| `110xxxxx` | 2 | 3 | 0 - 2,097,151 |
| `1110xxxx` | 3 | 4 | 0 - 268,435,455 |
| `11110xxx` | 4 | 5 | any i32 |

Extra bytes are little-endian. The data bits in the first byte occupy the remaining
low bits after the prefix.

### Byte Examples

| Value | Encoded bytes | Explanation |
|-------|--------------|-------------|
| 0 | `[0x00]` | 7 bits, direct |
| 1 | `[0x01]` | 7 bits, direct |
| 127 | `[0x7F]` | Max single-byte value |
| 128 | `[0x80, 0x80]` | `10|000000` + `0x80` |
| 255 | `[0x80, 0xFF]` | `10|000000` + `0xFF` |
| 256 | `[0x81, 0x00]` | `10|000001` + `0x00` |
| 16,383 | `[0xBF, 0xFF]` | Max 2-byte value |
| 16,384 | `[0xC0, 0x00, 0x40]` | `110|00000` + LE(0x4000) |

### varlong (64-bit)

The `varlong` format encodes 64-bit values with a `min_bytes` parameter. The leading
byte indicates how many of the remaining bytes carry data. File sizes use
`min_bytes=3`; modification times use `min_bytes=4`.

### longint (Protocol < 30)

Pre-protocol-30 uses fixed-width encoding:
- Values <= 0x7FFFFFFF: 4-byte LE i32
- Values > 0x7FFFFFFF: marker `[0xFF, 0xFF, 0xFF, 0xFF]` + 8-byte LE i64

## 4. File List Encoding

Each file entry is encoded with differential compression against the previous entry.

### Entry Structure

```
[flags]              - XMIT flags (encoding varies by protocol)
[same_len: u8]       - Shared prefix length (if XMIT_SAME_NAME set)
[suffix_len]         - u8 normally, varint if XMIT_LONG_NAME set
[name_suffix]        - Path bytes after shared prefix
[size]               - varlong30 (proto>=30) or longint (proto<30)
[mtime]              - varlong min_bytes=4 (proto>=30) or i32 LE (proto<30)
[mode: i32 LE]       - Only if XMIT_SAME_MODE not set
[uid]                - Only if XMIT_SAME_UID not set
[gid]                - Only if XMIT_SAME_GID not set
[rdev]               - Device entries only
[symlink_target]     - Symlinks only
[hardlink_info]      - If XMIT_HLINKED set
[checksum]           - If --checksum mode
```

End of list: a single zero byte (`[0x00]`).

### XMIT Flags (Primary, bits 0-7)

| Bit | Constant | Meaning |
|-----|----------|---------|
| 0 | XMIT_TOP_DIR | Top-level directory |
| 1 | XMIT_SAME_MODE | Mode matches previous entry |
| 2 | XMIT_EXTENDED_FLAGS | Extended flags follow |
| 3 | XMIT_SAME_UID | UID matches previous entry |
| 4 | XMIT_SAME_GID | GID matches previous entry |
| 5 | XMIT_SAME_NAME | Name shares prefix with previous |
| 6 | XMIT_LONG_NAME | Name length > 255, use varint |
| 7 | XMIT_SAME_TIME | Mtime matches previous entry |

### Flags Encoding

- **Protocol >= 30 with VARINT_FLIST_FLAGS**: single varint for all flag bits
- **Protocol 28-29**: 1 byte; if extended flags needed, set bit 2 and emit 2 bytes LE
- **Protocol < 28**: 1 byte only

### Golden Example: Protocol 28 Regular File

File "hello.txt", size=42, mode=0o100644, mtime=1700000000, first entry:

```
18                         flags: XMIT_SAME_UID(0x08) | XMIT_SAME_GID(0x10)
09                         name suffix length: 9
68 65 6C 6C 6F 2E 74 78 74  name: "hello.txt"
2A 00 00 00                size: write_longint(42) = 4-byte LE
00 F1 53 65                mtime: 1700000000 as u32 LE
A4 81 00 00                mode: 0o100644 = 33188 as i32 LE
```

### Golden Example: Protocol 28 Directory

Directory "mydir", mode=0o40755, mtime=1700000000:

```
18                         flags: XMIT_SAME_UID | XMIT_SAME_GID
05                         name suffix length: 5
6D 79 64 69 72             name: "mydir"
00 00 00 00                size: 0
00 F1 53 65                mtime: 1700000000 as u32 LE
ED 41 00 00                mode: 0o40755 = 16877 as i32 LE
```

### Name Prefix Compression

When consecutive entries share a path prefix, XMIT_SAME_NAME is set and a `same_len`
byte precedes the suffix length. For example, encoding "dir/file2.txt" after
"dir/file1.txt" (8 bytes shared: "dir/file"):

```
[same_len=8] [suffix_len=5] "2.txt"
```

## 5. Checksum Exchange (Signatures)

The generator sends block signatures for each basis file so the sender can compute
deltas. The signature wire format consists of a header followed by per-block data.

### Signature Header

```
[block_count: varint]       - Number of blocks in basis file
[block_length: varint]      - Bytes per block (typically 700-131072)
[strong_sum_length: varint] - Bytes of strong checksum per block (2-16)
```

### Per-Block Data

For each of `block_count` blocks:

```
[rolling_sum: u32 LE]       - 4-byte rolling checksum (Adler-32 variant)
[strong_sum: N bytes]       - Strong hash, truncated to strong_sum_length
```

### Checksum Selection

- **Rolling**: Always a 32-bit Adler-32 variant (`rsum`)
- **Strong** (by protocol negotiation):
  - MD4 (16 bytes) - protocol 27-29 default
  - MD5 (16 bytes) - protocol 30+ default without negotiation
  - XXH3/XXH128 (16 bytes) - protocol 30+ with checksum negotiation (`-e.LsfxCIvu`)

### Phase-Aware Truncation

- Phase 1 (initial transfer): `SHORT_SUM_LENGTH = 2` bytes of strong checksum
- Phase 2 (redo pass): `MAX_SUM_LENGTH = 16` bytes (full strong checksum)

## 6. Delta Tokens

Delta streams reconstruct files from basis blocks and literal data. The wire format
uses `write_int()` (4-byte LE i32) tokens.

### Token Types

| Wire value | Meaning | Format |
|-----------|---------|--------|
| Positive N | Literal data of N bytes | `[N as i32 LE]` + N raw bytes |
| Negative | Block match at index `-(token+1)` | `[token as i32 LE]` |
| Zero (0) | End of delta stream | `[0x00, 0x00, 0x00, 0x00]` |

### Byte Examples

**Literal "hello" (5 bytes):**
```
05 00 00 00                write_int(5): literal length
68 65 6C 6C 6F             raw data: "hello"
```

**Block match at index 0:**
```
FF FF FF FF                write_int(-1): -(0+1) = -1
```

**Block match at index 42:**
```
D5 FF FF FF                write_int(-43): -(42+1) = -43
```

**End of stream:**
```
00 00 00 00                write_int(0): end marker
```

### Whole-File Transfer

When no basis file exists (whole-file transfer), the entire file is sent as a single
literal followed by the end marker. Large files are chunked at CHUNK_SIZE (32,768 bytes).

**Example - transferring "hi" (2 bytes):**
```
02 00 00 00                literal length: 2
68 69                      data: "hi"
00 00 00 00                end marker
```

## 7. NDX Protocol (File Index Encoding)

NDX values reference file-list entries during INC_RECURSE transfers.

### Sentinel Constants

| Constant | Value | Purpose |
|----------|-------|---------|
| NDX_DONE | -1 | End of file requests for current phase |
| NDX_FLIST_EOF | -2 | No more incremental file lists |
| NDX_DEL_STATS | -3 | Delete statistics follow |
| NDX_FLIST_OFFSET | -101 | Base offset for incremental flist indices |

### Legacy Encoding (Protocol < 30)

All NDX values are plain 4-byte LE signed integers:

```
NDX_DONE:       [0xFF, 0xFF, 0xFF, 0xFF]   (-1 as i32 LE)
NDX_FLIST_EOF:  [0xFE, 0xFF, 0xFF, 0xFF]   (-2 as i32 LE)
Index 5:        [0x05, 0x00, 0x00, 0x00]   (5 as i32 LE)
```

### Modern Encoding (Protocol >= 30)

Delta-encoded byte-reduction format. Tracks previous positive and negative values
separately to minimize bytes on the wire.

| First byte | Meaning |
|-----------|---------|
| `0x00` | NDX_DONE (-1) |
| `0xFF` | Negative value prefix (read next byte for delta) |
| `0x01`-`0xFD` | Delta from previous positive value |
| `0xFE` | Extended encoding prefix |

**Extended encoding (after `0xFE`):**
- If next byte has bit 7 set: 4-byte absolute value (high byte & 0x7F, then 3 LE bytes)
- Otherwise: 2-byte delta (high byte, low byte) added to previous value

**Examples (starting from initial state, prev_positive=-1):**

```
Index 0:  [0x01]           delta from prev(-1): 0-(-1) = 1
Index 1:  [0x01]           delta from prev(0): 1-0 = 1
Index 5:  [0x04]           delta from prev(1): 5-1 = 4
NDX_DONE: [0x00]           always single byte, no state change
```

### NDX_DEL_STATS Wire Format

When the generator finishes, it sends NDX_DEL_STATS followed by 5 varints encoding
delete counts by type (files, directories, symlinks, devices, specials).

## 8. Checksum Seed

The checksum seed is exchanged as a 4-byte LE i32 after version negotiation
completes. It randomizes rolling checksums to prevent adversarial collisions.

## 9. Compatibility Flags (Protocol >= 30)

After version exchange, protocol 30+ peers exchange a varint-encoded compatibility
flags bitfield. Key flags include:

- INC_RECURSE - incremental recursion enabled
- SYMLINK_TIMES - preserve symlink timestamps  
- SYMLINK_ICONV - convert symlink target encoding
- SAFE_FLIST - safe file list ending
- VARINT_FLIST_FLAGS - use varint for flist entry flags

## Upstream References

- `io.c` - Multiplex framing (`mplex_read`, `mplex_write`), varint codec, NDX codec
- `flist.c` - File list encoding (`send_file_entry`, `recv_file_entry`)
- `token.c` - Delta token format (`simple_send_token`)
- `rsync.h` - Constants (MPLEX_BASE, NDX_*, XMIT_*)
- `clientserver.c` - Daemon greeting and module negotiation
- `match.c` / `sender.c` - Signature generation and delta computation
