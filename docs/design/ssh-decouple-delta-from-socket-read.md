# SSH Transport: Decouple Delta Apply from the Socket Read

Tracking issue: #1891. Followup to `docs/design/ssh-transport-async-io-eval.md`
(the umbrella eval) and the hybrid recommendation it adopts. See also
`ssh-async-default-linux.md` (#1890) for when the async surface flips on
by default.

## 1. Today: the socket read drives the delta apply

The receive direction inside the SSH transport runs as one logical
pipeline:

```
ChildStdout (sync Read) -> multiplex demux -> token apply -> disk write
                          [single thread; back-pressured by network]
```

The thread that reads frames out of `ChildStdout` is the same thread
that decodes the multiplex header, switches on `MSG_*`, applies the
token to the basis file (the delta-apply step in
`crates/engine/src/`), and either schedules the resulting bytes onto
the SPSC pipeline (`crates/transfer/src/pipeline/spsc.rs`) for the
disk-commit thread or, on the local-write fast path, writes them
itself. The umbrella eval section 6 names this thread the "receiver"
and section 6.4 confirms the disk-commit thread stays sync.

The coupling is hard. When delta apply on frame N blocks (basis-file
read, CPU work on a large literal, brief lock on the buffer pool),
the next `read(2)` on the socket does not issue until apply completes.
The kernel pipe buffer absorbs a bounded amount of upstream data, but
upstream rsync's sender is back-pressured by it within ~64 KiB on
Linux. Both halves stall together.

The umbrella eval section 6.2 identifies this as the dominant per-frame
contribution to the high-RTT bench prediction (8 - 12% wall clock at
100 ms RTT). The umbrella eval's recommendation handles the read /
write overlap *between* network and disk via async at the transport
boundary. It does not break the within-direction coupling between the
socket reader and the delta applier. That is what this document
addresses.

## 2. Proposal: two threads, one bounded queue

Split the receive pipeline at the multiplex-demux output:

```
ChildStdout (AsyncRead) -> demux -> bounded queue -> apply -> disk write
                          [reader thread]   [applier thread]
```

The reader thread does only: read frame header, read payload, decode
`MSG_*` tag, enqueue `(tag, payload_bytes)` onto a bounded
`crossbeam-channel::Sender`. It does no apply work, no disk I/O, no
basis-file lookup. When the queue is full it blocks on `send` and the
socket read naturally back-pressures.

The applier thread does only: pop `(tag, payload_bytes)` off the
queue, dispatch on tag, apply the token to the basis file, either
schedule a write on the SPSC pipeline or write directly. When the
queue is empty it blocks on `recv` and the disk-write half goes idle.

The two threads connect via one bounded queue. Queue depth is the
sole tuning knob and (by section 5 below) the user-visible
back-pressure control.

## 3. Benefits

### 3.1 Socket reads progress while apply is busy

Concrete win: on a 16 MiB literal-copy frame where the receiver is
basis-reading a fragmented destination file, apply takes order of
10 ms but the socket has ~30 ms of upstream data buffered behind it.
Today, the next read(2) waits for apply. With the split, the reader
drains the next several frames into the queue and parks on `send`
only when the queue is genuinely full. The sender side never observes
the disk-apply stall, which means upstream rsync's per-flush
sendto() boundary stays in its predictable cadence rather than
stuttering. The bench (`crates/rsync_io/benches/ssh_sync_vs_async.rs`
when extended with a frame-cadence cell, see section 7) measures
the throughput uplift.

### 3.2 CPU and I/O run in parallel

The reader thread is I/O-bound. The applier thread is mixed: CPU
(memcpy literal bytes, advance the rolling-hash window for COPY
tokens) plus I/O (basis read, write). On a typical core, they
saturate different units (network NIC vs disk + L1/L2) and the wall
clock collapses toward `max(network_time, apply_time)` rather than
the sum.

The umbrella eval section 2 predicts 10 - 20% wall clock on the
"LAN + slow rotational disk, 1 GiB file" row. That row already
benefits from the existing SPSC pipeline between apply and disk; the
new gain here is on the multiplex-demux to apply link, which is not
buffered today.

### 3.3 Composes with the async transport flip

When `ssh-async-default-linux.md` (#1890) ships, the reader thread is
already an async task on the SSH transport's tokio runtime. The split
makes the applier thread the natural `spawn_blocking` boundary the
umbrella eval section 3.3 names. Without the split, the boundary is
inside a single function and harder to enforce.

### 3.4 Recovery from transient apply slowdowns

A long literal frame followed by short COPY frames currently lets the
sender's batching collapse: the receiver does not read the next
header until the literal apply completes. With the split, the queue
absorbs the short COPY frames during the literal apply and dispatches
them back-to-back when the applier returns. The sender's batching is
preserved.

## 4. Costs

### 4.1 One extra thread per active SSH receive

On a CLI invocation (single SSH connection) the cost is one extra
`std::thread::spawn` per transfer. RSS overhead is the thread stack
(~8 KiB resident, ~8 MiB virtual on Linux default) plus the
`crossbeam-channel` queue allocation (bounded; see section 5).
Order of 16 KiB resident; negligible against the multi-MiB working
set the SSH transport already holds.

On the daemon (fan-out N connections), N extra threads. With the
async daemon option (a) from the umbrella eval, the applier threads
can collapse into a rayon pool or a single shared blocking pool: each
applier is a CPU-or-disk-bound task and rayon's work-stealing keeps N
of them busy on `num_cpus` workers. The exact pool choice is
deferred to the implementation; the simplest version (one
`std::thread::spawn` per connection) is the gate criterion in
section 6.

### 4.2 Queue tuning

A bounded queue has two failure modes. Depth too small and the reader
stalls on `send` whenever apply is slow, defeating the point. Depth
too large and the queue absorbs unbounded memory under pathological
sender pressure.

The queue holds `(tag, payload_bytes)` where `payload_bytes` is
either a `Vec<u8>` or (preferably) a `bytes::Bytes` slice from a
shared frame pool. Worst-case per entry is the multiplex max payload
(`MAX_PAYLOAD_LENGTH = 16 MiB` from
`crates/protocol/src/envelope/constants.rs:5`). A queue of depth 4
already permits 64 MiB of in-flight bytes in the worst case; a
depth of 8 permits 128 MiB.

Default depth: 4. Rationale: typical frame size is 8 KiB
(`MplexWriter::DEFAULT_MAX_FRAME_SIZE` in
`crates/protocol/src/multiplex/writer.rs:88`), so depth 4 buffers
~32 KiB of typical traffic, matching the Linux default pipe buffer
size; the rare 16 MiB literal frame consumes one slot. Tunable via
the `--ssh-max-in-flight-bytes` flag in section 5 by deriving
queue depth from `max_bytes / typical_frame_size`.

### 4.3 Ordering risk for multiplex tags

Upstream rsync's multiplex demux is strictly in-order across all
`MSG_*` tags. `MSG_DATA` carries delta tokens that must be applied
sequentially. `MSG_INFO`, `MSG_WARNING`, `MSG_ERROR`,
`MSG_ERROR_XFER` carry log lines that the receiver writes to its own
stderr in protocol-order so the upstream sender's interleaving is
preserved. `MSG_STATS`, `MSG_DONE`, `MSG_FLIST_EOF` are control
boundaries.

The queue must preserve insertion order across tags. A single bounded
FIFO (`crossbeam-channel::bounded`) does this trivially. A
per-tag-class queue (one for `MSG_DATA`, one for everything else)
would not; rejected for that reason. Section 5 codifies the choice.

### 4.4 Error propagation

On the sync path today, an error during apply is returned up the
single thread's call stack and ends the transfer with the appropriate
`ExitCode`. With the split, the applier thread must report errors
back to the reader thread (which is doing the SSH-side cleanup) and
the orchestration layer (which decides the exit code). Section 5
defines the error channel.

## 5. Detailed design

### 5.1 Queue type

`crossbeam-channel::bounded::<DemuxedFrame>(depth)`. Rationale:

- The umbrella eval section 6.4 stays sync downstream of the async
  pump. The applier thread is sync; an async queue (`tokio::sync::mpsc`)
  would force `block_on` calls on the applier side, which the
  migration plan R3 forbids.
- `crossbeam-channel` is already in the workspace, used by
  `crates/transfer/src/pipeline/spsc.rs` and a handful of other
  paths. Approved for measurably lower syscall overhead than
  `std::sync::mpsc`.
- Bounded variants give the back-pressure semantics for free.
  `send` blocks when full; `recv` blocks when empty.

```rust
struct DemuxedFrame {
    tag: MultiplexTag,           // MSG_DATA, MSG_INFO, ...
    payload: bytes::Bytes,       // refcounted slice from frame pool
}
```

`bytes::Bytes` instead of `Vec<u8>` so the reader can hand the
applier a slice of a larger frame-pool buffer without copying. A
follow-up optimisation; v1 ships with `Vec<u8>` and the pool comes
later.

### 5.2 Backpressure semantics

Three back-pressure paths exist; the split preserves all three:

1. **Network -> reader**: TCP receive window plus the SSH pipe buffer.
   Unchanged by this design.
2. **Reader -> applier**: the bounded queue. New. When full, reader
   parks on `send`; the next `read(2)` is delayed; the kernel pipe
   buffer fills; upstream's `write(2)` blocks; upstream sender
   stalls.
3. **Applier -> disk**: the SPSC pipeline
   (`crates/transfer/src/pipeline/spsc.rs`) for non-direct-write
   paths, or the `direct-write` synchronous write for the fast path.
   Unchanged.

The three back-pressure paths daisy-chain. The user-visible knob
(`--ssh-max-in-flight-bytes`, see #1892) controls path 2's capacity;
paths 1 and 3 are fixed by kernel and protocol.

### 5.3 MSG_DATA vs MSG_INFO ordering

The queue is one FIFO. All tags share it. The applier thread
dispatches by tag *after* popping:

```rust
match frame.tag {
    MultiplexTag::Data => apply_delta(&frame.payload, ...)?,
    MultiplexTag::Info => write_info_line(&frame.payload)?,
    MultiplexTag::Warning => write_warning_line(&frame.payload)?,
    MultiplexTag::Error | MultiplexTag::ErrorXfer => {
        report_error(&frame.payload)?;
    }
    MultiplexTag::Stats => record_stats(&frame.payload)?,
    MultiplexTag::Done => break Ok(()),
    // ... other MSG_* tags handled the same way
}
```

Sequential dispatch on the applier thread preserves upstream order
across tags. No reordering, no per-tag prioritisation, no fast path
for control messages. The cost is that a `MSG_INFO` log line behind
a slow `MSG_DATA` apply waits for the apply; the umpstream sender's
ordering is exact, so this matches sender intent.

### 5.4 Error propagation

The applier returns its result on a dedicated `Result<(), Error>`
oneshot channel:

```rust
let (err_tx, err_rx) = oneshot::channel::<Result<(), TransferError>>();
let applier = thread::spawn(move || {
    let result = run_applier(queue_rx, ...);
    let _ = err_tx.send(result);
});
```

The reader thread runs to EOF (or `MSG_DONE`), closes the queue
sender, joins the applier, and propagates whichever error fires
first:

- If apply fails mid-stream, the applier's `Err` is on `err_rx`. The
  reader stops issuing frames (the queue drains), the applier exits,
  the reader returns the applier's error to orchestration.
- If the socket read fails, the reader closes the queue sender. The
  applier sees `Recv(Disconnected)`, finishes any in-queue frames if
  it can, and returns. The reader returns the I/O error.
- If both fail, the reader's I/O error wins (it is the proximate
  cause; the apply error is logged at `debug!` level for diagnosis).

`oneshot` here is `crossbeam-channel::bounded(1)`, not the tokio
oneshot, to keep the applier sync.

The `SshChildHandle::Drop` reaping behaviour from
`crates/rsync_io/src/ssh/connection.rs:492` is unchanged; the
applier's lifetime is strictly nested inside the
`SshConnection`'s, so child process cleanup runs after both threads
have joined.

### 5.5 Cancellation

The reader is cancelled by closing the queue sender. The applier is
cancelled by closing the queue receiver (which the reader can do by
dropping its `Sender`) or by the applier itself checking a
cooperative cancellation token between frames. The umbrella eval
section 3.3 risk 2 (`spawn_blocking` cancellation gap) does not apply
here because both threads are plain `std::thread`; the reader's
ownership of the queue sender is the cancellation token.

When `ssh-async-default-linux.md` flips the reader to an async task,
the applier remains a `spawn_blocking` task that checks a
`CancellationToken` (`tokio_util::sync::CancellationToken`) at each
queue pop. The migration plan R7 cancellation-discipline rule
applies; the applier checks the token between frames.

## 6. Five-step implementation plan

1. **Land the `DemuxedFrame` type and the queue plumbing.** Add the
   bounded queue and the applier thread, but route both ends from
   the existing sync receiver: the reader stays
   `crates/rsync_io/src/ssh/connection.rs` style, the applier is a
   new `std::thread::spawn` in
   `crates/core/src/client/remote/`. Gate: the existing interop
   suite (`tools/ci/run_interop.sh`) passes with the split active
   and the default queue depth.

2. **Wire MSG_DATA dispatch first; control messages stay inline.**
   The applier only handles `MSG_DATA`; other tags are still
   processed on the reader thread immediately on demux. Gate: a
   delta-apply micro-bench cell added to
   `crates/engine/benches/` shows no regression vs the unsplit path
   on the LAN-loopback row, and a bench on a 100 ms RTT shaped link
   shows the predicted 8 - 12% win.

3. **Move control messages onto the queue.** All `MSG_*` tags go
   through the queue; the applier dispatches by tag. Gate: golden
   ordering tests in `crates/protocol/tests/golden/` for an
   interleaved `MSG_INFO` + `MSG_DATA` stream show the applier emits
   them in original order.

4. **Tune the queue depth and add the env-var knob.** Default depth
   = 4 (rationale in section 4.2). Add `OC_RSYNC_SSH_QUEUE_DEPTH`
   env var as an internal override during the rollout. Gate: bench
   sweep on `[1, 2, 4, 8, 16]` queue depths picks 4 as the wall-clock
   knee on the high-RTT row without exceeding the memory budget on
   the worst-case literal-frame row.

5. **Integrate with #1892's `--ssh-max-in-flight-bytes` flag.**
   Replace `OC_RSYNC_SSH_QUEUE_DEPTH` with the user-facing flag
   that derives queue depth from `max_bytes / typical_frame_size`,
   with the floor described in #1892's "failure modes" section
   (`max(N, MAX_FRAME_SIZE)`). Gate: the trigger-condition matrix
   in `ssh-async-default-linux.md` section 3 is updated to include
   this design as a prerequisite for trigger C.

The five steps land in separate PRs. Steps 1 - 3 can land before
the async transport flip; step 4 is the bench-driven tuning; step
5 lands together with #1892.

## 7. Trigger conditions

This design lands when all hold:

- **Trigger 1**: the umbrella eval's recommendation has shipped
  (`AsyncSshConnection` from #1806). The split makes the most
  sense alongside an async reader; landing it on the sync path is
  still net positive but harder to justify against the extra
  thread cost.
- **Trigger 2**: the bench cell in section 6 step 2 shows the
  high-RTT row beats unsplit by `>= 8%` wall clock. If the win is
  smaller, the extra thread is not justified; the design is
  shelved.
- **Trigger 3**: the ordering goldens in section 6 step 3 pass on
  all three CI platforms. Cross-platform thread scheduling
  differences must not perturb tag interleaving.
- **Trigger 4**: the rayon pool or shared blocking pool option for
  daemon fan-out (section 4.1) is sized correctly; the daemon
  bench from `daemon-tokio-async-listener-impl.md` shows no
  thread-pool exhaustion at the target fan-out.

If trigger 2 fails, the design is shelved and `connection.rs` stays
single-threaded on the receive side. If triggers 3 or 4 fail, the
design lands but with `default-features = ["sync-applier"]` until
the failure is resolved.

## 8. Open questions deferred to implementation

- Whether the applier thread should be a `crossbeam_utils::thread::scope`
  scoped thread or a long-lived `std::thread::spawn`. Probably scoped
  for join discipline; verify at step 1.
- Whether the frame-pool slice optimisation (section 5.1 v2) is worth
  the complexity. Defer to bench data from step 4.
- How the design interacts with `--inplace`. The applier writes to
  the basis file directly under `--inplace`; the SPSC pipeline path
  is bypassed. The split is orthogonal but the bench matrix in step
  4 must include an `--inplace` row.
