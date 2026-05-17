# IOCP synchronous blocking point audit (#2304)

Tracks every synchronous wait, drain, or fsync in `crates/fast_io/src/iocp/`
that pins the submitter thread before the next overlapped operation can be
queued. The goal is to identify which sync points sit on the per-IO or
per-batch hot path and could be overlapped with the next submission to lift
Windows write throughput.

## Method

1. `grep -rn "WaitForSingleObject\|WaitForMultipleObjects\|Sleep\|GetQueuedCompletionStatus[^E]\|GetQueuedCompletionStatusEx" crates/fast_io/src/iocp/`.
2. Inspected the surrounding code for each match to classify hot vs cold,
   timeout vs INFINITE, and whether the next submission was already prepared
   when the wait fires.
3. Cross-checked the fsync, `await_completion`, and `flush_current`
   sites for the same pattern.

The crate currently contains no `WaitForSingleObject`,
`WaitForMultipleObjects`, or `Sleep` calls; all blocking happens through
the completion port API (`GetQueuedCompletionStatus`,
`GetQueuedCompletionStatusEx`) or auxiliary file-system calls
(`FlushFileBuffers`). The 14 sync points below cover everything visible to
the IO submitter.

## Inventory

| # | File:Line | Kind | Hot/Cold | Timeout | Next SQE prepared? | Suggested mitigation |
|---|-----------|------|----------|---------|--------------------|----------------------|
| 1 | `iocp/file_writer.rs:183` | `GetQueuedCompletionStatus` after every `WriteFile` | Hot (per-IO) | `u32::MAX` (INFINITE) | No - submitter is the only producer | Convert `IocpWriter` to register with `CompletionPump` and pipeline the next `WriteFile` before draining the previous completion |
| 2 | `iocp/file_reader.rs:140` | `GetQueuedCompletionStatus` after every `ReadFile` | Hot (per-IO) | `u32::MAX` | No | Same as #1; use the shared pump and `await_completion` instead of an inline blocking drain |
| 3 | `iocp/file_reader.rs:227` | Per-completion `GetQueuedCompletionStatus` inside `read_all_batched` | Hot (per-IO, inside batch loop) | `u32::MAX` | Yes for the batch, but the loop drains one CQE per syscall | Replace with a single `GetQueuedCompletionStatusEx` drain (already done in `disk_batch.rs::drain_completions`) and order completions by submission index |
| 4 | `iocp/disk_batch.rs:638` | `GetQueuedCompletionStatusEx` inside `submit_write_batch` | Hot (per-batch) | `u32::MAX` (`DRAIN_TIMEOUT_MS`) | Partially - the next chunk is held in the buffer, but cannot be submitted until at least one slot frees | Switch to a credit-based scheme: as soon as one completion arrives, immediately submit the next chunk before processing the rest of the batch in `retain_mut` |
| 5 | `iocp/disk_batch.rs:327` | `FlushFileBuffers` in `commit_file` | Hot (per-file at finalize) | Blocking syscall | No - the next file's `begin_file` is gated on this | Move fsync to a background "finalizer" thread that owns a queue of `(File, completion_key)`; `commit_file` returns immediately and the next `begin_file` proceeds while fsync runs |
| 6 | `iocp/disk_batch.rs:230` (inside `begin_file`) | `flush_current()` before rotating files | Hot (per-file boundary) | Implicit (drains every in-flight write) | No - the new file's overlapped handle is not yet open | Decouple file rotation: open and associate the next file's overlapped handle before the previous file's drain completes |
| 7 | `iocp/pump.rs:360` | `GetQueuedCompletionStatusEx` drain loop | Cold (worker thread, off the submission path) | `DRAIN_TIMEOUT_MS = 100` | N/A (drain thread only) | Already non-blocking from the submitter's perspective; growth heuristic in #1930 is good. See WPG-3 for tuning `batch_size`. |
| 8 | `iocp/socket.rs:198` | `await_completion` after synchronous `WSARecv` success | Hot (per-IO) | Blocking on `mpsc::recv` | No | Skip the wait entirely on synchronous success: register a dummy completion entry rather than blocking on the pump's loopback (current code blocks even when the kernel reported `rc == 0`) |
| 9 | `iocp/socket.rs:209` | `await_completion` for pending `WSARecv` | Hot (per-IO) | Blocking on `mpsc::recv` | Yes if a multi-recv layer batched ahead; today the caller posts one recv at a time | Add a `recv_many_async` that submits N overlapped recvs into a fixed ring before any `await_completion`, exposing the receive credits to multiplex/I/O layers |
| 10 | `iocp/socket.rs:325` | `await_completion` after synchronous `WSASend` success | Hot (per-IO) | Blocking | No | Symmetric to #8: skip the loopback wait on `rc == 0` |
| 11 | `iocp/socket.rs:334` | `await_completion` for pending `WSASend` | Hot (per-IO) | Blocking | No - single send-in-flight | Allow >=2 sends in flight per writer; today every `send_async` waits before returning, which serialises the multiplex egress path |
| 12 | `iocp/disk_batch.rs:309` | `flush_current` at start of `commit_file` | Hot (per-file) | Drains all in-flight writes | No | See #5/#6: pipeline last chunk's completion with FlushFileBuffers and the next file's `begin_file` |
| 13 | `iocp/disk_batch.rs:282` | `flush_current` when caller buffer fills | Hot (per-buffer-fill on large writes) | Drains entire pending batch | Yes - the caller has more data ready to copy in | Use a double-buffer: while batch N drains, fill batch N+1 in a second buffer; submit as soon as a slot frees rather than after the full drain |
| 14 | `iocp/disk_batch.rs:295` (`flush`) and `disk_batch.rs:407` (`Write::flush`) | Explicit caller-driven flush | Cold (user-initiated) | Drains everything | Yes - caller intends a barrier | Leave as-is; this is a semantic barrier, not a spurious sync point |

