# parallel-receive-delta feature flag status

## Current status: default-on (stable)

The `parallel-receive-delta` Cargo feature is **enabled by default** in the
workspace and all forwarding crates (`engine`, `transfer`, `core`, `cli`).
The production receiver `token_loop` dispatches through
`ParallelDeltaApplier`, fanning out per-chunk strong-checksum verification
across rayon workers while serializing writes through a per-file reorder
buffer.

The feature was validated through the PIP-9 wire-up series (production
integration), PIP-10 end-to-end validation (interop, stress, correctness,
RSS, error-path parity), and the PIP-9.f bake window (5 consecutive green
nightlies with zero attributable regressions). See
[`docs/design/parallel-receive-delta-bake-prerequisites.md`](../design/parallel-receive-delta-bake-prerequisites.md)
for the complete bake-window evidence.

## What the feature does

When the receiver reconstructs files from delta instructions, each chunk of
data must be checksum-verified before it can be written to the destination
file. With the feature enabled (default), verification fans out across rayon
worker threads while writes stay serialized through a per-file `Mutex` and
reorder buffer. Verification scales with core count; writes remain
deterministic.

Without the feature, the receiver processes chunks sequentially on a single
thread - verify chunk N, write chunk N, then move to chunk N+1.

## What the flag controls

The feature flag compiles the following modules into the production binary
and activates the parallel dispatch at the receiver's cutover site
(`crates/transfer/src/receiver/transfer/sync.rs:241-253`):

### Modules

| Crate | Module | Description |
|-------|--------|-------------|
| `engine` | `concurrent_delta::chunk_adapter` | Adapts `DeltaWork` items into `DeltaChunk` payloads suitable for the parallel applier. |
| `engine` | `concurrent_delta::parallel_apply` | `ParallelDeltaApplier` - per-file slot registration, rayon fan-out verification, reorder-buffered write commit. |
| `transfer` | `delta_pipeline::chunk_builder` | `ChunkBuilder` that feeds `ParallelDeltaApplier::apply_batch_parallel`. |

### Benchmarks (engine crate)

- `parallel_receive_delta_perf` - end-to-end parallel applier throughput.
- `parallel_verify_chunk` - isolated checksum verification fan-out.
- `br_3j_f_dashmap_cores_vs_throughput` - DashMap scaling across core counts.

### Tests

- `crates/engine/tests/parallel_apply_concurrent.rs` - concurrent slot
  registration and chunk dispatch.
- `crates/engine/tests/parallel_apply_dg3_stress.rs` - stress test under
  the `dg-stress` feature (which implies `parallel-receive-delta`).
- `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs` -
  regression test for the PIP-7 receiver corruption.

### CI cells

- **`parallel-receive-delta (dist profile, non-required)`** in `ci.yml` -
  builds and runs `parallel_threshold` tests under `--profile dist`.
- **`interop-parallel-receive-delta`** in `_interop.yml` - full upstream
  interop matrix (3.0.9, 3.1.3, 3.4.1, 3.4.2) against a
  `parallel-receive-delta`-enabled binary.
- **`bench-drain-throughput`** workflow - builds and benchmarks the parallel
  applier in release mode.

## Emergency opt-out

The feature flag is retained temporarily as an emergency disable mechanism.
To fall back to the sequential receiver path, build without the feature:

```sh
cargo build --release --no-default-features --features <other-features>
```

Or selectively disable `parallel-receive-delta` while keeping other defaults.
The sequential path remains compiled and will be used when the feature is
absent. PIP-9.f.4 will retire the flag entirely after the post-flip bake
window confirms stability.

## Feature flag propagation

The flag cascades through the crate dependency graph:

```
workspace  parallel-receive-delta (default)
  -> cli/parallel-receive-delta
  -> core/parallel-receive-delta
  -> transfer/parallel-receive-delta
  -> engine/parallel-receive-delta
```

The workspace-level `Cargo.toml` includes `parallel-receive-delta` in its
default feature set. The production binary picks up the parallel receiver
path without any opt-in flag.

## Completed roadmap

| Task | Description | Status |
|------|-------------|--------|
| PIP-9.b.1-3 | Wire `ParallelDeltaApplier` into the receiver token loop via RJN-3 fan-out. | Done |
| PIP-9.b.4 | `flush_workers` drain - every registered slot reaches zero in-flight before transfer phase closes. | Done |
| PIP-9.b.6 | Worker-pool knob wiring (`RAYON_NUM_THREADS`, `OC_RSYNC_PARALLEL_WORKERS`). | Done |
| PIP-9.c | Re-validate `parallel-threshold-trip` interop scenario under dist profile. | Done |
| PIP-9.d | CI matrix cell: `--profile dist --features parallel-receive-delta` nextest run. | Done |
| PIP-9.e | Confirm PIP-7 receiver-corruption fix against the parallel-applier path. | Done |
| PIP-9.f | Bake criterion gate - 5 consecutive green CI cycles before default-on flip. | Done |
| PIP-10.a-f | Full end-to-end interop, stress, correctness, RSS, and error-path validation. | Done |
| Default-on | Flip `parallel-receive-delta` into workspace default features. | Done |

## History

- **PIP-3/5** wired an initial dispatch into the receiver context and
  flipped the feature to default-on.
- **PIP-4** surfaced receiver-side corruption - the first dispatched file in
  a directory wrote wrong bytes.
- **PIP-7** bisected the corruption to dead scaffolding: the parallel
  pipeline had one writer (the enable setter) and zero readers (no
  production code path drained the pipeline output).
- **PIP-8** tore out the dead wiring, leaving the core types compiled but
  unwired. The feature flag became a no-op.
- **PIP-9** rebuilt the integration properly, routing the receiver's token
  loop through `ParallelDeltaApplier` via a real fan-out caller with
  observable production readers.
- **PIP-10** validated the parallel path end-to-end across interop, stress,
  correctness, RSS, and error-path dimensions.
- **PIP-9.f** satisfied the bake criterion (5 consecutive green nightlies,
  zero regressions) and flipped the feature to default-on.

## Related documents

- `docs/user/parallel-receive-delta.md` - full user guide covering
  architecture, tuning knobs, performance expectations, and known
  limitations.
- `docs/design/parallel-receive-delta-bake-prerequisites.md` - bake
  prerequisites and evidence for the default-on flip (PFF-5).
- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` - PIP-9
  design document.
- `docs/design/pip-9-f-1-bake-criterion.md` - bake-window criterion.
- `docs/operations/pip-9-f-3-bake-window-monitor.md` - post-flip monitor
  runbook.
