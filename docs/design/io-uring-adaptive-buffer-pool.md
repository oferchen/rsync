# Adaptive sizing for the io_uring registered buffer pool

Tracking issue: oc-rsync #2045. Status: design (phase 2 follow-up to the
phase 1 telemetry that already shipped on `RegisteredBufferGroup`).
Audience: maintainers of `crates/fast_io/src/io_uring/`. Scope: replace
the fixed `(buffer_size, count)` tuple held by every
`RegisteredBufferGroup` with a feedback-driven sizer that grows under
miss pressure and shrinks under sustained idleness.

This document is the single design record for registered-buffer
adaptive sizing. It absorbs the former short-form brief that lived at
`docs/design/iouring-adaptive-buffer-pool.md`; the pressure scenarios
and the engine-pool divergence table below came from that brief. The
signal-layer extension and kernel-constraint analysis live in
`docs/design/iouring-registered-buffer-adaptive-sizing.md`, which
defers to this document for every policy parameter. The phase 1
telemetry rationale is `docs/audits/io-uring-adaptive-buffer-sizing.md`.

## Decision of record

Registered-buffer sizing will auto-adapt, and adaptive is the default.
The `--io-uring-adaptive-buffers` flag defaults to `auto`; `off` pins
the static count for reproducibility. That is the direction for the
implementation; it is not a license to build unmeasured.

Implementation is gated on measurements, per the rule in
`docs/design/structural-constant-measurement-gate.md`:

- The registered-buffer miss rate has never been measured on a real
  workload. Reachability first, then a sweep with the miss counters
  exposed and a shrink-the-pool negative control.
- The provided-buffer ring shape (`BufferRingConfig::default` in
  `crates/fast_io/src/io_uring_common.rs`: 64 entries of 64 KiB) is
  arbitrary. Its measurement gate is recorded in the same document; a
  flat sweep without a negative control cannot separate "well sized"
  from "not on the path".
- Measured context: with io_uring enabled, bulk transfers move by
  roughly 0% to +1.2% and high-fanout transfers by about -1.2% on a
  7.1.5 kernel. Buffer sizing is not expected to move headline
  numbers; the payoff case is the daemon-at-scale scenario in
  section 1.3.

A contrast worth recording: the engine-level `BufferPool` already
adapts (grow and shrink via `PressureTracker` in
`crates/engine/src/local_copy/buffer_pool/pressure.rs`), while the
io_uring registered buffers do not adapt today. The
`OC_RSYNC_BUFFER_POOL_SIZE` environment variable tunes the engine
pool's initial capacity only - it does not touch the registered
buffers or the provided-buffer (`PBUF_RING`) ring shape.

## 1. Problem

A registered buffer group is allocated once at ring construction with a
fixed slot count and held for the lifetime of the ring. Under sustained
pressure that fixed size produces one of two failure modes:

- **Under-provisioned.** Every `checkout` returns `None` because all
  slots are in use. The writer falls through to the regular
  `IORING_OP_WRITE` path, paying full per-SQE `get_user_pages()`
  overhead and losing the registered-buffer fast path. Repeated under
  load this is "registration thrash" - we register 8 buffers, never
  exercise the fast path because it is always saturated, and pay the
  pinned-memory cost without the throughput win.
- **Over-provisioned.** The pool holds far more buffers than the
  workload ever demands, pinning memory against `RLIMIT_MEMLOCK`
  without throughput benefit. On embedded targets and constrained
  containers this can push the process over the locked-memory ceiling
  even when the working set is small.

The fallback itself logs nothing, but it is not invisible: the phase 1
telemetry counts every acquire and miss (see section 3). What is
missing is a consumer - no control loop reads the counters, and no
sizing decision ever ties back to them. Phase 2 is that consumer.

### 1.1 100K+ small files

A receiver pulling a tree of 100K small files (4 KiB or less each)
produces back-to-back flushes with no batching window. At the default
count of 8, the slots are sized at 64 KiB apiece. Two pathologies:

- **Slot saturation.** Every flush burns all eight slots before the
  first completion drains. Subsequent flushes hit the
  `available() == 0` branch and fall through to non-registered
  `IORING_OP_WRITE`. The pool is registered and pinned (paying the
  `RLIMIT_MEMLOCK` cost) but never producing throughput.
- **Buffer over-sizing.** A 4 KiB file in a 64 KiB registered buffer
  wastes 60 KiB per slot of pinned memory. The pool occupies
  `8 * 64 KiB = 512 KiB` for a working set that needs 32 KiB.

The `for_small_files` preset improves the buffer size (16 KiB) but
still has eight slots; under 100K-file pressure the saturation pattern
recurs.

### 1.2 Deep flist / deep recursion

Incremental-recursion traversal produces nested directory fan-out
where many files are queued for transfer in parallel. The reader path
is invoked for every basis-file lookup, and under deep flist with
parallel basis-file readers it hits slot saturation on its own ring's
pool, sized identically to the writer's. The reader's miss path is
the more expensive one: basis-file reads block delta computation that
downstream stages depend on, so saturation here adds latency to the
critical path, not just the I/O fan-out.

### 1.3 Daemon thread-per-connection at scale

With one ring per connection (per
`docs/design/iouring-session-ring-pool.md`), 100 concurrent clients
each carry a static pool of `8 * 64 KiB = 512 KiB`, totalling 50 MiB
pinned. If `RLIMIT_MEMLOCK` is at the typical container default of
64 MiB, this leaves 14 MiB of headroom before the next
`register_buffers` call fails. The fixed sizing is wrong in both
directions simultaneously: hot rings are saturated and want to grow;
cold rings are over-provisioned and could shrink to free
`RLIMIT_MEMLOCK` headroom for the hot ones.

## 2. Current sizing in `fast_io`

The sizing inputs are static `IoUringConfig` fields, declared in
`crates/fast_io/src/io_uring_common.rs`:

| Preset | `buffer_size` | `registered_buffer_count` |
|--------|---------------|---------------------------|
| `IoUringConfig::default` | 64 KiB | 8 |
| `IoUringConfig::for_large_files` | 256 KiB | 16 |
| `IoUringConfig::for_small_files` | 16 KiB | 8 |

The hard kernel ceiling is `MAX_REGISTERED_BUFFERS = 1024` in
`crates/fast_io/src/io_uring/registered_buffers/mod.rs`, enforced by
the constructor in `registered_buffers/registry.rs`.

Construction funnels through `RegisteredBufferGroup::try_new` (and
`try_new_with_status`) in
`crates/fast_io/src/io_uring/registered_buffers/registry.rs`. On the
current per-thread ring topology the file writer no longer owns a
group of its own: `IoUringWriter::registered_buffer_count` returns
`None` and `registered_buffer_status` reports
`RegisteredBufferStatus::Disabled` (see
`crates/fast_io/src/io_uring/file_writer.rs`). The live production
constructor is the shared socket ring
(`crates/fast_io/src/io_uring/shared_ring.rs`), gated on
`IoUringConfig::register_buffers`. Any sizer must therefore attach to
the group's owner, not to a specific reader or writer type - which is
what the `RegisteredBufferOwner` trait in section 8 is for.

When no slot is available, submission falls back to the non-fixed
batch path (`submit_write_batch` in
`crates/fast_io/src/io_uring/batching.rs`); the fixed-opcode
counterparts live in
`crates/fast_io/src/io_uring/registered_buffers/submit.rs`.

## 3. The general `BufferPool` grow / shrink telemetry

The engine-level pool already implements the analogous design and is
the template phase 2 follows. It lives at
`crates/engine/src/local_copy/buffer_pool/`; the mechanisms are
documented in the module comments there, so this section only names
the anchors:

- **Hit / miss / growth counters.** Atomic counters on the pool
  (`pool/mod.rs`), snapshotted by `BufferPoolStats`
  (`pool/stats.rs`).
- **Pressure tracker.** `PressureTracker::evaluate` in `pressure.rs`,
  with `MISS_RATE_GROW_THRESHOLD = 0.20`,
  `UTILIZATION_SHRINK_THRESHOLD = 0.30`, `GROW_FACTOR = 2`,
  `SHRINK_DIVISOR = 2`, `MIN_CAPACITY = 2`, `MAX_CAPACITY = 256`,
  checked every `CHECK_INTERVAL = 64` acquires.
- **Resize execution.** `BufferPool::maybe_resize` in
  `pool/resize.rs` swaps the soft capacity atomically and lazily
  reclaims excess buffers, never blocking the hot path.
- **EMA encoding.** The throughput tracker in `throughput.rs` stores
  its EMA as `f64::to_bits` in an `AtomicU64`; the sizer reuses that
  encoding.

The phase 1 telemetry on `RegisteredBufferGroup` mirrors the counter
shape one-to-one: `total_acquires` and `total_misses` on the group
(`registered_buffers/registry.rs`), bumped inside `checkout`, exposed
by `RegisteredBufferStats::miss_rate`
(`crates/fast_io/src/io_uring_common.rs`) with the same "0.0 when no
acquires" convention as `BufferPoolStats`.

## 4. Why the registered-buffer pool is different

The general `BufferPool` is purely userspace: its capacity is a soft
atomic, and shrinking just deallocates `Vec<u8>` instances. The
registered buffer pool is a kernel-side resource. Resizing it crosses
the syscall boundary in two distinct ways:

- **Pinned pages.** Each registered buffer is allocated page-aligned
  and handed to the kernel via `register_buffers` (see the
  constructor in `registered_buffers/registry.rs`). The kernel calls
  `get_user_pages()` and pins those pages until the ring fd closes or
  `unregister_buffers()` is called. These pages count against
  `RLIMIT_MEMLOCK` for the lifetime of the registration; growing the
  pool means moving more pages into the locked set.
- **Slot indices are kernel-side identifiers.** A `buf_index: u16` in
  a `READ_FIXED` / `WRITE_FIXED` SQE refers to a specific entry in
  the registered iovec array. Re-registering reorders the array; any
  in-flight SQE that named an old index would point at the wrong
  buffer (or none at all). Resize is therefore a synchronization
  point: no `READ_FIXED` / `WRITE_FIXED` SQE may be outstanding when
  the registration changes.
- **No incremental update without `IORING_REGISTER_BUFFERS_UPDATE`.**
  The 5.13+ update opcode would let us patch individual slots, but
  it broadens the fork-safety surface (registered pages survive
  `fork()` in surprising ways) and is left out of scope per
  `docs/audits/io-uring-adaptive-buffer-sizing.md` section 6. Phase 2
  performs a full unregister / register cycle.
- **Drop ordering is load-bearing.** The Drop comment in
  `registered_buffers/registry.rs` documents that the ring fd must
  close before the user-side memory is freed; reversing this order is
  sound for cleanup but phase 2 must not rely on that variant when
  swapping a live group on a live ring.

Summarised against the engine template:

| Concern | Engine pool | Registered pool |
|---------|-------------|-----------------|
| Resource | Heap `Vec<u8>` cached in a queue | Page-aligned regions pinned by `IORING_REGISTER_BUFFERS` |
| Shrink shape | Halve, lazy reclaim | 0.75x, syscall-bound |
| Grow threshold | 20% miss rate | 10% miss rate |
| Check interval | 64 acquires | 256 acquires |
| Resize cost | O(1) atomics + lazy reclaim | `unregister` + `register` syscalls plus `get_user_pages()` over the new iovec |
| Memory cap | Soft atomic, not OS-enforced | OS-enforced `RLIMIT_MEMLOCK` |
| Quiescence required | No | Yes - in-flight SQEs reference slot indices |
| Failure on grow | None (`Vec` allocation) | `EAGAIN` / `ENOMEM` from `register_buffers` |

