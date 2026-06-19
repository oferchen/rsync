# Daemon Async Accept Loop with Sync Transfer Workers

Status: Design (TODO #1674)
Audience: daemon maintainers, transfer-pipeline maintainers, operators
Scope: hybrid async/sync execution model for the rsync daemon at high concurrency

> Feature flag: the rollout described here lands behind the existing
> `async-daemon` Cargo feature on `crates/daemon` (see `crates/daemon/Cargo.toml`).
> The concrete implementation steps live in
> `docs/design/daemon-tokio-async-listener-impl.md`; this document covers the
> long-form design rationale.

## 1. Problem Statement

`oc-rsync` runs as a long-lived daemon serving the rsync wire protocol over TCP. The current accept path is a synchronous loop that spawns one OS thread per accepted connection and then runs the full transfer state machine on that thread until the connection terminates. The model is simple, panic-isolated (`catch_unwind` per worker), and zero-allocation for the steady state. It works well into the low hundreds of concurrent connections and is the path that has been hardened against upstream interop.

The model breaks down at higher fan-out. Three measurable issues appear once the connection count climbs past roughly 1k:

1. **Thread creation cost dominates short connections.** Module listings (`rsync rsync://host/`) and small file probes complete in milliseconds. On Linux a fresh `pthread_create` plus `clone` plus stack mapping is in the same order of magnitude as the protocol handshake itself, so the OS thread cost can rival the productive work the worker performs.
2. **Accept-path serialisation under burst load.** The single-listener loop in `crates/daemon/src/daemon/sections/server_runtime/connection.rs:216-274` does one blocking accept, one synchronous `thread::spawn`, and only then circles back to accept the next connection. Each spawn is a syscall and a heap allocation; under SYN bursts the kernel queue grows while we are paying spawn cost.
3. **Memory pressure from thread stacks.** Every spawned worker reserves an 8 MiB virtual stack by default. At 10k concurrent connections that is 80 GiB of address space, even if RSS stays modest. RSS still grows linearly with page-touched stack frames.

The accepted upper bound on the existing model is roughly the lower of `max_connections` and what operator stacks tolerate. Operators who want to use `oc-rsync` as a fan-in target for 1k-10k concurrent listings or short transfers need a path that does not pay full thread cost per connection.

This document specifies a hybrid model that keeps the synchronous transfer path unchanged and only replaces the accept-and-dispatch layer.

## 2. Today's Model

The synchronous daemon entry point lives in
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:11`
(`fn serve_connections`). That function:

- registers signal handlers,
- binds one or more `std::net::TcpListener` sockets,
- builds the shared module table,
- drops privileges if requested,
- delegates to either `run_single_listener_loop` (single bind address) or
  `run_dual_stack_loop` (IPv4 + IPv6 dual stack).

Both loops live in
`crates/daemon/src/daemon/sections/server_runtime/connection.rs`. The single-bind
hot path is `run_single_listener_loop` at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:216`. The dual-stack
fan-in via `mpsc::channel` is `run_dual_stack_loop` at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:281`.

For each accepted `(TcpStream, SocketAddr)` they call
`spawn_connection_worker` at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:106`. That
function does exactly one thing: `std::thread::spawn(...)` a closure that wraps
`handle_session` in `catch_unwind`. The handle is pushed into
`AcceptLoopState::workers`
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs:5-24`) so
the accept loop can reap finished threads and so SIGUSR1 (graceful drain) can
wait for them.

Inside the worker, `handle_session` walks the rsync daemon protocol state
machine. The session-state enum is in
`crates/daemon/src/daemon/session_registry.rs:33-48`:

```
Handshaking -> Authenticating -> Listing | Transferring -> Completed | Failed
```

The implementation is fully blocking: the worker reads `@RSYNCD:` greetings,
runs the auth handshake, parses a module name, runs the receiver/sender loop,
and finally drops the stream. There is no `.await` anywhere on the data path.
Today's behaviour is the right baseline because the rsync transfer machinery
under `core`, `engine`, `transfer`, `protocol`, `checksums`, and `metadata`
is itself synchronous: blocking reads, blocking writes, mmap, file I/O,
SIMD checksums.

Panic isolation is done at the worker boundary by `catch_unwind`. A panic
inside `handle_session` is converted into a logged daemon error; the daemon
continues to accept. Match upstream's fork-per-connection crash-isolation:
`crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:1-10`.

This is the model the hybrid design must preserve byte for byte once a
connection has been handed to a worker.

## 3. Hybrid Model

The hybrid model splits the daemon into two cooperating layers:

```
+-------------------------+         bounded sync channel        +--------------------------+
|  Tokio async reactor    |  --(TcpStream, SocketAddr)-->       |  Sync transfer pool      |
|                         |                                     |                          |
|  - bind/accept          |                                     |  - N pre-spawned threads |
|  - apply socket opts    |                                     |  - run handle_session    |
|  - optional TLS         |                                     |  - catch_unwind          |
|  - reverse DNS          |                                     |  - block on rsync I/O    |
|  - send to handoff      |                                     |  - drop stream, loop     |
+-------------------------+                                     +--------------------------+
        ^                                                                |
        |                                                                |
        +---- shared signal flags / shutdown broadcast / metrics --------+
```

Key invariants:

1. **Tokio touches only the cheap parts of a connection.** Bind, accept, socket
   options, optional TLS termination, optional reverse DNS lookup, send the
   `(stream, peer_addr)` to a worker. Anything past the handshake stays
   synchronous.
2. **The transfer state machine is unchanged.** The worker calls the existing
   `handle_session` exactly as today. There is no async rewrite of the rsync
   sender, receiver, generator, or delta engine. We do not add `.await` to
   the hot path.
3. **The worker pool is small and fixed.** N workers, no implicit growth, no
   unbounded blocking dispatch.

The async layer exists already in skeleton form. The TCP listener and
per-connection task scaffolding is in
`crates/daemon/src/daemon/async_session/listener.rs` (the `AsyncDaemonListener`
type at `crates/daemon/src/daemon/async_session/listener.rs:108-293`). Today
it dispatches directly into an async session handler in
`crates/daemon/src/daemon/async_session/session.rs`; under this design that
direct dispatch is replaced by a handoff to the sync worker pool.

The async feature gate is already wired:
`crates/daemon/Cargo.toml:16-30` exposes `async = ["dep:tokio", "core/async"]`.
The pool layer extends the same gate.

## 4. Three Pool-Handoff Strategies

Three approaches are viable. Each preserves the sync transfer path; they
differ in how the async accept side hands a connection to a worker.

### Strategy A: Bounded sync channel + dedicated thread pool

A fixed pool of `N` `std::thread` workers is spawned at daemon startup. Each
worker blocks on a single `crossbeam_channel::Receiver<(TcpStream, SocketAddr)>`.
The async accept task pushes accepted connections through the matching
`Sender`. Capacity is a small bounded value, e.g. `2 * N`.

- The async side calls `tokio::task::spawn_blocking(|| sender.send(item))`
  or, more directly, uses an `Arc<async_channel::Sender>` so the send itself
  is `await`-able. When the channel is full the async accept future yields
  and the kernel SYN backlog absorbs the burst.
- The sync side calls `receiver.recv()` and runs the existing
  `handle_session` body unchanged, including the `catch_unwind` wrapper.
- Memory is bounded: at most `N + cap` live `TcpStream`s, plus `N` thread
  stacks.

This is the strategy used in mature high-throughput Rust services; the same
pattern is documented in
`docs/design/async-channel-abstraction.md` for the transfer pipeline and
the same `Sender`/`Receiver` abstraction can be reused here without
introducing a second concurrency primitive.

### Strategy B: tokio::task::spawn_blocking

`tokio::task::spawn_blocking` dispatches each accepted connection onto
tokio's blocking thread pool. The accept task simply does:

```text
let (stream, peer_addr) = listener.accept().await?;
tokio::task::spawn_blocking(move || handle_session(stream, peer_addr, ...));
```

This is the smallest code change, but it has two real problems:

- The blocking pool's default sizing is `512` threads (tokio default
  `max_blocking_threads`). It is unbounded relative to load: tokio queues
  blocking jobs and grows the pool on demand. At 10k bursting connections
  the daemon would spawn thousands of blocking threads, the same failure
  mode the synchronous loop already has.
- The blocking pool is shared with any other tokio blocking work (DNS
  resolution, file I/O on the async path). Mixing rsync transfers with
  tokio's other consumers makes pool sizing a moving target.

This strategy is rejected for production but is documented because operators
sometimes reach for it intuitively.

### Strategy C: Worker pool with explicit join handles

Same as A in steady state, but the daemon retains the `JoinHandle` for each
worker thread (mirroring `AcceptLoopState::workers` today at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:7`). Workers
are not respawned; they live for the lifetime of the daemon and pull from a
crossbeam channel.

