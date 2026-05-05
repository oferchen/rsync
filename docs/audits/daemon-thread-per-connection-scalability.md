# Daemon Thread-per-Connection Scalability Audit

Tracking: oc-rsync task #1673.

> Static analysis. No code lands in this PR. The empirical follow-up is
> task #1933 (benchmark at 100, 1000, 10000 concurrent connections).

## 1. Summary

The oc-rsync daemon serves every accepted TCP connection on its own OS
thread. Each thread runs the full session lifecycle - `@RSYNCD:`
greeting, capability advertisement, module-select, optional auth, then
the entire transfer body - synchronously. This audit measures the
ceiling that model imposes on concurrent connections, identifies the
hard and soft limits operators hit first, and frames the migration
sequencing against the already-RFC'd async listener (task #1934).

The headline result: per-connection cost is dominated by reserved
thread-stack address space (8 MiB by glibc default) and not by heap or
kernel state. At 100 concurrent connections the sync model is
comfortable. At 1000 the daemon is approaching the default fd ulimit
(`RLIMIT_NOFILE = 1024`) and consuming roughly 8 GiB of address space
in stack reservations alone. At 10000 the daemon is over committed
even on a 16 GiB box and the file-descriptor limit must be raised
explicitly.

The migration order has already been settled by tasks #1934 (RFC) and
#1935 (implementation). This audit's contribution is the per-stage
cost map, the lock-contention surface, the upstream-comparison table,
and a phased recommendation that the daemon enforce a daemon-level
`max_sessions` admission cap immediately (short-term, no protocol
change), then ship the async listener behind `--features
async-daemon` (medium / long-term).

Last verified: 2026-05-05 against
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
`crates/daemon/src/daemon/sections/server_runtime/connection.rs`,
`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs`,
`crates/daemon/src/daemon/sections/server_runtime/listener.rs`,
`crates/daemon/src/daemon/sections/server_runtime/workers.rs`,
`crates/daemon/src/daemon/sections/server_runtime/reload.rs`,
`crates/daemon/src/daemon/sections/signals.rs`,
`crates/daemon/src/daemon/connection_pool/pool.rs`,
`crates/daemon/src/daemon/connection_pool/types.rs`,
`crates/daemon/src/daemon/async_session/listener.rs`,
`crates/daemon/src/daemon/async_session/mod.rs`,
`crates/daemon/src/daemon/module_state/mod.rs`,
`crates/daemon/src/daemon/module_state/runtime.rs`,
`crates/daemon/src/daemon/module_state/connection_limiter.rs`,
`crates/daemon/src/daemon/sections/module_access/request.rs`,
`crates/daemon/src/daemon/sections/module_access/transfer.rs`,
`crates/daemon/src/daemon/sections/module_access/authentication.rs`,
`crates/daemon/src/daemon/sections/greeting.rs`,
and `crates/daemon/src/daemon/runtime_options/types.rs`.

## 2. Methodology

This is a static analysis. Files are read once and cited at file:LINE.
No daemon was started; no benchmark numbers are produced here. The
intent is to characterise the model so the benchmark in #1933 has a
shape to measure against.

In-scope source:

- The decomposed accept loop after #1354:
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
  (entry point), `connection.rs` (per-connection lifecycle),
  `listener.rs` (bind helpers and timing constants),
  `workers.rs` (join / drain), `connection_counter.rs` (in-process
  active-thread counter), `reload.rs` (SIGHUP).
- The decomposed module state after #1253:
  `crates/daemon/src/daemon/module_state/mod.rs`,
  `module_state/definition.rs`, `module_state/runtime.rs`,
  `module_state/connection_limiter.rs`,
  `module_state/auth.rs`,
  `module_state/hostname.rs`.
- The async listener scaffolding after #1357:
  `crates/daemon/src/daemon/async_session/mod.rs`,
  `async_session/listener.rs`,
  `async_session/session.rs`,
  `async_session/shutdown.rs`.
- The opt-in tracking pool:
  `crates/daemon/src/daemon/connection_pool/mod.rs`,
  `connection_pool/pool.rs`,
  `connection_pool/types.rs`.

Out-of-scope: the transfer body itself (sender, receiver, generator,
`core::session`), the wire protocol layer
(`crates/protocol`), `crates/engine` and `crates/transfer`. Those
crates appear here only as cost factors observed from inside a
session thread.

## 3. Connection lifecycle map

A single connection traverses six stages. Each stage runs on the
worker thread spawned at stage 1; no cross-stage hand-off changes
threads.

### 3.1 Accept

`serve_connections` is the daemon entry point
(`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`).
It picks one of two accept-loop shapes based on listener count
(`accept_loop.rs:288-294`):

