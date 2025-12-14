# Debugging Rsync Interoperability Issues

This document provides a systematic, repeatable workflow for debugging protocol-level interoperability issues between `oc-rsync` and upstream `rsync`.

## Table of Contents

1. [Understanding the Problem Space](#understanding-the-problem-space)
2. [Debugging Tools](#debugging-tools)
3. [Protocol Trace Analysis](#protocol-trace-analysis)
4. [Systematic Debug Workflow](#systematic-debug-workflow)
5. [Common Failure Patterns](#common-failure-patterns)
6. [Case Study: Current Connection Reset Issue](#case-study-current-connection-reset-issue)

---

## Understanding the Problem Space

### The Challenge

Rsync has no formal RFC specification. The protocol is defined by the behavior of the C implementation, making interoperability testing critical. Any deviation from the reference implementation can cause:

- **Connection resets** (peer closes connection)
- **Error Code 12** ("error in rsync protocol data stream")
- **Data corruption** (silent failures)
- **Hangs** (deadlocks in bidirectional protocol)

### Key Protocol Phases

1. **Handshake** - Version and capability negotiation
2. **Compat Exchange** - Compatibility flags (protocol >= 30)
3. **Multiplex Activation** - Message framing for stderr/stdout
4. **Filter List** - Client sends include/exclude rules
5. **File List** - Sender transmits metadata of files to sync
6. **Delta Transfer** - Block checksums and delta transmission
7. **Completion** - Final statistics and goodbye

Most interop failures occur in phases 1-5, before any file data is transferred.

---

## Debugging Tools

### 1. Protocol Trace System

The `crates/protocol/src/debug_trace.rs` module provides wrappers that log all I/O:

```rust
use protocol::debug_trace::{TraceConfig, TracingReader, TracingWriter};

// Enable tracing for daemon mode (where stderr is unavailable)
let config = TraceConfig::enabled("daemon");
let reader = TracingReader::new(tcp_stream.try_clone()?, config.clone());
let writer = TracingWriter::new(tcp_stream, config);
```

**Output:** Trace files in `/tmp/rsync-trace/`:
- `daemon_read_0000.log` - All bytes read, with hex and ASCII dumps
- `daemon_write_0000.log` - All bytes written

### 2. Minimal Reproduction Case

Create a simple test case:

```bash
# Create minimal test data
mkdir -p /tmp/rsync-test/source
echo "test" > /tmp/rsync-test/source/file.txt
mkdir -p /tmp/rsync-test/dest

# Test with upstream rsync (baseline)
rsync -av /tmp/rsync-test/source/ /tmp/rsync-test/dest/

# Test with oc-rsync
target/dist/oc-rsync -av /tmp/rsync-test/source/ /tmp/rsync-test/dest/
```

### 3. Upstream Baseline Traces

Run upstream rsync with strace to capture exact wire format:

```bash
# Capture client-side trace
strace -f -e trace=read,write -s 10000 -o /tmp/upstream_client.trace \
  rsync -av source/ rsync://localhost:873/module

# Capture server-side trace
strace -f -e trace=read,write -s 10000 -p $(pgrep -f "rsync --daemon") \
  -o /tmp/upstream_server.trace
```

### 4. tcpdump for Network Analysis

Capture raw TCP packets:

```bash
# Start capture
sudo tcpdump -i lo -w /tmp/rsync.pcap 'port 873'

# Run test
rsync -av source/ rsync://localhost:873/module

# Analyze in Wireshark
wireshark /tmp/rsync.pcap
```

### 5. Hexadecimal Comparison

Compare oc-rsync output vs upstream:

```bash
# Generate hex dumps from trace logs
hexdump -C /tmp/rsync-trace/daemon_write_0000.log > oc_rsync_hex.txt
hexdump -C /tmp/upstream_server.trace > upstream_hex.txt

# Diff to find first divergence
diff -u upstream_hex.txt oc_rsync_hex.txt | head -50
```

---

## Protocol Trace Analysis

### Reading Trace Files

Example trace output:

```
[WRITE] 4 bytes:
  Hex: 1e 00 00 00
  ASCII: ....

[READ] 8 bytes:
  Hex: 07 05 00 00 48 65 6c 6c
  ASCII: ....Hell
```

### Decoding Protocol Messages

#### Multiplex Message Format (Protocol >= 23)

```
Byte 0:     Tag (MSG_DATA=7, MSG_INFO=1, MSG_ERROR=2, etc.)
Bytes 1-3:  Payload length (24-bit little-endian)
Bytes 4+:   Payload
```

**Example:** `07 05 00 00 48 65 6c 6c 6f` decodes as:
- Tag: 7 (MSG_DATA)
- Length: 0x000005 = 5 bytes
- Payload: "Hello"

#### Varint Encoding

Rsync uses variable-length integers to save bandwidth:

- 1 byte: value < 0x80
- 2 bytes: value < 0x8000 (first byte has high bit set)
- 4 bytes: value < 0x80000000
- 8 bytes: larger values (protocol >= 30 for timestamps/sizes)

**Rust implementation:** `crates/protocol/src/varint.rs`

#### Filter List Format

```
[4-byte LE int: rule length]  # 0 = terminator
[rule bytes]
[repeat]
[4-byte zero terminator]
```

#### File List Entry (Protocol 31)

Critical fields:
- `mode` (4 bytes)
- `mtime` (**8 bytes** for protocol >= 30, **4 bytes** for protocol < 30)
- `size` (8 bytes for protocol >= 30)
- `filename` (variable length)

**Common Bug:** Sending 32-bit mtime when 64-bit is expected causes stream desynchronization.

---

## Systematic Debug Workflow

### Phase 1: Isolate the Failure Point

1. **Check exit code:**
   ```bash
   echo $?  # After failed rsync command
   ```
   - **Code 0:** Success
   - **Code 10:** Socket I/O error
   - **Code 12:** Protocol data stream error
   - **Code 104:** Connection reset by peer (ECONNRESET)

2. **Examine logs:**
   ```bash
   # Daemon log
   cat target/interop/run/3-0-9/oc.log

   # Client stderr (if available)
   cat /tmp/client_error.log
   ```

3. **Identify protocol phase:**
   - Handshake failure: "protocol version mismatch"
   - Multiplex failure: "unexpected tag"
   - Filter list failure: "invalid filter rule length"
   - File list failure: "invalid file entry" or premature EOF

### Phase 2: Enable Protocol Tracing

1. **Modify daemon code** to enable tracing:

   ```rust
   // In crates/daemon/src/daemon/sections/module_access.rs
   // After accepting TCP connection:

   use protocol::debug_trace::{TraceConfig, TracingReader, TracingWriter};

   let trace_config = TraceConfig::enabled("daemon");
   let traced_read = TracingReader::new(stream.try_clone()?, trace_config.clone());
   let traced_write = TracingWriter::new(stream, trace_config);
   ```

2. **Rebuild and run test:**
   ```bash
   cargo build --profile dist --bin oc-rsync
   bash scripts/rsync-interop-orchestrator.sh
   ```

3. **Examine trace files:**
   ```bash
   ls -l /tmp/rsync-trace/
   cat /tmp/rsync-trace/daemon_write_0000.log
   cat /tmp/rsync-trace/daemon_read_0000.log
   ```

### Phase 3: Compare with Upstream Baseline

1. **Capture upstream trace** using strace or tcpdump
2. **Align the traces:**
   - Find the first divergence
   - Identify which message differs
3. **Decode the divergence:**
   - Is it a missing byte? (too short)
   - Is it an extra byte? (too long)
   - Is it a wrong value? (wrong encoding)

### Phase 4: Locate the Bug in Code

1. **Search for the relevant serialization code:**
   ```bash
   # For filter list issues:
   grep -r "write_i32_le" crates/protocol/src/filters/

   # For file list issues:
   grep -r "mtime" crates/walk/src/wire/

   # For multiplex issues:
   grep -r "send_msg\|recv_msg" crates/protocol/src/multiplex/
   ```

2. **Check protocol version branching:**
   ```rust
   // Correct:
   if protocol.as_u8() >= 30 {
       write_i64(mtime)?;  // 64-bit
   } else {
       write_i32(mtime as i32)?;  // 32-bit
   }

   // WRONG:
   write_i32(mtime as i32)?;  // Always 32-bit!
   ```

3. **Verify varint usage:**
   - Filter lists use **4-byte LE integers**, not varints
   - File list uses **varints** for most fields
   - Timestamps may use **relative encoding** (difference from base time)

### Phase 5: Fix and Verify

1. **Apply fix**
2. **Add unit test:**
   ```rust
   #[test]
   fn test_mtime_encoding_protocol_31() {
       let mut buf = Vec::new();
       encode_file_entry(&entry, &mut buf, ProtocolVersion::V31).unwrap();

       // Verify mtime is 8 bytes
       assert_eq!(&buf[4..12], &expected_mtime_bytes);
   }
   ```
3. **Run full interop suite:**
   ```bash
   bash scripts/rsync-interop-orchestrator.sh
   ```

---

## Common Failure Patterns

### Pattern 1: Connection Reset by Peer (ECONNRESET)

**Symptom:** Client closes connection immediately

**Cause:** Server sent data that violates protocol expectations

**Debug:**
1. Check if multiplex is activated at correct protocol version
2. Verify compat flags are sent BEFORE multiplex activation
3. Ensure all writes are flushed before stream mode changes

**Example Fix:**
```rust
// WRONG: Activate multiplex before sending compat flags
writer = writer.activate_multiplex()?;
write_varint(&mut writer, compat_flags)?;  // Too late!

// CORRECT: Send compat flags, then activate
write_varint(&mut stdout, compat_flags)?;
stdout.flush()?;  // Critical!
writer = ServerWriter::new_plain(stdout).activate_multiplex()?;
```

### Pattern 2: Error Code 12 (Protocol Data Stream Error)

**Symptom:** "rsync error: error in rsync protocol data stream (code 12)"

**Cause:** Receiver read unexpected data (wrong length, impossible value)

**Debug:**
1. Enable protocol trace
2. Find the read operation that failed
3. Compare expected vs actual byte sequence

**Common Causes:**
- Wrong integer width (4-byte vs 8-byte)
- Missing terminator byte
- Stream desynchronization from earlier error

### Pattern 3: Unexpected Tag

**Symptom:** "unexpected tag 25 [sender]"

**Cause:** Multiplex reader interpreted plain data as a multiplex message

**Debug:**
1. Verify multiplex activation timing
2. Check if BOTH reader and writer are activated
3. Ensure filter list is read AFTER multiplex activation (protocol >= 30)

**Example Fix:**
```rust
// CLIENT sends filter list as MULTIPLEXED data (main.c:1308)
// So SERVER must activate INPUT multiplex BEFORE reading filters

if protocol.as_u8() >= 30 {
    reader = reader.activate_multiplex()?;  // MUST be before read_filter_list
}
let _rules = read_filter_list(reader, protocol)?;
```

### Pattern 4: Invalid Filter Rule Length

**Symptom:** "invalid filter rule length: 771825669" (huge number)

**Cause:** Filter list reader interpreted file list data as filter data

**Debug:**
1. Check if filter list reading is in correct role (Generator reads, Receiver may skip)
2. Verify stream is in correct state (multiplexed or plain)

**Example Fix:**
```rust
// WRONG: Receiver tries to read filter list when it shouldn't
let rules = read_filter_list(reader)?;  // Reads file list by mistake!

// CORRECT: Receiver MUST read filter list to consume wire data
// Even if receiver_wants_list is false, the terminating zero is still sent
let _rules = read_filter_list(reader, protocol)?;  // Consumes data correctly
```

---

## Case Study: Current Connection Reset Issue

### Observed Behavior

```
oc-rsync error: transfer failed to localhost (127.0.0.1):
  module=interop error=Connection reset by peer (os error 104) (code 1)
```

### Investigation Steps Taken

1. **Verified multiplex activation timing** against upstream `main.c`:
   - ✅ OUTPUT multiplex at protocol >= 23
   - ✅ INPUT multiplex at protocol >= 30

2. **Restored filter list reading** in receiver:
   - Client sends filter list as multiplexed data (main.c:1308)
   - Server must read it even if not processing rules

3. **Current status:** Still failing with connection reset

### Next Debug Steps

1. **Enable protocol tracing** in daemon
2. **Capture baseline** from upstream rsync daemon
3. **Compare byte-by-byte** to find first divergence
4. **Potential issues to check:**
   - MultiplexReader/Writer message framing
   - Compat flags encoding (varint format)
   - Filter list encoding (4-byte LE vs varint)
   - Buffer flushing before stream handoff

### Hypothesis

The connection reset happens **before file list transmission**, likely during:
- Compat flags exchange
- Multiplex activation
- Filter list reading

The mtime serialization issue described in the analysis report is **NOT** causing this failure (would occur later during file list phase).

---

## Verification Checklist

Before claiming interoperability is fixed:

- [ ] All three upstream versions pass: 3.0.9, 3.1.3, 3.4.1
- [ ] Both directions work: upstream→oc, oc→upstream
- [ ] Protocol versions 29, 30, 31 all work
- [ ] Large file lists (>1000 files) transfer correctly
- [ ] Files with mtime past 2038 (Year 2038 problem) serialize correctly
- [ ] Delta transfers work (not just full copies)
- [ ] Compression works (`-z` flag)
- [ ] Filter rules work (`--include`, `--exclude`)
- [ ] Bandwidth limiting works (`--bwlimit`)
- [ ] Resume works (`--partial`)

---

## References

- Upstream rsync C code: `target/interop/upstream-src/rsync-3.4.1/`
- Key files: `main.c`, `exclude.c`, `compat.c`, `io.c`
- Protocol docs: `doc/tech_report.tex` in rsync source
- This codebase: `crates/protocol/`, `crates/walk/`, `crates/core/src/server/`

---

## Appendix: Quick Reference

### Enable Tracing in Daemon

```rust
// In module_access.rs, after TCP accept:
let trace_config = protocol::debug_trace::TraceConfig::enabled("daemon");
let stream = protocol::debug_trace::TracingReader::new(
    stream.try_clone()?,
    trace_config.clone()
);
let write_stream = protocol::debug_trace::TracingWriter::new(stream, trace_config);
```

### Decode Multiplex Tag

```
7 + tag = wire_byte
---
7 + 0 (MSG_DATA)  = 7
7 + 1 (MSG_INFO)  = 8
7 + 2 (MSG_ERROR) = 9
7 + 3 (MSG_WARNING) = 10
```

### Run Single Version Test

```bash
# Test just 3.0.9
bash scripts/rsync-interop-client.sh 3.0.9

# Check logs
cat target/interop/run/3-0-9/oc.log
```

### Clean Trace Directory

```bash
rm -rf /tmp/rsync-trace
mkdir -p /tmp/rsync-trace
chmod 777 /tmp/rsync-trace
```

---

**Last Updated:** December 2024
**Status:** Connection reset issue under investigation
