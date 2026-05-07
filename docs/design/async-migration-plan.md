# Async Migration Plan (#1594)

Concise roadmap for incremental async adoption in oc-rsync. Wire
protocol, CLI flags, and exit codes are out of scope.

## 1. Current state

- `tokio` dependency is feature-gated and confined to two crates:
  - `daemon` (default `async-daemon` feature) drives the listener
    and per-connection async session via `AsyncDaemonListener`.
  - `core` (`async` feature) hosts the bridge surface used by the
    daemon and embedded SSH.
- Transfer hot loops stay sync-first: `run_pipeline_loop_decoupled`
  in `crates/transfer/src/receiver/transfer/pipeline.rs`, the SPSC
  channel in `crates/transfer/src/pipeline/spsc.rs`, the rayon
  CPU paths, and the `BufferPool`.
- Cross-references: #1732 (channel abstraction landed) and #1818
  (sync receiver baseline measured) are both done; this plan
  builds on their conclusions.

## 2. Pressure points

- **Async daemon listener** - #1934 RFC done; #1935 implementation
  promotes `AsyncDaemonListener` to the default accept loop.
- **Async SSH transport** - #1593 evaluation completed; the
  embedded backend already builds a tokio current-thread runtime,
  so the async surface is paid for but not exposed.
- **io_uring socket I/O integration** - `fast_io` keeps io_uring
  sync; the open question is whether async submission improves
  the receiver's network-to-disk handoff. Tracked under #1595.

## 3. Incremental strategy

- **Phase 1 - DONE.** Async daemon listener landed behind
  `async-daemon` (default-on). Sync workers handle the transfer
  state machine; only bind/accept/handoff is async.
- **Phase 2 - in flight.** Async SSH transport behind a feature
  flag (`async-ssh`). Lifts `connect_and_exec_async` to a public
  surface for callers that already own a runtime; sync facade
  remains for the rest.
- **Phase 3.** Bridge async I/O into rayon-driven transfer loops
  via `spawn_blocking`; per #1751 the bridge is one-shot per
  CPU burst and never `Handle::block_on` from a rayon worker.
- **Phase 4.** Full async io_uring integration evaluated under
  #1595. Decision pending: stay with sync io_uring driven from
  `spawn_blocking`, or adopt `tokio-uring` for single-threaded
  callers behind a separate feature.

## 4. Anti-goals

- No async per-file fanout. The receiver fans work via rayon, not
  tokio tasks; spawning a future per file would add wakeup cost
  on a path the SPSC + reorder buffer already paces.
- No async file syscalls. Disk reads, writes, fsync, and the
  fast_io fast paths block on a rayon thread (or the tokio
  blocking pool via `spawn_blocking`), never on a runtime worker.
- No `tokio::sync::Mutex` in hot paths. Buffer pool, signature
  index, and reorder buffer keep their `parking_lot` / lock-free
  primitives.

## 5. Compat

- Async features (`async-daemon`, `async-ssh`, future
  `async-transfer`) remain feature-gated until each clears its
  exit gates: golden byte tests, criterion regression budget,
  coverage floor, and `tools/ci/run_interop.sh` against upstream
  3.0.9, 3.1.3, and 3.4.1.
- `--no-default-features` builds stay tokio-free; the boundary is
  enforced by `tools/ci/check_tokio_boundary.sh`.
- Runtime kill switches (`OC_RSYNC_DAEMON_ASYNC=0`,
  `OC_RSYNC_SSH_ASYNC=0`) revert to the sync path on next process
  start; no rebuild required.

## 6. References

- #1593 async SSH evaluation, #1594 this plan, #1595 async
  io_uring evaluation, #1732 channel abstraction, #1751
  spawn_blocking bridge, #1818 sync receiver baseline, #1934
  async listener RFC, #1935 tokio listener implementation.
- `docs/design/daemon-async-accept-sync-workers.md`,
  `docs/design/async-channel-abstraction.md`,
  `docs/design/io-uring-rayon-composition.md`.
