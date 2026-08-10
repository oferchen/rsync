# WPG-10 - IOCP linked-timeout shim

Audit + design-only doc for emulating `IORING_OP_LINK_TIMEOUT` on the
Windows IOCP backend. This is the design WPG-10's implementation
follow-ups (WPG-10.a / .b / .c) work from. No source changes are made
by this task.

Inputs:

- WPG-7.a opcode inventory: `docs/design/wpg-7-iouring-opcode-inventory.md`.
  `IORING_OP_LINK_TIMEOUT` is one of the 23 opcodes in use; default-on,
  paired with every batched `SEND` poll gate to bound a back-pressured
  socket (WPG-7.a lines 50, 93). Linux min kernel 5.5.
- WPG-7.b IOCP mapping: `docs/design/wpg-7b-iouring-iocp-mapping.md`.
  `LINK_TIMEOUT` is classified as a P1 control-path gap with the
  recommended Win32 synthesis being
  `CreateWaitableTimerExW` + `SetThreadpoolWait` + `CancelIoEx`
  against the in-flight overlapped (WPG-7.b line 57).
- WPG-7.c gap list: `docs/design/wpg-7c-iocp-gap-list.md`. Gap #2 in
  the prioritised table; **P1** severity; mechanical workaround sized
  **M**; ranked third in the WPG sprint order behind WPG-9 (data
  path) and WPG-8 (zero-copy send) (WPG-7.c lines 35, 49-53, 65-71,
  96-98).
- Linux call site to mirror: `crates/fast_io/src/io_uring/batching.rs`
  `poll_writable`, lines 173-249. The single SQE chain that pairs
  `PollAdd(POLLOUT)` with `LinkTimeout(timeout)` and surfaces
  `WouldBlock` when the timeout fires.
- IOCP integration surface today:
  - `crates/fast_io/src/iocp/socket.rs` -
    `IocpSocketReader::recv_async` (`WSARecv`, lines 150-210),
    `IocpSocketWriter::send_async` (`WSASend`, lines 284-335).
    No per-op deadline today.
  - `crates/fast_io/src/iocp/disk_batch/mod.rs` - batched overlapped
    `WriteFile`s through `flush_current`, no per-op deadline today.
  - `crates/fast_io/src/iocp/file_reader.rs` /
    `crates/fast_io/src/iocp/file_writer.rs` - overlapped `ReadFile`
    / `WriteFile`, no per-op deadline today.
- Windows API surface already in the `windows-sys` feature set
  pulled by `crates/fast_io/Cargo.toml` (`Win32_System_Threading`,
  `Win32_System_IO`, `Win32_Foundation`). All three required calls
  - `CreateWaitableTimerExW`, `SetThreadpoolWait`, `CancelIoEx` -
  are reachable without expanding `Cargo.toml`.

## 1. What `LINK_TIMEOUT` does on Linux

`IORING_OP_LINK_TIMEOUT` is the atomic kernel-side linkage between a
target SQE (`READ` / `WRITE` / `SEND` / `RECV` / `POLL_ADD`) and a
deadline:

1. Caller submits the target SQE with `IOSQE_IO_LINK` and immediately
   pushes a sibling `LinkTimeout` SQE pointing at the same
   `submission()` slot. The kernel binds the two before either runs.
2. The target op runs. Two outcomes are possible:
   - **Target completes first** (success or error). The kernel
     cancels the `LinkTimeout` SQE with `-ECANCELED` and posts the
     target's natural CQE (positive byte count, or negative errno).
   - **Timer fires first.** The kernel cancels the target with
     `-ECANCELED`, posts the target CQE with that error code, and
     posts the timer CQE with its natural `-ETIME` payload.
3. Both SQEs always produce exactly one CQE. The caller drains both
   in the same `ring.completion()` walk and disambiguates by
   `user_data` (in `batching.rs:217-229`, the constants
   `POLL_OUT_USER_DATA` and `POLL_OUT_TIMEOUT_USER_DATA`).
4. The chain is single-submission. The cost of arming the deadline
   is one extra SQE push - no syscall overhead on top of the target
   submission.

The chain is per-target. There is no fan-out; one `LinkTimeout`
binds to one preceding SQE. The lifetime of the `Timespec` borrowed
to `LinkTimeout::new` must outlive the kernel reference, which the
`batching.rs:204-210` SAFETY note documents: the spec is borrowed
for the duration of the call and the chain is drained synchronously
before the stack frame returns.

