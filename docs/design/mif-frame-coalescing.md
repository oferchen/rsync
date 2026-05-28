# MSG_INFO Frame Coalescing Design (MIF-3/4)

## Problem Statement

The oc-rsync multiplex layer emits +140% more wire segments than upstream rsync
when itemize output (`-i` / `--itemize-changes`) is enabled. Each per-file
`MSG_INFO` frame - typically 30-80 bytes of itemize text - triggers an
immediate `flush_buffer()` of any pending MSG_DATA, followed by a `send_msg()`
plus `inner.flush()`. This produces one TCP segment per file, whereas upstream
rsync batches many info messages into a single kernel write.

### Measured Impact

For a 10,000-file transfer with itemize enabled, oc-rsync produces roughly
10,000 additional TCP segments that upstream would batch into approximately
60-120 writes (one per `iobuf.msg` buffer drain cycle). The extra segments
increase syscall count, TCP small-packet overhead, and Nagle/delayed-ACK
interactions. On high-latency links or daemon transfers, this measurably
increases wall-clock time.

## Upstream Architecture (Source of Truth)

Upstream rsync uses a dual-buffer architecture in `io.c`:

```
iobuf.out  - circular buffer for MSG_DATA (file data)
iobuf.msg  - circular buffer for MSG_INFO/MSG_ERROR/MSG_WARNING (control)
```

### Key Design: Control Messages Are Buffered, Not Flushed Immediately

1. **`send_msg()` (io.c:965)** appends the 4-byte header + payload into
   `iobuf.msg` without any flush. It only calls `perform_io(PIO_NEED_MSGROOM)`
   if the msg buffer is full - to drain enough room for the new message.

2. **`perform_io()` select loop (io.c:680-716)** decides which buffer to
   flush based on priority:
   - If `iobuf.out` has data beyond the empty header reserve AND `iobuf.msg`
     is empty, flush `iobuf.out` (raw data).
   - If `iobuf.msg` has data, flush `iobuf.msg` (control messages) with
     priority over `iobuf.out` data.
   - Both share the same `iobuf.out_fd` socket.

3. **`io_flush()` (io.c:2061)** flushes BOTH buffers - `iobuf.out` first
   (via `perform_io(PIO_NEED_OUTROOM)`), then `iobuf.msg` (via
   `perform_io(PIO_NEED_MSGROOM)`).

4. **`rwrite()` (log.c:330-340)** for server-side info messages calls
   `send_msg(MSG_INFO, buf, len, ...)` which merely appends to `iobuf.msg`.
   No flush. The message drains when `perform_io()` runs for any reason -
   waiting for input, needing output room, or an explicit `io_flush()`.

### Result: Natural Batching

Multiple `log_item()` calls during a single generator pass accumulate in
`iobuf.msg`. They drain together in the next `perform_io()` cycle, producing
one or a few large writes containing multiple MSG_INFO frames back-to-back.
The kernel then coalesces these into fewer TCP segments via Nagle or corking.

## oc-rsync Current Architecture

### The Flush-Per-Message Problem

`MultiplexWriter::send_message()` (transfer/src/writer/multiplex.rs:66):

```rust
pub(crate) fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    self.flush_buffer()?;          // 1. flush pending MSG_DATA
    protocol::send_msg(&mut self.inner, code, payload)?;  // 2. write the control frame
    self.inner.flush()             // 3. flush the underlying writer
}
```

Step 3 (`self.inner.flush()`) forces a kernel write after every single
MSG_INFO frame. This is the root cause of the +140% segment count.

### Emission Sites (MIF-1 Audit)

| Site | File | Frequency | Batchable? |
|------|------|-----------|------------|
| Generator itemize | `generator/protocol_io.rs:307` | Per-file (skipped, up-to-date, transferred) | Yes |
| Receiver itemize | `receiver/mod.rs:640` | Per-file (transferred, symlinks, dirs) | Yes |
| Receiver warnings | `receiver/transfer/pipeline.rs:339,384` | Sporadic (permission errors, etc.) | Yes |
| Deletion itemize | `receiver/directory/deletion.rs:305` | Per-deleted-file (burst after parallel scan) | Yes |
| Receiver symlinks | `receiver/directory/links.rs:79,157,303,382` | Per-symlink/hardlink | Yes |
| Receiver incremental | `receiver/transfer/pipelined_incremental.rs:72,77` | Per-file (INC_RECURSE) | Yes |

All MSG_INFO emissions follow the same pattern: format a short text line and
call `send_msg_info()` / `send_message(MessageCode::Info, ...)`. None require
immediate delivery - the client reads them only when the multiplexed input
loop processes control frames between data chunks.

### Messages That Must NOT Be Delayed

| Code | Reason |
|------|--------|
| `MSG_ERROR` / `MSG_ERROR_XFER` | Fatal errors must be visible immediately for diagnosis |
| `MSG_IO_ERROR` | io_error accumulator must be visible to the receiver promptly |
| `MSG_REDO` | Triggers immediate re-transfer; latency-sensitive |
| `MSG_NO_SEND` | Receiver is blocked waiting for file data or skip signal |
| `MSG_SUCCESS` | Remove-source-files depends on prompt confirmation |
| `MSG_DELETED` | File-list tracking depends on prompt delivery |
| `MSG_NO_OP` | Keepalive - must reach the peer within the timeout window |