C is operationally identical to A; the difference is only that A treats the
pool as fungible (any worker can crash and respawn) while C asserts a fixed
identity per worker. The implementation cost is similar; the operational
cost of C is slightly higher because crash-respawn becomes the daemon's
responsibility rather than the worker's. C is documented as a fallback in
case the channel-based handoff in A turns out not to be performant enough
in practice.

## 5. Recommended Approach

**Strategy A.**

Reasoning:

- It maps cleanly onto the existing transfer state machine. The worker code
  is byte-for-byte the closure body that
  `crates/daemon/src/daemon/sections/server_runtime/connection.rs:121-169`
  already runs. No protocol changes, no `.await` insertions, no risk of
  perf regression on the data plane.
- It bounds resource use deterministically. Memory is `O(N)` worker stacks
  plus `O(channel_capacity)` pending streams. No surprise pool growth.
- It produces natural backpressure at the kernel boundary. When the
  handoff is full, async accept stops accepting and clients queue at
  `listen()` backlog. Existing rsync clients tolerate this because the
  TCP connect simply completes a fraction of a millisecond later.
- It reuses the channel abstraction documented in
  `docs/design/async-channel-abstraction.md`, so the daemon does not
  introduce a second concurrency primitive (no SPSC ring vs MPSC channel
  vs crossbeam atomic mismatch).
