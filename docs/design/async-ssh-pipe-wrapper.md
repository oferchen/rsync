# Async SSH Pipe Wrapper - Read/Write Overlap (#1412)

Tracking issue: #1412 (prototype async SSH pipe wrapper with read/write
overlap).

Status: design only, no code. Phase-3 follow-up gated behind the
async-daemon work in #1935.

Sibling docs:

- `docs/design/async-migration-plan.md` (#1594, PR #4186) - the canonical
  multi-phase plan. Phase 3 owns async SSH work.
- `docs/design/async-ssh-transport.md` (#1593) - bench-gated transport
  evaluation.
- `docs/design/async-ssh-evaluation.md` (PR #4194) - embedded-russh
  first ordering; this doc focuses one level below it on the pipe wrapper.
- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) and
  `docs/audits/ssh-single-socketpair-bidirectional.md` (#1687) - wire
  primitive choice (pipes, not socketpairs).
- `docs/audits/ssh-daemon-perf-verification.md` (PR #4154) - SSH
  transport perf fix that landed and shifted the baseline.

## 1. Today's pipe wrapper

The SSH data channel is the spawned `ssh` child's inherited stdio. The
parent treats `ChildStdin` as the write half and `ChildStdout` as the
read half. Both are anonymous pipes (rationale: the audits cited above
ruled out the single- and two-socketpair variants on `splice(2)`
eligibility and half-close semantics).

The implementation lives entirely in `crates/rsync_io/src/ssh/`:

| Site | File:line | Role |
|------|-----------|------|
| `SshConnection` | `connection.rs:30-39` | Owns `Arc<Mutex<Option<Child>>>`, `Option<ChildStdin>`, `Option<ChildStdout>`. |
| `SshReader` | `connection.rs:222-230` | Blocking `Read` impl over `ChildStdout`. |
| `SshWriter` | `connection.rs:234-246` | Blocking `Write` impl over `ChildStdin`. |
| `SshConnection::split` | `connection.rs:187-217` | Splits the connection into independent reader/writer halves and a child handle. |
| `StderrAuxChannel` | `aux_channel.rs:149-156`, `219-228` | Spawns a `std::thread` per connection to drain `ChildStderr` (pipe or socketpair backed). |
| `ConnectWatchdog` | `connection.rs:266-333` | Spawns a `std::thread` that condvar-waits for the SSH greeting; kills the child on timeout. |

The bidirectional pump that turns these halves into a transfer lives at
`crates/core/src/client/remote/remote_to_remote.rs:258-269`. It spawns
two `std::thread`s, one per direction, plumbed through
`mpsc::channel()` for completion signalling and an `AtomicBool` for
cooperative shutdown.

Module-level rationale for the pipe topology is in
`crates/rsync_io/src/ssh/mod.rs:57-85`. The same comment block points
out that the `fast_io` io_uring socket fast path is unreachable for
SSH (pipe FDs are not sockets) and that pipe-FD io_uring work is
tracked separately under #1859 and #1860.

The recent socketpair-related work that bounds this design:

- PR #4154 (the SSH push and daemon push deadlock fix) is the
  perf-relevant change on master immediately before this proposal; the
  baseline numbers any async prototype is measured against come from
  the post-#4154 numbers in
  `docs/audits/ssh-daemon-perf-verification.md`.
- #4193 (NACK audit) re-confirms the single-socketpair variant does not
  ship, leaving anonymous pipes as the wire primitive any async
  wrapper must target.
- PR #4194 (async SSH evaluation) settles the embedded-russh vs async
  subprocess ordering at the *transport* level. This document narrows
  that scope to the *pipe wrapper* level: what shape do we give the
  async halves themselves, independent of whether they front a
  `tokio::process::Child` or a russh channel.

## 2. AsyncFd vs `tokio::process::Child` stdio

The pipe wrapper needs an async `Read`/`Write` surface. Two candidate
constructions exist within tokio.

### 2.1 `tokio::process::Child` stdio

`tokio::process::Command` returns a `tokio::process::Child` whose
`stdin: Option<ChildStdin>` and `stdout: Option<ChildStdout>`
implement `tokio::io::AsyncWrite` and `tokio::io::AsyncRead`. The
stderr handle is similarly `AsyncRead`. Internally, tokio registers
each pipe FD with its mio reactor via `AsyncFd`, sets `O_NONBLOCK`,
and drives readiness via the runtime's epoll/kqueue loop. On Windows,
the equivalent path uses overlapped I/O on the named pipes that
`tokio::process` synthesises in place of anonymous pipes.

This is the natural target. The question is whether oc-rsync's
subprocess type can switch from `std::process::Command` to
`tokio::process::Command`.

What changes:

- `SshCommand::spawn` (builder.rs) would return a tokio child. It must
  be called from inside an async context (or hold a reactor handle)
  because tokio drops the child via the runtime when the wrapper is
  dropped.
- `SshReader::stdout: ChildStdout` becomes
  `tokio::process::ChildStdout`. `Read` impl becomes `AsyncRead`.
- `SshWriter::stdin: ChildStdin` becomes
  `tokio::process::ChildStdin`. `Write` impl becomes `AsyncWrite`.
- `StderrAuxChannel`'s `std::thread` drain loop collapses into a
  `tokio::select!` arm consuming `tokio::process::ChildStderr` (no
  separate thread needed). The 64 KiB rolling-window buffer logic
  is reusable as-is.
- `ConnectWatchdog`'s condvar/thread pattern collapses into
  `tokio::time::timeout(connect_deadline, greeting_future)`. The
  child-kill on timeout becomes a direct `.kill().await`.

What stays the same:

- `SshCommand` builder API. All its setters are sync `&mut self` and
  remain so; only `spawn` changes return type.
- The argv/operand construction in `builder.rs` and `operand.rs`.
- The wire-format and the rsync protocol layer above the transport.

This is the construction the async migration plan already commits to
(see `docs/design/async-migration-plan.md` section 5.4 and the
embedded-russh-first ordering in
`docs/design/async-ssh-evaluation.md` section 5.3). The async
subprocess transport is tracked under #1806.

### 2.2 Raw `AsyncFd` over the existing pipes

The alternative is to keep `std::process::Command::spawn`, take the
`ChildStdin::as_raw_fd()` / `ChildStdout::as_raw_fd()`, set
`O_NONBLOCK` with `fcntl`, wrap each FD in
`tokio::io::unix::AsyncFd`, and write our own `AsyncRead`/`AsyncWrite`
adapters on top. Same internal mechanism the tokio child stdio uses,
just one layer below the official API.

Why one might consider it:

- Keeps `std::process::Command` and therefore avoids requiring the
  caller to own a tokio runtime at spawn time. The runtime is only
  needed at I/O time, when `AsyncFd::readable()` is awaited.
- Avoids tokio's `Child` reaper machinery (a `SIGCHLD`-driven task on
  Unix). For test harnesses or single-shot CLI invocations that already
  reap the child by hand, this is one fewer moving piece.

Why it loses to `tokio::process::Child`:

- We re-implement what tokio already supplies. `tokio::process` exists
  precisely to spare callers from the `O_NONBLOCK`+`AsyncFd`+
  signal-driven-reap dance.
- On Windows, anonymous pipes are not directly compatible with IOCP
  the way `tokio::process`'s named-pipe substitution makes them. A
  cross-platform `AsyncFd`-only design must add a separate Windows
  path; using `tokio::process` we inherit one.
- Reaping the child outside tokio requires either a dedicated
  `std::thread::Builder::new().spawn(move || child.wait())` (puts the
  watchdog thread back) or polling `try_wait` from an async loop
  (busy-polls the runtime).
- The connect watchdog and stderr drain still need their own
  asynchrony story. `AsyncFd` only buys us the data half, not the
  control half.

### 2.3 Recommendation for the wrapper type

Use `tokio::process::Child` stdio. The `AsyncFd` variant is only worth
considering as an escape hatch if a concrete cross-platform regression
makes the tokio child path untenable. None is currently known.

## 3. What does async actually add over today's threads?

Today's pipe wrapper already overlaps reads and writes - because the
parent runs the read half on one thread and the write half on another
(`remote_to_remote.rs:258-269`), and on a split connection the
upstream caller does the same. Two threads against two pipe FDs is
already two-way overlap. The honest accounting of what async adds:

### 3.1 Thread elimination (the main win)

Per active SSH transfer, the current model costs:

| Thread | File:line | Workload |
|--------|-----------|----------|
| Receiver pump | `remote_to_remote.rs:265` (or caller) | `read` on `ChildStdout` |
| Sender pump | `remote_to_remote.rs:258` (or caller) | `write` on `ChildStdin` |
| Stderr drain | `aux_channel.rs:149-156` (pipe) or `:219-228` (socketpair) | `read` on `ChildStderr` |
| Connect watchdog | `connection.rs:289-324` (when armed) | `wait_timeout_while` on a condvar |

That is three to four OS threads parked on syscalls per connection.
For the one-shot CLI client this is irrelevant: one connection, four
threads. For a fan-out client doing N concurrent SSH pulls this is
3*N to 4*N parked threads with 8 MiB virtual stacks each. With
`tokio::process::Child` + `tokio::select!`, all four collapse into
arms on a single per-connection async task driven by the shared
reactor. Per-connection thread cost amortises to roughly `num_cpus`
across the runtime, independent of N.

The migration plan's section 2.2 (SSH transport - MAYBE) explicitly
identifies this as the dominant benefit for fan-out clients, and
section 4 marks the stderr drain and watchdog as collapsing into
tokio primitives.

### 3.2 Event-loop scheduling (modest win)

Today's two pump threads each have to be scheduled by the kernel
each time the pipe peer becomes ready. With one async task in
`select!` over both halves, the scheduling decision is made in
userspace by the tokio reactor: no context switch between the read
and the subsequent write to the other half. On a high-RTT link
(>= 50 ms) where the read frequently parks the thread, the win is
already absorbed by the parking cost being a single epoll wait.
On LAN/loopback where pipe latency is microseconds, the win is
neutral or slightly negative.

This is the workload-class analysis already documented in
`docs/design/async-ssh-transport.md` section 4. The pipe wrapper
inherits it without modification.

### 3.3 Cancellation and timeout shape (correctness, not perf)

`tokio::time::timeout(deadline, fut)` is the natural shape for the
connect watchdog. `tokio::select!` with a `CancellationToken` is the
natural shape for the bidirectional pump's shutdown signal. Both
replace the current `Arc<AtomicBool>` + `Condvar` + manual `Child::kill`
choreography (`connection.rs:307-322`, `remote_to_remote.rs:252-269`).
This is a model simplification, not a throughput win, but it
materially reduces the surface area where Drop, panic, or
early-return paths can leak a child process or a thread.

### 3.4 What async does *not* add

- It does not reduce `fork`+`execve` cost. `tokio::process::Command`
  still calls `fork+execve`. The win is post-spawn (migration plan
  risk R2). The embedded russh path (`docs/design/async-ssh-evaluation.md`
  section 5.2) is the only way to eliminate that cost; it is a
  separate track from this wrapper question.
- It does not let SSH use the `fast_io` io_uring socket fast path -
  pipes are still pipes (`mod.rs:57-75`). Pipe-FD io_uring is
  tracked under #1859 and the splice path under #1860; both are
  orthogonal to the async wrapper.
- It does not change the wire protocol. Golden byte tests in
  `crates/protocol/tests/golden/` continue to pass byte-for-byte.
- It does not change the bidirectional pump's *throughput ceiling*
  on a saturated single-large-file LAN transfer. Async overlaps two
  pipe halves that the kernel pipe buffer already overlaps; the
  ceiling is the slower of the two halves.

## 4. Migration shape

The migration plan (`docs/design/async-migration-plan.md` section 3,
Phase 3) covers async SSH transport behind the `--features async-ssh`
flag. This section narrows to the wrapper-level shape - the surface
between the `SshConnection` type and the I/O traits it exposes.

### 4.1 Surface design

Keep `SshCommand` and `SshConnection` as the named types. Add a
parallel async type rather than a feature-flag fork of the same
types:

```text
SshCommand            (sync, today's builder, unchanged setters)
  .spawn() -> SshConnection                    (sync, blocking pipes)
  .spawn_async() -> AsyncSshConnection         (new, behind `async-ssh`)
```

`AsyncSshConnection` exposes:

- `split(self) -> io::Result<(AsyncSshReader, AsyncSshWriter, AsyncSshChildHandle)>`,
  matching today's `SshConnection::split` (`connection.rs:187-217`)
  shape one-for-one.
- `AsyncSshReader: tokio::io::AsyncRead` over
  `tokio::process::ChildStdout`.
- `AsyncSshWriter: tokio::io::AsyncWrite` over
  `tokio::process::ChildStdin`.
- `AsyncSshChildHandle` with `async fn wait(self)` and
  `async fn wait_with_stderr(self)`, replacing the sync
  `SshChildHandle::wait` / `wait_with_stderr` at
  `connection.rs:457-489`.

Parallel types let the sync `SshConnection` stay byte-for-byte
identical for `--no-default-features` builds (migration plan section
6.2 invariant). Internally, the two types can share a single builder
implementation and a single stderr-buffer struct (the 64 KiB rolling
window is generic over its reader type).

### 4.2 Stderr drain collapse

`StderrAuxChannel` (`aux_channel.rs`) keeps the same surface:
`collected() -> Vec<u8>`, `shutdown_read()`, `join()`. The
implementation behind it varies:

- Sync: today's `std::thread` drain loop (no change).
- Async: an internal `tokio::task::JoinHandle` that runs the same
  drain loop using `tokio::io::AsyncReadExt`. `shutdown_read()` aborts
  the task; `collected()` reads the shared `Mutex<VecDeque<u8>>` (or
  equivalent shared buffer).

The 64 KiB rolling window logic is reused unchanged. Only the I/O
primitive changes.

### 4.3 Connect watchdog collapse

The watchdog (`connection.rs:266-333`) goes away in the async path.
Replace with:

```text
let greeting = tokio::time::timeout(
    timeout,
    read_greeting(&mut reader),
).await?;
```

The cancel path is implicit - drop the timeout future. No condvar, no
shared `AtomicBool`, no kill-the-child dance. If the timeout fires,
the caller drops `AsyncSshConnection`; tokio reaps the child via its
internal `SIGCHLD` task.

### 4.4 Bidirectional pump

The two-thread relay at `remote_to_remote.rs:258-269` becomes a single
async task driven by `tokio::io::copy_bidirectional` or a hand-written
`select!` over the two halves. The cooperative shutdown
`Arc<AtomicBool>` is replaced by dropping the pump future (or by a
`tokio_util::sync::CancellationToken` shared with the rest of the
transfer state machine).

This is the boundary the migration plan calls Phase 3 (`docs/design/
async-migration-plan.md` section 3.3 / Phase 3 / item 1). Wrapper
work in this document lives below that line; the pump is the first
async consumer of the wrapper.

### 4.5 What stays sync

- The receiver pipeline (`crates/transfer/src/receiver/transfer/
  pipeline.rs`), the SPSC network->disk channel
  (`crates/transfer/src/pipeline/spsc.rs`), and the disk-commit thread
  (`crates/transfer/src/disk_commit/`). All three are pinned sync by
  migration plan risk R3 (the SPSC spins; it must not run inside a
  tokio worker).
- The delta apply, checksum, and filter layers. All CPU-bound.
- The `--no-default-features` build. Must remain tokio-free
  (migration plan section 6.2).

The handoff between the async pump and the sync receiver pipeline is
the bounded-channel bridge from migration plan section 5.3. That
bridge is part of Phase 2 work (#1591, #1732), not this design.

## 5. Bench plan

The migration plan promotion gate for any default-on async SSH path
is > 10% sustained wall-clock improvement on at least one supported
corpus without LAN regression. This wrapper's bench plan inherits
that gate and adds two specific configurations targeted at the
overlap claim.

Workload matrix:

| Link shape | RTT | Bandwidth | Source corpus | Expected sync vs async |
|-----------|-----|-----------|---------------|------------------------|
| LAN baseline | < 1 ms | line rate | `large_random_files` | Neutral; no regression allowed. |
| Emulated 1 MB/s link | 100 ms | 1 MB/s | `many_small_files` | Async wins; #1889 sync-vs-async benchmark task. |
| Emulated 1 MB/s link | 100 ms | 1 MB/s | `large_random_files` | Async neutral; pipe buffer hides serialisation. |
| Slow rotational disk | LAN | line rate | `large_random_files` | Async wins; disk-write half dominates. |
| Fan-out, 100 connections | LAN | line rate | small `mixed` | Async wins; thread count is the bottleneck. |

The 1 MB/s emulated link is the configuration #1889 specifically
calls out for sync-vs-async SSH transfer measurement. Shape with
`tc qdisc add dev lo root netem delay 100ms rate 1mbit` or
equivalent (the netem shape is already used by
`scripts/benchmark_remote.sh` per `docs/design/async-ssh-transport.md`
section 4).

Per-run capture:

- Wall clock (hyperfine warmup 1, runs 5).
- Resident set size from `/usr/bin/time -v`.
- Thread count from `/proc/<pid>/status` mid-transfer (count of
  `Threads:` line). Validate the "thread elimination" claim
  quantitatively.
- Syscall count from `strace -c` (validate no new syscall churn from
  the tokio reactor on the LAN baseline).

Acceptance:

- LAN baseline within 5% of post-#4154 sync numbers.
- 1 MB/s emulated link > 10% wall-clock improvement on
  `many_small_files`.
- Fan-out: thread count grows sublinearly in connection count
  (sync grows linearly, async should plateau near `num_cpus`).

## 6. Recommendation

**Implement async pipes only after async daemon (#1935) lands.**

The wrapper itself is a small refactor (Section 4 shows the surface
changes are local to `crates/rsync_io/src/ssh/`). The cost is not
the wrapper - it is the runtime ownership question. Three reasons to
defer:

1. **Shared runtime.** The async pump (Phase 3 in the migration
   plan) needs a tokio runtime to run on. The daemon (#1935) brings
   that runtime into the process for the production accept path. If
   the wrapper ships first, every CLI invocation that wants async
   SSH has to spin up its own current-thread runtime (the same
   `block_on` shape the embedded SSH facade uses today at
   `crates/rsync_io/src/ssh/embedded/connect.rs:107`), which the
   migration plan section 5.5 already flags as forbidden when the
   sync caller is inside another runtime. Defer until the runtime
   ownership story is settled by #1935.

2. **Single integration cost.** Lifting one async surface
   (`crates/rsync_io/src/ssh/embedded/`) is a strictly smaller
   change than lifting two. PR #4194 commits the embedded-russh
   surface as the first async SSH target. The pipe wrapper for the
   subprocess path lands second (the #1806 entry in the migration
   plan and PR #4194's Section 6 table). Doing the wrapper before
   #1935 means doing the runtime-handle ownership work twice -
   once for embedded russh, once for subprocess.

3. **Bench gate is gated on the daemon.** The fan-out workload that
   makes the wrapper's thread-elimination claim measurable
   (Section 5, fan-out row) requires the async daemon to host the
   connections being multiplexed. Without #1935, the bench harness
   for the wrapper's strongest workload class does not exist.

The wrapper itself is uncontroversial: `tokio::process::Child` stdio
behind a parallel `AsyncSshConnection` type, gated by the existing
`async-ssh` feature, with the stderr drain and connect watchdog
collapsed into `tokio::select!` and `tokio::time::timeout` arms.
The deferral is about sequencing, not about the design.

Concretely, the staging order:

1. #1935 lands the async daemon listener with the production tokio
   runtime owned by the daemon.
2. PR #4194 / #1796 / #1797 lifts the embedded russh surface to
   async, exercising the runtime-handle ownership pattern on a path
   that is already async-shaped internally.
3. This wrapper (#1412, tracked-with #1806) adds the
   `AsyncSshConnection` parallel type using
   `tokio::process::Child` stdio. The wrapper shares the builder,
   stderr-drain, and watchdog abstractions with the embedded path
   via traits.
4. The async pump in `remote_to_remote.rs` switches to
   `tokio::io::copy_bidirectional` over the wrapper, gated by the
   `async-ssh` feature.
5. Bench (#1889) runs the matrix in Section 5; promote to default
   only if the gate clears.

Until #1935 lands, the synchronous wrapper stays the only supported
path. This document records the design so that when sequencing
clears, implementation has no open questions.

## 7. Cross-references

| Tracker | Subject |
|---------|---------|
| #1412 | This design - prototype async SSH pipe wrapper with read/write overlap. |
| #1593 | Async SSH transport evaluation (`docs/design/async-ssh-transport.md`). |
| #1594 / PR #4186 | Async migration plan (`docs/design/async-migration-plan.md`). |
| #1687 | Single-socketpair audit (`docs/audits/ssh-single-socketpair-bidirectional.md`). |
| #1751 | `spawn_blocking` rayon bridge (`docs/design/tokio-spawn-blocking-rayon.md`). |
| #1795 | SSH transport composition entry point. |
| #1796 | Embedded russh async surface. |
| #1797 | Embedded russh integration with the bidirectional pump. |
| #1798-#1804 | SshTransport pieces (operand, stderr-aux, watchdog, builder, parse, embedded auth, embedded resolve). |
| #1805 | Embedded russh hardening - cancellation, panic isolation, exit-code mapping. |
| #1806 | Async subprocess transport - tokio-process backend; this wrapper lands here. |
| #1859 | io_uring read/write against pipe FDs (orthogonal). |
| #1860 | Splice/vmsplice zero-copy pipe transfer (orthogonal). |
| #1889 | Sync-vs-async SSH benchmark on emulated 1 MB/s link. |
| #1935 | Async daemon listener implementation - runtime ownership prerequisite. |
| #1938 | Two-socketpair audit (`docs/audits/ssh-socketpair-vs-pipes.md`). |
| #4154 | SSH transport perf fix - baseline shifted (`docs/audits/ssh-daemon-perf-verification.md`). |
| #4186 | Async migration plan PR. |
| #4193 | Single-socketpair NACK audit. |
| #4194 | Async SSH evaluation (`docs/design/async-ssh-evaluation.md`). |