- Single listener: `run_single_listener_loop`
  (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:216`)
  sets the listener non-blocking (`connection.rs:222`), then loops
  on `listener.accept()` (`connection.rs:230`). When `accept`
  returns `WouldBlock`, the loop sleeps `SIGNAL_CHECK_INTERVAL =
  500 ms`
  (`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`,
  invoked at `connection.rs:253`).
- Dual-stack: `run_dual_stack_loop` (`connection.rs:281`) spawns one
  acceptor thread per listener (`connection.rs:305`); each acceptor
  polls its non-blocking listener with a 50 ms sleep on `WouldBlock`
  (`connection.rs:316`) and forwards accepted streams through an
  `mpsc::channel` (`connection.rs:288`); the main loop calls
  `rx.recv_timeout(Duration::from_millis(100))`
  (`connection.rs:342`).

The default IPv4 + IPv6 dual-stack bind is selected at
`accept_loop.rs:107-120` when no explicit address is configured,
producing two listening sockets and two acceptor threads at idle.

### 3.2 Thread spawn

`spawn_connection_worker` (`connection.rs:106`) is the single thread
spawn site. It is invoked from both
`run_single_listener_loop` (`connection.rs:245`) and
`run_dual_stack_loop` (`connection.rs:346`). The body
`thread::spawn(move || ...)` lives at `connection.rs:121`. Inside
the new thread, `std::panic::catch_unwind` wraps the session body
(`connection.rs:127`) so a panic kills only the connection - the
documented thread-equivalent of upstream's per-connection fork
(`accept_loop.rs:1-10`).

State cloned into the worker (`connection.rs:112-119`):
`Arc<Vec<ModuleRuntime>>`, `Arc<Vec<String>>` (motd lines),
`Option<Arc<SharedLogSink>>`, `Arc<Vec<SocketOption>>`, the
bandwidth limit `Option<NonZeroU64>`, the bandwidth burst
`Option<NonZeroU64>`, two `bool` flags, plus a
`ConnectionGuard` from `ConnectionCounter::acquire`
(`connection.rs:115`,
`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:38`).
The guard is RAII: drop decrements the in-process active-thread
count atomically (`connection_counter.rs:71-73`).

### 3.3 Greeting / handshake

The session body called from `spawn_connection_worker` runs the
`@RSYNCD:` greeting line. The greeting is formatted by
`legacy_daemon_greeting`
(`crates/daemon/src/daemon/sections/greeting.rs:42`), which
appends a digest list keyed off the protocol version
(`greeting.rs:13-35`). A blocking `read_line` reads the client's
counter-greeting via `read_trimmed_line` (`greeting.rs:49`) on a
plain `BufRead` of the `TcpStream`. There is no timeout other than
the global `SOCKET_TIMEOUT` applied to accepted streams in
`configure_stream`
(`crates/daemon/src/daemon/sections/server_runtime/listener.rs:113-116`).

### 3.4 Auth

For modules where `requires_authentication()` is true, the worker
runs `perform_module_authentication`
(`crates/daemon/src/daemon/sections/module_access/authentication.rs:33`):
it generates a challenge (`authentication.rs:41`), writes it to
the stream via the `LegacyMessageCache`
(`authentication.rs:43-51`), reads the client's response with
`read_trimmed_line` (`authentication.rs:54`), and verifies the
digest with `verify_secret_response` (`authentication.rs:80`).
All I/O is blocking on the worker thread.

### 3.5 Module dispatch

`handle_authentication` (`module_access/request.rs:140`) is called
from the request entry point. On success it sends `@RSYNCD: OK`
via `send_daemon_ok` (`request.rs:84-90`). The module-connection
slot is then acquired in
`module_access/transfer.rs:213` via
`ModuleRuntime::try_acquire_connection`
(`crates/daemon/src/daemon/module_state/runtime.rs:55`). When
`max_connections` is set on the module, this performs an atomic
`compare_exchange` against the per-module `AtomicU32` counter
(`runtime.rs:77-95`). If a daemon-wide lock file is configured,
`ConnectionLimiter::acquire`
(`crates/daemon/src/daemon/module_state/connection_limiter.rs:67`)
takes an exclusive `flock(2)` on the lock file
(`connection_limiter.rs:73`), reads / increments / writes the
per-module count
(`connection_limiter.rs:99-114`), then drops the lock.
On failure, `handle_max_connections_exceeded`
(`module_access/request.rs:95`) sends an `@ERROR` and closes.

### 3.6 Transfer

`setup_transfer_streams` (`module_access/transfer.rs:48`) sets
`TCP_NODELAY` and clones the `TcpStream` for separate read and
write halves (`transfer.rs:52-70`). The session then runs the
synchronous transfer pipeline (file-list, generator, sender or
receiver), which lives in `crates/transfer`, `crates/engine`,
`crates/protocol`, etc. Every byte flows through the worker
thread; nothing offloads to a different thread except CPU work
that internally dispatches via rayon (see Section 6.4).

### 3.7 Close

The worker returns from `handle_session`. The `ConnectionGuard`
drops, decrementing the daemon-wide counter
(`connection_counter.rs:71-73`). The `ModuleConnectionGuard`
drops, releasing the module slot
(`module_state/runtime.rs:146-153`); the `ConnectionLockGuard`
drops separately, taking another `flock(2)` round-trip to
decrement the lock-file count
(`connection_limiter.rs:168-172`). The thread exits, its
`JoinHandle` becomes `is_finished == true`, and on the next
accept iteration `reap_finished_workers`
(`crates/daemon/src/daemon/sections/server_runtime/workers.rs:7`)
joins it - so the `Vec<JoinHandle>` in `AcceptLoopState`
(`connection.rs:7`) does not grow without bound. On daemon
shutdown, `drain_workers` (`workers.rs:23`) joins everything still
running (`accept_loop.rs:296`).

## 4. Per-connection resource cost

The static cost of one connection, before any user data flows:

| Resource | Per-connection cost | Reference |
|----------|---------------------|-----------|
| OS thread stack (reserved) | 8 MiB on glibc Linux, 256 KiB - 2 MiB on musl, 1 MiB on macOS, 1 MiB on Windows | `pthread_attr_getstacksize` defaults; see `man 7 pthreads` |
| OS thread stack (committed RSS) | typically 8-32 KiB until the session does work | demand-paged on Linux |
| Kernel `task_struct` | ~6-8 KiB | Linux scheduling structures, not visible to userspace |
| TCP socket descriptor | 1 fd | one accepted socket per connection |
| TCP send/receive buffers | Configurable, ~80-128 KiB at default `net.ipv4.tcp_{r,w}mem` | kernel-side, not in daemon RSS but counts against `wmem_max` |
| `JoinHandle<WorkerResult>` | ~64 bytes plus the `Box<dyn Any>` payload | Vec entry on accept thread (`connection.rs:7`) |
| `Arc` clones into worker | 5 strong references (modules, motd, log sink, socket opts, conn guard) at `connection.rs:112-119` | each `Arc::clone` is one atomic increment |
| `BufReader<TcpStream>` | 8 KiB default buffer | one per session; created in the session body |
| Per-module `AtomicU32` increment | 4 bytes (shared, not per-connection) | `module_state/runtime.rs:14` |
| Lock-file round-trip | One `flock(2)` + read + write + `flock(2)` on enter; same on exit | `connection_limiter.rs:67-91` (only when `lock_file` is configured) |

Estimates at the three scaling waypoints from #1933:

| Concurrent connections | Stack RSV (glibc) | Stack RSS estimate | fds | `JoinHandle` heap | Notes |
|------------------------|-------------------|--------------------|-----|-------------------|-------|
| 100 | 800 MiB | 1-3 MiB | 100 + 4 (listeners, signal pipe) | ~6 KiB | Comfortable on any modern host. Default fd ulimit unaffected. |
| 1 000 | 8 GiB | 8-32 MiB | 1 000 + 4 | ~64 KiB | Address-space pressure on 32-bit systems (n/a here, but visible in `/proc/$pid/status` `VmSize`). At Debian's default `ulimit -n = 1024` this is the ceiling without a `prlimit` tweak. |
| 10 000 | 80 GiB | 80-320 MiB | 10 000 + 4 | ~640 KiB | Address-space cost dwarfs any other budget. Past the default kernel `ulimit -u` for non-root users on most distros. Typical 16 GiB host commits ~5x its physical RAM. |

The committed-RSS column matters more than the reservation column for
operations: an idle blocked thread costs only its committed pages,
which is ~8-32 KiB on Linux for a thread that never touches the upper
parts of its stack. The reservation is virtual address space, not
physical memory. But the address space cap is real (`/proc/sys/vm/`
`max_map_count` and per-process `RLIMIT_AS`) and 80 GiB of `VmSize`
is enough to refuse `mmap`s elsewhere in the process - notably the
`crates/fast_io` buffer pool's hugepage path. Numbers are the upper
bound; #1933 will measure where committed RSS actually lands.

## 5. Hard limits

These are kernel-enforced ceilings the daemon hits before its own
data structures become the bottleneck.

### 5.1 File descriptor limit

The default `RLIMIT_NOFILE` on Debian / Ubuntu is 1024, on Fedora /
RHEL 4096, on macOS 256 (raised via `launchctl limit` to 10240),
on Windows uncapped at the Win32 layer. systemd units default to
the value in `/etc/systemd/system.conf` `DefaultLimitNOFILE`,
typically 1024 - 524288 depending on distro. Per accepted
connection the daemon consumes one fd; per listening socket it
consumes one fd; the systemd notifier opens one Unix datagram fd
on first use (`crates/daemon/src/systemd.rs`). At 1k connections
the daemon is two fds shy of the Debian default; at 10k
connections it cannot run without a `LimitNOFILE=` override in the
unit file or a `prlimit` invocation.

The async path inherits the same fd budget (one fd per connection
remains the dominant cost) but frees the per-thread stack
reservation, so the relative bottleneck shifts from stack to fds.

### 5.2 Thread count limit

Linux enforces `RLIMIT_NPROC` (per-user thread + process count).
On a 4 GiB / 16-core box `kernel.threads-max` is typically
~30 000-60 000 system-wide; per-user the default is half that.
Each thread allocates a `task_struct` (~6 KiB pinned in kernel
memory) plus its stack; at 10 000 threads the kernel commits
~64 MiB of pinned task memory before any userspace cost. macOS
caps total threads at `kern.num_threads` (typically 10 240 on
older releases, raised on Apple Silicon). Windows is effectively
unbounded but pays for each thread via the `KTHREAD` and stack
reservation.

The async listener (#1934, #1935) replaces the per-thread reservation
with a per-task heap allocation on the order of 120 bytes plus
held `Arc` clones, lifting the practical thread ceiling out of
the way (Section 7 quantifies this).

### 5.3 Memory ceiling

A naive 8 GiB host that holds 1 000 daemon connections will reserve
8 GiB of stack address space alone. The committed RSS is much
smaller (Section 4) so swap is not engaged for an idle workload,
but `VmSize` reaches the host's RAM ceiling and triggers
`madvise(MADV_DONTNEED)` warnings on Linux when the
`fast_io` buffer pool tries to grow. At 10 000 connections on the
same host the daemon's `VmSize` is over 10x the physical RAM,
which the kernel tolerates (overcommit is on by default) but
which makes the OOM-killer's heuristic unstable: the daemon is
the largest target by `VmSize` even though its actual working
set is small.

## 6. Soft limits

These are the project-internal constraints that bite before the OS
does.

### 6.1 `max_sessions` runtime option

`RuntimeOptions::max_sessions`
(`crates/daemon/src/daemon/runtime_options/types.rs:6`) is
parsed as `Option<NonZeroUsize>` and propagated through
`AcceptLoopState::max_sessions`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:12`).
It is enforced post-accept: `run_single_listener_loop`
checks `state.served >= limit` after a successful accept
(`connection.rs:264-270`); `run_dual_stack_loop` does the same
after pushing a worker (`connection.rs:352-358`). The check
counts total served (`state.served`,
`connection.rs:8`), not currently active. This means
`max_sessions = 100` is a lifetime cap on the daemon, not a
concurrent cap. There is no daemon-level concurrent-connection
admission gate today.

