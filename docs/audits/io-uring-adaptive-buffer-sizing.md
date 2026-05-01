# Adaptive sizing for the io_uring registered buffer pool

Tracking issues: oc-rsync #1834, #2045. Branch: `docs/iouring-adaptive-buffer-sizing`.

## Scope

Evaluate adding adaptive sizing to the io_uring registered buffer pool in
`fast_io::io_uring::RegisteredBufferGroup`. Today the pool is sized once at
ring construction with a fixed `(buffer_size, count)` tuple drawn from
`IoUringConfig`. Under sustained pressure the pool can be too small (every
acquire fails and the writer falls back to non-registered `IORING_OP_WRITE`,
losing the registered-buffer win) or too large (pinned memory wastes RAM that
the kernel could otherwise reclaim). The audit covers:

- Where the current sizing constants live and how they reach the pool.
- Where miss rate and throughput could be observed without adding an
  allocation or a syscall on the hot path.
- A proposed EMA design: smoothing factor, sample window, hysteresis, upper
  and lower triggers, and how the resize step interacts with the kernel.
- Concrete integration points: which call sites need a counter bump, which
  module owns the resize decision, and how the adaptive loop runs without
  blocking the I/O thread.
- Risks: registered buffer reallocation cost, the `RLIMIT_MEMLOCK` ceiling,
  the 1024-buffer kernel cap, and Drop ordering between the ring and the
  group.

Source files inspected (all paths repository-relative):

- `crates/fast_io/src/io_uring/registered_buffers.rs` (the pool itself,
  `RegisteredBufferGroup`, `checkout`, `available`, `Drop`).
- `crates/fast_io/src/io_uring/config.rs` (`IoUringConfig` defaults,
  `for_large_files`, `for_small_files`, `build_ring`).
- `crates/fast_io/src/io_uring/file_writer.rs` (the primary call site:
  `flush_buffer` and `write_all_batched` both speculatively check out
  every available slot).
- `crates/fast_io/src/io_uring/file_reader.rs` (the read-side mirror at
  `read_all_batched`).
- `crates/fast_io/src/io_uring/mod.rs` (the `register_buffers` policy
  flow into the factory functions).
- `crates/engine/src/local_copy/buffer_pool/throughput.rs` (existing,
  proven EMA implementation for the engine-level buffer pool;
  `ThroughputTracker` with `alpha = 0.1`, `WARMUP_SAMPLES = 8`,
  `recommended_buffer_size`).
- `crates/engine/src/local_copy/buffer_pool/pool.rs` (existing telemetry
  surface: `BufferPoolStats { total_hits, total_misses, total_growths }`,
  drained on Drop when `OC_RSYNC_BUFFER_POOL_STATS=1`).

## TL;DR

`RegisteredBufferGroup` is allocated once per `IoUringReader` /
`IoUringWriter` from `IoUringConfig::registered_buffer_count`
(`crates/fast_io/src/io_uring/config.rs:339`) at default `8`, with `16`
under `for_large_files()` and `8` under `for_small_files()`. Every flush
calls `reg.available().min(self.sq_entries as usize)` and then
`reg.checkout()` in a tight loop
(`crates/fast_io/src/io_uring/file_writer.rs:215-246`,
`file_writer.rs:282-308`, `file_reader.rs:158-184`). When `available()`
returns 0 - the all-in-use case - the writer silently falls back to
non-registered `IORING_OP_WRITE`, paying full per-SQE
`get_user_pages()` overhead. There is currently no counter that lets us
know how often that fallback fires; we cannot tell whether `count = 8` is
right-sized, oversized, or starving the registered path. The engine-level
buffer pool already solves the analogous problem with a lock-free EMA
(`crates/engine/src/local_copy/buffer_pool/throughput.rs:80-246`) and
`(hits, misses, growths)` counters
(`crates/engine/src/local_copy/buffer_pool/pool.rs:868-878`). We can
mirror that design into `fast_io` with two phases:

1. **Telemetry first (this PR):** add `total_acquires` /
   `total_misses` counters on `RegisteredBufferGroup` and a
   `RegisteredBufferStats` snapshot accessor. Zero behavioural change,
   zero pinned-memory change, zero syscalls. Cost: two `Relaxed`
   `fetch_add` per `checkout` call.