## 2. The IOCP gap

IOCP has no SQE-chain primitive. Each `WSARecv` / `WSASend` /
`ReadFile` / `WriteFile` stands alone: there is no kernel-side way
to link a deadline to the operation such that the timer fires
atomically inside the I/O subsystem.

Closest building blocks exist as three independent Win32 primitives:

- `CreateWaitableTimerExW` (XP+) creates a kernel timer object. The
  `EX` variant lets us pass `CREATE_WAITABLE_TIMER_MANUAL_RESET` so
  we can probe and re-arm without recreating the handle.
- `SetWaitableTimer` arms the timer with a `LARGE_INTEGER` due
  time (negative = relative, positive = absolute FILETIME).
- `SetThreadpoolWait` (Vista+) registers a `PTP_WAIT` callback that
  runs on the system thread-pool when the timer signals.
  `CreateThreadpoolWait` / `CloseThreadpoolWait` manage the
  callback handle's lifetime.
- `CancelIoEx(handle, lpOverlapped)` (Vista+) cancels exactly the
  in-flight overlapped that matches the pointer. Returns
  `ERROR_NOT_FOUND` if the op has already completed and the
  completion has been dequeued.

The shim has to compose these three so that the resulting behaviour
matches the io_uring CQE shape from the caller's perspective:
either the natural completion (success or kernel error) arrives, or
a single deadline-driven cancellation arrives, and never both.

## 3. Proposed shim design

### 3.1 Lifecycle

For each user-facing "submit op with deadline T" call:

1. **Allocate the timer and the wait.** `CreateWaitableTimerExW(NULL,
   NULL, CREATE_WAITABLE_TIMER_MANUAL_RESET, TIMER_ALL_ACCESS)`
   returns a `HANDLE`. `CreateThreadpoolWait(callback, ctx, NULL)`
   returns a `PTP_WAIT`. Both go into a `LinkedTimer` struct that
   the caller holds for the lifetime of the op.
2. **Arm the timer.** `SetWaitableTimer(timer, &due_time, 0, NULL,
   NULL, FALSE)` with `due_time` = `-(T_100ns)` for a relative
   deadline (LARGE_INTEGER convention: negative = relative,
   100-nanosecond units).
3. **Register the wait.** `SetThreadpoolWait(wait_obj, timer, NULL)`
   binds the wait to the timer. When the timer signals, the
   thread-pool calls the registered callback exactly once (because
   `MANUAL_RESET` plus our explicit cancel in step 5 prevents
   re-fire).
4. **Submit the actual IOCP op as usual.** Nothing changes about the
   `WSARecv` / `WSASend` / `ReadFile` / `WriteFile` call shape.
   The op carries its own `OVERLAPPED*`; the `LinkedTimer` borrows
   that same pointer through its callback context so it can call
   `CancelIoEx(handle, op_overlapped)` on timer fire.
5. **On op completion** (natural success, natural error, or our
   cancel firing), call `SetThreadpoolWait(wait_obj, NULL, NULL)`
   to detach the wait before the timer is closed. Then
   `CloseHandle(timer)` and `CloseThreadpoolWait(wait_obj)`. The
   ordering matters: `SetThreadpoolWait(NULL, ...)` synchronises
   with the threadpool to guarantee no callback is in flight when
   the next step runs.

### 3.2 Threadpool callback

The callback is the minimal possible:

```text
on timer signal (TP_CALLBACK_INSTANCE *, ctx *, TP_WAIT *, TP_WAIT_RESULT):
    if !ctx.completed.swap(true, AcqRel):
        CancelIoEx(ctx.handle, ctx.overlapped)
        ignore ERROR_NOT_FOUND
```

The `swap` returns the prior value. If it was already `true` the op
completed naturally and we do nothing; the natural completion has
won. If it was `false` we are the first observer and we issue the
cancel. The pump (which serialises CQE delivery) will report the
cancelled completion through the same channel the natural
completion would have used.

### 3.3 Caller completion path

The caller's existing code paths already wait on either the pump's
oneshot channel (`socket.rs::await_completion`) or the
disk-batch completion handler. Both deliver a single `io::Result`
per submitted overlapped. The shim's caller does this once
completion arrives:

