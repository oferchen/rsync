# MSG_INFO Emission Sites + Flush Discipline Audit (MIF-1, MIF-8)

Tracking: MIF-1 (emission sites), MIF-8 (flush discipline)
Status: Complete
Date: 2026-05-28

## Summary

oc-rsync emits +140% wire segments vs upstream rsync. The root cause is
two-fold: (1) each itemize/warning/deletion line is sent as an individual
MSG_INFO frame with an immediate `inner.flush()`, and (2) the
`send_message()` path in `MultiplexWriter` forces a full flush after every
control message. Upstream rsync buffers INFO messages in `iobuf_out` and
flushes only at phase boundaries or when the buffer is full.

---

## 1. MSG_INFO Constant Definition

| Item | Value |
|------|-------|
| Enum variant | `MessageCode::Info` |
| Wire value | `2` (u8) |
| Alias | `MessageCode::FLUSH` (same value) |
| Upstream name | `MSG_INFO` |
| Definition file | `crates/protocol/src/envelope/message_code.rs:28` |
| Upstream equivalent | `enum msgcode { MSG_INFO = 2 }` in `rsync.h` |

MSG_INFO carries informational log output (`FINFO` log level). In server
mode, upstream rsync's `rwrite()` (`log.c:330-340`) routes `FINFO` payloads
through `send_msg(MSG_INFO, ...)` to the multiplexed output stream. In
client mode, the same payloads go directly to stdout.

---

## 2. MSG_INFO Emission Sites

### 2.1 Generator-side emission (sender role)

| # | File | Line | Function | Trigger | Frequency | Batchable? |
|---|------|------|----------|---------|-----------|------------|
| G1 | `transfer/src/generator/protocol_io.rs` | 307 | `maybe_emit_itemize()` | Per-file itemize (non-transfer items + post-transfer) | 1 per file | Yes |
| G2 | `transfer/src/generator/transfer/transfer_loop.rs` | 269 | (calls `maybe_emit_itemize`) | Non-transfer item (up-to-date, metadata-only) | 1 per skipped file | Yes |
| G3 | `transfer/src/generator/transfer/transfer_loop.rs` | 440 | (calls `maybe_emit_itemize`) | Post-transfer log_item | 1 per transferred file | Yes |

Generator-side emission uses `ServerWriter::send_message(MessageCode::Info, ...)` directly, which calls `MultiplexWriter::send_message()`. This is the send_message path that flushes the buffer + calls `inner.flush()`.

In client mode (`config.connection.client_mode`), the generator routes itemize output to a callback instead, producing zero MSG_INFO frames. The MSG_INFO path is active only in server/daemon mode.

### 2.2 Receiver-side emission (via `MsgInfoSender` trait)

| # | File | Line | Function | Trigger | Frequency | Batchable? |
|---|------|------|----------|---------|-----------|------------|
| R1 | `transfer/src/receiver/mod.rs` | 640 | `emit_itemize()` | Per-file itemize for receiver context | 1 per file | Yes |
| R2 | `transfer/src/receiver/transfer/candidates.rs` | 173 | (calls `emit_itemize`) | Quick-check match (up-to-date file) | 1 per up-to-date file | Yes |
| R3 | `transfer/src/receiver/transfer/pipeline.rs` | 339 | `send_msg_info(warning)` | Pipelined disk-commit warning | Per warning | No (correctness) |
| R4 | `transfer/src/receiver/transfer/pipeline.rs` | 357 | (calls `emit_itemize`) | Post-transfer itemize | 1 per transferred file | Yes |
| R5 | `transfer/src/receiver/transfer/pipeline.rs` | 384 | `send_msg_info(warning)` | Post-loop drain warnings | Per warning | No (correctness) |
| R6 | `transfer/src/receiver/transfer/pipelined_incremental.rs` | 72 | (calls `emit_itemize`) | New directory itemize | 1 per new dir | Yes |
| R7 | `transfer/src/receiver/transfer/pipelined_incremental.rs` | 77 | (calls `emit_itemize`) | Existing directory itemize | 1 per existing dir | Yes |
| R8 | `transfer/src/receiver/directory/links.rs` | 79 | (calls `emit_itemize`) | Symlink up-to-date | 1 per existing symlink | Yes |
| R9 | `transfer/src/receiver/directory/links.rs` | 157 | (calls `emit_itemize`) | Symlink created | 1 per new symlink | Yes |
| R10 | `transfer/src/receiver/directory/links.rs` | 303 | (calls `emit_itemize`) | Hardlink up-to-date | 1 per existing hardlink | Yes |
| R11 | `transfer/src/receiver/directory/links.rs` | 382 | (calls `emit_itemize`) | Hardlink created | 1 per new hardlink | Yes |
| R12 | `transfer/src/receiver/directory/deletion.rs` | 305 | `send_msg_info(line)` | Delete itemize (`*deleting`) | 1 per deleted item | Yes - high value |