### Notes on classification

- "Hot (per-IO)" means the sync point fires once per `WriteFile` /
  `ReadFile` / `WSASend` / `WSARecv` and is therefore the per-byte cost
  multiplied by the chunk size.
- "Hot (per-batch)" means it fires once per `submit_write_batch` call -
  every 256 KB by default (`DEFAULT_BUFFER_CAPACITY`).
- "Hot (per-file)" means it fires once per `commit_file` / `begin_file`.
  Significant when transferring many small files.
- The pump drain loop (#7) is the only call that runs off the submitter's
  thread and is therefore already overlapped with submission.

## Top 3 mitigations

### M1 - Replace `IocpWriter` blocking drain with a shared `CompletionPump`

`file_writer.rs:140-200` issues `WriteFile` then immediately blocks on
`GetQueuedCompletionStatus(..., u32::MAX)`. The next chunk cannot be queued
until the previous one is fully reaped, which gives a per-IO syscall pair
(WriteFile + GetQueuedCompletionStatus) plus a context switch.

```rust
// Today (file_writer.rs:140)
let success = unsafe { WriteFile(..., overlapped_ptr) };
if success != TRUE && err.raw_os_error() != Some(997) { return Err(...) }
let wait_ok = unsafe { GetQueuedCompletionStatus(self.port.handle(), ..., u32::MAX) };

// Proposed
let (handler, rx) = oneshot_handler();
self.pump.register(overlapped_ptr, handler);
let success = unsafe { WriteFile(..., overlapped_ptr) };
// Caller can submit the next chunk immediately; only block on `rx`
// when the caller actually needs the completion result.
let pending = WriteHandle { rx, overlapped };
self.in_flight.push_back(pending);
if self.in_flight.len() >= self.config.concurrent_ops as usize {
    self.drain_one()?; // bounded by max-in-flight, not per-chunk
}
```

Expected win: largest of the three because `IocpWriter` is the per-file
hot path used by the local-copy executor on Windows.

### M2 - Pipeline `FlushFileBuffers` and overlapped handle setup across file boundaries

`disk_batch.rs::commit_file` (line 308) and `begin_file` (line 227)
serialise three blocking operations per file boundary:

1. `flush_current()` drains every outstanding write.
2. `FlushFileBuffers` issues a blocking fsync.
3. The next call to `begin_file` opens and associates the new overlapped
   handle.

Move fsync to a dedicated finalizer:

```rust
struct PendingFsync { file: File, completion_key: usize, tx: mpsc::Sender<io::Result<()>> }

// In commit_file:
self.flush_current()?;
let active = self.current_file.take().unwrap();
if do_fsync {
    self.finalizer.queue(PendingFsync { file: active.file, ... });
    // Return immediately; caller can poll `tx` if it needs to confirm.
} else {
    return Ok((active.file, active.bytes_written));
}
```

`begin_file` then runs in parallel with the finalizer thread's fsync of
the previous file. Net win: fsync time (often the dominant per-file cost
on NTFS with `write_through = false`) is hidden behind the next file's
WriteFile submissions.

### M3 - Allow >=2 sends and recvs in flight in `IocpSocketWriter` / `Reader`

`socket.rs::send_async` (line 284) and `recv_async` issue one overlapped
op, then immediately `await_completion`. The pump is already capable of
multiplexing, so the cost is purely architectural.

```rust
// Today
self.pump.register(overlapped_ptr, handler);
let rc = unsafe { WSASend(...) };
await_completion(&rx, &mut overlapped)  // blocks before returning

// Proposed: ring of N in-flight ops with credit accounting.
pub fn send_async_pipelined(&mut self, buf: &[u8]) -> io::Result<SendCredit> {
    if self.in_flight.len() >= self.max_in_flight {
        self.in_flight.pop_front().unwrap().wait()?;
    }
    let pending = self.submit(buf)?;
    self.in_flight.push_back(pending);
    Ok(SendCredit { id: pending.id })
}
```

The protocol multiplex layer (`crates/protocol/src/multiplex.rs`) can
then post several MUX frames before any single one has been
acknowledged. This is the only mitigation that affects the network side
rather than disk.

## Cross-reference to WPG-3 (CQ depth auto-size)

WPG-3 proposes adapting `GetQueuedCompletionStatusEx`'s entry array length
to current load. Two sync points in this audit interact with that work:

- **#4 (`disk_batch.rs::drain_completions`)** uses a fixed
  `COMPLETION_DRAIN_BATCH = 64`. WPG-3's auto-sizing belongs here so that
  bursts of writes (large `concurrent_ops` settings, large files) reap
  more completions per syscall instead of looping.
- **#7 (`pump.rs::drain_loop`)** already implements a one-shot growth
  heuristic via `ERROR_INSUFFICIENT_BUFFER` (issue #1930), capped at
  `MAX_BATCH_SIZE = 8192`. WPG-3 should replace the never-shrinks
  behaviour with a moving-average shrink path so idle ports release the
  256 KiB peak allocation.

Neither cross-reference unlocks throughput on its own, but both compose
with M1 once more completions are in flight per drain.

## Recommendation: which sync points to overlap first

Order by expected throughput win on the Windows write hot path:

1. **M1 (#1, #2, #3)** - the per-IO blocking drain in `IocpWriter` /
   `IocpReader` is on every single overlapped operation issued for local
   file copies. Removing it lifts effective queue depth from 1 to
   `concurrent_ops` (default 4, `for_large_files` 8) with no API change
   visible above the `FileWriter` / `FileReader` traits.
2. **M2 (#5, #6, #12)** - per-file fsync and rotation serialisation
   dominate small-file workloads (the "many small files" preset path).
   Moving fsync off the submission thread closes the gap with io_uring's
   `IORING_OP_FSYNC` async submission.
3. **M3 (#8-#11)** - socket sends are already pipelined upstream of this
   layer by the multiplex protocol's own buffering; the win here matters
   for daemon push throughput but is smaller than the disk wins because
   the TCP stack already coalesces.

Sync points #7 (pump worker) and #14 (caller-driven `flush`) are
intentionally blocking and should remain as-is.

## Out-of-scope follow-ups

- `disk_batch.rs::submit_write_batch` orders completions by lookup against
  `in_flight` rather than by submission index. M1 and M2 do not require
  changing this, but the linear scan in `retain_mut` becomes O(N^2) once
  `concurrent_ops` exceeds ~32. Track separately.
- `await_completion` allocates an `mpsc::channel` per IO (`oneshot_handler`).
  Replace with a slab or thread-local oneshot if M1/M3 reveal the
  allocation as a hotspot under high concurrency.
