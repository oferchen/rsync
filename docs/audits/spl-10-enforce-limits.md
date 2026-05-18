# SPL-10 enforce-limits audit for spill module (#2332)

Verifies the current state of the `cargo xtask enforce-limits` check
against `crates/engine/src/concurrent_delta/spill.rs` after the
in-flight SPL-2/SPL-3/SPL-6 extractions, and maps the remaining overage
to the SPL tasks that close it.

## Tool

- Wrapper: `tools/enforce_limits.sh` (delegates to
  `cargo run -p xtask -- enforce-limits`).
- Source: `xtask/src/commands/enforce_limits/mod.rs` (and
  `xtask/src/commands/enforce_limits/config.rs`).
- Config: `tools/line_limits.toml`.
- CI job: `enforce-limits (informational)` in
  `.github/workflows/ci.yml:104` - `continue-on-error: true`, so
  overages are surfaced as warnings rather than blocking PRs.

## Config

`tools/line_limits.toml` declares:

- `default_max_lines = 650`
- `default_warn_lines = 400`

There is no per-file override for
`crates/engine/src/concurrent_delta/spill.rs` (nor any other file under
`crates/engine/src/concurrent_delta/`). The spill module is therefore
evaluated against the workspace default cap of **650 lines**.

The eleven existing overrides cover legacy ports
(`crates/cli/src/frontend/mod.rs`, `crates/core/src/client/mod.rs`,
`crates/daemon/src/lib.rs`, `crates/transport/src/negotiation/mod.rs`),
the buffer-pool implementation (`crates/engine/src/local_copy/buffer_pool/pool.rs`),
five negotiation sniffer test files, and the daemon test module.

## Baseline

- Branch: `origin/master` (worktree checkout).
- `crates/engine/src/concurrent_delta/spill.rs`: **1232 lines**.
- Effective cap: 650 lines.
- Delta: **+582 lines over cap**.

Adjacent files in the same module (informational; not part of the
SPL-10 scope but useful when planning future extractions):

| File                                                          | Lines | Cap | Delta |
|---------------------------------------------------------------|-------|-----|-------|
| `crates/engine/src/concurrent_delta/spill.rs`                 | 1232  | 650 | +582  |
| `crates/engine/src/concurrent_delta/consumer.rs`              | 1276  | 650 | +626  |
| `crates/engine/src/concurrent_delta/work_queue/limiter.rs`    | 1493  | 650 | +843  |
| `crates/engine/src/concurrent_delta/reorder/tests.rs`         | 1405  | 650 | +755  |
| `crates/engine/src/concurrent_delta/work_queue/tests.rs`      | 998   | 650 | +348  |
| `crates/engine/src/concurrent_delta/types.rs`                 | 896   | 650 | +246  |
| `crates/engine/src/concurrent_delta/reorder/mod.rs`           | 723   | 650 | +73   |
| `crates/engine/src/concurrent_delta/parallel_apply.rs`        | 645   | 650 | warn  |

Only `spill.rs` is in scope for SPL-10. The other entries are listed
because they share the same `continue-on-error` channel and will skew
the informational CI log alongside it.

## Run

`bash tools/enforce_limits.sh` could not execute in the audit sandbox
because cargo invocations are denied. The tool's logic was replicated
by parsing `tools/line_limits.toml` and counting newlines in every
tracked `.rs` file under the worktree (`xtask/src/commands/enforce_limits/mod.rs:111`
uses `count_file_lines`, defined in `xtask/src/util.rs`, which counts
`b'\n'` occurrences). The numbers above use the same count.

The full informational run currently reports **319 files over cap**,
dominated by integration-test suites. `spill.rs` ranks at position 74
in that list. Cleaning out every overage is out of scope here; the
audit focuses on the spill module per task #2332.

## In-flight extractions

Three leaves-first extractions are open against master at the time of
this audit:

| Task   | PR    | Extracts                                       | New file                                         | Bytes off `spill.rs` |
|--------|-------|------------------------------------------------|--------------------------------------------------|----------------------|
| SPL-2  | #4345 | `SpillError` enum + impls + `From` conversions | `spill/error.rs` (78 lines)                      | -60 lines            |
| SPL-3  | #4369 | `SpillCodec` trait                             | `spill/codec.rs` (37 lines)                      | -26 lines (atop SPL-2) |
| SPL-6  | #4386 | `SpillStats` struct                            | `spill/stats.rs` (24 lines)                      | -15 lines            |