That is the gap the short-term recommendation (Section 8) closes:
swap the served-count check for an active-thread check using
`ConnectionCounter::active`
(`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:33`),
which is already plumbed but
`#[allow(dead_code)]` (`connection_counter.rs:32`).

### 6.2 Per-module `max_connections`

`ModuleDefinition::max_connections`
(`crates/daemon/src/daemon/rsyncd_config/sections.rs:149`) is
enforced at module-acquire time
(`module_state/runtime.rs:55-95`). Combined with the optional
lock file (`connection_limiter.rs`), this works across daemon
processes for fork-style deployments. It is per-module, not
daemon-wide, so a daemon serving 50 modules with no per-module
limit can still saturate the host.

### 6.3 `listen(2)` backlog

The accept loop binds with an explicit backlog
(`accept_loop.rs:138`). Default is 5 - the upstream rsync default,
matching `lp_listen_backlog()` at upstream `socket.c:533`. The
operator can raise it via the `listen_backlog` directive
(`crates/daemon/src/daemon/runtime_options/types.rs:42`). With
backlog 5 and an arrival rate that exceeds the per-accept latency
(thread spawn + `Arc` cloning + greeting write), the kernel will
drop SYNs once the queue fills. This is a soft ceiling on
*arrival rate*, distinct from concurrent connections.

