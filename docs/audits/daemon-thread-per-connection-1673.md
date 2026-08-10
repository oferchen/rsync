# Daemon Thread-per-Connection Scalability Audit (task #1673)

Static analysis. No code changes. Companion to the longer
`docs/audits/daemon-thread-per-connection-scalability.md` (task-level audit)
and the predecessor sketch `docs/audits/daemon-thread-per-connection.md`.
The two existing documents catalogue mitigation paths exhaustively; this
audit narrows to a single concrete next step and a defensible threshold
table.

## 1. Scope

Audit the dimensions called out in task #1673:

- thread creation cost
- per-worker stack budget
- admission control (max connections, listen backlog)
- worker lifecycle (spawn / reap / drain)
- shared state between accept thread and workers
- upstream comparison (fork vs thread)

Sources:

- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs`
- `crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs`
- `crates/daemon/src/daemon/sections/server_runtime/listener.rs`
- `crates/daemon/src/daemon/sections/server_runtime/workers.rs`
- `crates/daemon/src/daemon/module_state/runtime.rs`
- `target/interop/upstream-src/rsync-3.4.1/socket.c`
- `target/interop/upstream-src/rsync-3.4.1/clientserver.c`

## 2. Current model in one paragraph

The accept loop dispatches to `run_single_listener_loop`
(`connection.rs:216-274`) or `run_dual_stack_loop`
(`connection.rs:281-385`). Both end in `spawn_connection_worker`
(`connection.rs:106-170`), which calls `thread::spawn` directly
(`connection.rs:121`) for every accepted `TcpStream`. The worker
acquires a `ConnectionGuard` (`connection_counter.rs:38-44`), wraps the
session in `std::panic::catch_unwind` (`connection.rs:127-145`), and
runs `handle_session` to completion. `JoinHandle`s accumulate in
`AcceptLoopState::workers` (`connection.rs:7`) and are reaped
opportunistically by `reap_finished_workers` (`workers.rs:7-20`) and
drained at shutdown by `drain_workers` (`workers.rs:23-28`).

## 3. Audit dimensions

### 3.1 Thread creation cost

Each connection calls `thread::spawn` directly at
`connection.rs:121`. There is no pool, no pre-warmed worker set, no
back-pressure on the spawn. Linux `clone3` plus stack mapping costs
roughly 50-150 us per spawn on commodity hardware - small compared to
a typical transfer but a measurable accept-latency floor when a burst
arrives.

### 3.2 Stack budget

`thread::spawn` (`connection.rs:121`) uses the Rust default stack of
2 MiB per worker. A repository-wide search for `stack_size` and
`thread::Builder` confirms no daemon code path constructs threads with
a smaller stack. On glibc the kernel actually reserves 8 MiB by
default (`pthread_attr_setstacksize` is not called); on musl the
reservation is 128 KiB. Most deployments run glibc, so the worst-case
budget is what matters: 8 MiB of reserved virtual address space per
connection, of which only a few KiB are committed in practice.

### 3.3 Admission control

The TCP listen backlog is wired through
`bind_with_backlog(addr, backlog)` (`listener.rs:87-110`) with a
default of 5 at `accept_loop.rs:139`, configurable via `listen
backlog` in `oc-rsyncd.conf`. This mirrors upstream
`socket.c:550` (`listen(sp[i], lp_listen_backlog())`).

There is no daemon-level cap on the number of concurrent worker
threads. `ConnectionCounter` exists (`connection_counter.rs:16-44`)
and is incremented on every spawn (`connection.rs:115,122`), but
`ConnectionCounter::active` is annotated
`#[allow(dead_code)]` at `connection_counter.rs:32` with the comment
"wired when daemon accept loop enforces max-connections". The accept
loop never reads it.

Per-module caps exist (`module_state/runtime.rs:55-95`) and are
enforced inside the worker after handshake completes; this gates the
session but does not gate the spawn. A module-less greeting and an
authentication that never finishes both consume a worker for the full
read timeout.

`max sessions` (the runtime option at `accept_loop.rs:42`) is the
total-lifetime accept count after which the daemon drains and exits
(`connection.rs:264-270`, `connection.rs:352-359`). It is not a
concurrency cap.

### 3.4 Lifecycle

