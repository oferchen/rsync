## io_uring submission modes - benchmark plan

Tracking issue: oc-rsync task #1626. Sibling audits:
[`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md),
[`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md),
[`docs/audits/shared-iouring-session-instance.md`](shared-iouring-session-instance.md).

## Summary

This audit plans the empirical benchmark that should decide whether oc-rsync
flips the default io_uring submission mode away from regular `submit_and_wait()`
toward kernel-side polling (`IORING_SETUP_SQPOLL`) - and if so, on what kernel,
workload, and privilege profile. The decision is currently driven by a static
default (`IoUringConfig::sqpoll = false`) with an opt-in path that falls back
on `EPERM`. We have no production numbers showing whether the kernel-poller
saving (no `io_uring_enter` per submit) outweighs the cost of a permanently
busy kernel thread for rsync's bursty disk I/O. This plan locks in workloads,
metrics, and pass/fail criteria so the benchmark, when run, produces numbers
that justify any default change.

The benchmark is wire-compatible: SQPOLL changes only how SQEs reach the
kernel, not what the kernel does with them, so no protocol implications.

Upstream evidence: a recursive grep for `io_uring`, `IORING_`, `liburing`,
`SQPOLL` in `target/interop/upstream-src/rsync-3.4.1/` returns no matches.
Upstream rsync uses plain `read(2)`/`write(2)`, so any io_uring tuning is a
pure oc-rsync optimisation with no upstream parity constraint.

## Current submission modes in `crates/fast_io/src/io_uring/`

The submission path is funnelled through one builder:

- `crates/fast_io/src/io_uring/config.rs:438-453` `IoUringConfig::build_ring`
  is the single ring constructor. It reads `self.sqpoll` and either calls
  `io_uring::IoUring::builder().setup_sqpoll(self.sqpoll_idle_ms).build(...)`
  or falls back to `RawIoUring::new(self.sq_entries)` (plain ring).
- `crates/fast_io/src/io_uring/config.rs:332-341` documents the two SQPOLL
  knobs: `sqpoll: bool` (off by default) and `sqpoll_idle_ms: u32` (1000 ms
  default before the kernel thread sleeps).
- `crates/fast_io/src/io_uring/config.rs:369-383` `Default::default` plus
  `for_large_files`/`for_small_files` all set `sqpoll: false`. SQPOLL is
  never enabled implicitly.
- `crates/fast_io/src/io_uring/config.rs:25-46` exposes a process-wide
  `SQPOLL_FALLBACK: AtomicBool` flag and the `sqpoll_fell_back()` accessor
  used by `--version` / diagnostics so operators can confirm whether the
  privileged path was actually taken.

Once the ring is built, every submission goes through one wrapper:

- `crates/fast_io/src/io_uring/shared_ring.rs:402-406`
  `SharedRing::submit_and_wait(wait_for)` is a thin pass-through to
  `io_uring::IoUring::submit_and_wait`. With SQPOLL the call still runs but
  the kernel thread has usually already drained the SQ, so it short-circuits
  in `io_uring_enter`. With the plain ring it issues the full
  `io_uring_enter(2)` syscall.

Mode summary:

| Mode | Build call | Per-submit syscall | Kernel thread |
|------|------------|--------------------|---------------|
| Regular (default) | `IoUring::builder().build(sq_entries)` | `io_uring_enter` per `submit_and_wait` | none |
| SQPOLL (opt-in) | `builder.setup_sqpoll(idle_ms).build(...)` | none while poller is active; one wakeup `io_uring_enter` after `sqpoll_idle_ms` of idleness | one `io_uring-sq` thread per ring |
| Standard I/O (fallback) | n/a (no ring) | `pread64`/`pwrite64`/`writev` per op | none |

