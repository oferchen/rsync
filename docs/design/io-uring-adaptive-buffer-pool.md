# Adaptive sizing for the io_uring registered buffer pool

Tracking issue: oc-rsync #2045. Status: design (phase 2 follow-up to the
phase 1 telemetry that already shipped on `RegisteredBufferGroup`).
Audience: maintainers of `crates/fast_io/src/io_uring/`. Scope: replace
the fixed `(buffer_size, count)` tuple held by every
`RegisteredBufferGroup` with a feedback-driven sizer that grows under
miss pressure and shrinks under sustained idleness. Sibling tasks: #2042
(recycle bounds, completed), #2043 (`PBUF_RING` probe, completed), #2044
(`bgid` namespace, design pending), #1739 (registered ring rings,
completed), #1735 (adaptive queue depth, completed).

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

Both modes are silent. There is no log, no fallback counter that ties
back to a sizing decision, and no way for the operator to see "the
registered fast path was hit 99% of the time" or "we paid the
registration cost for nothing". The phase 1 work in #2045 already added
acquire / miss telemetry; phase 2 is the consumer of that telemetry.

## 2. Current sizing in `fast_io`

The pool is built per ring in three writer call sites and one reader
call site, each reading directly from `IoUringConfig`:

- `crates/fast_io/src/io_uring/file_writer.rs:56` (`IoUringWriter::create`).
- `crates/fast_io/src/io_uring/file_writer.rs:83` (`IoUringWriter::from_file`).
- `crates/fast_io/src/io_uring/file_writer.rs:118` (`IoUringWriter::with_ring`).
- `crates/fast_io/src/io_uring/file_writer.rs:144`
  (alternate factory entry).
- `crates/fast_io/src/io_uring/file_reader.rs:74` (`IoUringReader::open`).
- `crates/fast_io/src/io_uring/shared_ring.rs:268` (the shared-ring
  factory used by `iouring-session-ring-pool`).

Every site funnels through:

```text
RegisteredBufferGroup::try_new(&ring, config.buffer_size,
                               config.registered_buffer_count)
```

defined at `crates/fast_io/src/io_uring/registered_buffers.rs:352`.

The static slot count lives in `IoUringConfig` as a single `usize`:

- Field declaration: `crates/fast_io/src/io_uring/config.rs:326`
  (`pub registered_buffer_count: usize`).
- General-purpose default: `crates/fast_io/src/io_uring/config.rs:339`
  (`registered_buffer_count: 8`).
- Large-files preset: `crates/fast_io/src/io_uring/config.rs:356` (16).
- Small-files preset: `crates/fast_io/src/io_uring/config.rs:371` (8).
- Hard kernel ceiling: `crates/fast_io/src/io_uring/registered_buffers.rs:80`
  (`const MAX_REGISTERED_BUFFERS: usize = 1024`), enforced at
  `crates/fast_io/src/io_uring/registered_buffers.rs:264`.

The fallback path that fires whenever `available()` returns 0 lives at
`crates/fast_io/src/io_uring/file_writer.rs:248` (and the symmetric
read site at `crates/fast_io/src/io_uring/file_writer.rs:311`); it
silently switches to non-registered `submit_write_batch`, leaving no
trace beyond the eventual telemetry counters added in phase 1.

## 3. The general `BufferPool` grow / shrink telemetry

The engine-level pool already implements the analogous design and is
the template phase 2 follows. Citations are to the current tree, not
to the original PRs:

- **Hit / miss / growth counters** (#1639). Atomic `AtomicU64` fields
  on the pool itself, declared at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:137`
  (`total_hits`), `pool.rs:142` (`total_misses`), and `pool.rs:147`
  (`total_growths`). Bumped on the hot path inside `pop_buffer` at
  `pool.rs:622` (hit branch) and `pool.rs:633` (miss branch). The
  `BufferPoolStats` snapshot type at `pool.rs:868` exposes the three
  counters with `Relaxed` reads.
- **Pressure tracker with grow trigger** (#1640). The miss-rate signal
  is computed in
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:120`
  (`PressureTracker::evaluate`) and the grow threshold is
  `MISS_RATE_GROW_THRESHOLD = 0.20` at `pressure.rs:35`. The grow
  step doubles capacity (`GROW_FACTOR = 2` at `pressure.rs:56`)
  capped by `MAX_CAPACITY = 256` at `pressure.rs:53`.
- **Pressure tracker with shrink trigger** (#1641). The shrink branch
  uses `UTILIZATION_SHRINK_THRESHOLD = 0.30` at `pressure.rs:41`,
  divides capacity by `SHRINK_DIVISOR = 2` at `pressure.rs:59`, and
  floors at `MIN_CAPACITY = 2` at `pressure.rs:47`. The shrink branch
  also gates on a low miss rate to avoid shrinking a pool whose slots
  are all checked out (the "demand high, available 0" trap), at
  `pressure.rs:155`.
- **Amortized cadence.** The check fires every `CHECK_INTERVAL = 64`
  acquires (`pressure.rs:29`), tested by power-of-two AND at
  `pressure.rs:107` (`should_check`). Between checks the only
  per-acquire cost is two `Relaxed` `fetch_add` calls.
- **Resize execution.** `BufferPool::maybe_resize` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:650` swaps the
  soft capacity atomically and lazily reclaims excess buffers via
  `ArrayQueue::pop`, never blocking the hot path.

The phase 1 telemetry on `RegisteredBufferGroup` mirrors these
counters one-to-one: see `total_acquires` at
`crates/fast_io/src/io_uring/registered_buffers.rs:118` and
`total_misses` at `registered_buffers.rs:121`, both bumped inside
`checkout` at `registered_buffers.rs:388` (every entry) and
`registered_buffers.rs:412` (the `None` exit). The `RegisteredBufferStats`
snapshot at `registered_buffers.rs:145` exposes `miss_rate` at
`registered_buffers.rs:157`, with the same "0.0 when no acquires"
convention used by `BufferPoolStats::hit_rate` at `pool.rs:891`.

## 4. Why the registered-buffer pool is different

The general `BufferPool` is purely userspace: its capacity is a
soft `AtomicUsize` (`pool.rs:110`), and shrinking just deallocates
`Vec<u8>` instances popped from a `crossbeam_queue::ArrayQueue`. The
registered buffer pool is a kernel-side resource. Resizing it requires
crossing the syscall boundary in two distinct ways:

- **Pinned pages.** Each registered buffer is allocated page-aligned
  via `std::alloc::alloc_zeroed` at
  `crates/fast_io/src/io_uring/registered_buffers.rs:285`, then handed
  to the kernel via `submitter().register_buffers(&iovecs)` at
  `registered_buffers.rs:307`. The kernel calls `get_user_pages()` and
  pins those pages until the ring fd closes or
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
- **Drop ordering is load-bearing.** The Drop comment at
  `crates/fast_io/src/io_uring/registered_buffers.rs:18` documents
  that the ring fd must close before the user-side memory is freed;
  reversing this order is sound for cleanup but Phase 2 must not
  rely on that variant when swapping a live group on a live ring.

The general `BufferPool` shrink can be lazy because excess buffers in
the central queue are reclaimed on the next pop (`pool.rs:667`). The
registered pool cannot be lazy: a slot left registered after a shrink
decision still pins kernel pages until we explicitly call
`unregister_buffers`.

## 5. Cost of resize

A resize step performs:

1. Drain any in-flight SQEs that hold a registered slot. The pool's
   bitset (`crates/fast_io/src/io_uring/registered_buffers.rs:116`)
   already exposes `available()` at `registered_buffers.rs:372`;
   when `available() == count()` no slot is checked out and the ring
   is quiescent for registered ops.
2. Call `RegisteredBufferGroup::unregister` at
   `crates/fast_io/src/io_uring/registered_buffers.rs:448`. This is a
   single `io_uring_register(IORING_UNREGISTER_BUFFERS)` syscall.
3. Drop the old `RegisteredBufferGroup`, freeing the user-side
   memory in the Drop impl at
   `crates/fast_io/src/io_uring/registered_buffers.rs:453`.
4. Construct a new group via `RegisteredBufferGroup::new` at
   `crates/fast_io/src/io_uring/registered_buffers.rs:251`, which
   allocates `count` page-aligned regions and calls
   `register_buffers` (a `get_user_pages()`-class syscall over the
   new iovec array).
5. Atomically replace the `Option<RegisteredBufferGroup>` field on
   the writer / reader (declared at `file_writer.rs:41`,
   `file_reader.rs:40`, and `shared_ring.rs:205`).

The dominant cost is step 4: pinning the new buffer set. For
`count = 32, buffer_size = 64 KiB` that is `32 * 64 KiB / 4 KiB = 512`
pages. On a healthy 6.x kernel under no contention this is
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
  miss counters at
  `crates/fast_io/src/io_uring/registered_buffers.rs:118` and
  `registered_buffers.rs:121`. Already emitted by the phase 1
  `RegisteredBufferStats::miss_rate` at `registered_buffers.rs:157`.
- **Miss rate (smoothed).** EMA over a rolling window of acquires.
  Identical encoding pattern to
  `crates/engine/src/local_copy/buffer_pool/throughput.rs` (one
  `AtomicU64` holding `f64::to_bits`).
- **Mean wait time.** Time elapsed inside `flush_buffer` between the
  `available()` check and the `checkout` returning a slot. Captured
  cheaply by the existing call site at
  `crates/fast_io/src/io_uring/file_writer.rs:283` because it
  already wraps the loop in a known scope. The signal is "did this
  flush spend any time hunting for a slot, or did the first
  `checkout` return immediately?". A sustained non-zero mean wait is
  a classic under-provisioned signal.
- **Peak depth.** Maximum simultaneously-checked-out slots within a
  sample window. Tracked by a `peak_in_use: AtomicUsize` on the
  group, updated whenever `count() - available()` rises above the
  current peak. Compared against `count()` at sampler time:
  `peak_in_use == count` indicates saturation pressure even when
  `miss_rate` is low (every `checkout` succeeds but only because the
  workload happens to release a slot just in time).

Phase 2 adds three new lightweight counters (`peak_in_use`, an EMA
slot, and the cooldown deadline) on `RegisteredBufferGroup`. None
crosses the syscall boundary; each is a single `Relaxed`
`fetch_add` / `fetch_max`. Hot-path overhead is unchanged from phase 1.

## 7. Proposed policy

Mirror the engine pool's threshold structure (cited above) but with
parameters tuned for the kernel-resource cost:

| Parameter | Value | Why |
|-----------|-------|-----|
| `CHECK_INTERVAL` | 256 acquires | Larger than the engine's 64 (`crates/engine/src/local_copy/buffer_pool/pressure.rs:29`) because a resize is far more expensive. Power of two so the trigger is a bitwise AND. |
| `EMA_ALPHA` | `0.2` | Slightly more reactive than the engine's throughput tracker; the registered pool turns over faster than per-file throughput. |
| `WARMUP_SAMPLES` | 8 | Identical to the engine's pattern. During warmup a cumulative average avoids zero-bias. |
| `GROW_THRESHOLD` | `miss_rate >= 0.10` | Conservative: grow only when at least one in ten acquires fail. The engine pool uses 0.20 (`pressure.rs:35`), but the registered-pool miss path is much more expensive (full fallback to non-registered ops), so we react earlier. |
| `GROW_FACTOR` | `2x` | Geometric growth, identical to `GROW_FACTOR = 2` at `pressure.rs:56`. |
| `SHRINK_THRESHOLD` | `miss_rate <= 0.005 AND peak_in_use < count / 2` | Shrink only when the pool is dramatically over-provisioned. Adds the peak-depth guard absent from the engine pool because we cannot afford the `unregister`/`register` thrash. |
| `SHRINK_FACTOR` | `0.75x` (round down) | Shrink in smaller steps than we grow; over-shrinking causes immediate re-grow churn under bursty workloads. |
| `MIN_BUFFERS` | 2 | Same floor as `MIN_CAPACITY = 2` at `pressure.rs:47`. One slot forces serialisation. |
| `MAX_BUFFERS` | `min(64, kernel_cap, bgid_cap)` | Soft cap of 64 covers any reasonable workload at `64 * 64 KiB = 4 MiB` per ring. Hard ceiling is `MAX_REGISTERED_BUFFERS = 1024` from `registered_buffers.rs:80`. The bgid cap interaction is covered in section 10 and #2044. |
| `COOLDOWN_SAMPLES` | `4 * CHECK_INTERVAL` | After any resize, suppress the next decision for 1024 acquires. Prevents grow / shrink / grow oscillation under noisy workloads. |

### Hysteresis

Geometric growth followed by linear shrink already provides decay
asymmetry. The explicit cooldown counter records the value of
`total_acquires` at which the next decision is allowed to fire, with
the sampler consulting it before reading new statistics. This pattern
matches the engine pool's "low miss rate AND low utilization" gate at
`pressure.rs:155` but moves the second condition into a temporal
dimension, recognising that resize cost (a syscall) is far higher
than the engine pool's lazy `ArrayQueue::pop` reclaim.

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

- The existing CLI tunability flag `--io-uring-registered-buffer-count`
  remains the upper hint. When phase 2 lands, the value becomes the
  *initial* count; the sizer is allowed to grow up to `MAX_BUFFERS`
  and shrink down to `MIN_BUFFERS`.
- A new flag `--io-uring-adaptive-buffers={auto,off}` (default
  `auto`). `off` pins the count at `registered_buffer_count` for the
  lifetime of the ring, preserving today's behaviour for users with
  reproducibility requirements (interop test harnesses, benchmarks).
- The diagnostic env var `OC_RSYNC_REGISTERED_BUFFER_STATS=1` mirrors
  the engine pool's `OC_RSYNC_BUFFER_POOL_STATS=1` pattern at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:850`. Drained on
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
  `IoUringWriter`, `IoUringReader`, and `SharedRing`. Dependency
  Inversion per the project guidance: the sizer never names a
  concrete owner.
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

- **Sustained pressure.** Construct an `IoUringWriter` with the
  default `registered_buffer_count = 8`. Issue 4096 batched writes
  of `data.len() > 8 * buffer_size` so that every flush exhausts
  the slot pool and forces the fallback path. Assert that within
  10 batches the registered count grows to `>= 32` and that
  subsequent batches hit `submit_write_fixed_batch`
  (`crates/fast_io/src/io_uring/registered_buffers.rs:617`) rather
  than `submit_write_batch` from `batching.rs`. Verify the
  miss rate drops below `GROW_THRESHOLD` after stabilization.
- **`RLIMIT_MEMLOCK` regression.** With `prlimit(RLIMIT_MEMLOCK,
  64 KiB)`, assert that grow attempts beyond the limit return the
  buffer set unchanged (the existing `try_new` path at
  `registered_buffers.rs:352` swallows `ENOMEM`) and that the
  sizer records the failure and lowers `MAX_BUFFERS` for the
  remaining lifetime of the group.

All tests skip cleanly if `IoUring::new` returns an error (CI
without io_uring support), matching the existing pattern at
`crates/fast_io/src/io_uring/registered_buffers.rs:729`.

## 10. Cross-references

### #2044 (`bgid` namespace bound)

Buffer-group identifiers (`bgid`) are 16-bit kernel-side handles
shared with provided buffers (`PROVIDE_BUFFERS` / `PBUF_RING`). The
audit at `docs/audits/io-uring-bgid-namespace.md` proposes a process-
wide ceiling on simultaneously-allocated `bgid`s. The adaptive sizer
must observe that ceiling: when it would grow past the per-process
`bgid_cap`, the grow request is denied and the sizer treats that
denial identically to an `ENOMEM`. The two designs share one knob
(`MAX_BUFFERS`), and #2044 owns the global accounting; this design
owns the per-ring decision.

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
`docs/architecture/reorder-buffer.md:173` for the queue-depth
heuristic, and the engine sizer's role in
`docs/design/adaptive-thread-pool-sizing.md:47` which already cites
both `#1640`/`#1641` and `#1735` as siblings.

### Phase 1 audit

The full design (parameter table, EMA encoding, integration
checklist) is at `docs/audits/io-uring-adaptive-buffer-sizing.md`.
This document narrows that audit to the phase 2 implementation
contract; the audit remains the source of truth on phase 1
telemetry that already shipped.

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
  shrinks). Consider once `iouring-session-ring-pool`
  (`docs/design/iouring-session-ring-pool.md`) lands and there are
  multiple rings to coordinate.
- **Should `buffer_size` be adaptive too?** Today only `count` is
  adaptive; `buffer_size` is fixed at ring construction
  (`Layout::from_size_align` at
  `crates/fast_io/src/io_uring/registered_buffers.rs:273`). Resizing
  buffer size means tearing down the ring entirely. Out of scope for
  phase 2 but worth re-examining once the registered-ring-pool work
  in #1739 makes ring teardown cheaper.
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
