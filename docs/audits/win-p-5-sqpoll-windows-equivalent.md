# WIN-P.5: SQPOLL Windows equivalent evaluation

Status as of 2026-06-11. Per-stub audit for the WIN-P series, asking
whether SQPOLL has a Windows equivalent worth shipping or whether the
IOCP path that oc-rsync already wires is the structural answer.

This audit takes the following as input and does not re-derive them:

- `docs/audits/win-p-1-fast-io-stubs.md` (WIN-P.1, 2026-06-11)
  classified `sqpoll_basis::WiredBasisWindow` as a **Class-D no-op
  success** on Windows. The Linux-side concern - SQPOLL kernel-thread
  page-faulting on the mmap basis - does not exist on Windows because
  there is no SQPOLL thread to race with the userspace mmap window.
- `docs/design/sqm-series-closeout.md` (SQM-4.b) closes the SQPOLL +
  mmap race with a defensive disable when an mmap basis is in flight.
  The cost is an estimated 10-15% NVMe throughput tax on affected
  workloads; everywhere else SQPOLL is engaged unconditionally.
- `docs/design/wpg-7-iouring-opcode-inventory.md`,
  `docs/design/wpg-7b-iouring-iocp-mapping.md`, and
  `docs/design/wpg-7c-iocp-gap-list.md` mapped every io_uring opcode
  oc-rsync emits to either an IOCP equivalent or a documented gap.
  WPG-7.c lists four confirmed gaps; SQPOLL is **not one of them** -
  it is a submission-model attribute, not an opcode.
- `docs/audits/iocp-sync-blocking-audit.md` (WPG-5, PR #2304)
  catalogued every synchronous wait in `crates/fast_io/src/iocp/`. The
  fast path drains completions via `GetQueuedCompletionStatusEx` in
  batched mode and the pump worker thread runs off the submitter
  thread; both are SQPOLL-equivalent in the structural sense (kernel
  amortisation of completion delivery without a per-op syscall).
- `docs/design/wpg-9-registered-buffer-windows-equivalent.md` (WPG-9)
  designed the RIO_BUF path for socket-side zero-copy. It also
  documents that RIO completions live on a separate `RIO_CQ` queue
  drained via `RIODequeueCompletion`, which is the closest analogue
  to "no syscall per completion" that Windows offers.

## 0. Pre-flight git-history check

The task description requires verifying nothing along these lines has
already shipped. Running:

```
git log --grep="WIN-P.5\|SQPOLL.*windows\|RIO.*completion" \
  --oneline --since="2026-05-15"
```

returned no matches on master. Nothing under WIN-P.5 has been merged.

## 1. Linux SQPOLL semantic recap

io_uring's `IORING_SETUP_SQPOLL` mode dedicates a kernel polling
thread to the ring's submission queue. The kernel thread spins on the
SQ head (with a configurable idle timeout) and drains submitted SQEs
without the userspace submitter ever calling `io_uring_enter(2)`. The
submitter writes the SQE, bumps the tail pointer, and returns; the
kernel thread services it without a syscall round-trip.

Key properties of SQPOLL:

| Property | Behaviour |
|---|---|
| Submission cost | Zero syscalls under steady-state (kernel thread spins on SQ head) |
| Kernel thread dedication | One dedicated `io_uring-sq` kernel thread per ring (or shared via `IORING_SETUP_ATTACH_WQ`) |
| Capability requirement | `CAP_SYS_NICE` on the calling process (SQP-1 inventory) |
| Idle timeout | Configurable via `sq_thread_idle`; kernel thread parks after timeout |
| Wakeup | `io_uring_enter(IORING_ENTER_SQ_WAKEUP)` when SQ thread is parked |
| Hazard | SQPOLL thread can page-fault on userspace memory (e.g. mmap basis); see SQM series |

The win is **per-SQE syscall amortisation**: when the submitter is
producing SQEs faster than the SQ thread can drain (the SQ thread is
warm), the per-SQE submission cost approaches zero.

## 2. oc-rsync's Linux SQPOLL engagement

Production code paths that engage SQPOLL in oc-rsync today
(cross-reference SQM series, SQP-LAND, IUR-3):

- **File-writer disk-commit ring** - the per-thread io_uring writers
  registered via `IUR-3.b` engage SQPOLL when the ring is built with
  `IoUringConfig::sqpoll = true`. This is the highest-IOPS path on
  the receive side (write-heavy hot path).
- **File-reader sender data path** - per `IUR-3.c`, the file-reader
  factory builds per-thread rings; SQPOLL applies there too.
- **Socket-writer egress ring** - per `IUR-3.d`, daemon push uses a
  per-thread socket-writer ring; SQPOLL applies.
- **Disk-commit shared ring** - per `IUR-3.f`, the one-shot probes
  and disk-commit ring stay shared. SQPOLL applies only when the
  shared ring is built with sqpoll enabled.

Where SQPOLL is **disabled** (SQM-3 + SQP-LAND.4):

- When the basis is mmap'd (SQM-3 defensive disable; mmap+SQPOLL
  page-fault race).
