# ASY-1: Threading model audit of the transfer pipeline

Map of every synchronous barrier, channel boundary, and runtime hop in the
oc-rsync transfer pipeline (Generator + Receiver + disk-commit thread).
Feeds ASY-2 (design) through ASY-6 (adopt-or-skip decision). This is a map,
not a redesign; remediation lives in follow-up tasks.

## Scope

- The "Sender" role does not exist as a separate runtime entity in oc-rsync.
  The wire-side sender work runs inside `GeneratorContext` (see
  `crates/transfer/src/generator/transfer/transfer_loop.rs`); the dispatcher
  in `crates/transfer/src/lib.rs::run_server_with_handshake` only ever
  branches on `ServerRole::Receiver` vs `ServerRole::Generator`.
- Daemon listener (tokio multi-thread runtime, opt-in via `async-daemon`)
  and SSH client transport (current-thread tokio runtime) are mapped under
  "tokio integration".
- Out of scope: in-process unit-test threads, fuzzers, `xtask`, anything
  under `crates/transfer/src/pipeline/async_pipeline.rs`
  (`#[cfg(feature = "async")]` skeleton not yet wired into production).

## Flowchart

```text
                +---------------------------------------------+
                |          Server thread (sync)               |
                |  run_server_with_handshake -> Receiver      |
                +---------------------------------------------+
                       |                              ^
                       v wire (Read)                  | wire (Write, multiplex)
                +---------------+              +---------------+
                | ServerReader  |              | ServerWriter  |
                +---------------+              +---------------+
                       |                              ^
                       | NDX requests outbound        | delta responses inbound
                       v                              |
   +----------------------------------------------------------------+
   |  Generator side (peer, separate process: upstream rsync or     |
   |  another oc-rsync `ServerRole::Generator`)                     |
   +----------------------------------------------------------------+

Receiver-internal pipeline (single process, multiple threads):

  +----------------------+    spsc::Sender<FileMessage>   +----------------+
  | Receiver / network   | -----------------------------> | disk-commit    |
  | thread               |   capacity = 128 (default,     | thread         |
  |  - run_pipelined     |    clamped 8..=4096)           |  - process_file|
  |  - run_pipeline_loop_|                                |  - whole_file  |
  |    decoupled         | <----------------------------- |                |
  |  - rayon par_iter    |   spsc::Receiver<              |  io_uring /    |
  |    on signature      |     io::Result<CommitResult>>  |  IOCP /        |
  |    batch (>= sig     |   capacity = 256               |  buffered fs   |
  |    threshold)        |                                |                |
  |                      | <----------------------------- |                |
  |                      |   spsc::Receiver<Vec<u8>>      |                |
  |                      |   capacity = 256 (buf recycle) |                |
  +----------------------+                                +----------------+
```

The SPSC channels are lock-free (`crossbeam_queue::ArrayQueue` +
`AtomicBool` disconnect flags + `std::hint::spin_loop` on full/empty).
Both producer and consumer block by spinning - no syscall, no
`thread::park`, no condvar. See `crates/transfer/src/pipeline/spsc.rs`.

## Per-stage table

