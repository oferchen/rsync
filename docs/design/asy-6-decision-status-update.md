# ASY-6: Decision status update - adopt or defer async pipeline

Status: Status update. Supplements
`docs/design/asy-6-adopt-or-defer-decision.md` (the binding decision
doc). The ASY-6 decision remains **Option B: defer** pending benchmark
evidence from ASY-4.a/b/c.

Date: 2026-06-01.

## 1. Current decision status

The ASY-6 decision document chose Option B (defer) on the basis that
three design docs without bench data do not justify committing 10-14 PRs
of async conversion churn. That reasoning has not changed.

**Decision: defer remains in effect.** The exit criteria (Section 3 of
the original doc) are not yet satisfied because ASY-4 benchmark data has
not been produced. No implementation tickets (ASY-7..12) have been
opened.

## 2. What has been validated

The design phase (ASY-1..6) and prototype sketches (ASY-7.a/b through
ASY-10.a/b) are complete:

| Task | Deliverable | Status |
|------|-------------|--------|
| ASY-1 | Threading model audit (12 boundaries, 8 invariants) | Done |
| ASY-2 | `tokio-transfer` feature spec, open questions | Done |
| ASY-3 | Per-boundary async disposition (6 await, 4 spawn_blocking, 1 dissolve, 1 unchanged) | Done |
| ASY-5.a | Embeddability test harness spec | Done |
| ASY-5.b | Embeddability harness implementation | Done |
| ASY-5.c | Embeddability gap inventory (9 gaps, 5 independent of ASY-6) | Done |
| ASY-6 | Adopt/defer/close decision | Done (defer) |
| ASY-7.a | Receiver tokio prototype design | Done |
| ASY-7.b | Receiver tokio prototype implementation sketch | Done |
| ASY-8.a | Sender tokio prototype design | Done |
| ASY-8.b | Sender tokio prototype implementation sketch | Done |
| ASY-9.a | io_uring async dispatch design | Done |
| ASY-9.b | io_uring async dispatch implementation sketch | Done |
| ASY-10.a | token_loop async migration design | Done |
| ASY-10.b | token_loop async migration implementation sketch | Done |
| ASY-12.a | Concurrent transfers async vs threaded bench design | Done |

**Embeddability findings (ASY-5.c).** Nine gaps identified. Five gaps
(G2 cancellation token, G3 BufferPool per-transfer, G4 rayon pool
ownership, G7 signal handler opt-out, G8 feature probe overrides) are
independent of the adopt decision and can ship under the current
synchronous architecture. Four gaps (G1 async API, G5 async daemon
accept, G6 russh bridge dissolution, G9 daemon library API) are gated on
full or partial async conversion.

Key conclusion: the most impactful embedding improvement (cooperative
cancellation, G2) does not require async. This weakens the urgency for
Option A unless high-concurrency SSH transfers (G1+G6) are a demanded
use case.

## 3. Remaining bench tasks gating the decision

The following tasks must complete before the defer window can exit:

| Task | Description | Blocked on |
|------|-------------|-----------|
| ASY-4.a | Bench harness infrastructure (client driver, resource sampler, coordinator) | Linux hardware access |
| ASY-4.b | Threaded baseline measurement (all concurrency levels, all workloads) | ASY-4.a |
| ASY-4.c | Tokio prototype measurement (requires ASY-7..10 feature-flag builds) | ASY-4.a + ASY-7..10 impl |
| ASY-7.c | Receiver tokio prototype bench run | ASY-4.a + ASY-7.b impl |
| ASY-8.c | Sender tokio prototype bench run | ASY-4.a + ASY-8.b impl |
| ASY-9.c | io_uring async dispatch bench run | ASY-4.a + ASY-9.b impl |

ASY-4.a is the critical-path item. The bench harness design
(`docs/design/concurrent-transfers-async-vs-threaded-bench.md`) is
complete and specifies: 6 concurrency levels (C1 through C1024), 4
workload profiles (small-files, large-files, mixed, delta-update), ABAB
variant ordering, bootstrap CI with Bonferroni correction for
significance, and tiered decision criteria with explicit adopt/defer/close
thresholds.

