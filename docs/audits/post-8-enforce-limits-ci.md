# POST-8 enforce-limits CI workflow audit

Snapshot of the `enforce-limits (informational)` job on `.github/workflows/ci.yml`
against `origin/master`. The job has been red on every PR run throughout the
recent merge cascade. This audit explains why, quantifies the workspace-wide
overage, and recommends a fix path.

## Tool surface

- Wrapper: `tools/enforce_limits.sh` (delegates to
  `cargo run -p xtask -- enforce-limits`).
- Source: `xtask/src/commands/enforce_limits/mod.rs` and
  `xtask/src/commands/enforce_limits/config.rs`.
- Config: `tools/line_limits.toml` (workspace `default_max_lines = 650`,
  `default_warn_lines = 400`).
- CI job: `.github/workflows/ci.yml:103-124`, `continue-on-error: true`.

The job runs on every push and pull request. Because `continue-on-error`
is set, the check is informational - GitHub renders the run as failed but
PR merges are not blocked.

## Current failure cause

The job is not failing on LoC overages. It is exiting with exit code 1
during config validation, before any line counting happens, because one
override path no longer exists in the workspace:

```
override path crates/transport/src/negotiation/mod.rs in
/home/runner/work/rsync/rsync/tools/line_limits.toml does not exist
```

`xtask/src/commands/enforce_limits/config.rs:188-205` (`validate_line_limit_overrides`)
turns a missing override target into a hard validation error. The
`crates/transport` crate has been removed from the workspace; the negotiation
module now lives under `crates/protocol/src/negotiation/` and
`crates/rsync_io/src/negotiation/`. The override entry has not been pruned.

Net effect: the job produces no override-evaluated LoC report at all. The
"informational" channel is silently broken, so nobody can use it to track the
overage backlog the comment on `.github/workflows/ci.yml:99` references.

## Override audit

`tools/line_limits.toml` lists 11 overrides. Current line counts vs caps:

| Override path                                                    | Cap   | Current | Status               |
|------------------------------------------------------------------|-------|---------|----------------------|
| `crates/cli/src/frontend/mod.rs`                                 | 5700  | 324     | Under cap by 5376    |
| `crates/core/src/client/mod.rs`                                  | 5800  | 145     | Under cap by 5655    |
| `crates/daemon/src/lib.rs`                                       | 5500  | 198     | Under cap by 5302    |
| `crates/daemon/src/tests.rs`                                     | 600   | 362     | Under cap by 238     |
| `crates/transport/src/negotiation/mod.rs`                        | 6400  | -       | **File missing**     |
| `crates/protocol/src/negotiation/tests/detector.rs`              | 480   | 422     | Under cap by 58      |
| `crates/protocol/src/negotiation/tests/detector_sniffer.rs`      | 530   | 486     | Under cap by 44      |
| `crates/protocol/src/negotiation/tests/sniffer_read.rs`          | 580   | 534     | Under cap by 46      |
| `crates/protocol/src/negotiation/tests/sniffer_reset.rs`         | 585   | 537     | Under cap by 48      |
| `crates/protocol/src/negotiation/tests/sniffer_take.rs`          | 570   | 522     | Under cap by 48      |
| `crates/engine/src/local_copy/buffer_pool/pool.rs`               | 1200  | 1169    | Under cap by 31      |

Every existing override target is well under its current cap. The first
four (`frontend/mod.rs`, `client/mod.rs`, `daemon/src/lib.rs`,
`negotiation/mod.rs`) were sized for legacy monoliths that have since
been decomposed - the 5500-6400 line caps are now meaningless. The
override list is stale across the board.

## Workspace overage snapshot

Once the stale override is removed and the job runs to completion, the
workspace produces **322 files over the default 650-line cap** (out of
2512 tracked `.rs` files, ignoring `target/` and `.git/`). Numbers below
were computed by replicating `xtask/src/util.rs::count_file_lines` (counts
`\n` occurrences) outside the cargo sandbox; the prior audit
(`docs/audits/spl-10-enforce-limits.md`) put the count at 319 and
`docs/audits/module-loc-audit-session.md` at 324, so the figure is
slowly drifting upward.

### Top 10 worst offenders

| Rank | Lines | Over by | File                                                                          | Kind            |
|------|-------|---------|-------------------------------------------------------------------------------|-----------------|
| 1    | 2959  | +2309   | `crates/protocol/src/flist/write/tests.rs`                                    | unit tests      |
| 2    | 2951  | +2301   | `crates/cli/src/frontend/arguments/tests.rs`                                  | unit tests      |
| 3    | 2901  | +2251   | `crates/engine/src/local_copy/executor/file/sparse/tests.rs`                  | unit tests      |
| 4    | 2854  | +2204   | `crates/transfer/src/generator/tests.rs`                                      | unit tests      |
| 5    | 2816  | +2166   | `crates/protocol/src/negotiation/capabilities/tests.rs`                       | unit tests      |
| 6    | 2698  | +2048   | `crates/cli/src/frontend/tests/output_parity.rs`                              | integration     |
| 7    | 2614  | +1964   | `crates/engine/src/local_copy/tests/execute_directories.rs`                   | integration     |
| 8    | 2510  | +1860   | `crates/core/src/client/remote/invocation/tests.rs`                           | unit tests      |
| 9    | 2507  | +1857   | `crates/protocol/src/flist/read/tests.rs`                                     | unit tests      |
| 10   | 2431  | +1781   | `crates/engine/src/local_copy/tests/execute_skip.rs`                          | integration     |

