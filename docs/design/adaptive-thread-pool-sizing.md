# Adaptive Thread-Pool Sizing

Status: Design (cross-cutting follow-up to PRs #3645 and #3649)
Audience: engine, daemon, and operations maintainers
Scope: replace static `num_cpus * 2` worker/shard counts with a feedback-driven
adaptive sizer that observes load and re-tunes within bounded limits

## 1. Problem Statement

Two recently landed designs both use a static `num_cpus * 2` default:

- `docs/design/buffer-pool-sharding.md` (merged as PR #3645) sets the
  sharded `BufferPool`'s shard count to `num_cpus * 2`, bounded above by
  64 and below by 4.
- `docs/design/daemon-async-accept-sync-workers.md` (in flight as PR #3649)
  sets the daemon transfer worker pool to `N = num_cpus::get() * 2`.

The `num_cpus * 2` heuristic is a reasonable starting point. It matches
tokio's default worker count, rayon's default global pool size, and the
factor-of-two oversharding rule used by tcmalloc and jemalloc. The
heuristic is wrong, however, in two opposite ways:

1. **I/O-bound workloads underutilize idle cores.** A long-haul SSH pull
   over a 50 ms RTT link, or a slow-disk receiver writing to a spinning
   disk, runs each worker at well below 100% CPU. With a fixed pool of
   `2 * num_cpus`, the daemon caps concurrency below what the link or
   disk can absorb. Adding workers improves throughput linearly until the
   bottleneck saturates.
2. **CPU-bound workloads oversubscribe the cores.** With `--compress`
   doing zlib or zstd encode, or large-file checksum runs, each worker
   already pegs a core. A fixed pool of `2 * num_cpus` then forces twice
   as many runnable threads as cores, paying context-switch cost
   without throughput gain. Worse, the buffer pool's shard layer
   contends on hardware that already cannot keep up with the
   computational load.

The same observation applies to the buffer pool's shard count. A pure
I/O-bound consumer (network writer thread blocking on `sendmsg`) does
not need 16 shards on an 8-core box; a pure CPU-bound producer (parallel
signature generator) does. Both cases benefit from sizing that follows
observed load rather than a static guess.

This design specifies a single cross-cutting **adaptive sizer** that
both sites can opt into. The sizer is the third member of an existing
adaptive family in this codebase, alongside the `BufferPool` capacity
resizer (#1640 grow when miss-rate exceeds 20%, #1641 shrink when
utilization drops below 30%) and the adaptive queue depth work (#1735).
The pattern is now generalised so future sites such as the `io_uring`
registered-buffer pool (follow-up #2045) can adopt it without
re-deriving thresholds.

There is **zero wire-protocol impact**. Pool sizing is internal.

## 2. Telemetry Signals Available

The sizer adjusts size only in response to signals it can measure
cheaply on the existing data path. Four signals are reused from
neighbouring adaptive subsystems with no new instrumentation cost.

- **Pool utilization.** `utilization = workers_busy / pool_size`
  in `[0.0, 1.0]`. A single `AtomicUsize` is incremented on `recv()`
  return and decremented before the worker loops back. Sampled once
  per second.
- **Queue depth and stall time.** The handoff channel
  (`docs/design/daemon-async-accept-sync-workers.md` section 7) and
  the buffer pool's central `ArrayQueue` both expose `len()`. The
  buffer pool also counts hits and misses via the existing
  `PressureTracker::record_hit` / `record_miss` pair at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:80-100`
  (introduced in #1639). The shard-count sizer reuses those counters;
  no new hot-path increments are added.
- **Per-worker idle time.** Each worker takes `Instant::now()`
  immediately before and after the `recv()` call. Idle time is folded
  into a per-pool EWMA with a 30 s time constant.
  `worker_idle_fraction in [0.0, 1.0]`; near 1.0 means starvation,
  near 0.0 means saturation.
- **CPU-bound vs I/O-bound classification.** Comparing
  `queue_stall_fraction` (handoff back-pressure) to
  `worker_idle_fraction` (worker starvation) classifies the workload:
  stall-high + idle-low = CPU-bound (growing will not help);
  stall-low + idle-high = I/O-bound (growing will help). The
  classification is informational; the sizing decision is made on
  utilization alone, but the classification feeds the telemetry log
  line so operators can interpret each grow or shrink.

A more direct CPU signal exists on Linux via `getrusage(RUSAGE_THREAD)`.
We deliberately do not use it: Linux-only, ~200 ns per sample, no
information beyond what utilization plus queue depth already give.
Cross-platform parity matters here.

## 3. Two Sizing Modes

Two control-loop families are viable. We document both, recommend one,
and explicitly reject the other.

### 3.1 Mode A: PI-controller in a target band

Maintain utilization in the band `[60%, 85%]`. Below 60% the pool is
oversized; above 85% it is undersized.

```text
loop forever:
    sleep(1 s)
    sample utilization u, queue depth q, idle fraction i
    if u < 0.60 and now - last_shrink > 30 s:
        shrink_by_one()
    elif u > 0.85 and now - last_grow > 5 s:
        grow_by_one()
    else:
        hold()
```

The "by one" step is intentional. Each grow allocates one new shard or
spawns one new worker; each shrink retires one. Step size of one is the
quietest control law that still reaches the target band; larger steps
oscillate.

The 5-second grow cadence and 30-second shrink cadence are
asymmetric on purpose. Growing is cheap (allocation + spawn) and the
cost of being undersized is throughput loss; we react fast. Shrinking
is irreversible-in-the-short-term (a worker that is retired must be
respawned to handle the next burst) and the cost of being oversized is
modest memory; we react slow.

Bounds:

```
hard_min = max(2, num_cpus / 2)        // never serialize on a multi-core box
hard_max = num_cpus * 4                // never spawn unbounded threads
```

### 3.2 Mode B: AIMD (additive-increase, multiplicative-decrease)

```text
loop forever:
    sleep(1 s)
    sample utilization u
    if queue depth > pool_size and worker_idle < 0.10:
        size += 1
    elif u < 0.30:
        size = max(hard_min, size / 2)
```

AIMD reacts faster than PI-controller. The grow path is identical,
but shrinks halve the pool in a single step, which empties oversize
pools quickly after a burst.

The cost is oscillation. After a multiplicative shrink, the next burst
has to grow back from a much smaller floor. The pool size traces out a
sawtooth pattern centred on the steady-state size. Sawtooth is fine for
TCP congestion control (where loss is the signal and packets are
disposable); it is bad for daemon worker counts because spawning a
worker thread costs ~50 us and the work it would have done in that
50 us is gone.

### 3.3 Recommendation: Mode A

Mode A is recommended for both sites. The reasoning:

- **Steady-state predictability.** A daemon with a stable connection
  rate should pick a worker count and stay there. Operators reading
  `top` should see a stable thread count, not a sawtooth.
- **Long-running transfers reward a calm pool.** Most rsync transfers
  are tens of seconds to many minutes. Within one transfer the pool
  size should not change. Mode A's 5 s / 30 s cadence is well below
  the typical transfer lifetime.
- **Buffer pool shards have the same property.** The cost of dropping
  a shard is buffers freed; the cost of adding one is buffers
  allocated. Sawtooth on shard count thrashes the allocator for no
  benefit.
- **Mode B is documented but rejected.** It is the right shape for
  TCP congestion control because TCP loss is multiplicative and the
  cost of overshoot is loss. Neither condition holds here.

The sizer therefore implements Mode A. Mode B is referenced in the
risk table for the case where Mode A is empirically too slow (no
evidence to date suggests it will be).

## 4. Bounds and Safeguards

The sizer is constrained at four levels.

### 4.1 Hard bounds

```
size in [max(2, num_cpus / 2), num_cpus * 4]
```

The lower bound prevents the sizer from serializing the daemon on a
multi-core box during a long quiet period; the upper bound prevents
unbounded thread or memory growth during a sustained burst. A 16-core
box ranges from 8 to 64; a 4-core box ranges from 2 to 16; a 1-core box
is pinned at 2 (the lower-bound floor wins).

### 4.2 Hysteresis

Shrink decisions are delayed 30 s. The rationale is that bursty loads
follow a bimodal distribution: long quiet periods punctuated by short
busy bursts. A pure utilization metric averaged over 1 s would shrink
the pool during the quiet period; the next burst then has to grow it
back from below. Hysteresis catches the second burst before the first
shrink commits.

Grow decisions are delayed only 5 s. The asymmetry is the standard
PI-controller pattern: aggressive on the cost side (undersized hurts
clients), conservative on the benefit side (oversized just costs RAM).

### 4.3 Convergence guard

The sizer holds size unconditionally for one sample if the previous
two decisions were a grow followed by a shrink, or vice versa. This is
a thrash detector: if the controller is bouncing inside the band, the
band is too narrow for the load and holding is the safest action.
Three consecutive holds re-enable normal control.

### 4.4 Disable knob

Two ways to disable:

- **Environment variable.** `OC_RSYNC_ADAPTIVE_THREADS=0` falls back to
  the static `num_cpus * 2` default at process start. Standard escape
  hatch for benchmarks and bisection.
- **Daemon config.** `transfer-worker-threads = <fixed>` parses an
  integer and pins the pool to that size; `transfer-worker-threads =
  adaptive` (the default) enables the sizer. Same parser pattern as
  `max-connections = unlimited | <int>`.

The buffer pool exposes `BufferPool::with_capacity(adaptive)` (string-or-
int) as a constructor variant. The existing `BufferPool::new(N)`
remains and pins the pool to a fixed capacity, mirroring the existing
`with_memory_cap`/`with_buffer_size` constructors at
`crates/engine/src/local_copy/buffer_pool/pool.rs:166-206`.

## 5. Application Sites

### 5.1 BufferPool shard count

Today (PR #3645): `shard_count = num_cpus * 2`, bounded `[4, 64]`,
frozen at construction
(`docs/design/buffer-pool-sharding.md` section "Shard count").

Adaptive: same bounds, but the count starts at `max(4, num_cpus / 2)`
and adapts up to `min(64, num_cpus * 4)` based on shard hit rate
versus fallback overflow rate. Telemetry hook is the existing
`shard_hits` / `shard_overflows` counters that the sharding design
already adds to `BufferPoolStats`. The adapter watches those counters
the same way the existing capacity resizer at
`crates/engine/src/local_copy/buffer_pool/pressure.rs:120-164` watches
hits and misses.

The shard layer's adapter is a strict superset of the capacity
resizer. The capacity resizer adjusts the *fallback queue's* size; the
shard adapter adjusts the *number of shards in front of the
fallback*. They are independent and may run concurrently with no
interaction.

### 5.2 Daemon transfer worker pool

Today (PR #3649): `N = num_cpus::get() * 2`, fixed at startup, knob is
`transfer-worker-threads = N`
(`docs/design/daemon-async-accept-sync-workers.md` section 6).

Adaptive: `transfer-worker-threads = adaptive | <fixed>`. Default
`adaptive`. The adapter watches:

- handoff channel `len()` from the bounded crossbeam channel that the
  pool design already specifies (section 4 of that document),
- per-worker idle EWMA (section 2.3 above),
- pool utilization at 1 s sample rate (section 2.1 above).

When a grow decision fires, the pool spawns one additional worker
that immediately blocks on the handoff `recv()`. When a shrink
decision fires, the pool sends a poison-pill `Shutdown` message
through the channel; the next available worker takes the pill, runs
its `Drop` chain, and exits. Inflight transfers are never cancelled.

This integrates cleanly with the daemon's pool primitive
(`crates/daemon/src/daemon/transfer_pool.rs` per phase 1 of PR #3649).
The pool keeps a `Vec<JoinHandle>` and a `Sender<Item>`; growth pushes
a fresh handle; shrink consumes one handle by sending the pill and
joining.

### 5.3 io_uring registered buffer pool (future)

Issue #2045 (pending follow-up to the io_uring registered buffer pool)
specifies an adaptive sizing pass for the registered buffer count.
The same sizer applies: utilization is the fraction of registered
buffers in flight; the bounds are `[max(2, num_cpus / 2),
num_cpus * 4]`; the cadence is identical. The implementation site is
behind the `io_uring` feature gate and is not landed by this design,
but the design pre-commits the API surface so the io_uring team does
not have to re-derive it.

### 5.4 Rayon thread pool (deliberately not adopted)

Rayon has its own work-stealing scheduler with internal load
balancing. The library's documented contract is that the thread count
is set once via `rayon::ThreadPoolBuilder::num_threads`. Rayon
internally adapts queue depth and steal frequency; an outer adapter
that re-sized the pool would either fight rayon's scheduler or be
ignored.

The decision is to leave rayon's pool size at
`std::thread::available_parallelism()`, which is what the engine
already uses today. The sizer is **not** plumbed into rayon. If a
future workload demands it, a separate design note can revisit; for
now this is an explicit non-goal.

## 6. Wire-Compat Invariant

Zero impact. The sizer adjusts process-internal pool counts. It does
not change:

- the rsync wire protocol,
- the multiplex framing,
- the daemon greeting / auth handshake,
- the file-list, delta, or end-of-transfer envelopes,
- the byte count, byte order, or padding of any frame.

Nothing the sizer does is observable from the network. The same
invariant holds for the buffer pool sharding design (PR #3645) and the
async accept design (PR #3649); the adaptive sizer inherits both.

## 7. Memory Model

Each grow is an eager allocation; each shrink is a deferred drop.

### 7.1 Grow

For the buffer pool shard, a grow allocates one fresh `ArrayQueue`
slab plus the bookkeeping. With `shard_capacity = 4` and
`MaybeUninit<Vec<u8>> + u32` slots = ~32 bytes per slot, the
incremental cost is ~128 bytes per grow plus the `ArrayQueue`'s
two cache-line cursor pair = ~256 bytes total. Tiny.

For the daemon worker pool, a grow spawns one OS thread. Stack reserve
is the platform default (8 MiB virtual on Linux, ~1 MiB resident
typical). The pool retains the `JoinHandle` for graceful shutdown.

Both grow operations are fail-fast on OOM. If the allocation fails or
`thread::spawn` returns an error, the sizer logs the failure, retains
the previous size, and skips the next two sample windows (10 s of
back-off) to avoid a tight retry loop.

### 7.2 Shrink

For the shard, a shrink decision marks one shard as draining. The
shard stops accepting `push` and the next consumer drains its
remaining buffers into the fallback queue. Once empty, the shard's
storage is dropped. This deferred-drop pattern is consistent with the
existing `pressure.rs:120-164` resize: capacity is decremented
immediately but the actual buffer release happens on the next pop.

For the worker pool, a shrink sends a poison pill that the next
available worker consumes. The worker's transfer state at the time of
the pill is one of: idle on `recv()` (consumes pill, exits), or busy
in `handle_session` (will not consume the pill until the session
finishes). The pool guarantees that a shrink decision drops *one*
worker once *one* session ends, not before.

### 7.3 Memory ceiling

Combining the bounds:

```
shard_count ceiling  = num_cpus * 4
shard_capacity       = 4 (constant)
fallback             = soft_capacity buffers (~256 default)
worker_pool ceiling  = num_cpus * 4 threads
```

On a 16-core box with default config, the ceiling is:

```
shards    = 64 * 4 slots * 128 KB ~= 32 MiB
fallback  = 256 slots * 128 KB    = 32 MiB
workers   = 64 threads * 1 MiB    = 64 MiB resident
```

Total ceiling = ~128 MiB. The static `num_cpus * 2` baseline is half
of that. Operators who need a tighter ceiling pin
`transfer-worker-threads = <fixed>`.

## 8. Failure Semantics

The sizer runs on a dedicated background thread. It must not poison
the data plane.

### 8.1 Sizer thread panic

The sizer thread wraps its main loop in `std::panic::catch_unwind`.
A panic:

1. Is caught at the loop boundary.
2. Logs the panic payload at error level via the existing
   `describe_panic_payload` helper.
3. Drops the loop's local state (last-decision timestamp, EWMA).
4. The thread exits.

After the sizer thread exits, the pool stays at its **last-known-good
size**. There is no respawn loop. The pool continues to serve
requests; only the adaptive resizing stops. An operator can restart
the daemon (or send `SIGHUP` to reload, which re-spawns the sizer
thread) to recover the adaptive behaviour.

This is intentional. The sizer is a non-essential optimisation;
respawning it inside the same process risks an infinite panic loop on
a deterministic bug. Stopping is safer than retrying.

### 8.2 Grow allocation failure

Covered in section 7.1. Fail-fast, log, back off 10 s, retain previous
size.

### 8.3 Shrink stuck on long session

A worker that has been issued a poison pill but is mid-session does
not exit until the session ends. If the operator wants the worker
gone immediately, they have the existing `--timeout` and SIGTERM
escalation paths. The sizer does not force-cancel a session; doing so
would violate wire-compat invariants.

### 8.4 Sizer thread overhead

The sizer thread runs once per second. Its work per tick is two
atomic reads, one EWMA update, one comparison against the band, and
a sleep. Cost is on the order of 1 us per tick = ~0.0001% of one core.
Below that floor, skipping the sizer entirely costs more in startup
plumbing than running it costs in steady state, so the sizer always
runs once started. The disable knob (section 4.4) is the only path
that prevents it from starting.

## 9. Telemetry Hooks

Every sizing decision is logged via the standard `debug_log!` macro at
`-vv` verbosity. Format:

```
adaptive_thread_pool: domain=buffer_pool size=12 utilization=72%
                      queue_stall=5% worker_idle=14%
                      target=[60,85] action=hold
```

```
adaptive_thread_pool: domain=daemon_worker size=20 utilization=89%
                      queue_stall=42% worker_idle=2%
                      target=[60,85] action=grow new_size=21
```

Fields:

- `domain`: which pool is being sized (`buffer_pool` shards,
  `daemon_worker`, future `iouring_buffer`).
- `size`: current pool size at the start of the tick.
- `utilization`: percentage of workers busy or shards filled.
- `queue_stall`: percentage of the sample window in which the handoff
  queue was at capacity.
- `worker_idle`: EWMA of per-worker `recv()` idle time.
- `target`: configured utilization band.
- `action`: `hold | grow | shrink`.
- `new_size`: only on grow/shrink.

The same logging pattern as #1369's SPSC contention metrics: a single
structured line, parseable by simple regex, and always at `-vv`. No
Prometheus or OpenMetrics export in this design (out of scope; tracked
as a future telemetry follow-up).

A `BufferPoolStats` extension exposes the four most recent decisions
and the current size; tests assert on the size, not on the log line.

## 10. Concrete Defaults

Defaults are tabulated for review.

| Parameter             | Default        | Rationale                                          |
|-----------------------|----------------|----------------------------------------------------|
| Sample period         | 1 s            | Balance reactivity vs sample noise.                |
| Target utilization    | [60%, 85%]     | Standard PI band; tcmalloc / jemalloc precedent.   |
| Grow cadence          | 5 s            | React fast on undersize; cost is throughput.       |
| Shrink cadence        | 30 s           | React slow on oversize; cost is just RAM.          |
| Hard minimum          | max(2, n/2)    | Avoid single-thread serialization on multi-core.   |
| Hard maximum          | num_cpus * 4   | Avoid unbounded growth under DoS-shaped load.      |
| Step size             | 1              | Smallest law that converges; no sawtooth.          |
| Convergence guard     | hold 1 sample  | Thrash detector for grow-then-shrink oscillation.  |
| Idle EWMA constant    | 30 s           | Matches shrink cadence; smooths bursty workloads.  |
| Disable env var       | OC_RSYNC_ADAPTIVE_THREADS=0 | Bisection / benchmark escape hatch. |
| Daemon config knob    | transfer-worker-threads = adaptive | Default-on. |
| BufferPool ctor       | BufferPool::with_capacity(adaptive) | Default-on. |

PI controller gains are captured implicitly by the cadences. The
controller is non-traditional: it is a band-pass with a one-step
saturating action, not a continuous-output PID. Tuning the gains
therefore reduces to tuning the cadences. The asymmetry between the
grow and shrink cadences is the only "gain" the controller exposes.

## 11. Override Path

### 11.1 Environment variable

`OC_RSYNC_ADAPTIVE_THREADS=0` at process start disables the sizer for
both the daemon worker pool and the buffer pool shard count. The
variable is read once at startup; runtime mutation has no effect.
Implementation lives next to the existing `OC_RSYNC_BUFFER_POOL_SIZE`
parser at `crates/engine/src/local_copy/buffer_pool/global.rs:44-49`.

`OC_RSYNC_ADAPTIVE_THREADS=1` (or unset) enables the sizer. Any other
value logs a warning and falls back to the default-on behaviour.

### 11.2 Daemon config

```
transfer-worker-threads = adaptive    # default
transfer-worker-threads = 32          # pin to 32
transfer-worker-threads = 0           # treat as default (adaptive)
```

Parsed at config load by extending the existing
`crates/daemon/src/rsyncd_config/sections.rs` parser. Validation lives
in `crates/daemon/src/rsyncd_config/validation.rs`. The validator
accepts the literal string `adaptive` or any positive integer in the
range `[1, 4096]` (an upper sanity bound).

### 11.3 BufferPool API

```rust
pub enum PoolCapacity {
    Adaptive,
    Fixed(usize),
}

impl BufferPool {
    pub fn with_capacity(cap: PoolCapacity) -> Self { ... }
}
```

The existing `BufferPool::new(usize)` and `with_buffer_size` continue
to exist and pin the pool to a fixed capacity. They are equivalent to
`with_capacity(PoolCapacity::Fixed(n))`. New code paths default to
`PoolCapacity::Adaptive`.

## 12. Migration Plan

The sizer lands behind the env-var disable. Phases:

1. **Phase 0: this design.** Land the design note. No code change.
2. **Phase 1: adaptive_sizer module.** Add
   `crates/engine/src/adaptive_sizer.rs`. Self-contained: trait
   `SizingTarget` with `utilization()`, `queue_stall()`, `idle_ewma()`,
   `grow()`, `shrink()`, `current_size()`. The sizer thread takes a
   `Vec<Box<dyn SizingTarget>>` and ticks them in parallel. Unit tests
   assert the controller stays in the band under simulated load.
3. **Phase 2: BufferPool integration.** Implement `SizingTarget` for
   the sharded pool. Default-on with env-var disable. Telemetry tests
   under `crates/engine/src/local_copy/buffer_pool/tests.rs` mirror
   the existing `pressure.rs` tests.
4. **Phase 3: daemon worker pool integration.** Implement
   `SizingTarget` for `TransferWorkerPool`. The daemon's
   `transfer-worker-threads` directive becomes `adaptive | <int>`.
   Default-on with env-var disable.
5. **Phase 4: telemetry / dashboard.** Add the structured-log line at
   `-vv`. Optionally surface size and last-decision in the systemd
   status output. No Prometheus export in this phase.
6. **Phase 5: remove env-var disable.** After one release of telemetry
   showing no operator complaints and no adverse benchmarks, remove
   `OC_RSYNC_ADAPTIVE_THREADS`. The daemon config knob and
   `BufferPool::with_capacity(Fixed(N))` remain as the supported
   pinning paths.

Each phase is independently reviewable. Phase 0 is this PR.

## 13. Risks

- **Tuning oscillation.** The controller might bounce inside the band.
  Mitigation: convergence guard (section 4.3) holds size for one
  sample after a direction reversal; the 5 s / 30 s grow / shrink
  asymmetry biases against oscillation because the shrink cooldown
  will almost always catch a grow request before a shrink fires.
  Mode B (AIMD) remains documented as a one-line fallback if the
  asymmetry proves empirically wrong.
- **Memory bloat at peak load.** The controller could grow to
  `num_cpus * 4` during a load spike. Mitigation: hard upper bound,
  plus `transfer-worker-threads = <fixed>` for operators who need a
  guaranteed ceiling. The benchmark harness in PR #3649 measures the
  adaptive sizer alongside the static default at 100 / 1k / 10k
  concurrent connections.
- **Sizer-thread overhead at low concurrency.** Section 8.4 puts the
  sizer cost at ~1 us per tick. At one-connection-per-minute that is
  below the noise floor of every other daemon operation. A phase-6
  follow-up can suspend the sizer after >5 minutes at hard-min if
  embedded targets prove that assumption wrong.
- **Disagreement between sizers.** The shard adapter and the worker
  adapter optimise independent signals (buffer-pool contention vs
  accept-side load). They share no state; their cadences are aligned
  but their decisions are independent and never override each other
  under the convergence guard.
- **Integration churn.** The `SizingTarget` trait is six methods,
  lives in `engine`, and is `#[non_exhaustive]`. The daemon already
  depends on `engine` indirectly via `core`; the direct dep is
  consistent with the crate graph in `AGENTS.md`.

## 14. Tracking

These follow-ups are not added to the persistent TODO list. They are
listed here so reviewers can plan implementation order:

- **adaptive_sizer module TODO**: implement
  `crates/engine/src/adaptive_sizer.rs` with the `SizingTarget` trait,
  the PI-controller loop, the convergence guard, and unit tests.
- **BufferPool integration TODO**: wire the shard count to the sizer
  via a `SizingTarget` impl that watches
  `BufferPoolStats::shard_overflows` and the shard hit rate.
- **Daemon worker pool integration TODO**: wire
  `TransferWorkerPool` to the sizer; extend
  `crates/daemon/src/rsyncd_config/sections.rs` for the
  `adaptive | <fixed>` parser; gate behind the existing
  `async-daemon` feature so non-async builds keep the sync model.
- **Telemetry / dashboard TODO**: add the `-vv` structured-log line in
  every sizing decision; add the `BufferPoolStats` snapshot of the
  last four decisions; document the schema in the operator guide.

The io_uring registered-buffer adopter (#2045) reuses the same trait;
the work for that follow-up is documented separately and is out of
scope here.

## Decision

Land the design now. Implementation follows phases 1-4 above. Phases
1 and 2 are the gating prerequisites; phase 3 lands once PR #3649
merges. The env-var disable (`OC_RSYNC_ADAPTIVE_THREADS=0`) is the
escape hatch for the first release; phase 5 removes it once telemetry
confirms stability across one release cycle.
