# POST-5 Ignored Tests Follow-Up

Re-audit of `#[ignore]` annotations after PR #4431 ("re-enable stale ignored
tests and remove obsolete entries") landed alongside roughly seventy other
merges in the same window. The baseline is the catalogue in
`docs/audits/ignored-tests.md` (PR #4418), which counted **154 attributes**
across **152 unique tests**. This follow-up confirms the cleanup stuck and
surfaces what was added since.

## Methodology

- Scope: `crates/*/src`, `crates/*/tests`, `tests/`.
- Source: `grep -rn '#\[ignore' tests/ crates 2>/dev/null | sort > /tmp/post5-current.txt`
- Filter: only lines whose payload is `#[ignore`; doc-comment mentions
  (`//!`, `///`, `//`) are excluded.
- Reconciliation: parse the function name immediately following each
  `#[ignore]` attribute, dedupe, and diff against the baseline test set
  extracted from the audit tables.

## Current counts

| Metric | Baseline (PR #4418) | Today | Delta |
| --- | --- | --- | --- |
| Total `#[ignore]` attributes | 154 | 137 | -17 |
| Unique ignored tests | 152 | 137 | -15 |

The two-attribute slack in the baseline (154 attributes vs 152 unique tests)
came from duplicate rows where the audit listed the same test in both the
"stale" section and the obsolete/blocked-on-feature section. After PR #4431
deleted those entries outright, the per-attribute and per-test counts now
match exactly.

## PR #4431 re-enables: still active

PR #4431 removed `#[ignore]` from 11 stale tests flagged in the baseline.
Each one is still active on master today (no `#[ignore]` attribute, no
`#[cfg(...)]` guard hiding the body):

| Test | Status |
| --- | --- |
| `tests/integration_basic.rs::dry_run_shows_changes_without_modifying` | Active |
| `tests/integration_basic.rs::verbose_shows_transferred_files` | Active |
| `tests/integration_checksum.rs::checksum_skips_identical_files_with_verbose` | Active |
| `tests/integration_checksum.rs::checksum_itemize_shows_checksum_indicator` | Active |
| `tests/integration_checksum.rs::upstream_comparison_checksum_with_itemize` | Active |
| `tests/integration_checksum.rs::checksum_dry_run_no_changes` | Active |
| `tests/integration_preallocate.rs::preallocate_with_verbose_shows_transfer` | Active |
| `tests/integration_sparse.rs::sparse_with_verbose_shows_transfer` | Active |
| `tests/size_filter_tests.rs::max_size_with_verbose` | Active |
| `tests/size_only_tests.rs::size_only_verbose_no_transfer_for_same_size` | Active |
| `tests/size_only_tests.rs::size_only_verbose_transfer_for_different_size` | Active |

Nothing was re-disabled by the subsequent merge cascade.

## PR #4431 deletions: still gone

The same PR deleted 5 entries flagged as obsolete or self-contradicting.
None have reappeared:

| Test | Status |
| --- | --- |
| `crates/protocol/tests/protocol_v27_compat.rs::cannot_create_codec_for_v27` | Deleted |
| `crates/protocol/tests/protocol_v27_compat.rs::cannot_create_ndx_codec_for_v27` | Deleted |
| `crates/protocol/tests/compatibility_flags.rs::print_all_flag_encodings` | Deleted |
| `crates/engine/src/local_copy/tests/execute_metadata.rs::execute_can_read_mode_0000_source_as_owner` | Deleted |
| `crates/engine/src/local_copy/tests/execute_permissions.rs::mode_0000_file_is_readable_by_owner` | Deleted |

## New ignores introduced since the baseline

Exactly one new ignored test landed between PR #4418 and this follow-up:

| Test | File:line | Reason | Recommendation |
| --- | --- | --- | --- |
| `spill_env_e2e_engages_spill_layer` | `crates/engine/tests/spill_env_e2e.rs:218` | `blocked on env-var wiring: STN-8/STN-9/STN-10` | Leave (BLOCKED-ON-FEATURE). The file-level doc comment is explicit about the gate: `LocalCopyPlan::execute` does not yet consult `OC_RSYNC_SPILL_THRESHOLD_BYTES` / `OC_RSYNC_SPILL_DIR`, so the spill counter cannot trip. Remove the `#[ignore]` once tasks STN-8 (env parse), STN-9 and STN-10 (engine wiring) merge. Tracking lives in `docs/audits/spl-8-still-blocked.md`. |

Source landed in PR #4408 ("test(engine): env-var driven E2E spill
integration test (STN-14 #2348)") on the same day as the audit but a few
minutes later, which is why it slipped past the original catalogue.

No other `#[ignore]` annotations were added during the recent merge wave.

## Bucket-level rollup

After the cleanup the 137 attributes break down as:

| Category | Count |
| --- | --- |
| EXTERNAL-BINARY (interop CI) | 64 |
| EXTERNAL-DAEMON (manual / interop CI; sshd, MOTD, read-only module, auth, access restrictions) | 8 |
| EXTERNAL-NETWORK (public hosts) | 6 |
| STRESS / SLOW / FUZZ (opt-in) | 14 |
| PLATFORM (mode-0000 cluster) | 19 |
| BLOCKED-ON-FEATURE (incl. spill env e2e, exit-codes harness, ServerModeTest harness) | 26 |

Total: 137 attributes. The 92 EXTERNAL-* and STRESS entries remain
correctly gated and run in the interop harness or by manual opt-in.

## Recommendations

1. Leave PR #4431's 11 re-enabled tests as-is. CI confirms they pass.
2. Leave the one new spill ignore in place; it is correctly gated on
   tasks STN-8/STN-9/STN-10 and the file-level doc comment already states
   the trigger to remove the attribute.
3. No further triage required from this audit. Re-run the same diff after
   the next mass-merge window or when STN-8/STN-9/STN-10 land.

## Reproduction

```sh
grep -rn '#\[ignore' tests/ crates 2>/dev/null \
  | grep -E '^[^:]+:[0-9]+:[[:space:]]*#\[ignore' \
  | wc -l
# 137
```
