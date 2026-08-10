# Daemon Event-Loop Multiplexing Audit

Tracking: oc-rsync task #1675.

## Summary

This audit evaluates whether the oc-rsync daemon should adopt epoll (Linux) or
kqueue (BSD/macOS) based event-loop multiplexing in place of the current
thread-per-connection model. The motivating scenario is high-fanout deployments
where many clients hold long-lived but mostly-idle TCP connections (for
example, a fleet of backup agents periodically polling a daemon for module
listings). Each idle connection currently parks a kernel thread on a blocking
read, which scales poorly past a few thousand clients.

The conclusion is that the right path is option (b) - extend the existing
`tokio` async session feature already present in `crates/daemon/src/daemon/async_session/`,
not introduce a new raw `mio` event loop alongside the synchronous one. This
keeps oc-rsync close to upstream rsync's per-connection isolation guarantees
(a faulting session never tears down the daemon), removes one accept-loop
implementation, and reuses the existing `tokio` dependency that the workspace
already pulls in. Until a deployment with thousands of concurrent idle
connections actually exists, the work is not urgent and can stay deferred -
the synchronous path is correct, well-tested, and parity-matched with upstream
3.4.1.

## Current model

oc-rsync's daemon spawns one OS thread per accepted TCP connection. Two
accept-loop variants live in `crates/daemon/src/daemon/sections/server_runtime/`:

- Single-listener loop: `run_single_listener_loop`
  (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:216`).
  Sets the listener non-blocking
  (`connection.rs:222`), then loops on `listener.accept()` with a
  `thread::sleep(SIGNAL_CHECK_INTERVAL)` of 500 ms when `WouldBlock` is
  returned (`connection.rs:251-253`). On every successful accept it calls
  `spawn_connection_worker` (`connection.rs:245`).
- Dual-stack loop: `run_dual_stack_loop`
  (`connection.rs:281`). Spawns one acceptor thread per listener
  (`connection.rs:305`), each polling its non-blocking listener with a 50 ms
  sleep on `WouldBlock` (`connection.rs:316`). Accepted streams are fanned
  through a `mpsc::channel` (`connection.rs:288`) and the main loop calls
  `spawn_connection_worker` for each (`connection.rs:346`).

Both paths funnel into:

- `spawn_connection_worker` (`connection.rs:106`), which calls
  `thread::spawn(move || ...)` (`connection.rs:121`). Inside the thread,
  `std::panic::catch_unwind` (`connection.rs:127`) isolates panics so a
  faulting session does not tear down the daemon. This is documented as the
  thread-equivalent of upstream's per-connection fork
  (`accept_loop.rs:1-10`, `connection.rs:103-105`).

Worker lifecycle is managed by:

- `reap_finished_workers` (`crates/daemon/src/daemon/sections/server_runtime/workers.rs:7`),
  called from `check_signals_and_maintain` (`connection.rs:33`) on every
  iteration.
- `drain_workers` (`workers.rs:23`), called once on shutdown
  (`accept_loop.rs:296`).
- `join_worker` (`workers.rs:38`), which treats `BrokenPipe`,
  `ConnectionReset`, and `ConnectionAborted` as success
  (`workers.rs:75-82`) so an orderly client disconnect is not surfaced as
  a fatal error.

Signal handling between accept iterations runs at the same 500 ms cadence as
the accept loop's `WouldBlock` sleep
(`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`). SIGHUP
reloads the config (`connection.rs:66-75`), SIGUSR1 drains active
connections then exits (`connection.rs:46-64`), SIGUSR2 logs a progress
summary (`connection.rs:78-85`), and SIGTERM/SIGINT shut down immediately
(`connection.rs:35-43`).

### Existing async-feature pattern

A parallel `tokio`-based listener already exists, gated behind the `async`
Cargo feature in `crates/daemon/Cargo.toml:20`. The feature pulls in
`tokio` with `net`, `io-util`, `sync`, `rt`, `time`
(`crates/daemon/Cargo.toml:43`). The implementation lives in
`crates/daemon/src/daemon/async_session/`:

- `AsyncDaemonListener::serve`
  (`crates/daemon/src/daemon/async_session/listener.rs:180`) runs an
  accept loop using `tokio::select!` between
  `self.listener.accept()` and a `broadcast` shutdown channel
  (`listener.rs:184-255`).
- A `tokio::sync::Semaphore` bounds concurrent connections
  (`listener.rs:113`, capacity from `ListenerConfig::max_connections`,
  default 200 at `listener.rs:25`).
- Each accepted connection becomes a `tokio::spawn` task
  (`listener.rs:216`); the comment at `listener.rs:211-215` documents
  that tokio's `JoinError` on panic is the async equivalent of upstream's
  fork-per-connection isolation.
- The module is currently wired only into the test suite (the public
  re-export at `mod.rs:34-35` is `#[cfg(test)]`-gated and the
  module-level `#![allow(dead_code)]` at `mod.rs:28` confirms the path
  is not yet on a production code path).