Workers are joined, not detached. `state.workers.push(handle)`
(`connection.rs:246`, `connection.rs:347`) retains every spawned
`JoinHandle`. `reap_finished_workers` (`workers.rs:7-20`) walks the
vector each accept iteration, joining any handle whose
`is_finished()` returns true; `drain_workers` (`workers.rs:23-28`)
joins survivors during shutdown. Worker errors that are not normal
connection-closed kinds propagate as `DaemonError` and abort the
accept loop (`workers.rs:38-58`). Panics caught by `catch_unwind` log
and return `Ok(())`.

The reap walk is O(n) per accept. At a steady-state worker count of
n, every accept pays an n-step scan, which becomes a measurable
accept-latency component above ~1 k workers.

### 3.5 Shared state

State flowing from the accept thread into every worker
(`connection.rs:111-119`):

- `Arc<Vec<ModuleRuntime>>` cloned per spawn. `ModuleRuntime` carries
  `AtomicU32 active_connections` (`module_state/runtime.rs:15`) and
  an optional `Arc<ConnectionLimiter>` (line 16). The per-module
  counter contends on every connect/disconnect through
  `compare_exchange` (`module_state/runtime.rs:85-93`).
- `Arc<Vec<String>>` (motd lines), `Arc<Vec<SocketOption>>` (client
  socket options) - read-only after build.
- `Option<SharedLogSink>` cloned by `Arc::clone`
  (`connection.rs:114`).
- `ConnectionGuard` (`connection.rs:115`) holding an
  `Arc<AtomicUsize>`. Two atomic ops per session
  (`connection_counter.rs:39,72`); negligible contention.

The only hot lock is the per-module `ConnectionLimiter` lock file
(`module_state/runtime.rs:60`), held only during slot acquisition.

SIGHUP reload (`reload.rs`, invoked from
`connection.rs:66-75`) swaps `state.modules` in place. In-flight
workers continue to use their `Arc` clone of the previous module
list, so the swap is lock-free for workers. Operators should not
expect a reload to retroactively apply a new chroot or uid to an
existing session.

### 3.6 Upstream comparison

Upstream `socket.c:start_accept_loop` (lines 533-624)
`fork()`s a child per accepted fd (line 599). The child closes the
listening sockets (lines 603-604), reopens the log file (line 607),
runs the session function (line 608), and `_exit`s (line 610). The
parent closes the accepted fd (line 621) and continues. Crash
isolation is the OS process boundary; max-connections enforcement is
inside the child via `claim_connection` on a lock file
(`clientserver.c:744`).

We replace fork with thread plus `catch_unwind` (`connection.rs:127`)
and lose three properties upstream gets for free:

1. Address-space isolation. A heap-corruption bug in one session can
   poison another's `BufferPool` buffer. Upstream's child carries the
   corruption to `_exit`.
2. Per-child resource accounting. `getrusage(RUSAGE_CHILDREN)` does
   not apply; we cannot attribute RSS or CPU to a single session
   without instrumentation.
3. Safe per-module privilege drop. `chroot` and `setuid` from a
   worker thread affect the entire daemon process. The daemon
   chroot-and-drop audit (#2129, merged in PR #4009 - see
   `docs/audits/daemon-chroot-uid-drop-audit.md`) confirmed that
   per-module privilege drop is impossible under threads and that
   the daemon-level drop in `accept_loop.rs:238-270` is the only safe
   option.

We gain: lower per-connection RSS (no COW page-table cost), shared
`BufferPool` across sessions, no fork failure under address-space
fragmentation, and Windows portability (Windows has no fork).

## 4. Threshold table

Per-connection memory accounting (worst case, glibc, peak transfer):

| Component | Size | Citation |
|---|---|---|
| Reserved stack (glibc default) | 8 MiB virtual | `connection.rs:121` |
| Committed stack | ~64 KiB | kernel page-fault on use |
| `BufferPool` borrowed block | ~256 KiB | `crates/engine/src/local_copy/buffer_pool/pool.rs` |
| `ReorderBuffer` (concurrent delta) | 1-4 MiB | `crates/engine/src/concurrent_delta/` |
| Sender / receiver flist (per 10 k entries) | ~1.2 MiB | `crates/protocol/src/flist/` |
| `JoinHandle` retained in `state.workers` | ~64 B | `connection.rs:246` |
| Effective committed RSS / connection | ~5-8 MiB | sum, excl. virtual stack |

Concurrency waypoints:

| Concurrency | Virtual address space | Committed RSS | First limit hit | Verdict |
|---|---|---|---|---|
| 100 | ~800 MiB | ~600 MiB | none | Fine. Production-ready. |
| 1 000 | ~8 GiB | ~6 GiB | `RLIMIT_NOFILE` (default 1024) | Marginal. Operators must raise `ulimit -n` and tolerate ~50 us per-accept reap overhead. No daemon-level admission cap to refuse the 1001st connection cleanly. |
| 10 000 | ~80 GiB | ~60 GiB | `RLIMIT_NPROC`, address space on 32-bit, OOM on most boxes | Broken. The daemon will either run into the per-user thread cap (~30 000 on Linux default), commit thread stacks into the OOM killer's range, or block on `clone3` returning `EAGAIN`. The reap walk also becomes O(10 000) per accept. |

The 1 k waypoint is the operational ceiling today and the 10 k
waypoint is unreachable without either an async listener (task
#1935) or a hard admission cap that returns
`@ERROR: max connections (N) reached` like upstream
`clientserver.c:752`.

## 5. Mitigations, in order of implementation cost

1. **Daemon-level `max connections` admission cap.** Read
   `ConnectionCounter::active()` (`connection_counter.rs:32-35`,
   already wired) before `spawn_connection_worker`. If above a
   configured threshold, write
   `@ERROR: max connections (N) reached` to the socket and close.
   One config field, ~30 lines in the accept loop, no protocol
   change, no behaviour change for unconfigured daemons. Matches
   upstream `clientserver.c:744-756`.
2. **Smaller worker stack.** Replace `thread::spawn` at
   `connection.rs:121` with `thread::Builder::new().stack_size(512 *
   1024).spawn(...)`. Reduces glibc reservation from 8 MiB to 512
   KiB (16x), keeping a 32x safety margin over the deepest known
   call stack in `handle_session`. Pairs with the admission cap.
3. **Bounded worker pool.** Replace ad-hoc
   `Vec<JoinHandle>` (`connection.rs:7`) with a fixed-size pool plus
   a backlog queue. Eliminates the O(n) reap scan
   (`workers.rs:7-20`) and bounds the spawn rate. Larger change;
   requires rethinking the SIGHUP reload path.
4. **Async listener (task #1935).** Move accept, greeting, and auth
   onto a tokio runtime; reserve OS threads only for the bulk
   transfer phase. RFC at
   `docs/audits/daemon-async-listener-rfc.md` (task #1934). Largest
   change; eliminates the slowloris-on-auth class of failures
   entirely.
5. **Fork-per-connection.** Matches upstream byte-for-byte. Loses
   shared `BufferPool`, breaks Windows, requires re-architecting the
   logging sink and module state. Highest cost, lowest marginal
   payoff over options 1-4.

## 6. Recommendation

Implement option 1 only: a daemon-level `max connections` admission
cap that consults `ConnectionCounter::active()` before
`thread::spawn` in `connection.rs:121` and returns
`@ERROR: max connections (N) reached` (matching upstream
`clientserver.c:752`) when the limit is hit.

Rationale:

- It is the smallest change. The counter is already incremented and
  decremented correctly (`connection_counter.rs:38-44`,
  `connection_counter.rs:70-73`). Only the read site and the config
  field are missing.
- It is the change with the largest immediate payoff: it converts the
  1 k -> 10 k transition from "OOM / `EAGAIN` from `clone3`" into
  "graceful refusal at the configured cap". Today there is no clean
  way to refuse the 1001st connection.
- It is the prerequisite for every other mitigation. A smaller stack
  (option 2) only buys headroom; a bounded pool (option 3) needs an
  admission gate to feed it; the async listener (option 5) still
  wants a concurrency ceiling. Option 1 is the floor under
  everything else.
- It does not require benchmarking (task #1933) to land. Operators
  who hit the cap today have no signal beyond `clone3: EAGAIN` in
  dmesg; option 1 gives them an actionable error.

Out of scope for this audit: option 2 (smaller stack), option 3
(pool), option 4 (async listener, tracked in #1935), option 5 (fork,
not pursued).

## 7. Related tasks

- #1935 - async listener (RFC at
  `docs/audits/daemon-async-listener-rfc.md`).
- #1933 - benchmark at 100 / 1000 / 10000 concurrent connections.
- #2129 - daemon chroot / privilege drop audit (completed, PR #4009;
  `docs/audits/daemon-chroot-uid-drop-audit.md`).
- #1934 - the async listener RFC tracking task.
