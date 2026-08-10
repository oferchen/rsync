# Per-stream token-bucket with throughput feedback loop

Status: Draft / Proposal
Tracking: #2097 (this RFC), #2098 (convergence tests)
Related: #2090 (AIMD limiter), #2094 (adaptive-buffer controller)

## 1. Motivation

oc-rsync inherits upstream rsync's `--bwlimit` semantics. The current
limiter is a single fixed-rate token-bucket that mirrors
`io.c:sleep_for_bwlimit()`: every completed write registers byte-debt
against a wall-clock allowance and the caller sleeps when that debt
exceeds 100 ms of transfer time. The implementation lives in
`crates/bandwidth/src/limiter/core/limiter.rs:34` (`BandwidthLimiter`
struct) and is wired into the client config at
`crates/core/src/client/config/bandwidth.rs:127` (`to_limiter`).

Two operational scenarios are not served well today:

1. **Daemon multi-module fairness.** A single `oc-rsyncd` process can
   serve N concurrent client connections through the connection pool
   (`crates/daemon/src/daemon/connection_pool/pool.rs:48`,
   `ConnectionPool`). Each connection currently constructs its own
   limiter from the merged module/client `--bwlimit`. There is no
   central reconciler: a stream that under-uses its budget cannot
   donate slack to a starving sibling, and a stream that overshoots
   on bursty workloads cannot borrow from quiet siblings. The pool
   already tracks per-IP byte counters
   (`crates/daemon/src/daemon/connection_pool/pool.rs:36`,
   `add_bytes`) but nothing closes the feedback loop back into the
   limiter.

2. **SSH multiplexed transfers.** When several oc-rsync invocations
   share an SSH ControlMaster, the kernel applies one TCP congestion
   window across all logical streams. A fixed `--bwlimit` per stream
   either leaves the underlying link idle or causes head-of-line
   stalls, because every stream paces independently of observed
   throughput.

The kernel already performs TCP backoff; we do not need to reinvent
congestion control. What we lack is a *closed-loop* limiter that uses
*observed* throughput (already collected for `--stats`) to:

- decay its target rate when the downstream consumer is slower than
  the configured cap (no point holding the budget); and
- elevate its target rate, capped by `--bwlimit`, when the link has
  measurable headroom and other streams are not saturating it.

This proposal extends the existing limiter with an opt-in
`AdaptiveBandwidthLimiter` variant driven by a per-stream throughput
sample window.

## 2. Background

Token-bucket pacing (RFC 2697 single-rate three-color marker, srTCM)
maintains a bucket of size `capacity` filled at `fill_rate` bytes per
second. A request for `n` bytes succeeds when `>= n` tokens are
available; otherwise the caller waits. A leaky bucket, by contrast,
enforces a *maximum* output rate by draining at `fill_rate` regardless
of demand, so bursts in excess of `capacity` are dropped or queued.
A token-bucket enforces a *long-term average* rate while permitting
bursts up to `capacity`. Upstream rsync's `sleep_for_bwlimit` is a
token-bucket variant; `BandwidthLimiter::with_burst`
(`limiter/core/limiter.rs:62`) exposes the burst cap.

Pure rate enforcement is open-loop: the configured rate is the only
input. A closed-loop limiter samples *delivered* throughput and steers
the fill rate within bounds. AIMD (additive-increase /
multiplicative-decrease - the same family TCP Reno uses) is a well-
studied feedback law that converges on fair, stable utilisation
provided the sampling interval is longer than the observation jitter.

## 3. Current state

The limiter module is decomposed into three concerns:

- `crates/bandwidth/src/limiter/core/limiter.rs:34` - the
  `BandwidthLimiter` struct: `limit_bytes`, `write_max`,
  `burst_bytes`, `total_written`, `last_instant`,
  `simulated_elapsed_us`. `register()` at line 175 is the hot path:
  add bytes, subtract elapsed allowance, clamp to burst, sleep if
  debt >= 100 ms.
- `crates/bandwidth/src/limiter/change.rs:113` -
  `apply_effective_limit` reconciles a daemon-imposed cap with the
  current limiter and returns a `LimiterChange` describing the
  transition (`Unchanged` / `Updated` / `Enabled` / `Disabled`).
- `crates/bandwidth/src/limiter/sleep.rs:21` - `LimiterSleep`
  records requested vs actual sleep so deterministic tests can
  assert pacing without real sleeping.

Configuration is built in
`crates/core/src/client/config/bandwidth.rs`:
`to_limiter` at line 127 instantiates a `BandwidthLimiter`,
`into_limiter` at line 137 is the consuming variant.

Throughput telemetry is captured by `TransferStats` at
`crates/protocol/src/stats/transfer.rs:50`. Fields `total_read`
(line 52) and `total_written` (line 54) are the sender-/receiver-
relative byte counters already exchanged on the wire and printed by
`--stats`. They are accumulated continuously across the transfer,
not sampled, so a feedback controller that wants a *rate* must
diff them against a wall clock.