This is the unfinished but in-tree async accept-loop scaffold that any
future event-loop work should extend.

## Upstream comparison

Upstream rsync 3.4.1 uses `select(2)` over a small set of listening sockets
(one per address family) and forks a child process for each accepted
connection. The relevant source is in
`target/interop/upstream-src/rsync-3.4.1/`:

- `clientserver.c:1496` `daemon_main()` is the entry point. It calls
  `become_daemon()` (`clientserver.c:1521`), `log_init` (`clientserver.c:1528`),
  then `start_accept_loop(rsync_port, start_daemon)`
  (`clientserver.c:1536`).
- `socket.c:533` `start_accept_loop()` opens the listening socket(s) via
  `open_socket_in()` (`socket.c:543`), calls `listen(2)` per fd
  (`socket.c:550`), populates an `fd_set` via `FD_SET`
  (`socket.c:559`), and enters the accept loop at `socket.c:566`.
- The loop blocks indefinitely in `select(maxfd + 1, &fds, NULL, NULL, NULL)`
  (`socket.c:584`), accepts on whichever fd became readable
  (`socket.c:587-589`), and forks: `if ((pid = fork()) == 0)` at
  `socket.c:599`. The child closes the listening sockets
  (`socket.c:603-604`), reopens the log file (`socket.c:607`), and
  invokes the per-connection handler `fn(fd, fd)` (`socket.c:608`,
  resolving to `start_daemon` at `clientserver.c:1275`). The parent
  closes the accepted fd (`socket.c:621`).
- SIGCHLD is reaped via `sigchld_handler()` registered at
  `socket.c:597` and reaped on entry at `socket.c:524-526`.

Two upstream details are load-bearing for any oc-rsync redesign:

1. Upstream uses `select(2)` only across listening sockets; it never adds
   per-connection fds to the readable set. The forked child owns its
   accepted socket and runs entirely synchronously. This means upstream
   has *no* connection-multiplexing event loop. Each session is an
   isolated process performing blocking I/O on a single socket.
