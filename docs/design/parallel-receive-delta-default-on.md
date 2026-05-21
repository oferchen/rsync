# Parallel receive-side delta apply: default-on decision (#1368 followup)

Tracking issue: #1368 followup. Companion to
`docs/design/parallel-receive-delta-application.md` (the umbrella design)
and PR #4319 (the scaffold that landed the feature behind
`--features parallel-receive-delta`).

This document does not re-litigate the apply-loop architecture, the
per-file ordering invariants, or the wire-format parity strategy. The
umbrella design covers those. It narrows one open question:

> When does `parallel-receive-delta` stop being opt-in, and what is the
> runway between the current `--features` gate and a default-on
> receiver?

## 1. Current state

`crates/engine/src/concurrent_delta/parallel_apply.rs` ships
[`ParallelDeltaApplier`] gated behind the `parallel-receive-delta`
cargo feature. Path B (runtime auto-detect) is now wired by default
and the feature is in the default set on `engine`, `transfer`, `core`,
`cli`, and the workspace binary.

- `crates/engine/Cargo.toml` declares
  `parallel-receive-delta = []` and includes it in `default`.
- `crates/transfer/Cargo.toml` declares
  `parallel-receive-delta = ["engine/parallel-receive-delta"]` and
  includes it in `default`. Same forwarding lives on `core` and
  `cli`, plus the workspace binary in the root `Cargo.toml`.
- `crates/transfer/src/receiver/mod.rs` exposes
  `ReceiverContext::enable_parallel_receive_delta` (feature-gated) and
  the always-compiled helpers `select_receiver_strategy`,
  `total_source_bytes`, and `dispatch_receiver_strategy`. The driving
  loops (`run_sync`, `run_pipelined`, `run_pipelined_incremental`)
  invoke the dispatcher immediately after `setup_transfer` returns.