The two pools share a counter shape, an EMA encoding, and a geometric
grow step. Everything else diverges because one is a userspace cache
and the other is a kernel-pinned page set.

## 5. Cost of resize

A resize step performs:

1. Drain any in-flight SQEs that hold a registered slot. The group's
   bitset already exposes `available()`; when
   `available() == count()` no slot is checked out and the ring is
   quiescent for registered ops.
2. Call `unregister_buffers` - a single
   `io_uring_register(IORING_UNREGISTER_BUFFERS)` syscall.
3. Drop the old `RegisteredBufferGroup`, freeing the user-side
   memory.
4. Construct a new group, which allocates `count` page-aligned
   regions and calls `register_buffers` (a `get_user_pages()`-class
   syscall over the new iovec array).
5. Atomically replace the owner's `Option<RegisteredBufferGroup>`
   field.

The dominant cost is step 4: pinning the new buffer set. For
`count = 32, buffer_size = 64 KiB` that is `32 * 64 KiB / 4 KiB = 512`
pages. On a healthy kernel under no contention this is
sub-millisecond, but it is not free and must run off the hot path -
specifically, between batches in the disk-commit loop, never inside
`submit_and_wait`.

There is also a "dropped completions" hazard: if the sampler triggers
a resize while a `READ_FIXED` SQE is in flight, the SQE will fail with
the kernel having released the buffer registration mid-op. The
quiescence check in step 1 prevents this; the sampler must skip the
resize and retry on the next sample window if any slot is checked out.

## 6. Proposed signals

Mirroring the engine's `BufferPoolStats` shape:

- **Hit rate.** `1 - miss_rate` derived from the existing acquire /
  miss counters, already emitted by
  `RegisteredBufferStats::miss_rate`.
- **Miss rate (smoothed).** EMA over a rolling window of acquires,
  using the engine throughput tracker's `f64::to_bits` encoding.
- **Mean wait time.** Time spent between the `available()` check and
  a `checkout` returning a slot during a flush. A sustained non-zero
  mean wait is a classic under-provisioned signal.
- **Peak depth.** Maximum simultaneously-checked-out slots within a
  sample window, tracked by a `peak_in_use: AtomicUsize` on the
  group. `peak_in_use == count` indicates saturation pressure even
  when `miss_rate` is low (every `checkout` succeeds but only because
  the workload happens to release a slot just in time).

Phase 2 adds three new lightweight counters (`peak_in_use`, an EMA
slot, and the cooldown deadline) on `RegisteredBufferGroup`. None
crosses the syscall boundary; each is a single `Relaxed`
`fetch_add` / `fetch_max`. Hot-path overhead is unchanged from phase 1.
The extended signal layer (CQE wait time, exhaustion events, window
saturation) is specified in
`docs/design/iouring-registered-buffer-adaptive-sizing.md`.

## 7. Proposed policy

Mirror the engine pool's threshold structure (section 3) but with
parameters tuned for the kernel-resource cost:

| Parameter | Value | Why |
|-----------|-------|-----|
| `CHECK_INTERVAL` | 256 acquires | Larger than the engine's 64 because a resize is far more expensive. Power of two so the trigger is a bitwise AND. |
| `EMA_ALPHA` | `0.2` | Slightly more reactive than the engine's throughput tracker; the registered pool turns over faster than per-file throughput. |
| `WARMUP_SAMPLES` | 8 | Identical to the engine's pattern. During warmup a cumulative average avoids zero-bias. |
| `GROW_THRESHOLD` | `miss_rate >= 0.10` | Conservative: grow only when at least one in ten acquires fail. The engine pool uses 0.20, but the registered-pool miss path is much more expensive (full fallback to non-registered ops), so we react earlier. |
| `GROW_FACTOR` | `2x` | Geometric growth, identical to the engine's. Linear growth takes too many cooldown-separated decisions to recover from severe under-provisioning; exponential reaches 8 -> 64 in three. |
| `SHRINK_THRESHOLD` | `miss_rate <= 0.005 AND peak_in_use < count / 2` | Shrink only when the pool is dramatically over-provisioned. Adds the peak-depth guard absent from the engine pool because we cannot afford the `unregister`/`register` thrash. |
| `SHRINK_FACTOR` | `0.75x` (round down) | Shrink in smaller steps than we grow; over-shrinking causes immediate re-grow churn under bursty workloads, paying two syscalls where one would have sufficed. |
| `MIN_BUFFERS` | 2 | Same floor as the engine's `MIN_CAPACITY`. One slot forces serialisation. |
| `MAX_BUFFERS` | `min(64, kernel_cap, bgid_cap)` | Soft cap of 64 covers any reasonable workload at `64 * 64 KiB = 4 MiB` per ring. Hard ceiling is `MAX_REGISTERED_BUFFERS = 1024`. The bgid cap interaction is covered in section 10 and #2044. |
| `COOLDOWN_SAMPLES` | `4 * CHECK_INTERVAL` | After any resize, suppress the next decision for 1024 acquires. Prevents grow / shrink / grow oscillation under noisy workloads. |

### Hysteresis

Geometric growth followed by sub-linear shrink already provides decay
asymmetry; that asymmetry is the central design choice, and it needs
no separate grow or shrink weight. The engine pool gets away with
symmetric halving because shrinking a userspace `Vec<u8>` is free; the
registered pool cannot. The explicit cooldown counter records the
value of `total_acquires` at which the next decision is allowed to
fire, with the sampler consulting it before reading new statistics.
This matches the engine pool's "low miss rate AND low utilization"
gate but moves the second condition into a temporal dimension,
recognising that a syscall-class resize cost dominates the threshold
gap.

### Decision algorithm

```text
acq = group.stats().total_acquires
if acq < cooldown_until: return
sample_miss_rate = (misses - last_misses) / (acq - last_acq)
ema = update_ema(ema, sample_miss_rate, EMA_ALPHA)
last_acq, last_misses = acq, misses
if group.available() != group.count():
    return  # not quiescent; defer to next window
if ema >= GROW_THRESHOLD and group.count() < MAX_BUFFERS:
    new_count = min(MAX_BUFFERS, group.count() * GROW_FACTOR)
    resize(group, new_count)
    cooldown_until = acq + COOLDOWN_SAMPLES
elif ema <= SHRINK_THRESHOLD and group.count() > MIN_BUFFERS
        and peak_in_use < group.count() / 2:
    new_count = max(MIN_BUFFERS, (group.count() * 3) / 4)
    resize(group, new_count)
    cooldown_until = acq + COOLDOWN_SAMPLES
```

## 8. API surface sketch

### What the user sees

- The existing `registered_buffer_count` configuration remains the
  upper hint. When phase 2 lands, the value becomes the *initial*
  count; the sizer is allowed to grow up to `MAX_BUFFERS` and shrink
  down to `MIN_BUFFERS`.
- A new flag `--io-uring-adaptive-buffers={auto,off}` (default
  `auto`, per the decision of record). `off` pins the count at
  `registered_buffer_count` for the lifetime of the ring, preserving
  static behaviour for users with reproducibility requirements
  (interop test harnesses, benchmarks). The staged rollout in
  `docs/design/iouring-registered-buffer-adaptive-sizing.md`
  section 9 ships the flag as `off` for one soak release and then
  flips the default to `auto`.
- The diagnostic env var `OC_RSYNC_REGISTERED_BUFFER_STATS=1` mirrors
  the engine pool's `OC_RSYNC_BUFFER_POOL_STATS=1` pattern (see
  `crates/engine/src/local_copy/buffer_pool/pool/mod.rs`). Drained on
  Drop; prints `acquires=N misses=M miss_rate=p% growths=G shrinks=S`.

