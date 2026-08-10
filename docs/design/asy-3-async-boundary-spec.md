# ASY-3: Per-boundary async disposition contract

Status: Design spec. Refines ASY-2
(`docs/design/asy-2-tokio-runtime-feature.md`) by binding each of the
12 boundaries enumerated in ASY-1
(`docs/audits/asy-1-threading-model.md`) to a contract that ASY-7..10
implementations link against. Scope: per-boundary disposition,
signatures, channel-type swaps, cancellation semantics, and
`spawn_blocking` pool sizing. Out of scope: code changes, ASY-9's
native `tokio-uring` decision (still punted), ASY-12's flip-to-on
gate.

ASY-2 left the disposition at table-cell granularity. This spec turns
each cell into a contract.

## 1. Disposition summary

| ASY-1 # | Boundary | ASY-2 cut | ASY-3 disposition | Changed? |
|---------|----------|-----------|-------------------|----------|
| 1 | Generator wire `reader.read` | `.await` | `.await` | no |
| 2 | Generator wire `writer.flush` / `write_all` | `.await` | `.await` | no |
| 3 | Generator basis-file `read_to_end` / `MapFile` | `spawn_blocking` | `spawn_blocking` | no |
| 4 | Receiver wire `reader.read` for delta tokens | `.await` | `.await` | no |
| 5 | Receiver `writer.flush` gated on `flushed_pending == 0` | `.await` | `.await` | no |
| 6 | `spsc::Sender::send` spin-wait | `.await` (mpsc swap) | `.await` (mpsc swap) | no |
| 7 | `spsc::Receiver::recv` spin-wait | `.await` (mpsc swap) | `.await` (mpsc swap) | no |
| 8 | `find_basis_file_with_config` rayon worker | `spawn_blocking` per batch | `spawn_blocking` per batch | no |
| 9 | `disk_commit::process_file` write + fsync | `spawn_blocking` island | long-lived `spawn_blocking` task | refined |
| 10 | `fast_io::IoUringDiskBatch::submit_and_wait` | `spawn_blocking` (until ASY-9) | co-located inside #9 | refined |
| 11 | Daemon `TcpListener::accept` | already `.await` | already `.await` (`async-daemon`) | no |
| 12 | SSH `spawn_blocking(run_blocking_server)` | dissolves | dissolves | no |

Tally: **6 `.await`** (1, 2, 4, 5, 6, 7), **4 `spawn_blocking`** (3,
8, 9, 10), **1 dissolves** (12), **1 unchanged** (11; already async).
No disposition flipped from ASY-2. Boundaries 9 and 10 were refined
(not flipped) to bind to a single long-lived blocking task rather
than per-call islands; see 2.9, 2.10, and section 4.

## 2. Per-boundary contracts

### Boundaries 1, 2, 4, 5: wire `.await` on transport halves

Locations: #1 generator read and #2 generator write in
`crates/transfer/src/generator/transfer/transfer_loop.rs`; #4
receiver delta-token read in
`crates/transfer/src/receiver/transfer/transfer_ops/response.rs`
(`process_file_response_streaming`); #5 receiver flush in
`crates/transfer/src/receiver/transfer/pipeline.rs` (gated on
`flushed_pending == 0`).

Disposition: `.await`. Rationale: socket / stdio reads and writes,
trivially pollable. No fsync, mmap, or ioctl. ASY-4's
`tokio::io::AsyncRead` / `AsyncWrite` shims preserve buffering and
let tasks yield instead of spin.

```rust
// before
fn read_ndx_request(reader: &mut Reader) -> io::Result<NdxRequest>;
fn write_frame(writer: &mut Writer, frame: &[u8]) -> io::Result<()>;
// after (cfg(feature = "tokio-transfer"))
async fn read_ndx_request(reader: &mut AsyncReader) -> io::Result<NdxRequest>;
async fn write_frame(writer: &mut AsyncWriter, frame: &[u8]) -> io::Result<()>;
```