**Practical blocker.** ASY-4.a requires a dedicated Linux bench host
(tmpfs backing, CPU pinning, network namespace isolation) to produce
results with acceptable variance. macOS is unsuitable due to missing
`/proc` resource sampling and `taskset` CPU pinning. The bench harness
has not been implemented because Linux hardware has not been provisioned
for this purpose.

## 4. Preliminary findings from prototype work

The completed prototype designs (ASY-7.b, ASY-8.b, ASY-9.b, ASY-10.b)
produce several architectural observations that inform the eventual
bench interpretation:

### 4.1 Receiver path (ASY-7)

- The SPSC spin channel dissolves into `tokio::sync::mpsc` with bounded
  backpressure. Expected overhead: one atomic CAS per message vs the
  current zero-syscall spin-wait. At high throughput the spin-wait is
  cheaper; at low throughput the mpsc yields CPU more gracefully.
- Signature batching moves to `spawn_blocking` with rayon inside. The
  async boundary adds a task-switch per batch but does not change the
  compute cost.
- Disk commit stays in `spawn_blocking` (long-lived). io_uring remains
  synchronous per ASY-9.a decision.

### 4.2 Sender path (ASY-8)

- Source file reads become `tokio::fs::File` operations (or
  `spawn_blocking` for mmap). The overlap benefit is theoretical: on
  local transfers the read is memory-mapped and instant; on remote
  transfers the wire write is the bottleneck, not file read.
- Hash computation (`rolling + strong checksum`) is CPU-bound. It stays
  inside `spawn_blocking`. No async benefit on the compute hot path.
- Token emission gains from async wire write only if multiple files are
  in flight simultaneously (pipelining). The current serial
  one-file-at-a-time sender model does not pipeline.

### 4.3 io_uring interaction (ASY-9)

- ASY-9.a decided: io_uring stays synchronous behind `spawn_blocking`.
  `tokio-uring` is rejected due to thread-per-ring constraint and
  `!Send` futures incompatible with work-stealing. This means the async
  migration does not unlock new io_uring capabilities - the disk commit
  path is identical under both models.
- The theoretical async win for io_uring (SQE submission pipelining
  across files) is not realizable without `tokio-uring` adoption, which
  is rejected.

### 4.4 token_loop (ASY-10)

- The token_loop is the innermost receiver hot path. Converting `read_exact`
  to `AsyncReadExt::read_exact` adds one poll transition per token. At
  ~10 tokens/file on average across 100K files, that is ~1M additional
  poll wakeups per transfer.
- The benefit is yielding between tokens, enabling cooperative
  cancellation and reducing tail latency under contention. The cost is
  measurable only under bench.

### 4.5 Summary of expected trade-offs

| Dimension | Threaded (current) | Tokio (proposed) |
|-----------|-------------------|-----------------|
| Single-connection latency | Lower (no poll overhead) | Slightly higher |
| Memory at scale (C256+) | ~2 MiB stack per thread | ~8 KiB task state |
| CPU efficiency at C1-C16 | Equivalent | Equivalent or slightly worse |
| Cancellation | Not possible | Cooperative via CancellationToken |
| Embeddability | spawn_blocking wrapper | Native async API |
| io_uring benefit | Unchanged | Unchanged (stays in spawn_blocking) |

The table above is hypothesis, not measurement. ASY-4 exists to
validate or refute it.

## 5. Risk assessment update

### 5.1 Tokio dependency weight

As of tokio 1.44 (current), the `tokio-transfer` feature would add:

- ~400 KiB to binary size (rt-multi-thread + io + sync + time features).
- 7 transitive dependencies (mio, socket2, pin-project-lite, bytes,
  parking_lot, signal-hook-registry, libc - most already present).
- CVE exposure surface: tokio averages 1-2 advisories/year (RustSec).
  The daemon already depends on tokio for the async accept prototype and
  russh bridge, so the marginal exposure increase for `tokio-transfer`
  is modest.