The plain `RawIoUring::new` path is also what is reached on every kernel
older than 5.6 or any time io_uring is feature-disabled
(`#[cfg(not(all(target_os = "linux", feature = "io_uring")))]` stubs in
`crates/fast_io/src/io_uring_stub.rs`). The benchmark therefore covers
three rings (SQPOLL, regular, none) and not two.

## Trade-offs

### SQPOLL

- Pro: zero `io_uring_enter` syscalls while the kernel poller is hot. Small
  bursty submits (4 KB random writes) drop from one syscall per submit to
  one per `sqpoll_idle_ms` window.
- Pro: amortised submission latency falls. The SQE is visible to the kernel
  the moment we increment the SQ tail; no userspace round-trip.
- Con: the `io_uring-sq` kernel thread runs at 100% of one core while busy.
  On idle/sparse rsync sessions this is pure waste.
- Con: requires `CAP_SYS_NICE` on kernels older than 5.13. Older systems
  return `EPERM` and we silently fall back, which means a benchmark must
  measure the privileged and unprivileged process separately.
- Con: kernel-thread scheduling pressure. With many concurrent rsync
  processes (daemon mode, tens of connections), each ring spawns its own
  poller. They contend on CPU and on the receive ring, which can starve
  the original userspace path.
- Con: pinning. Without `IORING_SETUP_SQ_AFF` the kernel thread floats; a
  cache-cold migration on every submit defeats the saving. Pinning needs
  to be part of the experiment, not assumed.
- Con: cgroup CPU accounting is misleading - the io_uring kernel thread
  is parented to the submitting task, but its time shows as `system` in
  `top` and as `irq` in some kernels. Operators reading load averages on
  small VMs see numbers that look like a regression even when throughput
  improves.

### Regular submission

- Pro: zero idle CPU - syscall only when we have work.
- Pro: no privilege requirement. Works as `nobody` in containers without
  `CAP_SYS_NICE`.
- Pro: predictable scheduler behaviour - the submitter is also the reaper,
  so completion latency tracks `submit_and_wait` directly.
- Con: every submit costs an `io_uring_enter`. For rsync's signature-block
  pipeline (one 700-byte block per `WRITE`) this is the dominant overhead
  vs. SQPOLL.
- Con: under heavy fan-out (daemon, parallel transfers) the syscall count
  scales linearly with submission count. SQPOLL converts this to a flat
  cost.

### Standard I/O baseline

Required as the floor. If io_uring (in either mode) does not beat plain
`pwrite64` + `writev` on the same workload, the io_uring code path is dead
weight. We have measured the wins informally; the benchmark must record
them as numbers, not folklore.

## Proposed bench

The benchmark binary lives under
`crates/fast_io/benches/io_uring_submission.rs` and is driven by Criterion
plus `perf stat` instrumentation. Three workloads, three modes, three queue
depths, all combinations. Reproducer image is the existing
`localhost/oc-rsync-bench:latest` container so results match the standard
benchmark machine.

### Workloads

1. **4 KB random writes**: 64 MB working set on tmpfs and on ext4, write
   one 4 KB block at a random offset, `O_DIRECT` and buffered variants.
   Captures the worst case for syscall amortisation - many tiny SQEs,
   completions arrive in arbitrary order.
2. **1 MB sequential writes**: 1 GiB file written start to end in 1 MiB
   chunks, ext4, no `O_DIRECT`. Captures the rsync receiver's typical disk
   write pattern. Each SQE is large; the question is whether SQPOLL still
   wins when the per-op time dominates the per-syscall time.
3. **Network reads**: paired client/server inside the same container,
   server `sendfile`s 1 GiB over a `127.0.0.1` TCP socket, client uses an
   io_uring socket reader (`crates/fast_io/src/io_uring/socket_reader.rs`)
   with each mode. Captures the daemon mode profile where the same ring
   fans `IORING_OP_RECV` SQEs across a connection. Loopback only - WAN
   latency would swamp the submission cost we are trying to measure.