These message types already go through `send_message()` but should retain the
immediate flush. Only `MSG_INFO` (and `MSG_WARNING` for non-fatal warnings)
are safe to defer.

## Strategy Evaluation

### Option A: Buffer MSG_INFO and Flush at File Boundaries

Accumulate MSG_INFO payloads in a secondary buffer. Flush when:
- A non-INFO control message is sent (ERROR, REDO, etc.)
- A DATA flush occurs (file boundary)
- The buffer reaches a size threshold

**Pros:** Simple, matches upstream's natural batching rhythm.
**Cons:** Requires a second buffer alongside the existing DATA buffer. The
flush trigger must be wired into every code path that currently calls
`flush_buffer()`.

### Option B: Vectored Writes (writev) for Multiple Small Frames

Use `send_msgs_vectored()` (already exists in protocol crate) to combine
multiple MSG_INFO frames into a single syscall.

**Pros:** Uses existing infrastructure. Reduces syscalls from N to 1.
**Cons:** Requires a collection point to gather the frames before writing.
Doesn't help unless there's a buffering layer above it - vectored writes
only help if you have multiple frames ready at the same call site.

### Option C: Nagle-Style Timer-Based Coalescing

Buffer MSG_INFO frames and flush on a timer (e.g., 50ms) or when a threshold
is reached.

**Pros:** Simple to implement; guarantees bounded latency.
**Cons:** Adds artificial latency to interactive use (`-v` output appears
in bursts). Timer threads add complexity. Upstream does not use timers for
this - its batching is purely buffer-pressure-driven.

### Option D: Match Upstream's Exact Flush Discipline

Remove `self.inner.flush()` from `send_message()` for batchable message codes
(MSG_INFO, MSG_WARNING). Let the existing buffer pressure drive flushing -
MSG_INFO frames accumulate in the underlying `BufWriter` or TCP write buffer
and drain naturally when DATA writes push the buffer to capacity or when an
explicit `io_flush()` occurs at phase boundaries.

**Pros:** Minimal code change. Matches upstream semantics exactly. No new
buffers, no timers, no vectored-write coordination. Wire bytes are identical -
only TCP segmentation changes.
**Cons:** Requires careful audit that no caller depends on MSG_INFO being
visible immediately after `send_message()` returns.

## Recommended Strategy: Option D (Deferred Flush)

Option D is the correct approach because it matches upstream's architecture
with minimal code change and zero risk to wire-byte parity.

### Rationale

1. **Upstream does not flush after control messages.** `send_msg()` in io.c
   merely appends to `iobuf.msg`. The bytes reach the socket only when
   `perform_io()` runs - typically when the next DATA write needs room or
   when `io_flush()` is called at phase boundaries.

2. **Wire bytes are unchanged.** Coalescing only affects TCP segmentation,
   not the byte stream. The receiver sees identical multiplexed frames
   regardless of how many frames were packed into each `write()` syscall.

3. **No new abstractions needed.** The existing `MultiplexWriter` buffer
   already provides the accumulation mechanism. We just stop forcing a
   flush after every MSG_INFO.

4. **Latency is bounded by DATA writes.** In practice, MSG_INFO frames are
   interleaved with MSG_DATA frames from file transfers. The DATA writes
   provide natural flush points. For `--dry-run` (no DATA), explicit flushes
   at phase boundaries and keepalive intervals bound the latency.

### Implementation Sketch

#### Change 1: Selective Flush in MultiplexWriter (transfer crate)

File: `crates/transfer/src/writer/multiplex.rs`

```rust
// Before (current):
pub(crate) fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    self.flush_buffer()?;
    protocol::send_msg(&mut self.inner, code, payload)?;
    self.inner.flush()
}

// After (proposed):
pub(crate) fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    self.flush_buffer()?;
    protocol::send_msg(&mut self.inner, code, payload)?;
    if code.requires_immediate_flush() {
        self.inner.flush()?;
    }
    Ok(())
}
```

#### Change 2: Flush Classification on MessageCode (protocol crate)

File: `crates/protocol/src/envelope/message_code.rs`