```text
match overlapped.completion {
    Ok(n)                                   => natural success
    Err(e) if e is ERROR_OPERATION_ABORTED  => deadline-driven cancel
    Err(e)                                  => natural error
}
```

Then drop the `LinkedTimer`. Drop runs the unregister + close
sequence from step 5. The drop also flips `completed` to `true` if
it was not already; that is the third path that closes a
not-yet-fired timer cleanly (race C below).

## 4. Race conditions

The shim has three documented races. Each is closed by the same
`AtomicBool::completed` primitive plus the threadpool's
`SetThreadpoolWait(NULL, ...)` synchronisation contract.

### Race A: timer fires while op completes naturally

Two threads race:

- The IOCP pump dequeues the natural completion and signals
  `await_completion`'s receiver.
- The timer fires and the threadpool dispatches our callback.

Both want to claim "the completion". The `AtomicBool` deduplicates:
whoever wins the `swap(true)` performs its side effect (cancel for
the timer, deliver result for the natural path). The loser sees
`prior == true` and returns immediately.

If the timer wins the swap and the natural completion is already
queued in the IOCP, the kernel's `CancelIoEx` returns
`ERROR_NOT_FOUND` (it cannot cancel what is no longer in flight).
We swallow that error - the natural completion is still being
delivered through the pump.

If the natural path wins the swap, the timer callback runs but does
nothing. The pump still delivers the natural completion. The drop
path will detach the wait under the
`SetThreadpoolWait(wait_obj, NULL, NULL)` synchronisation.

### Race B: CancelIoEx fires after op completes naturally

Sub-case of Race A. The timer callback called `CancelIoEx` before
the natural completion was dequeued from the IOCP. The kernel will
either:

- **Have already completed the op** - `CancelIoEx` returns
  `ERROR_NOT_FOUND`. The pump still delivers the natural completion
  bytes/status. The caller treats it as success.
- **Cancel before the op posted to the IOCP** - the pump delivers
  `ERROR_OPERATION_ABORTED`. The caller treats it as deadline.

Both behaviours are acceptable; the IOCP guarantees exactly one
completion per overlapped. The `AtomicBool::completed` already
serialised "who calls cancel", so there is no double-cancel risk.

### Race C: op submission fails before timer is armed

If `arm()` allocates the timer + wait but the caller's `WSASend` /
`WSARecv` returns synchronously with a non-`WSA_IO_PENDING` error
before any completion is queued, no `OVERLAPPED` ever flows through
the kernel. The `LinkedTimer` drop must:

- Detach the wait (`SetThreadpoolWait(wait_obj, NULL, NULL)`).
- Close the wait (`CloseThreadpoolWait(wait_obj)`).
- Close the timer (`CloseHandle(timer)`).
- Never call `CancelIoEx` because the timer's `completed` flag is
  flipped to `true` by drop before unregistering the wait, so a
  callback racing drop will short-circuit at the `swap`.

The struct stores the two handles as `Option<HANDLE>` /
`Option<PTP_WAIT>` so that a partially-initialised `LinkedTimer`
(one or both still `None`) drops without invoking the Win32
close-on-null undefined path.

## 5. RAII drop contract

```rust
#[cfg(windows)]
pub struct LinkedTimer {
    timer: Option<HANDLE>,
    wait: Option<PTP_WAIT>,
    overlapped: *mut OVERLAPPED,
    handle: HANDLE,
    completed: AtomicBool,
}
```

Drop order, with rationale:

1. **`completed.swap(true, AcqRel)`** - signal to any in-flight
   callback that the natural path has won. If the callback already
   ran and won, this is a no-op.
2. **`SetThreadpoolWait(wait, NULL, NULL)`** - detach the wait.
   Per MSDN, this synchronises with the threadpool: after the call
   returns, no callback for this wait is or will be running.
3. **`CloseThreadpoolWait(wait)`** - free the `PTP_WAIT`.
4. **`CancelWaitableTimer(timer)`** - belt-and-braces; harmless if
   the timer already fired.
5. **`CloseHandle(timer)`** - free the timer kernel object.

On any error during drop (`SetThreadpoolWait` cannot fail per
MSDN; `CloseHandle` can on a stale handle), log via
`tracing::warn!` and continue. Drop must not panic - panicking
inside a destructor while the surrounding scope is unwinding is
undefined behaviour at the language level (double-panic aborts
the process).

