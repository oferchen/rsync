# Performance Governance Model

## 1. Purpose

This document defines the model that governs all pipeline performance work
in oc-rsync: how staged pipelines reach the hardware ceiling, how buffers
between stages should be sized, and which improvements are worth making.
It is the foundation for the BND (buffer right-sizing) task series and the
review baseline for any change that adds a queue, grows a buffer, or adds
concurrency.

The model has five layers. Each layer answers one question:

| Layer | Question | Answer |
|-------|----------|--------|
| L1 | How much work must be in flight? | Little's Law: in-flight >= bandwidth x latency (BDP) |
| L2 | What limits throughput? | Exactly one stage - the constraint (drum) - at any instant |
| L3 | How do stages coordinate? | They don't - bounded queues + blocking, optimum emerges locally |
| L4 | How big should buffers be? | BDP + variability margin, no bigger |
| L5 | What is worth improving? | Only the current drum; everything else is a mirage |

A transfer pipeline governed by these five layers converges to the hardware
ceiling on its own: every stage runs at the drum's pace, the drum runs
saturated, queues stay near-empty, and memory stays flat.

## 2. L1 - Little's Law and the bandwidth-delay product

Little's Law relates the three quantities of any steady-state flow system:

```
throughput = in-flight / latency
```

Rearranged, it gives the minimum amount of in-flight work needed to sustain
a target rate through a path with a given latency:

```
in-flight >= bandwidth x latency = bandwidth-delay product (BDP)
```

Depth is the lever that hides latency. A disk write that takes 1 ms only
sustains 1 GB/s if at least 1 MB is in flight while it completes. If the
pipeline holds less than the BDP in flight, the downstream resource idles
between requests and throughput drops below the ceiling regardless of how
fast any individual stage is.

Consequences:

- Every queue between stages exists to hold in-flight work, and the amount
  it needs to hold is computable: the BDP of the path it feeds.
- BDP is small for local paths. A local disk with ~1 GB/s bandwidth and
  ~100 us commit latency has a BDP of ~100 KB - hundreds of kilobytes, not
  tens of megabytes.
- Raising depth beyond the BDP adds nothing to throughput (see L4).

Reference: APNIC, "Sizing the buffer" - the canonical treatment of BDP-based
buffer sizing for network paths; the same arithmetic applies to any
producer-consumer path with latency.

## 3. L2 - The constraint (drum)

At any instant, exactly one stage of the pipeline is 100% utilized. That
stage - the constraint, or drum in Theory of Constraints terms - sets the
throughput of the entire pipeline. Every other stage has spare capacity by
definition.

"Peak hardware performance" has a precise meaning under this layer: the
drum is a saturated hardware resource. The pipeline is at its ceiling when
the binding constraint is cores, memory bandwidth, or disk bandwidth - not
an artificial limit such as an undersized buffer, a serialization point, or
a sleep.

The drum is not fixed. It shifts per workload and can shift mid-transfer:

- Small-file trees: disk metadata operations and per-file syscalls bind.
- Large local copies: memory bandwidth binds.
- Checksum-heavy modes (`--checksum`, whole-file hashing): CPU binds.
- Network transfers: the wire or the remote end binds.

Because the drum shifts, no static tuning targets "the" bottleneck. The
governance model must make the pipeline track the drum wherever it moves -
which is what L3 provides.

Reference: Goldratt, "The Goal" / Theory of Constraints - the five focusing
steps and drum-buffer-rope scheduling.

## 4. L3 - Decentralized backpressure

Coordination between stages is achieved without a coordinator. The
mechanism is bounded queues plus blocking:

- A stage that finds its downstream queue full blocks until space appears.
- A stage that finds its upstream queue empty blocks until work appears.

Each stage reacts only to local state. No stage knows which stage is the
drum, and none needs to: when the drum's input queue fills, the stall
propagates backwards hop by hop until every upstream stage is pacing itself
to the drum. When the drum speeds up or the constraint moves, the same
local rules re-converge on the new drum. The global optimum - every stage
running at the drum's pace - emerges from local blocking. This is the same
principle as TCP congestion control: endpoints react to local signals
(loss, delay) and the network-wide allocation emerges.

oc-rsync has this layer, and it is measured working:

- The network-receiver to disk-commit SPSC channel
  (`crates/transfer/src/disk_commit/`, `crates/transfer/src/pipeline/spsc.rs`)
  is a bounded `ArrayQueue` (default capacity 128 messages,
  `DEFAULT_CHANNEL_CAPACITY` in `disk_commit/config.rs`). The sender blocks
  when the queue is full. Under a deliberately slow disk sink, RSS stays
  flat and the network side blocks - the bound holds and backpressure
  propagates.
- The local-copy streaming loop
  (`crates/engine/src/local_copy/executor/file/copy/transfer/`) reads and
  writes through a single adaptively sized buffer (8 KB to 1 MiB by file
  size, `adaptive_buffer_size`). In-flight data is bounded at one buffer;
  blocking read and write syscalls provide the backpressure. Measured: flat
  RSS regardless of file size, producer paced by the sink.

