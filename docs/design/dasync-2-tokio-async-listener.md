# DASYNC.2 - Tokio Async Listener Design

Tracking: oc-rsync task #3983 (design). Builds on DASYNC.1
(`docs/design/dasync-1-daemon-accept-loop-inventory.md`, PR #5624).
Unblocks DASYNC.3 (#3984 impl behind `daemon-async-accept`), DASYNC.4
(#3985 bridge to TLS / LSM / cap-admission overlays), DASYNC.5 (#3986
5K/10K bench, closes D10K-3/4/5).

> Specifies the bridge from `tokio::net::TcpListener::accept` to the
> existing synchronous `handle_session()` worker. The hybrid skeleton
> already exists at `crates/daemon/src/async_listener.rs:73`
> (`run_hybrid_listener()`) but its worker closure at
> `crates/daemon/src/daemon.rs:411-418` drops accepted streams instead
> of dispatching. DASYNC.3 is therefore a wiring change, not greenfield;
> this document fixes the bridge contract, invariants, back-pressure
> model, and rollback criteria so the implementation lands on a single
> evidence-based design.

## 1. Scope and non-goals

In scope:

- Bridge from `tokio::net::TcpStream` to `std::net::TcpStream` and into
  the existing `handle_session()` entry point in
  `crates/daemon/src/daemon/sections/session_runtime.rs:44`.
- Admission cap (`--max-connections`) re-expressed as a
  `tokio::sync::Semaphore` permit acquired before `spawn_blocking`.
- Back-pressure model that lets accept() yield instead of opening
  unbounded sessions, while preserving upstream's eager `@ERROR: max
  connections (N) reached` refusal.
- Panic isolation across the runtime boundary so a panicking
  `handle_session` cannot poison the tokio runtime.

Out of scope:

- Async-ifying the data path. The sync receiver/sender state machine
  stays on `spawn_blocking`. See
  [[project_no_async_threaded_only]].
- New wire-protocol features. The bridge is wire-byte-for-byte
  equivalent to the thread-per-conn path.
- TLS handshake async-ification. Per DASYNC.1 section 1.8 the rustls
  handshake stays in the synchronous worker (inside `spawn_blocking`),
  matching the current sync accept loop behaviour.

## 2. Architecture

```
tokio::net::TcpListener::bind(bind_addr).await?
        |
        loop {
            tokio::time::timeout(250ms, listener.accept()).await
        }
        |
        AcceptedConnection { tokio_stream, peer_addr }
        |
        async_listener::handle_accepted_connection (new)
        |
        // Phase A: admission - awaits permit OR eagerly refuses.
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,                       // capacity available
            Err(TryAcquireError::NoPermits) => {
                refuse_eagerly(tokio_stream).await;      // upstream-compatible @ERROR
                return;
            }
            Err(TryAcquireError::Closed) => return,     // shutdown
        };
        |
        // Phase B: bridge - tokio stream -> std blocking stream.
        let std_stream = tokio_stream.into_std()?;     // sets nonblocking=false
        |
        // Phase C: dispatch on the blocking pool.
        tokio::task::spawn_blocking(move || {
            // permit moved into closure; Drop releases on exit
            let stream = DaemonStream::plain(std_stream);
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                handle_session(stream, peer_addr, session_params.clone())
            }));
            drop(permit); // explicit for clarity; Drop is sufficient
            result
        })
