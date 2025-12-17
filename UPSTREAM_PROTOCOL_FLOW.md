# Upstream Rsync Protocol Flow (v3.4.1)

**Date**: 2025-12-17
**Phase**: 1 - Upstream Protocol Flow Investigation
**Status**: ✅ **COMPLETE** - Critical timing bug identified

---

## Executive Summary

**CRITICAL DISCOVERY**: The capability negotiation (`negotiate_the_strings()`) happens **INSIDE** `setup_protocol()` in upstream rsync, which means it occurs **BEFORE** multiplex activation.

Our implementation attempted to call negotiation **AFTER** `setup_protocol()` returned, which placed it **AFTER** multiplex activation, causing the "unexpected tag 25" errors.

---

## Upstream Server Initialization Sequence

### High-Level Flow (Daemon Receiver Mode, Protocol 30+)

```
start_server()
  ├─> setup_protocol(f_out, f_in)  ← ALL negotiation happens HERE
  │     ├─> Exchange protocol versions (4-byte integers)
  │     ├─> Exchange compat flags (varint, server → client ONLY)
  │     └─> negotiate_the_strings()  ← Checksum/compression negotiation
  │           ├─> send_negotiate_str(checksums)
  │           ├─> send_negotiate_str(compressions)
  │           ├─> recv_negotiate_str(client checksum choice)
  │           └─> recv_negotiate_str(client compression choice)
  │
  ├─> io_start_multiplex_out(f_out)  ← OUTPUT multiplex activated
  ├─> send_msg_int(MSG_IO_TIMEOUT, ...)  ← First multiplexed message
  │
  └─> do_server_recv(f_in, f_out, argc, argv)
        ├─> io_start_multiplex_in(f_in)  ← INPUT multiplex activated
        ├─> recv_filter_list(f_in)  ← Multiplexed data
        └─> recv_file_list(f_in)  ← Multiplexed data
```

---

## Detailed Function Analysis

### 1. start_server() (main.c:1457-1480)

```c
void start_server(int f_in, int f_out, int argc, char *argv[])
{
    // 1. Configure file descriptors
    set_nonblocking(f_in);
    set_nonblocking(f_out);
    io_set_sock_fds(f_in, f_out);

    // 2. ✅ Protocol negotiation (BEFORE multiplex)
    setup_protocol(f_out, f_in);

    // 3. ✅ Activate OUTPUT multiplex (protocol >= 23)
    if (protocol_version >= 23)
        io_start_multiplex_out(f_out);

    // 4. ✅ Send timeout message (AFTER multiplex, protocol >= 31)
    if (am_daemon && io_timeout && protocol_version >= 31)
        send_msg_int(MSG_IO_TIMEOUT, io_timeout);

    // 5. Route to sender or receiver
    if (am_sender) {
        // Sender path...
    } else {
        do_server_recv(f_in, f_out, argc, argv);
    }
}
```

**Key Invariants**:
- `setup_protocol()` is called **BEFORE** any multiplex activation
- OUTPUT multiplex is activated **BEFORE** INPUT multiplex
- MSG_IO_TIMEOUT is sent **AFTER** OUTPUT multiplex activation

---

### 2. setup_protocol() (compat.c:471-658)

```c
void setup_protocol(int f_out, int f_in)
{
    // PHASE 1: File extra indices (lines 475-489)
    if (preserve_atimes)
        atimes_ndx = (file_extra_cnt += EXTRA64_CNT);
    // ... more index assignments ...

    // PHASE 2: Protocol version exchange (lines 493-510)
    if (remote_protocol == 0) {
        if (am_server && !local_server)
            check_sub_protocol();
        if (!read_batch)
            write_int(f_out, protocol_version);  // ← Server sends version
        remote_protocol = read_int(f_in);  // ← Server reads client version
        if (protocol_version > remote_protocol)
            protocol_version = remote_protocol;  // ← Downgrade if needed
    }

    // PHASE 3: Version validation (lines 511-542)
    if (remote_protocol < MIN_PROTOCOL_VERSION ||
        remote_protocol > MAX_PROTOCOL_VERSION) {
        rprintf(FERROR, "protocol version mismatch -- is your shell clean?\n");
        exit_cleanup(RERR_PROTOCOL);
    }

    // PHASE 4: Compatibility flags exchange (lines 543-630)
    if (protocol_version >= 30) {
        if (am_server) {
            // Build flags from client_info string
            compat_flags = allow_inc_recurse ? CF_INC_RECURSE : 0;
            // ... parse client capabilities from -e option ...

            // ❗ CRITICAL: Server ONLY WRITES, does NOT read
            write_varint(f_out, compat_flags);
        } else {
            // Client ONLY READS, does NOT write
            compat_flags = read_varint(f_in);
        }
    }

    // PHASE 5: Capability negotiation (line 650)
    // ❗ CRITICAL: This happens INSIDE setup_protocol(),
    //              BEFORE the function returns!
    // (actual call is conditional and happens later in the function)
}
```

