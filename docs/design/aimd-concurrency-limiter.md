# AIMD Adaptive-Concurrency Limiter (RFC)

Status: Draft
Tracking: task #2090 (design), #2091 (impl), #2092 (tests), #2093 (CLI flag)
Related: #1553 (tune `CAPACITY_MULTIPLIER`), #1554 (per-operation adaptive thresholds), #1834 (adaptive buffer sizing with EMA throughput feedback)

## 1. Motivation

The transfer pipeline currently exposes two static knobs that control how many delta jobs are in flight at once:

- `DEFAULT_PARALLEL_THRESHOLD = 64` in `crates/transfer/src/delta_pipeline.rs:42`. Files below this count run sequentially; at or above it, the receiver dispatches into the parallel work-queue.
- `CAPACITY_MULTIPLIER = 2` in `crates/engine/src/concurrent_delta/work_queue/capacity.rs:8`, used by `default_capacity()` at line 36 and by `bounded()` at `crates/engine/src/concurrent_delta/work_queue/bounded.rs:90`. The work queue capacity is `2 * rayon::current_num_threads()`.
- `adaptive_queue_depth()` at `crates/engine/src/concurrent_delta/work_queue/capacity.rs:66` chooses 8x / 4x / 2x of the rayon thread count based on average file size only.

These knobs are picked once and never revisited during a transfer. They do not respond to any of the conditions that actually bound throughput in practice:

- Kernel queue depth on the disk-commit side (`crates/transfer/src/disk_commit/thread.rs`, `crates/transfer/src/disk_commit/process.rs`).
- Network RTT and sender-side stalls visible to the receiver via inter-token gaps.
- Disk saturation: write-amplification and fsync stalls on the receiver.
- Rayon worker contention on the buffer pool, the file map, and metadata application.
- Effective in-flight bytes when the destination is slow (NFS, FUSE, USB).

Symptoms observed on real workloads:

