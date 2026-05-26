# parallel-receive-delta feature flag status

## Current status: experimental scaffolding only

The `parallel-receive-delta` Cargo feature flag is a **compile-time no-op**.
Enabling it compiles additional modules, benchmarks, and tests, but does
**not** activate the parallel receive-delta path in production transfers.
The production receiver token loop takes the sequential `DeltaWork` path
regardless of whether this flag is enabled.

## What the flag controls

Enabling `--features parallel-receive-delta` compiles the following code
that is otherwise excluded from the build:

### Modules

| Crate | Module | Description |
|-------|--------|-------------|
| `engine` | `concurrent_delta::chunk_adapter` | Adapts `DeltaWork` items into `DeltaChunk` payloads suitable for the parallel applier. |
| `engine` | `concurrent_delta::parallel_apply` | `ParallelDeltaApplier` - per-file slot registration, rayon fan-out verification, reorder-buffered write commit. |
| `transfer` | `delta_pipeline::chunk_builder` | `ChunkBuilder` that feeds `ParallelDeltaApplier::apply_one_chunk` (currently used only in tests). |

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
  `parallel-receive-delta`-enabled binary. Non-required.
- **`bench-drain-throughput`** workflow - builds and benchmarks the parallel
  applier in release mode.

## What it does NOT do

- **Does not change the production token loop.** Neither the sequential nor
  the threshold-based receiver dispatch paths contain any
  `#[cfg(feature = "parallel-receive-delta")]` branch point. The flag
  compiles the parallel machinery but nothing in the production transfer
  code calls it.
- **Does not make transfers faster.** Enabling the flag and running
  `oc-rsync` produces identical transfer behavior to a default build.
- **Does not enable parallel delta application for real rsync transfers.**
  The `ParallelDeltaApplier` is exercised only by benchmarks and
  unit/integration tests - never by the receiver's `recv_files` loop.

## Feature flag propagation

The flag cascades through the crate dependency graph:

```
workspace  parallel-receive-delta
  -> cli/parallel-receive-delta
  -> core/parallel-receive-delta
  -> transfer/parallel-receive-delta
  -> engine/parallel-receive-delta
```

Note: `core/Cargo.toml` lists `parallel-receive-delta` in its `default`
features, but the workspace-level `Cargo.toml` does **not**. The workspace
default feature set controls the binary build, so the flag is off by default
for end users.

## Roadmap

| Task | Description | Status |
|------|-------------|--------|
| PIP-9.b.1-3 | Wire `ParallelDeltaApplier` into the receiver token loop via RJN-3 fan-out. | In progress |
| PIP-9.b.4 | `flush_workers` drain - ensure every registered slot reaches zero in-flight before the transfer phase closes. | Pending |
| PIP-9.c | Re-validate `parallel-threshold-trip` interop scenario under dist profile. | Blocked on PIP-9.b |
| PIP-9.d | CI matrix cell: `--profile dist --features parallel-receive-delta` nextest run. | Blocked on PIP-9.b |
| PIP-9.e | Confirm PIP-7 receiver-corruption fix against the parallel-applier path. | Blocked on PIP-9.b |
| PIP-9.f | Bake criterion gate - N consecutive green CI cycles before default-on flip. | Blocked on PIP-9.c-e |
| Default-on | Flip `parallel-receive-delta` into workspace default features. | After PIP-9.f bake window passes |

Until PIP-9.f completes, enabling the flag is informational only.

## When to enable

- **Developers contributing to the parallel receive-delta pipeline.** The
  flag compiles the code you need to build and test against.
- **Benchmark runners measuring the parallel apply engine in isolation.**
  The three gated benchmarks exercise the `ParallelDeltaApplier` directly,
  independent of the transfer loop.
- **CI validation.** The non-required CI cells exercise the feature to catch
  compilation regressions and maintain readiness for PIP-9 wire-up.

**Not for production use.** Enabling the flag does not change transfer
behavior. There is no performance benefit or functional difference in
production builds today.

## Build instructions

Build with the feature enabled:

```sh
cargo build --features parallel-receive-delta
```

Run feature-gated tests:

```sh
cargo nextest run --features parallel-receive-delta
```

Run feature-gated benchmarks:

```sh
cargo bench -p engine --features parallel-receive-delta
```

## History

The feature has been through several integration attempts:

- **PIP-3/5** wired an initial dispatch into the receiver context and
  flipped the feature to default-on.
- **PIP-4** surfaced receiver-side corruption - the first dispatched file in
  a directory wrote wrong bytes.
- **PIP-7** bisected the corruption to dead scaffolding: the parallel
  pipeline had one writer (the enable setter) and zero readers (no
  production code path drained the pipeline output).
- **PIP-8** tore out the dead wiring, leaving the core types compiled but
  unwired. The feature flag became a no-op.
- **PIP-9** (current) is rebuilding the integration properly, routing the
  receiver's token loop through `ParallelDeltaApplier` via a real fan-out
  caller with observable production readers.

## Related documents

- `docs/user/parallel-receive-delta.md` - full user guide covering
  architecture, tuning knobs, performance expectations, and known
  limitations.
- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` - PIP-9
  design document.
