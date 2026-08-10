# Audit: `Arc<Mutex<Vec<...>>>` contention sites under parallel-stat workloads

Last verified: 2026-05-07 against `crates/rsync_io/src/ssh/aux_channel.rs`,
`crates/engine/src/concurrent_delta/work_queue/drain.rs`,
`crates/engine/src/local_copy/buffer_pool/pool.rs`,
`crates/transfer/src/pipeline/spsc.rs`,
`crates/transfer/src/receiver/transfer/pipeline.rs`,
`crates/flist/src/parallel.rs`,
`crates/flist/src/batched_stat/{cache,dir_stat}.rs`,
`crates/transfer/src/parallel_io.rs`, and tracking issue #1192.

## Scope

Catalog every production-code `Arc<Mutex<Vec<_>>>` (and bare `Mutex<Vec<_>>`)
site reachable on the parallel-stat / file-list build / receiver pipeline
hot paths, classify each by lock-hold duration and call site, and recommend
lock-free alternatives where the workload exceeds 100k files. Test-only
recorders (`tests.rs`, `examples/`, `benches/baseline`) are listed for
completeness but excluded from the contention discussion.

## Inventory

`grep -rn "Arc<Mutex<Vec\|Mutex<Vec\|Mutex::new(Vec" crates/` returns the
following sites in production source:

| Site | Role | Hot path? | Lock granularity |
|------|------|-----------|------------------|
| `crates/rsync_io/src/ssh/aux_channel.rs:99` (`PipeStderrChannel.buffer`) | Bounded SSH-stderr capture | No - one drainer thread per SSH child, single appender, sporadic snapshot reads | `Vec<u8>` push of a single chunk; cap 64 KiB |
| `crates/rsync_io/src/ssh/aux_channel.rs:148` (`SocketpairStderrChannel.buffer`) | Same as above, Unix socketpair variant | No - single drain thread, capped buffer | Identical pattern |
| `crates/engine/src/concurrent_delta/work_queue/drain.rs:63` (sharded `Mutex<Vec<R>>` per rayon thread) | `WorkQueueReceiver::drain_parallel` collector | Yes - executes inside `rayon::scope` for delta work items | Per-shard `Vec::push` of one result per work item |

Test-only or non-shipping uses (excluded from contention analysis):

- `crates/transfer/src/{reader,writer}/tests.rs` recorder buffers.
- `crates/protocol/src/multiplex/reader.rs:339` and `examples/mplex_usage.rs:43` in tests/examples.
- `crates/core/src/signal/cleanup.rs:304` cleanup-order assertion test.
- `crates/bandwidth/src/limiter/test_support.rs:16` recorded-sleep harness.
- `crates/engine/benches/buffer_pool_benchmark.rs:30` baseline-against-`ArrayQueue` micro-benchmark; not used at runtime.

The codebase already migrated the central buffer pool away from
`Mutex<Vec<Vec<u8>>>` to `crossbeam_queue::ArrayQueue` plus a thread-local
single-slot cache (`crates/engine/src/local_copy/buffer_pool/pool.rs:96`),
and the network-to-disk pipeline uses an `ArrayQueue`-based SPSC
(`crates/transfer/src/pipeline/spsc.rs:18`). Both serve as templates for
the recommendations below.

## Per-site analysis

### 1. SSH stderr capture (`PipeStderrChannel`, `SocketpairStderrChannel`)

`spawn()` allocates an `Arc<Mutex<Vec<u8>>>`, hands one clone to a dedicated
drain thread, and exposes `collected()` to the parent. Lock acquisitions:

- Drain thread - one `lock()` per `read()` chunk in `drain_loop` via
  `append_bounded()` (`aux_channel.rs:232`). Worst case is a few KiB per
  chunk and the buffer is capped at `STDERR_BUFFER_CAP = 64 * 1024`, so the
  thread holds the lock for nanoseconds and only when the SSH child has
  emitted diagnostic output.
- Parent - `snapshot()` clones the current bytes on demand (error paths,
  `Drop` surfacing). Called at most a handful of times per SSH session.

**Hot path?** No. The SSH transport handles a single subprocess per
session and the stderr stream is intentionally low-volume. There is no
per-file lock acquisition.

**Recommendation:** keep as-is. Replacing this with a lock-free structure
buys nothing measurable and the bounded-buffer logic (`append_bounded`
truncates from the front when over-cap) is easier to reason about under a
mutex than under an MPSC ring.

### 2. `WorkQueueReceiver::drain_parallel` shards

