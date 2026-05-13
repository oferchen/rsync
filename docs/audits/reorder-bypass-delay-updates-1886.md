# ReorderBuffer Bypass When `--delay-updates` Is Off (#1886)

Conformance audit verifying that the `ReorderBuffer` bypass for the
concurrent delta pipeline is wired correctly when `--delay-updates` is
disabled. Tracks task #1886 (the bypass) against task #1884 (the
correctness-mandatory spill design it must avoid paying for in the
common case).

## 1. Background

The parallel delta pipeline routes per-file results through a
`ReorderBuffer` so that downstream consumers see them in submission
(wire) order. When the head-of-line slot stalls, task #1884 added an
optional spill-to-tempfile layer to bound memory while preserving
ordering. That spill incurs disk I/O on the local temp filesystem and
is correctness-mandatory only when ordering is load-bearing for
atomicity - in practice, when `--delay-updates` is in effect (all
files renamed at the end of the transfer).

When `--delay-updates` is off (the common case) each file is renamed
via `do_atomic_rename` the moment its delta application completes.
Files are independent; submission order is irrelevant; the reorder
buffer and its spill are pure overhead. Task #1886 is to ensure that
the bypass added in [PR #3988] (the `ReorderBuffer::passthrough` path)
is actually engaged in this mode and that no spill machinery runs.

[PR #3988]: https://github.com/RsyncProject/oc-rsync/pull/3988

## 2. Source-of-truth wiring

All paths repository-relative.

### 2.1 ReorderBuffer (engine layer)

`crates/engine/src/concurrent_delta/reorder.rs`:

- `162` - `ReorderBuffer::passthrough()` constructs a buffer with zero
  ring slots and `bypass: true`. All items flow through a
  lightweight `VecDeque` FIFO. No `BTreeMap`, no slot indexing.
- `198` - `is_passthrough()` exposes the mode for tests / metrics.
- `1516` - `passthrough_tests` module covers insertion-order delivery,
  zero buffered count, and large-batch FIFO semantics.

The passthrough buffer is intentionally *not* a `SpillableReorderBuffer`
(`crates/engine/src/concurrent_delta/spill.rs:108`). The spill layer is
the structure task #1884 designed; it sits behind the ordered path
only. The bypass path never instantiates it, so the disk-spill cost is
unreachable when bypass is selected.

### 2.2 DeltaConsumer (engine layer)

`crates/engine/src/concurrent_delta/consumer.rs`:

- `129` - `DeltaConsumer::spawn(rx, reorder_capacity)` constructs the
  ordered path via `spawn_inner(rx, reorder_capacity, false)`.
- `143` - `DeltaConsumer::spawn_bypass(rx)` constructs the bypass path
  via `spawn_inner(rx, 0, true)`.
- `173` - inside `spawn_inner`, the `delta-reorder` thread selects:

```rust
let mut reorder = if bypass {
    ReorderBuffer::passthrough()
} else {
    ReorderBuffer::new(reorder_capacity)
};
```

The bypass flag therefore propagates from the consumer constructor
down to the buffer instantiation with no intermediate decision points.

### 2.3 ParallelDeltaPipeline (transfer layer)

`crates/transfer/src/delta_pipeline.rs`:

- `209` - `ParallelDeltaPipeline::new(worker_count)` wires the ordered
  path: `DeltaConsumer::spawn(work_rx, capacity)`.
- `228` - `ParallelDeltaPipeline::new_bypass(worker_count)` wires the
  bypass path: `DeltaConsumer::spawn_bypass(work_rx)`.

Both constructors size the work queue identically
(`worker_count.saturating_mul(2).max(2)`). The only difference is the
choice of `DeltaConsumer::spawn` vs `spawn_bypass`, which in turn
selects the `ReorderBuffer` mode shown in section 2.2.

### 2.4 ThresholdDeltaPipeline (dispatcher)

`crates/transfer/src/delta_pipeline.rs`:

- `305-312` - the dispatcher carries a `bypass_reorder: bool` field
  alongside `threshold` and `mode`.
- `331` - `ThresholdDeltaPipeline::new(threshold)` defaults
  `bypass_reorder` to `false`.
- `352` - `ThresholdDeltaPipeline::new_bypass(threshold)` sets it to
  `true`.
- `361-373` - `promote_to_parallel` (called when the buffered work
  count crosses `threshold`) branches on the flag:

```rust
fn promote_to_parallel(&mut self, buffered: Vec<DeltaWork>) -> io::Result<()> {
    let worker_count = rayon::current_num_threads();
    let mut parallel = if self.bypass_reorder {
        ParallelDeltaPipeline::new_bypass(worker_count)
    } else {
        ParallelDeltaPipeline::new(worker_count)
    };
    ...
}
```

So a caller that constructs the dispatcher via `new_bypass(threshold)`
gets the bypass path end-to-end the first time the threshold is
crossed: dispatcher -> `ParallelDeltaPipeline::new_bypass` ->
`DeltaConsumer::spawn_bypass` -> `ReorderBuffer::passthrough`.

### 2.5 BoundedReorderBuffer (transfer layer)

`crates/transfer/src/reorder_buffer.rs:223` exposes the same
`passthrough()` constructor for the transfer-side bounded reorder
buffer. `is_passthrough()` is at `268`. The bypass branch at line
`291` skips window admission, BTreeMap insertion, and metric updates
unrelated to delivery counts. Tests at `1047-1145` cover the bypass
semantics directly.

## 3. End-to-end trace: bypass engaged

When a caller constructs `ThresholdDeltaPipeline::new_bypass(threshold)`
and installs it on `ReceiverContext` via
`set_delta_pipeline` (`crates/transfer/src/receiver/mod.rs:257`), the
chain is:

```
ThresholdDeltaPipeline { bypass_reorder: true }   delta_pipeline.rs:352
  -> promote_to_parallel                          delta_pipeline.rs:361
     -> ParallelDeltaPipeline::new_bypass         delta_pipeline.rs:228
        -> DeltaConsumer::spawn_bypass            consumer.rs:143
           -> spawn_inner(rx, 0, true)            consumer.rs:148
              -> ReorderBuffer::passthrough()     reorder.rs:162
                 (no slots, no BTreeMap, no spill)
```

There is no decision point along this chain that can fall back to the
ordered or spilling path. Likewise the ordered case
(`ThresholdDeltaPipeline::new`) cannot accidentally pick up
`ReorderBuffer::passthrough()`. The two paths are statically
disjoint at every layer.

## 4. End-to-end trace: bypass not engaged

When a caller constructs `ThresholdDeltaPipeline::new(threshold)` the
chain remains the ordered path:

```
ThresholdDeltaPipeline { bypass_reorder: false }  delta_pipeline.rs:331
  -> promote_to_parallel                          delta_pipeline.rs:361
     -> ParallelDeltaPipeline::new                delta_pipeline.rs:209
        -> DeltaConsumer::spawn(rx, capacity)     consumer.rs:129
           -> spawn_inner(rx, capacity, false)    consumer.rs:130
              -> ReorderBuffer::new(capacity)     reorder.rs:120
                 (ring buffer + ordered drain; spill-aware design
                  per task #1884 in SpillableReorderBuffer)
```

## 5. `delay_updates` flag plumbing

`delay_updates` lives on `ServerWriteConfig`
(`crates/transfer/src/config/mod.rs:55`) and is settable via the
builder at `crates/transfer/src/config/builder.rs:260`. The build-time
guard at `crates/transfer/src/config/builder.rs:433` rejects the
`inplace + delay_updates` combination, matching the upstream mutual
exclusion.

The flag is the right signal for picking
`ThresholdDeltaPipeline::new` vs `new_bypass`: when
`server_config.write.delay_updates` is `false`, callers should select
`new_bypass`; when `true`, they should select `new`.

## 6. Verdict

The bypass wiring inside the threshold dispatcher and below is
correct as audited above. A caller that selects
`ThresholdDeltaPipeline::new_bypass(threshold)` receives a buffer-free
FIFO path with no spill exposure. A caller that selects
`ThresholdDeltaPipeline::new(threshold)` receives the ordered ring
buffer path that retains spill semantics for `--delay-updates`
correctness.

No code changes are required to deliver task #1886's stated goal at
the dispatcher boundary: the constructor choice already drives the
entire downstream pipeline. Task #1886 is complete at this layer.

## 7. Related follow-up

A separate concern - not in scope for #1886 as scoped here - is the
glue between `ServerConfig::write::delay_updates` and the
`set_delta_pipeline` call site. As of this audit the default pipeline
on `ReceiverContext` is `SequentialDeltaPipeline`
(`crates/transfer/src/receiver/mod.rs:241`) and no production caller
in the workspace invokes `set_delta_pipeline` to upgrade it to a
threshold pipeline. Wiring that upgrade - and selecting `new` vs
`new_bypass` based on `config.write.delay_updates` at that single
call site - is the remaining integration task. That work belongs to
the receiver bootstrap, not the dispatcher, and is tracked
independently of #1886.