### 6.4 Rayon pool interaction

`crates/transfer`, `crates/engine`, `crates/flist`, `crates/fast_io`
dispatch parallel work to `rayon`'s global thread pool. Rayon's
global pool is sized at first use to `num_cpus::get()` unless
overridden by `RAYON_NUM_THREADS`. Multiple daemon connections
that simultaneously enter a rayon-parallelised stage (parallel
stat in `crates/flist/src/parallel.rs`, parallel delta in
`crates/transfer/src/delta_pipeline.rs:324`, parallel buffer pool
shards in `crates/engine/src/local_copy/buffer_pool/`) all share
that one global pool. With N daemon worker threads and a pool of
P rayon workers, each daemon thread that calls `par_iter()`
queues work onto the same P workers. At 1 000 daemon threads on
a 16-core box (`P = 16`) the rayon queue fills with cross-session
work and the per-session parallel speed-up collapses; the rayon
thread count does not grow. This is by design - rayon assumes
CPU-parallel work, not per-request fan-out.

The proposed adaptive sizer
(`docs/design/adaptive-thread-pool-sizing.md`) and the explicit
fixed pool work tracked in #1683 do not change this analysis:
the rayon pool stays a single shared resource and oversubscription
shows as queue depth rather than thread count growth. The async
listener's `spawn_blocking` pool is independent of rayon (it
defaults to 512 threads, configurable via
`Builder::max_blocking_threads`); see #1934 Open question 5 and
Section 8.3 here.

`RAYON_NUM_THREADS` documentation:
<https://docs.rs/rayon/1/rayon/index.html#using-the-rayon-thread-pool>.

### 6.5 Signal-flag polling latency

