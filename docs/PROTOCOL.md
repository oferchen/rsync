# oc-rsync Protocol 32 Implementation

This document describes the wire protocol negotiation, framing, roles, and state machine logic used to implement upstream rsync 3.4.1 Protocol 32.

---

## Protocol Version

- oc-rsync must always emit: `protocol version = 32`
- Must accept fallback negotiation to 28–32
- Fails with explicit error for unsupported versions

---

## Capability Bitmask

Capabilities must be announced/negotiated:

| Capability Bit       | Meaning                              |
|----------------------|--------------------------------------|
| `xattrs`             | Preserve extended attributes         |
| `acls`               | Preserve POSIX ACLs                  |
| `symlink-times`      | Timestamp preservation for symlinks  |
| `iconv`              | Supports character set conversion    |
| `delete-during`      | Mid-transfer deletes allowed         |
| `partial-dir`        | Use partial temp directories         |
| `msgs2stderr`        | Info/debug sent to stderr channel    |

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

| Code | Tag | Name             | Purpose                          |
|------|-----|------------------|----------------------------------|
| 0    | 7   | `MSG_DATA`       | File list, file content blocks   |
| 1    | 8   | `MSG_ERROR_XFER` | Non-fatal transfer errors        |
| 2    | 9   | `MSG_INFO`       | Informational messages           |
| 3    | 10  | `MSG_ERROR`      | Fatal errors (triggers exit)     |
| 4    | 11  | `MSG_WARNING`    | Warning messages                 |
| 22   | 29  | `MSG_IO_ERROR`   | I/O error during transfer        |
| 42   | 49  | `MSG_NOOP`       | Keep-alive / flow control        |
| 100  | 107 | `MSG_SUCCESS`    | Transfer success notification    |

### Activation Sequence

**CRITICAL**: Multiplex activation must follow this exact sequence to match upstream rsync:

1. **Setup Phase** (protocol negotiation, pre-multiplex):
   - Daemon: `@RSYNCD:` text handshake
   - Exchange protocol version
   - Server calls `setup_protocol(f_out, f_in)` which:
     - Sends compatibility flags as **plain varint** (if protocol >= 30)
     - Flushes output buffer

2. **Multiplex Activation** (happens AFTER setup_protocol):
   ```
   if (protocol_version >= 23)
       io_start_multiplex_out(f_out);  // OUTPUT only!
   ```
   - **OUTPUT multiplex activated immediately**
   - **INPUT multiplex deferred** until after filter list exchange

3. **Filter List Exchange** (pre-input-multiplex):
   - Sender reads filter list as **plain data** (not multiplexed)
   - Upstream: `recv_filter_list(f_in)` at main.c:1259
   - Client sends filter rules terminated by varint(0)

4. **Conditional INPUT Multiplex**:
   - **Protocol >= 30**: `need_messages_from_generator = 1` (compat.c:776)
     - Generator role: `io_start_multiplex_in(f_in)` (main.c:1254-1255)
     - Receiver role: NO INPUT multiplex activation
   - **Protocol < 30**: Uses `io_start_buffering_in(f_in)` instead (main.c:1257)
   - Filter list read BEFORE INPUT multiplex activation (main.c:1258)

5. **Transfer Phase**:
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

| Bit | Flag                   | Meaning                              |
|-----|------------------------|--------------------------------------|
| 0x01| `CF_INC_RECURSE`       | Incremental recursion supported      |
| 0x20| `CF_CHECKSUM_SEED_FIX` | Proper checksum seed ordering        |
| 0x80| `CF_VARINT_FLIST_FLAGS`| File list flags use varint encoding  |

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
     - Flushes output
   - Server activates OUTPUT multiplex (if protocol >= 23)
   - Server calls `start_server()`:
     - Reads filter list (plain data)
     - Activates INPUT multiplex (conditional)
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
- Ensure filter list read as plain data before INPUT multiplex
- Confirm stream flush after compat flags write
- Verify no duplicate stream cloning after protocol negotiation

**"protocol mismatch"** - Version negotiation failure:
- Server must support protocol 28-32
- Check @RSYNCD handshake for daemon mode
- Verify binary protocol exchange for SSH mode

**"invalid multi-message"** - Malformed multiplex frame:
- Verify little-endian byte order: `[len_low, len_mid, len_high, tag]`
- Check tag calculation: `tag = MPLEX_BASE + message_code` (MPLEX_BASE=7)
- Ensure payload length matches actual data

### Debugging Daemon Mode

**CRITICAL**: Standard debugging tools unavailable in daemon mode:
- `stderr` is closed/redirected (eprintln! panics silently)
- `dbg!()` and `println!()` are unusable
- Worker thread crashes manifest as protocol errors on client

**File-based checkpoint debugging**:
```rust
let _ = std::fs::write("/tmp/checkpoint_name", "data");
```

**Checkpoint placement strategy**:
1. Function entry points
2. Before/after critical operations (flush, multiplex activation)
3. Branch points (protocol version checks)
4. Protocol data writes (log actual bytes)

**Gap analysis**: If checkpoint A exists but B does not:
- Code between A and B crashed, never executed, or has early return
- Check for hidden `eprintln!()` in dependencies
- Verify no panics in low-level protocol functions

### Protocol Validation

**Varint encoding verification**:
```bash
# For value 161 (0xa1), expect [0x80, 0xa1]
echo "Value: 161" | your_encoder | xxd
# Should output: 80 a1
```

**Multiplex frame verification**:
```bash
# For 21-byte MSG_DATA (code=0), expect [0x15, 0x00, 0x00, 0x07]
echo "Frame with 21 bytes" | your_framer | xxd | head -1
# Should output: 15 00 00 07 [21 bytes of payload]
```

**Stream cloning verification**:
- Clone TCP stream ONCE before protocol negotiation
- Use same cloned handles throughout entire session
- Never re-clone after compat flags exchange
- Both handles point to same kernel socket (no buffering mismatch)

---

## Framing Tests

Required:

- Upstream → oc-rsync frame parsing
- oc-rsync → upstream receiver tests
- Out-of-order frame recovery
- Error handling on invalid tags

---