If all three land sequentially, `spill.rs` drops to roughly **1131
lines** (-101). Still **+481 over the 650 cap**. The leaves-first
ordering matches `docs/audits/spill-rs-decomposition-plan.md`; tests
remain in `spill.rs` until SPL-8.

The remaining overage maps to the unstarted SPL tasks documented in
the decomposition plan:

| Task   | Status              | Extracts                                                                 | Plan estimate          |
|--------|---------------------|--------------------------------------------------------------------------|------------------------|
| SPL-4  | not yet open        | `SpillBackend`, `ReadWriteSeek`, `open_backend`, `spill_item`, `write_record`, `reload_item`, `recreate_spill_dir` -> `spill/tempfile.rs` | source lines 163-190, 564-687 (approx 150 lines) |
| SPL-5  | not yet open        | `DEFAULT_SPILL_THRESHOLD`, `HOT_ZONE`, `spill_excess` -> `spill/policy.rs` | source lines 57-68, 509-562 (approx 65 lines)    |
| SPL-7  | not yet open        | `SpillableReorderBuffer<T>` + `Debug` impl + accessors -> `spill/buffer.rs` | source lines 192-507 (approx 315 lines)          |
| SPL-8  | deferred            | Move tests alongside their modules -> `spill/tests/*`                    | source lines 689-1228 (approx 540 lines)         |

SPL-7 is the largest single drop, but cannot land until SPL-4 and
SPL-5 land first (the buffer struct calls into both). SPL-8 is the
mechanical follow-up and is the step that finally takes the parent
file under the 650 cap, because the tests block alone is currently
540 lines of the 1232 total.

## Overage attribution

| Extraction landed (in order) | Approx `spill.rs` size | Cap delta | Over by |
|------------------------------|------------------------|-----------|---------|
| Baseline (today)             | 1232                   | 650       | +582    |
| SPL-2 only                   | 1172                   | 650       | +522    |
| SPL-2 + SPL-3                | 1146                   | 650       | +496    |
| SPL-2 + SPL-3 + SPL-6        | 1131                   | 650       | +481    |
| ... + SPL-4 (`tempfile.rs`)  | ~981                   | 650       | +331    |
| ... + SPL-5 (`policy.rs`)    | ~916                   | 650       | +266    |
| ... + SPL-7 (`buffer.rs`)    | ~601                   | 650       | under   |
| ... + SPL-8 (`tests/*`)      | small `mod.rs` shell   | 650       | under   |

Numbers are estimates derived from the line ranges in the SPL-1 plan
(`docs/audits/spill-rs-decomposition-plan.md`). They assume each
extraction removes only the cited range plus a small `pub use`
addition in `spill/mod.rs`. Actual measurements will replace the
estimates as each PR merges. The overage is expected to clear when
SPL-7 lands; SPL-8 is then about co-locating tests with their
modules, not about clearing the cap.

## Recommendation

**Do not add a per-file override for
`crates/engine/src/concurrent_delta/spill.rs` in `tools/line_limits.toml`.**

Justifications:

1. The job is informational (`continue-on-error: true`). The overage
   does not block merges today.
2. A per-file override would communicate that the current 1232 LoC
   shape is acceptable. The SPL-1 plan disagrees and the in-flight
   PRs (SPL-2, SPL-3, SPL-6) are actively reducing it.
3. The remaining tasks (SPL-4, SPL-5, SPL-7, SPL-8) are clearly
   scoped in the plan and bring `spill.rs` under the cap without any
   override. Adding an override now would have to be unwound when
   SPL-7 lands.

The same reasoning applies to the sibling overages
(`consumer.rs`, `types.rs`, `work_queue/limiter.rs`,
`reorder/tests.rs`, `reorder/mod.rs`) which have their own decomposition
backlog. None of them currently warrant a per-file override either.

If the informational CI output starts to obscure other signal, the
preferred fix is to merge the next SPL PR in the queue (SPL-2 is
ready), not to silence the warning.

## References

- `docs/audits/spill-rs-decomposition-plan.md` - SPL-1 plan, source
  of the line ranges and split order.
- `docs/audits/spl-9-mod-reexports.md` - SPL-9 public-API contract
  the extractions must preserve.
- `xtask/src/commands/enforce_limits/mod.rs` - tool implementation.
- `xtask/src/commands/enforce_limits/config.rs` - override parsing.
- `tools/line_limits.toml` - active overrides.
- `.github/workflows/ci.yml` - `enforce-limits (informational)` job.
- PR #4345 (SPL-2), PR #4369 (SPL-3), PR #4386 (SPL-6).