`drain_parallel` builds `num_shards = rayon::current_num_threads()` mutex-
guarded vectors and dispatches each rayon task to the shard indexed by
`rayon::current_thread_index()`. This is exactly the per-thread sharding
pattern recommended for high-fanout collectors and was introduced as a
deliberate replacement for a single shared `Mutex<Vec<R>>`.

Lock-hold duration is bounded by one `Vec::push` per work item; the
critical section is identical to a `crossbeam_queue::ArrayQueue::push` but
without the bounded-capacity panic risk (rayon work items are unbounded).

**Hot path?** Yes during delta computation, but contention is structurally
near zero: only the rare cross-thread case (when a rayon task migrates
mid-execution and `current_thread_index()` returns a different shard) can
collide, and even then the collision is on a per-shard mutex sized at the
hardware thread count.

**Recommendation:** keep the sharded `Mutex<Vec<R>>` design. An
`ArrayQueue<R>` would require a fixed capacity, which is unknown for
`drain_parallel` (item count is determined by the producer side of the
work queue). A thread-local `Vec` collected via rayon's `fold`/`reduce`
is an alternative but the current implementation is already O(num_shards)
locks held for nanoseconds each, well below the perf floor that would
justify churn.

If contention is later observed in `perf lock-stat`, the next step is to
replace the inner `Vec::push` with `crossbeam_queue::SegQueue<R>` per
shard (unbounded, lock-free). Document any change with a reproducible
benchmark on a 100k-file delta workload.

## Highest-contention candidates under parallel stat (100k+ files)

The `Arc<Mutex<Vec>>` inventory does **not** include the file-list build
or batched-stat parallel paths. The five `par_iter`/`into_par_iter`
sites in `crates/flist/src/parallel.rs`, `crates/flist/src/batched_stat/`,
and `crates/transfer/src/parallel_io.rs` use `par_iter().map(f).collect()`,
which delegates collection to rayon's lock-free reducer (split-and-merge
into per-thread `Vec`s, joined via `extend`). They never share a single
mutex-guarded `Vec` across rayon workers.

Likewise the receiver pipeline's signature pre-fetch
(`crates/transfer/src/receiver/transfer/pipeline.rs:184`) collects via
`par_iter().map().collect()` - rayon's reducer, not a shared mutex.

**Conclusion:** under a 100k-file parallel-stat workload there is no
production `Arc<Mutex<Vec<_>>>` site on the hot path. The two
production sites (SSH stderr and delta-work shards) are either off the
hot path entirely or already sharded to eliminate contention. Engineering
effort targeted at parallel-stat throughput should look elsewhere
(allocator pressure on `PathBuf`, `getdents`/`stat` syscall batching,
file-list sort, see `docs/audits/profiling-100k-files.md`).

If a future change reintroduces a shared collector under `par_iter`, the
recommendations below apply.

## Recommended lock-free alternatives by use case

| Use case | Recommended structure | Rationale |
|----------|----------------------|-----------|
| Bounded fan-in of typed records (known max items) | `crossbeam_queue::ArrayQueue<T>` | Already used for `BufferPool` and `pipeline::spsc::Channel`; lock-free push/pop, fixed capacity matches the parallel-stat batch size |
| Unbounded fan-in (item count unknown a priori) | `crossbeam_queue::SegQueue<T>` | Lock-free, unbounded, slightly higher per-op cost than `ArrayQueue` but no capacity to size |
| Per-key aggregation (filename to metadata, basis-file lookup) | `dashmap::DashMap<K, V>` | Sharded internally; safe to mutate concurrently from rayon workers; preferred over `Mutex<HashMap>` for any shared map under `par_iter` |
| Per-thread collection followed by ordered merge | `Vec<Vec<T>>` indexed by `rayon::current_thread_index()`, flattened after `rayon::scope` | Matches `drain_parallel` pattern; zero contention in steady state |
| Order-preserving collector (file-list slot 1:1 mapping) | `par_iter().map(f).collect::<Vec<_>>()` | Rayon's built-in reducer; preserves input order; never mutex-guarded |
| Counter / accumulator | `std::sync::atomic::AtomicU64` (or `AtomicUsize`) | For totals like `delete_stats.files`; cheaper than any mutex-protected integer |

Avoid `Arc<Mutex<Vec<T>>>` as a shared collector inside a rayon task. The
two acceptable patterns are (a) per-shard mutexed `Vec` sized to
`rayon::current_num_threads()` and (b) lock-free queues from
`crossbeam_queue`.

## Methodology for measuring contention