| Stage | Thread origin | Inbound channels | Outbound channels | Blocking sites | Sync barriers held |
|-------|---------------|------------------|-------------------|----------------|---------------------|
| Generator (`GeneratorContext::run`) | Caller stack (server thread spawned by daemon `thread::spawn`, by SSH `spawn_blocking`, or by foreground CLI) | wire `Read` (NDX requests from receiver) | wire `Write` (file list, sum heads, delta tokens), multiplexed | `reader.read`, `writer.flush`, `writer.write_all`, basis file `read_to_end`, `MapFile::map_window` | None (single-threaded role; segment scheduling and ndx codecs are stack-local) |
| Receiver / network (`ReceiverContext::run_pipelined` -> `run_pipeline_loop_decoupled`) | Caller stack (same lineage as generator) | wire `Read` (delta responses), `spsc::Receiver<CommitResult>`, `spsc::Receiver<Vec<u8>>` (buf recycle) | wire `Write` (NDX requests, multiplexed warnings via `MsgInfoSender`), `spsc::Sender<FileMessage>` | `reader.read` for delta tokens, `writer.flush` when `flushed_pending == 0`, `spsc::Sender::send` spin-waits when ring full, `try_recv` polls for buf recycle, `rayon::ThreadPool::install` boundary in `par_iter` signature batch | Sequential `zip` over `par_iter().collect()` to preserve wire ordering; redo-pass barrier between phase 1 and phase 2 |
| Signature batch worker (rayon) | `rayon` global pool (per-iteration) | `&[(idx, FileEntry, PathBuf)]` slice | `Vec<BasisResult>` collected back | `find_basis_file_with_config` opens basis files and reads them | Implicit `par_iter().collect()` barrier - all workers must finish before sequential `zip` send loop runs |
| Disk commit (`disk_commit::thread::disk_thread_main`) | `thread::Builder::new().name("disk-commit").spawn(...)` from `spawn_disk_thread` | `spsc::Receiver<FileMessage>` (capacity 128 default) | `spsc::Sender<io::Result<CommitResult>>` (capacity 256), `spsc::Sender<Vec<u8>>` (capacity 256) | `file_rx.recv()` spin-wait, `process_file`/`process_whole_file` blocking I/O, `io_uring submit_and_wait(1)` in `fast_io::IoUringDiskBatch`, optional `fsync`, optional ACL/xattr syscalls | None - single-threaded after spawn; owns the io_uring / IOCP ring exclusively |
| Daemon listener (sync default) | `thread::spawn` per accepted connection (`spawn_connection_worker` in `crates/daemon/src/daemon/sections/server_runtime/connection.rs:178`) | `std::net::TcpStream` (blocking) | per-worker stdout/stderr to socket | Blocking `accept()` in the parent thread, `read`/`write` on `TcpStream` | `connection_counter` semaphore (`ConnectionGuard`), shared `Arc<modules>` (read-only) |
| Daemon listener (async, `async-daemon` feature) | `tokio::runtime::Builder::new_multi_thread().worker_threads(N)` in `run_hybrid_listener` (`crates/daemon/src/async_listener.rs:73`) | `tokio::net::TcpListener` accept | dispatches each stream to `tokio::task::spawn_blocking` running the sync worker | `tokio::time::timeout(250ms, accept)` polling loop on shutdown flag, blocking pool absorbs the synchronous per-connection worker | `Arc<AtomicBool> shutdown`, tokio runtime `block_on` at the entry point |
| SSH client transport (`async_ssh_transport.rs`) | `tokio::runtime::Builder::new_current_thread()` + `runtime.block_on(...)` (`crates/core/src/client/remote/async_ssh_transport.rs:245`) | `tokio_mpsc::Receiver<Vec<u8>>` (capacity = `CHANNEL_CAPACITY`) for ssh stdout, `std_mpsc::Receiver<Vec<u8>>` for sync-side reader | `tokio_mpsc::Sender<Vec<u8>>` for ssh stdin, `std_mpsc::SyncSender<Vec<u8>>` for sync-side writer | `AsyncRead::read.await`, `AsyncWrite::write_all.await`, blocking `std_mpsc::Receiver::recv` inside `spawn_blocking`, `blocking_send` from sync side into tokio mpsc | Three `tokio::spawn` pumps (outbound, inbound, reader-fanout), one `spawn_blocking` writer-fanin, one `spawn_blocking` running the entire sync server (`run_blocking_server`); joined sequentially after the server task |

`rayon::join`, `rayon::scope`, and `into_par_iter` calls inside
`crates/engine/src/concurrent_delta/` are reachable from the receiver only
when the experimental `parallel-receive-delta` feature is enabled. They
are documented in `project_parallel_interop_parity_gap.md`; this audit
treats them as out-of-band of the production transfer pipeline.

## Tokio integration

- **Daemon accept loop (default build):** purely synchronous. Each
  `TcpStream` accepted by the main thread is handed to a
  `thread::spawn`-created worker that calls into
  `transfer::run_server_with_handshake`. No tokio is loaded.
- **Daemon accept loop (`async-daemon` feature):**
  `tokio::runtime::Builder::new_multi_thread()` runs `accept_loop`. Every
  accepted connection is converted to a blocking `std::net::TcpStream` via
  `into_std()` + `set_nonblocking(false)` and handed to
  `tokio::task::spawn_blocking(move || worker(std_stream, peer_addr))`.
  The sync worker is the same callable used by the default thread-per-
  connection path; the only difference is which thread it runs on. The
  rollout gate is documented in
  `docs/audits/async-daemon-listener.md`.
- **SSH client transport (always-on when remote shell mode is selected):**
  `async_ssh_transport.rs` builds a `current_thread` tokio runtime and
  uses `block_on` to host (a) inbound/outbound byte pumps and
  (b) `tokio::task::spawn_blocking(run_blocking_server)` which runs the
  whole synchronous `run_server_with_handshake` body. Cross-runtime data
  passes through paired `tokio_mpsc` and `std_mpsc::sync_channel` queues
  with `blocking_send` on the boundary.
- **Receiver/Generator transfer hot path:** zero tokio. Disk-commit is a
  plain `std::thread`; channels are `pipeline::spsc`.

## Candidate async boundaries

Each entry below is a current blocking site that could become an `.await`
under a future async design. ASY-2 picks which (if any) to convert; ASY-6
decides the global adopt/skip. Listed without prescription:

1. Generator `reader.read` (wire ingress of NDX requests) in
   `generator/transfer/transfer_loop.rs`.
2. Generator `writer.flush` / `writer.write_all` for outbound delta
   stream (same file).
3. Generator basis-file `read_to_end` / `MapFile::map_window` for
   delta-source ingest in `generator/transfer/transfer_loop.rs` and
   `engine/src/delta/`.
4. Receiver `reader.read` for delta-token responses in
   `transfer_ops/response.rs::process_file_response_streaming`.