- It is the model used by `hyper`'s blocking handler integration and by
  the standard rayon-channel patterns; the operational behaviour is
  well understood.

The remainder of the document specifies sizing, backpressure, failure
semantics, and migration for Strategy A.

## 6. Pool Sizing

Default: `N = num_cpus::get() * 2`.

Rationale:

- Rsync transfers are mixed I/O and CPU. Reads from disk, network sends,
  and (with `--compress`) zlib/zstd encode are interleaved. Pure I/O bound
  workloads benefit from oversubscription up to roughly 2x logical cores
  before context-switch cost dominates.
- Pure CPU work (rolling checksum, MD4/MD5/XXH3 strong hash, compression)
  is bounded by `num_cpus`. With 2x oversubscription the CPU-bound case
  reduces to "all cores busy plus a few queued workers", which is what we
  want.
- The same heuristic is what tokio uses for its default worker count and
  what rayon uses for its default global pool, so it is well calibrated
  for typical deployments.

Configuration knob: `transfer-worker-threads = N` in `oc-rsyncd.conf`.

Behaviour:

- If unset, `N = num_cpus::get() * 2`.
- If set to a positive integer, that value is used verbatim.
- If set to `0`, fall back to the default.
- Validated at config parse time via the existing
  `crates/daemon/src/rsyncd_config/validation.rs` machinery.