`SIGNAL_CHECK_INTERVAL = 500 ms`
(`crates/daemon/src/daemon/sections/server_runtime/listener.rs:45`)
is the worst-case latency between a SIGTERM / SIGUSR1 / SIGHUP /
SIGUSR2 arrival and the accept loop noticing it
(`connection.rs:251-253`). The dual-stack acceptor threads use
50 ms (`connection.rs:316`) and the main dual-stack loop uses
100 ms (`connection.rs:342`). At 1 000 active sessions on a
healthy daemon this is invisible; at 10 000 it adds up to a
500 ms drain delay between the operator sending SIGTERM and
`drain_workers` (`workers.rs:23`) starting. Tracker #1683
(under #1675) addresses this if the async migration is deferred.

## 7. Lock contention surface

Every shared mutable structure visible from a session thread is a
candidate bottleneck at high N.

### 7.1 `ModuleRuntime::active_connections`

Per-module `AtomicU32`
(`crates/daemon/src/daemon/module_state/runtime.rs:14`).
`try_acquire_connection` (`runtime.rs:55`) executes a CAS loop
(`runtime.rs:79-95`) to atomically check-and-increment. Per
session this is one CAS on entry plus one `fetch_sub` on exit
(`runtime.rs:99-102`). At 10 000 sessions concurrently entering
the same module the contention shows as failed CAS retries -
linear in concurrent acquirers, but each retry is one cache-line
bounce, on the order of tens of nanoseconds. Negligible.

### 7.2 `ConnectionLimiter` lock file

`flock(2)` on the configured `lock_file`
(`crates/daemon/src/daemon/module_state/connection_limiter.rs:73`).
Each entry takes an exclusive lock, reads the file, increments
the line for the module, writes back, releases the lock. Each
exit takes the same round-trip. Both happen on the worker thread
in the synchronous path. At 100+ concurrent connection
attempts the lock becomes serialised: one acquirer at a time.
Median time per round-trip on a tmpfs lock file is typically
~50-200 us; on a mechanical disk it can climb to milliseconds.
This is the dominant per-session userspace lock when a lock
file is configured.

### 7.3 `SharedLogSink`

`Arc<Mutex<MessageSink<File>>>`
(`crates/daemon/src/daemon.rs:140`,
`crates/daemon/src/daemon/sections/privilege.rs:89`,
`crates/daemon/src/daemon/sections/module_access/helpers.rs:63`).
Every log line goes through `log_message`, which acquires the
mutex for the duration of one formatted write. Default daemon
logging is sparse (one line per connection accept, one per
auth event, one per connection close). Under verbose logging
or a high error rate the mutex serialises across all session
threads. At 10 000 sessions hitting the log on a connection
storm the `Mutex` becomes the visible hot spot. Mitigations:
async sink (out of scope), per-session ring buffers, or moving
to syslog where the kernel arbitrates
(`logging-sink::syslog`, used at `accept_loop.rs:71-83`).

### 7.4 `ConnectionPool` (DashMap)

`crates/daemon/src/daemon/connection_pool/pool.rs` uses
`DashMap` for the connection table and the per-IP stats table
(`pool.rs:50-54`). DashMap shards internally so concurrent
inserts and lookups across different keys do not contend; same
key contends. The pool is currently `#[allow(dead_code)]`
(`crates/daemon/src/daemon/connection_pool/mod.rs:8`,
`mod.rs:16`) and only wired through the async-feature scaffold
(`async_session/listener.rs:202`). It does not contribute to
sync-path contention today; when the async path lands it will,
and `DashMap` is the right choice for the access pattern (one
insert per accept, one delete per close, occasional iteration
for rate limiting).

### 7.5 `ConnectionCounter`

Single shared `AtomicUsize`
(`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:17`).
Increment on accept (`connection_counter.rs:39`), decrement on
worker exit (`connection_counter.rs:71-73`). Two atomic ops per
session, no locks. Negligible at all scales.

### 7.6 BufferPool (`crates/engine`)

`crossbeam_queue::ArrayQueue` plus a thread-local single-slot
cache (`crates/engine/src/local_copy/buffer_pool/pool.rs:32-77`).
Single-CAS acquire / return on the central queue
(`pool.rs:54-58`). The thread-local cache means the *common*
case never touches the central queue; only when a thread holds
multiple buffers concurrently does the queue see traffic. Per
#1329 (completed) the lock-free design absorbs daemon-level
fan-out without becoming a bottleneck at 1 000 sessions.
At 10 000, the soft-cap admission counter
(`pool.rs:69-77`) starts rejecting returns, falling back to
fresh allocation - which is exactly the safety valve.

### 7.7 Audit / metrics writers

Daemon does not currently maintain per-connection metrics on the
sync path. The async-side `ConnectionPool` aggregates byte
counters via `add_bytes` (`pool.rs:202-210`), again on
`DashMap`. Pre-xfer / post-xfer hook execution is per-connection
and runs in the worker thread; no shared writer.

### Verdict

At 1 000 sessions the dominant contended structure is the
`SharedLogSink` `Mutex`, followed by the `ConnectionLimiter`
lock file (only when configured). Both are O(1) per session
without contention, and serialise under load. At 10 000
sessions the limiter is the hot-spot if configured, and the
log mutex catches up if logging is verbose. None of the
in-process atomics or DashMaps are first-order bottlenecks
within the range of #1933.

## 8. Comparison with upstream rsync daemon

Upstream rsync 3.4.1 forks one child process per accepted TCP
connection (`socket.c:599` per the citations in
`docs/audits/daemon-event-loop-multiplexing.md` and
`docs/audits/daemon-async-listener-rfc.md` Section 10). The
parent's `select(2)` watches only listening fds (one per address
family), so upstream has *no* connection-multiplexing event loop -
just an accept-and-fork.

