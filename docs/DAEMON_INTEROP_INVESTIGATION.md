# Daemon Interoperability Investigation

This document tracks the investigation into why the oc-rsync daemon fails interoperability
tests with upstream rsync 3.4.1 clients. The server completes successfully but the client
reports "connection unexpectedly closed".

## Problem Statement

When running a simple directory listing:
```bash
rsync rsync://localhost:8873/testmodule/
```

**Server behavior**: Completes successfully (TRANSFER_COMPLETE checkpoint exists)
**Client error**:
```
rsync: connection unexpectedly closed (69 bytes received so far) [receiver]
rsync error: error in rsync protocol data stream (code 12) at io.c(232) [receiver=3.4.1]
rsync: connection unexpectedly closed (56 bytes received so far) [generator]
rsync error: error in rsync protocol data stream (code 12) at io.c(232) [generator=3.4.1]
```

## Key Observation: Byte Count Discrepancy

- **Receiver** reports: 69 bytes received
- **Generator** reports: 56 bytes received
- **Difference**: 13 bytes

This suggests the client's receiver and generator processes receive different amounts of data.

## Wire Protocol Analysis

### What Our Server Sends (4 multiplexed frames, 69 bytes total)

| Frame | Type     | Header Bytes       | Payload Size | Payload Content |
|-------|----------|-------------------|--------------|-----------------|
| 1     | MSG_DATA | [0x24, 0x00, 0x00, 0x07] | 36 bytes | File list |
| 2     | MSG_DATA | [0x01, 0x00, 0x00, 0x07] | 1 byte   | NDX_DONE (0x00) |
| 3     | MSG_DATA | [0x0f, 0x00, 0x00, 0x07] | 15 bytes | Stats (5 varlong30 values) |
| 4     | MSG_DATA | [0x01, 0x00, 0x00, 0x07] | 1 byte   | NDX_DONE (0x00) |

Total: 4×4 (headers) + 36+1+15+1 (payloads) = 16 + 53 = 69 bytes

### Multiplexing Header Format
- Header: 4 bytes, little-endian
- Format: `(MPLEX_BASE + message_code) << 24 | payload_length`
- MPLEX_BASE = 7
- MSG_DATA = 0, so tag = 7

## Client Verbose Output Analysis

```
recv_files phase=1
recv_files phase=2
recv_files finished       <-- Receiver completes
generate_files phase=3    <-- Generator starts phase 3
[ERROR]                   <-- Generator fails here
```

The receiver finishes successfully, but the generator fails during phase 3.

## Key Findings

### 1. Client-to-Server Multiplexing
Despite protocol documentation stating "Transmissions received from the client are not multiplexed",
our MultiplexReader successfully reads MSG_DATA frames from the client:

| Frame | Payload | Content |
|-------|---------|---------|
| 0 | [00, 00, 00, 00] | Filter list terminator (empty) |
| 1 | [00] | Goodbye NDX_DONE #1 |
| 2 | [00, 00, 00] | ??? (3 bytes of zeros) |
| 3 | (from buffer) [00] | Goodbye NDX_DONE #2 |

**Question**: Why is frame 2 a 3-byte payload instead of 1 byte for NDX_DONE?

### 2. Server-to-Client All MSG_DATA
All our output goes as MSG_DATA (tag 7). In rsync's multiplex architecture:
- MSG_DATA routes to the **receiver** process
- Other message types route to the **generator** process

**Hypothesis**: The client's generator process never sees our NDX_DONE because it's wrapped
in MSG_DATA which goes only to the receiver.

### 3. Phase 3 Generator Expectations
From upstream generator.c analysis:
- Phase 1: Process files, send NDX_DONE
- Phase 2: Redo processing, send NDX_DONE
- Phase 3: Delete/finalize, expects MSG_DONE signals

The generator expects 3 MSG_DONE messages: one after each phase completion.

### 4. Goodbye Handshake (Protocol 31+)

**Upstream server flow** (read_final_goodbye):
1. Read NDX_DONE from client (read_ndx_and_attrs)
2. Write NDX_DONE to client (write_ndx)
3. Read final NDX_DONE from client (read_ndx_and_attrs)

**Our implementation** matches this, but:
- Our `write_ndx` goes through `ServerWriter` which wraps in MSG_DATA
- Upstream's `write_ndx` might write directly without multiplexing

### 5. Statistics Format
Both upstream and our implementation write stats as plain varlong30 values (not MSG_STATS):
- total_read (3 bytes min)
- total_written (3 bytes min)
- total_size (3 bytes min)
- flist_buildtime (3 bytes min, protocol >= 29)
- flist_xfertime (3 bytes min, protocol >= 29)

Total: 5 × 3 = 15 bytes minimum

## Code Locations

### Our Implementation
- Generator: `crates/core/src/server/generator.rs`
- Writer: `crates/core/src/server/writer.rs`
- Reader: `crates/core/src/server/reader.rs`
- Setup: `crates/core/src/server/mod.rs`