The knob is intentionally a global; per-module worker sizing would force
per-module pools and would not interact well with `--max-connections`. A
single global pool is the right unit because connections are fungible
during the dispatch window.

A second, related knob already exists: `max_connections` (per-module) and
`max-sessions` (daemon-wide). Those continue to gate how many connections
are admitted. `transfer-worker-threads` only sizes the pool that runs them.

## 7. Backpressure

The system has two bounded queues in series:

1. **Kernel SYN/accept backlog.** Sized by the `listen(2)` backlog argument,
   which is already plumbed through
   `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:135-160`
   as `listen_backlog` and capped by the kernel constant `somaxconn`.
2. **Handoff channel between async accept and the worker pool.** Capacity
   `2 * N` by default.

Steady state:

- Accept future receives a connection, applies socket options (already
  parsed at startup, not per-connection), optionally negotiates TLS,
  optionally resolves reverse DNS, then `await`s the channel send.
- A worker that finishes a session pulls the next item from the channel.
- The channel send completes immediately; accept loops back.

Burst state:

- The handoff fills. Send awaits.
- New TCP connections queue at the kernel backlog.
- When the kernel backlog fills, the kernel rejects further SYNs with
  RST or drops them depending on `tcp_abort_on_overflow`. Clients see a
  connect failure, which is the correct user-visible signal: the daemon
  is at capacity.

Operator implication: kernel `somaxconn` matters. The default is 4096 on
modern Linux; older kernels default to 128. Documentation must call out
that operators expecting to handle 10k+ concurrent connect attempts should
set both `listen_backlog` (in `oc-rsyncd.conf`) and `net.core.somaxconn`
(sysctl) to a value at least equal to the expected burst. Recommended
starting value: `min(expected_burst, 16384)`.

A future enhancement is a metric counter for "connections dropped because
handoff full". That metric is not in scope here but is listed in
section 13 as a tracking item.

## 8. Wire Compatibility

Zero impact.

The hybrid model only changes which thread runs `handle_session`. It does
not change:

- the `@RSYNCD:` greeting format,
- the protocol version negotiation,
- the auth challenge/response,
- the multiplex framing (`MSG_DATA`/`MSG_ERROR`/etc.),
- the file-list, delta, and end-of-transfer handshakes,
- the byte count, byte order, or padding of any wire frame.

The transfer state machine in `core::session()` and the engine pipeline
in `engine::*` are reached through the same `handle_session` entry point
in both models. There is no async/sync split inside that call.

This invariant is enforceable by inspection: under Strategy A, the diff
must not touch any file under `crates/protocol`, `crates/engine`,
`crates/checksums`, `crates/transfer`, or `crates/core`. The diff is
confined to `crates/daemon` plus the `core/async` feature wiring that
already exists.

## 9. Migration Plan

The hybrid path is feature-gated behind the existing daemon `async`
feature flag at `crates/daemon/Cargo.toml:20`. The async listener
scaffolding at `crates/daemon/src/daemon/async_session/listener.rs:108`
is already gated this way and is currently dead-code-allowed
(`#![allow(dead_code)]` at
`crates/daemon/src/daemon/async_session/mod.rs:28`) because no caller
reaches it from the production accept loop. The plan promotes that
scaffolding from "available in tests" to "available behind a feature
flag in production".

Phases:

1. **Phase 0: scaffolding (already merged).** `AsyncDaemonListener` exists
   and is exercised by tokio tests. The async feature is opt-in. No CLI
   surface change.
2. **Phase 1: pool primitive.** Add a `TransferWorkerPool` type in
   `crates/daemon/src/daemon/` that owns `N` worker threads and exposes
   `Sender<(TcpStream, SocketAddr, SessionParams)>`. Implementation
   mirrors `spawn_connection_worker` for each worker body so the worker
   closure is identical to today's. Synchronous dispatch into the pool
   is unit-testable without tokio.