The `completed` flag is intentionally `AtomicBool` rather than a
`Mutex<bool>`. `Mutex` would serialise the natural completion
delivery with the threadpool callback, which is what we want to
avoid - the natural path must never block on a mutex held by a
threadpool thread that may itself be queued behind other waits.
An atomic swap is wait-free, races resolve in O(1) without any
kernel object, and the cost is one cache-line bounce per op.

## 6. Public API surface

```rust
#[cfg(windows)]
pub struct LinkedTimer { /* opaque */ }

#[cfg(windows)]
impl LinkedTimer {
    /// Arms a deadline that cancels `op` via `CancelIoEx` if it
    /// does not complete within `timeout`. The returned guard must
    /// outlive the op; drop it after the op's completion has been
    /// observed (success, natural error, or `ERROR_OPERATION_ABORTED`).
    ///
    /// Returns `Err` if the underlying Win32 calls
    /// (`CreateWaitableTimerExW`, `CreateThreadpoolWait`,
    /// `SetWaitableTimer`, `SetThreadpoolWait`) fail. On `Err` the
    /// caller has not yet submitted the op and can either retry or
    /// proceed without a deadline.
    pub fn arm(timeout: Duration, op: &OverlappedIo) -> io::Result<Self>;
}

#[cfg(not(windows))]
pub struct LinkedTimer { /* no-op stub */ }

#[cfg(not(windows))]
impl LinkedTimer {
    pub fn arm(_timeout: Duration, _op: &OverlappedIo) -> io::Result<Self> {
        Ok(Self { /* nothing to track */ })
    }
}
```

`OverlappedIo` is the existing abstraction over `(HANDLE, *mut
OVERLAPPED)` already shaped by `crates/fast_io/src/iocp/overlapped.rs`.

Internal visibility: `pub(crate)` initially. Once at least one
caller in `crates/transport` or `crates/daemon` exists, promote to
`pub` if the abstraction proves useful outside `fast_io`. WPG-10
follow-ups recommend keeping it internal until WPG-10.c integration
data exists; see section 11.

## 7. Integration sites

Today's IOCP hot path has three places where a per-op deadline is
the right semantic:

| Site | File / function | Default | Rationale |
|---|---|---|---|
| Socket recv | `iocp/socket.rs::IocpSocketReader::recv_async` (line 150) | **on** | A hung connection is the canonical reason a receive sits indefinitely. The Linux side bounds the poll gate via `LINK_TIMEOUT`; the recv itself is bounded by transport-level read timeouts in `crates/transport` today. Wrapping the kernel `WSARecv` with `LinkedTimer` lets us surface a `WouldBlock` / `TimedOut` rather than rely on the slower transport-layer watchdog. |
| Socket send | `iocp/socket.rs::IocpSocketWriter::send_async` (line 284) | **opt-in** | The typical batched-send path does not need a deadline; back-pressure manifests as `WSA_IO_PENDING` rather than a hang. But the `--timeout` CLI flag, when set, must bound every send; the shim is the mechanism for that. Mirrors the Linux side, which only adds `LINK_TIMEOUT` to the `POLL_ADD(POLLOUT)` that guards the batched send, not to the `SEND` itself (`batching.rs:194`). |
| Disk batch write | `iocp/disk_batch/mod.rs::flush_current` (line 257 entry; submission inside `disk_batch/writer.rs`) | **opt-in** | A stuck local-disk write is rare in practice but blocking the receiver pipeline waiting for it is the worst-case symptom. The shim provides the watchdog without baking it into the steady-state path. |

`iocp/file_reader.rs` and `iocp/file_writer.rs` are out of scope
for the initial WPG-10.c integration; they target file I/O where a
deadline is an unusual contract (local disk reads do not hang
short of hardware failure). Add later if user reports surface a
need.

Each integration site grows an `Option<LinkedTimer>` parameter,
defaulting to `None`. The presence of `Some` switches on the
deadline; the call site arms before submission, observes
completion, and drops the guard. The internal completion-pump
path is untouched.

## 8. Capability detection

