# oc-rsync Protocol 32 Implementation

This document describes the wire protocol negotiation, framing, roles, and state machine logic used to implement upstream rsync 3.4.1 Protocol 32.

---

## Protocol Version

- oc-rsync must always emit: `protocol version = 32`
- Must accept fallback negotiation to 28–32
- Fails with explicit error for unsupported versions

---

## Multiplex Protocol

### Wire Format

Multiplex frames use a **4-byte little-endian header** followed by payload:

```
[byte 0: length low]
[byte 1: length mid]
[byte 2: length high]
[byte 3: tag]
[payload bytes...]
```

The tag is computed as: `MPLEX_BASE + message_code` where `MPLEX_BASE = 7`.

**Example**: A 21-byte MSG_DATA frame (code=0):
- Wire bytes: `[0x15, 0x00, 0x00, 0x07]` + 21 payload bytes
- Tag = 7 (MPLEX_BASE + 0)
- Length = 0x000015 (21 decimal)

### Message Codes

| Code | Tag | Name             | Purpose                                            |
|------|-----|------------------|----------------------------------------------------|
| 0    | 7   | `MSG_DATA`       | File list, file content blocks                     |
| 1    | 8   | `MSG_ERROR_XFER` | Transfer error (FERROR_XFER); causes exit code 23  |
| 2    | 9   | `MSG_INFO`       | Informational messages                             |
| 3    | 10  | `MSG_ERROR`      | Non-fatal remote error (FERROR); protocol >= 30    |
| 4    | 11  | `MSG_WARNING`    | Warning messages                                   |
| 5    | 12  | `MSG_ERROR_SOCKET` | Error from receiver/generator pipe (FERROR_SOCKET) |
| 6    | 13  | `MSG_LOG`        | Daemon-log-only message (FLOG)                     |
| 7    | 14  | `MSG_CLIENT`     | Client-only message (FCLIENT)                      |
| 8    | 15  | `MSG_ERROR_UTF8` | UTF-8 conversion error (FERROR_UTF8)               |
| 9    | 16  | `MSG_REDO`       | Request to reprocess a file-list index             |
| 10   | 17  | `MSG_STATS`      | Transfer statistics for the generator              |
| 22   | 29  | `MSG_IO_ERROR`   | I/O error during transfer                          |
| 33   | 40  | `MSG_IO_TIMEOUT` | Daemon communicates its timeout value              |
| 42   | 49  | `MSG_NOOP`       | Keep-alive / flow control                          |
| 86   | 93  | `MSG_ERROR_EXIT` | Synchronize error exit (protocol >= 31)            |
| 100  | 107 | `MSG_SUCCESS`    | Transfer success notification                      |
| 101  | 108 | `MSG_DELETED`    | Receiver reports a deleted file                    |
| 102  | 109 | `MSG_NO_SEND`    | Sender failed to open a requested file             |

### Activation Sequence

**CRITICAL**: Multiplex activation must follow this exact sequence to match upstream rsync:

1. **Setup Phase** (protocol negotiation, pre-multiplex):
   - Daemon: `@RSYNCD:` text handshake
   - Exchange protocol version
   - Server calls `setup_protocol(f_out, f_in)` which:
     - Sends compatibility flags as **plain varint** (if protocol >= 30)
     - Does NOT flush; the flush happens in `io_start_multiplex_out()`

2. **Multiplex Activation** (happens AFTER setup_protocol):
   ```
   if (protocol_version >= 23)
       io_start_multiplex_out(f_out);  // OUTPUT only! Also flushes.
   ```
   - **OUTPUT multiplex activated immediately** (includes flush of compat flags)
   - **INPUT multiplex deferred** until after filter list exchange

3. **Input Multiplex and Filter List Exchange**:
   - For **sender role** (`am_sender=true` on server):
     - `io_start_multiplex_in(f_in)` — enables multiplexed reads (main.c:1254-1255)
     - `recv_filter_list(f_in)` — reads filter list as **multiplexed data** (main.c:1258)
   - **Protocol < 30**: Uses `io_start_buffering_in(f_in)` instead (main.c:1257)

4. **Transfer Phase**:
   - File list sent via multiplex (MSG_DATA frames)
   - All subsequent I/O uses multiplex protocol

### Stream Handling

**Daemon mode** uses `dup()` (Rust: `try_clone()`) to create independent file descriptors for read/write:

```rust
// Clone ONCE, before any protocol exchange
let read_stream = tcp_stream.try_clone()?;
let write_stream = tcp_stream;

// Use these same handles throughout - do NOT clone again!
setup_protocol(protocol, &mut write_stream, &mut read_stream)?;
// ... multiplex activation on both handles
// ... role execution with same handles
```