Run inside the `rsync-profile` podman container against a 100k-file
fixture (see `scripts/benchmark_100k.sh`) so the host workspace and
upstream-rsync interop daemons are pre-staged.

### 1. Linux `perf lock-stat`

```sh
sudo sysctl -w kernel.lock_stat=1   # enable kernel lock-stat
sudo perf record -e lock:contention_begin -e lock:contention_end \
    --call-graph dwarf -- target/release/oc-rsync \
    -a /workspace/fixture/100k /workspace/dst/
sudo perf script | scripts/perf_lock_summarize.py | head -40
```

Look for `Mutex::lock` frames inside `aux_channel`, `drain_parallel`, or
any future shared collector. A contention rate above ~0.1% of wall time
is worth investigating; below that the lock is statistically uncontended.

For a kernel-level view (futex sleeps), use `perf trace -e 'syscalls:sys_enter_futex'`
to confirm the userspace `Mutex` never blocks the rayon workers.

### 2. macOS `dtruss` and `dtrace` lockstat probes

```sh
sudo dtrace -n 'plockstat$target:::mutex-block { @[ustack()] = count(); }' \
    -c "target/release/oc-rsync -a fixture/100k dst/"
```

`plockstat` requires the binary be built without `LDFLAGS=-Wl,-no_pie`;
the workspace `Cargo.toml` does not set it. The output is a flame-graph-
ready ustack histogram of mutex blocks.

### 3. `parking_lot::deadlock` detection (debug builds)

The codebase uses `std::sync::Mutex`, but a temporary swap to
`parking_lot::Mutex` with `parking_lot::deadlock::check_deadlock()`
running on a sidecar thread surfaces deadlocks and contention hotspots
during integration tests. Useful when adding a new shared collector and
unsure whether it can deadlock with another lock taken inside rayon.

```rust
#[cfg(feature = "deadlock-detection")]
fn install_deadlock_detector() {
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        for set in parking_lot::deadlock::check_deadlock() {
            for thread in set {
                eprintln!("deadlock thread: {:?}\n{:?}", thread.thread_id(), thread.backtrace());
            }
        }
    });
}
```

Gate behind a non-default Cargo feature so it never ships in release.

### 4. Synthetic benchmark

`crates/engine/benches/buffer_pool_benchmark.rs` already compares
`Mutex<Vec<Vec<u8>>>` against `ArrayQueue`-backed pools at 1/4/8/16
threads. Use the same harness shape for any new collector under
suspicion: emit a `criterion` benchmark that drives 100k push operations
from N rayon workers, compare against the lock-free alternative on the
same shape, and only land the change if the speedup exceeds noise.

### 5. Wall-clock A/B against the 100k fixture

End-to-end is the final gate. Run the suspect workload three times
before and after the change with `hyperfine`:

```sh
hyperfine --warmup 1 --runs 5 \
    'target/release/oc-rsync.before -a fixture/100k dst/' \
    'target/release/oc-rsync.after  -a fixture/100k dst/'
```

A change that only moves contention from one mutex to another rarely
shows up here; a real reduction in lock-related stalls does.

## Action items

1. Keep the SSH stderr `Arc<Mutex<Vec<u8>>>` sites as-is - off the hot path.
2. Keep `drain_parallel`'s sharded `Mutex<Vec<R>>` design - already at the
   contention floor for unbounded collectors.
3. Add a regression note in any future PR that introduces a new shared
   collector reachable from `par_iter`/`rayon::scope`: prefer the patterns
   in the table above and demonstrate equivalence with one of the
   measurement methods.
4. Cross-reference this audit from `docs/audits/profiling-100k-files.md`
   the next time that document is revised, so future investigators do not
   re-derive the parallel-stat contention picture.

## References

- `crates/rsync_io/src/ssh/aux_channel.rs` - SSH stderr drain (sites 1-2).
- `crates/engine/src/concurrent_delta/work_queue/drain.rs` - sharded
  drain (site 3).
- `crates/engine/src/local_copy/buffer_pool/pool.rs` - `ArrayQueue`-
  backed buffer pool (template for lock-free fan-in).
- `crates/transfer/src/pipeline/spsc.rs` - `ArrayQueue`-backed SPSC
  (template for network-to-disk handoff).
- `crates/flist/src/parallel.rs`, `crates/flist/src/batched_stat/`,
  `crates/transfer/src/parallel_io.rs` - parallel-stat sites that use
  `par_iter().collect()` and never share a mutex.
- `docs/audits/profiling-100k-files.md` - companion audit on per-file
  fixed-cost hot spots in the same workload.
- Issue #1192 - tracking item that motivated this audit.