`CreateWaitableTimerExW`, `SetThreadpoolWait`, `CancelIoEx`, and
`CreateThreadpoolWait` are all stable Win32 APIs available since
Vista (Vista for the threadpool calls and `CancelIoEx`, XP for
`CreateWaitableTimerExW`). The repository's minimum supported
Windows is Server 2016 / Windows 10 per `README.md`'s platform
matrix, well above the floor.

No `OnceLock` capability probe is needed. The `windows-sys`
feature set `Win32_System_Threading` (already in
`crates/fast_io/Cargo.toml`) exports all four. The whole module is
gated behind `#[cfg(windows)]` only.

## 9. Test plan

Drives the WPG-10.a implementation. All tests live under
`crates/fast_io/src/iocp/linked_timeout/tests.rs`.

1. **Deadline fires on stalled op.** Open a TCP pair where the peer
   never reads. Arm `LinkedTimer::arm(Duration::from_millis(100),
   &send_op)`, post the `WSASend`, await completion. Assert the
   completion is `ERROR_OPERATION_ABORTED` and that the elapsed
   wall-clock is within `[90 ms, 250 ms]`.
2. **Clean cancel when op completes early.** Open a TCP loopback
   pair, arm `LinkedTimer::arm(Duration::from_secs(1), &recv_op)`,
   write 1 KB on the peer, await completion. Assert the recv
   returned `Ok(1024)`, that the timer drop returns in <50 ms, and
   that the threadpool callback never observed the timer fire.
3. **Race A coverage (timer vs natural completion).** 100k
   iterations of: arm `LinkedTimer::arm(Duration::from_micros(N),
   &op)`, post the op, await completion. Sweep N across a window
   that brackets typical localhost latency so both winners happen.
   Assert no deadlock, exactly one completion per iteration, no
   double-cleanup (drop succeeds idempotently if Race C path runs).
4. **Drop without firing.** Arm a 10 s timer, drop immediately
   without submitting the op. Assert the drop returns in <10 ms
   (the threadpool detach is synchronous but cheap when no
   callback is in flight). Assert no handle leaks via a
   `_HANDLE_COUNT`-style sentinel test (process handle count
   before vs after 1000 iterations).
5. **Submission failure path (Race C).** Arm a 1 s timer, then
   simulate a synchronous `WSASend` failure (closed socket).
   Assert: the timer drops cleanly, no `CancelIoEx` is ever
   invoked (the callback short-circuits because drop flipped
   `completed = true`), the caller surfaces the natural
   `WSAESHUTDOWN` error.
6. **Comparison test against Linux `LINK_TIMEOUT`.** Drive the
   same `poll_writable`-style workload through both backends with
   a parametrised deadline `T`. Assert the observed cancel
   latencies match within +/-10 ms across `T in [10ms, 100ms, 1s]`.
   Gated behind both `target_os = "linux"` and `target_os =
   "windows"` runners in the interop matrix; ships as part of
   WPG-10.c not WPG-10.a.
7. **No panic in drop.** Inject a fake `CloseHandle` failure (via
   the test-only error-injection hook used by other
   `fast_io::iocp` tests) and verify the drop logs but does not
   panic. Uses a `#[cfg(test)]` shim mirroring
   `disk_batch::inject_next_write_error_for_test`.

## 10. Performance considerations

- **Per-op overhead.** Arming the timer + wait costs roughly two
  user/kernel transitions plus one allocation
  (`CreateWaitableTimerExW` + `CreateThreadpoolWait`). On a warm
  Win10 1909 box this is approximately 10 us per op. The Linux
  side pays one extra SQE push (~50 ns) for the same primitive.
  The delta is acceptable for ops with ms-scale deadlines (every
  WPG-10 use case) and unacceptable for sub-microsecond hot loops
  (no use case in scope).
- **Threadpool contention.** Win32's default threadpool is a
  process-wide shared pool. If `LinkedTimer` is on the steady-state
  send hot path (it is not - we opt the send side out for that
  reason) the threadpool's wait dispatch can serialise behind other
  long-running callbacks. Audit risk only; no measured contention
  in the current call sites. Mitigation if benchmarks expose a
  hotspot: use `CreateThreadpoolWait`'s `PTP_CALLBACK_ENVIRON*`
  argument to pin waits to a dedicated `CreateThreadpool` instance
  owned by `fast_io`.