The daemon-side per-connection counters live separately in
`crates/daemon/src/daemon/connection_pool/pool.rs:48`
(`ConnectionPool`) and are updated through
`add_bytes(&id, sent, received)`. Stream identification uses
`ConnectionId` from
`crates/daemon/src/daemon/connection_pool/types.rs`.

## 4. Design

### 4.1 Per-stream limiter

```rust
// crates/bandwidth/src/limiter/adaptive.rs (new submodule)
pub struct AdaptiveBandwidthLimiter {
    inner: BandwidthLimiter,           // existing pacer
    ceiling_bytes: NonZeroU64,         // hard cap == --bwlimit
    floor_bytes: NonZeroU64,           // min target rate (default ceiling/8)
    samples: SampleWindow,             // rolling bytes/sec observations
    consecutive_under: u8,             // K-of-N trigger for decay
    last_adjust: Instant,
}
```

`AdaptiveBandwidthLimiter` is a thin wrapper. It owns one
`BandwidthLimiter` and re-uses its `register()` for the actual
sleep/debt accounting. The adaptive logic only mutates the wrapped
limiter's rate via the existing
`BandwidthLimiter::update_limit(NonZeroU64)`
(`limiter/core/limiter.rs:80`).

### 4.2 Feedback signal

`SampleWindow` holds a fixed-size circular buffer of
`(timestamp, bytes)` tuples covering the last `window_secs` of
transfer (default 4 s, tunable via daemon config). Bytes are pushed
by the same call site that already updates `TransferStats`:

- For client-side transfers, the receiver's
  `bytes_received_running_total` (already maintained in
  `crates/transfer/src/receiver/`) feeds the window after each
  multiplex frame.
- For daemon-served transfers, `ConnectionPool::add_bytes` at
  `crates/daemon/src/daemon/connection_pool/pool.rs:36` is the
  natural insertion point - both counters update together.

`SampleWindow::observed_rate()` returns
`(last.bytes - first.bytes) / (last.ts - first.ts).as_secs_f64()`
or `None` if the window is shorter than `min_window_secs` (default
1 s). Bytes counters never decrease so the subtraction is monotonic.

### 4.3 Adjustment rule

A control step runs whenever `register()` is called *and*
`Instant::now() - last_adjust >= adjust_interval` (default 500 ms):

```
let observed = window.observed_rate();
let target = inner.limit_bytes().get() as f64;

if observed < 0.7 * target {
    consecutive_under = consecutive_under.saturating_add(1);
    if consecutive_under >= K {           // default K = 3
        let new_rate = (target * 0.85).max(floor_bytes);
        inner.update_limit(new_rate);     // multiplicative decrease
        consecutive_under = 0;
    }
} else if observed >= 0.95 * target {
    consecutive_under = 0;
    let bump = ceiling_bytes.get().min(target as u64 + step_bytes);
    inner.update_limit(bump);             // additive increase
} else {
    consecutive_under = 0;                // steady state, no change
}
last_adjust = Instant::now();
```

Decay uses multiplicative-decrease so a stuck downstream sheds
budget quickly (caller is downstream-bound and there is no point
holding tokens). Elevation uses additive-increase so multiple
adaptive streams sharing a path converge on fair shares without
oscillation. `step_bytes` defaults to `ceiling / 16`.

### 4.4 Per-stream isolation under daemon mode

The daemon constructs one `AdaptiveBandwidthLimiter` per accepted
connection in
`crates/daemon/src/daemon/connection_pool/pool.rs` at the point the
`ConnectionInfo` is registered. `ConnectionId` (`types.rs`) is the
stream identifier. Limiters live alongside `ConnectionInfo` in a
sibling `DashMap<ConnectionId, AdaptiveBandwidthLimiter>` so the
existing lock-free read path is preserved. Each stream's window,
debt, and fill-rate are private; the `--bwlimit` ceiling is the
only global constant and is copied into each limiter at
construction. A daemon-wide budget reconciler that redistributes
unused rate across siblings is left as a future extension.

## 5. Integration points

| Site | File and item | Change |
|------|---------------|--------|
| Limiter crate | `crates/bandwidth/src/limiter/mod.rs` | Add `mod adaptive;` and re-export `AdaptiveBandwidthLimiter`. No change to existing exports. |
| Configuration | `crates/core/src/client/config/bandwidth.rs:127` (`to_limiter`) | Add sibling `to_adaptive_limiter()` returning the wrapper when `adaptive` config flag is set. |
| Daemon pool | `crates/daemon/src/daemon/connection_pool/pool.rs:48` (`ConnectionPool`) | Add `limiters: DashMap<ConnectionId, AdaptiveBandwidthLimiter>` and a `register_limiter()` helper called from the existing `register()` path. |
| Stats hook | `crates/protocol/src/stats/transfer.rs:50` (`TransferStats`) | No struct change. The wrapper observes `total_written` deltas externally; counters remain wire-compatible. |
| CLI | `crates/cli/src/...` | Add `--adaptive-bwlimit` boolean (open question, see section 8). |

