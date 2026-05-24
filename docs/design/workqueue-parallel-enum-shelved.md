# WorkQueue Parallel Multi-Root Enumeration - Shelved

## Decision

WorkQueue parallel multi-root enumeration in the INC_RECURSE sender is shelved.
No further design or implementation work is planned until the re-evaluation
criteria below are met.

## Background

The INC_RECURSE sender currently performs flist partitioning serially. For each
source root, the sender walks the directory tree, builds file entries, and
emits sub-list segments in deterministic order. On very large multi-root
pushes (for example, several million-file roots passed in a single invocation),
the serial enumeration phase dominates sender start time before the first byte
of file data is transferred.

The WorkQueue design proposed parallelising enumeration across roots: a pool
of workers would walk each root concurrently, feeding a coordinator that
assigned sub-list segment numbers and emitted entries on the wire in a
protocol-compatible order.

## Why Shelved

- **Narrow benefit.** The optimisation only helps sender start time, and only
  on huge multi-root pushes. Single-root transfers, daemon transfers, and the
  steady-state data phase see no improvement. The audience is a small slice
  of real workloads.
- **High complexity.** Wire-compatible parallel enumeration requires
  coordinated sub-list segment numbering, careful ordering of `flist_eof`
  markers, and synchronisation around the shared output stream. Each of these
  is a source of subtle interop bugs that would need full upstream parity
  testing across 3.0.9, 3.1.3, 3.4.1, and 3.4.2.
- **Precedent.** The parallel chunks design was shelved on 2026-03-28 for the
  same shape of reason: narrow benefit not worth wire protocol churn. The
  WorkQueue proposal sits in the same category - speculative perf work whose
  payoff does not justify the protocol-care budget.
- **Current baseline is captured.** The ISI.g bench in PR #4862 records the
  best-case sender start time on a 100K-file source. That bench is the
  reference for how fast the sender can start today. WorkQueue must not be
  promised as a future improvement until that bench shows a meaningful
  regression against upstream.

## Re-evaluation Criteria

Any one of the following would justify revisiting the decision:

- User-reported sender start times exceeding 30 seconds on million-file pushes
  that are otherwise wire-compatible with upstream rsync.
- Discovery of a simpler wire-compatible enumeration approach, for example
  parallel `readdir` that does not require sub-list renumbering or coordinator
  synchronisation.
- ISI.g bench (PR #4862) shows a 5x or larger regression versus upstream
  rsync sender start time.

Until then, treat the serial flist partition step as the intended design.
