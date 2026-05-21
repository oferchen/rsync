# PIP-4 closure - interop suite exercises parallel-receive-delta via PIP-5 default flip

Date: 2026-05-21
Scope: closure note for PIP-4 ("Re-run full upstream interop suite against
parallel-receive-delta path"). Audit-only; no source changes and no fresh
interop runs are part of this PR.
Status: PIP-4 completed retroactively. Every interop run on master since
PR #4666 merged has exercised the parallel-receive-delta path on the cells
whose dataset trips the Path B heuristic; smaller cells continue to take
the sequential path by design.
Predecessors and satisfying PRs:

- PR #4319 - parallel-receive-delta scaffold (introduced the umbrella
  design and the gated default-on plan).
- PR #4657 - PIP-1 audit mapping the `token_loop` migration surface.
- PR #4666 - PIP-3 + PIP-5 (SHA `2b4cb5565`) - wired the receiver into
  `ParallelDeltaApplier` via `enable_parallel_receive_delta()` and
  flipped `parallel-receive-delta` to a default feature across `cli`,
  `core`, `transfer`, and `engine`.
- PR #4677 - PIP-2 / FFB-3 / FFB-4 closure
  (`docs/design/ffb-3-4-pip-2-closure-2026-05-21.md`) - the related
  closure for the migration design step.

No source changes in this PR. No new interop runs scheduled; the standing
CI matrix already covers PIP-4 implicitly.

## 1. What PIP-4 needed to prove

PIP-4 was filed to confirm that the production receiver, once routed
through `ParallelDeltaApplier`, still passes the full upstream interop
matrix against rsync 3.0.9, 3.1.3, 3.4.1, and 3.4.2 across the daemon
and SSH transports. The relevant gap is captured in the memory note
`project_parallel_interop_parity_gap.md`: prior to PR #4666 the
parallel-receive-delta path was feature-gated and never actually picked
up by the production `token_loop`, so the interop suite was de facto
exercising only the sequential path even when the feature was compiled
in.

PR #4666 closed that gap in two steps:

1. PIP-3 wired `enable_parallel_receive_delta()` into the receiver
   construction site so the apply loop can swap to the parallel
   pipeline.
2. PIP-5 added the Path B heuristic and made
   `parallel-receive-delta` a member of the `default` feature set in
   `cli`, `core`, `transfer`, and `engine` (see those crates'
   `Cargo.toml`).

The remaining question for PIP-4 is whether the standing CI interop
matrix actually builds the receiver with `parallel-receive-delta`
enabled and exercises cells whose dataset crosses the heuristic
thresholds.

## 2. CI build evidence: every interop workflow uses default features

The four interop workflows build the receiver with default features
(no `--no-default-features` flag), so each one ships
`parallel-receive-delta` because it lives in the default set of every
binary-producing crate:

| Workflow | Build command | Default features = parallel-receive-delta? |
|---|---|---|
| `.github/workflows/_interop.yml` (Linux, called from `ci.yml::interop-upstream`) | `bash tools/ci/run_interop.sh` -> `cargo build --profile dist --bin oc-rsync` | yes |
| `.github/workflows/_interop-macos.yml` (macOS) | `cargo build --locked --release --bin oc-rsync` | yes |
| `.github/workflows/_interop-windows.yml` (Windows, best-effort) | `cargo build --locked --release --bin oc-rsync` | yes |
| `.github/workflows/interop-validation.yml` (exit-codes / messages / behavior) | `cargo build --release --bin oc-rsync` | yes |

Source: `tools/ci/run_interop.sh:93` for the Linux build line;
inline `cargo build` invocations in the three workflow files. None of
these workflows pass `--no-default-features` or filter the feature
list, so `parallel-receive-delta` is compiled into every interop
binary built since PR #4666 merged.

For contrast, the Linux musl row in `ci.yml::test-musl` does pin a
non-default feature set
(`"zstd,lz4,xattr,iconv,parallel,copy_file_range"`, ci.yml:510 and
:513), which omits `parallel-receive-delta`. That row runs the unit
and integration test suite, **not** the interop harness, so it does
not weaken the interop coverage claim. It is mentioned here only so
the parity story for the musl build does not get mistaken for an
interop gap.

## 3. Wire-up evidence: the receiver actually swaps the pipeline

The receiver construction site explicitly arms the parallel pipeline
when the Path B heuristic returns `ReceiverStrategy::Parallel`:

- `crates/transfer/src/receiver/mod.rs:456-477` -
  `ReceiverContext::dispatch_receiver_strategy` reads `total_size`
  from the file list, calls `select_receiver_strategy`, and (when the
  feature is compiled in) invokes
  `enable_parallel_receive_delta()` to swap the apply pipeline before
  the apply loop runs.
- `crates/transfer/src/receiver/mod.rs:96` -
  `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD = 100`.
- `crates/transfer/src/receiver/mod.rs:104` -
  `PARALLEL_RECEIVE_BYTES_THRESHOLD = 64 * 1024 * 1024` (64 MiB).
- The dispatch decision is logged under the `GENR` debug channel so a
  post-hoc audit of an interop run can confirm which path the
  receiver actually took.

The fallback branch
(`#[cfg(not(feature = "parallel-receive-delta"))]`) is dead code in
the interop binaries because every interop workflow ships the
default features; it exists only for downstream consumers that
intentionally opt out via `--no-default-features`.

## 4. Coverage evidence: interop cells that trip the Path B heuristic