Subordination rule: a non-drum stage must never consume the drum's
resources while waiting. Blocking is the correct waiting primitive; a
spin-wait is a subordination violation - a stage with spare capacity
burning CPU that the drum may need. The SPSC channel's wait path escalates
spin hints to `yield_now` to `park_timeout`, which caps the damage, but any
sustained spin phase on a non-drum stage remains an L3 defect to eliminate
(see section 7).

## 5. L4 - Buffer sizing: BDP plus a variability margin, no bigger

L1 gives the lower bound: a buffer smaller than the BDP of the path it
feeds starves the drum. L4 adds the upper bound: a buffer materially larger
than BDP plus a margin for arrival variability is bufferbloat.

Bufferbloat is a pure loss. A standing queue above the BDP:

- adds latency (every queued item waits longer),
- wastes memory (the queue occupancy is resident),
- adds zero throughput (the drum is already saturated; extra queue depth
  cannot make it faster).

BBR demonstrates the point at internet scale: by pacing to the estimated
BDP instead of filling buffers until loss, it holds ~99% utilization with a
near-empty buffer - "full pipe, empty buffer". A full buffer is not a sign
of health; it is a sign the buffer is oversized or the drum is downstream.

The health signal is not occupancy but the buffer's empty-fraction and the
sojourn time of items passing through it - CoDel's insight. CoDel controls
queues by watching the minimum sojourn time over an interval, which is
parameterless and independent of link rate. The same signal applies to an
in-process queue:

- Minimum sojourn near zero: the queue drains fully in each cycle - healthy.
- Standing minimum sojourn: a persistent backlog - the buffer is absorbing
  nothing but latency and should shrink.
- Frequent empty-with-blocked-consumer: starvation - the buffer is below
  BDP plus margin, or the feeder is the drum.

Applied to oc-rsync: the disk-commit SPSC bounds 128 messages of up to
256 KiB payload (`WRITE_BUF_SIZE`) - a worst-case 32 MiB standing queue
(~4 MB at the measured ~32 KB average chunk). The BDP of a local disk path
is on the order of hundreds of kilobytes. A 32 MiB cap over a ~hundreds-of-KB
BDP is likely textbook bufferbloat - a memory defect, not a throughput
feature. This yields the model's falsifiable prediction, tested by the
BND-3 sweep:

> Shrinking the SPSC capacity toward the path BDP cuts peak RSS at
> approximately zero throughput cost. If throughput drops materially before
> the cap reaches the BDP estimate, the model is wrong for that path.

Either outcome is recorded. A confirmed prediction licenses right-sizing
every queue in the pipeline by the same method; a refuted one localizes
where the model's latency estimate or variability margin is off.

References: Nichols and Jacobson, "Controlling Queue Delay" (ACM Queue) -
CoDel; Cardwell et al., "BBR: Congestion-Based Congestion Control".

## 6. L5 - Elevate only the drum

The mirage rule: time saved at a non-bottleneck is a mirage - it yields
nothing. A non-drum stage that gets 2x faster spends more time blocked; the
pipeline's throughput is unchanged because the drum's pace is unchanged.

Governance consequences:

- Optimization effort targets the current drum only. Before optimizing a
  stage, demonstrate it is the constraint under the workload of interest.
- At the ceiling, the steady state is: drum saturated, all queues
  near-empty, every other stage partially blocked. This is the target
  picture, not a problem to fix.
- Pushing in-flight work beyond the BDP is not elevation - it is
  bufferbloat (L4). Do not churn hardware for no reason: extra depth, extra
  threads, or extra readahead at a non-drum stage costs memory and power
  and buys nothing.
- When the drum is elevated past another stage's capacity, the constraint
  moves and the target of further work moves with it. Re-identify before
  re-optimizing.

## 7. The oc-rsync map

How the current codebase sits against the five layers:

- L3 is present and measured. Both bounded paths (disk-commit SPSC,
  local-copy streaming loop) hold their bounds with flat RSS and correct
  producer blocking under a slow sink. The emergent-pacing mechanism works.
- L4 is the gap. Queue capacities were chosen as safe upper bounds, not
  derived from BDP plus variability margin. The BND task series
  (right-sizing sweeps, starting with BND-3 on the disk-commit SPSC) closes
  this layer.
- Spin-waits are an L3 subordination violation. The SPSC wait path's spin
  phase burns CPU on a stage that is by definition not the drum while it
  waits. The escalation to yield and park bounds the cost; the governance
  position is that a blocked non-drum stage should cede the CPU promptly.
- io_uring is an L5 elevation. Batched submission pays off only where
  syscall overhead is the binding cost of the drum stage (small-file
  metadata and open-write-close storms on Linux). Enabling it on a non-drum
  stage is mirage work; evaluation must measure the drum with and without.