- **Pooling.** Each `arm()` creates fresh handles. A pool keyed by
  thread-id could amortise the allocation, but only WPG-10's
  bench coverage can prove the win is worth the lifecycle
  complexity. Defer to WPG-10.b (section 11).
- **Cache-line bouncing.** The `AtomicBool::completed` is the only
  contended cache line per op. With sub-microsecond IOCP
  completion latency and timer wakeups on a different core, the
  bounce is single-digit nanoseconds and dominated by the syscall
  overhead of the IOCP completion delivery itself.

## 11. Open issues / follow-ups

- **WPG-10.a** - implement the shim under
  `crates/fast_io/src/iocp/linked_timeout/`. Owner: this design's
  author. Exit criteria: unit tests 1-5 and 7 from section 9 pass
  on Windows; the workspace continues to build on Linux/macOS via
  the `#[cfg(not(windows))]` no-op stub.
- **WPG-10.b** - benchmark + decide pooling. Owner: `fast_io`
  perf workstream. Exit criteria: if `criterion` over `arm` +
  `drop` shows >25% of the steady-state op cost, ship a
  per-thread pool keyed by `GetCurrentThreadId()`. Otherwise
  document that the per-op allocation is the chosen trade-off.
- **WPG-10.c** - integrate at the three sites from section 7,
  threaded through the existing CLI surface
  (`--timeout` -> per-op `LinkedTimer`). Owner: `crates/transport`
  + `crates/daemon`. Exit criteria: comparison test 6 from
  section 9 passes; the receiver-side IOCP recv pipeline reports
  `TimedOut` within the configured deadline rather than relying
  on the slower transport-layer watchdog.
- **API visibility.** The recommendation is to keep `LinkedTimer`
  internal to `crates/fast_io` (visible as
  `pub(crate)`) until the WPG-10.c integration data shows whether
  any caller outside `fast_io` benefits from the abstraction. If
  `crates/daemon` ends up wrapping its own `IocpSocketWriter`
  with a deadline, promote to `pub` and document the contract in
  the public rustdoc.
- **WPG-11** stays separate. It tracks an IOCP test-harness
  parity gap for drop-contract assertions and is unrelated to the
  linked-timeout shim.

## 12. Deduplication primitive

`AtomicBool` with `Ordering::AcqRel` for `swap` and `Ordering::Acquire`
for the natural-completion side of the flag check. Rationale, in one
line per alternative considered:

- `AtomicBool` (chosen): wait-free, no kernel object, races resolve in
  O(1), the threadpool callback can run on any thread without blocking
  the natural completion path on a mutex.
- `Mutex<bool>`: rejected - the threadpool callback would block the
  natural path if it landed first; a wait-free race resolver is the
  whole point of the shim being cheap.
- `Once`: rejected - the cancel side wants to know whether it won the
  race, not just whether some side initialised the value.
- Channel send + try_recv: rejected - higher overhead than a single
  atomic for the trivial "did I observe completion first" question.

## 13. Cross-references

- WPG-7.a (opcode inventory): `docs/design/wpg-7-iouring-opcode-inventory.md`
  - `LINK_TIMEOUT` row: line 50.
  - Dispatch classification: line 93.
- WPG-7.b (IOCP mapping): `docs/design/wpg-7b-iouring-iocp-mapping.md`
  - `LINK_TIMEOUT` row: line 57.
  - Gap summary: lines 78-83.
- WPG-7.c (prioritised gap list): `docs/design/wpg-7c-iocp-gap-list.md`
  - Gap #2 row: line 35.
  - Severity rationale: lines 49-53.
  - Follow-up entry: lines 65-71.
  - Sprint ordering: lines 96-98.
- Linux call site to mirror: `crates/fast_io/src/io_uring/batching.rs`
  - `poll_writable`: lines 173-249.
- IOCP integration sites:
  - `crates/fast_io/src/iocp/socket.rs::recv_async`: lines 150-210.
  - `crates/fast_io/src/iocp/socket.rs::send_async`: lines 284-335.
  - `crates/fast_io/src/iocp/disk_batch/mod.rs::flush_current`:
    line 257.
- WPG-8 (zero-copy socket send): `docs/design/wpg-8-send-zc-windows-equivalent.md`.
- WPG-9 (registered-buffer scheme): `docs/design/wpg-9-registered-buffer-windows-equivalent.md`.