- When the process is running in a rootless container (SQP-LAND.4;
  `CAP_SYS_NICE` is not held).
- When `--no-io-uring-sqpoll` is passed (CLI opt-out per
  `set_sqpoll_disabled_by_policy()`).

The win is largest on **NVMe + non-mmap delta-apply** workloads where
the writer ring sustains high-IOPS submissions and the kernel thread
saves a per-SQE syscall every time.

## 3. Windows candidate APIs

The structural question for WIN-P.5 is: does Windows have a
submission/completion model where the kernel amortises per-operation
syscalls in the same way SQPOLL does for io_uring? Four candidates.

| API | Syscall amortisation | Scope | Kernel-thread dedication | Production-wired in oc-rsync? |
|---|---|---|---|---|
| **`GetQueuedCompletionStatusEx` (GQCS-Ex)** | Batched dequeue: one syscall returns up to N completions. **No SQPOLL-equivalent submission amortisation** - each `WriteFile` / `WSASend` issues a syscall. | File + socket + any HANDLE bound to an IOCP. | None. The caller's worker thread blocks in the kernel waiting for completions. | **Yes**. `iocp/disk_batch.rs:638` and `iocp/pump.rs:360` drain via GQCS-Ex with batch sizes auto-tuned by WPG-3 (#2302). |
| **TP_IO threadpool I/O (`CreateThreadpoolIo` + `StartThreadpoolIo`)** | Same completion-side amortisation as GQCS-Ex (the thread pool internally drains an IOCP). Submission side is still per-op `ReadFile` / `WriteFile`. | File + socket. | **Yes - kernel-managed worker pool**. The system thread pool spawns and parks worker threads sized to the load. | **No**. The IOCP pump in `iocp/pump.rs` is a hand-rolled drain loop; it does not delegate to TP_IO. Worker thread count is fixed at port-creation time. |
| **`BindIoCompletionCallback`** | Same as TP_IO; predates `CreateThreadpoolIo`. Microsoft now recommends TP_IO. | File + socket. | Kernel-managed callback dispatch on the default system thread pool. | **No**. Deprecated for new code in favour of TP_IO. |
| **Winsock Registered I/O (RIO) - `RIONotify` / `RIODequeueCompletion`** | **Closest SQPOLL analogue**. The RIO completion queue (`RIO_CQ`) is a userspace memory-mapped structure; `RIODequeueCompletion` is a userspace function call - no syscall. Submission via `RIOSend` is also lock-free userspace (the request queue is shared with the kernel). | **Sockets only**. No file equivalent. | The kernel still services the request queue; there is no dedicated polling thread, but the userspace-to-kernel notification is via a shared memory mapping rather than per-call syscalls. | **No**. WPG-9 designed the integration; only the RIO_BUF zero-copy buffer registration shipped, not the RIO_CQ completion drain. |

Notes per row:

- **GQCS-Ex** is the workhorse. oc-rsync's IOCP path already engages
  it for both file and socket completion drain. It amortises **dequeue
  cost**, not submission cost: it does not eliminate the `WriteFile`
  syscall, but it does eliminate the per-completion `GetQueuedCompletionStatus`
  syscall round-trip. WPG-5's audit (`iocp-sync-blocking-audit.md`)
  confirms the pump thread already does this and that the recommended
  per-IO mitigation (M1) is to widen in-flight depth so GQCS-Ex's
  batched drain pays off per batch rather than per IO.