- The O(N) file-list memory work (flist retention, inc_recurse) is an
  orthogonal data-structure axis. It bounds per-entry memory, while
  de-bufferbloating (L4) bounds queued-payload memory. The two stack: total
  RSS improvements multiply, and neither substitutes for the other.

### Summary table

| Queue / stage | Location | Governing layer | Status |
|---------------|----------|-----------------|--------|
| Disk-commit SPSC (network receiver -> disk writer), cap 128 msgs, worst-case 32 MiB | `crates/transfer/src/disk_commit/` | L3 + L4 | Measured-bounded; to-size (BND-3: cap >> local BDP) |
| Result and buffer-return SPSC channels, cap 2x file channel | `crates/transfer/src/disk_commit/thread.rs` | L3 + L4 | Measured-bounded; to-size (derived from file-channel cap) |
| Local-copy streaming loop, one adaptive buffer 8 KB - 1 MiB | `crates/engine/src/local_copy/executor/file/copy/transfer/` | L3 + L4 | Measured-bounded; sized to file class |
| SPSC wait path (spin -> yield -> park escalation) | `crates/transfer/src/pipeline/spsc.rs` | L3 | Violation (spin phase burns CPU on a non-drum stage; escalation caps it) |
| `BufferPool` free-list, queue capacity 256 (64 page-aligned) | `crates/engine/src/local_copy/buffer_pool/` | L4 | To-size (capacity chosen as safe bound, not from demand) |
| io_uring depth (`io_uring_depth`, Linux fast_io paths) | `crates/fast_io/`, `disk_commit/config.rs` | L1 + L5 | To-size (depth = BDP of device path; enable only on drum stages) |
| File-list retention (O(N) entries, inc_recurse) | `crates/protocol` / `crates/core` flist paths | Orthogonal axis | Separate campaign; stacks with L4 right-sizing |

Status vocabulary: measured-bounded (bound exists and is verified under
load), to-size (bound exists but was not derived from BDP + margin),
violation (breaks a layer's rule).

## 8. Methodology

### Five Focusing Steps

All performance work follows the Theory of Constraints loop:

1. Identify the constraint - measure which stage is saturated under the
   workload of interest. Never assume; the drum shifts (L2).
2. Exploit the constraint - remove waste at the drum itself (fewer
   syscalls, better batching, no redundant work) before adding resources.
3. Subordinate everything else - every other stage paces itself to the drum
   via backpressure (L3) and never steals the drum's resources (no spins,
   no speculative work, no oversized queues).
4. Elevate the constraint - only now invest in making the drum faster
   (SIMD, io_uring, parallelism) (L5).
5. Repeat - the constraint has moved; go to step 1. Do not let inertia
   leave step-4 machinery running where it no longer pays.

### Buffer sizing discipline

Buffers protect the drum from upstream variability, so a buffer is sized to
the feeding stage's variance, not to a round number. Foote (1996, Massey
thesis, "Practical buffer sizing techniques under Drum-Buffer-Rope")
formulates it as:

```
buffer = f(effect of variability, protective capacity)
```

- Effect of variability: how bursty the feeding stage's output is - the
  arrival-time distribution's spread, not its mean.
- Protective capacity: how much headroom the non-drum stages have to
  refill the buffer after a disruption.
- Disruption factor: measured at the buffer as starvation events - the
  count and duration of drum stalls on an empty queue. This is the
  operational metric for "buffer too small", complementing sojourn time
  (L4) as the metric for "buffer too big".

The sizing loop for each queue: estimate BDP, add a margin from the
measured variance of the feeder, deploy, then watch starvation events
(grow) versus standing sojourn (shrink).

### Kanban form, not classic DBR

Classic drum-buffer-rope places one time buffer before a single known drum
and ropes release to it. oc-rsync instead runs a Kanban-form system: a
space (WIP) limit on every inter-stage queue, with bidirectional blocking.
This is the right choice here because the drum shifts (L2) - per workload
and mid-transfer. Per-stage WIP limits protect whichever stage is currently
the constraint without anyone identifying it, where a single-drum rope
would need re-pointing every time the constraint moved.

## 9. References

- APNIC blog, "Sizing the buffer" - Little's Law and BDP-based buffer
  sizing.
- K. Nichols and V. Jacobson, "Controlling Queue Delay", ACM Queue 10(5),
  2012 - CoDel, sojourn time as the parameterless queue-health signal.
- N. Cardwell, Y. Cheng, C. S. Gunn, S. H. Yeganeh, V. Jacobson, "BBR:
  Congestion-Based Congestion Control", ACM Queue 14(5), 2016 - full pipe,
  empty buffer.
- D. Foote, "Practical buffer sizing techniques under Drum-Buffer-Rope",
  Massey University thesis, 1996 - buffer as a function of variability and
  protective capacity.
- E. M. Goldratt, "The Goal" / Theory of Constraints - the five focusing
  steps and drum-buffer-rope.
