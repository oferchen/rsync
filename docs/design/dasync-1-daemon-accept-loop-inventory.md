# DASYNC.1 - Daemon Accept Loop + Per-Connection Worker Inventory

Tracking: oc-rsync task #3982 (audit). Unblocks DASYNC.2 (#3983 design),
DASYNC.3 (#3984 impl behind `daemon-async-accept`), DASYNC.4 (#3985 bridge),
DASYNC.5 (#3986 5K/10K bench). DASYNC.5 closes D10K-3/4/5.

> Audit-only inventory of the synchronous, thread-per-connection daemon
> accept loop. This document fixes file:line anchors for the listener
> bind, accept loop, worker spawn, per-connection state hand-off, TLS /
> LSM / `--max-connections` overlays, and resource cleanup so the
> follow-on design (DASYNC.2) and implementation (DASYNC.3) work against
> a single, evidence-based baseline. No production code is modified.

All path/line anchors below are relative to the repository root and
refer to the tree at the audit commit on the
`docs/dasync-1-daemon-accept-loop-inventory` branch.

## 1. Current architecture (sync, thread-per-conn)

### 1.1 Entry point

- Public entry: `crates/daemon/src/daemon.rs:233 run_daemon()`.
- Dispatch: when `is_stdin_socket()` returns true the call routes to
  `serve_inetd_session(options)` (single-session inetd / socket-activation
  path). Otherwise it calls
  `serve_connections(options, external_signal_flags, pre_bound_listener)`
  at `crates/daemon/src/daemon.rs:249`.
- The synchronous accept loop body lives in
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11
  serve_connections()`.

### 1.2 Listener bind

- Bind helper: `crates/daemon/src/daemon/sections/server_runtime/listener.rs:126
  bind_with_backlog(addr, backlog) -> io::Result<TcpListener>`.
- Bind call sites (one per address family in dual-stack mode):
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:144`.
- Backlog default: `DEFAULT_LISTEN_BACKLOG = 128`
  (`listener.rs:48`), overridable per
  `oc-rsyncd.conf` via `listen backlog`. Upstream rsync defaults to 5
  (`socket.c:554`, `listen(sp[i], lp_listen_backlog())`).
- IPv6: `bind_with_backlog` sets `IPV6_V6ONLY=true` so a separate IPv4
  listener can coexist (`listener.rs:149-151`).
- Pre-bound listener injection (test infra, also socket activation):
  `accept_loop.rs:130-135` reuses a listener pulled from
  `DaemonConfig::take_pre_bound_listener()`.

### 1.3 Accept loop sites

Two distinct loops are selected by listener count:

- Single-listener path (IPv4-only or IPv6-only):
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs:334
  run_single_listener_loop()`.
  Uses non-blocking accept with `SIGNAL_CHECK_INTERVAL =
  Duration::from_millis(500)` between iterations
  (`listener.rs:55`).
- Dual-stack path (IPv4 + IPv6 acceptor threads + central dispatcher):
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs:408
  run_dual_stack_loop()`. Per-listener acceptor threads forward
  accepted sockets through an `std::sync::mpsc::channel` to the main
  dispatcher; the dispatcher polls with
  `rx.recv_timeout(Duration::from_millis(100))`.

Both loops poll `signal_flags` (SIGHUP, SIGTERM, SIGINT, SIGUSR1,
SIGUSR2) via `check_signals_and_maintain()` at
`connection.rs:41`.

### 1.4 Worker spawn pattern

Primary spawn site:
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:189
spawn_connection_worker(stream, raw_peer_addr, state)`.

Spawn primitive: `std::thread::spawn` (`connection.rs:204`). No
`Builder::stack_size()` override - each worker uses the platform
default (Linux glibc: 8 MiB virtual / ~2 MiB committed via lazy
mapping; macOS: 512 KiB; Windows: 1 MiB).

Crash isolation: `std::panic::catch_unwind(AssertUnwindSafe(...))` wraps
the session handler so a panic in one connection kills only that
worker, mirroring upstream's fork-per-connection isolation
(`connection.rs:210-228`). Panic payloads are decoded by
`describe_panic_payload()` and logged.

Auxiliary spawns in the dual-stack path:
`connection.rs:432 thread::spawn(...)` per listener (one per address
family - typically 2 total, so they are not part of the per-connection
cost).

### 1.5 Worker fn entry

- Per-connection entry inside `catch_unwind`:
  `crates/daemon/src/daemon/sections/session_runtime.rs:44
  handle_session(stream, peer_addr, params)`.
- After PROXY-protocol header handling
  (`session_runtime.rs:71 parse_proxy_header()`), the session
  delegates to:
  - `handle_binary_session()` (currently unreachable - daemon always
    speaks legacy `@RSYNCD:`; see comment at
    `session_runtime.rs:59-64`), or
  - `handle_legacy_session()` (the live path).

### 1.6 Per-connection state moved into the worker

`spawn_connection_worker()` captures and moves the following into the
worker closure (`connection.rs:189-227`):

- `stream: DaemonStream` (owned, full ownership transferred).
- `peer_addr: SocketAddr` (normalized via `normalize_peer_address()`;
  IPv4-mapped IPv6 demangled to plain IPv4).
- `modules: Arc<Vec<ModuleRuntime>>` (cloned via `Arc::clone`, refcount
  bump only).
- `motd_lines: Arc<Vec<String>>` (`Arc::clone`).
- `log_for_worker: Option<SharedLogSink>` (`Arc::clone`).
- `conn_guard: ConnectionGuard`
  (`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:69`)
  - RAII handle that decrements the daemon-wide active-connections
    atomic on `Drop`.
- `bandwidth_limit: Option<NonZeroU64>`, `bandwidth_burst:
  Option<NonZeroU64>` (`Copy`).
- `reverse_lookup: bool`, `proxy_protocol: bool` (`Copy`).

The cost per spawn is one OS thread plus four `Arc` refcount bumps and
one atomic increment - the per-connection state itself is reference-
counted, not cloned.

### 1.7 Resource cleanup + worker join

- Worker join handles accumulate in `AcceptLoopState::workers:
  Vec<JoinHandle<WorkerResult>>` (`connection.rs:7`).
- Per-iteration reap:
  `crates/daemon/src/daemon/sections/server_runtime/workers.rs:7
  reap_finished_workers()` walks the vector and joins any handle whose
  `is_finished()` returned true. Called from
  `check_signals_and_maintain()` at the top of each accept iteration.
- Shutdown drain:
  `crates/daemon/src/daemon/sections/server_runtime/workers.rs:23
  drain_workers()` joins remaining workers after the accept loop
  exits.
- Error mapping: `workers.rs:38 join_worker()` swallows panics and
  treats `BrokenPipe`, `ConnectionReset`, `ConnectionAborted` as
  normal session closes (`workers.rs:75
  is_connection_closed_error()`).
- Connection-counter cleanup: `ConnectionGuard::drop`
  (`connection_counter.rs:73`) decrements the atomic at thread exit
  via the RAII move into the worker closure - no explicit cleanup
  required from the session handler.

### 1.8 TLS overlay (rustls)

- Feature flag: `daemon-tls = ["dep:rustls", "dep:rustls-pemfile"]`
  (`crates/daemon/Cargo.toml:40`).
- Acceptor lives on `AcceptLoopState::tls_acceptor:
  Option<crate::tls::TlsAcceptor>` (`connection.rs:34`).
- Wrap site: `connection.rs:291 wrap_accepted_stream()` calls
  `crate::tls::wrap_stream(acceptor, tcp_stream)` immediately after
  `accept()` returns and before the worker thread spawns. On failure
  the error is logged and `None` is returned so the accept loop drops
  the socket and continues.
- The TLS handshake therefore runs in the **main accept thread**, not
  in the worker. Under DASYNC, the handshake must move into either the
  async runtime (`tokio_rustls`) or the blocking pool to avoid head-of-
  line blocking the accept loop.

### 1.9 LSM / sandbox overlays

- Daemon-level chroot + setuid/setgid (POSIX privilege drop): applied
  inside `serve_connections()` at
  `accept_loop.rs:235-267` after binding, after daemonization, after
  PID file creation, and **before** the accept loop starts. Pure
  startup-time concern - no impact on the per-connection path.
- Landlock LSM (defense-in-depth, opt-in via `landlock` feature on the
  `fast_io` dependency): engaged inside the per-module transfer
  prologue, not at accept time. Entry point
  `crates/daemon/src/daemon/sections/module_access/transfer.rs:217
  engage_landlock_sandbox()`, used by the transfer dispatch at
  `module_access/transfer.rs:737`.
- The `concurrent-sessions` feature toggles a richer session registry
  (`crates/daemon/src/daemon.rs:77-84`) but does not change the spawn
  primitive.

### 1.10 Connection cap admission (`--max-connections`)

- Per-daemon (global) cap is materialized into
  `AcceptLoopState::max_connections: Option<usize>`
  (`connection.rs:19`) from `RuntimeOptions::max_connections`
  (`accept_loop.rs:42, 296`).
- Refusal site: `connection.rs:124 refuse_if_at_capacity()`. Runs
  **after** TLS wrap and socket options, **before** worker spawn. On
  cap-hit the helper writes
  `@ERROR: max connections (N) reached -- try again later\n`
  (matching upstream `clientserver.c:752` byte-for-byte) and the loop
  drops the stream.
- Per-module cap continues to be enforced by `ModuleRuntime` /
  `ConnectionLimiter` inside the session handler
  (`accept_loop.rs:92-96, 98-103`); unchanged by DASYNC.
- Tracked under the DMC-* series (`--max-connections` admission).

## 2. Saturation analysis

Anchored by [[project_daemon_10k_conn_ceiling]] and the D10K-2 baseline
captured in `docs/design/daemon-thread-per-conn-bench.md`.

| Concurrent N | Status |
| --- | --- |
| 100  | Comfortable. p99 dominated by transfer time, not scheduling. |
| 1 000 | Documented OK. No measured cliff; CPU and disk dominate. |
| 5 000 | Untested. Predicted to start showing scheduler tail under spike arrivals; see DASYNC.5 (#3986). |
| 10 000 | Documented thread-per-conn ceiling (D10K-6). Stack VA pressure and `clone(2)` storm on burst arrival become the dominant cost. |

### 2.1 Stack overhead at N = 10K

- Linux: 8 MiB virtual address space per thread x 10 000 = **80 GiB
  VA**; resident pages depend on actual stack usage, but the kernel
  reserves the mapping eagerly for guard-page placement, so the
  virtual ceiling is the hard constraint on 32-bit and a soft
  constraint on 64-bit (kernel default `vm.max_map_count = 65 530`
  bounds the per-process VMA count - thread stacks plus `mmap` regions
  must fit).
- macOS: 512 KiB x 10 000 = ~5 GiB VA (default stack is smaller; the
  `pthread_attr_setstacksize` floor is 16 KiB).
- Windows: 1 MiB x 10 000 = ~10 GiB VA.

Beyond stack, each `std::thread::spawn` allocates a TLS area, a
`JoinHandle` heap entry, and registers with the libc thread list.

### 2.2 Accept-loop overhead

- Single-listener path: 500 ms tick on `set_nonblocking(true)` plus
  `accept`. On burst arrival the loop drains the accept queue
  synchronously, but each spawn between `accept()` calls adds
  `clone(2)` + `mmap` + `mprotect` syscalls on Linux (~hundreds of µs
  per spawn under contention).
- Dual-stack path: 100 ms tick on the central `recv_timeout`. The two
  acceptor threads run blocking `accept()` so they do not contribute
  CPU at idle, but they add an extra channel hop per connection.

### 2.3 What breaks first under DASYNC.5 load

In order of expected ceiling:
1. Listener backlog (`SOMAXCONN` / `DEFAULT_LISTEN_BACKLOG = 128`
   today) - DASYNC.5 will surface SYN drops first if not raised.
2. Reaper latency - finished workers stay in `state.workers` until the
   next accept iteration; under bursty arrivals the join handle
   vector grows.
3. Thread cost (the documented D10K-6 cliff).

## 3. Migration target shape (DASYNC.2 design hint)

The skeleton already exists at
`crates/daemon/src/async_listener.rs:73 run_hybrid_listener()` and the
entry point at `crates/daemon/src/daemon.rs:370 run_async_daemon()`
behind `--cfg feature="async-daemon"`. The target shape for DASYNC.2 to
formalize is:

1. **Async accept** via
   `tokio::net::TcpListener::bind(bind_addr).await?.accept().await`.
   Already wired at `async_listener.rs:95`. Polled with a 250 ms
   `tokio::time::timeout` so a stalled accept does not block shutdown
   (`async_listener.rs:106`).
2. **Per-conn worker still runs sync code** via
   `tokio::task::spawn_blocking(move || worker(std_stream,
   peer_addr))` (`async_listener.rs:133`). The bridge converts
   `tokio::net::TcpStream` -> `std::net::TcpStream` with
   `into_std()` then `set_nonblocking(false)`
   (`async_listener.rs:121-131`).
3. **Bound the blocking pool** to the current `--max-connections`
   value (or a tunable derived from it) so the existing admission cap
   becomes back-pressure for the runtime. Tokio's default blocking
   pool size is 512; for DASYNC.5 (N = 5K - 10K) the cap must be
   raised through `tokio::runtime::Builder::max_blocking_threads()`.
4. **TLS and LSM overlays unchanged.** TLS handshake remains on the
   synchronous worker (via `rustls::ServerConnection::complete_io()`
   in the blocking closure) so the wire-format and rustls invariants
   are untouched. LSM engagement (`engage_landlock_sandbox`,
   chroot/setuid drop) remains startup-time / module-prologue and is
   not visible to the bridge.
5. **Cap admission moves to a tokio
   `Semaphore` or `mpsc::channel(cap)`** so the accept loop awaits
   permits instead of refusing eagerly. Eager refusal must remain
   available so the wire-level `@ERROR:` message stays byte-identical
   with upstream when the cap is breached.

## 4. Open questions for DASYNC.2

### 4.1 Does the existing `async` feature flag already wire a tokio listener we can flip?

Partial. Two distinct feature flags exist:

- `async = ["dep:tokio", "core/async"]` (`Cargo.toml:20`) - pulls
  tokio for unrelated async paths in `core`. Does **not** install an
  async listener.
- `async-daemon = ["dep:tokio"]` (`Cargo.toml:25`) - gates the
  hybrid listener skeleton in `async_listener.rs` and the entry point
  `run_async_daemon()` at `daemon.rs:370`.

The skeleton accepts connections but installs a **drop-on-accept
worker** (`daemon.rs:411-418`) instead of dispatching to
`handle_session`. DASYNC.3 needs to wire the real synchronous worker
through the `SyncWorker` type alias
(`async_listener.rs:50`). DASYNC.2 must decide whether to keep two
flags or collapse to one.

### 4.2 Does the russh path (PR #5613) provide a shared async-accept pattern we should mirror?

To audit in DASYNC.2. The russh server-handle migration runs a tokio
runtime around russh's async stream and bridges into the rsync session
via `tokio::task::spawn_blocking`. The same bridge primitive
(`spawn_blocking` over `TcpStream`) is already used in
`async_listener.rs:133`. Sharing the runtime builder and blocking-pool
sizing across both paths is the natural unification point. Note the
known [[project_russh_spawn_blocking_ceiling]] - DASYNC.5 will share
that ceiling unless the blocking-pool cap is sized intentionally.

### 4.3 Will the worker bridge add latency under N = 1K (where current sync is already fine)?

To measure in DASYNC.5. Expected baseline cost of
`spawn_blocking` per connection is ~5 - 20 µs (channel hop +
worker-thread wake). At N = 1K with multi-second transfers this is
noise. The risk surface is ttfb p99 on short-lived requests - DASYNC.5
must therefore run both arrival shapes (steady-state plus burst) and
both connection lifetimes (1 MiB transfer plus protocol-only ping).

## 5. Follow-up tasks unblocked

- **DASYNC.2** (#3983) - design tokio-based async listener; formalize
  the section 3 target shape; decide single-feature vs dual-feature
  flag.
- **DASYNC.3** (#3984) - implement behind `daemon-async-accept`
  feature flag. Replace the drop-on-accept worker at
  `daemon.rs:411-418` with a real bridge to `handle_session`.
- **DASYNC.4** (#3985) - bridge from the async accept to the existing
  TLS / LSM / cap-admission overlays without duplicating their
  startup logic.
- **DASYNC.5** (#3986) - bench at 5K and 10K connections; closes
  D10K-3 / D10K-4 / D10K-5.

## 6. Cross-references

Project memory anchors:

- [[project_daemon_10k_conn_ceiling]] - documents the thread-per-conn
  ceiling at ~10K; gating decision for DASYNC.5.
- [[project_russh_spawn_blocking_ceiling]] - shared `spawn_blocking`
  bridge ceiling; relevant once DASYNC.3 lands and we share a runtime
  with the russh server path.
- [[project_no_async_threaded_only]] - establishes that the default
  daemon is blocking-channels-plus-threads, which is precisely the
  baseline DASYNC.1 inventories.

Adjacent design docs:

- `docs/design/daemon-async-runtime-choice.md` - runtime selection
  ADR; trigger conditions for production rollout.
- `docs/design/daemon-async-accept-sync-workers.md` - long-form design
  this audit feeds.
- `docs/design/daemon-tokio-async-listener-impl.md` - skeleton
  implementation plan already partially realized in
  `async_listener.rs`.
- `docs/design/daemon-thread-per-conn-bench.md` - D10K-2 baseline
  harness; DASYNC.5 reuses the driver.
- `docs/audits/daemon-thread-per-connection-scalability.md` - paper
  upper bounds the bench validates.

Upstream rsync 3.4.4 anchors used during the audit:

- `clientserver.c:1289 start_daemon()` - upstream per-connection entry
  (called by the parent for inetd and by each forked child for TCP).
- `clientserver.c:1546 daemon_main()` - upstream dispatch between
  inetd and TCP modes (mirrored at `daemon.rs:233 run_daemon()`).
- `socket.c:537 start_accept_loop()` - upstream TCP accept loop;
  forks per connection via `fork()` at `socket.c:603`. oc-rsync
  replaces the fork with `std::thread::spawn` plus `catch_unwind`,
  documented in the comment at
  `accept_loop.rs:1-10`.