2. **Adaptive resize (follow-up PR):** a separate
   `RegisteredBufferSizer` that consumes those counters plus the engine
   `ThroughputTracker`'s bps estimate, decides on a new `count` (and
   optionally a new `buffer_size`), and drives a register / unregister
   cycle through `RegisteredBufferGroup::unregister` followed by a fresh
   `RegisteredBufferGroup::new`. Triggered off-hot-path between batches,
   never inside `submit_and_wait`.

This document specifies phase 2 in detail. Phase 1 lands as a trivial
telemetry addition alongside the doc.

## Upstream evidence

Upstream rsync 3.4.1 has no io_uring path; `fileio.c` uses a single 256 KB
static `wf_writeBuf` and plain `write(2)`. There is no upstream wire or
behavioural expectation around buffer-pool sizing. Adaptive sizing is
purely a local optimisation and must be invisible to peers: identical
on-disk bytes, identical syscall observable side effects (file mtime,
permissions, partial-file presence on error), and identical
`--fsync` durability semantics. The only relevant upstream constraint is
that the registered-buffer fast path and the regular-`Write` slow path
must produce byte-identical output, which they already do.

## 1. Current sizing parameters

The pool is built per ring in three places, all reading directly from
`IoUringConfig`:

- `crates/fast_io/src/io_uring/file_writer.rs:56-77`
  (`IoUringWriter::create`).
- `crates/fast_io/src/io_uring/file_writer.rs:80-104`
  (`IoUringWriter::from_file`).
- `crates/fast_io/src/io_uring/file_writer.rs:110-131`
  (`IoUringWriter::with_ring`, used by the `writer_from_file` policy
  entry point at `crates/fast_io/src/io_uring/mod.rs:140-188`).
- `crates/fast_io/src/io_uring/file_reader.rs:73-91` (the read-side
  mirror in `IoUringReader::open`).

Each call site eventually invokes:

```text
RegisteredBufferGroup::try_new(&ring, config.buffer_size,
                               config.registered_buffer_count)
```

(`crates/fast_io/src/io_uring/registered_buffers.rs:303`). The defaults
controlling the size (defined in `IoUringConfig`):

| Source | `buffer_size` | `registered_buffer_count` | Rationale |
|--------|---------------|---------------------------|-----------|
| `IoUringConfig::default()` (`config.rs:330-342`) | 64 KB | 8 | General-purpose default. |
| `IoUringConfig::for_large_files()` (`config.rs:347-358`) | 256 KB | 16 | Bias toward fewer, larger SQEs. |
| `IoUringConfig::for_small_files()` (`config.rs:362-373`) | 16 KB | 8 | Reduce per-buffer waste. |
| Hard kernel cap | n/a | 1024 | Enforced at `registered_buffers.rs:80,217-221`. |

Per-buffer memory is rounded up to a page size by
`Layout::from_size_align(aligned_size, page_size)`
(`registered_buffers.rs:225-231`), so the pinned-memory cost is always
`count * round_up(buffer_size, 4 KiB)` on x86_64 / aarch64. With the
defaults: `8 * 64 KiB = 512 KiB` per ring.

There is **no** sizing logic that observes runtime behaviour. Every call
site that wants something other than the default has to hard-code
`for_large_files()` or `for_small_files()`, both of which are static
templates picked at config construction.

## 2. Where to measure

The hot path always funnels through `RegisteredBufferGroup::checkout`
(`registered_buffers.rs:335-360`). That single function is the right
place to record `(acquires, misses)`:

- `total_acquires += 1` unconditionally on entry.
- `total_misses += 1` on the `None` exit (i.e., the loop terminates
  without a successful CAS).

Both counters can be `AtomicU64::fetch_add(1, Ordering::Relaxed)` -
`Relaxed` is fine because nothing depends on the cross-counter ordering
and the snapshot is intentionally non-atomic across fields, mirroring
`BufferPoolStats` (see `pool.rs:783-797`).

Throughput observation can reuse the engine's existing
`ThroughputTracker` (`buffer_pool/throughput.rs:80`). The submit path in
`submit_write_fixed_batch` (`registered_buffers.rs:544-628`) already
knows the byte count it submitted and `submit_and_wait` provides a
natural sample boundary. We do **not** need a separate timer in
`fast_io`: the caller (the disk-commit thread, the local-copy executor)
already wraps batched submissions with `Instant::now()` checkpoints for
its own profiling and can feed the same `(bytes, duration)` pair into a
`fast_io`-owned tracker.

Concretely, two new counters land in `RegisteredBufferGroup`:

```rust
total_acquires: AtomicU64,
total_misses: AtomicU64,
```