### 5.2 Runtime overhead

- Tokio's multi-thread scheduler adds one `epoll_wait` syscall per event
  loop tick. Under sustained throughput, this is amortized across many
  tasks.
- Task wake/poll overhead is ~100 ns per transition (measured by
  `tokio-metrics` on similar workloads). For the receiver hot path with
  ~10 tokens/file, this adds ~1 microsecond per file.
- The SPSC channel replacement (`tokio::sync::mpsc`) is ~3x slower than
  the current lock-free spin channel on microbenchmarks but yields CPU
  under low load, reducing power consumption for idle daemons.

### 5.3 Maintenance burden

- Dual pipeline (threaded + async behind feature flag) requires parallel
  test matrices. CI cost doubles for transfer-related tests.
- The ASY-12 gate (flip async to default) is the exit from dual
  maintenance. Until the gate fires, both paths must stay green.
- Code complexity: `#[cfg(feature = "tokio-transfer")]` guards at 6
  boundary sites, plus shared trait abstractions for wire read/write.
  Estimated 800-1200 lines of feature-gated code during the transition
  window.

### 5.4 Opportunity cost

- Every PR spent on async conversion is a PR not spent on perf
  optimization, upstream parity, or platform hardening.
- The threaded model currently meets all performance targets (local 3x+,
  daemon 2x+, SSH on par with upstream). There is no correctness or
  performance deficiency that async uniquely solves.

## 6. Timeline

The decision can be made when all bench tasks complete. The dependency
chain is:

```
Linux bench host provisioned
    |
    v
ASY-4.a (harness implementation)  ~1 week dev effort
    |
    v
ASY-4.b (threaded baseline)       ~2 days run time
    |
    +---> Threaded baseline available
    |
    v
ASY-7..10 implementation PRs       ~4-6 weeks (10-14 PRs)
    |
    v
ASY-4.c + ASY-7.c/8.c/9.c (tokio bench)  ~2 days run time
    |
    v
ASY-6 re-evaluation with bench evidence
```

**Estimated calendar time from today:** 6-10 weeks assuming Linux
hardware is available and ASY-7..10 implementation proceeds without
blocking issues.

**Alternative fast path:** If the ASY-5.c finding (that the most
impactful embeddability gaps are async-independent) is accepted as
sufficient to close the embeddability argument, and if no performance
regression exists in the threaded model at scale, the decision could
move to Option C (close) without waiting for the full bench. This would
require explicit acceptance that the 10K-connection daemon ceiling and
high-concurrency SSH ceiling are acceptable permanent constraints.

## 7. Recommendation

Maintain Option B (defer). The design investment is preserved, the
prototype sketches are complete, and the remaining blocker is purely
empirical. No urgency exists to force either adopt or close:

- The threaded model meets all stated performance targets.
- The embeddability gaps with highest impact-to-effort ratio are
  async-independent (G2, G3, G4, G7).
- No user has requested the async API (G1) or high-concurrency daemon
  embedding (G5).

When bench hardware becomes available, prioritize ASY-4.a/b to establish
the threaded baseline. The baseline alone may be sufficient to justify
Option C if it demonstrates that the threaded model saturates available
bandwidth at all concurrency levels tested.

## 8. Cross-references

- `docs/design/asy-6-adopt-or-defer-decision.md` - binding decision.
- `docs/design/asy-5c-embeddability-gap-list.md` - embeddability evidence.
- `docs/design/concurrent-transfers-async-vs-threaded-bench.md` - bench
  design (ASY-12.a).
- `docs/design/ssh-transport-async-io-eval.md` - SSH async evaluation.
- `docs/design/async-migration-plan.md` - phased migration plan.
- `docs/design/sender-tokio-prototype.md` - ASY-8.a sender prototype.
- `docs/design/receiver-tokio-prototype.md` - ASY-7.a receiver prototype.
- `docs/design/token-loop-async-migration.md` - ASY-10.a token_loop.
- `docs/design/iouring-async-dispatch.md` - ASY-9.a io_uring dispatch.