5. Receiver `writer.flush` gated on `flushed_pending == 0` in
   `receiver/transfer/pipeline.rs`.
6. `pipeline::spsc::Sender::send` spin-wait when the disk-commit ring is
   full (`pipeline/spsc.rs:74`).
7. `pipeline::spsc::Receiver::recv` spin-wait in the disk-commit thread
   when the network thread starves it (`pipeline/spsc.rs:110`).
8. `find_basis_file_with_config` opens/reads basis files inside the
   rayon worker (`receiver/basis.rs`); currently amortised by
   `par_iter` but each call is blocking I/O.
9. `disk_commit::process_file` / `process_whole_file` blocking write +
   optional fsync + ACL/xattr metadata syscalls
   (`disk_commit/process.rs`).
10. `fast_io::IoUringDiskBatch::submit_and_wait(1)`
    (`crates/fast_io/src/io_uring/file_writer.rs:149,301`,
    `file_reader.rs:136,234`). Owned by the disk-commit thread; ASY-9
    tracks the proper async hand-off.
11. Daemon accept loop blocking `TcpListener::accept` in the sync default
    path (`crates/daemon/src/daemon/sections/server_runtime/`).
12. SSH transport `spawn_blocking(run_blocking_server)` + paired
    `std_mpsc::sync_channel` boundary (`async_ssh_transport.rs:337-369`)
    is the largest single sync island wrapped by an existing tokio
    runtime; an end-to-end async path would let this dissolve.

## Preserved

Sync semantics that must not regress, regardless of which boundaries get
converted:

- **Wire-byte parity with upstream rsync 3.4.1 / 3.4.2.** Golden tests in
  `crates/protocol/tests/golden/` and the interop harness
  (`tools/ci/run_interop.sh`) gate any reordering.
- **In-order NDX request dispatch in the receiver.** The pipeline relies
  on `par_iter().collect()` followed by a sequential `zip` send loop;
  the comment at `receiver/transfer/pipeline.rs:186-188` documents this
  invariant. Any async conversion must keep send-side ordering.
- **In-order disk commit per file.** `PipelinedReceiver` consumes
  `CommitResult`s FIFO against `expected_checksums: VecDeque<...>` and
  records redo indices in that order
  (`pipeline/receiver.rs`).
- **`flush_workers` / `try_unwrap` barrier on `Arc<SlotBarrier>`.**
  Engine-side `ParallelDeltaApplier` semantics
  (`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:585,622,667`)
  are unrelated to the production pipeline today but must stay
  barrier-preserving under any future feature-gated rollout
  (cross-ref `project_parallel_interop_parity_gap.md`).
- **`PipelinedReceiver::shutdown` -> `JoinHandle::join`.** Drop and
  explicit shutdown both send `FileMessage::Shutdown` and join the
  disk-commit thread; this is the only point where the network side
  blocks on a thread join (`pipeline/receiver.rs:347-358,375-383`).
- **Phase 1 -> phase 2 redo barrier.** `run_pipelined` drains phase 1
  fully before issuing the redo pass with `REDO_CHECKSUM_LENGTH`
  (`receiver/transfer/pipelined.rs:97-156`).
- **Multiplex flush-before-block invariant.** Generator flushes the
  output stream before blocking on `recv_filter_list` to avoid daemon
  pull deadlock (`generator/transfer/orchestrator.rs:55-65`); receiver
  flushes buffered itemize messages before NDX_DONE handshake. Any
  async conversion must keep these flushes ahead of the read.
- **Single-owner io_uring / IOCP rings.** `fast_io::IoUringDiskBatch`
  and `IocpDiskBatch` are created once on the disk-commit thread and
  never shared; an async hand-off must preserve single-owner discipline
  (cross-ref `project_io_uring_shared_ring_bottleneck.md`).
- **Per-connection panic isolation.** Daemon worker threads use
  `catch_unwind` to keep a panic from killing the listener
  (`server_runtime/connection.rs:184`). The async-daemon path relies
  on tokio's join-handle panic capture; both must remain in place.

## Counts

- Blocking sites identified as candidate async boundaries: **12**
  (numbered list above).
- Sync semantics catalogued as preserved: **8** (bulleted list above).

## Cross-references

- `docs/audits/async-daemon-listener.md` - rollout gate for the hybrid
  listener.
- `docs/audits/async-ssh-transport.md` - current SSH transport design.
- `docs/audits/tokio-dependency-boundary-2026.md` - workspace-wide
  tokio surface.
- `docs/audits/spsc-disk-commit-utilization.md` - measurement of the
  SPSC channel that connects the network and disk threads.
- `project_no_async_threaded_only.md` - standing constraint that the
  pipeline is not embeddable in tokio without `spawn_blocking`.
- `project_parallel_interop_parity_gap.md` - feature-gated parallel
  delta apply, kept out of this audit's scope.
- `project_io_uring_shared_ring_bottleneck.md` - single-owner ring
  invariant that bounds option space for ASY-9.
