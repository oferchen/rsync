# KQ-1: macOS kqueue transfer pipeline audit

Tracking issue: KQ parent. Sibling design: `docs/design/kqueue-async-file-writer.md`
(KQ-2). Foundational primitive: `crates/fast_io/src/kqueue/mod.rs::KqueueLoop`
(`kqueue(2)`/`kevent(2)` wrapper with `EVFILT_READ` and `EVFILT_WRITE`,
runtime-probed via `is_kqueue_available()`). Earlier design context lives in
`docs/design/macos-kqueue-fast-io.md`.

This audit catalogues every transfer-pipeline surface on macOS that currently
runs synchronously and could be reworked onto `KqueueLoop`. It is the
inventory pass; per-surface designs land in follow-up tasks (KQ-2 covers the
disk-writer; KQ-S.1..5 cover daemon accept, SSH child monitoring, multiplex
socket I/O, the bandwidth timer, and vnode watchers).

## Upstream rsync reference

Upstream rsync 3.4.1 uses synchronous blocking I/O on macOS for every path
listed below (see `target/interop/upstream-src/rsync-3.4.1/`):

- `socket.c:open_socket_in()` calls `listen(2)` then blocks in `accept(2)`.
- `io.c:read_timeout()` / `writefd_unbuffered()` use blocking `read(2)` and
  `write(2)` with a `select(2)` timeout shim, not kqueue.
- `pipe.c:piped_child()` reaps the remote shell child with `waitpid(2)`
  during cleanup.
- `io.c:sleep_for_bwlimit()` calls `nanosleep(2)` directly.

This audit therefore treats "the macOS path matches upstream rsync" as a
correctness floor; kqueue migration is a performance-only proposal. Wire
bytes do not change.

## Surface table

| Surface                       | Current implementation                                                                                                                                                                                                          | Path                                                                              | Lines           | Gap vs kqueue-driven async                                                                                                                                                                                                |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Receiver disk-commit writer   | Synchronous `ReusableBufWriter` dispatched from the `Writer::Buffered` arm. 256 KB reusable buffer fronts `std::fs::File`; large chunks call `write_all_vectored`. Each `write_all` blocks the disk-commit thread on writeback. | `crates/transfer/src/disk_commit/writer.rs`                                       | 1-150           | No `EVFILT_WRITE` parking. Worker thread blocks instead of dispatching the next chunk while the kernel drains buffers; KQ-2 specifies the replacement.                                                                    |
| Sender file-read              | Synchronous `read(2)` loop through `BufReader` or `mmap`. On macOS the chunked reader is the stub in `crates/fast_io/src/mmap_reader_stub.rs`. No `EVFILT_READ` queueing.                                                       | `crates/transfer/src/reader/`, `crates/fast_io/src/mmap_reader_stub.rs`           | n/a             | No readiness wait; large files block the sender thread on `read(2)` instead of overlapping with the socket-write side. KQ-4 / KQ-5 cover the migration.                                                                   |
| Daemon accept (single listen) | `run_single_listener_loop` sets the `TcpListener` non-blocking, then polls with `thread::sleep(SIGNAL_CHECK_INTERVAL)` between `accept()` attempts.                                                                             | `crates/daemon/src/daemon/sections/server_runtime/connection.rs`                  | 334-401         | Sleep-polling adds latency proportional to `SIGNAL_CHECK_INTERVAL` per accept. Submitting `EVFILT_READ` on the listener fd would block until either readiness or signal-flag wakeup, eliminating the sleep.              |
| Daemon accept (dual stack)    | `run_dual_stack_loop` spawns one acceptor thread per listener, each non-blocking with `thread::sleep(Duration::from_millis(50))` between attempts; results funnel through an MPSC channel.                                      | `crates/daemon/src/daemon/sections/server_runtime/connection.rs`                  | 408-460         | Same sleep-polling pattern, plus N extra threads. A single `KqueueLoop` registering both listener fds would replace the thread fan-out entirely.                                                                          |
| Daemon socket I/O             | Per-connection worker reads and writes synchronously through `rsync_io` framed transport. No multiplexing across connections.                                                                                                   | `crates/rsync_io/src/`, dispatched by daemon worker threads                       | n/a             | Each connection holds a thread for the duration of the transfer. `EVFILT_READ`/`EVFILT_WRITE` multiplexing across connection fds would unblock D10K-3..5 ceiling work. Covered by KQ-S.3.                                |
| SSH child monitoring          | `SshChildHandle::try_wait()` polls `child.try_wait()` from the synchronous reaper path (`spawn_child_with_args` site reapers).                                                                                                  | `crates/rsync_io/src/ssh/connection.rs`                                           | 163, 501, 572   | `try_wait` returns `Ok(None)` without sleeping, but callers either spin or rely on the connection close to drive progress. `EVFILT_PROC` with `NOTE_EXIT` would deliver an event when the child reaps. KQ-S.2 covers it. |
| Bandwidth limiter timer       | `BandwidthLimiter::register` schedules sleeps via `std::thread::sleep`, chunked by `MAX_SLEEP_DURATION`. PR #5818 is in flight to swap this for `EVFILT_TIMER`; it has not landed on master.                                    | `crates/bandwidth/src/limiter/sleep.rs`                                           | 88, 94          | Current thread sleeps coarsen to the OS scheduler quantum (~1 ms). `EVFILT_TIMER` resolves at sub-ms with the same `KqueueLoop` already owned by the disk-commit thread. KQ-S.4 / PR #5818 owns the migration.            |

## Composition rules carried forward

- One `KqueueLoop` per long-lived thread (disk-commit, daemon accept,
  per-connection worker, SSH reaper). This mirrors the
  "one io_uring per thread" rule in
  `docs/design/io-uring-rayon-composition.md` and prevents the cross-thread
  `Sync` issue baked into `KqueueLoop` (`Send` only).
- `EVFILT_TIMER` and `EVFILT_PROC` filters are not yet exposed by
  `KqueueLoop`; KQ-S.2 and KQ-S.4 must extend the `KEventFilter` enum before
  they can wire callers.
- All migrations stay behind feature flags until KQ-7 bench numbers justify
  flipping default-on (KQ-8).

## Out of scope

- Sender file-read (covered by KQ-4 / KQ-5; this audit notes it but does
  not design the migration).
- `EVFILT_VNODE` for `--daemon` config-reload (KQ-S.5; scoped to module
  changes, not transfer hot path).
- Cross-platform parity matrix (FP344-8 owns the macOS/Linux/Windows
  acceleration document).

## Reference

- `kqueue(2)` / `kevent(2)`: Apple Open Source `xnu/bsd/sys/event.h`
  documents the filter set, the `EV_*` flags, and the timespec semantics
  the `KqueueLoop` wrapper relies on. Equivalent BSD man page text ships
  with macOS as `man 2 kqueue`.