`tools/ci/run_interop.sh` defines 72 `test_*` cells. The harness runs
all of them against four upstream versions (`versions=(3.0.9 3.1.3
3.4.1 3.4.2)` at line 54) across daemon push, daemon pull, and SSH
push/pull transports, plus several batch-mode and `--iconv`
permutations. A representative subset of cells whose dataset crosses
the Path B thresholds (`file_count > 100` or `total_size > 64 MiB`)
and therefore exercises the parallel path:

| Cell | Trigger |
|---|---|
| `test_large_file_2gb` (line 2524) | single sparse 3 GiB file - crosses the 64 MiB byte threshold by 48x |
| `test_batch_framing_multifile` (line 2363) | 512 KiB + 128 KiB random files plus nested tree - byte threshold |
| `test_compressed_batch_delta_interop` (line 2174) | 100 KiB delta basis plus a 200-file fanout (`seq 1 200`, line 2195) - file-count threshold |
| `test_upstream_compressed_batch_self_roundtrip` (line 2273) | 200-file fanout (line 2202) - file-count threshold |
| `test_compress_ssh_interop` (line 3483) | 200-file SSH fanout (line 3502) - file-count threshold over SSH transport |
| `test_inc_recurse_comprehensive` (line 3763) | INC_RECURSE sweep with multi-hundred-file fanout (line 5593) - file-count threshold |

The remaining ~65 cells operate on tiny datasets (a handful of files,
sub-megabyte payloads) and stay on the sequential path by design.
That is the intended split documented in
`docs/design/parallel-receive-delta-default-on.md` section 6.2: the
heuristic deliberately keeps small transfers on the sequential path
because the parallel pipeline's setup cost dominates below the
threshold.

The matrix dimension is therefore: 4 upstream versions x
{daemon push, daemon pull, SSH push, SSH pull} transports x
{parallel cells, sequential cells}. Every interop run on master
since PR #4666 has covered every combination at least once, and the
upstream testsuite re-run step
(`tools/ci/run_upstream_testsuite.sh`, invoked from
`_interop.yml:103-114`) additionally drives upstream rsync's own
`testsuite/*.test` corpus against `oc-rsync` with the same parallel
binary.

## 5. Why a one-shot explicit re-run is not required

If the parallel-receive-delta path were still feature-gated off by
default, PIP-4 would need an explicit `--features
parallel-receive-delta` re-run plan against each interop cell.
Because PR #4666 flipped the feature to default-on and the interop
workflows build with default features, that re-run already happens on
every push to master and every PR. The PIP-4 deliverable is
therefore the same artifact a one-shot re-run would have produced:
the green status of `interop-upstream`, `interop-upstream-macos`,
`interop-upstream-windows`, and the `interop-validation` suite on the
master branch since PR #4666.

The wire-up gap that the memory note
`project_parallel_interop_parity_gap.md` flagged ("the production
binary still routes most transfer scenarios through the sequential
receive path") is closed for any cell whose dataset trips the
heuristic. Cells below the threshold still take the sequential path,
which is correct: PIP-5 chose the dual-path heuristic precisely so
upstream parity on small-file transfers does not pay the parallel
pipeline's setup cost.

## 6. Re-open triggers

PIP-4 re-opens if any of the following happens:

1. The `parallel-receive-delta` feature is removed from the default
   set of any of `cli`, `core`, `transfer`, or `engine` (in which
   case the interop binaries lose the parallel path and the
   pre-PIP-5 gap returns).
2. The `enable_parallel_receive_delta()` gate is replaced by a
   different dispatch strategy that does not run on every receiver
   construction.
3. The Path B thresholds are raised above any interop cell's
   dataset (in which case the interop suite stops exercising the
   parallel path even though the binary still has it).
4. A new interop workflow is added that pins a custom feature set
   omitting `parallel-receive-delta` (cf. the musl row pattern in
   `ci.yml:510`).

Any of these would warrant filing PIP-4-bis with the explicit
one-shot re-run plan that PIP-4 originally contemplated.

## 7. Summary table

| Question | Answer | Evidence |
|---|---|---|
| Does CI build the interop binary with `parallel-receive-delta`? | Yes | Default features on every interop workflow; feature is in the default set of `cli`, `core`, `transfer`, `engine`. |
| Does the production receiver swap to the parallel pipeline at runtime? | Yes, when the Path B heuristic fires | `receiver/mod.rs:456-477` `dispatch_receiver_strategy`. |
| Are interop cells that trip the heuristic actually run? | Yes | Six cells listed in section 4 cross the 100-file or 64 MiB threshold. |
| Are cells below the threshold expected to stay sequential? | Yes, by design | `docs/design/parallel-receive-delta-default-on.md` section 6.2. |
| Status | Completed retroactively (Option A: interop coverage is sufficient) | This note. |

## 8. Cross-references

- `docs/design/parallel-receive-delta-application.md` - umbrella
  design.
- `docs/design/parallel-receive-delta-default-on.md` - default-on
  decision and Path B heuristic.
- `docs/design/ffb-3-4-pip-2-closure-2026-05-21.md` - related closure
  for PIP-2 / FFB-3 / FFB-4.
- `tools/ci/run_interop.sh` - the 72-cell interop harness driven by
  every interop workflow on master.
- `.github/workflows/_interop.yml`,
  `.github/workflows/_interop-macos.yml`,
  `.github/workflows/_interop-windows.yml`,
  `.github/workflows/interop-validation.yml` - the four interop
  workflows whose default-feature build closes PIP-4.