Every entry in the top 10 is a test module. The worst non-test source
files immediately below the cut are:

| Lines | File                                                             |
|-------|------------------------------------------------------------------|
| 2129  | `crates/engine/src/local_copy/buffer_pool/buffer_controller.rs`  |
| 1922  | `crates/engine/src/concurrent_delta/spill/buffer.rs`             |
| 1761  | `crates/metadata/src/acl_windows.rs`                             |
| 1669  | `crates/fast_io/src/splice.rs`                                   |
| 1629  | `crates/fast_io/src/io_uring/buffer_ring.rs`                     |
| 1604  | `xtask/src/commands/benchmark.rs`                                |
| 1515  | `crates/transfer/src/delta_pipeline.rs`                          |
| 1493  | `crates/engine/src/concurrent_delta/work_queue/limiter.rs`       |
| 1330  | `crates/batch/src/replay.rs`                                     |
| 1325  | `crates/engine/src/local_copy/hard_links.rs`                     |

## Recommendation

**Keep the job informational. Do not promote to a required check yet.**

Justifications:

1. 322 over-cap files is an order of magnitude too large to land as a
   blocking gate. Flipping `continue-on-error: false` today would lock
   every PR.
2. The top 10 are all test modules. Splitting a 2900-line unit-test
   suite is mechanical busywork; the line count is not load-bearing
   complexity. Test-file caps should likely be relaxed (or test files
   excluded) before non-test caps are tightened.
3. Active decomposition work is already in flight against the
   non-test offenders (SPL-2..SPL-8 against
   `crates/engine/src/concurrent_delta/spill/`, BUF-* against
   `buffer_pool/buffer_controller.rs`). Per-file overrides would
   undercut that work.

### Concrete fixes

1. **Prune the stale `crates/transport/...` override.** Remove the entry
   at `tools/line_limits.toml:30-33`. This alone restores the
   informational signal - the job will run to completion and report
   the 322 overages as `::error file=...::` annotations on every PR.
2. **Drop the four obsolete legacy overrides.** The
   `frontend/mod.rs`, `client/mod.rs`, `daemon/src/lib.rs`, and
   `daemon/src/tests.rs` files are now 145-362 lines. Their 600-5800
   caps no longer reflect reality and only add noise to the config.
   The five `protocol/src/negotiation/tests/sniffer_*.rs` overrides
   can likewise be dropped - those files are 422-537 lines and would
   pass the default 650 cap.
3. **Keep the `buffer_pool/pool.rs` override (1169/1200).** It is the
   only override still serving its original purpose: a real ceiling
   above an actively-maintained source file.
4. **Decide on test-file policy.** Either raise the cap for files
   matching `**/tests.rs` and `**/tests/**.rs` (a new `kind = "test"`
   match in the config), or accept that the informational job will
   stay red until per-test-file overrides are issued one at a time.
   Recommended: add a second default
   (`default_test_max_lines = 3000`, say) so the 322-file overage
   count drops to the ~50 production-source files that actually
   need to shrink.
5. **For each non-test offender, prefer split over override:**
   - `buffer_controller.rs` (2129) - already paired with
     `buffer_pool/pool.rs` extraction; continue down the same path.
   - `concurrent_delta/spill/buffer.rs` (1922),
     `work_queue/limiter.rs` (1493) - tracked by SPL-* and the
     concurrent-delta decomposition backlog.
   - `metadata/src/acl_windows.rs` (1761),
     `fast_io/src/splice.rs` (1669),
     `fast_io/src/io_uring/buffer_ring.rs` (1629) - platform
     wrappers; candidates for `cfg`-gated sub-modules.
   - `xtask/src/commands/benchmark.rs` (1604) - already partially
     split into `xtask/src/commands/benchmark/`; finish the move.
   - `transfer/src/delta_pipeline.rs` (1515),
     `batch/src/replay.rs` (1330),
     `engine/src/local_copy/hard_links.rs` (1325) - separate
     extraction tasks; not yet tracked.

### Promotion criteria

Flip `continue-on-error: false` only when:

- Stale overrides are pruned (item 1 + 2).
- Test-file policy is resolved (item 4).
- The remaining over-cap count is small enough that every entry
  has a tracked decomposition task or an active per-file override.

Until then, the comment block at `.github/workflows/ci.yml:97-102`
should be updated to reflect the current overage count (322, not 19)
and the missing-override blocker so future readers know the job is
not silently reporting clean.

## References

- `tools/enforce_limits.sh` - shell entry point.
- `xtask/src/commands/enforce_limits/mod.rs` - command implementation.
- `xtask/src/commands/enforce_limits/config.rs` - override loader and
  `validate_line_limit_overrides` (source of the current failure).
- `tools/line_limits.toml` - active config.
- `.github/workflows/ci.yml:103-124` - `enforce-limits (informational)`
  job definition.
- `docs/audits/module-loc-audit-session.md` - prior baseline (324 files,
  19 commit-touched).
- `docs/audits/spl-10-enforce-limits.md` - prior baseline (319 files)
  and recommendation to defer per-file overrides until decomposition
  lands.
