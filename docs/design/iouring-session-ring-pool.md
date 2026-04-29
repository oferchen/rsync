# Session-Level io_uring Ring Pool (#1409)

## Summary

Today every io_uring-backed reader/writer constructs its own `RawIoUring`
via `IoUringConfig::build_ring()`. With concurrent transfers in a single
oc-rsync session this produces N rings per worker, wasting kernel
descriptors and SQPOLL kthreads. This design introduces a `RingPool`
owned by the session: a small fixed pool (default = `min(num_cpus, 4)`)
of pre-built rings handed out via a lease/return API, defaulting to
round-robin assignment with a free-list fast path.

## Current State

Ring construction happens at the per-object lifetime - one ring per
file or socket I/O object. Each ring lives only as long as that object.
Citations:

- `crates/fast_io/src/io_uring/config.rs:381` - `IoUringConfig::build_ring()`,
  the single construction primitive (wraps `io_uring::IoUring::builder`).
- `crates/fast_io/src/io_uring/file_writer.rs:54,81,141` - per-file ring
  creation in three constructors.
- `crates/fast_io/src/io_uring/file_reader.rs:60` - per-reader ring.
- `crates/fast_io/src/io_uring/socket_reader.rs:32` and
  `crates/fast_io/src/io_uring/socket_writer.rs:32` - per-socket rings.
- `crates/fast_io/src/io_uring/disk_batch.rs:71` - per-batch ring.
- `crates/fast_io/src/copy_file_range.rs:159` - per-call ring.
- `crates/fast_io/src/io_uring/mod.rs:151,174` - probe/fallback paths.

Lifetime today is per-I/O-object; there is no session-level cache.

## Design

The pool lives in a new `crates/fast_io/src/io_uring/ring_pool.rs`.
Shape:

```rust
pub struct RingPool { rings: Vec<Mutex<RawIoUring>>, cursor: AtomicUsize }
pub struct RingLease<'a> { /* &mut RawIoUring guard, returns on drop */ }
```

Public API: `RingPool::new(config: &IoUringConfig, count: usize)`,
`pool.lease() -> RingLease<'_>`, automatic return on `Drop`.

Ring count heuristic: `count = config.ring_pool_size.unwrap_or_else(||
num_cpus::get().min(4).max(1))`. Four covers the common SMT-4/8 laptop
and small server cases without exhausting fd quota; CPU count caps it
on bigger boxes. Each ring is sized using the existing
`IoUringConfig::sq_entries` so per-ring queue depth is unchanged.

Thread-safety: rings are `!Sync` in the upstream crate, so each slot
sits behind a `Mutex<RawIoUring>`. `lease()` walks slots starting at
`cursor.fetch_add(1)` and returns the first `try_lock()` success; if all
slots are busy it blocks on the round-robin slot. This gives MPMC
semantics without per-call allocation. Note that current per-object
rings are SPSC by construction (one owner submits and waits); shared
pool rings become MPMC across workers, so the lease guard must hold the
mutex for the entire submit-and-wait cycle to preserve ordering and
keep `CompletionQueue` reads single-consumer.

## Pitfalls

- Registered-buffer scope: `RegisteredBufferGroup` (file_writer.rs:56)
  is registered against one specific ring and cannot migrate. Pooled
  rings must register their buffer set once at pool init, or the
  per-lease path must fall back to an unregistered buffer.
- File-fd registration (`try_register_fd`) is also ring-scoped. With a
  shared ring the slot table fills quickly; the pool must expose a
  `register_fd_in_lease` helper that reuses or recycles slots, or skip
  registration when slot pressure exceeds a threshold.
- Kernel fd limits: even four rings per session multiply the eventfd
  and SQPOLL kthread cost. Honour `RLIMIT_NOFILE` and degrade to a
  pool of 1 if `build_ring()` returns `EMFILE`.
- Fallback when io_uring is unavailable: `RingPool::try_new` returns
  `Option<Self>`; callers keep the existing scalar/std-io paths
  (`mod.rs:151`) when `None`.

## Implementation Steps

1. Add `ring_pool.rs` with `RingPool`, `RingLease`, and unit tests
   covering lease/return, contention fairness, and `EMFILE` fallback.
2. Wire `RingPool` into `IoUringConfig` (new optional `pool: Option<
   Arc<RingPool>>` field) without changing existing call sites.
3. Migrate `file_writer.rs` and `file_reader.rs` to lease from the pool
   when present, else build a private ring (preserves current behaviour).
4. Migrate sockets and `disk_batch` last, gated behind a
   `--io-uring-ring-pool` flag during a stabilisation window.
5. Remove the per-object construction path once interop and benchmark
   workflows are green for two consecutive nightly runs.

## References

- `crates/fast_io/src/io_uring/config.rs:381` (`build_ring`)
- `crates/fast_io/src/io_uring/file_writer.rs:54`
- `crates/fast_io/src/io_uring/file_reader.rs:60`
- `crates/fast_io/src/io_uring/socket_reader.rs:32`
- `crates/fast_io/src/io_uring/socket_writer.rs:32`
- `crates/fast_io/src/io_uring/disk_batch.rs:71`
- `crates/fast_io/src/copy_file_range.rs:159`
- `crates/fast_io/src/io_uring/mod.rs:151`