### 2.3 Emission path

All MSG_INFO frames flow through the same code path:

```
emit_itemize() / send_msg_info()
  -> ServerWriter::send_message(MessageCode::Info, data)
    -> MultiplexWriter::send_message(code, payload)
      -> flush_buffer()          // flush any buffered MSG_DATA
      -> protocol::send_msg()    // write 4-byte header + payload
      -> self.inner.flush()      // <-- FULL FLUSH after every frame
```

The critical issue is on the last line: `MultiplexWriter::send_message()`
at `transfer/src/writer/multiplex.rs:66-70` flushes the underlying writer
after every single MSG_INFO frame. This means each itemize line - typically
30-80 bytes - triggers its own TCP segment.

### 2.4 Total MSG_INFO frames per transfer

For a transfer of N files where all need updating:

| Role | Call site | Count | Total |
|------|-----------|-------|-------|
| Generator (server mode) | G1 via G3 | 1 per file | N |
| Receiver | R4 (post-transfer) | 1 per file | N |
| **Total per-file** | | | **up to 2N** |

Additional frames for non-transfer items (up-to-date files, symlinks,
hardlinks, deletions) add proportionally. In a typical delta sync where
most files are up-to-date, the frame count can exceed the number of files
transferred.

Upstream rsync batches all `rwrite(FINFO, ...)` calls into `iobuf_out`
(32KB buffer). Multiple itemize lines accumulate and are sent together
when the buffer fills or when a phase boundary forces a flush. For N=1000
files with ~50-byte itemize lines, upstream sends ~2 TCP segments vs
oc-rsync's ~2000 segments.

---

## 3. Flush Discipline Findings (MIF-8)

### 3.1 MultiplexWriter flush paths

| # | File | Line | Method | Trigger | Necessary? |
|---|------|------|--------|---------|------------|
| F1 | `transfer/src/writer/multiplex.rs` | 69 | `send_message()` | Every control message (MSG_INFO, MSG_WARNING, etc.) | **No** - per-message flush is the primary overhead |
| F2 | `transfer/src/writer/multiplex.rs` | 79 | `write_raw()` | Raw protocol writes (handshake, goodbye) | Yes - protocol ordering |
| F3 | `transfer/src/writer/multiplex.rs` | 168-170 | `Write::flush()` | Explicit `writer.flush()` calls from higher layers | Depends on caller |

### 3.2 MplexWriter (protocol crate) flush paths

| # | File | Line | Method | Trigger | Necessary? |
|---|------|------|--------|---------|------------|
| P1 | `protocol/src/multiplex/writer.rs` | 231 | `write_message()` | Every control message | **No** - same issue as F1 |
| P2 | `protocol/src/multiplex/writer.rs` | 300 | `write_raw()` | Raw writes | Yes |
| P3 | `protocol/src/multiplex/writer.rs` | 377-380 | `Write::flush()` | Explicit flush | Depends on caller |

### 3.3 Explicit `writer.flush()` calls in transfer crate

These are the higher-layer flush sites that trigger F3/P3:

| # | File | Line | Context | Necessary? |
|---|------|------|---------|------------|
| E1 | `transfer/src/generator/protocol_io.rs` | 64 | After sending uid/gid lists | Yes - phase boundary |
| E2 | `transfer/src/generator/protocol_io.rs` | 84 | After sending io_error flag | Yes - phase boundary |
| E3 | `transfer/src/generator/protocol_io.rs` | 369 | After probing | Yes - pre-read sync point |
| E4 | `transfer/src/generator/protocol_io.rs` | 570 | After NDX_FLIST_EOF | Yes - phase boundary |
| E5 | `transfer/src/generator/transfer/orchestrator.rs` | 64 | Before receiving filter list | Yes - deadlock prevention |
| E6 | `transfer/src/generator/transfer/goodbye.rs` | 102 | Goodbye handshake | Yes - protocol ordering |
| E7 | `transfer/src/generator/transfer/stats.rs` | 45 | After sending stats | Yes - phase boundary |
| E8 | `transfer/src/generator/diagnostics.rs` | 66 | After diagnostics | Yes - protocol ordering |
| E9 | `transfer/src/receiver/transfer/pipeline.rs` | 63 | Empty file list early return | Yes - flush before NDX_DONE |
| E10 | `transfer/src/receiver/transfer/pipeline.rs` | 290 | When sender has no queued requests | **Questionable** - could be deferred |
| E11 | `transfer/src/receiver/transfer/pipeline.rs` | 424-465 | Dry-run per-file + end-of-loop | **No** - per-file flush in dry run |
| E12 | `transfer/src/receiver/transfer/phases.rs` | 52 | After NDX_DONE per segment/phase | Yes - phase boundary |
| E13 | `transfer/src/receiver/transfer/sync.rs` | 181 | After sending sum_head + signature | Yes - pre-read sync point |
| E14 | `transfer/src/lib.rs` | 511 | After filter list send | Yes - protocol ordering |
| E15 | `transfer/src/lib.rs` | 522 | After files-from data | Yes - protocol ordering |
| E16 | `transfer/src/ack_batcher/batcher.rs` | 208,225 | After ACK batch flush | Yes - batched by design |
| E17 | `transfer/src/setup/compat.rs` | 98 | After version exchange | Yes - handshake |
| E18 | `transfer/src/setup/negotiator.rs` | 201 | After negotiation | Yes - handshake |

### 3.4 Implicit flushes via `send_message()`

Every call to `send_message()` triggers an implicit flush (F1). This
means MSG_INFO emission (section 2) produces an implicit flush per frame
in addition to any explicit flushes. The send_message flush at F1 is the
single highest-impact unnecessary flush in the codebase.

### 3.5 Upstream comparison

Upstream rsync's flush discipline in `io.c`:

- `io_flush()` flushes `iobuf_out` at phase boundaries, before blocking
  reads, and before `select()` in `perform_io()`.
- `rwrite()` writes to `iobuf_out` without flushing. The data is sent
  when the buffer fills (32KB) or at the next `io_flush()`.
- `send_msg()` writes a 4-byte header + payload to `iobuf_out` without
  flushing. The data coalesces with subsequent writes.
- There is no per-message flush for MSG_INFO or any other control message.

The key difference: upstream's `send_msg()` is a buffer append, while
oc-rsync's `send_message()` is a buffer flush + write + flush.

---

## 4. Per-Transfer Overhead Estimate

### 4.1 Extra MSG_INFO frames

For a transfer of N files with itemize enabled (server/daemon mode):

| Scenario | Upstream frames | oc-rsync frames | Overhead |
|----------|----------------|-----------------|----------|
| All files transferred | ~N/100 (batched in 32KB) | N (generator) + N (receiver) = 2N | +200x per batch |
| 90% up-to-date, 10% transferred | ~N/100 | 0.9N (quick-check) + 0.1N (transfer) + 0.1N (gen) = 1.1N | +110x |
| Delete 1000 files | ~10 | 1000 | +100x |

### 4.2 Extra flushes

Each MSG_INFO frame triggers one `inner.flush()` via `send_message()`.
For N=1000 files:

- oc-rsync: ~2000 flushes (one per MSG_INFO frame)
- Upstream: ~10-20 flushes (phase boundaries + buffer-full events)