3. **Phase 2: async accept calls into the pool.** Wire
   `AsyncDaemonListener::serve` to send into the pool's channel instead
   of `tokio::spawn(handle_async_session(...))`. The async session
   handler in `crates/daemon/src/daemon/async_session/session.rs` becomes
   a thin pre-handoff stage (TLS, reverse DNS, socket options) and then
   defers to the sync worker.
4. **Phase 3: production opt-in.** Add a config directive
   `use-async-listener = true` (default false). When set, the daemon
   starts the tokio runtime and uses `AsyncDaemonListener` plus the
   sync worker pool; otherwise it uses today's
   `run_single_listener_loop` / `run_dual_stack_loop`. The default stays
   off until benchmarks and panic tests in section 13 are green.
5. **Phase 4: default flip.** Once the async path is benchmarked at 100,
   1k, and 10k concurrent connections, and once panic-isolation tests
   pass, the default for `use-async-listener` flips to `true` for
   production builds. The sync path remains compiled in for at least
   one release for fallback.

The feature flag plus the config directive together give operators two
escape hatches: build without `--features async-daemon` (binary has no
tokio at all) or run the binary with `use-async-listener = false`
(tokio is linked but not used).

## 10. Failure Semantics

Two failure modes matter: a panic inside a worker, and a worker that
becomes unresponsive (deadlocked or stuck in a syscall).

### Panic Isolation

