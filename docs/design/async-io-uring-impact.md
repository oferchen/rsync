# Async Runtime Impact on io_uring Integration (#1595)

Tracking issue: #1595.

Related design notes:

- `docs/design/io-uring-rayon-composition.md` (#1283/#1284) - rayon-side
  composition policy for the native io_uring path.
- `docs/design/tokio-spawn-blocking-rayon.md` (#1751) - bridge between
  the async daemon and rayon CPU work.
- `docs/design/async-migration-plan.md`,
  `docs/design/daemon-tokio-async-listener-impl.md` (#1934/#1935) -
  async daemon runtime this evaluation must compose with.

## 1. Question

Does an async runtime (`tokio`) compose well with io_uring on Linux 5.6+?
Two integration paths are on the table:

- **Path A: `tokio-uring` crate.** A tokio-native runtime where io_uring
  is the I/O reactor; futures yield on completion events.
- **Path B: native `fast_io::io_uring` + `tokio::task::spawn_blocking`.**
  Keep io_uring driven from rayon/sync code, bridge to async via the
  blocking pool when called from an async daemon task.

## 2. `tokio-uring`: Tokio-Native, Separate Runtime

`tokio-uring` re-uses tokio's API surface (`spawn`, `JoinHandle`) but
runs a dedicated **single-threaded** ring driver entered via
`tokio_uring::start`. Constraints that disqualify it for oc-rsync:

- Single-threaded by design; the multi-threaded `tokio::main` runtime
  the async daemon (#1934) uses cannot host it. Mixing the two needs
  `tokio_uring::start` on a dedicated OS thread, recreating path B
  with extra ceremony.
- Futures from `tokio-uring` cannot be awaited from a multi-threaded
  `tokio` worker without `LocalSet` and manual driving. The daemon
  listener and accept loops are explicitly multi-threaded.
- Buffer ownership uses the crate's `BufResult<T, B>` shape, forcing
  every `fast_io` call site to surrender and reclaim buffers around
  each `await`.
- No exposure of `IORING_REGISTER_BUFFERS`, SQPOLL, PBUF_RING
  (`IORING_REGISTER_PBUF_RING`), or fixed-fd registration - features
  `fast_io::io_uring` already wires
  (`registered_buffers.rs`, `config.rs`, `buffer_ring.rs`). Adopting
  it would regress those.

## 3. Native io_uring + `spawn_blocking`: Proven Path

Path B keeps the existing `fast_io::io_uring` infrastructure. From an
async daemon task:

```text
async fn daemon_task(...)
    -> tokio::task::spawn_blocking(move || {
           // sync code path: rayon worker pool + session ring pool
           // (#1409) submits SQEs, blocks on submit_and_wait or the
           // model-B dispatcher (#1284).
       })
       .await
```

The blocking pool soaks I/O wait so async workers stay free, exactly
the role tokio's blocking pool is sized for. SQPOLL or the userspace
reaper from #1284 still drives completions; submit batching, registered
buffers, and PBUF_RING continue to apply. No new abstraction layer.

CLI mode does not enter this bridge - it has no tokio runtime and
calls `fast_io::io_uring` directly from rayon workers per the
composition design.

## 4. Cross-Reference: #1751 spawn_blocking Bridge

`docs/design/tokio-spawn-blocking-rayon.md` (#1751) already specifies
the bridge for rayon CPU work: any rayon-driven parallel job invoked
from an async task runs under `tokio::task::spawn_blocking`. The same
bridge covers io_uring submission because rayon workers are the
issuers under #1284. Daemon code that needs a result `await`s the
join handle; CLI code never enters the bridge.

The single existing user
(`crates/engine/src/async_io/copier.rs:184`) and the two planned sites
(`transfer/src/receiver/directory/{creation,deletion}.rs`) all fit the
shape: blocking sync work, await on the join handle, no nested
runtimes.

## 5. Recommendation

Stay on native `fast_io::io_uring`. Drive it from sync code (rayon
workers under the model-B dispatcher from #1284). When called from the
async daemon, bridge through `tokio::task::spawn_blocking` per #1751.
Do not adopt the `tokio-uring` crate: it forces a single-threaded
runtime context that conflicts with the multi-threaded daemon, drops
features oc-rsync already depends on (registered buffers, SQPOLL,
PBUF_RING), and offers no measurable upside over the spawn_blocking
bridge for our workload (disk I/O, not high-fan-out socket I/O).

This decision is wire-compat-neutral and platform-neutral: non-Linux
targets continue to use the synchronous `fast_io` fallbacks unchanged.
