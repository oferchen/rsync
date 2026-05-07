# Adaptive Buffer-Sizing Controller (PID-like)

Tracking issue: oc-rsync #2094. Status: design RFC. Sibling tasks:
#2095 (implementation), #2096 (convergence tests), #2093
(`--adaptive-buffers` CLI flag, companion to the AIMD concurrency
limiter flag). Audience: maintainers of the engine pipeline,
transfer disk-commit path, and protocol multiplex writer. Scope:
specify a feedback-driven controller that resizes per-pipeline I/O
buffer windows in response to observed wire throughput, layered on
top of the existing pressure-driven `BufferPool` capacity logic.

## 1. Motivation

The existing `BufferPool` already adapts its slot count to allocation
pressure. The grow path (#1640) expands the lock-free `ArrayQueue`
when the miss rate crosses a threshold; the shrink path (#1641)
releases excess buffers when utilisation drops. That loop is
sensitive to "how often did we have to allocate a buffer" but blind
to "how fast are bytes leaving the pipeline". Two failure modes
follow.

Under-sized buffers. The wire delta path is sliced to upstream's
`CHUNK_SIZE = 32 * 1024` (see
`crates/protocol/src/wire/delta/types.rs:4`) and the multiplex writer
defaults to a 32 KB `IO_BUFFER_SIZE`
(`crates/protocol/src/multiplex/writer.rs:81-82`). The disk-commit
SPSC channel (`crates/transfer/src/disk_commit/config.rs:29-37`,
`crates/transfer/src/pipeline/spsc.rs`) is sized once at session
start. On a long-haul, low-loss link these constants leave the
sender bottlenecked on syscall rate, not bandwidth, and the receiver
spends its time waiting for the next 32 KB chunk to land.

Over-sized buffers. On a slow or contended link those same constants
under-utilise the wire but still pin RSS, and once `--max-alloc=N`
(`crates/cli/src/frontend/arguments/parsed_args/mod.rs:468-474`,
implemented via #1837) approaches its ceiling the pool starts to
queue rather than allocate, dragging effective throughput down with
no signal to anything that could shrink the per-pipeline window in
response.

The grow/shrink loop in #1638-#1641 cannot disambiguate these cases
because it observes the wrong variable. Adding a second loop that
observes throughput, with a controller designed for that signal, is
the next step.

## 2. Background

A PID controller produces a manipulated variable `u(t)` from the
error `e(t) = r(t) - y(t)` between a setpoint `r(t)` and a measured
process variable `y(t)`:

```
u(t) = K_p * e(t) + K_i * integral(e) dt + K_d * de/dt
```

- The proportional term `K_p` reacts to the current error. Pure P
  control leaves a steady-state offset whenever the actuator must
  hold a non-zero `u` to keep `e` at zero (Wikipedia, "PID
  controller", section "Proportional"; Aström and Hägglund, "PID
  Controllers: Theory, Design, and Tuning", 2nd ed., chapter 3).
- The integral term `K_i` accumulates past error and drives the
  steady-state offset to zero, but on its own oscillates: pure I
  control is unstable for almost any plant with delay (Aström and
  Hägglund, chapter 3.4, "Integral action and reset windup").
- The derivative term `K_d` reacts to the rate of change of error
  and damps overshoot, but amplifies measurement noise and is
  usually low-pass filtered before use (Aström and Hägglund,
  chapter 3.5, "Derivative action").

PID is the right family for this problem because the plant - "more
buffer means more bytes per syscall, up to the link's capacity" -
has both a steady-state offset (we want to sit at the link limit,
not below it) and a rate component (we want to catch link-quality
changes without overshooting into RSS pressure). Pure proportional
under-fills the link; pure integral hunts.

## 3. Design

### 3.1 Signals

- Setpoint `r`: target throughput in bytes per second. Default is
  the EMA-tracked observed peak from
  `crates/engine/src/local_copy/buffer_pool/throughput.rs`, capped
  by `--bwlimit` when set. The controller drives towards `r`, not
  past it.
- Process variable `y`: observed throughput, sampled from the same
  `ThroughputTracker`. The tracker already emits an EMA over a
  configurable smoothing factor and switches from cumulative
  average to EMA after a warmup window (see the doc comment block
  in `throughput.rs`).
- Manipulated variable `u`: per-pipeline buffer window size in
  bytes, clamped to `[16 KiB, 4 MiB]`. The lower bound is below
  the protocol `CHUNK_SIZE` of 32 KB so the controller can choose
  to send sub-chunk-sized batches when the link cannot absorb a
  full chunk; the upper bound keeps a single in-flight buffer
  under the L2 cache size of typical x86_64 and aarch64 cores.

### 3.2 Loop body

```
e        = r - y
P        = K_p * e
I        = clamp(I_prev + K_i * e * dt, -I_max, I_max)
D        = K_d * (e - e_prev) / dt
u_raw    = u_base + P + I + D
u_next   = round_to_pow2(clamp(u_raw, 16 KiB, 4 MiB))
e_prev   = e
I_prev   = I
```

`u_base` is the previous accepted `u`, not the initial default;
this is a velocity-form update so reset events do not punch the
buffer size back to the static default mid-transfer. The next
buffer size is rounded to the next power of two before being
applied, both because the underlying `BufferPool` slot allocator
already operates on power-of-two sizes and because it provides
free hysteresis: small `u_raw` jitter does not produce a resize
event.

### 3.3 Sample interval

The controller fires on whichever of the following triggers first:

- 64 completed write batches have flushed through the multiplex
  writer or the disk-commit thread, or
- 100 ms of wall time has elapsed since the last sample.

The batch trigger keeps the controller responsive on fast links
where 100 ms is a long time. The time trigger keeps it responsive
on slow or quiescent links where 64 batches may never complete.
The two-trigger pattern matches the existing `ThroughputTracker`
sampling cadence and avoids adding a second timer source.

### 3.4 Anti-windup

The integral term is clamped to `[-I_max, I_max]` with `I_max`
chosen so that `K_i * I_max` cannot exceed half the buffer-size
range. Without the clamp, a long stall (e.g. a slow disk pause, an
SSH reconnect, or `--bwlimit` throttling) accumulates an integral
that takes seconds of fast wire to bleed off, which manifests as
the buffer overshooting its sane range as soon as the stall ends.
This is the textbook reset-windup case (Aström and Hägglund,
chapter 3.4); the clamp is the textbook fix.

### 3.5 Reset events

The controller resets `I_prev`, `e_prev`, and `u_base` to defaults
when:

- A protocol renegotiation completes (new session, possibly new
  peer capabilities).
- The pipeline switches between local-copy and network paths
  mid-session (rare, but possible with `--copy-dest` cascades).
- The user-visible `--max-alloc` ceiling is hit; in this case the
  controller is paused until the pool reports capacity again.

## 4. Integration points

The controller lives in a new
`crates/engine/src/pipeline/buffer_controller.rs` module and is
consumed by three call sites. The engine crate is the right home
because it already owns the `BufferPool` and is the lowest layer
that sees both wire and disk traffic.

- `crates/transfer/src/disk_commit/config.rs` and
  `crates/transfer/src/disk_commit/process.rs`. The controller
  publishes a recommended channel capacity that the disk-commit
  process consults at the existing
  `effective_channel_capacity()` clamp boundary (see
  `disk_commit/config.rs:118-124`). The default `channel_capacity`
  becomes a starting point, not a fixed value.
- `crates/engine/src/local_copy/buffer_pool/`. The controller
  publishes a per-pipeline buffer size that the existing
  `recommended_buffer_size` accessor (see
  `buffer_pool/throughput.rs:216-244`) overrides when adaptive mode
  is enabled. The pool's own grow/shrink path keeps running; the
  controller affects the size of each slot, not the count of
  slots.
- `crates/protocol/src/multiplex/writer.rs`. The controller
  publishes a recommended frame-batching threshold that the
  multiplex writer consults before deciding whether to coalesce
  pending small writes. The default `IO_BUFFER_SIZE` of 32 KB
  remains the upstream-compatible value at session start; the
  controller can grow it but never below the floor required by the
  wire format.

The controller exposes a single `BufferController::recommend()`
method returning a `BufferRecommendation { window: usize,
batch_size: usize, channel_capacity: usize }`. Each call site
extracts the field it needs. The controller is `Send + Sync`,
backed by atomics, and never blocks; the recommendation is
advisory and stale reads are safe (the call site clamps).

## 5. Trade-offs

PID vs MIMD-on-throughput. Multiplicative-increase / multiplicative-
decrease driven by throughput is simpler and converges fast, but
oscillates around the optimum because it has no notion of "we are
already at the right size". PID with a derivative term damps that
oscillation. The cost is two extra gains to tune and one extra
clamp to maintain; we accept this cost because buffer sizing
errors are quadratic (too small starves the wire, too large
starves the cache) and a damped controller pays off.

PID vs raw EMA. The existing throughput tracker is an EMA. EMA
smooths a measurement; it does not generate a correction. Wiring
the EMA value directly back into buffer sizing is the
`recommended_buffer_size` heuristic that already exists in
`throughput.rs:216-244`, and it is what we are layering on. The
PID controller does not replace the EMA; it consumes the EMA and
emits a correction that asymptotically settles at zero error.

Risks.

- Untuned gains oscillate. Mitigation: the gains live in a
  centralised constants module, are validated against synthetic
  workloads in #2096, and the entire controller is gated behind
  `--adaptive-buffers` until convergence is demonstrated. Default
  off.
- Interaction with `--max-alloc=N`. When the cap is reached the
  pool returns backpressure, which the controller would otherwise
  read as "the link is slow" and continue to grow `u`. The reset
  rule in section 3.5 pauses the controller in this case.
- Interaction with `--bwlimit`. The setpoint is capped at
  `--bwlimit` when set so the controller does not spend integral
  budget chasing a target it cannot reach.
- Interaction with the existing pool grow/shrink path
  (#1638-#1641). The two loops observe orthogonal signals
  (allocation pressure vs throughput) and act on orthogonal
  variables (slot count vs slot size), so they compose. We confirm
  composition with the convergence tests in #2096.

## 6. Tuning strategy

Initial gains use the Ziegler-Nichols closed-loop method (Aström
and Hägglund, chapter 6.2): drive `K_p` up with `K_i = K_d = 0`
until the loop oscillates with period `T_u` at gain `K_u`, then
set `K_p = 0.6 * K_u`, `K_i = 1.2 * K_u / T_u`, `K_d = 0.075 * K_u
* T_u`. We run this against three synthetic workloads:

- 1 GiB single-file LAN copy (high bandwidth, low jitter).
- 100K small files over SSH (high syscall rate, modest bandwidth).
- 1 GiB single-file copy with `--bwlimit=10M` (capped link).

The resulting gains become the workload-tagged defaults in the
constants module. Operators selecting an unusual workload pick a
preset via `--adaptive-buffers=lan|wan|throttled`; the default is
`lan`. The flag itself is the companion to `--adaptive-concurrency`
in #2093.

## 7. Implementation phases

Phase 1 (#2095). Add `BufferController` and the
`BufferRecommendation` type, wire the three integration points
behind a feature flag, and ship the LAN preset. Per-pipeline state
lives in the existing transfer config; controller state lives in
the engine pipeline module.

Phase 2 (#2096). Convergence tests against the three synthetic
workloads above. Tests assert that `u` settles within 5% of the
optimum within 1 s of a step change in link bandwidth, that the
controller does not exceed `--max-alloc` even under reset-windup
stress, and that the wire format is byte-for-byte unchanged
relative to the static-buffer baseline.

Phase 3. Production roll-out: flip `--adaptive-buffers=lan` to the
default after the WAN and throttled presets have a release of
field data. Until then, the static defaults remain.

## 8. Open questions

- Single global controller vs per-stream. The current proposal is
  one controller per active transfer. A daemon serving N
  concurrent connections would have N controllers. An alternative
  is a shared controller across all connections, which converges
  faster on a uniform workload but mis-tunes when one connection
  is on a fast link and another is on a slow one. We pick
  per-stream and revisit once #2097 (per-stream token-bucket with
  throughput feedback loop) has data.
- Reset on protocol renegotiation. Section 3.5 specifies this, but
  the renegotiation path on `oc-rsync` is rare (it happens during
  protocol downgrade for upstream `rsync 3.0.9` interop) and the
  reset cost is a few hundred milliseconds of re-convergence. We
  may decide to skip the reset and let the integral term absorb
  the change. Decided in #2095 once the renegotiation path is
  instrumented.
- Interaction with io_uring registered buffer sizing
  (#2045). The registered buffer pool in
  `crates/fast_io/src/io_uring/registered_buffers.rs` has its own
  adaptive sizing proposal; both layers want to vary the same
  underlying memory. The current plan is for the engine controller
  to publish the *target* size and the io_uring layer to round
  that down to the nearest pre-registered group. Confirmed once
  #2045 lands.
- Should the controller emit a metric. A counter for "number of
  resize events per session" and a gauge for "current `u`" would
  be cheap (atomics already exist) and useful for operator
  diagnosis. Defer until #2095 wires the metrics sink.