Each worker body wraps `handle_session` in `std::panic::catch_unwind` with
`AssertUnwindSafe`, the same pattern used today at
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:127-169`.
A panic:

1. Is caught at the worker boundary.
2. Logs an error message including the peer address and the panic payload
   description (already implemented as `describe_panic_payload`).
3. Drops the `TcpStream` (which sends RST or FIN to the client depending
   on the kernel state of the socket).
4. Releases the connection accounting (decrement
   `connection_counter`, drop `_conn_guard`).
5. The worker thread loops back to `recv()` on the channel.

The pool is not poisoned. A panicking session must not take down any
other session and must not take down the worker thread. The
`catch_unwind` boundary makes this explicit.

If `catch_unwind` itself returns `Err` because the panic was in a thread
that was already unwinding (pathological), the worker exits and the pool
respawns it. The respawn is bounded: at most one respawn per worker per
30 seconds, to avoid a respawn loop on a bug that panics during channel
recv. Repeated respawn attempts are logged at `error` level.

### Stuck Workers

The daemon does not enforce a per-session wall-clock timeout today, and
neither does upstream rsync. A pathologically slow client can hold a
worker indefinitely. The hybrid model inherits this property. Mitigations
that already exist:

- TCP keepalive, set on the accepted socket via the existing
  `client_socket_options` plumbing in
  `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:178-206`.
- `--timeout` on the rsync wire protocol, which `core` enforces by
  read deadline.
- `--max-connections` at the daemon and module level.

The hybrid model adds one knob: when all `N` workers are busy and the
handoff channel is full, accept stops admitting connections. This is the
correct behaviour because admitting more would just translate handoff
backpressure into RAM growth.

A future improvement is a per-worker watchdog (worker-side last-progress
timestamp; reaper thread aborts after configurable idle time). That is
out of scope for the initial design and is tracked as a follow-up.

## 11. Auth and TLS

The async accept layer is the right place to optionally terminate TLS.
This is documented in the stunnel-replacement design (issue #2052) and
remains a future feature; it is not part of the initial async-accept
implementation. When TLS is added later, the dispatch order is:

1. async accept,
2. async TLS handshake (rustls or native-tls async wrapper),
3. handoff of the (now plaintext) byte stream to the sync worker.

The handoff item type changes from `(TcpStream, SocketAddr)` to a wrapper
that exposes synchronous `Read + Write` over either a raw `TcpStream`
or a TLS-wrapped stream. The sync worker does not know or care which is
which.

Auth (the rsync `@RSYNCD: AUTHREQD` challenge/response and the secrets
file lookup) runs inside the sync worker today. Under the hybrid model
that does not change. Auth is a few hundred bytes of synchronous I/O
plus a hash; pulling it into the async layer would force the auth code
to be async-aware, with no performance benefit. The recommendation is
to leave auth in the sync worker.

A future "auth in async" path is conceivable for very large fleets where
the auth challenge dominates connection cost; that is explicitly out of
scope here.

## 12. Risks

### Tokio Runtime Overhead on Small Daemons

Linking and starting a tokio runtime adds ~100k of code and a small
amount of fixed RAM (one runtime, one reactor). On a daemon serving a
single concurrent connection this is pure overhead.

Mitigation: the `async-daemon` feature flag is off by default, and even
when the binary is built with the flag the runtime is only started when
`use-async-listener = true` is set in `oc-rsyncd.conf`. Small daemons
keep paying zero tokio cost.

Recommendation: enable the async path only when expected concurrent
connections are >= 100. Below that, the sync path's per-thread cost is
negligible and tokio's reactor is a regression in straight-line latency.

### Thread-Pool Starvation Under Long-Running Transfers

If `N` long-running transfers consume every worker, the daemon stops
accepting new connections. This is by design (backpressure works), but
it is also a denial-of-service vector if a client can hold a worker
indefinitely.

Mitigations:

- Operator-tunable `transfer-worker-threads` to oversize the pool when
  long-running transfers are expected.
- `max-connections` at module level so a single module cannot starve
  the pool.
- Existing `--timeout` flag and TCP keepalive.
- Future: per-worker watchdog (section 10).

### Accept-vs-Worker Imbalance Under Load Spikes

If the async accept side runs faster than workers (typical), the handoff
fills. This is bounded by channel capacity. The risk is the opposite:
that the handoff is empty for a long time and workers spin (they don't,
because the channel `recv` blocks).

Mitigation: bounded handoff channel with capacity `2 * N`. Acts as a
shock absorber for short bursts (fills, drains) but does not let
unbounded queueing happen.

### Sync vs Async Code Duplication

The sync and async accept paths coexist for at least one release. This
duplicates some glue code (signal handling, listener bind logic, socket
options). The risk is divergence: a fix to one path that does not land
on the other.

Mitigation: shared helpers for the common bits. Today's
`accept_loop.rs` already factors out `bind_with_backlog`,
`apply_socket_options_to_listener`, `ConnectionLimiter`, etc. The async
path uses the same helpers. Only the loop and dispatch differ.

## 13. Tracking

Follow-ups (not added to the persistent TODO list, listed here for
reference):

- **Implementation TODO**: build `TransferWorkerPool` per phase 1 above.
  Includes the bounded crossbeam channel, the worker-thread lifecycle
  (spawn, recv, run, panic-recover, loop), and the respawn-on-double-
  panic guard.
- **Benchmark TODO**: measure throughput, latency p50/p95/p99, and CPU
  use at 100, 1k, and 10k concurrent connections. Compare hybrid against
  the existing sync model and against upstream rsync 3.4.1. Use the
  existing `scripts/benchmark.sh` harness and the
  `localhost/oc-rsync-bench:latest` container.
- **Panic-isolation test TODO**: integration test that injects a panic
  inside `handle_session` and asserts the daemon continues to accept
  new connections. Mirror today's
  `concurrent_tests.rs` style under
  `crates/daemon/src/daemon/concurrent_tests.rs`.
- **somaxconn doc TODO**: add a section to the daemon operator guide
  documenting `listen_backlog`, `net.core.somaxconn`, and the
  `transfer-worker-threads` knob, with sizing guidance for 100 / 1k /
  10k expected concurrency.
- **Drop-counter metric TODO**: expose a counter for connections
  dropped at the handoff (channel full + accept retry path).
- **Stuck-worker watchdog TODO**: optional per-worker idle reaper for
  pathological slow clients.

## Appendix A: Affected Files

Files that already exist and define the current synchronous accept path:

- `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs`
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs`
- `crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs`
- `crates/daemon/src/daemon/sections/server_runtime/reload.rs`
- `crates/daemon/src/daemon/sections/signals.rs`
- `crates/daemon/src/daemon/session_registry.rs`
- `crates/daemon/src/daemon/connection_pool/pool.rs`