### Upstream Reference
- main.c: read_final_goodbye(), handle_stats()
- generator.c: generate_files() phases
- io.c: multiplex I/O, whine_about_eof()
- sender.c: send_files()

## Checkpoint Files

Debug checkpoint files are written to `/tmp/daemon_*`:
- `daemon_GENERATOR_RUN_ENTRY` - Generator::run() entered
- `daemon_READER_BEFORE_MULTIPLEX` - Before reader multiplex activation
- `daemon_READER_AFTER_MULTIPLEX` - After reader multiplex activation
- `daemon_BEFORE_READ_FILTER_LIST` - Before reading client filter list
- `daemon_ENTERING_TRANSFER_LOOP` - Transfer loop started
- `daemon_SENDING_NDX_DONE_1` - First NDX_DONE sent
- `daemon_SENDING_STATS` - Stats being sent
- `daemon_STATS_VALUES` - Actual stats values
- `daemon_READING_GOODBYE_NDX_1` - Reading first goodbye
- `daemon_GOT_GOODBYE_NDX_1` - First goodbye received
- `daemon_SENDING_NDX_DONE_2` - Second NDX_DONE sent
- `daemon_READING_GOODBYE_NDX_2` - Reading second goodbye
- `daemon_GOT_GOODBYE_NDX_2` - Second goodbye received
- `daemon_TRANSFER_COMPLETE` - Transfer completed successfully
- `daemon_MUX_READ_*` - MultiplexReader operations

## Hypotheses to Test

### Hypothesis 1: MSG_DATA vs Direct Write
The goodbye NDX_DONE should not be wrapped in MSG_DATA. Upstream might write directly
to the socket, bypassing the multiplexer.

**Test**: Send goodbye NDX_DONE as raw bytes, not through MultiplexWriter.

### Hypothesis 2: Missing Generator Message
The generator expects a specific message type (not MSG_DATA) to signal completion.

**Test**: Send completion markers using different message codes.

### Hypothesis 3: Multiplex Deactivation
Upstream might deactivate multiplexing before the goodbye handshake.

**Test**: Check if upstream sends raw bytes during goodbye sequence.

### Hypothesis 4: Phase Completion Signals
Each phase might require a separate completion signal sent to the generator.

**Test**: Send NDX_DONE markers for each phase using appropriate message types.

## Upstream Source Code Analysis

### Key Files Referenced
- `target/interop/upstream-src/rsync-3.4.1/main.c` - Server/client main flows
- `target/interop/upstream-src/rsync-3.4.1/io.c` - I/O functions, multiplexing
- `target/interop/upstream-src/rsync-3.4.1/generator.c` - Generator phases
- `target/interop/upstream-src/rsync-3.4.1/receiver.c` - Receiver logic
- `target/interop/upstream-src/rsync-3.4.1/sender.c` - Sender logic

### Upstream Server Sender Flow (do_server_sender, main.c:908-966)
```c
send_file_list(f_out, argc, argv);
io_start_buffering_in(f_in);
send_files(f_in, f_out);           // Ends with write_ndx(NDX_DONE)
io_flush(FULL_FLUSH);
handle_stats(f_out);               // Writes 5 varlong30 values
if (protocol_version >= 24)
    read_final_goodbye(f_in, f_out);
io_flush(FULL_FLUSH);
exit_cleanup(0);
```

### write_ndx Function (io.c:2243-2266)
For protocol >= 30, NDX_DONE is sent as a **single byte 0x00**:
```c
} else if (ndx == NDX_DONE) {
    *b = 0;
    write_buf(f, b, 1);
    return;
}
```

### write_int Function (io.c:2082-2087)
Always writes 4 bytes:
```c
void write_int(int f, int32 x) {
    char b[4];
    SIVAL(b, 0, x);
    write_buf(f, b, 4);
}
```

### CRITICAL FINDING: Client Uses Different Formats!

**Client RECEIVER** (do_recv in main.c:1065):
```c
write_int(f_out, NDX_DONE);  // 4 bytes: [0xFF, 0xFF, 0xFF, 0xFF]
```

**Client GENERATOR** (do_recv in main.c:1121):
```c
write_ndx(f_out, NDX_DONE);  // 1 byte: [0x00] (protocol >= 30)
```

**Server read_final_goodbye** (main.c:886):
```c
i = read_ndx_and_attrs(f_in, ...);  // Uses read_ndx (expects 1 byte for NDX_DONE)
```

### read_ndx Handling of 0xFF (io.c:2300-2310)
```c
if (CVAL(b, 0) == 0xFF) {
    read_buf(f, b, 1);
    prev_ptr = &prev_negative;
}
```
When reading 0xFF, it reads another byte for negative number encoding.

### Goodbye Handshake Sequence (Protocol 31+)