2. The fork-per-connection model gives crash isolation for free (a SIGSEGV
   in one session takes down only that child). oc-rsync emulates this with
   `catch_unwind` plus thread isolation
   (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:127`,
   referenced again in `workers.rs:36-37`).

`epoll`, `kqueue`, and `poll` are not used anywhere in the upstream daemon
path. A move to event-loop multiplexing is therefore an oc-rsync-only
optimisation, not a parity feature.

## Options analysis

Three concrete paths were evaluated.

### Option (a) - Raw epoll/kqueue via `mio` 1.x, sync workers per accepted FD

Shape: replace `run_single_listener_loop` and `run_dual_stack_loop` with a
`mio::Poll` registration of all listening fds. On a readiness event, `accept`
the new connection and hand it to a worker thread (sync I/O, identical to the
existing `handle_session` body). The event loop only multiplexes *accepts*
and signal-pipe wake-ups; per-connection I/O remains blocking on the worker
thread.

Pros:

- Tightest fit for the actual scaling problem: thousands of mostly-idle
  *listening* fds are not the issue here, but a `mio` accept loop also
  removes the `thread::sleep(SIGNAL_CHECK_INTERVAL)` busy-poll
  (`connection.rs:253`) and the per-acceptor-thread layout in dual-stack
  mode (`connection.rs:305`).
- No async runtime in the daemon's hot path. Sync workers stay byte-for-byte
  comparable to upstream's forked child, which simplifies upstream-fidelity
  reasoning.
- `mio` 1.x is a small, stable dependency (no transitive runtime), already
  proven in production (it underpins `tokio`'s reactor).

Cons:

- Adds a new dependency that is not currently in the workspace.
- Does *not* solve the high-fanout-idle-connections case. Each accepted
  connection still parks an OS thread on blocking reads. The win over the
  status quo is small (signal-flag latency drops from ~500 ms to event-driven,
  and the accept fast-path stops sleeping).
- Forks the codebase: now there is a sync path, an async path
  (`async_session/`), and a `mio` path. The `async` feature already exists
  to solve roughly this problem; introducing `mio` alongside it is
  duplicative.
- Cross-platform parity work: `mio` abstracts epoll/kqueue/IOCP, but
  signal-pipe wake-up has to be implemented per-OS (eventfd on Linux,
  pipe on BSD/macOS, IOCP completion port on Windows). The current
  `SignalFlags` (`AcceptLoopState::signal_flags` at
  `connection.rs:7`) is `AtomicBool`-based and would need an event source.

Kernel/OS requirements: epoll requires Linux 2.6+, kqueue requires FreeBSD
4.1+ / macOS 10.3+. Both are universally available on supported platforms.
Windows uses IOCP via `mio`, but we would need to verify socket readiness
semantics on Windows match Unix.

Complexity: medium. ~400-600 LoC of new code (event loop, signal-pipe,
dual-stack listener registration, dispatch to existing worker) plus tests.

Upstream parity: equivalent to the status quo for the per-connection path
(blocking I/O on a worker), better than status quo for the accept fast-path,
no upstream precedent for either.

### Option (b) - Full `tokio` accept loop with `spawn_blocking` workers

Shape: promote `crates/daemon/src/daemon/async_session/` from
`#[cfg(test)]`-only scaffolding to the production accept loop, behind the
existing `async` feature. The accept loop is `AsyncDaemonListener::serve`
(`async_session/listener.rs:180`). Per-connection sessions, which today
do blocking reads/writes inside `handle_session`, run via
`tokio::task::spawn_blocking` so the synchronous session code is reused
verbatim. The event loop multiplexes accepts, signal handling, and
`spawn_blocking` join futures on the tokio reactor.

Pros:

- Reuses code that is already written, reviewed, and unit-tested
  (`async_session/listener.rs`, `session.rs`, `shutdown.rs`).
- `tokio` is already a workspace dependency
  (`Cargo.toml:180`) used elsewhere; no new transitive crates.
- Preserves the upstream-equivalent crash-isolation model. Tokio surfaces
  a panic in a `spawn_blocking` task as a `JoinError`, mirroring the
  `catch_unwind` path documented at
  `async_session/listener.rs:211-215`.
- `tokio::sync::Semaphore` (`listener.rs:113`) gives a natural cap on
  concurrent connections, matching the existing `max_sessions` field
  (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:12`)
  but enforced at admission time rather than after the spawn.
- Signal handling can use `tokio::signal::unix` and a `broadcast` channel,
  removing the 500 ms poll on `SIGNAL_CHECK_INTERVAL` (`listener.rs:45`).
- One accept-loop implementation, not three.

Cons:

- `spawn_blocking` workers still consume one OS thread each *while a
  session is doing I/O* (which is most of the time for a real transfer).
  This option only wins on idle connections - exactly the target scenario
  - but it is *not* free for active transfers. Tokio's blocking-pool
  default (512 threads) bounds the active worker pool and may need tuning
  via `Builder::max_blocking_threads`.
- Migrates the daemon onto an async runtime. Although `tokio` is already
  in-tree, making the daemon depend on it unconditionally requires either
  flipping `async` to a default feature or rebuilding the synchronous
  path on top of it. The cleanest answer is to leave both paths in the
  tree behind the existing feature gate and route the binary's
  `daemon_main` through whichever is enabled.
- The async session module is currently `#[allow(dead_code)]`
  (`async_session/mod.rs:28`); a productionisation pass is needed
  before it is wired in (signal handling, log-sink integration, syslog,
  systemd notifier, PID file, dual-stack bind, socket-options injection,
  reverse DNS, proxy-protocol pre-read, bandwidth limit, all of which
  the sync path handles in `accept_loop.rs:62-285`).

Kernel/OS requirements: tokio uses `mio` underneath, so the same epoll /
kqueue / IOCP support matrix applies. Tokio 1.x targets Rust 1.70+ which
is well below our 1.88 toolchain.

Complexity: medium-high. The event-loop primitives exist; the work is
porting the production wiring (signal handling, systemd, PID file,
dual-stack, socket options, motd, proxy-protocol, reverse-DNS) onto the
async path and adding integration tests. Estimated ~800-1200 LoC delta,
much of it deletion in `connection.rs` after the async path lands.

Upstream parity: equivalent to status quo. Upstream's fork-per-connection
runs sync I/O against a single socket; tokio + `spawn_blocking` runs
the same sync I/O on a thread-pool thread, with crash isolation via
`JoinError`.

### Option (c) - Status quo

Keep the synchronous thread-per-connection model. Optionally raise the
ceiling by tightening the accept-loop sleep
(`connection.rs:253`) or replacing it with an
`epoll_wait`-on-eventfd-aware sleep.

Pros:

- No code change. Zero risk of regression on a path with stable,
  well-tested behaviour and 100% interop with upstream.
- Matches upstream's per-session semantics (blocking I/O, crash
  isolation) without translation.
- One OS thread per active session is fine for the realistic deployment
  size (single-digit thousands of concurrent transfers is well within
  modern Linux thread-table limits).

Cons:

- Idle connections cost one full OS thread each (8 MiB default stack
  on glibc, less on musl). For a backup-agent fleet of 5-10k clients,
  this is several tens of GiB of address space committed even though
  most threads are blocked on `read(2)`.
- Signal-flag observation is gated by the 500 ms `SIGNAL_CHECK_INTERVAL`
  (`listener.rs:45`), so a SIGTERM may take up to 500 ms to begin
  draining. Tolerable but visible in shutdown timing.
- The dual-stack path adds an extra hop through a `mpsc::channel`
  (`connection.rs:288`) and per-listener acceptor threads
  (`connection.rs:305`) - two extra threads even before any client
  connects. A multiplexed accept loop would collapse these.

## Findings

### F1. Production daemon spawns one OS thread per accepted connection (MEDIUM severity)

Evidence: `crates/daemon/src/daemon/sections/server_runtime/connection.rs:121`
calls `thread::spawn` in `spawn_connection_worker`, invoked from both
`run_single_listener_loop` (`connection.rs:245`) and `run_dual_stack_loop`
(`connection.rs:346`). Each accepted TCP connection gets its own OS thread
and stays parked on blocking reads in `handle_session`.

Impact: scales linearly in OS threads with concurrent connections. For
high-fanout idle-connection workloads (large backup-agent fleets, monitoring
clients periodically polling for module lists), thread-table and
address-space cost grows with the number of connected clients regardless
of activity level. Upstream rsync has the same property because each
forked child is a full process, but the practical cost is similar.

Recommended path: option (b). Promote the existing `async_session/`
listener so idle connections cost a tokio task (~1 KiB) rather than an
OS thread (~8 MiB).

### F2. Two parallel accept-loop implementations already coexist in the tree (MEDIUM severity)

Evidence: synchronous path at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:216`
(`run_single_listener_loop`) and `connection.rs:281`
(`run_dual_stack_loop`); async path at
`crates/daemon/src/daemon/async_session/listener.rs:180`
(`AsyncDaemonListener::serve`). The async module is gated behind the
`async` feature (`crates/daemon/Cargo.toml:20`) and currently only
exercised in tests (`async_session/mod.rs:28-35`).

Impact: maintenance burden grows with each new daemon feature (signal
handling, systemd integration, socket options) because both paths must
be updated. Adding a third event-loop variant via raw `mio` would
compound this.

Recommended path: option (b) consolidates onto a single accept-loop
shape. Do not introduce a third (option (a)) variant.

### F3. Sync accept loop polls signal flags every 500 ms via a sleep (LOW severity)

Evidence: `crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`
defines `SIGNAL_CHECK_INTERVAL = Duration::from_millis(500)`. The
single-listener loop sleeps for this duration when `accept` returns
`WouldBlock` (`connection.rs:253`). The dual-stack path uses a tighter
50 ms sleep (`connection.rs:316`) and a 100 ms `recv_timeout`
(`connection.rs:342`) to coordinate via `mpsc::channel`.

Impact: SIGTERM, SIGHUP, SIGUSR1, SIGUSR2 observation latency is up to
500 ms in the single-listener path. Worker reaping
(`reap_finished_workers`, `workers.rs:7`) is also pinned to the same
cadence. Not user-visible in normal operation; relevant only for
shutdown timing in tests and CI.

Recommended path: any move to event-loop multiplexing (option (a) or
(b)) eliminates the sleep. If status quo is kept (option (c)),
consider lowering the constant to 100 ms to match the dual-stack path.

### F4. Async listener path is `#[allow(dead_code)]` and has no production wiring (HIGH severity if option (b) is chosen)

Evidence: `crates/daemon/src/daemon/async_session/mod.rs:28`
(`#![allow(dead_code)]` with the comment "async daemon path not yet
wired to production; types used in tests"). The public re-export at
`mod.rs:34-35` is `#[cfg(test)]`-gated. None of the production wiring
performed by `serve_connections` (`accept_loop.rs:11`) - signal
registration (`accept_loop.rs:22`), syslog open
(`accept_loop.rs:71-83`), connection-limiter setup
(`accept_loop.rs:90-93`), modules and motd
(`accept_loop.rs:96-102`), dual-stack bind
(`accept_loop.rs:107-173`), socket options
(`accept_loop.rs:178-206`), become_daemon
(`accept_loop.rs:212-215`), PID file
(`accept_loop.rs:222-227`), drop_privileges
(`accept_loop.rs:233-246`), systemd notifier
(`accept_loop.rs:248-257`) - exists on the async side.

Impact: option (b) is not a one-line swap. Choosing it implies a
multi-PR productionisation effort to port every wiring step listed
above onto the async loop, plus integration tests covering each.

Recommended path: if option (b) proceeds, file follow-ups for each
wiring step and gate the new path behind the `async` feature until
parity is reached, then flip the default.

### F5. Upstream rsync uses `select(2)` over listening fds only, never per-connection multiplexing (LOW severity, informational)

Evidence: `target/interop/upstream-src/rsync-3.4.1/socket.c:533-624`
(`start_accept_loop`). The fd_set is populated with listening sockets
only (`socket.c:548-562`), `select` blocks indefinitely
(`socket.c:584`), and the accepted fd is handed to `fork()`
(`socket.c:599`) for synchronous handling.

Impact: any oc-rsync event-loop multiplexing is an oc-rsync-only
optimisation, not a parity feature. The upstream code base has no
guidance on how to structure a per-connection event loop because it
does not have one.

Recommended path: keep per-connection I/O blocking and isolated, in
both options (a) and (b). Multiplex only at the accept layer plus
signals.

### F6. Dual-stack path adds two threads and an mpsc hop before any client connects (LOW severity)

Evidence: `crates/daemon/src/daemon/sections/server_runtime/connection.rs:281`
(`run_dual_stack_loop`). For each listener it does
`thread::spawn(move || { ... listener.accept() ... tx.send(...) ... })`
at `connection.rs:305`, fanning into a single
`mpsc::channel::<Result<(TcpStream, SocketAddr), ...>>`
(`connection.rs:288`). With the default IPv4 + IPv6 dual-stack bind,
the daemon idles with three threads: the main accept loop, the IPv4
acceptor, and the IPv6 acceptor.

Impact: cheap (each thread is parked in `accept`), but visible in
process listings and consumes three thread-table slots. A multiplexed
accept loop registers both listening fds on the same `epoll`/
`kqueue`/tokio reactor and runs as one thread.

Recommended path: option (a) or option (b) collapses this to one
accept thread/task; status quo accepts the small overhead.

### F7. No runtime knob to opt into the higher-scale path (LOW severity)

Evidence: `RuntimeOptions` (the struct fields destructured at
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:39-60`)
has `max_sessions`, `listen_backlog`, `socket_options`, but no
`event_loop` or `connection_model` setting. The daemon picks the sync
model unconditionally; the async path is reachable only via tests.

Impact: even when option (b) lands, operators cannot pick per
deployment without a config flag. Backup-fleet operators want the
event-loop path; small interactive deployments may prefer the
sync/upstream-equivalent model for behavioural fidelity.

Recommended path: when option (b) is implemented, add an
`event_loop = sync | async` (or equivalent) directive to
`oc-rsyncd.conf`, defaulting to `sync` until the async path is at
parity.

## Recommendation

Pursue option (b) - extend the existing `tokio` `async_session/` listener
into a production-ready accept loop and route the daemon binary through it
behind the existing `async` Cargo feature - but defer the work until a
deployment with thousands of concurrent idle connections actually needs
it. Option (a) (raw `mio`) is rejected because it adds a third accept-loop
implementation alongside paths that already exist; option (c) (status quo)
remains correct and upstream-faithful but does not scale to the
high-fanout-idle-connections case the task brief calls out. The right
sequencing is: (1) finish the async-path productionisation work tracked in
F4 behind the feature flag, (2) add an `event_loop` runtime knob (F7),
(3) benchmark `tokio::spawn_blocking` workers against the sync path on a
representative high-fanout workload, (4) flip the default if the
benchmark validates the win.

## Follow-up tasks

- [ ] #1676 inventory every `serve_connections` setup step in
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
  and design parity coverage on the async path (signal handling, syslog,
  PID file, become_daemon, drop_privileges, systemd notifier,
  socket options, dual-stack bind).
- [ ] #1677 port `SignalFlags` integration to a `tokio::signal::unix`
  + `broadcast` channel on the async path, replacing the 500 ms
  `SIGNAL_CHECK_INTERVAL` poll
  (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`).
- [ ] #1678 promote the `AsyncDaemonListener` re-export
  (`crates/daemon/src/daemon/async_session/mod.rs:34`) out of
  `#[cfg(test)]` once parity is reached and remove the
  `#![allow(dead_code)]` (`async_session/mod.rs:28`).
- [ ] #1679 add an `event_loop` directive to `oc-rsyncd.conf`
  (default `sync`) and route `daemon_main` accordingly. Document the
  trade-offs in `docs/DAEMON_PROCESS_MODEL.md`.
- [ ] #1680 author a `crates/daemon/benches` harness that opens N
  long-lived idle TCP connections and measures the daemon's RSS,
  thread count, and SIGTERM-to-drain latency; run against both the
  sync and async paths.
- [ ] #1681 evaluate a `tokio::task::Builder::max_blocking_threads`
  cap so a flood of active transfers cannot exceed the existing
  `max_sessions` ceiling enforced by the synchronous path
  (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:264`).
- [ ] #1682 cross-platform check on Windows: confirm tokio's IOCP
  reactor returns the same `accept` semantics as Unix epoll/kqueue
  for `socket2`-built listeners
  (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:87`).
- [ ] #1683 if option (b) cannot be funded, lower
  `SIGNAL_CHECK_INTERVAL`
  (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`)
  from 500 ms to 100 ms to match the dual-stack path's cadence.

## References

- Synchronous accept loop:
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
  `connection.rs`, `listener.rs`, `workers.rs`.
- Async accept loop scaffold:
  `crates/daemon/src/daemon/async_session/listener.rs`,
  `session.rs`, `shutdown.rs`, `mod.rs`.
- Daemon Cargo features (`async`, `concurrent-sessions`):
  `crates/daemon/Cargo.toml:16-28`.
- Upstream rsync 3.4.1 daemon:
  `target/interop/upstream-src/rsync-3.4.1/clientserver.c:1275`
  (`start_daemon`), `clientserver.c:1496` (`daemon_main`),
  `socket.c:533` (`start_accept_loop`).
- Process-model rationale: `docs/DAEMON_PROCESS_MODEL.md`.
- `mio` 1.x: <https://docs.rs/mio/1>.
- `tokio` 1.x: <https://docs.rs/tokio/1>.