The sequential apply loop in `crates/transfer/src/receiver/transfer.rs`
remains the fallback path the dispatcher picks for small / dispatch-
bound transfers, and the only path still wired when the feature is
disabled. Parity tests (PRs #4300 and #4319) keep the wire-format
contract; the bench evidence in section 4 is the remaining
follow-up (PIP-6).

## 2. The gap

Multi-core receivers underperform until manual opt-in. The verify step
inside the receiver is CPU-bound (strong-checksum rollup per chunk) but
the dispatch loop is serial. On an 8-core box transferring a 10,000-file
directory of small source files, seven cores are idle while one core
walks the per-file token reader. The parallel scaffold reclaims those
cores - in principle. Whether the reclamation pays off across the
receiver-shape distribution, or whether it loses to dispatch overhead
on dispatch-bound workloads, is the open question this bench answers.

## 3. Bench shape

`crates/engine/benches/parallel_receive_delta_perf.rs` runs three
workload classes against two cells each:

| Workload      | File count | Per-file size  | Mix                  | Why                                  |
|---------------|------------|----------------|----------------------|--------------------------------------|
| `small_files` | 10,000     | 4 KiB          | 50/50 delta + whole  | Dispatch-overhead dominates          |
| `mixed`       | 1,000      | 4 KiB - 4 MiB  | 50/50 delta + whole  | Typical project / media directory    |
| `large_files` | 10         | 256 MiB (4x64 MiB in default mode) | All delta | Cross-file parallelism the only lever |

Two cells per workload:

- `sequential_apply` - current default, walks chunks in submission
  order to a per-file in-memory sink.
- `parallel_apply` - drives `ParallelDeltaApplier` from
  `engine::concurrent_delta` with the rayon ambient pool.

The bench drives the apply loop directly. It writes to in-memory sinks
rather than real files; the goal is to isolate apply-loop scheduling
overhead from disk I/O. A real-I/O variant is the next bench in the
sequence if the in-memory result motivates it.

The large-file cell runs at 4x64 MiB by default to stay within
criterion's default sample budget; set
`OC_RSYNC_BENCH_FULL_LARGE=1` to run the full 10x256 MiB shape
documented above.

## 4. Bench results

To be filled in by the first CI run of the bench. Layout the table to
read:

| Workload      | sequential (s) | parallel (s) | speedup | bytes/s seq | bytes/s par |
|---------------|----------------|--------------|---------|-------------|-------------|
| `small_files` | TBD            | TBD          | TBD     | TBD         | TBD         |
| `mixed`       | TBD            | TBD          | TBD     | TBD         | TBD         |
| `large_files` | TBD            | TBD          | TBD     | TBD         | TBD         |

Run: `cargo bench -p engine --features parallel-receive-delta
--bench parallel_receive_delta_perf`. Capture the JSON from
`target/criterion/parallel_receive_delta_perf/` into
`target/criterion-history/` for the comparison commit so the table can
be reproduced.

## 5. Promotion criteria

The default flip from sequential to parallel is gated on all five of
the following holding simultaneously on the same nightly bench run.
Any single failure reverts the flip to opt-in.

> **Note (2026-05-21):** the maintainer directed the flip with the
> message "enable", explicitly accepting the open soak and bench gates
> (criteria 1, 3, 4, 5). Criterion 2 (zero wire-format divergence)
> remains the load-bearing safety net; PRs #4300 and #4319 keep the
> parity proptests green in CI. The other criteria become follow-up
> validation tasks (PIP-4 interop suite, PIP-6 bench backfill) rather
> than blocking gates.

1. **`small_files` wall-clock wins by >= 10%** at 4+ rayon workers.
   This is the dispatch-bound cell; if parallel cannot beat sequential
   here by a comfortable margin, the runtime overhead is paying for
   itself in noise rather than work.
2. **Zero wire-format divergence.** Already covered by the property
   tests in PRs #4300 (per-file ordering proptest) and #4319
   (parallel-vs-sequential byte-for-byte parity). The promotion PR
   re-runs the parity matrix and links the green run.
3. **No single workload regresses by more than 5%.** The `large_files`
   cell is the highest risk - per-file writes are mutex-serialised, so
   cross-file parallelism is the only lever. A regression there means
   the bench is measuring queueing overhead, not apply overhead, and
   the runtime auto-detect (path B below) becomes the only safe shape.
4. **Soak: one release cycle of `parallel-receive-delta` as a non-gating
   opt-in CI baseline.** The `--features parallel-receive-delta` build
   must compile and pass `cargo nextest run --workspace
   --all-features` on every PR for one release cycle before the flip.
   This is the existing convention - see
   `docs/design/ssh-async-default-linux.md` section 3.1 - and the
   parity gating in `docs/design/parallel-receive-delta-application.md`
   section 6.3 already names it.
5. **Two consecutive nightly runs** showing 1-3 green before the flip
   PR opens, to filter run-to-run noise.

## 6. Promotion paths

Three paths are viable. The bench output picks one.

### Path A: flip the default

Move `parallel-receive-delta` into `default-features` for both `engine`
and `transfer`. Wire `enable_parallel_receive_delta` into the receiver
construction site so every transfer takes the parallel path.

Risk: if the `large_files` cell regresses by even 1%, every VM-image
and container-layer transfer pays for it. The parallel path is the
right default only when it wins on every workload class by more than
the noise floor of the bench.

Indication: bench shows parallel wins on all three workloads by >= 10%
with no workload-class flip.

### Path B: runtime auto-detect

Keep the feature flag compiled in by default, but dispatch parallel
vs sequential per transfer at construction time based on a cheap
heuristic:

- `file_count > 100` **or** `total_size > 64 MiB` -> parallel
- otherwise -> sequential

The thresholds match the rayon parallel-stat threshold convention
(`PARALLEL_STAT_THRESHOLD = 64` per `MEMORY.md`); 100 is the next
common cutoff in the codebase and 64 MiB is the
`copy_file_range` crossover documented in
`crates/engine/benches/per_op_thresholds.rs`. No new constants invented
without an existing precedent.

The receiver knows both numbers before the transfer starts -
`file_count` from the file list segment header, `total_size` from the
sum of file sizes the generator wrote into the file entries. Both are
zero-cost reads.

Risk: heuristic mispredicts on workloads that straddle the cutoff.
Mitigation: log the dispatch decision under `--debug=GENR` so an
operator can see which path their transfer took, and surface a
`receiver_strategy_chosen` counter on `ReceiverStats` for telemetry.

Indication: bench shows parallel wins on `small_files` and `mixed` but
loses on `large_files` (or vice versa). One path cannot beat the other
on every workload class.

### Path C: per-workload CLI flag

Expose `--receive-strategy=auto|sequential|parallel` on the CLI.
`auto` defaults to path B's heuristic; `sequential` and `parallel`
override.

Risk: yet another knob on a CLI that already has more flags than the
upstream rsync man page. Justified only if telemetry from path B shows
the heuristic mispredicts often enough that operators need an escape
hatch.

Indication: path B ships, telemetry from one release cycle shows
`receiver_strategy_chosen=auto` mispredicts more than 5% of the time
across the production workload distribution.

## 7. Recommendation framing

- If the bench shows parallel wins on **all three** workloads by >= 10%
  with no regression on any cell: choose **Path A**. The cost of one
  more `default-features` entry is small; the benefit is automatic on
  every transfer.
- If the bench shows **workload-dependent winners** (parallel wins on
  some, loses on others): choose **Path B**. The runtime auto-detect
  is the only shape that captures both wins without losing on the
  workloads where parallel costs more than it earns.
- Hold **Path C** in reserve. It is the response to telemetry from a
  shipped Path B, not a first-choice promotion shape.

The default position is Path B. Receiver workloads vary widely (a
build farm differs from a backup target which differs from a CDN
mirror), and the rayon-overhead-vs-parallelism tradeoff is exactly the
kind of decision a per-transfer heuristic resolves better than a
compile-time flag. Path A is the rare-case shortcut for when the
parallel scaffold turns out to be a universal win.

## 8. Five-step plan keyed to Path B

The plan below assumes Path B is the bench-selected shape. If the
bench picks Path A instead, steps 1 and 2 collapse into a single
`Cargo.toml` edit and step 4 changes from "wire the heuristic" to
"wire the unconditional call".

1. **Land this bench.** Commit prefix `bench:`. The bench plus this
   doc are the promotion gate's evidence substrate. Without the
   numbers, none of the criteria in section 5 can be evaluated.
2. **Publish the first numbers.** Run the bench on the
   `rsync-profile` container against an 8-core baseline; commit the
   table in section 4 with the actual values. Commit prefix
   `docs(design):`. Link the criterion JSON from
   `target/criterion-history/`.
3. **Address any regression the bench reveals.** If
   `large_files` regresses by more than 5%, the per-file mutex is the
   suspect - either widen the per-file reorder buffer (current default
   64 chunks per file) or shard the per-file writer across the rayon
   pool. Commit prefix `perf(engine):`. Re-run step 2.
4. **Wire the heuristic.** Add the `file_count > 100 || total_size >
   64 MiB` dispatch decision in the receiver construction site (the
   call site of `enable_parallel_receive_delta` in
   `crates/transfer/src/receiver/mod.rs`). Surface the decision through
   `--debug=GENR` and a `receiver_strategy_chosen` counter on
   `ReceiverStats`. Commit prefix `perf(transfer):`.
   **SHIPPED (combined with step 5) via perf(transfer): enable parallel
   receive-delta by default via Path B heuristic.** Thresholds live as
   `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD` / `PARALLEL_RECEIVE_BYTES_THRESHOLD`
   in `crates/transfer/src/receiver/mod.rs`. Dispatch happens in
   `ReceiverContext::dispatch_receiver_strategy` and is invoked at the
   top of `run_sync`, `run_pipelined`, and `run_pipelined_incremental`.
   The decision is stamped on `TransferStats::receiver_strategy_chosen`
   (`ReceiverStrategy::Sequential | Parallel`) and logged via
   `debug_log!(Genr, 1, "receiver_strategy=...")` for `--debug=GENR`.
5. **Soak and flip default-features.** Run the feature opt-in as a
   non-gating CI baseline for one release cycle. If no `fix:`-prefixed
   PR lands against `crates/engine/src/concurrent_delta/parallel_apply.rs`
   or the receiver dispatch site during the cycle, move
   `parallel-receive-delta` into `default-features` on `engine` and
   `transfer`. Commit prefix `perf(transfer):` and reference both
   #1368 and this doc in the PR body.
   **SHIPPED (combined with step 4) via perf(transfer): enable parallel
   receive-delta by default via Path B heuristic.** Added to the
   `default = [...]` set on `engine`, `transfer`, `core`, `cli`, and the
   workspace binary so the production binary picks up the dispatch
   without any opt-in flag. The soak gate was waived by maintainer
   direction (see section 5 note); revert path is to drop
   `parallel-receive-delta` from each `default = [...]` list.

The five steps are strictly serial. Any step that fails its gate
stops the promotion; the feature gate remains in place for the next
attempt.

## 9. Out of scope

- Sender-side delta parallelism. The sender already pipelines per-file
  signature generation across rayon workers; the receiver is the
  remaining serial path.
- io_uring integration. The bench writes to in-memory sinks; the
  io_uring write path (`crates/fast_io/src/io_uring/`) composes with
  the parallel apply but is measured separately by
  `crates/engine/benches/local_copy_bench.rs`.
- A new wire-protocol negotiation. The parallel scaffold preserves
  wire-format byte-for-byte (see PR #4319 parity tests); no protocol
  flag is needed and no protocol flag is added.

## 10. Cross-references

- Umbrella design: `docs/design/parallel-receive-delta-application.md`.
- Scaffold PR: #4319 (`crates/engine/src/concurrent_delta/parallel_apply.rs`).
- Parity proptest PR: #4300.
- Bench: `crates/engine/benches/parallel_receive_delta_perf.rs`.
- Receiver opt-in site:
  `crates/transfer/src/receiver/mod.rs::enable_parallel_receive_delta`.
- Feature flags: `crates/engine/Cargo.toml`,
  `crates/transfer/Cargo.toml`.
- Related promotion docs:
  `docs/design/inc-recurse-sender-reenable-audit.md`,
  `docs/design/ssh-async-default-linux.md`.
- Related trackers: #1368, #4205 (wire-format audit), #4319 (scaffold).