The existing `BandwidthLimiter` API stays untouched. `LimiterChange`
and `apply_effective_limit` continue to operate on the inner limiter
because daemon-module overrides apply equally to fixed and adaptive
modes.

## 6. Trade-offs

**Token-bucket vs gradient probing.** A gradient probe (BBR-style)
samples RTT and inflight bytes to infer bottleneck bandwidth. It is
more accurate but requires per-frame timestamping, a model of the
bottleneck queue, and far tighter integration with the transport
layer. Closed-loop token-bucket re-uses byte counters we already
emit and stays out of the transport's way. We accept lower
precision in exchange for negligible CPU overhead and zero new
syscalls.

**Per-stream cost.** Memory: one `AdaptiveBandwidthLimiter` is
roughly 200 bytes plus a 64-entry `SampleWindow` (~1 KiB). At 1 000
concurrent daemon connections that is ~1 MiB total - acceptable.
Lock contention: `SampleWindow` and the wrapper are accessed only
by the owning thread (each connection runs on its dedicated
worker), so no synchronisation is needed beyond what `DashMap`
already provides for the pool index.

**Interaction with `--bwlimit` and `--bwlimit-mtu`.** The user-
supplied `--bwlimit` becomes the wrapper's `ceiling_bytes`. The
adaptive controller never exceeds it. `--bwlimit-mtu` (the chunk-
size cap) maps onto the inner limiter's `write_max` and is
recomputed automatically when `update_limit` runs because
`calculate_write_max` is called inside `update_configuration`
(`limiter/core/limiter.rs:91`).

**Risk: miscalibrated decay starves bursty workloads.** A workload
that alternates 4 s of metadata bursts with 4 s of large-file
streaming could decay during the metadata phase and cap itself
before the streaming phase ramps. Mitigations:

- `K`-of-`N` threshold (default `K=3`) prevents single-sample
  decay.
- The floor (`ceiling / 8`) bounds the worst case so a single
  large-file phase regains saturation in a few control steps.
- `--adaptive-bwlimit-window`, `--adaptive-bwlimit-floor`, and
  `--adaptive-bwlimit-step` daemon-config knobs allow operators to
  retune for known workloads without recompilation.

## 7. Implementation phases

Phase tracking aligns with #2098 (convergence tests). Each phase is
its own PR; phases 1-3 are gated by passing convergence tests.

1. **Skeleton.** Land `adaptive` submodule wrapping
   `BandwidthLimiter`, with tests that verify pass-through when
   feedback is disabled. No CLI surface yet.
2. **SampleWindow + AIMD rule.** Use `LimiterSleep` recording
   (`RecordedSleepSession` in `test_support.rs`) to assert
   convergence under steady, bursty, and throttled-downstream
   simulated load.
3. **Daemon wiring.** Plumb per-`ConnectionId` limiters through
   `ConnectionPool`. Add interop test where two daemon connections
   share one `--bwlimit` and jointly converge on the ceiling.
4. **CLI and config plumbing.** Add `--adaptive-bwlimit` and the
   three tuning knobs once the controller is stable.
5. **Benchmarks.** Add a multi-stream daemon workload to
   `scripts/benchmark.sh` and record convergence curves vs the
   fixed-rate baseline.

## 8. Open questions

1. **Default behaviour.** On by default when `--bwlimit` is set, or
   opt-in via `--adaptive-bwlimit`? Lean: opt-in for v1 (preserves
   predictability), revisit after a release of operational data.
2. **Coordination with #2090 (AIMD limiter).** #2090 proposes a
   TCP-style AIMD pacer at the transport layer. Is the controller
   in section 4.3 the same one, or do they live at different layers
   (token-bucket = application pacing, AIMD = transport smoothing)?
   If shared, does the state live in `bandwidth` or `transport`?
3. **Coordination with #2094 (adaptive-buffer controller).** That
   controller already maintains a throughput sample window. Share
   `SampleWindow` (factored into `crates/bandwidth/src/limiter/sample.rs`
   or a sibling crate) to avoid double-instrumentation, or keep
   them independent? Risk of coupling: one bad sample affects two
   control loops.
4. **State-machine coupling.** Should the limiter reset its window
   on transfer-phase transitions
   (`Handshake → FilterExchange → FileListTransfer → DeltaTransfer
   → Finalization → Complete`)? Phase profiles differ; carrying
   samples across the boundary could bias the controller for
   seconds.
5. **Sender-side observability.** The receiver sees delivered
   throughput; the sender sees only what it pushed on the wire.
   Sender-side adaptive limiting needs receiver `total_written`
   deltas via a `MSG_*` side-channel - worth the wire cost, or
   confine adaptive mode to the receiver side in v1?
