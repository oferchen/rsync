# Single-Producer vs Multi-Producer WorkQueue Bench Plan (#1572)

## Summary

`crates/transfer/src/pipeline/spsc.rs` wraps a `crossbeam_queue::ArrayQueue`
with disconnect flags and a spin-wait. The transport layer is presently
single-producer, but #1404 has begun gating the producer side behind a
`Clone` capability so multiple ingest tasks could feed the disk thread. This
note specifies the criterion benchmark that decides whether the multi-producer
path costs enough to justify keeping the feature flag.

## 1. Cross-References

- **#1611 (done)**: `Sender<T>` clone gated behind a feature flag. The
  flag exists today; this bench is the gate that decides its fate.
- **#1612 (done)**: multi-producer correctness tests (drop ordering,
  disconnect signaling, drained queue semantics). Correctness is settled;
  what remains is the throughput question.
- **#1613 (related)**: this issue. The data this bench produces feeds
  the decision matrix in section 5 directly.

## 2. Bench Plan

`crates/transfer/benches/workqueue_producers.rs` (new file, criterion).

- Two arms over the existing `pipeline::spsc` channel:
  - **Single-producer**: one sender pushes 100K `FileMessage` items.
  - **Multi-producer**: 4 senders (cloned via the #1611 feature flag)
    push 25K items each; total = 100K.
- One consumer in both arms drains until `RecvError`. Items are
  pre-allocated zero-byte payloads so the bench measures queue overhead,
  not allocator noise.
- Queue capacity fixed at 1024 (matches `pipeline::mod.rs` default) so
  back-pressure spin-waits exercise both arms equally.
- Producers pinned with `core_affinity` where available; consumer pinned
  to a separate physical core. Disable hyperthreading siblings.
- Metrics:
  - **Consumer throughput** (items/sec) - primary signal.
  - **Sender contention** - per-producer push latency p50/p99 measured
    via `Instant::now()` deltas around `Sender::send`. Multi-producer p99
    must not collapse the aggregate consumer rate.
- Warm-up: 200 ms; measurement: 2 s; samples: 100. Throughput reported
  per arm. Runner: `cargo bench -p transfer --bench workqueue_producers`,
  gated behind `OC_RSYNC_RUN_WORKQUEUE_BENCH=1` in
  `tools/ci/run_benches.sh` to bound nightly time.

## 3. Crossbeam Channel Semantics

`crossbeam_queue::ArrayQueue` is already MPMC: every `push` does a CAS
on the tail cursor, every `pop` does a CAS on the head cursor. Switching
from one producer to four does not change the cursor protocol; it changes
how often the tail CAS observes contention. The cost we are measuring is
**N producers vs 1 producer on the same tail atomic**, not a queue
swap. The disconnect flags (`producer_alive`, `consumer_alive`) only
flip at sender drop and so are not on the hot path.

## 4. Pass / Fail Criteria

The multi-producer path keeps the feature flag and ships only if every
condition holds.

| Condition | Threshold |
|---|---|
| Single-producer throughput at 100K items | baseline (record measured value) |
| Multi-producer throughput at 4x25K items | within 15% of single-producer baseline |
| Sender p99 push latency (multi) | <= 3x sender p99 push latency (single) |
| Consumer thread CPU utilisation | does not drop more than 5 percentage points |
| No regression at 1-producer arm with feature flag enabled | within 2% (flag overhead is free) |

If multi-producer throughput regresses by more than 15%, the feature
flag stays off-by-default and the design note records the regression as
the reason. If overhead is below 5%, the flag is removed and clone
becomes free.

## 5. Decision Matrix

| Multi-producer overhead | Action on #1404 feature flag |
|---|---|
| <= 5% | Promote to default; remove the flag in a follow-up PR |
| 5% - 15% | Keep the flag opt-in; document the trade-off |
| > 15% | Keep the flag, mark experimental; open an issue to re-evaluate after the next `crossbeam_queue` major release |

The bench output is committed to `docs/benchmarks/` as the artifact
that justifies whichever cell of the matrix is selected.
