# SPSC disk-commit channel utilization profile

Last verified: 2026-05-07 against `crates/transfer/src/pipeline/spsc.rs` and
`crates/transfer/src/disk_commit/{config,thread,process,writer}.rs`.

Tracking issue: #1081. Cross-references the metrics scaffolding from #1369.

## 1. SPSC channel location

The SPSC primitive lives in `crates/transfer/src/pipeline/spsc.rs` (not
`crates/engine/`). It wraps `crossbeam_queue::ArrayQueue` with two
`AtomicBool` liveness flags and `std::hint::spin_loop` synchronization - no
syscalls, no futex, no `thread::park`. `crates/transfer/src/disk_commit/`
spawns three of these channels per receive session:

- `file_tx` (network -> disk) carries `FileMessage` at
  `DEFAULT_CHANNEL_CAPACITY = 128` (`config.rs:32`, `thread.rs:49`).
- `result_rx` (disk -> network) carries `io::Result<CommitResult>` at
  `capacity * 2 = 256` (`thread.rs:50`).
- `buf_return_rx` (disk -> network) recycles `Vec<u8>` chunk buffers at
  `capacity * 2 = 256` (`thread.rs:51`).

Capacity is clamped to `[8, 4096]` via `effective_channel_capacity`
(`config.rs:35-38, 117-127`). No CLI override exists today.

## 2. Open question

How full does each queue actually get during a transfer? Specifically:

- Is the disk-commit consumer chronically starved (queue near empty,
  network-bound)?
- Or chronically blocked (queue at capacity, disk-bound)?
- What is the steady-state distribution between those two extremes?

We need a per-channel utilization histogram bucketed by depth (0, 1-8,
9-32, 33-64, 65-127, 128) plus counters for sender-blocked spins and
receiver-empty spins. Without that, capacity sizing remains guesswork.

## 3. Profile plan

Instrument the SPSC primitive with optional, feature-gated counters so
release builds pay nothing:

- Add `AtomicU64` counters on `Shared<T>`: `enqueue_total`,
  `dequeue_total`, `producer_spin_total`, `consumer_spin_total`,
  `peak_depth`, plus a 6-bucket `[AtomicU64; 6]` depth histogram sampled
  on every `push`/`pop`.
- Increment `producer_spin_total` inside `Sender::send`'s retry arm
  (`spsc.rs:79-84`) and `consumer_spin_total` inside `Receiver::recv`
  (`spsc.rs:110-120`).
- Sample `queue.len()` on every successful `push` and `pop`; route into
  the histogram by saturating bucket index. `len()` is
  eventually-consistent on `ArrayQueue` but adequate for sampling.
- Capture wall-clock timestamps at first enqueue and final dequeue per
  `FileMessage::Begin`/`Commit` pair so we can compute per-file dwell
  time alongside depth.
- Emit a single structured log line on `FileMessage::Shutdown` from the
  disk thread (`thread.rs:194`) listing totals, peak depth, histogram,
  and per-channel sender-blocked time.

Surface behind `--debug io2` to match the existing disk-IO tracing
(`thread.rs:114-164`). No wire-protocol change.

## 4. Workload variants

Run the profile against three deliberate operating points:

- **Fast network, slow disk** - target HDD or fsync-on-every-file; expect
  `file_tx` to saturate at capacity, high producer-spin counts, full
  histogram weight in the top bucket.
- **Slow network, fast disk** - throttle ingress with `--bwlimit=10M`
  against tmpfs; expect near-empty depth, high consumer-spin counts,
  histogram weight in bucket 0.
- **Bursty** - mixed file sizes (1 KiB to 1 GiB) without `--bwlimit`;
  expect bimodal distribution with peaks at 0 and capacity. Drives the
  `WholeFile` coalescer (`messages.rs:32-37`) versus per-`Chunk` mode.

For each, capture three runs and report median. Reuse
`scripts/benchmark.sh` plus `tools/ci/run_interop.sh` as the harness so
results are reproducible.

## 5. Findings to track

The audit output should record, per channel, per workload:

- **Average depth** (mean of sampled `len()`). Indicates which side is
  the bottleneck.
- **Peak depth** (`peak_depth` watermark). Confirms whether headroom
  exists.
- **Backpressure events** (`producer_spin_total` / total `push`
  attempts). Ratio above ~10% signals chronic sender stalls.
- **Starvation events** (`consumer_spin_total` / total `pop` attempts).
  Ratio above ~50% signals the consumer waits more than it works.
- **Per-file dwell time** (enqueue-to-dequeue). Tail latency drives
  perceived sync time on small-file flists.
- **Capacity sizing recommendation** - if peak < 32 across all
  workloads, `DEFAULT_CHANNEL_CAPACITY` is over-provisioned and can
  shrink to free `~3.5 MiB` of queue memory; if backpressure ratio > 10%
  on the fast-network/slow-disk workload, raise the default toward 256
  and add a CLI flag.

## 6. Cross-reference to #1369

Issue #1369 (and its companion thread) added the initial scaffolding for
disk-commit metrics: enqueue/dequeue counters and a peak-depth tracker.
This audit extends that scaffolding with the depth histogram, spin
counters, and per-file dwell timestamps described above. Implementation
should reuse #1369's `AtomicU64` fields and emit format rather than
introducing a parallel telemetry surface.

## 7. Out of scope

- Replacing the SPSC primitive (`crossbeam-channel`, `tokio::sync::mpsc`).
  The lock-free `ArrayQueue` is the deliberate choice for the
  network-to-disk hot path.
- Adding fairness fallback (yield/park) to `Sender::send`. Tracked
  separately as a follow-up to this audit.

## References

- `crates/transfer/src/pipeline/spsc.rs` - SPSC primitive.
- `crates/transfer/src/pipeline/messages.rs` - `FileMessage` variants.
- `crates/transfer/src/disk_commit/{config,thread,process,writer}.rs` -
  channel wiring and disk thread main loop.
- Issue #1081 - this audit's tracking item.
- Issue #1369 - prior metrics scaffolding extended here.