### Hidden internal surface

- New file `crates/fast_io/src/io_uring/adaptive_buffers.rs`:
  - `pub(crate) struct AdaptiveBufferSizer` carrying the EMA state
    (`AtomicU64` of `f64::to_bits`), the cooldown deadline
    (`AtomicU64`), and the `last_acq` / `last_misses` snapshot.
  - `pub(crate) fn observe(&self, group: &RegisteredBufferGroup)`
    consults `group.stats()` and updates the EMA.
  - `pub(crate) fn maybe_resize(&self, owner: &mut dyn
    RegisteredBufferOwner) -> io::Result<()>` performs the
    unregister / drop / register cycle when the policy fires.
- A `RegisteredBufferOwner` trait abstracts the field swap on
  whichever types own a group (today the shared socket ring; see
  section 2). Dependency Inversion per the project guidance: the
  sizer never names a concrete owner.
- Two new fields on `RegisteredBufferGroup`: `peak_in_use:
  AtomicUsize` and an opaque `cooldown_until: AtomicU64`.

No public API on the existing `RegisteredBufferGroup` changes; phase 2
adds, never breaks.

## 9. Test plan

Unit tests live next to the sizer in
`crates/fast_io/src/io_uring/adaptive_buffers.rs`:

- **Grow on miss saturation.** Feed a synthetic stream of
  `(acquire, miss)` pairs with miss rate `>= 0.10`. Assert
  `group.count()` doubles after the first cooldown-eligible
  evaluation. Stop at `MAX_BUFFERS` and assert no further growth.
- **Shrink on idleness.** Feed all-hits with `peak_in_use < count/2`
  for a full window. Assert `group.count()` shrinks by `0.75x`
  (rounded down). Stop at `MIN_BUFFERS` and assert no further
  shrink.
- **Hysteresis.** Alternate one window above `GROW_THRESHOLD` and
  one below `SHRINK_THRESHOLD`. Assert exactly one resize fires,
  followed by `COOLDOWN_SAMPLES` of held capacity even with
  contradictory signals.
- **Quiescence guard.** Hold a `RegisteredBufferSlot` (via
  `checkout`) across a sampler tick that would otherwise resize.
  Assert no `register` / `unregister` syscall is issued; the next
  sampler tick after the slot drops performs the resize.
- **Property test.** Generate arbitrary
  `Vec<(acquires_delta, misses_delta)>` and assert
  `count` stays in `[MIN_BUFFERS, MAX_BUFFERS]` and the
  monotonicity property `miss_rate_high -> count_non_decreasing`
  holds across the sequence.

Integration scenario in
`crates/fast_io/tests/io_uring_adaptive_buffer_pool.rs`:

- **Sustained pressure.** Construct an owner with the default
  `registered_buffer_count = 8`. Issue 4096 batched writes of
  `data.len() > 8 * buffer_size` so that every flush exhausts the
  slot pool and forces the fallback path. Assert that within 10
  batches the registered count grows to `>= 32` and that subsequent
  batches hit the fixed-opcode submit path
  (`registered_buffers/submit.rs`) rather than the non-fixed batch
  path. Verify the miss rate drops below `GROW_THRESHOLD` after
  stabilization.
- **`RLIMIT_MEMLOCK` regression.** With `prlimit(RLIMIT_MEMLOCK,
  64 KiB)`, assert that grow attempts beyond the limit return the
  buffer set unchanged (the existing `try_new` path swallows
  `ENOMEM`) and that the sizer records the failure and lowers
  `MAX_BUFFERS` for the remaining lifetime of the group.

All tests skip cleanly if ring construction returns an error (CI
without io_uring support), matching the existing pattern in the
`registered_buffers` tests.

## 10. Cross-references

### #2044 (`bgid` namespace bound)

