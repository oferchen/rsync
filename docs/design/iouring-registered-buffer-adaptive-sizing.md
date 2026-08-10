# io_uring registered buffer adaptive sizing under sustained pressure

Tracking issue: oc-rsync #2045. Status: design (signal layer). Audience:
maintainers of `crates/fast_io/src/io_uring/`. Scope: the
`IORING_REGISTER_BUFFERS` slot-count sizing problem under sustained
back-pressure, focused on the signals and kernel constraints that the
two existing companion documents intentionally leave open.

## 0. Relationship to existing documents

This is the third document in the #2045 series. It does not restate the
sizing policy that the prior two already specify; it covers the gaps:

- `docs/design/io-uring-adaptive-buffer-pool.md` (23 KiB, phase 2
  specification) defines the EMA-smoothed `miss_rate` policy, the
  parameter table, the `AdaptiveBufferSizer` API sketch, the test plan,
  and the cross-references to #2044 and #1735. It treats "sustained
  pressure" as a synonym for "miss rate above `GROW_THRESHOLD`".
- `docs/design/iouring-adaptive-buffer-pool.md` (13 KiB, design brief)
  summarises the four-question form, contrasts the engine `BufferPool`
  lineage (#1638 / #1640 / #1641), and quantifies the `RLIMIT_MEMLOCK`
  budget at daemon scale.

Both lean on a single signal (acquire / miss counters) and assume the
fixed-buffer registration slot is the only kernel-resource axis. This
document widens the signal set, expands the kernel-constraint section
beyond `RLIMIT_MEMLOCK`, gives concrete trigger numbers for signals
that the prior two leave qualitative, and produces an explicit
recommendation on when (or whether) to implement this work relative to
the in-flight ring pool (#1937).

What this document does NOT redefine: `CHECK_INTERVAL`, `EMA_ALPHA`,
`GROW_THRESHOLD`, `SHRINK_THRESHOLD`, `GROW_FACTOR`, `SHRINK_FACTOR`,
`MIN_BUFFERS`, `MAX_BUFFERS`, `COOLDOWN_SAMPLES`, the
`AdaptiveBufferSizer` shape, or the CLI flag layout. Those are the
contract owned by `io-uring-adaptive-buffer-pool.md` section 7. The
sub-millisecond resize cost calculation in that document's section 5
remains the authoritative cost estimate; this document does not
re-derive it.

## 1. The registered-vs-generic gap

The closest sibling work is the engine-side adaptive `BufferPool`
(`crates/engine/src/local_copy/buffer_pool/`), which is sometimes
described as covering "the same problem". It does not. Four properties
make the registered pool different from the generic pool, and only one
(miss-rate plumbing) is shared:

| Axis | Generic `BufferPool` | Registered buffer group |
|------|----------------------|--------------------------|
| Resource | Heap `Vec<u8>` cached in `ArrayQueue` | Page-aligned regions pinned by `IORING_REGISTER_BUFFERS` |
| Shrink mechanism | Lazy `ArrayQueue::pop` on next acquire (`pool.rs:667`) | Synchronous `IORING_UNREGISTER_BUFFERS` + new `IORING_REGISTER_BUFFERS` syscall pair |
| Miss penalty | One `Vec::with_capacity` allocation | Full fallback to `IORING_OP_WRITE` (no `WRITE_FIXED`), losing the registered-buffer fast path entirely |
| Index stability across resize | None - opaque `Vec<u8>` | Required - in-flight `READ_FIXED` / `WRITE_FIXED` SQEs reference `buf_index: u16` |
| Accounting | Soft `AtomicUsize` (`pool.rs:110`) | OS-enforced `RLIMIT_MEMLOCK` |

The first two existing documents address axes 2, 3, and 5. Axis 4
(index stability) is treated as a quiescence pre-condition rather than
a design space; it is the axis that opens up `IORING_REGISTER_BUFFERS_UPDATE`
(see section 6). The `CQE wait time` signal in section 4 below is the
first one that exploits a property unique to the registered pool: the
miss path here is so much more expensive than a generic-pool miss that
we want to react before the miss happens, not after.

## 2. Current static sizing model

The pool is sized once at ring construction by a single `try_new` call.
Field declarations and call sites at the current tree:

- `IoUringConfig::registered_buffer_count` field:
  `crates/fast_io/src/io_uring_common.rs:107` (declared `pub usize`).
- Default value `8`:
  `crates/fast_io/src/io_uring_common.rs:123`.
- `for_large_files` preset value `16`:
  `crates/fast_io/src/io_uring_common.rs:142`.
- `for_small_files` preset value `8`:
  `crates/fast_io/src/io_uring_common.rs:159`.
- `MAX_REGISTERED_BUFFERS = 1024` kernel ceiling:
  `crates/fast_io/src/io_uring/registered_buffers.rs:80`, enforced at
  `registered_buffers.rs:233`.
- Page-aligned allocation site:
  `crates/fast_io/src/io_uring/registered_buffers.rs:241` (`Layout`)
  and `registered_buffers.rs:254` (`alloc_zeroed`).
- Kernel registration:
  `crates/fast_io/src/io_uring/registered_buffers.rs:276`
  (`submitter().register_buffers(&iovecs)`).
- Call sites that turn the static config into a live group:
  `crates/fast_io/src/io_uring/file_writer.rs:65,91,132,185`,
  `crates/fast_io/src/io_uring/file_reader.rs:82`,
  `crates/fast_io/src/io_uring/shared_ring.rs:175`,
  `crates/fast_io/src/io_uring/mod.rs:230,254`.
- Telemetry that already ships (phase 1 of #2045):
  `RegisteredBufferStats` at
  `crates/fast_io/src/io_uring_common.rs:399`, `miss_rate` at
  `io_uring_common.rs:411`, `total_acquires` / `total_misses` atomics
  at `crates/fast_io/src/io_uring/registered_buffers.rs:118` and
  `registered_buffers.rs:121`, bumped inside `checkout` at
  `registered_buffers.rs:386` (every entry) and `registered_buffers.rs:410`
  (the `None` exit).
- Snapshot getter: `RegisteredBufferGroup::stats` at
  `crates/fast_io/src/io_uring/registered_buffers.rs:426`.

There is no `ring_size_hint` field on `IoUringConfig`. The closest
proxy is `sq_entries` at `io_uring_common.rs` (default `64`, large
preset `256`, small preset `128`), which bounds how many SQEs - and
therefore how many in-flight `WRITE_FIXED` SQEs - can be outstanding.
The natural invariant `registered_buffer_count <= sq_entries` is
documented by `io-uring-adaptive-buffer-pool.md` section 10 as the
#1735 cross-reference but is not enforced anywhere in code today.

The `BufferRing` (`PBUF_RING`) path at
`crates/fast_io/src/io_uring/buffer_ring.rs:111` carries its own
`ring_size` field validated to be a power of two; it is a separate
mechanism (provided buffers, not registered buffers) and is not the
subject of this document. The `BgidAllocator` at
`buffer_ring.rs:174` is the analogous resource controller for that
mechanism. See `docs/design/io-uring-bgid-namespace.md` for the bgid
namespace audit and section 7 below for the interaction.

## 3. Failure mode under sustained pressure

The existing documents describe slot saturation as "every checkout
returns `None` because all slots are in use". That description is
necessary but not complete. Under sustained pressure with all slots in
flight, three distinct failure shapes occur and the existing miss-rate
EMA only detects the first cleanly:

### 3.1 Steady-state miss (detected today)

Every flush burns all eight slots before the first completion drains.
Subsequent `checkout` calls return `None`, the writer falls through to
`submit_write_batch` at `file_writer.rs:248`, and the miss counter
ticks up. EMA-smoothed miss rate rises above `GROW_THRESHOLD = 0.10`
within roughly `CHECK_INTERVAL / EMA_ALPHA = 256 / 0.2 = 1280`
acquires. The phase 2 design already handles this case.

### 3.2 Just-in-time saturation (currently invisible)

Every `checkout` succeeds because the previous flush released a slot
exactly in time, but `available()` dwells at or near zero for the
entire window. `total_misses` stays at zero. Miss-rate EMA reports the
pool is healthy; in reality the pool is one tail-latency event away
from regime 3.1. Peak-depth tracking (`peak_in_use == count` for a full
window, sketched in `io-uring-adaptive-buffer-pool.md` section 6) is
the signal that fires here, but neither existing document specifies
when to sample `available()`. Section 4.2 below proposes the sampling
rule.

### 3.3 CQE drain stall (currently invisible)

The writer holds a slot, submits a `WRITE_FIXED` SQE, and waits inside
`submit_and_wait`. The kernel returns the CQE more slowly than the
producer wants. Acquires and misses are both quiescent; the writer is
idle. This is back-pressure from the kernel side - typically a slow
block device, a saturated NVMe queue, or a contended writeback path -
and growing the pool makes it worse (more in-flight SQEs against the
same bottleneck). The signal here is not miss rate; it is
*completion latency*, the time between SQE submission and CQE arrival.
The existing documents do not address this case. Section 4.1 below
proposes the signal and an explicit grow-suppression rule.

The three shapes can be distinguished by the (acquire-rate, miss-rate,
CQE-wait) triple. Growing the pool helps in 3.1, helps modestly in
3.2, and actively hurts in 3.3.

## 4. Signal layer

Three signals beyond the existing acquire / miss counters. All three
are cheap (single `Relaxed` atomic per event, two atomics per sample
window).

### 4.1 CQE wait time (`mean_cqe_wait_ns`)

Time inside `ring.submit_and_wait(n)` averaged over the sample window.
The natural instrumentation point is the existing
`submit_write_fixed_batch` at
`crates/fast_io/src/io_uring/registered_buffers.rs:615` and
`submit_read_fixed_batch` at `registered_buffers.rs:496` - the call to
`submit_and_wait` is the only blocking point in the registered path.

Implementation: wrap the `submit_and_wait` call with
`Instant::now()` before and after; accumulate elapsed nanoseconds into
an `AtomicU64` on `RegisteredBufferGroup` and a separate counter for
the number of wait events. The sample window reads both, divides, and
resets to zero (or uses a delta-snapshot like the existing
`last_acq` / `last_misses` pattern).

Trigger interaction:

- **`mean_cqe_wait_ns > GROW_SUPPRESS_THRESHOLD = 500 us`.** The
  kernel is the bottleneck. Growing the pool adds more in-flight SQEs
  against the same slow device, which deepens the CQE queue and
  increases tail latency. The sizer must SUPPRESS grow even if
  `miss_rate >= GROW_THRESHOLD`. Shrink remains allowed.
- **`mean_cqe_wait_ns < SHRINK_SUPPRESS_THRESHOLD = 50 us`.** The
  kernel is keeping up. Normal grow / shrink rules apply.

The 500 us / 50 us numbers are derived from the slot turnover budget:
at default `buffer_size = 64 KiB` and a baseline NVMe write at
1 GiB/s, a single 64 KiB write completes in ~64 us. Three to eight
times that (200 us - 500 us) signals queueing rather than service
time. Below 50 us the device is essentially idle and the registered
path is the bottleneck. Both numbers are wall-clock derived from the
buffer-size constant; they MUST be recomputed if `buffer_size` ever
becomes adaptive (the existing design defers this; see
`io-uring-adaptive-buffer-pool.md` section 11).

### 4.2 Slot exhaustion counter (`exhaustion_events`)

A counter that increments whenever `available()` transitions from
`>= 1` to `0`. Distinct from `total_misses`: a `total_misses` increment
counts every `checkout` that fails, but a single saturation event can
produce many of those. The exhaustion counter measures how many times
the pool actually ran dry within the window, which is the signal
section 3.2 needs.

Implementation: an `AtomicU64` on `RegisteredBufferGroup`, bumped from
`return_slot` and `checkout` when the bitset transition crosses zero.
Cost is one extra `compare_exchange` per slot return; under contention
this is dwarfed by the bitset CAS already present at
`registered_buffers.rs:396`.

Trigger interaction:

- **`exhaustion_events >= 4` within one `CHECK_INTERVAL` window**
  promotes the grow decision even when `miss_rate < GROW_THRESHOLD`.
  Four crossings per 256 acquires (~1.5%) signals a workload that is
  consistently hitting the ceiling without the smoothed miss rate
  ever rising above the threshold (the regime 3.2 case).

The "4" is the analogue of `WARMUP_SAMPLES = 8` in the parent design;
small enough to react to a sustained pattern, large enough to ignore
isolated bursts.

### 4.3 Sustained backpressure window (`window_saturation`)

A boolean derived from `peak_in_use == count` *for the entire sample
window*. Computed by maintaining a `peak_in_use_low_watermark` -
the minimum of `peak_in_use` snapshots within the window. When that
low watermark equals `count`, every observation within the window saw
the pool at full depth, and the workload is one slow CQE away from
regime 3.1.

Implementation: a `peak_in_use_low: AtomicUsize` initialised to
`count`, updated on every `checkout` and `return_slot` with the
current `count - available()` value via `fetch_min`. Reset to `count`
at the start of each window.

Trigger interaction:

- **`window_saturation == true AND mean_cqe_wait_ns < 500 us`**
  promotes grow. Sustained full depth without device backpressure is
  the cleanest signal that the next miss is imminent.
- **`window_saturation == false AND peak_in_use < count / 2 for 4
  consecutive windows`** is the existing shrink signal made temporal:
  the design brief (`iouring-adaptive-buffer-pool.md` section 3.1)
  calls for "a full window" of `peak_in_use < count / 2`; this
  document tightens it to four windows to suppress a single quiet
  burst from triggering an `unregister`/`register` syscall pair.

## 5. Shrink signal: idle slot ratio over a window

The existing design uses `miss_rate <= SHRINK_THRESHOLD = 0.005 AND
peak_in_use < count / 2`. The gap: `peak_in_use` is captured at a
single moment per window and a noisy workload can produce one quiet
window followed by one busy window, neither of which triggers shrink
under the existing rule but which together represent steady use.

Tightening:

- **Idle-slot ratio `r = sum_window(available()) / sum_window(count)`,
  sampled at the same `CHECK_INTERVAL` cadence.** Approximated cheaply
  by accumulating `available()` snapshots at every `return_slot` (the
  natural release point) and dividing by the number of snapshots at
  window close. Cost: one `AtomicU64::fetch_add` per slot return.
- **Shrink eligibility: `r >= 0.6 AND miss_rate <= 0.005 AND
  window_saturation == false for 4 consecutive windows`.** The 0.6
  ratio means at least 60% of observed capacity sat idle; combined
  with the four-window history this resists single-burst-then-quiet
  workloads.

The four-window requirement is the cooldown analogue from the shrink
side: the existing `COOLDOWN_SAMPLES = 4 * CHECK_INTERVAL` suppresses
the *next decision after* a resize, while this rule suppresses the
*first decision* unless the workload has been consistently idle.
Together they make the shrink path conservative without making it
unreachable.

## 6. Kernel constraints beyond `RLIMIT_MEMLOCK`

The existing documents cover `RLIMIT_MEMLOCK` thoroughly. Three further
kernel-side constraints shape the design and are not covered.

### 6.1 `IORING_REGISTER_BUFFERS_UPDATE` (kernel 5.13+)

The 5.13 `io_uring_register(2)` opcode `IORING_REGISTER_BUFFERS_UPDATE`
(opcode 13) accepts an `io_uring_rsrc_update2` describing a *range*
of slots to replace in place. Two properties matter:

- **No global quiescence required.** Only the slots in the update
  range must be free; SQEs in flight against slots outside the range
  are undisturbed. The existing design's "drain all in-flight SQEs"
  pre-condition (`io-uring-adaptive-buffer-pool.md` section 5 step 1)
  becomes "drain the slots being changed", which a careful shrink can
  arrange without ever stopping the writer.
- **Reservation semantics.** The update path replaces the iovec entry
  for the named slot; if the new iovec is shorter, the prior pinning
  is dropped. Growing requires extending the iovec array, which the
  current `register_buffers` interface does not support without a full
  re-register. The `_UPDATE` path therefore supports incremental
  *shrink* but not incremental *grow*: a grow still needs the full
  `unregister` / `register` cycle.

Cost model: a buffer update is a single `io_uring_register` syscall
against the named slot range; it does NOT invoke `get_user_pages()` on
slots being released (those pages are already pinned and simply
unpinned). A shrink from 16 to 12 slots costs four pinning releases,
no allocations, and no fresh registrations. This is roughly an order
of magnitude cheaper than the full-cycle cost in
`io-uring-adaptive-buffer-pool.md` section 5.

Phase 2 caveat: the parent design (section 4 third bullet)
intentionally leaves `_UPDATE` out of scope, citing the
fork-safety surface broadening called out in
`docs/audits/io-uring-adaptive-buffer-sizing.md` section 6. This
document agrees with that decision for the FIRST implementation but
recommends `_UPDATE` for the shrink path in a follow-up once the basic
grow / full-cycle pipeline has run in CI for one release. The
asymmetric cost (cheap shrink, expensive grow) maps onto the
asymmetric trigger rates (rare grow, common shrink across daemon
lifetime) cleanly.

Kernel-version gate: `KernelVersion::probe()` at
`crates/fast_io/src/kernel_version.rs` should be extended with an
`IoUringFeature::RegisterBuffersUpdate` probe that runs at
`AdaptiveBufferSizer::new` time and caches the result. The
`_UPDATE` path is a strict refinement; absence of the feature falls
back to the full-cycle shrink already specified in
`io-uring-adaptive-buffer-pool.md` section 5.

### 6.2 Reservation cost is the `get_user_pages()` walk

The full-cycle register cost is dominated not by the syscall itself
but by the kernel-side `get_user_pages()` walk that pins each page in
the new iovec into the kernel address space. For
`count = 32, buffer_size = 64 KiB` the walk covers 512 pages; on a
typical 6.x kernel with no contention this is roughly 100 us - 300 us
plus a TLB flush. Under memory pressure (the kernel's LRU is busy)
the walk can stretch to milliseconds. Two implications:

- **The `submit_and_wait` thread must never run a resize.** The full-
  cycle resize cost is bounded above by tens of milliseconds in the
  worst case; that is well within the existing 50 us - 500 us CQE-wait
  thresholds in section 4.1 and would itself trigger spurious back-
  pressure suppression. The sizer must run on a dedicated path (the
  one the parent design already specifies, but this document explicitly
  forbids inlining the resize into the SQE submission path).
- **Grow under memory pressure should escalate to "permanent cap
  lower"**, matching the `MAX_BUFFERS` clamping that the parent
  design already prescribes for `EAGAIN`/`ENOMEM`. Add: a single grow
  that takes > 5 ms wall-clock should ALSO clamp `MAX_BUFFERS` to the
  pre-grow value, even if it succeeded. A slow grow indicates that
  the next grow will also be slow and the gain does not justify the
  cost.

### 6.3 `RLIMIT_MEMLOCK` per-process vs per-namespace

`getrlimit(RLIMIT_MEMLOCK)` is per-process. In a container with
multiple oc-rsync invocations sharing a host (typical for benchmark
matrices and CI), the kernel-side accounting is per-process at the
syscall level, but the *available* memory is bounded by cgroup
`memory.max`. A grow that fits `RLIMIT_MEMLOCK` but pushes the cgroup
over `memory.max` triggers an OOM kill rather than an `ENOMEM` return
from `register_buffers`. The mitigation:

- **`MAX_BUFFERS` clamping should consider `cgroup memory.current`
  headroom on Linux**, read from
  `/sys/fs/cgroup/memory.max` and
  `/sys/fs/cgroup/memory.current` (cgroup v2). When
  `(memory.max - memory.current) / buffer_size < proposed_grow_delta`,
  refuse the grow and lower `MAX_BUFFERS` for the remaining lifetime
  of the group.
- **Cache the cgroup paths once at startup**; do not re-read them per
  sample window. Falls back gracefully on hosts without cgroup v2
  (return "unbounded" headroom).

This is the corner of the design where the per-process daemon scenario
(`iouring-adaptive-buffer-pool.md` section 2.3) cleanly composes with
the bgid namespace audit (`docs/design/io-uring-bgid-namespace.md`):
the bgid namespace is a kernel-side `u16`, the registered-buffer
budget is a per-cgroup memory bound, and a healthy adaptive sizer must
respect both.

## 7. Concrete trigger thresholds

The full grow / shrink decision summarised as a single rule:

```text
sample = read_signals(group)  # 5 signals, 5 atomic reads

if sample.acq < cooldown_until:
    return  # parent-design cooldown

# Grow path
grow_pressure = false
if sample.ema_miss_rate >= 0.10:                    # 4.1 (existing)
    grow_pressure = true
if sample.exhaustion_events >= 4:                   # 4.2 (new)
    grow_pressure = true
if sample.window_saturation:                        # 4.3 (new)
    grow_pressure = true

if sample.mean_cqe_wait_ns > 500_000:               # 4.1 (new)
    grow_pressure = false  # SUPPRESS: kernel-side back-pressure

if grow_pressure and group.count() < MAX_BUFFERS:
    if not group.is_quiescent():
        return  # defer to next window
    resize(group, min(MAX_BUFFERS, group.count() * 2))
    cooldown_until = sample.acq + COOLDOWN_SAMPLES
    return

# Shrink path
if sample.idle_slot_ratio < 0.6:                    # 5 (new)
    return
if sample.ema_miss_rate > 0.005:
    return
if sample.window_saturation:
    return
if sample.consecutive_idle_windows < 4:             # 5 (new)
    return
if group.count() <= MIN_BUFFERS:
    return

# Shrink eligible
if kernel.supports_register_buffers_update:         # 6.1
    incremental_shrink(group, max(MIN_BUFFERS, (group.count() * 3) / 4))
else:
    full_cycle_shrink(group, max(MIN_BUFFERS, (group.count() * 3) / 4))
cooldown_until = sample.acq + COOLDOWN_SAMPLES
```

Threshold summary:

| Threshold | Value | Origin |
|-----------|-------|--------|
| `ema_miss_rate >= ?` grow | `0.10` | inherited from `io-uring-adaptive-buffer-pool.md` section 7 |
| `exhaustion_events >= ?` window grow | `4` per `CHECK_INTERVAL = 256` | this document section 4.2 |
| `window_saturation` grow | `peak_in_use == count` for full window | this document section 4.3 |
| `mean_cqe_wait_ns >= ?` grow SUPPRESS | `500_000` (500 us) | this document section 4.1 |
| `mean_cqe_wait_ns <= ?` normal regime | `50_000` (50 us) | this document section 4.1 |
| `idle_slot_ratio >= ?` shrink eligible | `0.60` | this document section 5 |
| `consecutive_idle_windows >= ?` shrink | `4` | this document section 5 |
| Grow time-bound -> permanent `MAX_BUFFERS` cap | `> 5 ms` | this document section 6.2 |
| Cgroup memory headroom check | `(max - current) / buffer_size` | this document section 6.3 |

Parameters NOT redefined here: `CHECK_INTERVAL = 256`,
`EMA_ALPHA = 0.2`, `WARMUP_SAMPLES = 8`, `GROW_FACTOR = 2`,
`SHRINK_FACTOR = 0.75`, `MIN_BUFFERS = 2`,
`MAX_BUFFERS = min(64, kernel_cap, bgid_cap)`,
`COOLDOWN_SAMPLES = 4 * CHECK_INTERVAL`. Owned by the parent design.

## 8. Recommendation: defer until #1937 (session ring pool) lands

This document recommends **DEFER** the registered-buffer adaptive
sizing implementation until `docs/design/iouring-session-ring-pool.md`
(#1937) is merged. Three reasons:

1. **One-ring sizing has limited daemon impact.** The daemon-at-scale
   scenario in `iouring-adaptive-buffer-pool.md` section 2.3 is the
   strongest motivation for adaptive sizing: a 100-client daemon
   should let hot rings grow while cold rings shrink. But that
   redistribution only matters when there are multiple rings to
   redistribute *between*. Today there is one ring per writer / reader
   / shared-ring factory site; the session ring pool (#1937) is what
   creates the multi-ring world where adaptive sizing pays off.
   Implementing the sizer first means tuning it against a one-ring
   workload where the optimum is well-approximated by the small-files
   preset (`registered_buffer_count = 8`).

2. **The `MAX_BUFFERS` formula has an unresolved term.**
   `MAX_BUFFERS = min(64, kernel_cap, bgid_cap)` cites the bgid cap
   from #2044. The bgid audit ships with the session ring pool
   because that is where the bgid namespace actually gets pressure.
   Implementing adaptive sizing before bgid accounting forces a
   placeholder for the bgid term, which becomes a migration burden
   when #2044 finally lands.

3. **The CQE-wait signal in section 4.1 needs per-ring isolation to
   be meaningful.** A single shared ring's CQE-wait time aggregates
   over every concurrent transfer through that ring; an EMA over that
   mixed signal cannot distinguish "slow device" from "fan-out".
   Per-ring CQE wait (one ring per active transfer, or per worker)
   gives the signal a clean meaning. The session ring pool is what
   makes per-ring CQE wait a useful signal rather than a process-wide
   noise floor.

The alternative recommendations and why they are rejected:

- **Implement now** (rejected). The implementation cost is real (the
  parent design's section 8 lists a new module, a new trait, three
  new fields on `RegisteredBufferGroup`, a new CLI flag, an env var,
  and an integration test) and the workload that justifies it does
  not yet exist in tree. Carrying the sizer through one or two
  releases without exercise is how subtle bugs accumulate.
- **Reject entirely** (rejected). The signal layer specified here is
  cheap (five atomic counters), the kernel constraints are real, and
  the daemon-at-scale workload that justifies the work IS coming.
  Permanently shelving it would mean re-doing this audit when #1937
  lands.

Defer is the correct call. This document goes into the design folder
now so the signal layer and kernel-constraint analysis are recorded;
the implementation waits.

## 9. Implementation sequencing (five steps, post-#1937)

Each step is a separate PR with green CI and is independently
revertable. No step adds public API on the existing
`RegisteredBufferGroup` that would break callers.

### Step 1: Signal-layer instrumentation

Add the three new counters on `RegisteredBufferGroup`:

- `peak_in_use_low: AtomicUsize` (section 4.3).
- `exhaustion_events: AtomicU64` (section 4.2).
- `cqe_wait_ns: AtomicU64` + `cqe_wait_events: AtomicU64` (section
  4.1, wrapped around the `submit_and_wait` call sites in
  `submit_read_fixed_batch` / `submit_write_fixed_batch`).

Extend `RegisteredBufferStats` at
`crates/fast_io/src/io_uring_common.rs:399` with the three derived
fields (`exhaustion_events`, `mean_cqe_wait_ns`, `peak_in_use_low`).
No policy changes; the sizer is not yet present. CI runs the existing
test suite; new unit tests assert each counter ticks where expected.
Bench harness measures the per-checkout overhead under contention.

### Step 2: Kernel-feature probes

Extend `crates/fast_io/src/kernel_version.rs` with an
`IoUringFeature::RegisterBuffersUpdate` probe (5.13+, section 6.1)
and a cgroup-v2 path probe (section 6.3). Both cached via `OnceLock`.
No behavioural change; the probe results are read by step 4 and 5.
Unit tests use `tempfile::TempDir` to stage synthetic cgroup files
for the cgroup probe.

### Step 3: `AdaptiveBufferSizer` skeleton

Add `crates/fast_io/src/io_uring/adaptive_buffers.rs` per the parent
design section 8 with the EMA / cooldown state. Implement `observe`
as a read-only function; do not yet wire `maybe_resize`. CLI flag
`--io-uring-adaptive-buffers={auto,off}` plumbed but
defaults to `off`. Env var
`OC_RSYNC_REGISTERED_BUFFER_STATS=1` activated to dump the new
counters on Drop. Allows operators to observe the signal layer in
production for one release without any resize risk.

### Step 4: Full-cycle resize wired

Implement `maybe_resize` using the parent design's full
`unregister` / `register` cycle. Decision rule per section 7 above
(including the CQE-wait suppression and the cgroup headroom check).
Default `--io-uring-adaptive-buffers=off`; opt-in only. Integration
test in `crates/fast_io/tests/io_uring_adaptive_buffer_pool.rs`
per the parent design section 9 plus the regime 3.2 / 3.3 cases
specified in section 3 here.

### Step 5: `_UPDATE` shrink fast path

Add the kernel 5.13+ `IORING_REGISTER_BUFFERS_UPDATE` shrink path
gated by the probe from step 2. Grow remains full-cycle. After one
release of soak time, flip the CLI default to `auto`. The grow
remains full-cycle; if a future kernel exposes an in-place grow
opcode, that becomes a sixth step.

Each step lands behind CI on stable Rust with the existing
fast_io feature gating. None require changes to the wire protocol or
upstream-rsync interop tests.

## 10. References

- Phase 1 audit: `docs/audits/io-uring-adaptive-buffer-sizing.md`.
- Phase 2 parent design: `docs/design/io-uring-adaptive-buffer-pool.md`.
- Phase 2 design brief: `docs/design/iouring-adaptive-buffer-pool.md`.
- bgid namespace: `docs/design/io-uring-bgid-namespace.md`, #2044.
- Session ring pool: `docs/design/iouring-session-ring-pool.md`, #1937.
- Adaptive queue depth: #1735 (SQ analogue).
- Engine `BufferPool` template: `crates/engine/src/local_copy/buffer_pool/`,
  PRs #1638, #1640, #1641.
- Kernel `io_uring_register(2)` man page, sections
  `IORING_REGISTER_BUFFERS`, `IORING_REGISTER_BUFFERS_UPDATE`,
  *Locked Memory Accounting*.
- cgroup v2 memory accounting: `Documentation/admin-guide/cgroup-v2.rst`,
  `memory.max` / `memory.current` interface files.
