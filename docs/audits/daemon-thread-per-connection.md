# Daemon Thread-per-Connection Scalability Limits

Tracking: oc-rsync task #1673.

> Static analysis. No code changes land in this PR. Companion to
> `docs/audits/daemon-thread-per-connection-scalability.md` (per-stage
> cost map), `docs/audits/daemon-async-listener-rfc.md` (task #1934
> RFC), `docs/audits/daemon-event-loop-multiplexing.md` (task #1675),
> and `docs/audits/async-daemon-listener.md`. This document is the
> short-form scaling-limits brief: what the model is today, where it
> breaks, and the migration path through the already-designed hybrid.

## 1. Summary

The oc-rsync daemon spawns one OS thread per accepted TCP connection
and runs the full session lifecycle (greeting, auth, module dispatch,
transfer) synchronously on that thread. Crash isolation is provided
by `std::panic::catch_unwind` around the session handler, replacing
upstream rsync's per-connection `fork(2)`.

The model is correct, simple, and adequate up to roughly 100
concurrent connections. It loses headroom at 1000 (8 GiB of stack
address space reserved, default `RLIMIT_NOFILE = 1024` exhausted) and
is not viable at 10 000 without operator-side tuning that would push
the host past committed memory anyway. The bottleneck is not CPU but
reserved virtual address space and file-descriptor count.

The migration target is the already-designed hybrid: async tokio
accept loop and pre-handshake greeting, sync transfer worker pool for
the bulk-data phase. The async listener crate scaffold
(`crates/daemon/src/daemon/async_session/`) is in tree behind the
`async` Cargo feature; the worker pool design lands in task #1674
and the listener implementation in task #1934 / #1935.

## 2. Current thread-per-connection model

### 2.1 Accept loop

Source of truth:
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
`server_runtime/connection.rs`,
`server_runtime/listener.rs`,
`server_runtime/workers.rs`.

`serve_connections()` binds one or two `std::net::TcpListener`
sockets (IPv4, IPv6, or both for dual-stack) using `socket2` so the
`listen(2)` backlog matches `lp_listen_backlog()` from upstream
`socket.c` (default 5). Each listener is set non-blocking so the
accept loop can poll signal flags every
`SIGNAL_CHECK_INTERVAL = 500 ms`
(`server_runtime/listener.rs:45`). Single-listener mode runs the
accept loop in the main thread; dual-stack mode spawns one acceptor
thread per listener and funnels accepted sockets through an
`mpsc::channel` to the main loop
(`server_runtime/connection.rs:281`).

### 2.2 Per-connection thread spawn

Each accepted socket is handed to `spawn_connection_worker()`
(`server_runtime/connection.rs:106`):

- The peer address is normalised (IPv4-mapped IPv6 collapsed to v4).
- An `Arc` clone of the module list, MOTD lines, and (optionally)
  the shared log sink is captured by the closure.
- A `ConnectionCounter::acquire()` guard is taken so the in-process
  active count drops when the thread exits.
- `thread::spawn(move || ...)` runs the session inside
  `std::panic::catch_unwind(AssertUnwindSafe(|| handle_session(..)))`.
  The comment block at line 124 mirrors upstream semantics: rsync
  forks per connection so a child crash kills only that child; we
  use threads, so `catch_unwind` is the equivalent firewall.
- The returned `JoinHandle<WorkerResult>` is pushed into
  `state.workers: Vec<JoinHandle<WorkerResult>>` and reaped by
  `reap_finished_workers()`
  (`server_runtime/workers.rs:7`) on every iteration of the accept
  loop.

There is no thread pool. There is no async runtime on the production
path. There is no shared executor. Every connection gets its own
brand-new OS thread with a fresh stack reservation, fresh TLS area,
and its own kernel `task_struct`.

### 2.3 Session body

The spawned worker calls `handle_session()` which runs:

1. `configure_stream()` sets a 10 s `set_read_timeout` /
   `set_write_timeout` on the accepted socket
   (`SOCKET_TIMEOUT` in `daemon.rs:105`).
2. Optional `proxy_protocol` v1/v2 header parse.
3. `@RSYNCD:` greeting + capability negotiation.
4. Module selection, hostname/IP allow-list, secrets-file auth.
5. `core::server::run_server_with_handshake()` for the transfer
   body, which spins up the rsync sender/receiver/generator state
   machines on the same thread.

Bandwidth limiting, filter parsing, MD4/MD5 hashing, mmap reads,
delta computation, and disk writes all execute on this one thread.
Rayon and io_uring are used internally by lower crates but the
session control thread itself never moves. The thread is alive for
the entire connection.

### 2.4 Crash isolation and graceful shutdown

`catch_unwind` swallows panics, logs a description, and lets the
thread exit cleanly so the daemon stays up. SIGTERM / SIGINT set
`signal_flags.shutdown`; the accept loop breaks and `drain_workers()`
joins remaining `JoinHandle`s before the function returns. SIGUSR1
sets `signal_flags.graceful_exit` and stops accepting new connections
while existing transfers drain.

### 2.5 Admission control already in tree

The daemon currently has three throttles:

- `max connections` per module - enforced via `ConnectionLimiter`
  (file-locked counter file).
- `max_sessions` runtime option - terminates the accept loop after
  N total connections have been served (single-shot, not concurrent).
- `ConnectionCounter` - in-process active-thread count, used for
  systemd status messages but not for back-pressure.

There is no daemon-level concurrent-connection cap that refuses an
accept once N threads are alive. This is the lowest-risk
short-term mitigation and is recommended below.

## 3. Scaling limits

### 3.1 Per-thread cost

The dominant per-thread cost is reserved virtual address space, not
RSS. On glibc Linux x86_64 the default `pthread` stack is 8 MiB
(`ulimit -s`); musl defaults to 128 KiB; macOS pthread default is
512 KiB; Windows main-thread default is 1 MiB. Rust inherits the
platform pthread default unless `Builder::stack_size` overrides it,
and `std::thread::spawn` does not.

Address space reserved per thread (default config):

| Platform | Stack reservation |
|----------|-------------------|
| Linux glibc x86_64 | 8 MiB |
| Linux musl | 128 KiB |
| Linux glibc aarch64 | 8 MiB |
| macOS | 512 KiB |
| Windows | 1 MiB |

Heap state per session is small: one `TcpStream`, two `BufReader` /
`BufWriter` (default 8 KiB each), the captured `Arc` clones, the
session state machine, and on-stack delta buffers. Empirically this
adds up to under 100 KiB before the transfer body starts allocating
real buffers; once delta scheduling kicks in, peak per-session heap
sits in the low single-digit MiB range, dominated by the basis
mmap and the rolling-checksum table.

Kernel state per thread: one `task_struct` (~10 KiB on Linux), one
fd for the socket, one for the multiplex pipe if used, plus
file-table entries for any open transfer files.

### 3.2 100 concurrent connections

Comfortable. ~800 MiB stack reservation on glibc Linux, well under
default `RLIMIT_NOFILE = 1024`. Total kernel `task_struct` overhead
is ~1 MiB. Context switches stay in the noise on any multi-core
host. The bottleneck at this scale is whatever the transfer body
hits: disk bandwidth, network link, MD5 compute. Thread-per-connection
adds no measurable tax.

### 3.3 1000 concurrent connections

Loses the default-config envelope:

- Stack reservation: 8 GiB on glibc Linux. `MAP_NORESERVE` is not
  the default for thread stacks under glibc, so this is committed
  virtual memory, not just address space. Real RSS stays under
  1 GiB because most stack pages are never touched, but `vsize`
  and OOM-killer scoring see the full 8 GiB.
- File descriptors: 1000 listening sockets plus per-session
  transfer fds blow past `RLIMIT_NOFILE = 1024`. Operator must
  raise the limit (`LimitNOFILE=` in the systemd unit, or
  `ulimit -n`) before this load is feasible.
- `pid_max` and `threads-max`: Linux defaults are 32768 and around
  half of physical RAM divided by 2 KiB, so 1000 threads is fine
  numerically. Mac OS default `kern.num_taskthreads` is 4096.
- Context-switch cost: starts to show on small-core hosts when many
  sessions are simultaneously active. Each blocked I/O wakes a
  full thread including TLB and cache pollution.
- Lock contention: documented in section 7 of the companion
  scalability audit. The hot lock is `Mutex<MessageSink<File>>`
  for log writes; at 1000 sessions logging serialises all of them.

The model still works at this scale on a tuned host (raised
`RLIMIT_NOFILE`, fast disk for log fsync, 32+ GiB RAM) but the
operator is doing real configuration work to keep it stable.

### 3.4 10 000 concurrent connections

Not viable on the production path:

- Stack reservation: 80 GiB on glibc Linux. Even with overcommit,
  the daemon will be the largest VSZ on the host and the OOM killer
  treats it as the obvious victim. musl's 128 KiB default brings
  this down to 1.25 GiB but the daemon does not pin musl.
- File descriptors: requires `RLIMIT_NOFILE` raised to ~25 000
  (1 listener fd per session plus 1-2 transfer fds). Linux supports
  this; the operator pays the syscall overhead of `epoll_create` /
  `select` not scaling, but the daemon does not currently use
  either, so the cost is in `accept(2)` itself which scales fine.
- Thread creation and teardown rate: `pthread_create` + `mmap` for
  the stack adds ~30 microseconds per accept on Linux. At sustained
  10 000 connections/s churn this is 300 ms/s of pure spawn
  overhead, which competes with the accept loop itself.
- Scheduler pressure: 10 000 runnable kernel tasks pushes the
  Linux CFS run-queue beyond its design point on smaller core
  counts. Not a hard wall but a real CPU tax.
- The `Vec<JoinHandle<...>>` worker list becomes a linear scan
  every accept iteration (`reap_finished_workers` walks the vec).
  At 10 000 entries this is a non-trivial syscall-free pass but
  still O(N) per accept.

The async migration is a pre-condition for this scale, not an
optimisation.

### 3.5 Syscall surface per accepted connection

| Stage | Syscalls |
|-------|----------|
| Accept | `accept4` (1) |
| Setup | `setsockopt` x N socket options, `setsockopt(SO_RCVTIMEO/SO_SNDTIMEO)` (2) |
| Spawn | `clone` (1), `mmap` for stack (1), `mprotect` for guard page (1) |
| Greeting | `read`, `write` per line (~4-6 round trips) |
| Auth | optional `read`, `write`, `open(secrets)`, `close` (~4) |
| Transfer | dominated by file body; not a thread-per-conn cost |
| Teardown | `close` (1), `munmap` for stack (1), `exit` (1) |

The accept-to-greeting startup cost is ~10 syscalls + one
`pthread_create`. None of these dominate; the 8 MiB stack
reservation does.

## 4. Async tokio listener (#1934 RFC, #1675 multiplex audit)

The async listener path is documented in
`docs/audits/daemon-async-listener-rfc.md`. Headlines:

- A tokio accept loop replaces `TcpListener::accept()` in the main
  thread. Tokio's `TcpListener` uses `epoll` on Linux, `kqueue` on
  the BSDs and macOS, and IOCP on Windows; oc-rsync gets all three
  for free.
- Per-connection cost on the async path is one tokio task (~512 B
  + the future state machine) instead of an OS thread (8 MiB stack
  reservation on glibc).
- The greeting and auth phases are short, low-CPU, mostly waiting
  on I/O - exactly the workload tokio is good at.
- Cancellation, timeout, and graceful shutdown become cheap
  (`tokio::select!` over a shutdown broadcast channel).
- Existing scaffold lives in `crates/daemon/src/daemon/async_session/`
  behind the `async` Cargo feature. Currently `#[allow(dead_code)]`
  and not wired into `serve_connections()`.

`docs/audits/daemon-event-loop-multiplexing.md` (task #1675)
evaluated the lower-overhead alternative of raw `epoll` / `kqueue`
via `mio` 1.x, sync workers per accepted fd. Conclusion: the
runtime saving over tokio is small, the maintenance cost is
substantial (three platform implementations, no `async fn`
ergonomics, hand-rolled timer wheel). The audit recommends tokio
unless an explicit benchmark in task #1933 shows tokio overhead
exceeding 5 % of the per-connection budget.

## 5. Hybrid model: async accept + sync transfer worker pool (#1674 design)

The recommended end state is not "async everywhere". It is async
for the parts that are I/O bound and short, sync for the parts that
are CPU bound and long. This matches upstream rsync's actual
workload: a transfer body is a CPU-and-disk grind, not an
event-driven workload.

### 5.1 Shape

- Tokio runtime owns the accept loop, the `@RSYNCD:` greeting,
  capability negotiation, module selection, host allow-list, and
  the optional auth round-trip. All of these are pure I/O with
  short message sizes and idle-waiting patterns.
- Once the session is admitted (post-auth, pre-transfer), the
  socket is handed off to a sync worker thread from a fixed-size
  pool that runs the transfer body. The hand-off uses
  `tokio::task::spawn_blocking` initially; longer term it migrates
  to a dedicated `crossbeam_channel` work queue with a
  size-bounded worker pool.
- The worker pool size is configurable, defaults to
  `num_cpus * 2`, and provides a hard concurrency cap. New
  connections beyond the cap wait in the tokio queue (back-pressure
  is automatic) or are refused with `@ERROR: max connections`
  consistent with upstream behaviour.

### 5.2 Why hybrid wins

- The expensive thread cost (8 MiB stack reservation) is paid only
  for sessions that have already passed admission and are doing
  real transfer work. Idle / authenticating connections cost a
  tokio task.
- The transfer body still has direct, blocking syscalls (`read`,
  `write`, `splice`, `copy_file_range`, io_uring submit/wait
  inside `fast_io`). Forcing it through an async runtime would
  require either `spawn_blocking` (which puts it on a thread pool
  anyway) or rewriting the engine, which is out of scope.
- The worker-pool cap doubles as the daemon-level concurrent
  session cap that is currently missing. No separate admission
  control plumbing needed.
- Rayon parallelism inside the transfer (used by `core::server`,
  `engine::generator`, `signature`) keeps working without
  modification because each transfer still owns a real OS thread.

### 5.3 Risks

- A second runtime appears in the daemon process (tokio for accept,
  rayon for transfer compute). Upstream `core::client` SSH path
  may also use tokio in future. The existing tokio-scope policy
  (companion audit `tokio-dependency-boundary-2026.md`, task #1779
  / #1818) confines tokio to clearly marked Cargo features and a
  single runtime per binary.
- `spawn_blocking` has a default cap of 512 blocking tasks. The
  worker pool must be a dedicated `crossbeam` queue, not
  `spawn_blocking`, once concurrent transfers exceed roughly 100.
- Graceful shutdown semantics need to flow through both halves:
  the tokio side via a broadcast `Notify`, the sync side via a
  shared `AtomicBool` checked at I/O boundaries.

## 6. Recommendation and migration path

### Phase 0 (this PR, no code change)

- Land this audit and the companion scalability audit. No
  behaviour change.

### Phase 1 (short-term, sync model retained)

- Add a daemon-level `max_concurrent_sessions` admission cap,
  refusing accepts when the active worker count meets the cap.
  Reuses `ConnectionCounter`. Single-line guard in the accept
  loop. Operator-tunable via `oc-rsyncd.conf`. Does not require
  any tokio code.
- Replace the linear `Vec<JoinHandle>` reaper with a finished-flag
  scan that drops handles on exit rather than walking the vec on
  every accept. Reduces O(N) reap to O(finished).
- Document the recommended `RLIMIT_NOFILE` and stack-size
  operator tunings for the 100 / 1000 / 10 000 tiers in the
  daemon man page.
- Run the empirical benchmark scoped under task #1933 against the
  current sync model so the numbers in section 3 are validated
  rather than estimated.

### Phase 2 (medium-term, behind feature flag)

- Implement task #1934 / #1935: tokio accept loop and async
  greeting / auth, behind the existing `async` Cargo feature.
  Default-off. Same wire behaviour, same logging, same exit
  codes; only the listener / admission half changes.
- Add the sync transfer worker pool from task #1674 as the
  hand-off target. Pool size configurable; default
  `num_cpus * 2`.
- Re-run the task #1933 benchmark with `--features async-daemon`
  and compare. Acceptance: at 1000 concurrent connections,
  resident set under 2 GiB and `RLIMIT_NOFILE` ceiling pushed
  out by 4x or more.

### Phase 3 (long-term, default-on)

- Make `async-daemon` the default once the benchmark and a
  90-day soak test on the public daemon endpoint show no
  regression on connection establishment, transfer throughput,
  or memory steady state.
- Keep the sync path under a Cargo feature for at least one
  major release as a fallback for environments where tokio
  cannot be linked (small embedded targets, audited builds).

The end state retains 100 % wire compatibility with upstream
rsync 3.4.1 (the entire change is below the protocol layer) and
only changes the listener and admission half of the daemon. The
transfer body, the protocol, the engine, and every behavioural
test stay byte-equivalent.

## 7. References

- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs` -
  current accept loop entry point.
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs` -
  `spawn_connection_worker`, single-listener and dual-stack loops.
- `crates/daemon/src/daemon/sections/server_runtime/workers.rs` -
  `reap_finished_workers`, `drain_workers`, `join_worker`.
- `crates/daemon/src/daemon/sections/server_runtime/listener.rs` -
  `bind_with_backlog`, `configure_stream`, signal-poll interval.
- `crates/daemon/src/daemon/async_session/` - in-tree tokio
  listener scaffold, behind `async` feature.
- `crates/daemon/src/daemon/connection_pool/` - per-IP rate
  limiting and active-connection tracking via `DashMap`.
- `docs/audits/daemon-thread-per-connection-scalability.md` -
  per-stage cost map and lock-contention surface.
- `docs/audits/daemon-async-listener-rfc.md` - task #1934 RFC.
- `docs/audits/daemon-event-loop-multiplexing.md` - task #1675
  epoll/kqueue evaluation.
- `docs/audits/async-daemon-listener.md` - earlier sketch.
- `docs/audits/tokio-dependency-boundary-2026.md` - tokio scope
  policy.
- Upstream `clientserver.c:daemon_main()` - upstream fork-per-
  connection model that the thread-per-connection design mirrors
  semantically.