- 100K small-file workloads (#1551 profile, #1853 ReorderBuffer audit) see worker starvation when capacity is too low and head-of-line blocking in the `ReorderBuffer` (`crates/engine/src/concurrent_delta/reorder.rs`) when capacity is too high.
- Large-file delta transfers over slow disks fill the queue and block the wire reader.
- NFS / FUSE destinations (#1084) hit metadata stat latency that the static threshold cannot adapt to.

This RFC proposes a feedback-driven concurrency limiter that sits in front of the work queue and the disk-commit dispatcher, replacing the static capacity choice with an Additive-Increase / Multiplicative-Decrease (AIMD) controller.

## 2. Background

### 2.1 AIMD in TCP congestion control

TCP Reno's congestion-avoidance phase (RFC 5681, section 3.1) increases `cwnd` by one MSS per RTT on each successful ACK and halves it on a congestion signal (duplicate ACK or timeout). The AIMD law guarantees fairness and convergence under shared bottlenecks because every flow's window grows linearly and shrinks geometrically.

The same control law applies to any resource where:

- Increasing concurrency improves throughput up to a knee.
- Beyond the knee, latency rises faster than throughput.
- An overload signal is observable.

### 2.2 Netflix `concurrency-limits`

Netflix's open-source library `concurrency-limits` (https://github.com/Netflix/concurrency-limits) adapts the same idea for service clients and servers. It exposes `Limit` strategies (`AIMDLimit`, `Vegas`, `Gradient`, `Gradient2`) and a `Limiter` API that hands out tickets. The AIMD strategy increments by alpha on success, multiplies by beta on overload, and clamps to `[min_limit, max_limit]`. Production deployments at Netflix use it to bound RPC fan-out without static thread-pool sizing.

This RFC reuses the algorithm but reimplements it natively in Rust so the limiter integrates with `crossbeam-channel` and `rayon` without a JVM dependency. See section 5 for why we do not pull in a Rust binding.

## 3. Design

### 3.1 Placement

The limiter lives in `crates/engine/src/concurrent_delta/work_queue/`, alongside the existing capacity policy, exported as `pub mod limiter;` from `crates/engine/src/concurrent_delta/work_queue/mod.rs:97-110`.

```text
                        +---------------------+
Generator ──► WorkQueue │ AimdLimiter         │ ──► drain_parallel ──► ReorderBuffer
              (bounded) │  acquire() / record │     (rayon::scope)       (in-order)
                        +---------------------+
                              ▲          │
                              │          ▼
                         feedback   slot release
                       (timing, errors, queue depth)
```

The limiter wraps the single producer's `WorkQueueSender::send()` (`bounded.rs:78`) so an item only enters the queue once a slot is acquired; the limiter releases the slot when the consumer publishes a `DeltaResult`.

### 3.2 State machine

```rust
struct AimdLimiter {
    target: AtomicUsize,        // current concurrency limit
    in_flight: AtomicUsize,     // outstanding tickets
    min_limit: usize,           // floor (rayon thread count)
    max_limit: usize,           // ceiling (8 * rayon thread count, matching adaptive_queue_depth small-file branch)
    alpha: u32,                 // additive increase per success window
    beta_num: u32,              // multiplicative decrease numerator (1)
    beta_den: u32,              // multiplicative decrease denominator (2 -> beta = 0.5)
    rtt_ema: AtomicU64,         // exponentially smoothed completion latency (nanoseconds)
    rtt_var: AtomicU64,         // smoothed variance for sigma estimate
    error_window: AtomicU32,    // recent error count in rolling window
    last_decrease: AtomicU64,   // timestamp of last multiplicative-decrease (epoch nanos)
}
```

States are implicit in the value of `target` relative to `in_flight`:

- **Slow-start** (initial): `target` doubles on each successful window until the first overload signal.
- **Steady AIMD**: `target += alpha` per completed window of `target` successes; `target = (target * beta_num) / beta_den` on overload.
- **Quiescent**: `in_flight == 0` for more than 5 seconds. Reset `rtt_ema` to avoid stale baselines biasing the next active period.

### 3.3 AIMD rules

- `alpha = 1`. After `target` consecutive non-overloaded completions, `target` increases by 1.
- `beta = 0.5`. On any overload signal, `target` is set to `max(min_limit, target / 2)` and `last_decrease` is updated.
- A debounce window of `2 * rtt_ema` after a decrease suppresses further decreases. This prevents a burst of slow completions from collapsing the limit to `min_limit` on a single transient stall.

### 3.4 Overload detection

A completion is classified as overloaded if any of the following is true:

1. **RTT spike**. `completion_latency > rtt_ema + 2 * sqrt(rtt_var)`. EMA smoothing factor alpha_ema = 1/8, matching TCP's RFC 6298 RTT estimator.
2. **Queue saturation**. The bounded `crossbeam-channel` was full at `send()` time, observed via `try_send`-then-`send` pattern. The first failed `try_send` is the overload signal; the blocking `send()` follows.
3. **Error rate**. More than `target / 8` errors of class `io::ErrorKind::WouldBlock`, `Interrupted`, or any disk-commit `io::Error` in the last `target` completions.
4. **Disk-commit backpressure**. The disk-commit side (`crates/transfer/src/disk_commit/thread.rs`, `crates/transfer/src/disk_commit/process.rs`) exposes a `WriterPressure` enum (new); when the writer reports `Full`, the limiter treats every in-flight ticket as overloaded for the duration of the next decrease window.

Item (3) deliberately ignores `NotFound`, `PermissionDenied`, and other deterministic errors; those reflect filesystem state, not overload.

### 3.5 Feedback signal sources

| Signal | Source | Frequency |
|--------|--------|-----------|
| Completion latency | `Instant::now() - ticket.acquired_at` on `record_release()` | Per work item |
| Queue saturation | `crossbeam-channel::try_send` `Full` returns | Per send |
| Disk-commit pressure | new `WriterPressure` channel from `disk_commit/{thread,process}.rs` | Per batch |
| Wire-side stall | inter-token gap > `2 * rtt_ema` (receiver bookkeeping) | Per token |

## 4. Integration points

### 4.1 Engine crate

- `crates/engine/src/concurrent_delta/work_queue/limiter.rs` (new). Houses `AimdLimiter`, `Ticket`, and `LimiterConfig`.
- `crates/engine/src/concurrent_delta/work_queue/mod.rs` (existing, lines 98-110). Add `mod limiter;` and re-export `AimdLimiter` and `LimiterConfig`.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` (existing, lines 74-104). `WorkQueueSender::send()` becomes `WorkQueueSender::send_limited(work, &limiter) -> Result<Ticket, SendError>` for the adaptive path. The original `send()` remains for the static path so non-adaptive callers are unchanged.
- `crates/engine/src/concurrent_delta/consumer.rs` and `crates/engine/src/concurrent_delta/strategy.rs` (existing). Drop the `Ticket` when a `DeltaResult` is published, recording success or failure.
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs:36` (`default_capacity`) is unchanged; the limiter starts at `default_capacity()` and treats `adaptive_queue_depth(avg_file_size)` as `max_limit`.

### 4.2 Transfer crate

- `crates/transfer/src/delta_pipeline.rs:42` (`DEFAULT_PARALLEL_THRESHOLD`) is unchanged. The threshold still gates sequential vs parallel; the limiter only governs the parallel path.
- `crates/transfer/src/delta_pipeline.rs:284-319` (`ParallelDeltaPipeline::new`, `ParallelDeltaPipeline::default`). New `ParallelDeltaPipeline::with_limiter(...)` constructor that injects an `Arc<AimdLimiter>` shared with the disk-commit thread.
- `crates/transfer/src/disk_commit/mod.rs`, `crates/transfer/src/disk_commit/thread.rs`, `crates/transfer/src/disk_commit/process.rs`. Add a `WriterPressure` reporter that pushes overload notices to the limiter when its bounded queue (capacity from `disk_commit/config.rs`) is at high-water mark.

### 4.3 CLI crate

- `crates/cli/src/frontend/arguments/parsed_args/mod.rs:573-582`. Add fields next to `rayon_threads`:

  ```rust
  /// `--adaptive-concurrency` / `--no-adaptive-concurrency` - enable AIMD limiter.
  pub adaptive_concurrency: Option<bool>,
  ```

- `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs` (existing, around line 353 where `--rayon-threads` is registered). Add the `--adaptive-concurrency` / `--no-adaptive-concurrency` Clap flag pair using `clap::ArgAction::SetTrue` / `SetFalse`.
- `crates/cli/src/frontend/arguments/parser/mod.rs:190` (where `rayon-threads` is parsed). Parse the boolean tri-state (`Some(true)`, `Some(false)`, `None` for default).
- `crates/cli/src/frontend/help.rs:50` (where `--rayon-threads` help is rendered). Append a one-line description for the new flag pair.
- `crates/cli/src/frontend/defaults.rs:32`. Append `--adaptive-concurrency` to the list of default-true flags.
- Default: enabled. Disabling pins concurrency at `default_capacity()` and skips all feedback bookkeeping.

### 4.4 Daemon crate

The daemon path (`crates/daemon/`) reaches the same `core::session()` entry as the CLI. No daemon-specific wiring is needed beyond the shared CLI flag plumbing.

## 5. Trade-offs

### 5.1 AIMD vs alternatives

| Algorithm | Pros | Cons | Verdict |
|-----------|------|------|---------|
| AIMD | Simple, well understood, RFC 5681 lineage, predictable convergence | Sawtooth amplitude under bursty load | Selected |
| Vegas | Reacts to RTT inflation before loss | Needs accurate baseline RTT, hard to obtain on local-only transfers | Rejected for now (#2094 may revisit for buffer sizing) |
| BBR | Models bottleneck bandwidth and RTT directly | Complex, requires periodic probe phases that pollute throughput measurements | Rejected: complexity not justified |
| Gradient2 (Netflix) | Smoother than AIMD, uses gradient of RTT | Tighter coupling to latency model; harder to reason about under disk fsync stalls | Rejected: AIMD is enough for v1 |

### 5.2 Native Rust vs binding to a library

We do not pull in a Rust port of Netflix `concurrency-limits` or wrap the JVM library. Justifications:

- The algorithm is roughly 200 lines of code; a dependency is not worth the supply-chain surface, especially given the unsafe-code policy in CLAUDE.md.
- We need tight integration with `crossbeam-channel::try_send` semantics and the existing `WorkQueueSender` type, which a generic library does not give us.
- Native code lets us share atomics and `Instant` baselines with the existing `ReorderBuffer` instrumentation (#1885) without an FFI boundary.

### 5.3 Risks

- **Oscillation under bursty workloads**. AIMD converges by sawtooth; under highly bursty wire arrivals the limit can swing widely. Mitigation: the `2 * rtt_ema` debounce window after each decrease and the `min_limit = rayon::current_num_threads()` floor prevent collapse below the rayon thread count.
- **Interaction with `--rayon-threads`**. If the user pins `--rayon-threads=N`, the limiter must not exceed `8 * N`. The `LimiterConfig::max_limit` is computed after `rayon::current_num_threads()` is finalized in `crates/cli/src/frontend/execution/drive/thread_tunables.rs:34`. The limiter respects user-set thread caps.
- **Local-only transfers without RTT signal**. Local copies do not have a meaningful RTT; completion latency is dominated by per-file syscall cost. The variance term in the overload predicate becomes noisy. Mitigation: section 7 open question.
- **Interop sensitivity**. The limiter changes scheduling, not wire output. Wire format remains byte-identical (covered by `crates/protocol/tests/golden/`). Property test: parallel vs sequential output equivalence already passes (#1651) and is rerun under the new limiter.

## 6. Implementation phases

| Phase | Task | Output |
|-------|------|--------|
| 1 | #2091 - Implement AIMD concurrency limiter in transfer crate | `limiter.rs` module with `AimdLimiter`, `Ticket`, `LimiterConfig`, `WriterPressure` plumbing in `disk_commit/`. Default-off behind a feature flag during initial CI bake. |
| 2 | #2092 - AIMD limiter tests: convergence under error injection | Property tests for: target stays in `[min_limit, max_limit]`, monotonic recovery after a decrease, no decrease during debounce window, byte-for-byte output parity vs static path. |
| 3 | #2093 - Add `--adaptive-concurrency` / `--no-adaptive-concurrency` CLI flag pair | Wire the flag, add `parsed_args` field, default to enabled once CI bake passes. |
| 4 | Follow-on (no separate ticket yet) | Telemetry: expose `target`, `in_flight`, decrease count via `--info=adaptive` after #2112 is in flight. |

## 7. Open questions

1. **Per-pipeline vs per-process limiter?** A single process may run multiple `core::session()` invocations (daemon mode handles each connection in its own pipeline). Sharing one `AimdLimiter` across sessions gives global fairness; per-pipeline limiters give faster local convergence. Proposed: per-pipeline for v1, revisit after #1933 daemon scaling benchmarks.
2. **Interaction with `--bwlimit`?** The bandwidth limiter (`crates/bandwidth/src/limiter/`) already throttles wire bytes. When `--bwlimit` is the active bottleneck, completion latency is dominated by the token bucket sleep in `bandwidth/src/limiter/sleep.rs`, which would trip the RTT spike predicate and collapse `target`. Proposed: when `--bwlimit` is set, suppress the RTT-spike branch of overload detection and rely on queue saturation only. Alternative: subtract the token-bucket sleep from observed latency before computing the EMA.
3. **Feedback signal quality on local-only transfers?** Local copies bypass the wire, so `rtt_ema` reflects per-file syscall cost, not network conditions. For local copies the disk-commit `WriterPressure` signal is the only meaningful overload indicator. Proposed: detect local-mode at session setup and configure `LimiterConfig::ignore_rtt = true`; rely on queue saturation and `WriterPressure` only.
4. **Sender-side limiter?** This RFC scopes the limiter to the receive/disk-commit side. A symmetric sender-side limiter (governing how many files the sender reads concurrently from disk) is left for a follow-on RFC after #1551 profiles parallel-dispatch overhead.
5. **Persistence across reconnects?** The SSH transport may reconnect mid-transfer (#1688). The limiter's tuned `target` is lost on reconnect. Proposed: ignore for v1; the limiter reconverges within seconds from `default_capacity()`.

## 8. References

- RFC 5681. TCP Congestion Control. https://www.rfc-editor.org/rfc/rfc5681
- RFC 6298. Computing TCP's Retransmission Timer. https://www.rfc-editor.org/rfc/rfc6298
- Netflix concurrency-limits. https://github.com/Netflix/concurrency-limits
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs` (`CAPACITY_MULTIPLIER`, `adaptive_queue_depth`)
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` (`bounded`, `WorkQueueSender::send`)
- `crates/transfer/src/delta_pipeline.rs` (`DEFAULT_PARALLEL_THRESHOLD`, `ParallelDeltaPipeline`)
- `crates/transfer/src/disk_commit/{config,thread,process}.rs`
- `crates/cli/src/frontend/arguments/parsed_args/mod.rs` (`rayon_threads`, `tokio_threads`)