| Property | Upstream fork-per-connection | oc-rsync sync thread-per-connection |
|----------|-------------------------------|-------------------------------------|
| Concurrency unit | Process | OS thread |
| Address space per connection | COW page tables (~10-50 MiB initially, grows on write) | Shared, plus 8 MiB stack reservation per thread (glibc) |
| Heap | Independent (COW) | Shared `malloc` arena, contention possible |
| File descriptors | Inherited copy; closing parent's listener in child is mandatory (`socket.c:603-604`) | Shared process-wide; no isolation |
| Crash isolation | Free (process boundary) | `catch_unwind` on `connection.rs:127` |
| Inter-connection state | None (full isolation) | All `Arc<...>` config shared, all global locks shared |
| Reload / SIGHUP | Parent re-reads config; running children keep old config until they exit (their `Arc` clones in oc-rsync mirror this) | `reload_daemon_config` (`reload.rs:12`) swaps `Arc` for new connections; existing connections retain old config via their already-cloned `Arc` |
| Idle connection cost | Full process page tables + child heap (tens of MiB) | One thread stack reservation (8 MiB) |
| Active connection peak memory | Per-process heap, isolated | Shared heap, additive |
| Thread / process ceiling | `RLIMIT_NPROC` | `RLIMIT_NPROC` (Linux counts threads against this) |

Upstream's fork model has higher per-connection startup cost (the
COW page tables and the page-fault on first write are visible at
~ms latency on modern Linux) but pays no shared-state contention.
oc-rsync's thread model has near-zero spawn latency and shares
configuration `Arc`s for free, but pays for shared mutable state
(log sink mutex, lock file). In practice the two match each other
within a small constant factor on active load and diverge only on
*idle*-connection scaling, where upstream's COW tables dominate
and oc-rsync's reserved stacks dominate. Both lose to the async
path at 10 000 idle connections.

The fork model's crash isolation is the tradeoff oc-rsync
explicitly accepted. `catch_unwind` plus `panic = "unwind"` (the
workspace default) gives the same observable behaviour for safe
Rust panics. A SIGSEGV in `unsafe` code anywhere in the daemon
process still kills every connection - upstream survives that
case, oc-rsync does not. Mitigation: the strict unsafe-code policy
(see project rules) keeps `daemon` and `core` free of `unsafe`.

Reference: upstream daemon entry points and `start_accept_loop`
are documented in
`docs/audits/daemon-event-loop-multiplexing.md` Section "Upstream
comparison" with file:LINE citations into
`target/interop/upstream-src/rsync-3.4.1/clientserver.c` and
`socket.c` when the interop fixture is fetched per project
README.

## 9. Recommendations

A phased migration that does not require any wire-protocol change
and keeps the synchronous path the parity-tested default until the
async path lands and benchmarks favour it.

### 9.1 Short-term: enforce daemon-level concurrent-connection cap

`max_sessions` is currently a *total served* cap, not an
*active concurrent* cap (Section 6.1). This is a footgun: an
operator who sets `max_sessions = 100` expecting "at most 100
concurrent" gets instead "the daemon stops accepting after 100
total connections, ever, until restart".

Recommendation: change the post-accept check at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:264-270`
and `:352-358` to consult
`ConnectionCounter::active`
(`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:33`,
which is already plumbed but currently
`#[allow(dead_code)]` at `connection_counter.rs:32`). When
`active >= limit`, drop the new stream silently (matching the
upstream `lp_max_connections()` rejection behaviour cited in
`docs/audits/daemon-async-listener-rfc.md` Section 5.6) and log
the rejection at info level to the daemon log sink. Either rename
the directive to make the new semantics clear (`max_active`) or
keep `max_sessions` and document the change. The benchmark in
#1933 should run with this fix in place.

This is a self-contained ~30-line change. No protocol change. No
new dependencies. It removes the 10 000-connections-and-die
failure mode entirely.

### 9.2 Medium-term: shared transfer worker pool

The transfer body inside `handle_session` is the heavy bit (the
greeting and auth phases are ~hundreds of bytes each). A natural
intermediate step before full async migration is to keep
thread-per-connection for accept-and-greet but route the transfer
phase onto a bounded shared pool. Two shapes:

- `rayon::ThreadPool::install` per session for the file-list,
  generator, and sender / receiver entry points. Reuses an
  existing dependency. Limits the maximum concurrent
  *transferring* sessions to the pool size. Sessions waiting for a
  pool slot are blocked at the dispatch entry, so the
  pre-transfer phases stay parallel up to `max_sessions` while the
  expensive part is throttled.
- A daemon-owned `crossbeam_channel`-fed worker pool, sized by an
  operator directive, that the session thread pushes
  `(stream, params)` tuples into. The session thread blocks on a
  return-channel for the result. This is the more invasive
  change.

