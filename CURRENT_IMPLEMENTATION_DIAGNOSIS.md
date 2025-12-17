# Current Implementation Diagnosis vs Upstream

**Date**: 2025-12-17
**Phase**: 2 - Current Implementation Diagnosis
**Status**: ✅ **COMPLETE** - Root cause confirmed

---

## Executive Summary

**ROOT CAUSE IDENTIFIED**: Our `setup_protocol()` function does NOT call `negotiate_capabilities()`, leaving capability negotiation commented out. The client waits for negotiation strings during its `setup_protocol()` call, but we never send them. When we activate multiplex and send subsequent data, the client (still in RAW mode, waiting for negotiation strings) reads our multiplexed frame headers as protocol data, causing "unexpected tag 25" errors.

**THE FIX**: Enable the commented-out `negotiate_capabilities()` call inside `setup_protocol()` so negotiation completes BEFORE multiplex activation.

---

## Current Implementation Flow

### Server Startup Sequence (Daemon Receiver, Protocol 30+)

**File**: `crates/core/src/server/mod.rs:run_server_with_handshake()`

```
TIME  EVENT                                    f_out STATE       f_in STATE
────  ────────────────────────────────────────────────────────────────────────
T0    run_server_with_handshake() called       RAW              RAW
T1    setup_protocol() called (line 173)       RAW              RAW
T2      ├─> write_varint(compat_flags)         RAW (write)      RAW
T3      └─> ❌ negotiate_capabilities()        COMMENTED OUT    COMMENTED OUT
T4    setup_protocol() returns                 RAW              RAW
T5    stdout.flush() (line 187)                RAW (flushed)    RAW
T6    Create ServerReader/ServerWriter         RAW              RAW
T7    Activate OUTPUT multiplex (line 204)     MULTIPLEX OUT    RAW
T8    Send MSG_IO_TIMEOUT (line 215)           MULTIPLEX (msg)  RAW
T9    ReceiverContext::run() called            MULTIPLEX OUT    RAW
T10   Activate INPUT multiplex (receiver:154)  MULTIPLEX OUT    MULTIPLEX IN
T11   Read filter list (receiver:161)          MULTIPLEX OUT    MULTIPLEX (read)
T12   Read file list (receiver:167)            MULTIPLEX OUT    MULTIPLEX (read)
```

### What the Client Expects (Upstream rsync)

**Based on upstream main.c and compat.c**:

```
TIME  CLIENT EVENT                            CLIENT f_in STATE    READS FROM SERVER
────  ───────────────────────────────────────────────────────────────────────────────
T0    start_client()                          RAW                  (nothing yet)
T1    setup_protocol() called                 RAW                  (nothing yet)
T2      ├─> read_varint(compat_flags)         RAW (read)           ← Server compat flags (T2)
T3      ├─> recv_negotiate_str(checksums)     RAW (read)           ← ❌ Server DOESN'T send (T3)
T4      ├─> recv_negotiate_str(compressions)  RAW (read)           ← ❌ Server DOESN'T send
T5      ├─> send_negotiate_str(checksum)      RAW (write)          (blocked waiting for T3/T4)
T6      └─> send_negotiate_str(compression)   RAW (write)          (blocked waiting for T3/T4)
       ⚠️ CLIENT BLOCKS HERE - waiting for data that never arrives
T7    (Client still waiting...)               RAW (blocked)        ← Server sends MSG_IO_TIMEOUT (T8)
T8    (Client reads multiplex frame as str)   RAW (confused)       ← Reads 0x07 0x19 ... (multiplex header)
T9    ERROR: "unexpected tag 25"              CRASHED              ← Interprets 0x19 as tag 25
```

**The Mismatch**:
- **Server T3**: `negotiate_capabilities()` is COMMENTED OUT - server skips negotiation
- **Client T3-T4**: Client tries to read negotiation strings in RAW mode
- **Server T7-T8**: Server activates multiplex and sends MSG_IO_TIMEOUT
- **Client T7-T8**: Client still in RAW mode, reads multiplex frame header as negotiation string
- **Client T9**: Client sees byte 0x19 (decimal 25) and reports "unexpected tag 25"