1. **Server** sends: NDX_DONE (1 byte via write_ndx) + stats
2. **Client receiver** reads: NDX_DONE, stats
3. **Client receiver** sends: NDX_DONE (4 bytes via write_int!)
4. **Server** reads: First goodbye (needs to handle 4-byte write_int format)
5. **Server** sends: NDX_DONE (1 byte)
6. **Client receiver** reads: NDX_DONE
7. **Client generator** sends: NDX_DONE (1 byte via write_ndx)
8. **Server** reads: Second goodbye (1 byte format)

### The Problem

Our server uses read_ndx which expects:
- 0x00 = NDX_DONE (1 byte)
- 0xFF = negative number prefix (read more bytes)

But the client receiver sends write_int(-1) = [0xFF, 0xFF, 0xFF, 0xFF] (4 bytes).

When we read 0xFF, we should consume 3 more bytes to get the full write_int value.
Our current code does handle this case, but the flow might be off.

### Internal Client Communication (msgdone_cnt)

The generator waits for `msgdone_cnt > 0` in phase 3:
```c
while (1) {
    check_for_finished_files(itemizing, code, 1);
    if (msgdone_cnt)
        break;
    wait_for_receiver();
}
```

`msgdone_cnt` is incremented in `wait_for_receiver()` when reading NDX_DONE from
the internal error_pipe (receiver → generator), NOT from the server connection.

The generator's f_in is connected to error_pipe[0], reading internal messages.

## RESOLVED (2025-12-18)

**Root Cause**: Missing phase transition NDX_DONE echoes in the transfer loop.

**Fix**: Implemented phase transition handling in `crates/core/src/server/generator.rs` to mirror
upstream `sender.c:send_files()` behavior (lines 210, 236-258, 462).

### Root Cause Analysis

Upstream rsync's `send_files()` function implements a phase-based transfer protocol:

```c
// sender.c line 210
int phase = 0, max_phase = protocol_version >= 29 ? 2 : 1;

// sender.c lines 236-258
if (ndx == NDX_DONE) {
    if (++phase > max_phase)
        break;
    write_ndx(f_out, NDX_DONE);  // Echo back for each phase
    continue;
}

// sender.c line 462
write_ndx(f_out, NDX_DONE);  // Final after loop
```

Our original implementation just broke on the first NDX_DONE without echoing phase transitions.

**Expected behavior**:
- Client sends 3 NDX_DONEs for phases 0→1, 1→2, 2→3
- Server echoes 2 NDX_DONEs (phases 1 and 2)
- Server sends final NDX_DONE after loop exit

**Original behavior**:
- Client sends NDX_DONE
- Server breaks immediately, sends only 1 NDX_DONE
- Client times out waiting for phase transition responses

### Fix Applied

Added phase tracking to the generator transfer loop:

```rust
let mut phase: i32 = 0;
let max_phase: i32 = if self.protocol.as_u8() >= 29 { 2 } else { 1 };

loop {
    // ... read NDX ...
    if ndx_byte[0] == 0 {
        phase += 1;
        if phase > max_phase {
            break;
        }
        writer.write_all(&[0x00])?;  // Echo NDX_DONE
        writer.flush()?;
        continue;
    }
    // ... handle file transfer ...
}
writer.write_all(&[0x00])?;  // Final NDX_DONE
```

### Test Results

Successfully tested with upstream rsync 3.4.1 client:

```
$ rsync -vvv rsync://localhost:8873/testmodule/
recv_files phase=1
recv_files phase=2
generate_files phase=3
recv_files finished
generate_files finished
[generator] _exit_cleanup(code=0, ...)
```

Phase checkpoints confirm correct behavior:
- `daemon_PHASE_INIT`: `phase=0 max_phase=2`
- `daemon_ECHO_NDX_DONE`: phase 1, phase 2
- `daemon_PHASE_COMPLETE`: `phase=3`
- `daemon_TRANSFER_COMPLETE`: success
- `daemon_GOODBYE_READ1/2`: 0x00 (both goodbye handshakes complete)

## Code Locations

### Our Implementation
- Generator: `crates/core/src/server/generator.rs` - Phase transition fix at lines 397-429
- Writer: `crates/core/src/server/writer.rs`
- Reader: `crates/core/src/server/reader.rs`
- Setup: `crates/core/src/server/mod.rs`
- Daemon module access: `crates/daemon/src/daemon/sections/module_access.rs`

### Upstream Reference
- sender.c:210 - Phase initialization
- sender.c:236-258 - Phase transition handling
- sender.c:462 - Final NDX_DONE
- main.c:880-905 - read_final_goodbye()

## Related Documentation

- CLAUDE.md Section 4.0-4.7: Daemon mode debugging principles
- Protocol constants: `crates/protocol/src/envelope/message_code.rs`

---

*Last updated: 2025-12-18*
*Status: RESOLVED - Phase transition handling implemented, interop test passing*