Buffer-group identifiers (`bgid`) are 16-bit kernel-side handles
shared with provided buffers (`PROVIDE_BUFFERS` / `PBUF_RING`). The
audit at `docs/audits/io-uring-bgid-namespace.md` proposes a process-
wide ceiling on simultaneously-allocated `bgid`s, and the lease
mechanism now lives in `crates/fast_io/src/io_uring/bgid_lease.rs`.
Two measured facts bound the risk: exhaustion of the central pool is
reported loudly (see the error paths on the lease), and it has not
been observed in practice - sessions share a single bgid group, so
even 100K sessions consume one identifier, not 100K. The adaptive
sizer must still observe the ceiling: when it would grow past the
per-process `bgid_cap`, the grow request is denied and the sizer
treats that denial identically to an `ENOMEM`. The two designs share
one knob (`MAX_BUFFERS`), and #2044 owns the global accounting; this
design owns the per-ring decision.

### #1735 (adaptive queue depth, the SQ analogue)

`#1735` made the submission queue depth (`sq_entries`) adaptive,
sampling submit-and-wait latency to grow the SQ when the producer
runs ahead of the kernel and shrink it when it lags. The wiring is
analogous: a per-ring sampler runs off the hot path, an EMA smooths
the signal, and the resize is gated by quiescence. The two sizers
should not fight: `MAX_BUFFERS <= sq_entries` is the natural
invariant (you cannot in-flight more registered buffers than SQEs
the ring can carry). Phase 2 reads `sq_entries` from the ring and
clamps `MAX_BUFFERS` accordingly. See
`docs/architecture/reorder-buffer.md` for the queue-depth heuristic,
and `docs/design/adaptive-thread-pool-sizing.md` for the engine
sizer's sibling lineage.

### Phase 1 audit

The phase 1 telemetry rationale (parameter derivation, EMA encoding,
integration checklist) is
`docs/audits/io-uring-adaptive-buffer-sizing.md`. This document
narrows that audit to the phase 2 implementation contract; the audit
remains the source of truth on the phase 1 telemetry that already
shipped.

## 11. Open questions

- **Should resize be ring-driven or sizer-driven?** Today the design
  has the sizer call into the owner. An alternative is a sampler
  that posts a `RESIZE` request into the ring's own work queue, so
  the resize completes between the kernel's natural "no SQE in
  flight" windows. This would simplify the quiescence check but
  introduces a new internal opcode dispatch. Defer until we have
  measured the syscall cost on real workloads.
- **One sizer per ring, or one per process?** Each ring carries
  independent miss-rate state today. A process-wide aggregator
  would let us spend the `RLIMIT_MEMLOCK` budget where it produces
  the most throughput (one hot ring growing while a cold ring
  shrinks). The daemon-at-scale scenario in section 1.3 is the
  cleanest motivation. Consider once the session ring pool
  (`docs/design/iouring-session-ring-pool.md`) has multiple rings to
  coordinate.
- **Should `buffer_size` be adaptive too?** Today only `count` is
  adaptive; `buffer_size` is fixed at ring construction. Resizing
  buffer size means tearing down the ring entirely. Out of scope for
  phase 2 but worth re-examining once ring teardown is cheaper.
- **Telemetry surface for the daemon.** The engine pool's
  `OC_RSYNC_BUFFER_POOL_STATS=1` env var is a good fit for one-shot
  CLI runs but awkward for long-lived daemons. Consider exposing the
  registered-buffer telemetry through the daemon's existing event
  channel rather than via Drop. Tracked separately.
- **Failure mode when grow returns `EAGAIN`.** The kernel may
  transiently fail `register_buffers` under memory pressure even
  when we are below the static ceiling. Should the sizer back off
  permanently or retry on the next window? The audit (section 5.2)
  proposes lowering `MAX_BUFFERS` permanently on `EAGAIN`/`ENOMEM`
  but a hybrid (lower for one window, retry once) might be kinder
  to bursty memory loads. Decide alongside the integration test in
  section 9.