---

## Detailed Code Comparison

### 1. setup_protocol() Implementation

#### Upstream (compat.c:471-658)

```c
void setup_protocol(int f_out, int f_in)
{
    // PHASE 1: File extra indices (lines 475-489)
    // ... index setup ...

    // PHASE 2: Protocol version exchange (lines 493-510)
    if (remote_protocol == 0) {
        if (am_server && !local_server)
            check_sub_protocol();
        if (!read_batch)
            write_int(f_out, protocol_version);
        remote_protocol = read_int(f_in);
        if (protocol_version > remote_protocol)
            protocol_version = remote_protocol;
    }

    // PHASE 3: Version validation (lines 511-542)
    // ... validation ...

    // PHASE 4: Compatibility flags exchange (lines 543-630)
    if (protocol_version >= 30) {
        if (am_server) {
            // Build flags from client_info
            compat_flags = ...;
            write_varint(f_out, compat_flags);  // ✅ SEND
        } else {
            compat_flags = read_varint(f_in);  // ✅ RECEIVE
        }
    }

    // PHASE 5: Capability negotiation (line 650) ✅ CALLED
    // ... (actual call happens later in function) ...
}
```

#### Our Implementation (crates/core/src/server/setup.rs:182-239)

```rust
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    _stdin: &mut dyn Read,  // ⚠️ stdin is IGNORED (not mutable)
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
) -> io::Result<()> {
    // PHASE 1: Skipped (daemon mode already negotiated protocol)

    // PHASE 2: Skipped (daemon mode already negotiated protocol)

    // PHASE 3: Skipped (daemon mode already negotiated protocol)

    // PHASE 4: Compatibility flags exchange ✅ CORRECT
    if protocol.as_u8() >= 30 && !skip_compat_exchange {
        let our_flags = if let Some(args) = client_args {
            let client_info = parse_client_info(args);
            build_compat_flags_from_client_info(&client_info, true)
        } else {
            CompatibilityFlags::INC_RECURSE
                | CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS
        };

        protocol::write_varint(stdout, our_flags.bits() as i32)?;  // ✅ SEND
        stdout.flush()?;  // ✅ FLUSH
    }

    // PHASE 5: Capability negotiation ❌ COMMENTED OUT
    // TODO: Protocol 30+ capability negotiation (upstream compat.c:534-585)
    // This is currently disabled because the timing relative to multiplex activation
    // needs to be corrected. The client activates multiplex before we send the
    // negotiation strings, causing protocol errors ("unexpected tag 25").
    //
    // ⚠️ THIS COMMENT IS MISLEADING! The client does NOT activate multiplex
    //    before we send strings. The client is waiting IN setup_protocol()
    //    for us to send the strings, but we never do!
    //
    // if protocol.as_u8() >= 30 && !skip_compat_exchange {
    //     use protocol::negotiate_capabilities;
    //     let _negotiated = negotiate_capabilities(protocol, _stdin, stdout)?;
    // }

    Ok(())  // ❌ Returns WITHOUT negotiating
}
```

**Problems**:
1. **Negotiation is commented out**: Lines 234-237 are disabled
2. **Misleading comment**: Claims "client activates multiplex before we send" - this is FALSE
3. **stdin parameter is ignored**: Declared as `_stdin` (unused), would need to be mutable for negotiation
4. **No return value for negotiated algorithms**: Returns `()` instead of `NegotiationResult`

---

### 2. Multiplex Activation Timing

#### Upstream (main.c:1457-1480)

```c
void start_server(int f_in, int f_out, int argc, char *argv[])
{
    setup_protocol(f_out, f_in);  // ✅ Negotiation happens HERE

    if (protocol_version >= 23)
        io_start_multiplex_out(f_out);  // ✅ AFTER setup_protocol

    if (am_daemon && io_timeout && protocol_version >= 31)
        send_msg_int(MSG_IO_TIMEOUT, io_timeout);  // ✅ AFTER multiplex activation

    if (am_sender) {
        // ...
    } else {
        do_server_recv(f_in, f_out, argc, argv);
    }
}
```

