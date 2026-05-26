# parallel-receive-delta Feature Flag Surface Audit (PFF-1)

Audit date: 2026-05-26

## 1. Current Status

The `parallel-receive-delta` feature flag is a **no-op** pending PIP-9 wire-up.
PIP-7 (#4730) proved the previous dispatch scaffolding was a side-effect-only
no-op; PIP-8 tore out the dead receiver-side wiring. The `ParallelDeltaApplier`
and `ParallelDeltaPipeline` types still compile and are exercised by benches and
tests, but no production receiver path consumes them.

Tracking design doc: `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md`.

---

## 2. Feature Flag Definition (Cargo.toml files)

| Crate | File | Definition | In `default`? | Forwards to |
|-------|------|-----------|---------------|-------------|
| `bin` (workspace root) | `Cargo.toml:72` | `parallel-receive-delta = ["cli/...", "core/...", "transfer/...", "engine/..."]` | **No** | `cli`, `core`, `transfer`, `engine` |
| `cli` | `crates/cli/Cargo.toml:48` | `parallel-receive-delta = ["core/parallel-receive-delta"]` | **Yes** (line 15) | `core` |
| `core` | `crates/core/Cargo.toml:99` | `parallel-receive-delta = ["transfer/...", "engine/..."]` | **Yes** (line 66) | `transfer`, `engine` |
| `transfer` | `crates/transfer/Cargo.toml:111` | `parallel-receive-delta = ["engine/parallel-receive-delta"]` | **No** | `engine` |
| `engine` | `crates/engine/Cargo.toml:120` | `parallel-receive-delta = []` | **No** | (leaf - no forwarding) |

### Feature forwarding chain

```
bin ──► cli ──► core ──► transfer ──► engine
                   └─────────────────► engine   (redundant, harmless)
     └──────► core ──► ...
     └──────► transfer ──► engine
     └──────► engine
```

### Meta-feature coupling

`dg-stress` in the engine crate implies `parallel-receive-delta`:

```toml
dg-stress = ["parallel-receive-delta"]
```

---

## 3. Compile-Time `#[cfg]` Gates

### 3.1 Engine crate - `crates/engine/src/concurrent_delta/mod.rs`

| Line | Gate | What is gated |
|------|------|--------------|
| 173 | `#[cfg(feature = "parallel-receive-delta")]` | `pub mod chunk_adapter;` declaration |
| 179 | `#[cfg(feature = "parallel-receive-delta")]` | `pub mod parallel_apply;` declaration |
| 188 | `#[cfg(feature = "parallel-receive-delta")]` | `pub use chunk_adapter::{ChunkPayload, ChunkSource, DeltaChunkAdapter, delta_work_to_chunk};` |
| 192 | `#[cfg(feature = "parallel-receive-delta")]` | `pub use parallel_apply::{DeltaChunk, ParallelApplyError, ParallelDeltaApplier};` |

Gated modules:
- `chunk_adapter` (`crates/engine/src/concurrent_delta/chunk_adapter.rs`) - in-memory shape adapter from `DeltaWork` to `DeltaChunk`
- `parallel_apply/` (`crates/engine/src/concurrent_delta/parallel_apply/mod.rs` + submodules: `batch.rs`, `decrement_guard.rs`, `drain.rs`, `slot_barrier.rs`) - the `ParallelDeltaApplier` type

### 3.2 Transfer crate - `crates/transfer/src/delta_pipeline/mod.rs`

| Line | Gate | What is gated |
|------|------|--------------|
| 34 | `#[cfg(feature = "parallel-receive-delta")]` | `pub mod chunk_builder;` declaration |
| 43 | `#[cfg(feature = "parallel-receive-delta")]` | `pub use chunk_builder::{ChunkBuilder, ChunkBuilderError, TokenForBuild};` |

Gated module:
- `chunk_builder` (`crates/transfer/src/delta_pipeline/chunk_builder.rs`) - wire-token to `DeltaChunk` adapter for the parallel path

### 3.3 Always-compiled modules (no feature gate)

The following delta pipeline modules compile unconditionally even though they
are logically part of the parallel infrastructure:

| Module | File | Notes |
|--------|------|-------|
| `parallel` | `crates/transfer/src/delta_pipeline/parallel.rs` | `ParallelDeltaPipeline` - rayon dispatch |
| `threshold` | `crates/transfer/src/delta_pipeline/threshold.rs` | `ThresholdDeltaPipeline` - auto-selects sequential vs parallel |
| `sequential` | `crates/transfer/src/delta_pipeline/sequential.rs` | `SequentialDeltaPipeline` - baseline path |

### 3.4 Test file-level gates

| File | Gate |
|------|------|
| `crates/engine/tests/parallel_apply_concurrent.rs:23` | `#![cfg(feature = "parallel-receive-delta")]` |
| `crates/engine/tests/parallel_apply_dg3_stress.rs:57` | `#![cfg(all(feature = "dg-stress", feature = "parallel-receive-delta"))]` |
| `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs:53` | `#![cfg(feature = "parallel-receive-delta")]` |

### 3.5 Bench file-level gates

| File | Gate | `required-features` in Cargo.toml |
|------|------|----------------------------------|
| `crates/engine/benches/parallel_receive_delta_perf.rs:63` | `#![cfg(feature = "parallel-receive-delta")]` | Yes |
| `crates/engine/benches/parallel_verify_chunk.rs:63` | `#![cfg(feature = "parallel-receive-delta")]` | Yes |
| `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:71` | `#![cfg(feature = "parallel-receive-delta")]` | Yes |

### 3.6 CLI and Core crates

No `#[cfg(feature = "parallel-receive-delta")]` gates exist in `crates/cli/src/`
or `crates/core/src/`. The feature is purely a forwarding mechanism in those
crates.

---

## 4. Feature-Conditional Dependencies

The `parallel-receive-delta` feature does **not** pull in any optional
dependencies. In every crate where it is defined, it either forwards to
sub-crates or is an empty leaf (`parallel-receive-delta = []` in engine). The
code it gates uses only already-present unconditional dependencies (`dashmap`,
`rayon`, `crossbeam-queue`, etc.).

---

## 5. Documentation Surface

| Location | Mentions | Notes |
|----------|---------|-------|
| `README.md:315` | Yes | Feature table entry - marked "experimental (no-op pending PIP-9)" |
| `CHANGELOG.md:98` | Yes | PIP-4 entry, marked SUPERSEDED |
| Man page / `--help` | **No** | No mention in man page generator (`xtask/src/commands/man_page.rs`) or CLI help text |
| `docs/design/parallel-receive-delta-application.md` | Yes | Umbrella design doc (465 lines) |
| `docs/design/parallel-receive-delta-default-on.md` | Yes | Default-on graduation plan |
| `docs/design/parallel-receive-delta-tuning.md` | Yes | Tuning surface spec |
| `docs/design/pip-4-closure-2026-05-21.md` | Yes | PIP-4 closure (superseded) |
| `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md` | Yes | Corruption root cause |
| `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` | Yes | Current wire-up plan (112 lines) |
| `docs/audit/pip-7-parallel-receive-delta-corruption.md` | Yes | Corruption audit |
| `docs/design/del-1b-reordering-buffer.md` | Yes | Cross-reference |
| `docs/design/dg-5a-concurrent-finish-file-stress-test.md` | Yes | Stress test design |
| `docs/design/asy-3-async-boundary-spec.md` | Yes | Cross-reference |
| `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md` | Yes | DashMap bench design |

---

## 6. CI Coverage

### 6.1 Workflows that explicitly build with `--features parallel-receive-delta`

| Workflow | Job name | Required? | Build command | Test command |
|----------|----------|-----------|---------------|-------------|
| `ci.yml` | `parallel-receive-delta-dist` (line 596) | **Non-required** | `cargo build --locked --workspace --profile dist --features parallel-receive-delta` | `cargo nextest run ... --features parallel-receive-delta -E 'test(parallel_threshold)'` |
| `_interop.yml` | `interop-parallel-receive-delta` (line 555) | **Non-required** | `cargo build --profile dist --bin oc-rsync --features parallel-receive-delta` | Full interop matrix against rsync 3.0.9, 3.1.3, 3.4.1, 3.4.2 |
| `bench-drain-throughput.yml` | (line 73) | N/A (bench) | `cargo build --release -p engine --features parallel-receive-delta` | Criterion bench with `--features parallel-receive-delta` |

### 6.2 Workflows that exercise it via default features

`cli` and `core` include `parallel-receive-delta` in their `default` feature
set. However, the workspace-level `bin` crate does **not** include it in its
default. Since CI builds the binary from the workspace root, default-feature
builds do **not** include `parallel-receive-delta`. The `cli`/`core` defaults
only matter when those crates are used as library dependencies.

### 6.3 CI cells that do NOT exercise it

All required CI checks (`fmt+clippy`, `nextest (stable)`, `Windows (stable)`,
`macOS (stable)`, `Linux musl (stable)`) build with default features and do not
explicitly add `parallel-receive-delta`. Since the workspace default excludes
the flag, these cells compile without the feature.

---

## 7. Inconsistencies

### I-1: `default` mismatch across crate hierarchy

`cli` and `core` include `parallel-receive-delta` in their `default` features,
but the workspace `bin` crate, `transfer`, and `engine` do not. This creates a
split where:

- Building `cli` or `core` as a library with default features enables the flag.
- Building the `oc-rsync` binary with default features does **not** enable it.
- The workspace comment says "deferred from default per PIP-7" but `cli`/`core`
  defaults were not updated to match.

**Impact:** Low. The binary is always built from the workspace `bin` crate, so
the effective default is correct (off). But the stale `cli`/`core` defaults are
misleading. Doc tests, `cargo test -p cli`, or `cargo test -p core` would
compile the flag in via default features, which may not be intended.

### I-2: `parallel.rs` and `threshold.rs` compile unconditionally

`ParallelDeltaPipeline` and `ThresholdDeltaPipeline` in the transfer crate
compile without the feature gate. They reference
`engine::concurrent_delta::consumer::DeltaConsumer` and
`engine::concurrent_delta::work_queue`, which are also unconditionally compiled
in the engine crate. Only `chunk_builder` and `parallel_apply` (the actual
parallel applier) are gated.

**Impact:** No correctness issue - the ungated code is dead in default builds
since no production receiver path dispatches to `ParallelDeltaPipeline`. But it
adds ~16 KB of compiled-but-unused code in default builds.

### I-3: Redundant forwarding in workspace `bin` crate

The workspace `parallel-receive-delta` feature forwards to all four crates
(`cli`, `core`, `transfer`, `engine`), but `cli` already forwards to `core`,
and `core` already forwards to both `transfer` and `engine`. The explicit
`transfer/parallel-receive-delta` and `engine/parallel-receive-delta` in the
workspace definition are redundant (harmless but noisy).

### I-4: No `not(feature = "parallel-receive-delta")` fallback gates

There are no negated gates (`#[cfg(not(feature = "parallel-receive-delta"))]`)
anywhere in the codebase. This is correct for the current no-op state but
worth noting - when PIP-9 wires up the parallel path, it will need
either an `else` branch or a trait-based dispatch to select between
sequential and parallel paths at compile time.

### I-5: PIP-7 corruption repro test is `#[ignore]`

`crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs`
is gated behind `#[cfg(feature = "parallel-receive-delta")]` and the test
function carries `#[ignore]`. This means it never runs in any CI cell, even the
dedicated `parallel-receive-delta-dist` job. Its value is documentation-only
until PIP-9 lands.

---

## 8. Summary

| Dimension | Count |
|-----------|-------|
| Cargo.toml definitions | 5 (bin, cli, core, transfer, engine) |
| `#[cfg]` gates in production source | 6 (4 in engine, 2 in transfer) |
| `#[cfg]` gates in test files | 3 |
| `#[cfg]` gates in bench files | 3 |
| Bench targets with `required-features` | 3 |
| CI workflows with explicit feature | 3 (ci.yml, _interop.yml, bench-drain-throughput.yml) |
| Required CI cells exercising the feature | 0 |
| Non-required CI cells exercising the feature | 2 (ci.yml, _interop.yml) |
| Feature-conditional dependencies | 0 |
| Design docs referencing the feature | 9+ |
| Identified inconsistencies | 5 |
