# Parallel receive-delta bake prerequisites (PFF-5)

Tracking: PFF-5. Parent series: PIP-9 (parallel-receive-delta production
wire-up). Related: PIP-10 (end-to-end validation), PIP-9.f (default-on
flip series).

## 1. Purpose

This document records the prerequisites that gated the
`parallel-receive-delta` default-on flip (PIP-9.f) and the evidence
that each was met. The flip changes the workspace Cargo feature default
from OFF to ON, making the parallel receive-delta path the production
receiver pipeline for all builds.

The prerequisite structure follows the ISI.h / ISI.i.1 precedent
(`docs/design/isi-h-bake-window-criteria.md`) established for the
sender-side INC_RECURSE default flip.

## 2. Prerequisites

Three categories gated the flip: production wire-up (PIP-9.b),
end-to-end validation (PIP-10), and the bake-window criterion
(PIP-9.f.1 through PIP-9.f.3).

### 2.1 PIP-9.b - token_loop rewired through ParallelDeltaApplier

The production receiver `token_loop` had to dispatch through
`ParallelDeltaApplier` when the `parallel-receive-delta` feature was
enabled - not merely compile against it. This was the load-bearing
change that turned the feature flag from a no-op into a real
production path.

| Sub-task | Description | Status |
|----------|-------------|--------|
| PIP-9.b.1 | Call-shape audit of `apply_delta_tokens` at `sync.rs:445-573` identifying the cutover point for parallel dispatch. | Done (PR #4776, `docs/design/pip-9b-call-shape-audit.md`) |
| PIP-9.b.2 | `#[cfg]`-gated dispatch sketch inserting a compile-time branch at the cutover site (`sync.rs:241-253`) that routes to `apply_delta_tokens_parallel` when the feature is enabled. | Done (`docs/design/pip-9b2-cfg-dispatch-sketch.md`) |
| PIP-9.b.3 | Parallel-arm feed loop wiring `TokenReader` output into `ChunkBuilder` and dispatching through `ParallelDeltaApplier::apply_batch_parallel`. | Done (`docs/design/pip-9-b-3-parallel-arm-feed-loop.md`) |
| PIP-9.b.4 | `flush_workers` drain - `drain_inflight()` blocks until every registered slot's in-flight counter reaches zero before the transfer phase closes (FFB-1/FFB-2 guarantee). | Done |
| PIP-9.b.6 | Worker-pool knob wiring (`RAYON_NUM_THREADS`, `OC_RSYNC_PARALLEL_WORKERS`) into the applier constructor. | Done (`docs/design/pip-9hb-worker-pool-knobs-impl.md`) |

Evidence: after PIP-9.b, the receiver's `token_loop` uses
`apply_delta_tokens_parallel` (feature-gated) at the cutover site in
`crates/transfer/src/receiver/transfer/sync.rs:241-253`. The parallel
arm reads tokens from the `TokenReader`, builds `DeltaChunk` payloads
via `ChunkBuilder`, and dispatches through
`ParallelDeltaApplier::apply_batch_parallel` with rayon fan-out for
verification and serialized per-file writes through the reorder buffer.

### 2.2 PIP-10 - full end-to-end interop validation

PIP-10 validated the parallel path across six dimensions before it
could become the default. Each sub-task targeted a distinct failure
mode that unit tests and benchmarks alone could not cover.

| Sub-task | Scope | Status |
|----------|-------|--------|
| PIP-10.a | Full upstream interop matrix (3.0.9, 3.1.3, 3.4.1, 3.4.2) through the parallel path - ~324 cells covering all scenarios, both directions, sha256 parity between sequential and parallel builds. | Done (`docs/design/pip-10a-parallel-interop-matrix.md`) |
| PIP-10.b | Adversarial chunk-ordering stress tests - reverse completion, worst-case interleaving, burst patterns, drip feeds - targeting reorder buffer overflow, incorrect sequencing, deadlock, and spill-path failures. | Done (`docs/design/pip-10b-adversarial-chunk-ordering-stress.md`) |
| PIP-10.c | Multi-file mixed-size correctness test - realistic file-size distributions (tiny + medium + large in one transfer) exercising threshold dispatch, adaptive queue depth, and cross-path interactions. | Done (`docs/design/pip-10c-mixed-size-correctness-test.md`) |
| PIP-10.d | RSS overhead measurement - peak RSS comparison between parallel and sequential builds across 1K, 10K, and 100K file workloads to quantify the memory cost of rayon workers, per-file reorder buffers, and DashMap tracking. | Done (`docs/design/pip-10d-parallel-rss-overhead.md`) |
| PIP-10.e | Error-path validation - mid-transfer failures (broken pipe, disk full, file vanished, timeout, checksum mismatch, worker panic) produce the same exit codes, error messages, temp-file cleanup, and partial-transfer semantics as the sequential path. | Done (`docs/design/pip-10e-error-path-validation.md`) |
| PIP-10.f | Aggregate sign-off - all PIP-10.a through PIP-10.e passed; no open regression issues; interop parity confirmed. | Done |

Evidence: PIP-10.a's sha256 parity check confirmed byte-identical
destination files between sequential and parallel builds for every
upstream version. PIP-10.b's adversarial orderings found no reorder
buffer corruption. PIP-10.e confirmed exit-code parity across all
six error scenarios (INV-1 through INV-6).

### 2.3 PIP-9.f - bake-window criterion and monitoring

The bake window prevents shipping a regression behind a silent default
change. The criterion definition (PIP-9.f.1), the Cargo.toml flip
(PIP-9.f.2), and the production monitoring (PIP-9.f.3) form the final
gate.

| Sub-task | Scope | Status |
|----------|-------|--------|
| PIP-9.f.1 | Quantitative bake criterion: 5 consecutive green nightly runs across Interop Validation, Fuzz Coverage, and PIP-9.d bench cell; zero attributable regressions; minimum 5 calendar days. | Done (`docs/design/pip-9-f-1-bake-criterion.md`, PR #4924) |
| PIP-9.f.2 | Cargo.toml flip PR moving `parallel-receive-delta` into workspace default features. | Done |
| PIP-9.f.3 | Production CI monitor runbook covering the post-flip bake window - daily checks (D1-D6), weekly summaries, disqualifying-signal response procedures. | Done (`docs/operations/pip-9-f-3-bake-window-monitor.md`) |

## 3. Bake-window criteria (PIP-9.f.1)

The formal criterion required all of the following to hold
simultaneously over 5 consecutive nightly CI cycles spanning at least
5 calendar days.

### 3.1 Pre-conditions (entry gates)

Before the bake clock could start:

- PIP-9.b complete - production `token_loop` dispatches through
  `ParallelDeltaApplier`.
- PIP-9.c sha256 byte-identity scenario green at
  `tests/parallel_threshold_trip.rs`.
- PIP-9.d CI matrix cell green - `parallel-receive-delta (dist
  profile, non-required)` at `.github/workflows/ci.yml:586`.
- PIP-9.e closed - PIP-7 receiver-corruption issue confirmed fixed
  against the parallel-applier path; regression test at
  `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs`
  green.
- Zero open GitHub issues matching `is:open label:regression
  "parallel-receive-delta" OR "parallel-delta-apply"`.

### 3.2 Window criteria

- **N = 5 consecutive nightly Interop Validation runs green** across
  all four upstream rsync versions (3.0.9, 3.1.3, 3.4.1, 3.4.2).
- **N = 5 consecutive nightly fuzz-coverage runs green** with zero
  new panics from parallel-path code.
- **N = 5 consecutive nightly bench runs** where throughput is
  `>= 0.95x` the sequential baseline (no measurable slowdown).
- **Zero CI failures attributable to parallel-receive-delta** across
  the same 5-cycle window. Required checks (fmt+clippy, nextest
  stable, Windows stable, macOS stable, Linux musl stable) all green.
- **Minimum 5 calendar days** AND 5 consecutive green runs, whichever
  is longer. Skipped nightlies do not count as green.

### 3.3 Disqualifying signals

Any of the following reset the bake clock to day 0:

- Nightly Interop failure attributable to the parallel path.
- Panic from fuzz-coverage reachable only through the parallel path.
- Wire-byte divergence detected by the sha256 parity scenario.
- Throughput regression `>= 5%` vs the sequential baseline.
- User-reported correctness bug (silent corruption, missing files,
  wrong content, wrong metadata, wrong hardlink graph).
- Silent transfer corruption detected by the PIP-7 regression test.

Reset semantics: clock restarts to day 0 only if the regression
cannot be fixed in a forward-fix PR within 7 calendar days. A forward
fix within 7 days keeps the existing clock.

### 3.4 Daily monitor checks (PIP-9.f.3)

The post-flip bake window was monitored via six daily checks:

| Check | Signal |
|-------|--------|
| D1 | Nightly Interop Validation - all upstream versions green |
| D2 | Nightly Fuzz Coverage - green, no new parallel-path panics |
| D3 | PIP-9.d CI matrix cell green on most recent PR run |
| D4 | Zero new open regression issues (`gh issue list --label regression`) |
| D5 | Bench cell throughput `>= 0.95x` sequential baseline |
| D6 | PIP-7 corruption repro test passes in latest nightly |

Full runbook: `docs/operations/pip-9-f-3-bake-window-monitor.md`.

## 4. Outcome

All prerequisites were met:

1. **PIP-9.b** shipped sub-tasks b.1, b.2, b.3, b.4, and b.6. The
   production receiver `token_loop` dispatches through
   `ParallelDeltaApplier` when the feature is enabled.

2. **PIP-10** completed all six sub-tasks (PIP-10.a through PIP-10.f).
   The sha256 parity check confirmed byte-identical output between
   sequential and parallel builds. Adversarial chunk orderings,
   mixed-size distributions, RSS overhead, and error-path behavior all
   validated clean.

3. **PIP-9.f.1** bake criterion was satisfied: 5 consecutive green
   nightlies across Interop Validation, Fuzz Coverage, and the bench
   cell; zero attributable regressions over the bake window.

4. **PIP-9.f.3** production monitoring confirmed no disqualifying
   signals during the post-flip bake period.

The default-on flip (PIP-9.f.2) was approved. `parallel-receive-delta`
is now in the workspace default feature set on `engine`, `transfer`,
`core`, `cli`, and the workspace binary. The opt-out path (building
without `--features parallel-receive-delta`) remains available as an
emergency disable mechanism pending PIP-9.f.4 closure, which will
retire the feature flag entirely after an additional 1-week
post-flip bake.

## 5. Post-flip status

- The `parallel-receive-delta` Cargo feature is ON by default in the
  workspace and all forwarding crates.
- The `ParallelDeltaApplier` at
  `crates/engine/src/concurrent_delta/parallel_apply/` is the
  production receiver pipeline.
- The sequential `DeltaWork` path remains compiled as the fallback
  when the feature is explicitly disabled.
- The feature flag is retained temporarily as an emergency opt-out;
  PIP-9.f.4 will remove it after the post-flip bake window.

## 6. References

### Design documents

- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` -
  PIP-9 upstream acceptance reference.
- `docs/design/pip-9-parallel-receive-wireup.md` - PIP-9 architecture
  and punch-list.
- `docs/design/pip-9-f-1-bake-criterion.md` - bake-window criterion
  (PIP-9.f.1, PR #4924).
- `docs/operations/pip-9-f-3-bake-window-monitor.md` - post-flip
  monitor runbook (PIP-9.f.3).
- `docs/design/pip-10a-parallel-interop-matrix.md` - PIP-10.a interop
  matrix spec.
- `docs/design/pip-10b-adversarial-chunk-ordering-stress.md` -
  PIP-10.b stress test spec.
- `docs/design/pip-10c-mixed-size-correctness-test.md` - PIP-10.c
  mixed-size test spec.
- `docs/design/pip-10d-parallel-rss-overhead.md` - PIP-10.d RSS
  measurement spec.
- `docs/design/pip-10e-error-path-validation.md` - PIP-10.e error-path
  validation spec.
- `docs/design/parallel-receive-delta-default-on.md` - historical
  default-on rationale (superseded by PIP-7/PIP-8/PIP-9).
- `docs/design/parallel-receive-delta-application.md` - umbrella
  design for the apply-loop architecture.
- `docs/design/isi-h-bake-window-criteria.md` - ISI.h precedent for
  the bake-window pattern.

### Code references

- `crates/transfer/src/receiver/transfer/sync.rs:241-253` - cutover
  site where the `#[cfg]` dispatch selects parallel vs sequential.
- `crates/engine/src/concurrent_delta/parallel_apply/` -
  `ParallelDeltaApplier` implementation.
- `crates/transfer/src/delta_pipeline/parallel.rs` -
  `ParallelDeltaPipeline` glue.
- `crates/engine/src/concurrent_delta/consumer/` - `DeltaConsumer`
  drain loops.
- `tests/parallel_threshold_trip.rs` - PIP-9.c sha256 byte-identity
  scenario.
- `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs` -
  PIP-7 regression canary.
- `.github/workflows/ci.yml:586` - PIP-9.d CI matrix cell.
- `.github/workflows/_interop.yml:555` -
  `interop-parallel-receive-delta` CI job.

### PR history

- PIP-3+5 (#4666) - original default-on flip (reverted).
- PIP-4 (#4720) - parallel-threshold-trip scenario surfaced corruption.
- PIP-7 (#4730) - corruption investigation, proved dead scaffolding.
- PIP-8 (#4731) - dead scaffolding teardown.
- PIP-9.b.2 (#4776) - cfg-gated dispatch sketch.
- PIP-9.f.1 (#4924) - bake-window criterion definition.