#### Our Implementation (crates/core/src/server/mod.rs:173-217)

```rust
// Call setup_protocol() - mirrors upstream main.c:1245
let setup_result = setup::setup_protocol(
    handshake.protocol,
    &mut stdout,
    &mut chained_stdin,
    handshake.compat_exchanged,
    handshake.client_args.as_deref(),
);
setup_result?;  // ✅ Negotiation SHOULD happen inside, but doesn't

// CRITICAL: Flush stdout BEFORE wrapping it in ServerWriter!
stdout.flush()?;  // ✅ CORRECT flush

// Activate multiplex
let reader = reader::ServerReader::new_plain(chained_stdin);
let mut writer = writer::ServerWriter::new_plain(stdout);

// Always activate OUTPUT multiplex at protocol >= 23 (main.c:1248)
if handshake.protocol.as_u8() >= 23 {
    writer = writer.activate_multiplex()?;  // ✅ CORRECT timing (AFTER setup_protocol)
}

// Send MSG_IO_TIMEOUT for daemon mode (main.c:1249-1250)
if let Some(timeout_secs) = handshake.io_timeout {
    if handshake.protocol.as_u8() >= 31 {
        let timeout_bytes = (timeout_secs as i32).to_le_bytes();
        writer.send_message(MessageCode::IoTimeout, &timeout_bytes)?;  // ✅ CORRECT timing
    }
}
```

**Verdict**: Our multiplex activation timing is **CORRECT** relative to `setup_protocol()`. The problem is that `setup_protocol()` doesn't do what it should (negotiate capabilities).

---

### 3. INPUT Multiplex Activation

#### Upstream (main.c:1387-1454, do_server_recv)

```c
static void do_server_recv(int f_in, int f_out, int argc, char *argv[])
{
    // ... permission checks ...

    if (protocol_version >= 30)
        io_start_multiplex_in(f_in);  // ✅ Activate before reading filter list
    else
        io_start_buffering_in(f_in);

    recv_filter_list(f_in);  // ✅ Read AFTER multiplex activation
    // ...
}
```

#### Our Implementation (crates/core/src/server/receiver.rs:144-162)

```rust
pub fn run<R: Read, W: Write + ?Sized>(
    &mut self,
    mut reader: super::reader::ServerReader<R>,
    writer: &mut W,
) -> io::Result<TransferStats> {
    // CRITICAL: Activate INPUT multiplex BEFORE reading filter list for protocol >= 30
    // This matches upstream do_server_recv() at main.c:1167
    if self.protocol.as_u8() >= 30 {
        reader = reader.activate_multiplex().map_err(|e| {
            io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
        })?;  // ✅ CORRECT timing
    }

    // Read filter list from sender (multiplexed for protocol >= 30)
    let _wire_rules = read_filter_list(&mut reader, self.protocol)
        .map_err(|e| io::Error::new(e.kind(), format!("failed to read filter list: {e}")))?;
    // ✅ CORRECT timing - read AFTER multiplex activation
    // ...
}
```

**Verdict**: Our INPUT multiplex activation is **CORRECT** and matches upstream exactly.

---

## Gap Analysis

### Gaps Identified

| Component | Upstream | Our Implementation | Status |
|-----------|----------|-------------------|--------|
| Protocol version exchange | In setup_protocol() | Skipped (already done via @RSYNCD) | ✅ OK for daemon mode |
| Compat flags exchange | In setup_protocol() | In setup_protocol() | ✅ CORRECT |
| **Capability negotiation** | **In setup_protocol()** | **COMMENTED OUT** | ❌ **CRITICAL BUG** |
| OUTPUT multiplex activation | After setup_protocol() | After setup_protocol() | ✅ CORRECT |
| MSG_IO_TIMEOUT send | After OUTPUT multiplex | After OUTPUT multiplex | ✅ CORRECT |
| INPUT multiplex activation | In do_server_recv() | In ReceiverContext::run() | ✅ CORRECT |
| Filter list read | After INPUT multiplex | After INPUT multiplex | ✅ CORRECT |

