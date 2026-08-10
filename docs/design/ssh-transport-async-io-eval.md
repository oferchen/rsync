# SSH Transport Async I/O Evaluation

Tracking issue: #1593.

This document supersedes the brief `docs/design/async-runtime-ssh-eval.md`
for purposes of the #1593 evaluation. It deliberately does not re-open
questions settled elsewhere; cross-references point at the canonical
answers.

## 1. Scope and what the prior docs already answered

The prior docs settled four orthogonal questions. This evaluation builds
on them and does not re-litigate any of them:

| Question | Settled in | Decision |
|----------|------------|----------|
| Which executor? | `async-runtime-ssh-eval.md` (#1411) | tokio. smol and async-std rejected on the single-runtime rule (#1780). |
| Which SSH crate? | `async-runtime-ssh-eval.md` (#1411) | russh for embedded; system `ssh` for subprocess. thrussh and `ssh2` rejected. |
| Embedded russh vs async subprocess? | `async-ssh-evaluation.md` | Embedded russh first, async subprocess second. |
| Pipe wrapper type? | `async-ssh-pipe-wrapper.md` (#1412) | `tokio::process::Child` stdio. Parallel `AsyncSshConnection` type, not a feature-flag fork. |
| Migration sequencing? | `async-migration-plan.md` (#1594) | Phase 3 owns async SSH; gated behind `--features async-ssh`. |
| Pipe primitive (anonymous pipes vs socketpair)? | `ssh-socketpair-vs-pipes.md` (#1938) and `ssh-single-socketpair-bidirectional.md` (#1687) | Anonymous pipes. |
| Rayon bridge surface? | `tokio-spawn-blocking-rayon.md` (#1751) | `rayon_bridge` helper; threshold short-circuit; `JoinError` -> `ExitCode::PROTOCOL`. |

What none of those docs answer:

1. *Which tokio runtime flavour does the SSH I/O surface need?*
   `rt-multi-thread` (shared with the daemon and indirectly with rayon
   via `spawn_blocking`), or `current_thread` scoped to the SSH socket
   only? The migration plan picks tokio but leaves the runtime *shape*
   open at the per-connection scope.
2. *How does an async `AsyncRead`/`AsyncWrite` surface bridge from the
   raw FDs we already own on the subprocess path?* The pipe-wrapper doc
   commits to `tokio::process::ChildStdin`/`ChildStdout` but does not
   evaluate the alternative escape hatch (`tokio::io::unix::AsyncFd`
   wrapping a raw FD from `std::process::Child`).
3. *What is the cost in RSS, scheduler footprint, and learning curve
   for the blocking-vs-async boundary?* The migration plan flags the
   rule (R3, R7) but does not quantify the practical cost.
4. *Which latency-sensitive paths inside the SSH protocol actually
   benefit from the overlap?* The transport eval calls out high-RTT
   links abstractly; this doc names the three concrete read sites
   (handshake greeting, multiplex header, message dispatch) and
   measures expected wins per site.
5. *What are the trigger conditions that move async SSH from
   "researched and deferred" to "implement now"?* The prior docs say
   "behind a feature flag"; this doc states the conditions under which
   the flag should flip default.

This document answers those five.

## 2. Quantified hypothesis

The hypothesis the bench plan must falsify:

> Async I/O on the SSH transport overlaps the upstream rsync sender's
> protocol read with the local receiver's disk write at the sender,
> removing the serial stall in which the sender thread is parked on
> `ChildStdout::read` while the disk-writer thread is otherwise free
> (and the symmetric stall on the receive direction).

Concrete prediction, per workload class:

| Workload | Sync wall clock (today) | Predicted async win | Confidence |
|----------|-------------------------|---------------------|------------|
| LAN + SSD, single 1 GiB file | T_lan | 0% +/- 3% (no win, no regression) | High; pipe buffer + kernel readahead already overlap. |
| 100 ms RTT, 100 MiB/s shaped link, 1 GiB file | T_rtt | 8 - 12% wall clock | Medium; pipe buffer hides some serialisation. |
| 100 ms RTT, 1 MB/s shaped link, `many_small_files` | T_small_rtt | >= 15% wall clock | High; per-file flush boundaries dominate. |
| LAN + slow rotational disk, 1 GiB file | T_rot | 10 - 20% wall clock | High; disk-write half is the long pole. |
| Fan-out 100 concurrent SSH connections, mixed | T_fanout | 4-8x thread count reduction; >= 20% wall clock | High; thread-per-half model is the bottleneck. |

The fan-out row is the only one where the win is not from overlap; it
is from thread elimination. The migration plan's section 2.2 already
calls this out as the dominant benefit for multi-connection clients.

The numbers above are predictions for the gate criterion. The bench
plan in section 7 captures the runs that confirm or refute them.

## 3. Cost: what we pay for the win

These costs are independent of which option (a or b in section 4) we
pick. They are paid the moment async SSH ships.

### 3.1 Resident set size

A tokio `rt-multi-thread` runtime with the workspace feature flags
(`rt-multi-thread, io-util, net, fs, sync, time, process, macros`)
costs roughly 2 MiB RSS at steady state on Linux x86_64: the worker
thread stacks (default 2 MiB virtual, ~64 KiB resident per worker), the
mio reactor's epoll FDs, the blocking pool descriptors, the timer wheel.
On a CLI invocation that today reports < 10 MiB RSS, this is a
measurable percentage hit. On a daemon that already hosts the listener
runtime, the cost is zero - the runtime is already there.

A `current_thread` runtime is significantly cheaper: one worker, no
blocking pool unless `spawn_blocking` is called, no work-stealing
machinery. ~200 KiB RSS overhead.

### 3.2 Compile-time and dependency surface

Tokio is already a workspace dependency under the `embedded-ssh` and
`async` features (`Cargo.toml:189`). Default-features builds, including
the CLI, do not pull it in. The `async-ssh` feature must preserve that
invariant: `--no-default-features` builds stay tokio-free. The
migration plan section 6.2 codifies this; the CI matrix entry is
covered by open question 4 in `async-ssh-evaluation.md` section 6.

### 3.3 Learning curve for the blocking-vs-async boundary

This is the underestimated cost. The codebase today has one sync model
across the engine, transfer, and most I/O. Adding an async surface that
calls into sync code from `spawn_blocking` and accepts data from sync
code over a bounded channel introduces three failure modes that do
not exist today:

1. **Reactor starvation from inadvertent blocking.** A `std::fs::read`
   or a `Mutex::lock` inside an async task on a `current_thread`
   runtime stalls every other future on that runtime. On
   `rt-multi-thread` the same call only stalls one worker, but with
   the default worker count = `num_cpus`, four such calls saturate the
   pool. The rule is "blocking calls go through `spawn_blocking`"; the
   trap is that every PR touching the async path must enforce it.
2. **Cancellation gaps.** `spawn_blocking` futures cannot be
   cancelled. The async pump can be dropped while the sync worker on
   the blocking pool is mid-syscall, holding a `ChildStdin`. The sync
   worker has to check a cooperative cancellation token at every
   transfer-batch boundary, identical to the rayon bridge discipline.
   Tested via panic-isolation tests; the convention exists today but
   is not exercised on the SSH transport.
3. **Drop-order hazards under nested runtimes.** `tokio::process::Child`
   requires a tokio reactor handle to be alive for the lifetime of the
   child. If the wrapper is dropped after the runtime, the child reaper
   panics. The mitigation is to drop the child explicitly before the
   runtime shuts down. The current `SshChildHandle::Drop` impl
   (`connection.rs:492`) already reaps to prevent zombies on the sync
   path; the async equivalent must do the same with `.await` semantics,
   which means the Drop impl cannot be `async` and must instead block
   on a small embedded runtime or queue the kill onto a still-live
   reactor handle. This is a known russh/tokio pattern, not novel, but
   it is one more invariant to land tests for.

These three are the migration-plan risk register applied to the
specific SSH transport surface. None are blockers; all are review
discipline.

## 4. Two runtime options

The prior docs commit to tokio. This document picks between two
ways to *use* tokio on the SSH transport surface.

### 4.1 Option (a): shared `rt-multi-thread` with rayon via `spawn_blocking`

The daemon already plans to own a multi-thread runtime (#1935,
`daemon-tokio-async-listener-impl.md`). The async SSH pump runs as a
future on that runtime. CPU-bound work (delta, checksum, filter) and
the sync transfer pipeline (`pipeline/spsc.rs`, the disk-commit thread)
stay in `spawn_blocking` tasks. Rayon work spawned from inside those
blocking tasks goes through `rayon_bridge` (#1751,
`tokio-spawn-blocking-rayon.md`).

**Pros**:

- Single runtime for the whole process. No nested `block_on`, no
  cross-runtime channel bridging. Composition with the async daemon is
  trivial.
- Work-stealing across SSH connections. A burst of completions on one
  connection does not park the others.
- The rayon bridge is already designed for this shape. No new bridge
  primitives required.
- Default tokio worker count = `num_cpus` matches the rayon thread
  pool size, so the two pools do not double-book CPU cores under load.

**Cons**:

- The 2 MiB RSS hit is paid even when the CLI is single-connection.
  For the one-shot `oc-rsync user@host:src dst` case, this is pure
  waste. Mitigation: the CLI does not own the runtime; it spins up a
  `current_thread` runtime per invocation (effectively option b on the
  CLI side) and only the daemon uses the multi-thread runtime.
- Inadvertent blocking on a worker pool thread is harder to detect in
  reviews than on a `current_thread` runtime, because the symptom is
  reduced throughput, not stall.
- `spawn_blocking` to wrap the sync transfer pipeline costs one
  blocking-pool thread per active SSH transfer. The default pool is
  512 threads, so fan-out is bounded by that ceiling unless we tune
  `tokio::runtime::Builder::max_blocking_threads`. For the fan-out
  workload class this becomes the binding constraint.

### 4.2 Option (b): `current_thread` runtime scoped to the SSH socket only

Each SSH connection owns a private `tokio::runtime::Builder::new_current_thread()`
runtime - the same shape `crates/rsync_io/src/ssh/embedded/connect.rs:107`
already uses today for the embedded-ssh facade. The runtime hosts only
the SSH I/O: the bidirectional pump, the stderr drain, the connect
watchdog. The transfer engine, the SPSC pipeline, and the disk-commit
thread run on dedicated `std::thread::spawn`ed workers as today, with
a bounded channel bridging the two.

**Pros**:

- ~200 KiB RSS overhead per connection. CLI cost is negligible.
- No `spawn_blocking` discipline required. The async surface and the
  sync surface are physically separated by a channel, not logically by
  a function-call convention. Reviews are simpler.
- No worker-pool starvation. The reactor only ever runs the SSH I/O
  tasks; CPU-bound work cannot block it because CPU-bound work runs in
  a different thread by construction.
- Composes with the existing sync engine without any engine-side
  changes. The receiver pipeline keeps its current thread model
  verbatim.

**Cons**:

- Per-connection runtime means per-connection reactor, per-connection
  timer wheel, per-connection blocking pool (if anything wants
  `spawn_blocking`). For fan-out clients this multiplies the cost that
  option (a) amortises across one shared runtime.
- The async daemon (#1935) hosts its own multi-thread runtime. If the
  SSH connections inside the daemon each spin a current-thread
  runtime, we have N+1 runtimes in one process. Tokio supports this,
  but cross-runtime channels (`tokio::sync::mpsc` is runtime-local in
  some failure modes) require care. The migration plan section 5.5
  flags nested runtimes as a hazard; option (b) inside the daemon
  walks into that hazard intentionally.
- Composition with the rayon bridge is awkward. A `current_thread`
  runtime has no worker pool; `spawn_blocking` on it spawns on a
  shared blocking pool, but rayon work routed through
  `rayon_bridge` then crosses two scheduling boundaries. Workable but
  not natural.

### 4.3 The hybrid that the recommendation actually adopts

Neither option is right in all contexts. The recommendation in section 8
adopts option (a) for the daemon and option (b) for the CLI, with the
boundary at the `oc-rsync` binary mode dispatch (`crates/cli/src/`)
and a shared async surface that does not care which runtime is hosting
it.

## 5. Migration risk: bridging sync `Read + Write` to async

Today's transport exposes sync `Read + Write` over `SshChildHandle`:

- `SshConnection::split() -> io::Result<(SshReader, SshWriter, SshChildHandle)>`
  at `crates/rsync_io/src/ssh/connection.rs:187`.
- `SshReader` is a thin `Read` impl over `ChildStdout` at
  `connection.rs:222`.
- `SshWriter` is a thin `Write` impl over `ChildStdin` at
  `connection.rs:234`.
- `SshChildHandle` owns the child and reaps on Drop at
  `connection.rs:396` / `:492`.

An async surface needs `AsyncRead + AsyncWrite`. Three bridge paths
exist; the pipe-wrapper doc settles on the third but does not evaluate
the first two as escape hatches for the rare cases where it does not
fit.

### 5.1 `tokio::io::unix::AsyncFd` over a raw FD from `std::process::Child`

Keep `std::process::Command::spawn`. Take `ChildStdin::as_raw_fd()` and
`ChildStdout::as_raw_fd()`. Set `O_NONBLOCK` via `fcntl`. Wrap each FD
in `tokio::io::unix::AsyncFd`. Implement `AsyncRead`/`AsyncWrite` by
delegating to `AsyncFd::readable()` / `AsyncFd::writable()` ready
guards and issuing the underlying `read(2)` / `write(2)` directly.

**When this is the right call**:

- A caller that already has a `std::process::Child` and cannot
  re-spawn it through `tokio::process::Command` (rare; the only known
  case is the embedded-ssh facade re-entering with an external child,
  which we do not do).
- A platform where `tokio::process::Child` has a known limitation we
  hit. None currently known; on Windows tokio synthesises named pipes
  in place of anonymous pipes for IOCP compatibility, which works.

**When it is wrong**: every other case. We re-implement what
`tokio::process` already supplies, including the `SIGCHLD` reaper, the
`O_NONBLOCK` setup, and the Windows IOCP path.

**Risk**: Implementing our own `AsyncRead` over `AsyncFd` is a small
unsafe block (or use `tokio::io::Interest` and stay safe). The
`rsync_io` crate currently denies unsafe code; we would have to either
push the bridge into `fast_io` or use the safe `Interest` API. The
safe path is straightforward; this is not a blocker.

### 5.2 `russh::ChannelStream` for the embedded path

`russh` exposes `ChannelStream`, which implements both `AsyncRead` and
`AsyncWrite` over a russh channel. The embedded-ssh path
(`crates/rsync_io/src/ssh/embedded/`) returns `ChannelReader` and
`ChannelWriter` today; those wrap russh channel handles directly. The
async surface is the natural sibling: lift `connect_and_exec` to an
async public surface (the work tracked under #1796) and expose
`ChannelStream` halves.

**When this is the right call**: every async path that goes through
embedded russh. The futures already exist internally; the bridge work
is trivial (remove the `block_on` shim at `embedded/connect.rs:112`).

**When it is wrong**: the subprocess transport. `ChannelStream` is a
russh type; it does not generalise to subprocess pipe FDs.

**Risk**: russh API stability (migration plan R6). The russh crate is
pinned at `0.60.1` in the workspace `Cargo.toml`; minor-version churn
on `ChannelStream` would force the embedded path to chase it. Pin
strictly; isolate behind `embedded-ssh`; treat as a separable failure
surface from the subprocess path.

### 5.3 `tokio::process::ChildStdin` / `ChildStdout` for the subprocess path

The pipe-wrapper doc settles on this. `tokio::process::Command::spawn`
returns a `tokio::process::Child` whose stdin/stdout are
`AsyncWrite`/`AsyncRead`. Internally, tokio does exactly the
`AsyncFd` + `O_NONBLOCK` + `SIGCHLD` reaper construction option (5.1)
would force us to write ourselves.

**When this is the right call**: the subprocess transport. Always,
unless a concrete cross-platform regression makes the tokio child path
untenable. None currently known.

**When it is wrong**: the embedded path, where no subprocess exists.

**Risk**: `tokio::process::Child` requires a reactor handle alive for
the child's lifetime. This is the drop-order hazard from section 3.3.
The mitigation is structural: the `AsyncSshConnection` type owns the
child and is dropped before the runtime that hosts it. Enforced by
ownership; tested by a runtime-shutdown-with-live-child test.

### 5.4 Recommendation on bridge construction

- Embedded path: `russh::ChannelStream`. Bridge via section 5.2.
- Subprocess path: `tokio::process::ChildStdin`/`ChildStdout`. Bridge
  via section 5.3.
- `AsyncFd` over a raw FD (section 5.1): retained as the documented
  escape hatch. Not the default. Not implemented unless a concrete
  regression forces it.

## 6. Latency-sensitive paths inside the SSH protocol

The transport eval names workload classes. This section names the
three specific read sites inside the SSH protocol where async overlap
or thread elimination is observable, and what each one wins.

### 6.1 Handshake greeting

On connect, the receiver thread is parked on the first read from
`ChildStdout` waiting for the `@RSYNCD:` greeting or the server-side
rsync version line. The connect watchdog thread is parked on a
condvar waiting for either the greeting or the timeout. Today, this
costs two threads (receiver + watchdog) for the duration of the
TCP handshake + SSH key exchange + remote rsync startup.

**Async win**: the watchdog collapses into
`tokio::time::timeout(deadline, read_greeting(&mut reader))`. The
receiver task is the only thing waiting. Zero throughput win on the
critical path - the bottleneck is the SSH handshake itself, not
scheduling. The win is model simplification (section 3.3 of
`async-ssh-pipe-wrapper.md`): one fewer thread, one fewer Drop dance.

### 6.2 Multiplex header reads

After the handshake, the receiver demultiplexes `MSG_*` frames from
the multiplexed stream. Each frame is a 4-byte header read followed by
a payload read of size N. The two reads are syscall-adjacent; the
header read frequently returns immediately from the kernel pipe buffer
because the previous payload read drained less than a frame's worth
and the header is sitting in the buffer.

**Async win**: in the common case (header in buffer), `AsyncRead` and
sync `Read` are indistinguishable - both return immediately. In the
worst case (header straddles a kernel-pipe-buffer fill boundary), the
sync path parks the receiver thread on the second `read(2)` while the
disk-writer thread sits idle waiting for the previous frame's apply to
complete. Async overlap lets the disk-writer make progress on the
in-flight frame while the receiver waits on the next header. The win
on this path is the dominant per-frame contribution to the high-RTT
prediction in section 2.

### 6.3 Message dispatch and `MSG_DATA` payload reads

The hot loop: read header, switch on `MSG_*` type, read payload. For
`MSG_DATA`, the payload is delta tokens or literal bytes that flow
straight into the receiver pipeline's SPSC channel. For control
messages (`MSG_STATS`, `MSG_DONE`), the payload is short and
non-blocking.

**Async win**: dispatch itself is CPU-bound and fits in microseconds;
async makes no difference. The `MSG_DATA` payload read benefits from
the same overlap as section 6.2. The control-message reads are
neutral.

### 6.4 Where the overlap does *not* show up

- The SPSC pipeline (`crates/transfer/src/pipeline/spsc.rs`). It
  spin-waits in userspace; it stays sync forever (migration plan R3).
  Any attempt to drive it from a tokio worker starves the runtime.
- The delta apply (`crates/engine/src/`). Pure CPU. Async is neutral
  or negative (scheduling overhead per token).
- The disk-commit thread (`crates/transfer/src/disk_commit/`). Stays
  sync. Async would add a `spawn_blocking` hop per commit, which is
  net negative.

The async surface stops at the bidirectional pump. Everything
downstream stays sync. This boundary is identical to the one
`async-ssh-evaluation.md` section 4.1 commits to; this section names
the upstream side of it.

## 7. Bench plan

The transport eval (`async-ssh-transport.md` section 5) and the pipe
wrapper (`async-ssh-pipe-wrapper.md` section 5) both define bench
matrices. This document does not duplicate them. It adds two probes
that target the runtime-shape question this doc is about.

### 7.1 Runtime-flavour comparison

For each row in the pipe-wrapper bench matrix
(`async-ssh-pipe-wrapper.md` section 5), run three variants:

1. Sync baseline (today).
2. Async with option (a): shared `rt-multi-thread` runtime.
3. Async with option (b): per-connection `current_thread` runtime.

Capture wall clock, RSS, and thread count. The acceptance criteria
from the wrapper doc apply unchanged to variant 2 vs variant 1; we add
a fourth criterion: variant 3 must not regress variant 2 by more than
5% on the fan-out row, and must beat variant 2 by at least 50% on RSS
on the single-connection CLI rows. If those hold, the hybrid
recommendation in section 4.3 is justified.

### 7.2 Bridge-cost probe

On the subprocess path, micro-benchmark a 1 MiB round-trip through
`tokio::process::ChildStdin`/`ChildStdout` vs the `AsyncFd`-over-raw-FD
construction (section 5.1). Goal: confirm that the official
`tokio::process` path is within 5% of the hand-rolled `AsyncFd` path.
If it is, section 5.4's recommendation stands without an escape
hatch. If `tokio::process` is materially slower, the escape hatch
becomes a real fork, not a documented option.

## 8. Recommendation

Adopt the hybrid:

- **Daemon**: option (a). Shared `rt-multi-thread` runtime hosting the
  accept loop (already #1935) and the async SSH pump. Composes
  cleanly with rayon via the `spawn_blocking` bridge (#1751). Pays the
  2 MiB RSS cost once; amortises it across all connections.
- **CLI**: option (b). Per-invocation `current_thread` runtime scoped
  to the SSH transport. Sync engine remains untouched. Pays ~200 KiB
  RSS overhead per invocation. The runtime is owned by the
  SSH-transport composition entry point (#1795), not by `main`.

Bridges:
- Embedded russh: `russh::ChannelStream` halves (section 5.2).
- Subprocess: `tokio::process::ChildStdin`/`ChildStdout` (section 5.3).
- `AsyncFd` raw-FD bridge (section 5.1): documented escape hatch,
  unimplemented unless section 7.2 forces it.

Promotion default: stay deferred. The synchronous transport remains
the default. Trigger conditions for flipping `async-ssh` on by
default, all of which must hold simultaneously:

1. #1935 (async daemon listener) has shipped and proven stable in
   production for at least one release cycle.
2. The embedded russh async surface (#1796, #1797) has shipped and
   covers the OpenSSH-parity matrix in `async-ssh-evaluation.md`
   open question 1.
3. The pipe-wrapper bench in `async-ssh-pipe-wrapper.md` section 5
   clears the >= 10% wall-clock gate on at least one supported corpus
   without LAN regression.
4. The runtime-flavour comparison (section 7.1 above) confirms the
   hybrid recommendation.
5. The bridge-cost probe (section 7.2) confirms `tokio::process`
   stdio is within 5% of the `AsyncFd` raw-FD path, so no escape
   hatch is required.

If any one of those fails, async SSH stays opt-in behind
`--features async-ssh` and `RSYNC_ASYNC_SSH=1`.

## 9. Five-step sequencing

The implementation order, with the gate between each step:

1. **Land the runtime-ownership contract.** Define the
   `AsyncSshTransport` trait surface in
   `crates/rsync_io/src/ssh/` that takes a `tokio::runtime::Handle`
   (option a) or constructs a private `current_thread` runtime
   (option b) at composition time. No I/O changes yet. Gate: the
   trait compiles under `--no-default-features` (it is feature-gated
   on `async-ssh`) and the existing sync `SshConnection` types pass a
   parity test that exercises the trait's sync escape hatch. Tracking
   alongside #1795.
2. **Lift the embedded russh surface.** Remove the `block_on` shim at
   `crates/rsync_io/src/ssh/embedded/connect.rs:112`. Expose
   `ChannelStream` halves via the trait from step 1. Gate: the
   embedded-ssh feature builds, the interop harness against upstream
   3.4.1 passes over an embedded-ssh client, and the bench from
   section 7.1 row 1 shows no regression vs the existing
   `embedded-ssh` sync facade. Tracking alongside #1796 / #1797.
3. **Add the async subprocess transport.** Implement
   `AsyncSshConnection` over `tokio::process::Child`. Collapse the
   stderr drain and connect watchdog into `tokio::select!` /
   `tokio::time::timeout` arms. Gate: full transport-level interop
   parity with the sync subprocess transport; bench section 7.1
   variants 2 and 3 land within the wrapper-doc gates. Tracking
   alongside #1806.
4. **Wire the bidirectional pump.** Switch
   `crates/core/src/client/remote/daemon_transfer/orchestration/`
   from the thread-per-half copy to `tokio::io::copy_bidirectional`
   over the async halves, gated by `--features async-ssh`. Gate:
   the fan-out bench from `async-ssh-pipe-wrapper.md` section 5
   shows the predicted thread-count collapse (sublinear in connection
   count) and at least one workload row clears the >= 10%
   wall-clock gate. Tracking alongside #1797.
5. **Bench and decide on default promotion.** Run the full matrix:
   wrapper-doc section 5 plus this doc's section 7. If all five
   trigger conditions in section 8 hold, flip `async-ssh` to default
   in the next major release; otherwise keep it opt-in and document
   which trigger failed. Tracking alongside #1889.

The sequencing is strictly serial. Each step lands its own gate
before the next begins. If step 2 or 3 fails its gate, the async
path stops; the synchronous transport remains the supported default
and this document records the reason for the stop.
