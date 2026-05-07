# Session Ring Pool - Implementation Plan (#1937)

## Status

- #1408: design accepted in `docs/design/iouring-session-ring-pool.md`.
- #1409: partial implementation landed (`RingPool`, `RingLease`, init
  path, file_writer migration). Sockets, `disk_batch`, and
  `copy_file_range` still construct private rings. Pool is owned at
  `IoUringConfig` granularity, not at session granularity.
- #1936: companion design issue defining the per-session lookup
  contract that this plan binds to (see citation below).
- #1937: this document - the concrete plan for finishing the work as a
  per-session ring pool keyed by `SessionId`.

## Goals

- One `RingPool` per active session, not one per process or per config.
- Pool lookup keyed by `daemon::session_registry::SessionId`.
- Pool lifetime tied to the owning session value: drop the session,
  drop its rings, free SQPOLL kthreads and eventfds deterministically.
- Single-binary path: CLI sessions register a synthetic `SessionId`
  via `SessionRegistry` so they share one code path with the daemon.

## Lookup by SessionId

A new module `crates/fast_io/src/io_uring/session_pool.rs` exposes:

```rust
pub struct SessionRingPools { /* DashMap<SessionId, Arc<RingPool>> */ }
impl SessionRingPools {
    pub fn get_or_init(&self, id: SessionId, cfg: &IoUringConfig) -> Arc<RingPool>;
    pub fn remove(&self, id: SessionId) -> Option<Arc<RingPool>>;
}
```

- One `SessionRingPools` instance per process, held by the daemon
  runtime (alongside `SessionRegistry`) and by the CLI runtime.
- `get_or_init` constructs lazily on first I/O, reusing the
  `RingPool::try_new` fallback path so non-Linux and unprivileged
  hosts degrade to private/std-io rings.
- `RingPool` retains its existing `lease()` API; per-session lookup
  is the only new dispatch step. No change to `RingLease` semantics.
- Callers thread `SessionId` through `IoUringConfig` (new
  `session: Option<SessionId>` field). When `None`, behaviour matches
  today's per-object construction - keeps tests and `copy_file_range`
  ad-hoc paths unaffected.

## Lifecycle Tied To Session Drop

- `SessionRegistry::unregister` (already the single removal point)
  gains a hook: after removing the `SessionInfo`, call
  `SessionRingPools::remove(id)`. The returned `Arc<RingPool>` drops
  when the last outstanding `RingLease` returns, releasing rings,
  registered buffers, and registered fds in one place.
- For the daemon path, `unregister` already runs in
  `AsyncSession::Drop`, so ring teardown follows the connection
  without extra wiring.
- For CLI runs, the synthetic session is unregistered in
  `core::session()`'s teardown, mirroring daemon semantics.
- Reaper sweeps in `SessionRegistry` (`cleanup_stale`,
  `cleanup_completed`) call the same hook; no orphan rings.

## Migration Steps

1. Land `session_pool.rs` with `SessionRingPools`, `get_or_init`,
   `remove`, and unit tests covering eviction, contention, and
   `EMFILE` fallback (mirrors `ring_pool.rs` test layout).
2. Thread `SessionId` through `IoUringConfig`; default `None` keeps
   call sites unchanged.
3. Wire the `unregister` hook in `daemon/session_registry.rs` and the
   CLI teardown in `core::session()`.
4. Migrate sockets (`socket_reader.rs`, `socket_writer.rs`),
   `disk_batch.rs`, and `copy_file_range.rs` to lease via
   `SessionRingPools` when a `SessionId` is present; private ring
   stays as the fallback.
5. Remove the per-object construction path once two consecutive
   nightly interop and benchmark runs are green.

## Pitfalls

- Buffer-group registration (`RegisteredBufferGroup`) is ring-scoped;
  per-session pools must register at `get_or_init` time.
- `try_register_fd` slot tables are still ring-scoped; reuse the
  recycle helper from #1409.
- Synthetic CLI session IDs must not collide with daemon IDs; the
  registry's monotonic counter already covers this.

## References

- `docs/design/iouring-session-ring-pool.md` - #1408 / #1409 design.
- `docs/design/io-uring-rayon-composition.md` - shared-ring invariants.
- #1936 - per-session lookup contract this plan binds to.
- `crates/daemon/src/daemon/session_registry.rs:17` - `SessionId`.
- `crates/fast_io/src/io_uring/config.rs:381` - `build_ring`.
- `crates/fast_io/src/io_uring/file_writer.rs:54` - first migration site.