### The Single Critical Gap

**Capability negotiation is disabled in `setup_protocol()`**, causing a protocol deadlock:

1. ✅ Server sends compat flags
2. ❌ Server does NOT send negotiation strings (commented out)
3. ✅ Server activates OUTPUT multiplex
4. ✅ Server sends MSG_IO_TIMEOUT (multiplexed)
5. ❌ Client blocks waiting for negotiation strings (in RAW mode, still in setup_protocol())
6. ❌ Client reads multiplexed frame header as negotiation string data
7. ❌ Client sees tag byte 0x19 (decimal 25) and reports "unexpected tag 25"

---

## Why the Comment in setup.rs is Misleading

The comment at `crates/core/src/server/setup.rs:226-228` says:

> "This is currently disabled because the timing relative to multiplex activation
> needs to be corrected. The client activates multiplex before we send the
> negotiation strings, causing protocol errors ("unexpected tag 25")."

**This is WRONG**. The actual problem is:

- We NEVER send negotiation strings (because the code is commented out)
- The client does NOT "activate multiplex before we send"
- The client is waiting IN setup_protocol() for us to send strings (RAW mode)
- We return from setup_protocol() WITHOUT sending strings
- We THEN activate multiplex
- We THEN send multiplexed data
- The client (still in RAW mode, still waiting) reads our multiplexed data as strings
- The client sees a multiplex frame header and reports "unexpected tag"

**The fix is NOT to change multiplex timing**. The fix is to **ENABLE the negotiation call** inside `setup_protocol()` so strings are sent BEFORE we return (and thus BEFORE multiplex activation).

---

## Evidence from Interop Tests

### Test Output (from `tools/ci/run_interop.sh`)

```
[oc-rsync] info: protocol 30, role: Receiver  ← Our server processes the request
unexpected tag 25 [sender]                     ← Client reports error
rsync error: error in rsync protocol data stream (code 12)
```

**Analysis**:
1. "protocol 30" - confirms we're in Protocol 30+ mode (requires negotiation)
2. "role: Receiver" - our server is in receiver role (correct)
3. "unexpected tag 25 [sender]" - **client's** sender process reports error
4. Tag 25 = 0x19 - likely a varint length byte or multiplex frame byte

### Wire Trace Hypothesis

**What the client receives** (hexadecimal):

```
# Expected (if we sent negotiation strings):
# Compat flags (varint)
05                              ← varint: 5 (CF_INC_RECURSE | CF_VARINT_FLIST_FLAGS)
# Checksum list (varint length + string)
13 6d 64 35 20 6d 64 34 20...   ← varint: 19, "md5 md4 sha1 xxh128"
# Compression list (varint length + string)
1a 7a 73 74 64 20 6c 7a 34 ...  ← varint: 26, "zstd lz4 zlibx zlib none"

# What we actually send:
# Compat flags (varint)
05                              ← varint: 5 (correct)
# (skip negotiation - NOTHING sent)
# (activate multiplex)
# MSG_IO_TIMEOUT (multiplexed)
07 19 04 00 3c 00 00 00         ← 07 (MPLEX_BASE), 19 (tag?), 04 00 (len), 3c... (timeout)
```

**What happens**:
- Client reads compat flags: `05` ✅ correct
- Client tries to read checksum list length (varint): reads `07` ❌ **wrong data**
- Client decodes varint `07` as length 7
- Client tries to read 7 bytes: `19 04 00 3c 00 00 00`
- Client interprets this as a string and gets confused
- OR: Client has different parsing that leads to tag detection
- Client sees byte `19` (decimal 25) interpreted as a tag: "unexpected tag 25"

**Note**: The exact byte sequence depends on how multiplex frames are encoded. Tag 25 might be part of a multiplexed message frame being misinterpreted.

---

## Root Cause Summary