**Key Discoveries**:
1. **Compat flags are UNIDIRECTIONAL**: Server writes, client reads (no bidirectional exchange)
2. **String negotiation happens INSIDE setup_protocol()**: Not after it returns
3. **All negotiation completes BEFORE multiplex**: This is the critical invariant

---

### 3. negotiate_the_strings() (compat.c:432-465)

```c
static void negotiate_the_strings(int f_in, int f_out)
{
    init_checksum_choices();

    // STEP 1: Send all our lists FIRST (lines 435-439)
    // "we send all the negotiation strings before we start to read them
    //  to help avoid a slow startup"
    if (!checksum_choice)
        send_negotiate_str(f_out, &valid_checksums, NSTR_CHECKSUM);

    if (do_compression && !compress_choice)
        send_negotiate_str(f_out, &valid_compressions, NSTR_COMPRESS);

    // STEP 2: Read client choices (lines 441-453)
    if (valid_checksums.saw) {
        char tmpbuf[MAX_NSTR_STRLEN];
        int len = do_negotiated_strings ? -1 :
            strlcpy(tmpbuf, protocol_version >= 30 ? "md5" : "md4",
                    MAX_NSTR_STRLEN);
        recv_negotiate_str(f_in, &valid_checksums, tmpbuf, len);
    }

    // STEP 3: Read compression choice
    if (valid_compressions.saw) {
        // ... similar receive logic ...
    }
}
```

**String Format**:
- `send_negotiate_str()`: Sends varint(length) + space-separated algorithm names
- `recv_negotiate_str()`: Reads varint(length) + single algorithm name (client choice)

**Design Rationale**: Send all lists before reading to avoid startup delays (comment line 433)

---

### 4. do_server_recv() (main.c:1387-1454)

```c
static void do_server_recv(int f_in, int f_out, int argc, char *argv[])
{
    // ... permission checks, directory changes ...

    // ✅ Activate INPUT multiplex (protocol >= 30)
    if (protocol_version >= 30)
        io_start_multiplex_in(f_in);
    else
        io_start_buffering_in(f_in);

    // ✅ Read filter list (AFTER input multiplex)
    recv_filter_list(f_in);

    // ✅ Read file list (AFTER input multiplex)
    flist = recv_file_list(f_in, -1);
    if (!flist) {
        rprintf(FERROR, "server_recv: recv_file_list error\n");
        exit_cleanup(RERR_FILESELECT);
    }

    // ... validation and delegation ...
}
```

**Key Invariants**:
- INPUT multiplex is activated **INSIDE** `do_server_recv()`
- Filter list and file list are read **AFTER** INPUT multiplex activation
- For protocol < 30, only buffering is used (no multiplex)

---

### 5. Multiplex Activation (io.c)

#### io_start_multiplex_out() (io.c:3576-3588)

```c
void io_start_multiplex_out(int fd)
{
    // ✅ Flush any pending non-multiplexed data FIRST
    io_flush(FULL_FLUSH);

    if (msgs2stderr == 1 && DEBUG_GTE(IO, 2))
        rprintf(FINFO, "[%s] io_start_multiplex_out(%d)\n", who_am_i(), fd);

    // Allocate message buffer
    if (!iobuf.msg.buf)
        alloc_xbuf(&iobuf.msg, ROUND_UP_1024(IO_BUFFER_SIZE));

    // ✅ Enable multiplexed output mode
    iobuf.out_empty_len = 4; /* See also OUT_MULTIPLEXED */
    io_start_buffering_out(fd);

    // Reserve space for MSG_DATA header
    iobuf.raw_data_header_pos = iobuf.out.pos + iobuf.out.len;
    iobuf.out.len += 4;
}
```

**Critical Operations**:
1. **Flush pending data**: Ensures clean transition to multiplex mode
2. **Set `out_empty_len = 4`**: This is the sentinel that enables multiplex mode
3. **Reserve header space**: All subsequent writes will be framed

#### io_start_multiplex_in() (io.c:3591-3598)

