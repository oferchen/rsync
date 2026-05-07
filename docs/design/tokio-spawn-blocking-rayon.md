# `tokio::spawn_blocking` Bridge for Rayon CPU Work in Async Daemon

Tracking issue: #1751.

## 1. Context

The async daemon work tracked in #1934/#1935 moves the daemon listener,
per-connection accept loop, and protocol I/O onto a `tokio` runtime so a single
process can multiplex many concurrent transfers without one OS thread per
session. The rest of the engine still performs CPU-bound work on the global
`rayon` thread pool:

- Rolling + strong checksum batches (`crates/checksums/src/rolling/parallel.rs`).
- Delta apply / block-match scheduling (`crates/engine/src/delta/`).
- Parallel `lstat` and metadata application during file-list build
  (`crates/transfer/src/receiver/parallel.rs`, `PARALLEL_STAT_THRESHOLD = 64`).
- Parallel directory metadata fan-out after `create_dir_all`.

Both pools coexist: `tokio` drives I/O futures, `rayon` drives data-parallel CPU.

## 2. Problem

A naive call site that invokes `rayon::par_iter` directly from an `async fn`
running on a `tokio` worker blocks that worker for the entire parallel job.
`tokio`'s default multi-thread runtime has a small fixed worker count
(`num_cpus`), so even a few stalled workers starve the listener accept loop and
multiplex frame readers. Symptoms: rising connection-accept latency, MSG_DATA
back-pressure stalls, and missed keepalive deadlines under load.

`rayon` itself does not yield to any external scheduler; once a worker enters a
`par_iter` it runs to completion. The async daemon must explicitly bridge.

## 3. Pattern: `spawn_blocking` + rayon

Wrap each rayon entry point in `tokio::task::spawn_blocking` and `.await` the
join handle:

```rust
let result = tokio::task::spawn_blocking(move || {
    // Runs on a tokio blocking thread; free to drive rayon to completion.
    rayon_dispatched_checksum_batch(blocks, strong)
})
.await
.map_err(TransferError::from_join)?;
```

`spawn_blocking` moves the closure to tokio's blocking thread pool (default cap
512), releasing the async worker immediately. The blocking thread submits the
rayon job and parks on its join, while async workers stay free for I/O and
multiplex framing. Panics surface as `JoinError::is_panic()` and are mapped to
`ExitCode::PROTOCOL` with a `[server]` role trailer.

## 4. Alternative: dedicated rayon pool + `block_in_place`

Two variants worth comparing:

- **Dedicated rayon `ThreadPool`.** Build one `rayon::ThreadPoolBuilder`
  instance scoped to the daemon, sized to `num_cpus - tokio_workers` to avoid
  oversubscription. Submit jobs via `pool.install(|| par_iter(...))` from
  `spawn_blocking`. Eliminates contention with the global rayon pool used by
  CLI runs in the same process.
- **`tokio::task::block_in_place`.** Marks the current async worker as blocking
  so tokio promotes a sibling worker thread before the rayon call runs in
  place. Saves one thread hop versus `spawn_blocking`, but only works on the
  multi-thread runtime, cannot be used from `current_thread`, and cannot be
  composed with `select!`/`join!` arms that need true cancellation.

Combining the two-`block_in_place` inside an installed dedicated rayon pool-is
the best worker-pool-aware path when avoiding context switches matters.

## 5. Risks

- **Double-spawn cost.** Each call hops async worker -> tokio blocking thread
  -> rayon worker. For sub-microsecond batches the hop dominates; gate the
  bridge behind the same thresholds rayon already uses
  (`PARALLEL_STAT_THRESHOLD`, block-count minima in checksums).
- **Context-switch overhead.** The blocking thread parks on the rayon join,
  consuming a thread slot for the duration. Bound concurrent bridges via a
  semaphore so we cannot exhaust the 512-thread blocking pool under fan-out.
- **Panic propagation.** `JoinError` distinguishes panic vs cancellation;
  rayon panics propagate through `install`. Both must map to upstream exit
  codes (`ExitCode::PROTOCOL` / `ExitCode::PARTIAL`) with `[server]` trailer.
- **Cancellation.** `spawn_blocking` futures cannot be cancelled; dropping the
  join handle leaves the rayon job running. Use cooperative cancellation
  tokens checked between rayon batches.
- **Oversubscription.** Without a dedicated rayon pool, rayon's global pool
  can spawn `num_cpus` workers on top of tokio's `num_cpus` workers, doubling
  contention. Pin rayon to a smaller pool when the daemon owns the runtime.

## 6. Recommendation

Add a thin wrapper helper in `crates/transfer/src/async_compat.rs`:

```rust
/// Bridge a rayon-dispatched CPU job onto the async runtime without stalling
/// the tokio worker. Falls back to a direct call when the workload is below
/// the bridge threshold.
pub async fn rayon_bridge<F, T>(min_units: usize, units: usize, job: F) -> Result<T, TransferError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    if units < min_units {
        return Ok(job());
    }
    tokio::task::spawn_blocking(job)
        .await
        .map_err(TransferError::from_join)
}
```

- Single entry point keeps the bridge policy in one module (Single
  Responsibility) and lets us swap implementations (Strategy: `spawn_blocking`
  vs `block_in_place` + dedicated pool) without touching call sites.
- Threshold short-circuit avoids the bridge cost on small batches.
- All current rayon call sites in `crates/checksums`, `crates/engine`, and
  `crates/transfer` migrate to `rayon_bridge` when reached from an async
  context; sync call sites (CLI) keep calling rayon directly.
- Tests: unit test the threshold short-circuit, panic-to-`JoinError` mapping,
  and a tokio `current_thread` runtime smoke test that verifies the listener
  accept loop progresses while a long rayon job runs.