and a snapshot accessor:

```rust
pub struct RegisteredBufferStats {
    pub total_acquires: u64,
    pub total_misses: u64,
}

impl RegisteredBufferStats {
    pub fn miss_rate(&self) -> f64;
}

impl RegisteredBufferGroup {
    pub fn stats(&self) -> RegisteredBufferStats;
}
```

These are added in this PR. They are zero-cost when no caller reads
them and add two `fetch_add` to every `checkout`.

## 3. Proposed EMA design

The decision variable is `miss_rate = misses / acquires` in
`[0.0, 1.0]`, smoothed with an exponential moving average:

```text
ema = alpha * sample + (1 - alpha) * ema_prev
```

### Parameters

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `alpha` | `0.2` | Slightly more reactive than the engine's `0.1`; the registered-buffer pool turns over much faster than per-file throughput, so we prefer a shorter time-constant. |
| `WARMUP_SAMPLES` | `8` | Identical to `ThroughputTracker::WARMUP_SAMPLES` (`throughput.rs:30`). During warmup we use a simple cumulative average to avoid the zero-bias issue. |
| `SAMPLE_WINDOW` | 256 acquires | One sample is taken every 256 acquires (a power of two, so we can use `total_acquires & 0xff == 0` as the trigger without a divide). |
| `MIN_BUFFERS` | `2` | The pool must always be able to support a single in-flight SQE plus one queued; 1 forces serialisation. |
| `MAX_BUFFERS` | `min(64, kernel_cap)` | Pinning more than 64 buffers per ring is rarely beneficial and consumes `64 * 64 KiB = 4 MiB` per ring. Subject to the existing 1024 hard cap from `MAX_REGISTERED_BUFFERS` (`registered_buffers.rs:80`). |
| `GROW_THRESHOLD` | `miss_rate >= 0.10` | Conservative: grow only when at least one in ten acquires fail. |
| `SHRINK_THRESHOLD` | `miss_rate <= 0.005` AND `count > MIN_BUFFERS` | Shrink only when the pool is dramatically over-provisioned and at least one decision interval has passed since the last grow event (hysteresis - see below). |
| `GROW_FACTOR` | `2x` | Geometric growth, capped at `MAX_BUFFERS`. |
| `SHRINK_FACTOR` | `0.75x` (round down) | Shrink in smaller steps than we grow; over-shrinking causes immediate re-grow churn. |

### Hysteresis

Geometric growth followed by linear shrink already provides decay
asymmetry. We add an explicit `cooldown_samples = 4 * SAMPLE_WINDOW`
counter: after any resize event we record the value of `total_acquires`
at which the next decision is allowed to fire. This prevents a noisy
miss-rate signal from triggering grow/shrink/grow oscillation.

### Decision algorithm

Run on a non-hot-path thread (the existing engine
`maintenance` cadence, or a `decide_resize` call invoked between disk
commit batches by the disk-commit thread):

```text
acq = group.stats().total_acquires
if acq < cooldown_until: return
sample_miss_rate = (misses - last_misses) / (acq - last_acq)
ema = update_ema(ema, sample_miss_rate, alpha)
last_acq, last_misses = acq, misses
if ema >= GROW_THRESHOLD and group.count() < MAX_BUFFERS:
    new_count = min(MAX_BUFFERS, group.count() * GROW_FACTOR)
    resize(group, new_count)
    cooldown_until = acq + 4 * SAMPLE_WINDOW
elif ema <= SHRINK_THRESHOLD and group.count() > MIN_BUFFERS:
    new_count = max(MIN_BUFFERS, (group.count() * 3) / 4)
    resize(group, new_count)
    cooldown_until = acq + 4 * SAMPLE_WINDOW
```

The EMA itself is one `f64::to_bits` `AtomicU64`, identical to the
encoding pattern in `throughput.rs:53-60`.

## 4. Integration points

### Phase 1 (this PR)

- `crates/fast_io/src/io_uring/registered_buffers.rs`:
  - Add `total_acquires: AtomicU64`, `total_misses: AtomicU64` to
    `RegisteredBufferGroup` (initialised to `0` in `new()` at
    `registered_buffers.rs:289-296`).
  - Bump them inside `checkout` (`registered_buffers.rs:335-360`):
    - `fetch_add(1, Relaxed)` on `total_acquires` at function entry.
    - `fetch_add(1, Relaxed)` on `total_misses` only when the loop
      exits without a `Some(_)`.
  - Add `pub struct RegisteredBufferStats { total_acquires: u64,
    total_misses: u64 }` and a `RegisteredBufferGroup::stats()`
    accessor that returns a snapshot.
  - Add a `RegisteredBufferStats::miss_rate(&self) -> f64` helper
    (returns 0.0 when `total_acquires == 0`).

