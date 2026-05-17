# Adaptive BufferPool sizing with EMA throughput feedback (#1834)

Tracking issue: #1834. Audience: maintainers of the engine pipeline
and the `BufferPool` subsystem. Scope: evaluate whether to add an
EMA-throughput-driven feedback loop that shrinks the `BufferPool`'s
soft capacity under low load and grows it under high load, on top of
the existing miss-rate-driven resizer. The conclusion is **defer**;
the rest of this document records the reasoning, the design that
would land if/when the deferral is lifted, and the bench evidence
required to justify the change.

## 1. Current `BufferPool` sizing

The pool is a two-level cache: a per-thread single-slot
`thread_local!` followed by a lock-free
`crossbeam_queue::ArrayQueue<Vec<u8>>` central queue. Storage was
audited in PR #4179 and recorded in
`docs/audits/bufferpool-current-state.md`; the storage path is not
mutex-backed and the count cap is enforced via a CAS admission
counter.

There are three independent sizing knobs already in the pool, none
of which use EMA-throughput feedback:

- **Soft count cap (`max_buffers`)**. Default queue capacity is set
  in `crates/engine/src/local_copy/buffer_pool/pool.rs:34` at
  `DEFAULT_QUEUE_CAPACITY = 256`. The constructor stores the
  caller-requested cap in the atomic `soft_capacity` field
  (`pool.rs:124`, initialised in `pool.rs:209`). The hard capacity of
  the underlying `ArrayQueue` is `max(max_buffers,
  DEFAULT_QUEUE_CAPACITY)` per the helper at `pool.rs:42-44`.