### 4.3 Wire impact

Each unnecessary flush forces a TCP segment. At a typical itemize line
size of ~50 bytes, each segment carries:

- 4 bytes MSG_DATA header (flush_buffer drains buffered data)
- 4 bytes MSG_INFO header
- ~50 bytes payload
- 40 bytes TCP/IP overhead

Total: ~98 bytes per item. For 1000 files, that is ~98KB of wire traffic
vs upstream's ~58KB (50 bytes * 1000 = 50KB payload in ~2 segments with
~80 bytes TCP overhead).

The overhead is more significant on WAN links where RTT cost dominates:
each flush forces a wait for TCP ACK under Nagle's algorithm (or wastes
a segment under TCP_NODELAY).

---

## 5. Recommendations for MIF-2..7 Implementation

### Priority order

| Task | Description | Impact | Effort |
|------|-------------|--------|--------|
| **MIF-2** | Remove `inner.flush()` from `MultiplexWriter::send_message()` | Eliminates per-message flush; highest single-site impact | Low |
| **MIF-3** | Buffer MSG_INFO in `MultiplexWriter` alongside MSG_DATA | Coalesces INFO frames with DATA frames in the 64KB buffer | Medium |
| **MIF-4** | Batch deletion itemize lines (`*deleting`) | Deletion loop (R12) can format all lines, then send one MSG_INFO frame | Low |
| **MIF-5** | Use `send_msgs_vectored()` for multi-message batching | API already exists in `protocol::send_msgs_vectored()` but is unused outside tests | Medium |
| **MIF-6** | Audit dry-run per-file flush (E11) | Dry-run flush per file in pipeline.rs:453 is unnecessary | Low |
| **MIF-7** | Add wire segment count regression test | Instrument `MultiplexWriter` to count frames and flushes; assert within upstream range | Medium |

### MIF-2 implementation sketch

The highest-impact change is removing the flush from `send_message()` in
`MultiplexWriter`. The method currently does:

```rust
pub(crate) fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    self.flush_buffer()?;
    protocol::send_msg(&mut self.inner, code, payload)?;
    self.inner.flush()  // <-- remove this line
}
```

The `flush_buffer()` call is necessary to maintain message ordering (DATA
before INFO). The `inner.flush()` is not - it forces a TCP segment for
every control message. Removing it allows the next `write()` or explicit
`flush()` to coalesce the INFO frame with subsequent DATA frames.

Risk: some protocol handshake paths depend on `send_message()` flushing.
These are already covered by explicit `writer.flush()` calls (E1-E18).
The MSG_INFO path does not require immediate delivery - the receiver
processes INFO frames opportunistically during its read loop.

### MIF-3 implementation sketch

Instead of calling `protocol::send_msg()` directly (which writes to
`self.inner`), buffer the MSG_INFO header + payload into `self.buffer`
alongside MSG_DATA frames. The buffer already handles mixed content
correctly since each frame is self-describing (4-byte header with code +
length). Flush the buffer when full or at explicit flush points.

This requires changing `send_message()` to append to the buffer rather
than bypassing it, and ensuring `flush_buffer()` sends the buffer contents
without assuming all frames are MSG_DATA (which it currently does not
assume - it calls `send_msg(Data, ...)` to frame the buffer contents).
The buffer framing would need to change from "buffer is raw data, frame
as MSG_DATA on flush" to "buffer is pre-framed messages, write raw on
flush".

### MIF-4 implementation sketch

In `deletion.rs:302-306`, the loop formats and sends one MSG_INFO per
deleted item. Instead, collect all deletion lines into a single `String`
and send one MSG_INFO frame:

```rust
if self.should_emit_itemize() {
    let mut batch = String::new();
    for rel_path in deleted_paths {
        use std::fmt::Write;
        let _ = write!(batch, "*deleting   {}\n", rel_path.display());
    }
    if !batch.is_empty() {
        let _ = writer.send_msg_info(batch.as_bytes());
    }
}
```

This reduces N frames to 1 per directory's deletion batch.
