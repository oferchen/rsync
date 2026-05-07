# Shared io_uring Instance Across Concurrent Transfers

Tracking issue: #1060.

## 1. Relationship to Prior Work

Issues #1408 and #1409 (completed) introduced a session-level io_uring ring
pool: a single transfer reuses one ring across its file pipeline rather than
opening and tearing down a ring per file. This task extends the model one
level further - sharing a single ring (or small ring set) **across concurrent
in-flight transfers** within one daemon process, not just within one session.

Per-session pool (#1409) remains the default. The shared ring is an opt-in
mode for daemons facing high connection churn.

## 2. Use Case

A daemon serving N concurrent clients today opens N rings (one per session
pool). At N >= 64 the cumulative cost shows up as:

- Setup: each `io_uring_setup(2)` reserves locked memory, kthread context,
  and SQ/CQ pages (~64 KiB minimum, more with `IORING_SETUP_SQPOLL`).
- Memory pressure: locked pages count against `RLIMIT_MEMLOCK` and cgroup
  `memory.max`; rings are not swappable.
- Kernel scheduling: each `SQPOLL` ring spawns a kernel poll thread.

Amortising one ring across many sessions trades per-connection isolation for
lower aggregate kernel footprint. Target workloads: archive servers, mirror
endpoints, CI artifact stores - many short transfers, low per-transfer queue
depth.

## 3. Trade-offs

- **Completion routing.** Tag every SQE `user_data` with a packed
  `(conn_id: u32, op_id: u32)`. The router demultiplexes CQEs to per-conn
  oneshot channels. Cost: one extra hash lookup per completion.
- **Backpressure.** A single slow consumer can stall the shared CQ if it
  fails to drain. Mitigation: bounded per-conn completion channel; if full,
  drop the connection rather than block the ring drainer.
- **Fairness.** SQ submission order = service order. Use a token-bucket
  admission gate per conn so one greedy session cannot starve peers.
- **Security.** Tasks share kernel buffers; rely on
  `IORING_REGISTER_RESTRICTIONS` to forbid op classes per conn (no `openat`,
  no network ops). Memory limits enforced at the cgroup level, not per ring.
- **Failure blast radius.** A ring fault (`-EAGAIN`/`-ENOMEM` storm) takes
  down all attached sessions; per-session pool isolates faults.

## 4. Design

```text
+------------------+   submit   +-----------------------+   poll   +---------+
| Session 1 worker | ---------> |                       | -------> |         |
+------------------+            |  SharedIoUringRouter  |          | Kernel  |
| Session 2 worker | ---------> |  (Arc<...>)           | <------- | io_uring|
+------------------+            |                       |   cqe    |         |
| Session N worker | ---------> +-----------------------+          +---------+
+------------------+                       |
                                           v
                         Per-conn mpsc channels (oneshot per op)
```

- `Arc<SharedIoUringRouter>` owns the ring, an SQ submission `Mutex` (or a
  per-thread shard), a `DashMap<conn_id, ConnState>`, and a dedicated
  completion-drainer thread.
- Sessions register on accept (`router.register(conn_id) -> ConnHandle`) and
  unregister on disconnect; the handle drops outstanding ops.
- `ConnHandle::submit(op)` returns `oneshot::Receiver<CqeResult>`; the worker
  awaits or blocks as today.
- Per-thread SQ shards (one ring per CPU, sharded by `conn_id % N`) reduce
  submission lock contention while keeping completion routing global.

Crate placement: `fast_io::shared_ring`, gated by `cfg(target_os = "linux")`
and `feature = "io_uring"`. No new public API in `core`/`daemon` - they hold
an `Option<Arc<SharedIoUringRouter>>` selected at daemon startup.

## 5. Recommendation

- Gate behind `--features shared-io-uring` (off by default).
- Default daemons keep per-session ring pool from #1409.
- Enable shared mode only when `max-connections >= 64` or operator opts in
  via `oc-rsyncd.conf` `io-uring = shared`.
- Ship metrics (ring-depth, drop counts, per-conn fairness histogram) before
  promoting out of feature gate.