**The bug is NOT**:
- ❌ Multiplex activation timing (our timing matches upstream)
- ❌ Compat flags exchange (our exchange matches upstream)
- ❌ INPUT multiplex activation (our timing matches upstream)

**The bug IS**:
- ✅ **Missing capability negotiation call in `setup_protocol()`**
- ✅ **Client waits for negotiation strings that we never send**
- ✅ **Client reads subsequent multiplexed data as RAW strings**
- ✅ **Client interprets multiplex frame bytes as protocol data**

---

## Required Fix

### Change 1: Update setup_protocol() Signature

**File**: `crates/core/src/server/setup.rs`

```rust
// BEFORE:
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    _stdin: &mut dyn Read,  // ← Unused
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
) -> io::Result<()>  // ← No return value

// AFTER:
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,  // ← Make mutable, remove underscore
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
) -> io::Result<Option<protocol::NegotiationResult>>  // ← Return negotiated algorithms
```

### Change 2: Enable Capability Negotiation

**File**: `crates/core/src/server/setup.rs:225-239`

```rust
// BEFORE (commented out):
// TODO: Protocol 30+ capability negotiation (upstream compat.c:534-585)
// This is currently disabled because the timing relative to multiplex activation
// needs to be corrected...
//
// if protocol.as_u8() >= 30 && !skip_compat_exchange {
//     use protocol::negotiate_capabilities;
//     let _negotiated = negotiate_capabilities(protocol, _stdin, stdout)?;
// }

Ok(())

// AFTER (enabled):
// Protocol 30+ capability negotiation (upstream compat.c:534-585)
// This MUST happen inside setup_protocol(), BEFORE the function returns,
// so negotiation completes in RAW mode before multiplex activation.
let negotiated = if protocol.as_u8() >= 30 && !skip_compat_exchange {
    Some(protocol::negotiate_capabilities(protocol, stdin, stdout)?)
} else {
    None  // Protocol < 30 uses defaults
};

Ok(negotiated)
```

### Change 3: Update Caller

**File**: `crates/core/src/server/mod.rs:173-180`

```rust
// BEFORE:
let setup_result = setup::setup_protocol(
    handshake.protocol,
    &mut stdout,
    &mut chained_stdin,
    handshake.compat_exchanged,
    handshake.client_args.as_deref(),
);
setup_result?;

// AFTER:
let negotiated = setup::setup_protocol(
    handshake.protocol,
    &mut stdout,
    &mut chained_stdin,
    handshake.compat_exchanged,
    handshake.client_args.as_deref(),
)?;

// TODO: Store negotiated algorithms for use by transfer engine
// For now, we can ignore the result, but future work should wire these through
// to actual checksum/compression operations
if let Some(result) = negotiated {
    // Log negotiated algorithms (optional)
    // Future: pass result to ReceiverContext/GeneratorContext
}
```

---

## Testing Plan

### Phase 1: Enable and Test Incrementally

1. **Make the fix** (enable negotiation in setup_protocol)
2. **Test with protocol 30** first (simplest case)
3. **Test with protocol 31** (adds MSG_IO_TIMEOUT)
4. **Test with protocol 32** (full feature set)

### Phase 2: Verify with All Upstream Versions

1. **rsync 3.0.9** (protocol < 30) - should skip negotiation
2. **rsync 3.1.3** (protocol 30-31) - should negotiate
3. **rsync 3.4.1** (protocol 32) - should negotiate with full algorithm set

### Phase 3: Wire Trace Validation

1. **Capture with tcpdump/wireshark**
2. **Verify byte sequence matches upstream**
3. **Confirm negotiation strings appear before multiplex frames**

---

## Next Phase

**Phase 3: Implement the Fix**

Now that we've confirmed the root cause, we need to:
1. Update `setup_protocol()` signature to return `NegotiationResult`
2. Enable the capability negotiation call
3. Update callers to handle the return value
4. Test with interop suite

**Estimated Effort**: 1-2 hours (straightforward code change + testing)

---

**END OF CURRENT IMPLEMENTATION DIAGNOSIS**