Each workload runs at queue depth 1, 8, and 64. QD=1 isolates per-submit
cost. QD=8 reflects the daemon socket reader's typical inflight count.
QD=64 saturates the default `sq_entries` and forces the regular ring into
back-to-back `submit_and_wait` cycles.

### Run matrix

```
workload x mode x QD = 3 x 3 x 3 = 27 combinations
mode = { sqpoll_caplinked, regular, std_io }
sqpoll variants: pinned to CPU 1 (SQ_AFF), unpinned, idle_ms=10/100/1000
```

Repeat each combination 10 times; report median and p99. Total wall time
budgeted at ~25 minutes on the bench container.

### Privilege variants

- Run 1: process has `CAP_SYS_NICE` (root or `setcap cap_sys_nice+ep`).
- Run 2: unprivileged - asserts the fallback path matches the regular
  numbers exactly. Sanity check, not a comparison axis.

## Metrics

| Metric | Source | Why |
|--------|--------|-----|
| Throughput (MB/s, ops/s) | Criterion timer | Headline number |
| p50 / p99 op latency | Criterion histogram | Catches tail latency from kernel-thread migration |
| User CPU % | `perf stat -e task-clock:u` | SQPOLL should reduce this |
| Kernel CPU % | `perf stat -e task-clock:k` | SQPOLL should *increase* this; the question is by how much |
| Context switches | `perf stat -e context-switches` | Direct proxy for `io_uring_enter` calls saved |
| `io_uring_enter` calls | `bpftrace` kprobe | Confirms the context-switch delta is from the right place |
| Resident kernel-thread CPU | `/proc/<pid>/task/<tid>/stat` for the `io_uring-sq` tid | Calls out the dedicated-core cost |
| Cache misses (LLC-load-misses) | `perf stat` | Detects SQ_AFF migration penalty |

Throughput and p99 are the deciding metrics; the others are diagnostic.

## Recommendation criteria

A mode wins for a given workload + QD if it dominates the others on the
following ranked tests:

1. p99 latency within 10 % of the best.
2. Throughput within 5 % of the best.
3. Total CPU (user + kernel) lowest among the modes that pass (1) and (2).

Concrete decision rules for changing the default:

- **Flip `sqpoll: false` to `true` in `IoUringConfig::default()`**
  only if SQPOLL wins on at least two of the three workloads at QD=8 *and*
  the unprivileged-fallback path remains within 2 % of regular submission
  (i.e. no regression for users without `CAP_SYS_NICE`).
- **Add a new `IoUringConfig::for_daemon()` preset with SQPOLL enabled**
  if SQPOLL wins specifically on the network-read workload at QD=8 or
  QD=64. Daemon mode owns the ring per connection so the kernel-thread
  cost is bounded by connection count.
- **Keep SQPOLL opt-in only** if regular submission is within 5 % on every
  workload. The kernel-thread overhead is then unjustified for the
  general case.
- **Keep both io_uring modes off by default for a workload** if standard
  I/O is within 5 % at every QD on that workload. We then route that
  workload to the non-uring fallback even when io_uring is available.

The benchmark output (CSV + Criterion report) lands at
`docs/benchmarks/iouring-sqpoll-1626/` and the recommendation is recorded
as a follow-up PR amending this audit's "Result" section. No code change
ships before the numbers are committed.

## Out of scope

- `IORING_SETUP_DEFER_TASKRUN` / `IORING_SETUP_SINGLE_ISSUER` (Linux 6.1+).
  Worth a separate audit; combining them with SQPOLL changes the kernel
  scheduling model and would muddy this comparison.
- `SEND_ZC` vs. `SEND` for the socket writer - already gated by
  `IoUringConfig::zero_copy_policy` and tracked elsewhere.
- Multi-shot operations (`IORING_RECV_MULTISHOT`, ring-mapped buffers).
  These reduce SQE count, which is orthogonal to how SQEs are submitted.
- Windows IOCP - this audit is Linux-only by definition.