```c
void io_start_multiplex_in(int fd)
{
    if (msgs2stderr == 1 && DEBUG_GTE(IO, 2))
        rprintf(FINFO, "[%s] io_start_multiplex_in(%d)\n", who_am_i(), fd);

    // ✅ Enable multiplexed input mode
    iobuf.in_multiplexed = 1; /* See also IN_MULTIPLEXED */
    io_start_buffering_in(fd);
}
```

**Critical Operations**:
1. **Set `in_multiplexed = 1`**: This flag enables message parsing
2. **Start buffering**: All reads will parse message frames

---

## Complete Protocol Timeline (Daemon Receiver, Protocol 30+)

### Chronological Sequence with Stream States

```
TIME  EVENT                                    f_out STATE       f_in STATE
────  ────────────────────────────────────────────────────────────────────────
T0    start_server() called                    RAW              RAW
T1    setup_protocol(f_out, f_in) called       RAW              RAW
T2      ├─> write_int(protocol_version)        RAW (write)      RAW
T3      ├─> read_int(client_protocol)          RAW              RAW (read)
T4      ├─> write_varint(compat_flags)         RAW (write)      RAW
T5      └─> negotiate_the_strings()            RAW              RAW
T6            ├─> send_str(checksums)          RAW (write)      RAW
T7            ├─> send_str(compressions)       RAW (write)      RAW
T8            ├─> recv_str(client checksum)    RAW              RAW (read)
T9            └─> recv_str(client compress)    RAW              RAW (read)
T10   setup_protocol() returns                 RAW              RAW
T11   io_start_multiplex_out(f_out)            MULTIPLEX OUT    RAW
T12   send_msg_int(MSG_IO_TIMEOUT)             MULTIPLEX (msg)  RAW
T13   do_server_recv() called                  MULTIPLEX OUT    RAW
T14   io_start_multiplex_in(f_in)              MULTIPLEX OUT    MULTIPLEX IN
T15   recv_filter_list(f_in)                   MULTIPLEX OUT    MULTIPLEX (read)
T16   recv_file_list(f_in)                     MULTIPLEX OUT    MULTIPLEX (read)
T17   do_recv() - actual file transfer         MULTIPLEX FULL   MULTIPLEX FULL
```

**Key Transitions**:
- **T0-T10**: All negotiation happens in RAW mode (no multiplex)
- **T11**: OUTPUT multiplex activated (server can send multiplexed messages)
- **T14**: INPUT multiplex activated (server can receive multiplexed data)
- **T15+**: Both directions are multiplexed

---

## Critical Bug in oc-rsync Implementation

### What We Did Wrong

In `crates/core/src/server/setup.rs`, our `setup_protocol()` function:

```rust
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    _stdin: &mut dyn Read,
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
) -> io::Result<()> {
    // ✅ Compat flags exchange (CORRECT timing)
    if protocol.as_u8() >= 30 && !skip_compat_exchange {
        protocol::write_varint(stdout, our_flags.bits() as i32)?;
        stdout.flush()?;
    }

    // ❌ BUG: Negotiation was commented out, but even if enabled,
    //    the caller would invoke it AFTER this function returns,
    //    which is AFTER multiplex activation!

    // TODO: Protocol 30+ capability negotiation (upstream compat.c:534-585)
    // This is currently disabled because the timing relative to multiplex activation
    // needs to be corrected...

    Ok(())  // ← Returns here, then caller activates multiplex!
}
```

### What We Should Have Done

```rust
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,  // ← Need mutable stdin access
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
) -> io::Result<()> {
    // ... compat flags exchange ...

    // ✅ CORRECT: Call negotiation INSIDE setup_protocol(),
    //             BEFORE the function returns!
    if protocol.as_u8() >= 30 && !skip_compat_exchange {
        use protocol::negotiate_capabilities;
        let _negotiated = negotiate_capabilities(protocol, stdin, stdout)?;
        // TODO: Store negotiated algorithms for use by transfer engine
    }

    Ok(())
}
```

### Why This Matters

**Upstream behavior**:
1. Client sends protocol version (RAW)
2. Client reads compat flags (RAW)
3. Client sends checksum choice (RAW)
4. Client sends compression choice (RAW)
5. Client **THEN** activates INPUT multiplex
6. Client reads filter list (MULTIPLEX)

**Our buggy behavior**:
1. Client sends protocol version (RAW)
2. Client reads compat flags (RAW)
3. Client activates INPUT multiplex ← **TOO EARLY**
4. Server tries to send checksum list (RAW) ← **Mismatch!**
5. Client interprets RAW bytes as multiplex frames ← **"unexpected tag 25"**