### Phase 2 (follow-up)

- New file `crates/fast_io/src/io_uring/adaptive_buffers.rs` containing:
  - `pub struct AdaptiveBufferSizer` with the EMA state.
  - `pub fn observe(&self, group: &RegisteredBufferGroup)` which reads
    `group.stats()`, updates the EMA, and decides whether a resize is
    warranted.
  - `pub fn resize(&self, owner: &mut RegisteredBufferOwner) ->
    io::Result<()>` which actually performs the unregister-and-reregister
    cycle. `RegisteredBufferOwner` is a small trait implemented by
    `IoUringReader` and `IoUringWriter` so the sizer can call into them
    without depending on either concretely (Dependency Inversion - see
    CLAUDE.md "Design Patterns").
- Extend `IoUringReader` and `IoUringWriter` with an
  `Option<AdaptiveBufferSizer>` and call `observe` after each batch.
- Resize executes off-hot-path: between `submit_and_wait` calls, never
  while SQEs are in flight, and never holding any checked-out slot.
  `RegisteredBufferGroup::unregister` (`registered_buffers.rs:375-377`)
  must succeed before we replace the group; if it fails we keep the
  current group and skip the resize.

Phase 2 is **not** in this PR. The doc fixes the design contract so the
code change is mechanical.

## 5. Risks

### 5.1 Reallocation cost

`RegisteredBufferGroup::new` (`registered_buffers.rs:204-296`):

1. Allocates `count` page-aligned regions via `alloc::alloc_zeroed`.
2. Calls `submitter().register_buffers(&iovecs)`, which is a syscall
   that pins user pages (`io_uring_register(2)`).
3. Initialises a `count.div_ceil(64)`-word atomic bitset.

The dominant cost is step 2: kernel-side `get_user_pages()` over the
new buffer set. For `count = 32, buffer_size = 64 KiB` this pins
`32 * 64 KiB / 4 KiB = 512` pages. On a healthy 6.x kernel with no
contention this is sub-millisecond, but it is an `io_uring_enter`-
class syscall and must not run on the hot path.

Mitigation: the resize cooldown is 4 * `SAMPLE_WINDOW = 1024` acquires.
At a sustained 1 GB/s with 64 KiB buffers, that is ~64 ms between
allowed resizes, far longer than the syscall cost.

### 5.2 Pinned memory / `RLIMIT_MEMLOCK`

Registered buffers count against `RLIMIT_MEMLOCK` (`man io_uring(7)`,
`Locked Memory Accounting` section). On most modern distributions the
default is 64 MiB per process, but containers and embedded targets
sometimes ship with 64 KiB. The proposed `MAX_BUFFERS = 64` with a 64
KiB buffer is `4 MiB` per ring, well within the typical default but
above the embedded floor.

Mitigation:

- `MAX_BUFFERS` is a soft cap that we lower if `register_buffers`
  returns `EAGAIN`/`ENOMEM`. The existing `try_new` path
  (`registered_buffers.rs:303-305`) already swallows those errors,
  which is the right behaviour - we just need to record the cap and
  not retry above it.
- We never increase `buffer_size` adaptively in phase 2; only `count`.
  Buffer size is set at ring construction and stays fixed for the
  lifetime of the ring. This bounds the worst-case pin to
  `MAX_BUFFERS * buffer_size`.

### 5.3 Kernel buffer-count cap

`MAX_REGISTERED_BUFFERS = 1024` (`registered_buffers.rs:80`) is the
hard ceiling enforced both by the kernel and by `new()`. Our soft
`MAX_BUFFERS = 64` is two orders of magnitude below it; not a real
risk.

### 5.4 Drop ordering

The Drop comment at `registered_buffers.rs:18-39` is explicit: the
ring fd must close before the user-side memory is freed. Phase 2
must not violate this: `resize` is not "drop the old group". It is
"unregister, then drop the user buffers". The implementation must:

1. Call `old_group.unregister(&ring)` first (returns `io::Result`,
   handled at `registered_buffers.rs:375-377`).
2. Drop `old_group` (frees user-side memory).
3. Construct `new_group` via `RegisteredBufferGroup::new(&ring, ...)`.
4. Atomically replace the `Option<RegisteredBufferGroup>` field on
   the writer / reader.

