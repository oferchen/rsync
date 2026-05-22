# SQPOLL + mmap race: symptoms and kernel-version coverage (SQM-1.b)

Tracking issue: oc-rsync task #2626 (SQM-1) plus SQM-1.a (reproducer) and
SQM-1.b (this document).

Companion artefacts already in tree:

- `docs/audits/io_uring_sqpoll_mmap_pagefault.md` - long-form failure-mode
  audit covering the three kernel-side outcomes (synchronous fault, `io-wq`
  punt, `-EFAULT`).
- `docs/audits/mmap-page-fault-iouring-sqpoll.md` - companion three-mode
  summary written first.
- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` - resolution-strategy
  doc enumerating Options 1, 2, 3.
- `docs/design/mmap-vs-sqpoll-decision.md` - the decision framework that
  picked SMR Option 3.
- `crates/fast_io/src/io_uring/config.rs:336-373` - the defensive disable
  currently in production: `build_ring()` falls back to a non-SQPOLL ring
  whenever `mmap_basis_active == true`.
- `crates/fast_io/tests/repro_sqpoll_mmap.rs` - the SQM-1.a reproducer
  documented below.

## 1. What the race is

io_uring's `IORING_SETUP_SQPOLL` mode starts a dedicated kernel thread
(`io_sq_thread()` in `io_uring/sqpoll.c`; on kernels < 5.10 the same
function lived in `fs/io_uring.c`) that polls the submission queue head and
issues SQEs without the userspace task entering the kernel. The kthread
borrows the submitter's `mm` via `kthread_use_mm()` so that user-virtual
pointers in an SQE resolve through the right page tables. What it does not
inherit is the userspace task's fault-handling stack, signal-delivery
context, or scheduler class. That asymmetry is the hazard.

When the SQPOLL kthread services an SQE that references a non-resident
mmap'd user page, three outcomes are possible depending on kernel version
and opcode:

1. **Synchronous fault inside the kthread.** `io_issue_sqe()` derefs the
   user pointer, hits a missing PTE, and `handle_mm_fault()` (in
   `mm/memory.c`) runs inline on the kthread. For an anonymous or
   already-resident file page this completes in microseconds. For a cold
   file page the fault blocks on filesystem I/O. While the kthread is
   blocked the SQ stops draining; every ring that shares this kthread
   (post-5.11 with `wq_fd`) stalls. The latency win from SQPOLL is
   converted into latency strictly worse than a regular ring.

2. **Punt to `io-wq` task-work.** When the kernel detects the op cannot
   complete inline it queues the request to an `io-wq` worker that re-runs
   the op with the submitter's `mm` and takes the fault on its own stack.
   The request now costs one SQ-poll wakeup, one work-queue dispatch, and
   one CQE. Throughput becomes equal to (or worse than) a regular ring.

3. **Short read or `-EFAULT`.** On older kernels (pre-5.12 for the
   `IORING_OP_READ`/`WRITE` family) and on opcodes that do not punt, the
   kthread fails the op outright. CQE returns a short result or `-EFAULT`.
   Writers retry on short, but `-EFAULT` is fatal: it surfaces as
   `io::Error` from `submit_and_wait` and the transfer aborts. The
   truncate variant of this is the `SIGBUS`-on-mid-transfer-truncate case
   upstream rsync sidesteps by never `mmap(2)`ing basis files
   (`fileio.c:214-217`); under SQPOLL the fault is delivered in kernel
   context, so the userspace `SIGBUS` handler upstream relies on cannot
   run.

The `IORING_REGISTER_BUFFERS` path adds a fourth surface: registration
synchronously calls `get_user_pages_fast()` and pins each page. If the
registered iovec is backed by an mmap, registration either eats the
fault cost up front or returns `-EFAULT` for a hole; subsequent SQEs that
name the registered slot are immune to faults because the pages are pinned
until `unregister_buffers`.

### Primary kernel-source citations

- `io_uring/sqpoll.c::io_sq_thread()` - the polling loop. On kernels < 5.10
  the same function lives in `fs/io_uring.c`. Upstream:
  <https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/io_uring/sqpoll.c>.
- `io_uring/io_uring.c::io_issue_sqe()` - SQE dispatch entry that derefs
  user pointers.
- `mm/memory.c::handle_mm_fault()` - page-fault entry implicitly invoked
  when the kthread derefs a non-resident page.
- `kernel/kthread.c::kthread_use_mm()` - the `mm`-borrow primitive that
  makes user-virtual pointers in SQEs resolve correctly under SQPOLL.

### Secondary references

- LWN: "The rapid growth of io_uring" - <https://lwn.net/Articles/810414/>
  (background on SQPOLL design).
- LWN: "Ringing in a new asynchronous I/O API" -
  <https://lwn.net/Articles/776703/> (the original io_uring announcement
  and `IORING_SETUP_SQPOLL` rationale).
- man pages: `io_uring_setup(2)` (`IORING_SETUP_SQPOLL`),
  `io_uring_register(2)` (`IORING_REGISTER_BUFFERS`),
  `madvise(2)` (`MADV_WILLNEED`, `MADV_NOHUGEPAGE`),
  `mmap(2)` (`MAP_POPULATE`).

## 2. Reproducer how-to

The reproducer lives at `crates/fast_io/tests/repro_sqpoll_mmap.rs`. It
runs `ITERATIONS = 16` independent cycles, each one building a fresh ring,
registering a 256 MiB mmap'd buffer, and submitting a single 4 KiB
`READ_FIXED` against a separate source file. Every iteration has a strict
5-second `submit_with_args` timeout so the reproducer cannot hang.

The test is `#[ignore]` by default because SQPOLL requires `CAP_SYS_NICE`
on most kernels. To run on an instrumented kernel:

```sh
cargo nextest run -p fast_io --features io_uring \
    -E 'test(repro_sqpoll_mmap)' --ignored --no-capture
```

### Per-iteration status line format

Each iteration prints exactly one line:

```text
repro_sqpoll_mmap iter=NN status=<outcome>
```

`<outcome>` is one of:

| Status | Meaning |
|---|---|
| `ok bytes=N elapsed=...` | Iteration completed cleanly; CQE returned `READ_LEN` bytes. Race did not trip. |
| `short bytes=N` | CQE returned `0 <= N < READ_LEN`. Maps to failure mode 3 (short read). |
| `efault` | CQE returned `-EFAULT`. Maps to failure mode 3 (in-kernel fault refused). |
| `eagain` | CQE returned `-EAGAIN`. Kernel asked for resubmission; recoverable but indicates the kthread could not complete inline. |
| `errno=N` | CQE returned some other negative errno. Capture and triage. |
| `timeout` | `submit_with_args` returned `ETIME` or the per-iteration budget elapsed without a CQE. Maps to failure mode 1 (kthread stalled). |
| `sqpoll-unavailable` | `IoUring::builder().setup_sqpoll(...).build()` failed (typically `EPERM`). Reproducer aborts the remaining iterations. |
| `register-failed: <err>` | `register_buffers` returned an error. Maps to the registration-path variant of failure mode 3. |
| `submit-failed: <err>` | Setup of the read SQE failed before submission. Indicates an environment problem, not a race. |

The reproducer concludes with a one-line summary tallying every status
across iterations.

### Success vs failure

- **Pass condition (race did NOT trip):** every iteration reports
  `status=ok`. This is the expected outcome on a kernel that handles
  SQPOLL + registered mmap'd buffers correctly.
- **Fail condition (race tripped):** any iteration reports `short`,
  `efault`, `timeout`, `eagain`, or `errno=N`. Record the kernel version,
  the failure-mode mapping, and the iteration count in the coverage matrix
  below.

A run that reports `sqpoll-unavailable` is inconclusive - the kernel did
not grant SQPOLL, so the race surface was never reached. Re-run with
`CAP_SYS_NICE` (typically via `sudo setcap cap_sys_nice=eip <bin>` on the
test binary, or by running the test under `sudo`).

## 3. Kernel-version coverage matrix

Placeholder rows. Each cell is filled in when SQM-1.a runs on a fresh
host. The kernel matrix matches the LTS line that production rsync
deployments target plus the latest mainline.

| Kernel | Major.minor | Iterations | ok | short | efault | timeout | eagain | other_errno | Notes |
|---|---|---|---|---|---|---|---|---|---|
| Linux 5.10 LTS | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD |
| Linux 5.15 LTS | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD |
| Linux 6.1 LTS  | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD |
| Linux 6.6 LTS  | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD |
| Linux 6.12     | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD | TBD |

How to populate a row:

1. Boot a target kernel (KVM, hosted VPS, or bench hardware).
2. Run the reproducer with `--ignored`. Capture stdout.
3. Read the tally line and the per-iteration status lines.
4. Replace the corresponding `TBD` cells. Cite the exact kernel build
   (`uname -r`) in the `Notes` column.

## 4. Failure modes seen

Placeholder. SQM-1.b will be amended once the matrix above is populated.
Categories to anticipate:

- **Data corruption.** A non-`-EFAULT` short CQE that returns plausible-
  looking bytes which do not match the source file. The reproducer does
  not verify the bytes today; SQM-1.b round 2 will extend it to assert
  byte equality and record this mode here.
- **Hang.** A `submit_with_args` that exceeds the 5-second per-iteration
  budget. Reproducer maps to `status=timeout` and continues.
- **Kernel oops.** A panic in `dmesg` correlated with the iteration. Not
  observable from the reproducer; capture out-of-band via `dmesg -w`.
- **`-EFAULT` storm.** Every iteration returns `efault`. Strongest signal
  that the registered-buffer + SQPOLL combination is structurally broken
  on the target kernel.
- **Throughput regression without errors.** Every iteration reports `ok`
  but `elapsed` is much higher than a sibling non-SQPOLL run; indicates
  failure mode 2 (silent `io-wq` punt). Not a correctness failure but
  motivates promoting the defensive disable into a permanent policy on
  the affected kernels.

## 5. Why our current workaround works

Production today disables SQPOLL whenever an mmap'd basis reader is
active. The dispatch lives at
`crates/fast_io/src/io_uring/config.rs:336-373`:

```text
if sqpoll_requested && !sqpoll_safe {
    // refuse SQPOLL; fall back to a regular ring
    SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
}
```

`sqpoll_safe = sqpoll_requested && !self.mmap_basis_active`. The flag is
set by callers that own an `MmapReader` over the basis file. When the
flag trips, `build_ring()` builds a regular (non-SQPOLL) ring and records
the fallback so `--version` / diagnostics can surface it.

This is the SMR Option 3 conservative path picked by tasks #2287..#2292
and documented in `docs/design/mmap-vs-sqpoll-conflict-resolution.md`
"Implementation plan (option 3)". The fallback path costs ~10-15% on NVMe
+ large mmap'd basis workloads (per the SMR-1 bench cells in
`crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`) but trades that for
correctness: every failure mode above is sidestepped because the SQPOLL
kthread never enters the picture.

The disable is intentionally crude. SQM-2 will use the SQM-1.a reproducer
data to design a finer-grained workaround (candidates: `MADV_WILLNEED`
prefault window, `mlock` the basis region for the registered-buffer
window, or per-basis dispatch that keeps SQPOLL on for non-mmap'd basis
paths). SQM-1.c will spec those three candidates side-by-side once this
document's kernel-version matrix is populated.