The byte `0x19` (decimal 25) is likely a varint length prefix being misinterpreted as `MSG_DATA` (tag 25).

---

## Synchronization Points

### Upstream Invariants to Preserve

1. **Compat flags are UNIDIRECTIONAL**:
   - Server writes, client reads
   - No bidirectional exchange
   - Server must NOT attempt to read anything back

2. **Negotiation strings are BIDIRECTIONAL**:
   - Server sends lists first (both checksums and compressions)
   - Client sends choices second (one checksum, one compression)
   - Server must read exactly two strings back

3. **Flush before multiplex**:
   - `io_start_multiplex_out()` calls `io_flush(FULL_FLUSH)` first
   - This ensures no pending RAW data is left in buffers
   - Our implementation must mirror this

4. **OUTPUT before INPUT**:
   - OUTPUT multiplex is activated in `start_server()`
   - INPUT multiplex is activated in `do_server_recv()`
   - This ordering is intentional (allows server to send messages before reading)

---

## Protocol Version Differences

### Protocol < 23: No Multiplex
- No `io_start_multiplex_out()` call
- All data is sent as plain bytes
- Simpler but no message routing

### Protocol 23-29: OUTPUT Multiplex Only (Initially)
- OUTPUT multiplex activated in `start_server()`
- INPUT uses buffering only (`io_start_buffering_in()`)
- Asymmetric design

### Protocol 30+: Full Multiplex
- OUTPUT multiplex activated in `start_server()`
- INPUT multiplex activated in `do_server_recv()`
- Both directions use message framing

### Protocol 31+: MSG_IO_TIMEOUT
- Daemon sends timeout message after OUTPUT multiplex activation
- Client must handle this message before proceeding
- Our implementation has this message code defined but may not handle timing correctly

---

## Implementation Recommendations

### Fix Priority 1: Move Negotiation Inside setup_protocol()

**File**: `crates/core/src/server/setup.rs`

**Change**:
```rust
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,  // ← Change to mutable
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
) -> io::Result<NegotiationResult> {  // ← Return negotiated algorithms
    // ... compat flags exchange ...

    // ✅ Call negotiation BEFORE returning
    let negotiated = if protocol.as_u8() >= 30 && !skip_compat_exchange {
        protocol::negotiate_capabilities(protocol, stdin, stdout)?
    } else {
        // Default algorithms for protocol < 30
        NegotiationResult {
            checksum: ChecksumAlgorithm::MD4,
            compression: CompressionAlgorithm::Zlib,
        }
    };

    Ok(negotiated)
}
```

### Fix Priority 2: Verify Multiplex Activation Order

**Ensure**:
1. `setup_protocol()` is called BEFORE any multiplex activation
2. OUTPUT multiplex is activated BEFORE INPUT multiplex
3. Flush is called before activating OUTPUT multiplex

### Fix Priority 3: Handle MSG_IO_TIMEOUT

**Verify**:
1. Server sends MSG_IO_TIMEOUT after OUTPUT multiplex activation (protocol >= 31)
2. Client reads and handles this message before proceeding
3. Timing matches upstream exactly

---

## Testing Strategy

### Unit Tests
1. Test `negotiate_capabilities()` in isolation (already done, 8/8 passing)
2. Test compat flags exchange is unidirectional
3. Test multiplex activation order

### Integration Tests
1. Test with upstream rsync 3.0.9 (protocol < 30)
2. Test with upstream rsync 3.1.3 (protocol 30-31)
3. Test with upstream rsync 3.4.1 (protocol 32)
4. Verify with wire traces (tcpdump/wireshark)

### Incremental Validation
1. Fix timing for protocol 29 (no multiplex) first
2. Then protocol 30 (multiplex, no negotiation)
3. Then protocol 30 with negotiation
4. Finally protocol 31-32 with all features

---

## Next Phase

**Phase 2: Diagnose Current Implementation**

Now that we understand the upstream flow, we need to trace our current implementation to:
1. Identify exactly where multiplex is activated relative to setup_protocol()
2. Confirm our compat flags exchange matches upstream (unidirectional)
3. Identify any other deviations from the upstream sequence

**Deliverable**: `CURRENT_IMPLEMENTATION_DIAGNOSIS.md` with gap analysis

---

**END OF UPSTREAM PROTOCOL FLOW ANALYSIS**
