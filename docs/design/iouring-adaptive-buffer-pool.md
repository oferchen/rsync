# io_uring registered buffer pool: adaptive sizing brief

Tracking issue: oc-rsync #2045. Status: design brief. Companion to the
full design at `docs/design/io-uring-adaptive-buffer-pool.md` (the
phase 2 specification) and the audit at
`docs/audits/io-uring-adaptive-buffer-sizing.md` (the phase 1
telemetry rationale). This brief is the short-form summary the
follow-on review chain asked for: tight scope, four answers, no
re-derivation of the parameter table.

The four questions this brief answers:

1. Where does the pool live and how is it sized today?
2. What pressure scenarios make the current sizing wrong?
3. What trigger, growth shape, and ceiling should the resize use?
4. How does this relate to the engine `BufferPool` design lineage
   (#1638, #1640, #1641)?

## 1. Pool location and current sizing

The registered buffer group is owned by `RegisteredBufferGroup` in
`crates/fast_io/src/io_uring/registered_buffers.rs`. It is created at
ring construction by every reader, writer, and shared ring:

- `crates/fast_io/src/io_uring/file_writer.rs:56`
  (`IoUringWriter::create`).
- `crates/fast_io/src/io_uring/file_writer.rs:83`
  (`IoUringWriter::from_file`).
- `crates/fast_io/src/io_uring/file_writer.rs:118`
  (`IoUringWriter::with_ring`).
- `crates/fast_io/src/io_uring/file_writer.rs:144` (alternate factory).
- `crates/fast_io/src/io_uring/file_reader.rs:74`
  (`IoUringReader::open`).
- `crates/fast_io/src/io_uring/shared_ring.rs:268` (the shared-ring
  factory used by the session ring pool).

Every site funnels through
`RegisteredBufferGroup::try_new(&ring, config.buffer_size,
config.registered_buffer_count)` at
`crates/fast_io/src/io_uring/registered_buffers.rs:352`.

The sizing inputs are static `IoUringConfig` fields:

| Field / preset | `buffer_size` | `registered_buffer_count` |
|----------------|---------------|---------------------------|
| `IoUringConfig::default` (`config.rs:339`) | 64 KiB | 8 |
| `IoUringConfig::for_large_files` (`config.rs:347`) | 256 KiB | 16 |
| `IoUringConfig::for_small_files` (`config.rs:362`) | 16 KiB | 8 |
| Hard kernel cap (`registered_buffers.rs:80`) | n/a | 1024 |

**Today the pool is fixed at startup.** There is no resize path. The
only signal that the pool is too small is the silent fallback to
`submit_write_batch` / `submit_read_batch` when `available()` returns
0 (call sites at `file_writer.rs:248` and `file_reader.rs:179`). The
phase 1 telemetry shipped in #2045 added the acquire / miss counters
(`registered_buffers.rs:118`, `registered_buffers.rs:121`), bumped on
every `checkout` (`registered_buffers.rs:388`,
`registered_buffers.rs:412`). The `RegisteredBufferStats::miss_rate`
helper (`registered_buffers.rs:157`) is the input the adaptive sizer
will consume; the sizer itself does not yet exist.

## 2. Pressure scenarios that break the fixed sizing

### 2.1 100K+ small files

A receiver pulling a tree of 100K small files (`<= 4 KiB` each)
produces back-to-back `flush_buffer` calls with no batching window.
At default `registered_buffer_count = 8`, the eight slots are sized at
64 KiB apiece. Two pathologies:

- **Slot saturation.** Every flush burns all eight slots before the
  first completion drains. Subsequent flushes hit the
  `available() == 0` branch and silently fall through to
  non-registered `IORING_OP_WRITE`. The pool is registered and pinned
  (paying the `RLIMIT_MEMLOCK` cost) but never producing throughput.
- **Buffer over-sizing.** A 4 KiB file using a 64 KiB registered
  buffer wastes 60 KiB per slot of pinned memory. The pool occupies
  `8 * 64 KiB = 512 KiB` for a working set that needs `8 * 4 KiB =
  32 KiB`. The over-size is an order of magnitude.

The `for_small_files` preset improves the sizing (`16 KiB * 8`) but
still has eight slots; under 100K-file pressure the saturation pattern
recurs.

### 2.2 Deep flist / deep recursion

INC_RECURSE traversal with `--inc-recursive` produces nested directory
fan-out where many files are queued for transfer in parallel. The
generator-side reader (`IoUringReader::open` in `file_reader.rs:73`)
is invoked for every basis-file lookup. Under deep flist with
parallel basis-file readers, the reader path also hits slot saturation
on its own ring's pool, which is sized identically to the writer.

There is a second-order cost: the reader's miss path is more expensive
than the writer's because basis-file reads block delta computation
that downstream stages depend on. Slot saturation here adds latency to
the critical path of the transfer, not just the I/O fan-out.

### 2.3 Daemon thread-per-connection at scale

The daemon spawns one ring per connection (per the session ring pool
design at `docs/design/iouring-session-ring-pool.md`). Under 100
concurrent clients each pulling files, every ring has its own static
pool of `8 * 64 KiB = 512 KiB`, totalling `100 * 512 KiB = 50 MiB`
pinned. If `RLIMIT_MEMLOCK` is at the typical container default of
64 MiB, this leaves only 14 MiB of headroom for any other locked
allocation in the process - close to the threshold where the next
`register_buffers` syscall fails.

The fixed sizing is wrong in both directions simultaneously: the hot
rings are saturated and want to grow; the cold rings are
over-provisioned and could shrink to free `RLIMIT_MEMLOCK` headroom
for the hot ones.

## 3. Adaptive proposal

### 3.1 High-water trigger

The trigger fires on a smoothed miss-rate signal sampled every
`CHECK_INTERVAL = 256` acquires. The sample window is larger than the
engine pool's 64 (`pressure.rs:29`) because a registered-pool resize
crosses the syscall boundary; we want enough history that we do not
pay the syscall on noise.

```text
miss_rate_sample = (misses - last_misses) / (acquires - last_acquires)
ema = EMA_ALPHA * miss_rate_sample + (1 - EMA_ALPHA) * ema_prev
```

`EMA_ALPHA = 0.2`, slightly more reactive than the engine's `0.1`
default in `crates/engine/src/local_copy/buffer_pool/throughput.rs:24`
because the registered pool turns over faster than per-file
throughput.

The grow trigger is `ema >= 0.10`. This is half the engine pool's
`MISS_RATE_GROW_THRESHOLD = 0.20` (`pressure.rs:35`); the lower
threshold reflects that a registered-pool miss is far more expensive
(a full fallback to non-registered ops) than an engine-pool miss
(a single `Vec::with_capacity`).

A second high-water signal is **peak depth**. Track
`peak_in_use = max(count - available)` over the sample window. When
`peak_in_use == count` for the entire window, every slot was
simultaneously checked out at least once - saturation pressure even
when `miss_rate < 0.10` (every `checkout` succeeded but only because
a slot released just in time). The sizer treats `peak_in_use ==
count` as equivalent to a high miss rate for grow purposes.

The shrink trigger combines `ema <= 0.005` with `peak_in_use < count
/ 2` for a full window. The peak-depth gate prevents shrinking a
pool whose slots are merely cycling fast; the engine pool's analogous
guard at `pressure.rs:155` is a low-utilization check, but the
registered pool needs the temporal signal because its resize is far
more costly.

### 3.2 Exponential growth, sub-linear shrink

**Grow geometrically: 2x per decision, capped at `MAX_BUFFERS`.**
Linear growth (e.g. `count + 4`) takes too many decisions to recover
from severe under-provisioning; at `count = 8` and a workload
demanding 64 slots, linear growth needs 14 decisions (`14 * 4 = 56`)
to catch up, each preceded by a `COOLDOWN_SAMPLES` window of paying
the fallback cost. Exponential reaches the right size in three
decisions: `8 -> 16 -> 32 -> 64`. This matches the engine pool's
`GROW_FACTOR = 2` (`pressure.rs:56`) for the same reason.

**Shrink linearly: 0.75x per decision (round down), floored at
`MIN_BUFFERS = 2`.** The engine pool uses `SHRINK_DIVISOR = 2`
(`pressure.rs:59`), i.e. halving on shrink, but the registered pool
prefers a gentler shrink because the cost asymmetry runs the other
way: over-shrinking forces an immediate re-grow on the next bursty
window, paying two syscalls (`unregister` + `register`) where one
would have sufficed. Three-quarter shrink is conservative enough
that an over-shrink under noise is recoverable in one further
decision rather than two.

This **geometric grow, linear shrink** asymmetry is the central
design choice. It produces the desired hysteresis without a separate
"grow weight" or "shrink weight" parameter. The engine pool gets
away with symmetric halving because shrinking a userspace `Vec<u8>`
is free; the registered pool cannot.

### 3.3 `RLIMIT_MEMLOCK` ceiling

Registered buffers count against `RLIMIT_MEMLOCK` for the lifetime of
the registration (see `man io_uring(7)`, *Locked Memory Accounting*).
The ceiling shapes the proposal in three places:

- **Soft `MAX_BUFFERS` cap.** `MAX_BUFFERS = min(64, kernel_cap,
  bgid_cap)`. 64 is the policy ceiling: `64 * 64 KiB = 4 MiB` per
  ring, well within the typical 64 MiB default but two orders of
  magnitude below the kernel's hard `MAX_REGISTERED_BUFFERS = 1024`
  at `registered_buffers.rs:80`. The bgid term is enforced through
  the namespace audit at `docs/audits/io-uring-bgid-namespace.md`
  and #2044.
- **Adaptive cap on `EAGAIN` / `ENOMEM`.** When `register_buffers`
  fails with `EAGAIN` or `ENOMEM`, the existing `try_new` swallows
  the error (`registered_buffers.rs:352`) and the writer keeps its
  current group. The sizer must record the failed grow and lower
  `MAX_BUFFERS` to the last-successful `count` for the remaining
  lifetime of the group. Without that bookkeeping the sizer would
  retry the same grow on the next window and pay the failed syscall
  repeatedly.
- **Buffer size is not adaptive.** Only `count` is resized.
  `buffer_size` is fixed at `Layout::from_size_align`
  (`registered_buffers.rs:273`); changing it requires tearing down
  the ring. This bounds the worst-case pin to `MAX_BUFFERS *
  buffer_size`, which is what feeds the `RLIMIT_MEMLOCK` budget
  calculation above.

The daemon-at-scale scenario in 2.3 is the cleanest case for this
constraint: at 100 concurrent rings, `100 * 4 MiB = 400 MiB` exceeds
typical container limits. A process-wide aggregator that lets hot
rings grow while cold rings shrink is sketched in the open
questions of `docs/design/io-uring-adaptive-buffer-pool.md` section
11; this brief does not commit to that aggregator but notes that
`MAX_BUFFERS` is the right per-ring knob to expose to it.

### 3.4 Quiescence and cooldown

Resize is gated by two preconditions that the engine pool does not
need:

- **Quiescence.** `available() == count()` must hold before
  `unregister_buffers` runs. A resize while a `READ_FIXED` /
  `WRITE_FIXED` SQE is in flight would drop the SQE's buffer
  reference. The bitset at `registered_buffers.rs:116` already
  exposes the check cheaply.
- **Cooldown.** `COOLDOWN_SAMPLES = 4 * CHECK_INTERVAL = 1024`
  acquires after any resize. Prevents grow / shrink / grow
  oscillation under noisy signals. The engine pool relies on
  asymmetric thresholds for the same job; the registered pool needs
  the temporal gate because a syscall-class resize cost dominates
  the threshold gap.

## 4. Comparison to the engine `BufferPool` lineage

The engine pool at `crates/engine/src/local_copy/buffer_pool/` is
the design template. The historical PR thread that built it is the
reference for what a working adaptive pool looks like in this tree:

- **#1638 - PressureTracker introduction.** Added the hit / miss
  counter pattern that the registered pool now mirrors. The
  `BufferPoolStats` snapshot type at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:868` is the
  shape the registered pool's `RegisteredBufferStats`
  (`registered_buffers.rs:145`) follows, including the "0.0 when no
  acquires" convention on `miss_rate` / `hit_rate`.
- **#1640 - grow trigger.** Established `MISS_RATE_GROW_THRESHOLD =
  0.20` (`pressure.rs:35`) and `GROW_FACTOR = 2` (`pressure.rs:56`).
  The registered pool inherits the geometric growth shape but
  lowers the threshold to `0.10` because the miss path is more
  expensive (full fallback to non-registered ops vs. a single
  fresh `Vec` allocation).
- **#1641 - shrink trigger.** Established
  `UTILIZATION_SHRINK_THRESHOLD = 0.30` (`pressure.rs:41`) and
  `SHRINK_DIVISOR = 2` (`pressure.rs:59`). The registered pool
  inherits the dual-condition shrink (low miss rate AND low
  utilization) but adds the temporal cooldown and substitutes
  `peak_in_use` for utilization because the registered pool's
  `available()` reading is over a shorter scale.

Where the registered pool diverges from the engine template:

| Concern | Engine pool | Registered pool |
|---------|-------------|-----------------|
| Shrink shape | Halve (lazy reclaim) | 0.75x (linear, syscall-bound) |
| Grow threshold | 20% miss rate | 10% miss rate |
| Check interval | 64 acquires | 256 acquires |
| Resize cost | O(1) atomics + lazy `ArrayQueue::pop` | Two syscalls (`unregister` + `register`) plus `get_user_pages()` over the new iovec |
| Memory cap | Soft `AtomicUsize` (`pool.rs:110`), not OS-enforced | OS-enforced `RLIMIT_MEMLOCK` |
| Quiescence required | No | Yes - in-flight SQEs reference slot indices |
| Failure on grow | None (allocation always succeeds for `Vec<u8>`) | `EAGAIN` / `ENOMEM` from `register_buffers` |

The two pools share a counter shape, an EMA encoding, and a
geometric grow step. Everything else is divergent because one is a
userspace `Vec<u8>` cache and the other is a kernel-pinned page
set. The brief follows the engine template wherever it transfers
and diverges deliberately wherever the kernel cost model demands it.

## 5. References

- Phase 1 audit: `docs/audits/io-uring-adaptive-buffer-sizing.md`.
- Phase 2 design: `docs/design/io-uring-adaptive-buffer-pool.md`.
- bgid namespace: `docs/audits/io-uring-bgid-namespace.md`, #2044.
- Session ring pool: `docs/design/iouring-session-ring-pool.md`.
- Engine template: `crates/engine/src/local_copy/buffer_pool/`,
  PRs #1638, #1640, #1641.
- Upstream `io_uring(7)`, *Locked Memory Accounting* section.