Any failure in step 3 leaves the writer / reader without a registered
group, which is sound: `flush_buffer` and `read_all_batched` already
fall back to non-registered ops when `self.registered_buffers` is
`None`.

### 5.5 In-flight SQEs

A resize must not race a submitted-but-not-completed `READ_FIXED` /
`WRITE_FIXED` SQE. The pool uses the slot bitset
(`registered_buffers.rs:107`) to track checked-out slots; the EMA
sampler must check `available() == count()` (no slots in use) before
resizing. This is cheap (one atomic load per word). If the sampler
sees outstanding slots, it skips the resize and retries on the next
sample cycle.

### 5.6 SQPOLL interaction

When SQPOLL is enabled (`IoUringConfig::sqpoll = true`,
`config.rs:311`) the kernel thread continuously polls the ring. The
`unregister_buffers` syscall is still synchronous from userspace's
view but it must be called when no SQE that references a registered
buffer is in flight. The `available() == count()` precondition above
is sufficient.

### 5.7 Telemetry overhead

`fetch_add(1, Relaxed)` on x86_64 is a `lock xadd` (~5 ns
uncontended) and on aarch64 is `LDADD` (similar). The hot path
already pays a `submit_and_wait` (microseconds), so two extra
`fetch_add` per `checkout` is a sub-percent overhead even at
sustained line-rate. The pool is single-threaded per ring in
practice (the writer / reader is `!Sync` from the consumer's view),
so we will not see real cache-line contention.

## 6. What is **not** in scope

- Tuning `IoUringConfig::buffer_size` adaptively. The buffer size is
  fixed at ring construction (`Layout::from_size_align` is called
  once in `new()`). Resizing the buffer size means tearing down the
  ring entirely. Out of scope.
- Per-thread or per-fd pools. The current architecture is one
  registered group per ring per file handle. Multi-fd pooling is a
  separate redesign, tracked elsewhere.
- Cross-process sharing of registered buffers via
  `IORING_REGISTER_BUFFERS_UPDATE`. Available since Linux 5.13 but
  introduces a fork-safety surface we are not ready to take on.

## 7. Testing strategy (phase 2 sketch)

These tests land alongside phase 2 and are listed here so the
design accounts for them up front:

- Unit: `AdaptiveBufferSizer` produces monotonically growing `count`
  under saturating-miss synthetic input, with hysteresis preventing
  immediate shrink.
- Unit: shrink fires only after `cooldown_samples` and only when
  miss-rate stays below `SHRINK_THRESHOLD` for the full window.
- Property: any sequence of `(acquires, misses)` deltas keeps
  `count` within `[MIN_BUFFERS, MAX_BUFFERS]`.
- Integration: a synthetic write workload that saturates the pool
  causes `count` to grow from `8` to `>= 32` within 10 batches and
  the registered-buffer fast path stays selected (verified by
  asserting `submit_write_fixed_batch` is called, not
  `submit_write_batch`).
- `RLIMIT_MEMLOCK` regression: a `prlimit(RLIMIT_MEMLOCK, 64 KiB)`
  test must show that `try_new` returns `None` and adaptive resize
  retains the previous group.

## 8. Implementation checklist

Phase 1 (this PR):

- [x] Audit document at `docs/audits/io-uring-adaptive-buffer-sizing.md`.
- [x] `RegisteredBufferGroup` carries `total_acquires`, `total_misses`
      counters bumped on `checkout`.
- [x] `RegisteredBufferStats` snapshot type with a `miss_rate` helper.
- [x] `RegisteredBufferGroup::stats()` accessor.
- [x] Tests covering the new counters under hit and miss paths.

Phase 2 (follow-up):

- [ ] `RegisteredBufferOwner` trait abstracting reader / writer
      ownership of the group.
- [ ] `AdaptiveBufferSizer` with EMA state and decision logic.
- [ ] `resize` driver that performs the unregister / drop / register
      cycle off the hot path.
- [ ] Wiring in `IoUringReader`, `IoUringWriter`.
- [ ] Diagnostic env var (`OC_RSYNC_REGISTERED_BUFFER_STATS=1`)
      mirroring the engine pool's `OC_RSYNC_BUFFER_POOL_STATS=1`
      pattern (`pool.rs:849-859`).
- [ ] Property tests, integration tests, and the
      `RLIMIT_MEMLOCK` regression test from section 7.