```

The accept loop body already exists at
`crates/daemon/src/async_listener.rs:90-144` (`accept_loop()`). The
"drops streams" worker at `crates/daemon/src/daemon.rs:411-418` is
replaced by a closure that constructs `SessionParams` from the resolved
`RuntimeOptions` plus shared module/MOTD/log handles, then calls
`handle_session(stream, peer_addr, params)`.

### 2.1 Type bridge: `tokio::net::TcpStream` -> `DaemonStream`

`handle_session` takes a `DaemonStream`, not a raw `TcpStream`
(`crates/daemon/src/daemon/sections/session_runtime.rs:44-48`). The
bridge therefore:

1. Calls `tokio_stream.into_std()` which returns
   `io::Result<std::net::TcpStream>` and internally restores blocking
   mode (`set_nonblocking(false)`).
2. Wraps the std stream in `DaemonStream::plain(stream)`
   (`crates/daemon/src/daemon_stream.rs:77`).
3. Hands the `DaemonStream` to `handle_session`. The TLS overlay (see
   section 5.1) substitutes `DaemonStream::Tls(...)` instead of
   `Plain(...)` after a blocking rustls handshake; the bridge layer
   itself never sees TLS.

`into_std()` is infallible in practice on Linux / macOS / Windows for a
stream that came out of `TcpListener::accept`. The `Result` exists
because tokio must deregister the FD from the I/O driver; a failure
here means the runtime is in a bad state. We treat it as a per-
connection drop (log + continue) so a single corrupt FD cannot wedge
the accept loop.

### 2.2 Why `spawn_blocking` and not `tokio::spawn`?

The session handler calls `read`/`write` on the std stream, performs
synchronous file I/O against the local filesystem, and invokes the
synchronous transfer pipeline. Running it on a non-blocking tokio
worker would block the runtime worker thread for the full transfer
lifetime, defeating the point of going async. `spawn_blocking` schedules
on the dedicated blocking pool (default 512 threads, raised to N =
`--max-connections` via `Builder::max_blocking_threads()`) which is
specifically sized for synchronous workloads.

This is the same bridge primitive used in the russh server path; see
[[project_russh_spawn_blocking_ceiling]] for the analogous design and
its known scaling ceiling.

## 3. Key invariants

These must hold across every code path in the bridge:

1. **The blocking pool is bounded by `--max-connections`.**
   `tokio::runtime::Builder::max_blocking_threads(N)` where N is the
   resolved cap. Without this, tokio defaults to 512 blocking threads
   and would silently override the operator's admission cap.

2. **A `tokio::sync::Semaphore` with `N` permits gates dispatch.**
   The permit is acquired *before* the `into_std()` conversion. The
   `OwnedSemaphorePermit` is moved into the `spawn_blocking` closure so
   its `Drop` releases the slot only after the worker returns (success,
   error, or panic).

3. **`tokio::net::TcpStream::into_std()` restores blocking mode for
   us.** No explicit `set_nonblocking(false)` call is required; the
   tokio docs guarantee the returned `std::net::TcpStream` is in
   blocking mode. The current skeleton at
   `async_listener.rs:128` calls `set_nonblocking(false)` defensively;
   we keep that as belt-and-suspenders.

4. **Per-connection state is `Arc`-shared, not cloned.** Mirrors the
   sync path (DASYNC.1 section 1.6): `Arc<Vec<ModuleRuntime>>`,
   `Arc<Vec<String>>` for MOTD, `Option<SharedLogSink>`. The bridge
   only bumps refcounts.

5. **`ConnectionGuard` equivalent lives inside the closure.** The
   semaphore permit replaces the per-thread `ConnectionGuard`
   (`crates/daemon/src/daemon/sections/server_runtime/connection_counter.rs:69`)
   for the daemon-wide active-connections atomic. The atomic itself is
   redundant once the semaphore is authoritative, but we keep it for
   diagnostic reporting (current active count is read via the same
   getter the sync path uses).

6. **Panic isolation matches the sync path.** `catch_unwind` wraps
   `handle_session` inside the closure (DASYNC.1 section 1.4). A panic
   payload that escapes `catch_unwind` would otherwise abort the
   blocking thread. Tokio surfaces the panic via `JoinHandle::await`,
   but by then the permit has already dropped, so the runtime stays
   healthy. The existing dispatcher at `async_listener.rs:134-141`
   already logs `Err(JoinError)` to stderr; we keep that path and add
   panic-payload decoding via `describe_panic_payload()` to match the
   sync path's diagnostics.

7. **Shutdown semantics are unchanged.** The accept loop polls
   `shutdown: Arc<AtomicBool>` between accepts (already in place at
   `async_listener.rs:98`). On shutdown, in-flight workers continue to
   completion exactly as `drain_workers()` does in the sync path; the
   tokio `Runtime` is dropped after `block_on` returns, which joins all
   `spawn_blocking` tasks.

## 4. Back-pressure model

Three regimes, distinguished by current active-connection count `A` and
cap `N`:

| `A` vs `N`             | Behaviour                                               |
| ---------------------- | ------------------------------------------------------- |
| `A < N`                | `try_acquire_owned()` returns `Ok(permit)` immediately. |
| `A == N` (saturated)   | Eager refuse: send upstream-compatible `@ERROR:`, close stream, return. The accept loop continues to drain the kernel backlog. |
| Sustained `A >= N`     | Listener backlog (`DEFAULT_LISTEN_BACKLOG = 128`, raisable) absorbs SYN; kernel SYN-cookies kick in beyond that. The TCP layer applies its own back-pressure. |

### 4.1 Eager refusal preserves upstream wire compatibility

`refuse_if_at_capacity()` in
`crates/daemon/src/daemon/sections/server_runtime/connection.rs:124`
writes
`@ERROR: max connections (N) reached -- try again later\n` and closes
the socket. The bridge MUST emit byte-identical output when the
semaphore reports `NoPermits` (see [[project_daemon_max_connections_v062]]
for the DMC-3 wire-format requirement).

Implementation: an async variant `refuse_at_capacity_async(stream:
tokio::net::TcpStream, cap: usize)` writes the same bytes via
`tokio::io::AsyncWriteExt::write_all` and drops the stream. No
`spawn_blocking` is needed because the message is < 100 bytes and the
write is fire-and-forget; if the client has already gone away we log
at debug and move on.

### 4.2 Why not block on `acquire()` and let accept() naturally back off?

Two reasons we choose eager refusal over awaiting a permit:

1. **Wire-format parity.** Upstream rsync refuses with `@ERROR:` rather
   than silently parking; mimicking that is a hard interop
   requirement.
2. **Avoids accept-loop deadlock.** If we `await` for a permit, the
   accept loop blocks on a future that only completes when an existing
   worker exits. Under spike arrivals the kernel backlog drains slowly
   and clients see protracted "connecting..." pauses with no error
   message. Eager refuse + kernel-level backlog is the upstream
   behaviour operators expect.

### 4.3 Listener backlog tunable

DASYNC.1 section 2.3 identified listener backlog as the first failure
mode under burst load. The bridge inherits the `listen backlog`
oc-rsyncd.conf knob from
`crates/daemon/src/daemon/sections/server_runtime/listener.rs:48-126`.
DASYNC.5 must publish bench data with backlog ranging from 128 (default)
to `SOMAXCONN` (4096 on stock Linux, tunable up to 65535).

## 5. Bridge to existing sync overlays

### 5.1 LSM capabilities (LSM-CAP, PR #5598)

Linux capability drop runs at process startup before the listener
binds. Unchanged by DASYNC; both the sync and async entry points hit
the same `bind_with_backlog` -> capability-drop sequence.

### 5.2 LSM seccomp (LSM-SECCOMP, PR #5589)

The sync path applies seccomp at worker fork (in the spawned thread's
prologue). In the tokio model, the worker is the `spawn_blocking`
closure, so the seccomp filter is installed at the top of the closure,
before `handle_session` runs. Same syscall surface either way;
filter rules require no change.

### 5.3 Landlock (feature `landlock` on `fast_io`)

Engaged inside the per-module transfer prologue (DASYNC.1 section 1.9
at `transfer.rs:217 engage_landlock_sandbox()`). Below the bridge
layer; no change.

### 5.4 `--max-connections` admission

Moves from per-thread `ConnectionGuard` (sync path,
DASYNC.1 section 1.10) to per-permit `OwnedSemaphorePermit` (async
path). The cap value resolution is unchanged: the daemon CLI flag or
oc-rsyncd.conf `max connections` directive populates
`RuntimeOptions::max_connections`, which is then used both for
(a) `Builder::max_blocking_threads(N)` and (b) the semaphore
constructor `Arc::new(Semaphore::new(N))`.

When `--max-connections` is absent or zero (the documented "unlimited"
sentinel), the semaphore is constructed with `Semaphore::MAX_PERMITS`
and the blocking pool is sized to a conservative default of 512 to
match tokio's stock behaviour. Operators wanting >512 connections must
set `--max-connections` explicitly; this matches the existing sync-path
contract that an unset cap means "operator opted out of admission
control".

### 5.6 PROXY protocol header

Already handled inside `handle_session` itself
(`session_runtime.rs:70-86`); not visible to the bridge.

### 5.7 Per-module connection cap

Enforced by `ConnectionLimiter` inside `handle_session` after the
module is selected. Unchanged.

## 6. Implementation order (informs DASYNC.3 sub-tasks)

The DASYNC.3 PR ladder lands the bridge incrementally so each step is
independently revertible:

1. **DASYNC.3.a - Bridge skeleton.** Replace the "drops streams"
   closure at `daemon.rs:411-418` with a real
   `spawn_blocking + handle_session` dispatch. Hardcoded
   semaphore size = `--max-connections` (default 1024 if unset for
   this milestone). No tests beyond the existing
   `binds_accepts_and_dispatches_worker` smoke test.

2. **DASYNC.3.b - Wire semaphore-based admission.** Move the
   semaphore acquisition before `into_std()`. Implement
   `refuse_at_capacity_async()` to mirror upstream's
   `@ERROR:` byte-for-byte. Add unit tests asserting wire-format
   equality with the sync path's refusal.

3. **DASYNC.3.c - Panic isolation.** Wrap `handle_session` in
   `catch_unwind` inside the blocking closure. Add a test that
   forces a panicking session and asserts the runtime keeps
   accepting subsequent connections.

4. **DASYNC.3.d - Pool sizing wired to cap.**
   `Builder::max_blocking_threads(N)` and `Builder::thread_stack_size`
   tuned per DASYNC.1 section 2.1 stack budget. Lock down the
   `Builder` construction in a single helper so DASYNC.5's bench
   harness can override per-run.

5. **DASYNC.5 - Bench at 1K / 5K / 10K concurrent connections.**
   Closes D10K-3 / D10K-4 / D10K-5. Steady-state + burst arrival, two
   transfer shapes (short ping, 1 MiB body) so we cover both the
   handshake-dominated and transfer-dominated regimes.

6. **DASYNC.4 - Documentation + feature-flag collapse decision.**
   Decide whether to keep `async = ...` and `async-daemon = ...`
   separate (DASYNC.1 open question 4.1) or collapse to one. The
   bench data informs the call.

Each step gates the next: 3.a must show no functional regression in
the existing sync-path test suite before 3.b lands.

## 7. Test plan

### 7.1 Unit (in `crates/daemon/src/async_listener.rs`)

- Existing `binds_accepts_and_dispatches_worker` proves accept ->
  spawn_blocking -> worker dispatch.
- New: `permit_release_on_worker_exit` - run a worker that returns
  immediately, assert the semaphore returns to `N` permits.
- New: `refuses_at_capacity_with_upstream_wire_bytes` - exhaust the
  permits, connect once more, assert the exact byte sequence
  `b"@ERROR: max connections (N) reached -- try again later\n"`.
- New: `panicking_worker_does_not_poison_runtime` - dispatch a worker
  that panics, then a worker that returns Ok, assert the second one
  ran.

### 7.2 Integration

Reuse the daemon test harness in `crates/daemon/tests/` against the
async entry point:

- `tests/admission_cap_async.rs` - exercise the `@ERROR:` refusal under
  N+1 concurrent connections.
- `tests/graceful_shutdown_async.rs` - assert in-flight workers
  complete after `shutdown.store(true)` and no permit is leaked.

### 7.3 Bench (DASYNC.5 driver, separate PR)

Reuses `docs/design/daemon-thread-per-conn-bench.md` driver. Adds an
`--async-accept` flag selecting the new entry point. Captures:

- p50/p99/p999 connection latency at N = 100, 1K, 5K, 10K.
- Steady-state vs burst (10K SYNs in 100 ms) arrival shapes.
- RSS / VA / FD count at peak.

DASYNC.5 rollout criterion: async path within 5% of sync at N = 1K and
strictly faster at N >= 5K. If async loses at N = 1K, see section 8.

## 8. Rollback criteria

The async path stays opt-in until all three hold:

1. **Throughput.** DASYNC.5 benches show parity or better than the
   sync path at every measured N, with no >5% regression at N = 1K
   (the operating point where most operators live today).
2. **Wire equivalence.** The interop matrix
   (`tools/ci/run_interop.sh`) passes against rsync 3.0.9 / 3.1.3 /
   3.4.1 / 3.4.2 / 3.4.4 with the async entry point selected.
3. **Stability.** Two release cycles of green CI with the async
   feature opted-in via `--features async-daemon`.

Failure modes that hold the rollback:

- **Async accept worse than sync at N < 2K**: keep
  `daemon-async-accept` opt-in. Document the cross-over point in the
  user manual and do not flip the default.
- **TLS handshake latency regression**: per section 5.1 the handshake
  runs in `spawn_blocking`; if DASYNC.5 measures handshake-induced
  head-of-line blocking, document and stay opt-in. The follow-up is
  `tokio_rustls` async handshake, tracked as a separate task.
- **Tokio runtime panic** observed under fuzz or stress: stay opt-in
  pending root-cause and a regression test.

The opt-out path is always the existing sync `serve_connections`
entry; the feature flag gates only the new code, so removing
`--features async-daemon` returns the binary to the pre-DASYNC
behaviour byte-for-byte.

## 9. Risks and open questions

### 9.1 `spawn_blocking` overhead at N = 1K

Per DASYNC.1 open question 4.3, the channel-hop cost of
`spawn_blocking` is ~5 - 20 µs per dispatch. At N = 1K with multi-
second transfers this is invisible; for protocol-only pings it could
be a measurable fraction of total latency. DASYNC.5 bench includes a
ping-only arrival shape specifically to measure this.

### 9.2 Blocking-pool size vs cap interaction

If `--max-connections = 10000`, tokio's blocking pool needs 10K threads
- the same stack-VA pressure DASYNC.1 section 2.1 documents for the
sync path. The async model does not eliminate per-connection thread
cost; it eliminates per-connection thread *creation* cost (the pool is
pre-warmed) and the accept-loop serialisation cost. If the operator's
goal is purely "reduce RSS at 10K idle connections" the answer is
still "use a smaller cap"; if the goal is "smoother burst absorption",
the async path wins.

### 9.3 Single-feature vs dual-feature flag

DASYNC.1 open question 4.1 left this open: `async` (already pulls
tokio for `core`) vs `async-daemon` (gates the listener). DASYNC.4 is
the formal decision point; bench data informs whether collapsing the
two flags introduces a runtime cost on the sync path (it should not,
because `async` only adds a dependency, not a code path).

### 9.4 Dual-stack accept

The sync path runs two listener threads (IPv4 + IPv6) feeding an mpsc
channel (DASYNC.1 section 1.3). The async equivalent is one tokio
`select!` over two `TcpListener::accept` futures, eliminating the
channel hop. DASYNC.3 lands single-listener first; dual-stack is a
follow-up that does not change the bridge contract specified here.

## 10. Cross-references

Project memory anchors:

- [[project_daemon_10k_conn_ceiling]] - the saturation problem this
  design solves.
- [[project_no_async_threaded_only]] - establishes that the data path
  stays sync; only the accept loop becomes async.
- [[project_russh_spawn_blocking_ceiling]] - analogous bridge pattern
  on the russh side; same `spawn_blocking` primitive, same scaling
  ceiling.
- [[project_daemon_max_connections_v062]] - DMC series; the upstream-
  compatible `@ERROR:` refusal the bridge must preserve.

Design docs:

- `docs/design/dasync-1-daemon-accept-loop-inventory.md` - the audit
  this design builds on.
- `docs/design/daemon-async-accept-sync-workers.md` - long-form design
  this document operationalises.
- `docs/design/daemon-async-runtime-choice.md` - runtime selection ADR;
  trigger conditions for production rollout.
- `docs/design/daemon-tokio-async-listener-impl.md` - implementation
  plan partially realised in `async_listener.rs`.
- `docs/design/daemon-thread-per-conn-bench.md` - D10K-2 baseline; the
  DASYNC.5 bench harness reuses the driver.

Source anchors:

- `crates/daemon/src/async_listener.rs:73` - `run_hybrid_listener()`
  skeleton.
- `crates/daemon/src/async_listener.rs:90` - `accept_loop()` body.
- `crates/daemon/src/daemon.rs:370` - `run_async_daemon()` entry.
- `crates/daemon/src/daemon.rs:411` - the "drops streams" closure
  DASYNC.3 replaces.
- `crates/daemon/src/daemon/sections/session_runtime.rs:44` -
  `handle_session()` the bridge dispatches to.
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:124`
  - `refuse_if_at_capacity()` whose `@ERROR:` output the async refusal
  mirrors.
- `crates/daemon/src/daemon_stream.rs:77` - `DaemonStream::plain()`
  used inside the blocking closure.

Upstream rsync 3.4.4 anchors:

- `clientserver.c:752` - upstream `@ERROR: max connections` wire
  output that the async refusal preserves byte-for-byte.
- `socket.c:537 start_accept_loop()` - upstream TCP accept loop that
  forks per connection; the async path replaces fork with
  `spawn_blocking` over a pre-sized blocking pool.