Channel swap: none. Cancellation: a dropped future cancels at the
next `.await`. Reads emit no bytes when cancelled, so wire-byte
parity holds. A cancelled `write_all` may leave a partial frame on
the wire; mitigation is a `WriterGuard` that, on drop after a partial
write, marks the connection dead and refuses further writes - the
peer sees a truncated multiplex frame and exits with code 12,
matching today's mid-frame panic behaviour. Mid-token receiver
cancel leaves the parser inconsistent; the receiver task drops to
`Closing` and tears down. No partial commit because the disk task
(#9) only emits `CommitResult` at token-closure boundaries.

Critical invariant for #2 and #5: ASY-1 "Preserved"
flush-before-block (section 3, row 7) - defended in the
`WriterGuard` and asserted by ASY-5's capture-replay harness.
`spawn_blocking` pool: n/a.

### Boundary 3: Generator basis-file `read_to_end` / `MapFile::map_window`

Location: `crates/transfer/src/generator/transfer/transfer_loop.rs`
plus `crates/engine/src/delta/`. Disposition: `spawn_blocking`.

Rationale: `MapFile` is mmap-backed; reads are page-fault driven and
not pollable. Non-mmap basis reads use blocking POSIX `read()` with
fadvise / `O_NOATIME` flags that `tokio::fs::File` cannot honour
portably.

```rust
async fn load_basis_window(path: &Path, range: Range<u64>) -> io::Result<Bytes> {
    let path = path.to_path_buf();
    blocking_io(move || sync_load_basis_window(&path, range)).await
}
```

Channel swap: none. Cancellation: tokio's `spawn_blocking` join
handle is detach-on-drop. A cancelled basis load runs to completion
and the result is discarded; the only side effect is fadvise hints
and a `Bytes` allocation. Acceptable. `spawn_blocking` pool: shared
tokio default blocking pool, sized via `TOKIO_TRANSFER_BLOCKING_THREADS`
env var (default 512, the tokio default), shared with #8, #9, #10.

### Boundaries 6, 7: SPSC -> mpsc channel swap

Locations: `crates/transfer/src/pipeline/spsc.rs:74` (send) and
`:110` (recv), used by `receiver/transfer/pipeline.rs` and the
disk-commit thread. Disposition: `.await` via channel swap.

Rationale: the SPSC ring is a userspace spin. Under async it becomes
`tokio::sync::mpsc` whose `send().await` parks on full and
`recv().await` parks on empty.

```rust
// before
fn send(&self, msg: FileMessage) -> Result<(), SendError<FileMessage>>;
fn recv(&self) -> Result<T, RecvError>;
// after
async fn send(&self, msg: FileMessage) -> Result<(), SendError<FileMessage>>;
async fn recv(&mut self) -> Option<T>;
```

Channel swap: `pipeline::spsc::{Sender,Receiver}<T>` ->
`tokio::sync::mpsc::{Sender,Receiver}<T>`. Three mpsc pairs replace
three SPSC pairs (FileMessage, CommitResult, buf-recycle); capacity
preserved (default 128, clamped 8..=4096). The SPSC implementation
stays for the threaded path; the swap is `#[cfg]` gated.

Cancellation: a cancelled `send().await` drops the message. Receiver
and disk tasks share a `CancellationToken` so any dropped message
implies connection teardown (section 5.1). A cancelled `recv().await`
leaves the channel intact for the next consumer; since each channel
is single-consumer, cancellation of that consumer implies teardown
via `mpsc::Receiver::close()` to unblock senders. Channel disconnect
surfaces as `recv() -> None`, which exits cleanly. `spawn_blocking`
pool: n/a.

### Boundary 8: `find_basis_file_with_config` rayon worker

Location: `crates/transfer/src/receiver/basis.rs`, called from
`receiver/transfer/pipeline.rs` (the `par_iter` row of ASY-1's
per-stage table). Disposition: `spawn_blocking`, **one island per
batch**, not per file.

Rationale: rayon is sync-only. ASY-2 fixed granularity at one
`spawn_blocking` per signature batch; ASY-3 binds the contract: the
batch closure runs on rayon under a single `spawn_blocking`, returns
`Vec<BasisResult>`, and the calling task issues the sequential `zip`
send loop on the tokio side.

```rust
let results: Vec<BasisResult> = blocking_io(move || Ok(
    entries.par_iter()
        .map(|(idx, entry, path)| find_basis_file_with_config(...))
        .collect()
)).await?;
```

Channel swap: none. Results return through the join handle and feed
into the #6 mpsc. Cancellation: detach-on-drop like #3; the
`par_iter().collect()` barrier (ASY-1 "Preserved", section 3 row 2)
is inside the blocking closure and preserved verbatim.
`spawn_blocking` pool: shared with #3, #9, #10. Each connection
holds at most one batch-sized blocking slot at a time, bounded by
`connection_counter`. Operator guidance:
`TOKIO_TRANSFER_BLOCKING_THREADS >= max_connections * 4` (covers #3
and #8 transient + #9 long-lived); pushed to ASY-6 for sign-off.

### Boundary 9: `disk_commit::process_file` write + fsync

Location: `crates/transfer/src/pipeline/disk_commit/process.rs`,
spawned by `disk_commit::thread::disk_thread_main`. Disposition:
`spawn_blocking` island, **long-lived task per connection**, not per
file. Refined from ASY-2.

Rationale: per-file `spawn_blocking` would schedule N pool tasks per
file and shred the io_uring ring's single-owner discipline (ASY-1
"Preserved", section 3 row 8). The disk-commit body becomes a single
`spawn_blocking` task that drives an async loop via
`Handle::block_on(recv())`. The ring is owned by one OS thread for
the connection's lifetime, identical to today's `std::thread`.

```rust
fn spawn_disk_task(
    file_rx: mpsc::Receiver<FileMessage>,
    result_tx: mpsc::Sender<io::Result<CommitResult>>,
    buf_tx: mpsc::Sender<Vec<u8>>,
    handle: tokio::runtime::Handle,
    ...
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        handle.block_on(disk_task_main(file_rx, result_tx, buf_tx, ...))
    })
}
```

`disk_task_main` is async (uses `.await` on mpsc). `block_on` from
inside `spawn_blocking` is the standard idiom for a pool task driving
an async loop, and is the only crate-internal use of `block_on`
(ASY-2 confines it to `core`; this is the documented exception in
`transfer`).

Channel swap: SPSC -> mpsc, three pairs. Cancellation: the disk task
watches a `CancellationToken`. On trigger it drops the
`mpsc::Receiver<FileMessage>` to signal senders, flushes pending SQEs
via `submit_and_wait(0)` so the kernel releases our fds, closes
result/buf channels, and returns. In-flight `process_file` work
completes to its temp-file or atomic-rename boundary; partial commits
never reach the destination because rename is the last step. ASY-1
"in-order disk commit per file" (section 3, row 3) holds by
construction: observable states are "committed" or "missing", never
"partially committed". `spawn_blocking` pool: one slot per active
connection for the connection's lifetime; see #8 for sizing.

### Boundary 10: `fast_io::IoUringDiskBatch::submit_and_wait(1)`

Location: `crates/fast_io/src/io_uring/file_writer.rs:149,301` and
`file_reader.rs:136,234`. Disposition: `spawn_blocking`,
**co-located inside #9, no separate task**. Refined from ASY-2.

Rationale: the ring must stay owned by one OS thread
(`project_io_uring_shared_ring_bottleneck.md`). Keep
`submit_and_wait` sync and call it from inside the disk task's
`spawn_blocking`. The ring is created and dropped inside the disk
task; ownership never crosses an `.await`. Native `tokio-uring`
`.await` on completion stays the long-term shape (ASY-9), gated on
IUR-3 per-thread rings landing.

Signature: unchanged. `submit_and_wait` stays sync on
`IoUringDiskBatch`; only the surrounding context changes (async
runtime owns the OS thread instead of `std::thread`). Channel swap:
none. Cancellation: handled by #9's `CancellationToken`; the final
`submit_and_wait(0)` harvests completions for in-flight SQEs before
drop. `spawn_blocking` pool: see #9. The Windows IOCP analogue
(`fast_io::IocpDiskBatch`) inherits the same contract via the shared
single-owner-ring invariant.

### Boundary 11: Daemon `TcpListener::accept`

Location:
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:178`,
`crates/daemon/src/async_listener.rs:73`. Disposition: already
`.await` under `async-daemon`. `tokio-transfer` does not change
accept; it only changes the per-connection dispatch site
(`spawn_blocking(worker)` -> `tokio::spawn(async_worker)`). Channel
swap: none. Cancellation: a cancelled spawned worker aborts the
receiver task; #9 observes mpsc closure and drains cooperatively.
`spawn_blocking` pool: no longer used for the per-connection driver.

### Boundary 12: SSH `spawn_blocking(run_blocking_server)`

Location:
`crates/core/src/client/remote/async_ssh_transport.rs:337-369`.
Disposition: **dissolves**.

Rationale: `run_server_with_handshake` becomes async under
`tokio-transfer`. The paired `std_mpsc::sync_channel` bridge between
sync server and async ssh pumps disappears; the server `.await`s on
`AsyncRead`/`AsyncWrite` halves of the ssh stdio pipes directly (#1,
#2, #4, #5). The `spawn_blocking(run_blocking_server)` call site is
removed.

```rust
let (async_reader, async_writer) = ssh_stdio_into_async(child)?;
let server = tokio::spawn(run_server_with_handshake(
    async_reader, async_writer, role, cfg,
));
```

Channel swap: `std_mpsc::sync_channel` (`CHANNEL_CAPACITY`) and the
outbound/inbound `tokio_mpsc` pumps all dissolve. Cancellation:
dropping the server `JoinHandle` aborts the server task; the ssh
child is held by `SshChildHandle` whose `Drop` reaps it
(per `project_ssh_stderr_socketpair_silent_fallback.md`); reaper
runs after server-task abort, no race. `spawn_blocking` pool: not
used by SSH transport anymore.

## 3. Preserved-invariant survival audit

ASY-1 catalogued 8 sync semantics. ASY-3 keeps all 8; two require
explicit defence (rows 5 and 7 below).

| # | ASY-1 invariant | Survives? | Defence |
|---|-----------------|-----------|---------|
| 1 | Wire-byte parity with upstream 3.4.1 / 3.4.2 | yes | #1, #2, #4, #5 only change scheduling, not bytes; golden + interop tests gate per ASY-2 section 7. |
| 2 | In-order NDX request dispatch in the receiver | yes | The sequential `zip` send loop after `par_iter().collect()` is preserved by #8's one-batch-per-`spawn_blocking` rule; zip runs on the tokio task after join. |
| 3 | In-order disk commit per file | yes | #9's `mpsc::Receiver<FileMessage>` is single-consumer FIFO; `mpsc::Receiver<CommitResult>` is also single-consumer FIFO; the `VecDeque<expected_checksums>` pairing in `PipelinedReceiver` is unchanged. |
| 4 | `flush_workers` / `try_unwrap` barrier on `Arc<SlotBarrier>` | yes (out of scope) | Engine-side `ParallelDeltaApplier` is `parallel-receive-delta` and not on the production tokio path; see `project_parallel_interop_parity_gap.md`. |
| 5 | `PipelinedReceiver::shutdown -> JoinHandle::join` | at risk -> defended | Tokio `JoinHandle::abort()` does not wait. Contract: `shutdown()` becomes async, sends `FileMessage::Shutdown` on the mpsc, then `.await`s the disk task's `JoinHandle`. No `.abort()` on the disk task; `Drop` for `PipelinedReceiver` calls `Handle::block_on(shutdown())` as the only sync entry into an async shutdown (documented exception). |
| 6 | Phase 1 -> phase 2 redo barrier | yes | `run_pipelined` becomes async; phase-1 drain becomes `while let Some(_) = result_rx.recv().await {}` before phase 2 with `REDO_CHECKSUM_LENGTH`. Same barrier shape. |
| 7 | Multiplex flush-before-block invariant | at risk -> defended | Encoded in the writer wrapper: `AsyncWriter::flush().await` must precede any `AsyncReader::read*().await` on the paired transport. Implementation enforces via debug-assert in the writer guard; ASY-5 capture-replay asserts frame ordering. Call sites: #2, #5. |
| 8 | Single-owner io_uring / IOCP rings | yes | #9 owns the ring for the connection's disk-task lifetime; ring is created and dropped inside the `spawn_blocking`. #10 does not cross threads. |

Per-connection panic isolation is preserved separately: `tokio::spawn`
captures panics in the join handle; daemon dispatch (#11) matches on
the result and logs-and-continues, equivalent to the existing
`catch_unwind` shield in `server_runtime/connection.rs:184`.

## 4. Reconciliation with ASY-2's preliminary cut

No `.await` / `spawn_blocking` / "dissolves" label flipped. Two
refinements:

- **#9 (disk commit):** ASY-2 said "`spawn_blocking` island, same
  single-owner discipline as today." ASY-3 binds granularity to one
  **long-lived** `spawn_blocking` per connection driving an async
  loop via `Handle::block_on`, not per-file. Per-file would shred
  the ring and burn pool slots. Refinement (not flip) because
  "single-owner" already implied long-lived ownership.
- **#10 (io_uring `submit_and_wait`):** ASY-2 said "`spawn_blocking`
  until ASY-9 lands `tokio-uring`." ASY-3 clarifies there is **no
  separate** `spawn_blocking`; the ring is called from inside #9's
  blocking task.

Both refinements are mechanical, not directional. ASY-2's punt to
ASY-9 for a native `tokio-uring` driver stands.

## 5. Cross-cutting concerns for ASY-7

Three concerns need a design-doc-level decision before code lands.
ASY-3 surfaces them with a proposed answer; ASY-7 adopts or rebuts.

### 5.1 Cancellation semantics

Question: what does a dropped `Receiver` (or generator) future leave
behind?

Proposal: every per-connection tokio task tree shares a
`tokio_util::sync::CancellationToken`, held by the receiver/generator
future, the disk task (#9), and basis-load tasks (#3, #8). Dropping
the top-level `core::session()` future triggers the token; downstream
tasks exit cooperatively:

- mpsc senders drop; receivers see `None` from `recv().await` and
  exit.
- `spawn_blocking` tasks (#3, #8) run to completion; results are
  discarded (no observable side effect beyond filesystem cache and
  `Bytes` allocations).
- The disk task (#9) catches cancellation between files; the
  in-flight file completes to its temp-file rename (atomic) or is
  left as `.tmp.<pid>.<rand>` for the next run to reap.

Non-proposal: no `JoinHandle::abort()` on any task holding a kernel
resource. Abort is unsafe for io_uring / IOCP / fsync in flight; the
kernel holds SQEs until `submit_and_wait(0)` harvests them.
Cooperative-only cancellation is mandatory for #9 and #10.

ASY-7 must confirm cooperative-only cancellation and reject
`.abort()` on any task owning a kernel resource.

### 5.2 Error propagation (sync `io::Error` -> async `Result`)

Question: how do errors from blocking islands surface to async
callers without losing the path-context wrapping from the `io::Error`
extension trait in `crates/transfer`?

Proposal: blocking islands flow through one helper:

```rust
fn join_error_to_io(e: tokio::task::JoinError) -> io::Error {
    if e.is_panic() {
        io::Error::new(io::ErrorKind::Other, format!("blocking task panic: {e}"))
    } else {
        io::Error::new(io::ErrorKind::Interrupted, "blocking task cancelled")
    }
}

async fn blocking_io<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(join_error_to_io)?
}
```

The inner closure runs sync and applies the extension-trait
path-context wrapping before returning, so context is preserved.
Channel disconnect surfaces identically to today:
`mpsc::error::SendError` -> `io::ErrorKind::BrokenPipe`;
`recv() -> None` is graceful EOF, same as
`spsc::RecvError::Disconnected`. ASY-7 must confirm `blocking_io` is
the single chokepoint and forbid ad-hoc `spawn_blocking` inside
`transfer` (CI grep enforces).

### 5.3 Logging context propagation

Question: per-connection tracing spans must follow work across the
`spawn_blocking` boundary. Today logging is scoped by a thread-local
in the worker thread; under `spawn_blocking` the thread is a shared
pool worker, so thread-local scoping leaks across connections.

Proposal: use `tracing::Instrument` at every `tokio::spawn` site and
manually enter the span inside every `spawn_blocking` closure
(`Span::current()` -> `let _enter = span.enter()`). For mpsc messages
crossing task boundaries (#6, #7), the span follows the receiving
task (also `.instrument`-ed), not the message - matching today's
thread-local model where each thread carries its own connection
identity. `logging` / `logging-sink` crates are unchanged. Pool-thread
reuse does not contaminate logs because spans are per-task, not
per-thread. ASY-7 must mandate `.instrument(span)` at every
`tokio::spawn` and `spawn_blocking` site in `transfer` and add a CI
grep to enforce.

## 6. Not decided here

- Native `tokio-uring` driver for #10 (punted to ASY-9 per ASY-2
  section 8).
- Whether #8 migrates off rayon entirely (kept; ASY-6 measures
  blocking-pool starvation under daemon load).
- Default flip `tokio-transfer = on` (ASY-12 gate).
- Per-platform IOCP analogue inherits the #9/#10 contract via the
  shared single-owner-ring invariant; no separate spec.

## 7. Cross-references

- `docs/audits/asy-1-threading-model.md` - boundary numbering and
  preserved-invariant catalogue.
- `docs/design/asy-2-tokio-runtime-feature.md` - feature flag,
  runtime ownership, ASY-9 punt.
- `docs/audits/async-daemon-listener.md` - #11 rollout source.
- `docs/audits/async-ssh-transport.md` - #12 dissolution source.
- `docs/audits/spsc-disk-commit-utilization.md` - capacity numbers
  for the #6/#7 mpsc swap.
- `docs/design/capture-replay-harness.md` - test contract for
  invariants 1, 2, 3, 7.
- `project_io_uring_shared_ring_bottleneck.md` - single-owner
  discipline that pins #9 / #10's shape.
- `project_no_async_threaded_only.md` - constraint that
  `tokio-transfer` cooperatively lifts under a feature flag.
- `project_parallel_interop_parity_gap.md` - reason invariant 4 is
  out of scope.