The first option is mechanical; the second pre-figures the
async-listener hand-off and is best deferred to that work.
Recommendation: skip this layer if #1935 is funded; otherwise
adopt option 1 as a stop-gap.

### 9.3 Long-term: async listener (#1935)

Per `docs/audits/daemon-async-listener-rfc.md` (task #1934),
already RFC'd. The async path is gated behind a new
`async-daemon` Cargo feature on the `daemon` crate
(`docs/audits/daemon-async-listener-rfc.md` Section 5.2),
runs the accept loop and pre-handshake greeting on a tokio
`current_thread` runtime
(`daemon-async-listener-rfc.md` Section 5.3), and hands off the
selected session to `tokio::task::spawn_blocking` running the
*existing synchronous* `handle_session` body unchanged
(`daemon-async-listener-rfc.md` Section 5.5). Wire bytes are
identical between paths; goldens in `crates/protocol/tests/golden/`
apply equally.

Async surface area required before this is viable, per the RFC:

- A `tokio::sync::broadcast` channel that mirrors `SignalFlags`
  (`crates/daemon/src/daemon/sections/signals.rs:8-21`) so the
  500 ms poll cadence is replaced by an event-driven wake
  (RFC Section 5.7).
- A `tokio::sync::Semaphore` for admission-time `max_sessions`
  enforcement (RFC Section 5.6) - the semantics fix from
  Section 9.1 above already prefigures this.
- Per-listener `tokio::net::TcpListener` instances, one per
  bound address, multiplexed with `tokio::select!` (RFC
  Section 5.4). Replaces the per-listener acceptor thread plus
  `mpsc::channel` fan-in
  (`connection.rs:281-385`).
- Conversion from `tokio::net::TcpStream` back to
  `std::net::TcpStream` via `into_std()` plus
  `set_nonblocking(false)` before `spawn_blocking`, so no async
  type leaks across the crate boundary (RFC Section 5.5,
  consistent with the tokio-scope policy at #1779 / #1818).
- Production wiring parity with `serve_connections`
  (`accept_loop.rs:11-319`): become_daemon, drop_privileges,
  PID file, syslog, dual-stack bind, socket options, proxy
  protocol, reverse DNS, bandwidth limit. The RFC enumerates the
  port checklist; #1935 ships the implementation.

The benchmark gate for flipping the default from sync to async is
in #1933: idle RSS < 200 MiB at 10 000 connections, accept
latency comparable, drain latency on SIGTERM under 5 s.

## 10. Open questions

These are flagged here, not resolved. Each maps to an existing
tracker.

1. **`io_uring` interaction with the async listener (#1595).**
   The fast_io session-ring pool design
   (`docs/design/iouring-session-ring-pool.md`,
   `docs/audits/shared-iouring-session-instance.md`) keeps one
   `io_uring` instance per worker thread. With async +
   `spawn_blocking`, each `spawn_blocking` thread maps to one
   ring; the accept thread (the runtime's executor) does no
   io_uring work. The combination is straightforward in shape
   but unproven in practice; #1595 owns the verification.
2. **Cross-runtime SSH (#1593).** The russh client lives behind
   the `async-ssh` feature on `crates/rsync_io`. If both
   `async-daemon` (on `crates/daemon`) and `async-ssh` (on
   `crates/rsync_io`) are enabled in the same binary, the daemon
   runs a tokio runtime and the SSH client wants its own. Per
   tokio-scope policy (#1779, #1818) we keep one runtime; the
   open question is whether the SSH client can run on the
   daemon's runtime or must spawn its own
   `current_thread` runtime on a `spawn_blocking` thread.
   #1593 owns the answer.
3. **Rayon CPU work in async context (#1751).** Rayon's global
   thread pool (Section 6.4) is synchronous; calling
   `par_iter()` from an async task blocks the executor. The
   fix is to wrap rayon dispatches in `spawn_blocking`; the open
   question is whether to do that at the call sites (large
   surface area in `crates/transfer`, `crates/engine`,
   `crates/flist`) or at a single shim. #1751 owns the design.
4. **Adaptive blocking-pool sizing.** Tokio's default
   `max_blocking_threads = 512`. For `max_sessions = 1000` this
   queues; for `max_sessions = 10000` it serialises hard. The
   RFC (`daemon-async-listener-rfc.md` Section 8 question 5)
   recommends `max_blocking_threads = max(max_sessions + 32, 512)`;
   the open question is whether to autotune via the proposed
   adaptive sizer (`docs/design/adaptive-thread-pool-sizing.md`)
   or pin it from config.
5. **Dual-stack partial-bind on async.** The sync path tolerates
   per-family bind failure
   (`accept_loop.rs:152-160`); the async port must mirror this
   (`daemon-async-listener-rfc.md` Section 8 question 7).
   Mechanical with `tokio::net::TcpListener::bind` per address.
6. **Windows accept semantics.** Tokio uses IOCP on Windows;
   `accept` semantics differ from epoll / kqueue
   (`daemon-async-listener-rfc.md` Section 8 question 8 and
   #1682).
7. **Lock-file scaling under fan-out.** The `flock(2)`-based
   limiter (Section 7.2) is per-acquire and per-release. At
   10 000 concurrent acquire attempts on a single lock file the
   round-trip serialises in the kernel. Whether the lock file
   should be replaced by a per-module datagram socket or a
   process-shared `Mutex` in `/dev/shm` is out of scope here;
   tracked separately if the benchmark in #1933 highlights it.

## 11. Test plan

Empirical numbers are produced by #1933, which this audit frames.
The benchmark targets the three waypoints:

- **100 concurrent connections.** Baseline. Asserts the daemon
  serves cleanly with no resource pressure. Acceptance: idle RSS
  < 200 MiB, accept latency < 50 ms p99, no errors in log.
- **1 000 concurrent connections.** Operating point. Asserts the
  daemon does not exceed default fd ulimit when the operator
  raises it to 4096, and that signal handling stays responsive.
  Acceptance: idle RSS < 1.5 GiB on glibc, < 500 MiB on musl,
  SIGTERM-to-drain latency under 1 s, no `accept(2)` failures.
- **10 000 concurrent connections.** Stress. Asserts the
  short-term `max_sessions` enforcement (Section 9.1) cleanly
  rejects connections beyond the cap. With `max_sessions =
  10000` the daemon should accept all 10 000; with the async
  listener (#1935) the host RSS budget should hold under
  300 MiB. With the sync path the test documents (does not
  require) the address-space cost.

The benchmark must run against three configurations:

1. oc-rsync sync path (this audit's subject).
2. oc-rsync async path behind `--features async-daemon` (post #1935).
3. Upstream rsync 3.4.1 in `target/interop/upstream-src/`.

Measurement axes per #1933:

- Daemon RSS and `VmSize` (`/proc/$pid/status`).
- Thread / process count (`/proc/$pid/status` `Threads`).
- Accept latency: time from `connect()` returning to
  greeting-line receipt, p50 / p99 / max.
- SIGTERM-to-drain latency (clock from signal sent to last
  worker exit).
- Active-transfer follow-up: 1 000 concurrent active transfers
  of 1 MiB / 100 MiB / 1 GiB files, aggregate throughput vs the
  three configurations.

The harness lives at `crates/daemon/benches/daemon_benchmark.rs`
(extended in #1933) and reuses
`scripts/rsync-interop-server.sh` plus
`tools/ci/run_interop.sh` for the upstream-comparison axis.

## 12. References

- Sync accept loop:
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`,
  `connection.rs`, `connection_counter.rs`, `listener.rs`,
  `workers.rs`, `reload.rs`.
- Module state:
  `crates/daemon/src/daemon/module_state/mod.rs`,
  `module_state/runtime.rs`,
  `module_state/connection_limiter.rs`,
  `module_state/definition.rs`.
- Module-access flow:
  `crates/daemon/src/daemon/sections/module_access/request.rs`,
  `module_access/transfer.rs`,
  `module_access/authentication.rs`.
- Greeting:
  `crates/daemon/src/daemon/sections/greeting.rs`.
- Async scaffold:
  `crates/daemon/src/daemon/async_session/mod.rs`,
  `async_session/listener.rs`,
  `async_session/session.rs`,
  `async_session/shutdown.rs`.
- Connection-tracking pool:
  `crates/daemon/src/daemon/connection_pool/mod.rs`,
  `connection_pool/pool.rs`,
  `connection_pool/types.rs`.
- Runtime options:
  `crates/daemon/src/daemon/runtime_options/types.rs`,
  `runtime_options/parsing.rs`.
- Signals:
  `crates/daemon/src/daemon/sections/signals.rs`.
- Buffer pool:
  `crates/engine/src/local_copy/buffer_pool/pool.rs`.
- Process model:
  `docs/DAEMON_PROCESS_MODEL.md`.
- Sibling audits:
  `docs/audits/daemon-event-loop-multiplexing.md` (#1675),
  `docs/audits/async-daemon-listener.md` (#1934 sketch),
  `docs/audits/daemon-async-listener-rfc.md` (#1934 RFC).
- Adaptive sizer:
  `docs/design/adaptive-thread-pool-sizing.md`.
- Related trackers: #1933 (benchmark), #1934 (RFC, completed),
  #1935 (implement async listener), #1683 (lower
  `SIGNAL_CHECK_INTERVAL`), #1595 (io_uring async),
  #1593 (cross-runtime SSH), #1751 (rayon via
  `spawn_blocking`), #1329 (lock-free buffer pool, completed),
  #1354 (accept-loop decomposition, completed),
  #1357 (async-session decomposition, completed),
  #1253 (module-state decomposition, completed),
  #1779 / #1818 (tokio scope policy).
- Rayon thread pool:
  <https://docs.rs/rayon/1/rayon/index.html#using-the-rayon-thread-pool>.
- glibc default thread stack size: `man 7 pthreads`.
- Linux fd / thread limits: `man 2 getrlimit`,
  `/proc/sys/kernel/threads-max`.