- **TP_IO** is what most modern Windows IOCP servers use. It moves
  thread management to the kernel, which is closer to SQPOLL's
  "kernel manages the polling thread" model. Whether oc-rsync **should**
  migrate is a separate question (tracked under DASYNC / ASY-G);
  the answer for SQPOLL-equivalence is **the hand-rolled pump
  already covers the same throughput envelope** because the IOCP
  port itself does the wait coalescing; TP_IO would mainly help
  thread-count sizing on long-lived daemons, not per-IO syscall cost.
- **`BindIoCompletionCallback`** is deprecated; skip.
- **RIO** is the only Windows API that genuinely eliminates the
  per-operation syscall, and only on the socket side. The kernel and
  userspace share the request and completion queues via memory
  mappings; submission is `RIOSend` (userspace function), drain is
  `RIODequeueCompletion` (userspace function). The structural shape
  is **almost identical to io_uring without SQPOLL**: shared queues
  in mapped memory, no per-op syscall. **There is no RIO equivalent
  for files** - `RIOSend` / `RIOReceive` only operate on sockets
  created with `WSA_FLAG_REGISTERED_IO`.

## 4. Verdict - permanent gap

**SQPOLL has no separate concept to port to Windows.** The structural
syscall amortisation SQPOLL provides on Linux is already covered on
Windows by a combination of IOCP's batched-dequeue model and (for
sockets only) Registered I/O. Three points of evidence:

1. **Completion-side amortisation is already shipped.** WPG-3 auto-
   sizes the GQCS-Ex batch depth; WPG-5's audit confirms the pump
   thread already drains via the batched API. No SQPOLL equivalent
   would lift this further; the work-item is widening in-flight depth
   per WPG-5's M1/M3 mitigations, which is a separate audit
   (#1928, #1929, #1930) and which doesn't intersect SQPOLL.
2. **Submission-side amortisation has no IOCP analogue for files.**
   Every Windows file I/O dispatch goes through `NtWriteFile` /
   `NtReadFile` and pays one syscall per overlapped op. RIO covers
   sockets but not files. SQPOLL's "kernel polls SQ head, submitter
   never enters kernel" model is **structurally incompatible** with
   the NT I/O Manager's design - I/O packets (IRPs) are kernel
   objects allocated and queued by `NtWriteFile`'s caller-side syscall.
   There is no userspace-to-kernel shared submission queue for file
   I/O on Windows. Implementing one would require an out-of-tree
   kernel driver; not a userspace-shippable equivalent.
3. **The Class-D no-op stub is correct precisely because the
   race does not exist.** `sqpoll_basis::WiredBasisWindow` on Windows
   returns `Ok(Self { len })` with a null pointer because there is no
   SQPOLL kernel thread to race against the mmap basis window. WIN-P.1
   row 1.2 documents this; no Windows caller dereferences the
   returned pointer, and no Windows code path would benefit from
   pinning pages against a thread that does not exist.

**Conclusion:** WIN-P.5 ships **no Windows implementation**. WIN-P.10
(SQPOLL implementation task) should close as "permanent-gap-by-design"
with a pointer to:

- WIN-P.1 §1.2, §2 row `sqpoll_basis::WiredBasisWindow` (Class-D
  documentation).
- WPG-7.c §3 (gap-list does not include SQPOLL because it is a
  submission-model attribute, not an opcode).
- WPG-5 audit (existing GQCS-Ex batched-drain pump).
- WPG-9 design (RIO_BUF zero-copy on sockets; RIO_CQ drain not
  pursued because GQCS-Ex covers it for production traffic).
- IUM-1..4 (io_uring benefit model; the predicted SQPOLL win is
  Linux-NVMe-specific and has no Windows equivalent because the
  Windows file-I/O model does not have a syscall-free submission
  path).

## 5. WIN-TIER2 classification update

WIN-TIER2.1 (`docs/audits/win-tier2-stub-inventory.md`) does not list
`sqpoll_basis::WiredBasisWindow` because the stub is unreachable on
the Windows transfer path. WIN-P.1's add-on inventory does. Both are
correct: WIN-TIER2's lens is "what does the Windows transfer execute
today vs Cygwin rsync" (unreachable stubs are not in scope), and
WIN-P.1's lens is "what Linux-only entry points exist with a non-Linux
arm" (all of them, including unreachable ones).

For WIN-P.6's per-stub decision matrix, classify SQPOLL as:

| Field | Value |
|---|---|
| Linux entry point | `crates/fast_io/src/io_uring/config.rs::build_ring` (`IORING_SETUP_SQPOLL` flag) plus `sqpoll_basis::WiredBasisWindow` (mmap-window guard) |
| Non-Linux arm | `sqpoll_basis.rs:273` no-op success; `build_ring` is not compiled on Windows because io_uring is Linux-only |
| Class (WIN-P.1) | **D** (no-op success - correct because Linux concern is absent) |
| Windows production reach | Zero. Windows transfer never enters the `sqpoll_basis` module; IOCP path is the structural replacement. |
| Decision | **Permanent gap.** No Windows equivalent worth shipping. |
| Rationale | (1) IOCP completion-side amortisation is shipped via WPG-3 + WPG-5. (2) No Windows API provides a syscall-free file submission path; RIO covers only sockets. (3) The SQPOLL+mmap race the Linux wrapper guards against does not exist on Windows. |
| Cross-reference | WIN-P.1 §1.2 row 1.2; WPG-7.c §3 (SQPOLL absent from gap list); WPG-5 (existing GQCS-Ex drain); WPG-9 (RIO_BUF socket scope). |
| Feed-forward | WIN-P.10 close as no-implementation. |

## 6. Feed-forward to WIN-P.6 and WIN-P.10

### WIN-P.6 decision matrix entry

The per-stub decision matrix should include this row:

```
| Stub             | Class | Verdict        | Owner | Cross-reference                              |
|------------------|-------|----------------|-------|----------------------------------------------|
| sqpoll_basis     | D     | Permanent gap  | WIN-P | This audit; WPG-5; WPG-9; WPG-7.c            |
```

### WIN-P.10 closure

`WIN-P.10 (#3691)` should close with no Windows implementation.
Resolution annotation:

```
Closed as permanent-gap-by-design per WIN-P.5
(docs/audits/win-p-5-sqpoll-windows-equivalent.md §4).

The SQPOLL kernel-polling-thread model has no userspace-shippable
equivalent on Windows for file I/O. IOCP + GetQueuedCompletionStatusEx
already covers completion-side syscall amortisation; the
SQPOLL+mmap-basis race that motivates sqpoll_basis::WiredBasisWindow
does not exist on Windows.

No regression test needed; the Class-D no-op stub is correct as-is.
```

## 7. Cross-reference index

- WIN-P (parent) - `#3681`.
- WIN-P.1 - `docs/audits/win-p-1-fast-io-stubs.md` (2026-06-11,
  PR #5643). §1.2 row 1.2 first identified `sqpoll_basis` as
  Class-D.
- WIN-P.5 - this audit (`#3686`).
- WIN-P.6 - per-stub decision matrix (`#3687`); pending.
- WIN-P.10 - SQPOLL implementation task (`#3691`); will close as
  permanent gap.
- WPG-3 - auto-size IOCP CQ depth (`#2302`); shipped.
- WPG-5 - IOCP synchronous blocking audit (`#2304`); shipped via
  `docs/audits/iocp-sync-blocking-audit.md`.
- WPG-7.a/b/c - io_uring -> IOCP opcode mapping series; shipped.
- WPG-9 - registered-buffer Windows equivalent (`#2669`); RIO_BUF
  socket-side shipped.
- SQM series - SQPOLL + mmap basis race; closed via SQM-4.b
  (`docs/design/sqm-series-closeout.md`).
- SQP series - SQPOLL `CAP_SYS_NICE` rootless container failure
  (`#3289`); closed.
- SQP-LAND series - rootless container detection shipped into
  io_uring config.
- IUM-1..4 - io_uring benefit model; predicted SQPOLL win is
  Linux-NVMe-specific.