**Key invariants**:
- Both handles point to the same kernel socket
- No buffering mismatch between clones
- Compat flags sent on write_stream BEFORE any subsequent clones
- Never clone after protocol negotiation starts

---

## Compatibility Flags (Protocol >= 30)

Sent as **plain varint** inside `setup_protocol()`, before multiplex activation.

Server-to-client flags (unidirectional):

| Bit  | Flag                      | Meaning                                |
|------|---------------------------|----------------------------------------|
| 0x01 | `CF_INC_RECURSE`          | Incremental recursion supported        |
| 0x02 | `CF_SYMLINK_TIMES`        | Symlink timestamps can be preserved    |
| 0x04 | `CF_SYMLINK_ICONV`        | Symlink payload requires iconv         |
| 0x08 | `CF_SAFE_FLIST`           | Receiver requests safe file list       |
| 0x10 | `CF_AVOID_XATTR_OPTIM`    | Receiver cannot use xattr optimization |
| 0x20 | `CF_CHKSUM_SEED_FIX`      | Proper checksum seed ordering          |
| 0x40 | `CF_INPLACE_PARTIAL_DIR`  | Use partial dir with `--inplace`       |
| 0x80 | `CF_VARINT_FLIST_FLAGS`   | File list flags use varint encoding    |
| 0x100| `CF_ID0_NAMES`            | File-list entries support id0 names    |

**Wire encoding**: For flags=0xa1 (161), varint encodes as `[0x80, 0xa1]` (2 bytes)

**Varint Format** (from io.c:write_varint):
- Values 0-127: Single byte (MSB=0)
- Values 128+: Multi-byte with continuation marker
  - First byte has MSB set, encodes tag and partial value
  - Subsequent bytes encode remaining value in little-endian order
- Example: 0xa1 (161) → `[0x80, 0xa1]`
  - 0x80 = tag byte (indicates 1 extra byte follows)
  - 0xa1 = value byte

**CRITICAL**: Server-to-client compat flags are **unidirectional** (compat.c:736-738):
- Server (`am_server=true`): calls `write_varint(f_out, compat_flags)`
- Client (`am_server=false`): calls `read_varint(f_in)`
- No bidirectional exchange

---

## Roles

| Role     | Description                              |
|----------|------------------------------------------|
| Sender   | Emits file list and blocks               |
| Receiver | Applies deltas and writes to disk        |
| Generator| Builds file list, coordinates transfer   |

---

## Daemon Mode Protocol Flow

1. **TCP Accept**: Daemon accepts connection on port 873
2. **@RSYNCD Handshake**:
   - Server sends `@RSYNCD: <version>` greeting
   - Client sends module name
   - Server responds with `@RSYNCD: OK`
3. **Argument Exchange**:
   - Client sends command-line arguments as line-delimited text
   - Server parses and validates
4. **Binary Protocol**:
   - Server calls `setup_protocol()`:
     - Sends compatibility flags (plain varint)
   - Server calls `io_start_multiplex_out()` (flushes + enables output multiplex)
   - Server calls `start_server()`:
     - Activates INPUT multiplex (sender role, protocol >= 30)
     - Reads filter list (multiplexed)
     - Dispatches to Generator/Receiver roles
5. **Transfer Phase**: Multiplexed I/O for file list and content

---

## Wire Format References

Based on upstream rsync 3.4.1 source code:
- `io.c`: Multiplex frame construction (SIVAL macro, little-endian)
- `main.c:1245-1265`: `start_server()` multiplex activation sequence
- `compat.c:710-744`: Compatibility flags exchange
- `clientserver.c:1128-1210`: Daemon protocol flow

---

## Troubleshooting

### Common Protocol Errors

**"unexpected tag N"** - Byte synchronization error in multiplex stream:
- Check multiplex activation sequence (OUTPUT before INPUT)
- Verify compat flags sent as plain data before OUTPUT multiplex
- Confirm stream flush happens in `io_start_multiplex_out()`
- Verify no duplicate stream cloning after protocol negotiation

**"protocol mismatch"** - Version negotiation failure:
- Server must support protocol 28-32
- Check @RSYNCD handshake for daemon mode
- Verify binary protocol exchange for SSH mode

**"invalid multi-message"** - Malformed multiplex frame:
- Verify little-endian byte order: `[len_low, len_mid, len_high, tag]`
- Check tag calculation: `tag = MPLEX_BASE + message_code` (MPLEX_BASE=7)
- Ensure payload length matches actual data

---