```rust
impl MessageCode {
    /// Returns true if this message code requires an immediate flush after
    /// being written to the multiplexed stream.
    ///
    /// MSG_INFO and MSG_WARNING are batchable - they accumulate in the write
    /// buffer and drain with the next DATA flush or explicit io_flush().
    /// All other control messages (ERROR, REDO, NO_SEND, SUCCESS, etc.) are
    /// latency-sensitive and must be flushed immediately.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:965 send_msg()` - appends to iobuf.msg without flushing
    /// - `io.c:680-716 perform_io()` - drains iobuf.msg opportunistically
    pub const fn requires_immediate_flush(&self) -> bool {
        !matches!(self, Self::Info | Self::Warning)
    }
}
```

#### Change 3: Explicit Flush at Phase Boundaries

Verify that existing `flush()` calls at phase boundaries are sufficient:

- End of file-list sending (already flushes)
- End of delta transfer phase (already flushes)
- Goodbye handshake (already flushes via `write_raw`)
- Dry-run completion (verify flush exists)

No new flush points should be needed - the existing `ServerWriter::flush()`
calls at phase transitions and `write_raw()` for goodbye messages already
drain all buffered data.

#### Change 4: Dry-Run / List-Only Safety

In `--dry-run` and `--list-only` modes, no MSG_DATA is written, so there is
no natural buffer-pressure flush. Verify that:
- Phase-transition flushes drain accumulated MSG_INFO
- The keepalive timer (`maybe_send_keepalive`) triggers flushes
- The final `io_end_buffering_out` equivalent flushes everything

If any path is missing, add an explicit `flush()` at the end of the
dry-run transfer loop.

### Files to Modify

| File | Change |
|------|--------|
| `crates/protocol/src/envelope/message_code.rs` | Add `requires_immediate_flush()` method |
| `crates/transfer/src/writer/multiplex.rs` | Conditional flush in `send_message()` |
| `crates/protocol/src/multiplex/writer.rs` | Same conditional flush in `MplexWriter::write_message()` |

### Wire-Byte Parity Constraints

- The multiplexed byte stream MUST be identical regardless of coalescing.
  Each MSG_INFO frame still has its 4-byte header + payload. Only the
  TCP segmentation changes.
- The frame ordering MUST be preserved: if MSG_DATA was buffered before
  MSG_INFO, the DATA frame must appear first on the wire (the existing
  `flush_buffer()` before `send_msg()` already ensures this).
- Golden byte tests in `crates/protocol/tests/golden/` are unaffected
  because they test frame encoding, not TCP segmentation.
- Interop tests against upstream rsync 3.0.9/3.1.3/3.4.1/3.4.2 are the
  primary validation - upstream does not care how many TCP segments carry
  the frames.

### Verification Plan

1. **Unit test:** Write a test that sends multiple MSG_INFO frames without
   intervening DATA, then verifies all frames are present in the output
   after a single explicit flush (no intermediate flushes).

2. **Wire capture:** tcpdump a daemon transfer with `-i` before and after.
   Count TCP segments. Target: segment count within 20% of upstream.

3. **Interop:** Full interop suite (`tools/ci/run_interop.sh`) with
   `--itemize-changes` enabled. All versions must pass.

4. **Dry-run test:** `--dry-run -i` must produce output (not buffer
   indefinitely). Verify MSG_INFO frames drain at phase boundaries.

5. **Latency test:** Interactive `rsync -avv` must not introduce visible
   delays in output compared to current behavior. Acceptable latency
   bound: output appears within 1 DATA flush cycle or 100ms, whichever
   is shorter.

### Rollback Criteria

- Any interop test failure (upstream rsync rejects the stream or produces
  different results)
- Any golden byte test failure (frame encoding changed)
- Interactive output latency exceeds 500ms (user-visible delay)
- Dry-run / list-only modes produce no output (buffered indefinitely)
- Any deadlock (MSG_INFO flush depends on something that depends on
  MSG_INFO delivery)

### Expected Impact

| Metric | Before | After (Projected) |
|--------|--------|--------------------|
| TCP segments per 10K files (-i) | ~20,000 | ~10,000-12,000 |
| `write()` syscalls per 10K files | ~20,000 | ~2,000-4,000 |
| Daemon transfer wall-clock (10K files) | baseline | -5% to -15% |
| SSH transfer wall-clock (10K files) | baseline | -2% to -8% |
| Wire bytes | identical | identical |

## Appendix: Why Not Option A (Separate MSG Buffer)

Option A (a dedicated `msg_buffer: Vec<u8>` in `MultiplexWriter`) would
more closely mirror upstream's dual-buffer architecture. However:

1. The existing write buffer already provides batching via `BufWriter` or
   OS socket buffers. Adding a second buffer duplicates functionality.

2. Upstream's dual-buffer exists because `iobuf.msg` is written by a
   different process (the generator) than `iobuf.out` (the sender). In
   oc-rsync, both run in the same thread with the same writer, so
   separation provides no structural benefit.

3. A separate buffer requires coordinating flush order between two buffers,
   adding complexity for no correctness gain.

Option D achieves the same batching effect by simply not flushing after
MSG_INFO writes, letting the existing buffer absorb the coalescing.

## Appendix: Why Not Option B (Vectored Writes Alone)

`send_msgs_vectored()` exists in the protocol crate and works well for
the case where multiple frames are ready simultaneously. However:

1. MSG_INFO emissions happen one at a time in the transfer loop - they
   are not naturally batched at a single call site.

2. The deletion path does emit multiple MSG_INFO frames in a burst (the
   `for rel_path in deleted_paths` loop), but even there, each call goes
   through `send_msg_info()` individually.

3. Vectored writes would require a collection layer above the current
   emission sites - essentially reimplementing Option A's buffer.

Option D makes vectored writes unnecessary because the existing write
buffer already coalesces the individual `send_msg()` calls.