- **Adaptive count resize (miss-rate-driven, #1640/#1641)**.
  Enabled via `with_adaptive_resizing` (`pool.rs:377`). The
  pressure tracker fires every 64 acquires
  (`crates/engine/src/local_copy/buffer_pool/pressure.rs:29`,
  `should_check` at `pressure.rs:107-110`), doubles the soft cap
  when miss rate crosses 20% (`pressure.rs:35`), and halves it
  when utilisation drops below 30% (`pressure.rs:41`). Bounds are
  `MIN_CAPACITY = 2` (`pressure.rs:47`) and `MAX_CAPACITY = 256`
  (`pressure.rs:53`). This loop watches allocation pressure, not
  throughput.
- **Byte-budget retention cap (#2245)**. Enabled via
  `with_byte_budget`
  (`crates/engine/src/local_copy/buffer_pool/pool.rs:327`), backed
  by `ByteBudget` at
  `crates/engine/src/local_copy/buffer_pool/byte_budget.rs:34-41`.
  Bounds total retained bytes, rejecting admissions past the limit
  and incrementing an overflow counter. Solves the failure mode the
  count cap could not express: a handful of 1 MiB adaptive buffers
  blowing past a sane memory budget. It is a static ceiling, not a
  feedback loop.

There are also two existing EMA-style components in the tree, which
matter because they are the building blocks any new feedback loop
would compose on top of:

- **`ThroughputTracker` (EMA over bytes/sec)**.
  `crates/engine/src/local_copy/buffer_pool/throughput.rs:24` sets
  `DEFAULT_ALPHA = 0.1`. The tracker uses an `f64`-bits atomic for
  the EMA state and a CAS loop in `record_transfer`
  (`throughput.rs:132-194`). It already exposes
  `recommended_buffer_size` (`throughput.rs:229`) targeting ~10 ms
  of data per buffer (`throughput.rs:48`,
  `TARGET_BUFFER_DURATION_SECS = 0.01`), clamped to
  `[MIN_BUFFER_SIZE, MAX_BUFFER_SIZE]` =
  `[4 KiB, 256 KiB]` (`throughput.rs:39-42`). This drives *per-buffer
  size*, not *pool capacity*.
- **`AdaptiveBufferController` (PID over throughput, #2095)**.
  `crates/engine/src/local_copy/buffer_pool/buffer_controller.rs:180-184`
  holds the controller; design recorded in
  `docs/design/adaptive-buffer-controller.md`. Same input signal
  (EMA throughput), same output domain (per-buffer size). Slot
  count is untouched.

Net: the soft count cap reacts to miss rate, the byte cap is
static, and the only existing throughput-driven loop adjusts buffer
size, not slot count. The gap this task asks about is "shrink the
pool slot count when throughput is low, grow it when throughput is
high".

## 2. EMA formula

The pool would not allocate a second EMA. It would consume the
existing `ThroughputTracker` value via the accessor at
`pool.rs:472` (`throughput_tracker()`), keeping the single source
of truth:

```text
ema_new = alpha * sample + (1 - alpha) * ema_old
```

With `DEFAULT_ALPHA = 0.1` the effective memory is ~10 samples (the
"time constant" of the EMA is `1 / alpha = 10`). At the sampling
cadence proposed below (one sample per 64 acquires, matching the
existing pressure check), that is roughly 640 acquires of memory.
At the typical hot-path rate of one acquire per file or per chunk,
that is on the order of seconds at modest concurrency and tens of
milliseconds at peak concurrency. Both are acceptable: long enough
to filter per-file jitter, short enough to react inside a single
typical session.

If the smoothing is wrong, the `with_throughput_tracking_alpha`
constructor (`pool.rs:356`) already exposes the override; no new
configuration surface is required.

## 3. Sample source

Each sample is the EMA reading of `ThroughputTracker::throughput_bps`
(`throughput.rs:200`) at the moment the existing pressure check
fires. Three properties make this the right signal:

- It already exists, is lock-free, and is updated on the wire-touching
  path via `record_transfer` (`pool.rs:427`). No new hot-path
  instrumentation is added.
- It is in physical units (bytes/sec) so a setpoint is meaningful
  across workloads. Pool occupancy at sample time is not, because
  occupancy depends on `max_buffers` itself - using it would
  introduce a positive feedback loop.
- It is already smoothed. Adding a second EMA on top would double
  the lag without reducing variance.

What it is *not*: bytes drained from the pool per second. The pool
does not see bytes - it sees buffer hand-outs. Sampling bytes
in flight would require either a separate counter (extra hot-path
work) or reading the existing `record_transfer` argument
(duplicates `ThroughputTracker`). Reusing the tracker is strictly
cheaper.

## 4. Resize trigger

The loop fires inside `PressureTracker::should_check`
(`pressure.rs:107-110`), at the same 64-acquire cadence as the
count-cap resizer. Adding a second timer source is unjustified; the
two loops observe orthogonal signals (miss rate vs throughput EMA)
and act on the same variable (soft cap), so a single trigger is
sufficient and avoids resize-event interleaving.

Setpoint and step:

- The controller maintains a session-local EMA of *peak* throughput
  in addition to the current EMA. The peak is the maximum value the
  current EMA has ever reached this session, decayed by `0.99` per
  sample so it does not stick at a transient burst.
- Define `ratio = current_ema / peak_ema`.
- If `ratio > 0.8` and the current soft cap is below `MAX_CAPACITY`
  (`pressure.rs:53`, 256), double the soft cap (matches the existing
  grow factor at `pressure.rs:56`).
- If `ratio < 0.3` and the current soft cap is above `MIN_CAPACITY`
  (`pressure.rs:47`, 2), halve the soft cap (matches the existing
  shrink divisor at `pressure.rs:59`).
- Else hold. The 0.3-0.8 deadband is the hysteresis.

The deadband matches the existing count-cap thresholds (20% miss to
grow, 30% utilisation to shrink, `pressure.rs:35` and `:41`),
keeping the operator's mental model consistent. The double/halve
step matches the count-cap loop for the same reason. The peak EMA
provides a self-tuning setpoint without requiring the caller to know
their link speed ahead of time.

Anti-thrash:

- Apply the cap change at most once per `CHECK_INTERVAL`
  (`pressure.rs:29`), enforced by reading the existing `ops` counter.
- Skip the throughput evaluation when the EMA is still warming up
  (`is_warming_up` at `throughput.rs:212`).
- Skip when the pressure loop has already issued a `Grow` or `Shrink`
  this interval, so the two loops do not double-step.

## 5. Composition with existing loops

The throughput loop and the miss-rate loop both write to
`soft_capacity` (`pool.rs:124`). To keep the composition obvious:

- The throughput loop runs *after* the pressure loop in the same
  `should_check` callback. The pressure loop's decision wins for
  this interval; the throughput loop only acts when the pressure
  loop returned `Hold`.
- Both loops are clamped to `[MIN_CAPACITY, MAX_CAPACITY]`, so the
  cumulative effect is bounded by the same caps already in use.
- `with_buffer_controller` (`pool.rs:409`) is unaffected. The PID
  controller sizes individual buffers, not the slot count, so it
  composes with both feedback loops without conflict.

## 6. Bench evidence needed

Two benches must show no regression *and* a measurable improvement
before this loop ships:

- `crates/engine/benches/buffer_pool_benchmark.rs` (the lock
  contention bench from PR #4179, contention-only, fixed
  `POOL_CAPACITY = 32` at `buffer_pool_benchmark.rs:23`). The
  throughput loop must not regress the 1-, 4-, 8-, 16-thread
  acquire/release numbers by more than 2%, because those measure
  the hot path and the feedback loop adds work to the cold path
  (the 64-acquire check) only.
- `crates/engine/benches/buffer_pool_contention.rs` (mixed
  workload). A new bench variant `ema_feedback` must demonstrate
  that under a stepped-throughput workload (1 MB/s for N seconds,
  then 100 MB/s for N seconds, then 1 MB/s again), the loop:
  - Reduces peak retained bytes by at least 25% during the low-load
    segments, measured via `total_byte_overflows` and the
    `ByteBudget::retained` accessor
    (`byte_budget.rs:65-67`).
  - Does not reduce achieved throughput on the high-load segment by
    more than 2% relative to the static-cap baseline.

If either threshold is violated, the loop does not ship.

We do not yet have the *throughput baseline* on the contention bench
that the second threshold requires. Establishing that baseline is
prerequisite work and is the explicit reason for the deferral in
section 7.

## 7. Recommendation

**Defer.** Three reasons:

1. **No baseline to regress against.** The `buffer_pool_contention`
   bench (`crates/engine/benches/buffer_pool_contention.rs`) does
   not yet record per-thread throughput; PR #4179 measured
   acquire/release rate and hit/miss telemetry, not bytes/sec
   delivered. Without that number we cannot show the proposed loop
   improves anything, only that it does not crash.
2. **Existing loops already cover most of the win.** The count-cap
   loop (#1640/#1641) already shrinks the pool when the workload
   goes quiet (low miss rate implies low demand implies shrink
   path). The byte budget (#2245) already prevents the
   over-allocation failure mode the throughput loop would also
   target. The remaining gap is the steady-state case where miss
   rate is healthy but the workload's throughput is below peak,
   which is real but narrow.
3. **A live, more sophisticated controller exists.** The
   `AdaptiveBufferController` (#2095) consumes the same EMA signal
   and adjusts *buffer size*. Adding a second throughput-driven
   loop that adjusts *slot count* doubles the controller surface
   the operator has to reason about. Justifying both requires
   evidence neither alone is sufficient, which we do not have.

Concrete next step: extend `buffer_pool_contention.rs` (gap #1
above) to record bytes/sec, then run a stepped-throughput workload
against the current pool. If the data shows >5% retained-memory
waste during low-load segments that the count-cap loop fails to
reclaim, revisit this design and implement the loop per sections
2-5. If not, close #1834 as not-needed and reference this audit
plus #4179.

## 8. Cross-references

- #1297 - sharding audit precondition for the pool storage layer.
- #1370, #1681 - gated refactor follow-ups blocked on the same
  baseline data.
- #1642 - prior throughput-tracker work that introduced the EMA
  the loop would consume.
- #2245 - byte-budget retention cap; complementary, already shipped.
- #4179 - audit of the current `BufferPool` storage and bench
  surface (`docs/audits/bufferpool-current-state.md`).
- `docs/design/adaptive-buffer-controller.md` - PID controller
  over the same throughput signal, sizing individual buffers
  rather than slot count.