Files that already exist and define the async scaffolding:

- `crates/daemon/src/daemon/async_session/mod.rs`
- `crates/daemon/src/daemon/async_session/listener.rs`
- `crates/daemon/src/daemon/async_session/session.rs`
- `crates/daemon/src/daemon/async_session/shutdown.rs`

Files that the implementation phase will add or extend:

- `crates/daemon/src/daemon/transfer_pool.rs` (new) - the
  `TransferWorkerPool` type, bounded handoff channel, worker bodies.
- `crates/daemon/src/daemon/async_session/listener.rs` - replace the
  inline `tokio::spawn(handle_async_session(...))` with a handoff into
  `TransferWorkerPool`.
- `crates/daemon/src/rsyncd_config/sections.rs` - add
  `transfer-worker-threads` directive parsing.
- `crates/daemon/src/rsyncd_config/validation.rs` - validate the new
  directive.
- `crates/daemon/Cargo.toml` - no change required; the existing
  `async` feature flag is the on/off switch.

## Appendix B: Decision Record

| Decision | Choice | Rationale |
|---|---|---|
| Pool kind | Pre-spawned `std::thread` workers | Lowest overhead, no tokio coupling on the data plane. |
| Handoff | Bounded channel | Backpressure, no unbounded growth. |
| Pool size default | `num_cpus * 2` | Mixed I/O and CPU, matches tokio and rayon defaults. |
| Pool size knob | `transfer-worker-threads` | Single global, no per-module pools. |
| Async runtime | tokio, optional | Gated behind existing `async` feature. |
| Default state | sync model | Async path opt-in until benchmarked. |
| Panic boundary | per worker, `catch_unwind` | Matches today's invariant. |
| Wire compat | none | Transfer state machine is unchanged. |

## Appendix C: Sequence Diagrams

### Steady-state connection acceptance (hybrid model)

```
client          tokio accept       handoff channel       sync worker
  |                  |                    |                   |
  | --- TCP SYN ---> |                    |                   |
  |                  | --- accept ------> kernel              |
  |                  | <-- (stream,addr) -|                   |
  |                  | (apply socket opts)|                   |
  |                  | --- send -------> [item queued] ------ recv -->
  |                  | (loop to accept)   |                   | run handle_session
  |                  |                    |                   | (handshake, auth,
  |                  |                    |                   |  list/transfer)
  |                  |                    |                   | drop stream
  |                  |                    |                   | loop
```

### Burst load with full handoff

```
client(s)        tokio accept       handoff channel       sync worker(s)
  |                  |                    |                   |
  | --- 1000 SYN --> | (kernel queues)    |                   |
  |                  | --- accept x N --> [N items in queue]  |
  |                  | --- send (full) -> [await]             |
  |                  |                    |                   | recv ----->
  |                  |                    |                   | (worker busy)
  |                  | (does not accept)  |                   |
  |                  |                    |                   | done
  |                  |                    | <- recv unblocks  |
  |                  | (send unblocks)    |                   |
  |                  | (resume accept)    |                   |
```

### Panic in a worker

```
sync worker
  |
  | recv (stream, addr)
  | catch_unwind {
  |   handle_session(...) -- panic!
  | }
  |
  | log error with peer addr
  | drop stream (RST / FIN)
  | release conn_guard
  | loop -> recv (next item)
```

The other workers and the async accept task are not affected. The daemon
continues to serve.
