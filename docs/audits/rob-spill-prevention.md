# ROB - Reorder Buffer Spill Prevention

Parent task: ROB-1 (spill-prevention initiative for the parallel-delta
`SpillableReorderBuffer`). This audit tracks the observability and adaptive
sizing work that lets us see and then reduce disk-spill events on
normal-operation transfers.

## Status

| Task | Topic | Status |
|------|-------|--------|
| ROB-2 | `spill_activations` counter on `SpillStats` | shipped |
| ROB-3 | One-shot `warn!` on first spill activation per transfer | shipped |
| ROB-4 | Which-paths-spill audit (consumer / WholeBatch vs PerItem) | open |
| ROB-5 | Heuristics for adaptive ring sizing | open |
| ROB-6 | Bench normal-operation spill rate against upstream | open |
| ROB-7 | Adaptive ring sizing wired into `SpillPolicy` | open |

## Telemetry surface (ROB-2 / ROB-3)

`SpillStats` now exposes `spill_activations: u64` alongside the historical
`spill_events` counter:

- `spill_events` rises once per on-disk record written. With
  `SpillGranularity::PerItem` a single `spill_excess` call can produce many
  events; with `SpillGranularity::WholeBatch` it produces one event per call.
- `spill_activations` rises exactly once per `spill_excess` call that
  succeeded in writing at least one record. The counter is
  granularity-invariant, so ROB-6's bench harness and ROB-7's adaptive sizer
  can compare normal-operation spill pressure across granularities without
  compensating for record fan-out.

The one-shot `warn!` (emitted via `tracing::warn!` when the `tracing` feature
is enabled) carries the spill directory path so operators can locate the
temp files for diagnosis even before ROB-7's adaptive sizing ships. The
warning fires at most once per buffer lifetime; subsequent activations
increment `spill_activations` silently so log volume stays bounded under
sustained pressure.

## Why this is the prerequisite

ROB-4 (which-paths-spill audit) needs a per-call counter to attribute spill
pressure to specific dispatch paths. ROB-6 (bench rate) needs a counter
that does not change shape when the granularity policy is tuned. Both depend
on this PR landing first so the downstream tasks measure something stable.

## Verification convention

The buffer module does not wire `tracing-subscriber` as a dev-dependency, so
warning behaviour is verified through the existing `spill_warned()` accessor
rather than log capture. New tests for `spill_activations` exercise the
counter directly through `spill_stats()` and confirm the one-shot semantics
via the same flag the earlier `spill_warned_*` tests use.
