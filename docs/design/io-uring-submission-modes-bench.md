# io_uring Submission Modes Benchmark Plan

Issue: #1626. Goal: choose a default io_uring submission policy in the `fast_io`
crate by measuring regular submission, SQPOLL, and DEFER_TASKRUN against the
existing standard-I/O baseline. Output drives a `SubmissionMode` selector with
documented kernel/CAP requirements.

## 1. Submission Modes

| Mode             | Mechanic                                                                    | Cost                                | Requirement                                |
|------------------|-----------------------------------------------------------------------------|-------------------------------------|--------------------------------------------|
| Regular          | One `io_uring_enter(2)` per batch of SQEs.                                  | One syscall per submit; lowest tail.| Linux 5.6+.                                |
| SQPOLL           | Kernel polling thread drains the SQ ring; userspace just writes the tail.   | Pinned kernel core; idle wakeups.   | Linux 5.13+ with `CAP_SYS_NICE` (or 5.11+ unprivileged with limits). |
| DEFER_TASKRUN    | Completions handled in the submitting task's context, no async worker hop. | Lower scheduling jitter on submit.  | Linux 5.19+, `IORING_SETUP_SINGLE_ISSUER`. |
| Standard I/O     | `pread`/`pwrite` baseline for comparison.                                   | One syscall per op.                 | Always available.                          |

`SQPOLL` requires `CAP_SYS_NICE` to set the polling thread's idle period above
default; without it, the thread sleeps after `sq_thread_idle` (default 1 ms).
`DEFER_TASKRUN` mandates `SINGLE_ISSUER`; the bench harness keeps one submitter
thread per ring to satisfy this.

## 2. Bench Plan

- Workload: 64 KiB random reads, queue depth 256 outstanding ops, fixed buffers
  registered up-front (`IORING_REGISTER_BUFFERS`).
- File set: 4 GiB random data, pre-faulted to avoid first-touch noise.
- Storage tiers:
  - `tmpfs` (`/dev/shm`) - upper bound, removes block-layer variance.
  - Real NVMe SSD on ext4 (`noatime`, default queue depth) - production proxy.
- Harness: extend `crates/fast_io/benches/` with a `submission_modes` Criterion
  group. Each mode runs as a dedicated benchmark function; `SubmissionMode`
  enum is plumbed through the existing ring constructor.
- Repetitions: 5 warm-up iterations, 30 measured. Cache dropped via
  `echo 3 > /proc/sys/vm/drop_caches` between SSD runs.
- Pinning: `taskset -c 2,3` for submitter; SQPOLL thread inherits sibling core
  via `IORING_SETUP_SQ_AFF`.

## 3. Workload Variants

- Sustained: continuous submission for 30 s; measures peak throughput when the
  SQPOLL kernel thread stays hot and never sleeps.
- Bursty: 4 ms work bursts separated by 8 ms idle gaps for 30 s; forces SQPOLL
  to sleep and incur `io_uring_enter(IORING_ENTER_SQ_WAKEUP)` wakeups, which is
  where regular submission and DEFER_TASKRUN should win.
- Mixed-size sanity: re-run sustained with 4 KiB and 1 MiB reads to confirm
  the recommendation does not invert at the size boundaries.

## 4. Metrics

| Metric           | Source                                               | Reported as              |
|------------------|------------------------------------------------------|--------------------------|
| ops/sec          | Criterion throughput.                                | Mean +/- stddev.         |
| syscalls/sec     | `perf stat -e raw_syscalls:sys_enter`.               | Per-second rate.         |
| Kernel CPU %     | `pidstat -u 1` plus `mpstat -P ALL` for SQPOLL core. | User vs system split.    |
| p50/p99 latency  | Per-op timestamps via `clock_gettime(MONOTONIC)`.    | Microseconds.            |
| Wakeups          | `perf stat -e sched:sched_wakeup` on the ring fd.    | Count per 30 s run.      |

p99 latency is the tie-breaker. Anything within 3 % on throughput defers to the
mode with the lowest p99 and lowest kernel CPU.

## 5. Decision Matrix

Default selection logic for `SubmissionMode::auto()`:

1. Kernel < 5.6 or io_uring disabled at runtime -> standard I/O.
2. Kernel 5.6 - 5.18 -> regular submission (no DEFER_TASKRUN available).
3. Kernel >= 5.19, single submitter thread, sustained workload -> DEFER_TASKRUN.
4. Process holds `CAP_SYS_NICE`, sustained workload, dedicated core available
   -> SQPOLL with `sq_thread_idle = 200` ms.
5. Bursty workload (transfer pipeline with idle gaps) -> regular submission;
   SQPOLL idle-thread cost outweighs its submit savings.
6. Standard I/O wins on small file sets where setup cost dominates (< 1 MiB
   total bytes per op stream); dispatch keeps the existing `Auto` fallback.

The benchmark must publish a CSV under `target/bench/io-uring-modes/` so the
selector thresholds (kernel version, queue depth, idle interval) can be tuned
with data, not guesses. Re-run on every kernel bump in the CI matrix.

## References

- Upstream io_uring docs: `Documentation/io_uring/` in the Linux tree.
- `IORING_SETUP_SQPOLL`, `IORING_SETUP_DEFER_TASKRUN`, `IORING_SETUP_SINGLE_ISSUER`
  in `include/uapi/linux/io_uring.h`.
- Existing fast_io probe: `crates/fast_io/src/io_uring/probe.rs`.
