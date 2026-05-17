# 24h Cumulative Filter Fuzzer Schedule

The differential filter fuzz targets (`filter_differential` and
`filter_rules_vs_upstream`) exist to flush out divergences between oc-rsync's
filter engine and upstream rsync 3.4.x. The original requirement was a single
24h soak. A single contiguous 24h run is hostile to shared CI: it blocks one
runner for a full day, can be evicted partway, and produces a long, hard-to-
attribute log.

This page describes how the workflow trades a contiguous 24h block for a
nightly cadence that accumulates more than 24h of fuzzing per target per week.

## Schedule

- Workflow: `.github/workflows/filter-fuzzer-overnight.yml`
- Cron: `0 2 * * *` (every day at 02:00 UTC)
- Matrix: two targets per run, each fuzzed for `FUZZ_DURATION` seconds
  (default 3600 = 1 hour).
- Per night: ~2h of fuzz time (1h per target), plus build/upstream setup.
- Per week: ~14h per target, ~28h aggregate.

A 24h cumulative budget is therefore reached every 1.7 days. Any single
finding is recorded and triaged the next morning, instead of being buried at
the end of a day-long run.

## Why nightly rather than a single 24h job

- GitHub-hosted runners cap at 6h per job. A 24h soak would have to be
  stitched together across four sequential jobs anyway.
- A nightly cadence keeps the corpus warm (libFuzzer reuses prior coverage
  via the `fuzz/corpus/<target>/` directory) and means a regression introduced
  on day N is caught on night N+1 rather than 24h+rebuild later.
- Failures interrupt only a 90-minute window of capacity, so a hung target
  cannot starve other CI tenants.

## Findings flow

1. Workflow runs `tools/ci/run_filter_fuzz.sh` and
   `tools/ci/run_filter_differential_fuzz.sh` for each target.
2. If a crash or divergence reproducer lands under `fuzz/artifacts/`:
   - The job uploads `fuzz/artifacts/**` and a `fuzz-report/inventory.tsv`
     listing all reproducers + sizes as the workflow artifact
     `filter-fuzz-artifacts-<target>-<run_id>`, retained for 30 days.
   - The job fails so the morning triage notification fires.
3. Download the artifact and feed each reproducer into
   `tools/ci/triage_fuzz_artifact.sh <path>` to replay it locally against the
   same fuzz target.
4. File a bug, fix the divergence, then re-run the failing target with
   `FUZZ_DURATION` bumped (see below) to confirm the fix holds.

## Manual longer soak

Use `workflow_dispatch` to run a longer soak on demand without touching the
cron schedule:

```sh
# 4-hour run per target (~8h total wall-clock):
gh workflow run filter-fuzzer-overnight.yml -f duration=14400

# Full 24h soak per target (24h total per matrix slot; needs a self-hosted
# runner or split across multiple dispatches because GitHub-hosted runners
# cap at 6h):
gh workflow run filter-fuzzer-overnight.yml -f duration=86400
```

The dispatched value flows into the matrix via the `FUZZ_DURATION` /
`FUZZ_SECONDS` environment variables that the runner scripts already honour.

## Local reproduction

For interactive triage, the runner scripts respect the same env vars:

```sh
FUZZ_SECONDS=600 bash tools/ci/run_filter_fuzz.sh
FUZZ_DURATION=600 bash tools/ci/run_filter_differential_fuzz.sh
```

Both require:

- `cargo +nightly` (rustup `toolchain install nightly`)
- `cargo-fuzz` (`cargo install cargo-fuzz`)
- An upstream rsync binary discoverable via `OC_RSYNC_UPSTREAM_BIN` or
  `target/interop/upstream-install/3.4.{1,2}/bin/rsync`.

## Tracking

This schedule subsumes the original "run for 24h" requirement on task #1293.
The PR opening the workflow links back to #1293; subsequent triage PRs that
fix individual findings should reference the artifact name and the upstream
behaviour they restore.
