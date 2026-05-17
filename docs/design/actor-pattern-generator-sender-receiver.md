# Actor Pattern for Generator / Sender / Receiver (#2136)

Status: Design.
Audience: maintainers of `crates/transfer`, `crates/engine`, and the
async migration working group.
Scope: evaluate whether the three rsync wire-protocol roles - Generator,
Sender, Receiver - should be modelled as supervised `tokio::spawn`-ed
tasks (Erlang / Akka actor style: bounded mailboxes, one supervisor
that restarts crashed actors) once the async migration plan
(`docs/design/async-migration-plan.md`, #1594) lands.

The migration plan commits the project to tokio for daemon accept,
SSH transport, and the receiver pipeline. The natural follow-on
question is whether the rsync session itself should be reshaped as a
supervision tree of actors. This note answers that question.

**Recommendation: reject for the production hot path; adopt the
actor surface only for the multi-host fan-out driver and for
fault-injection tests, behind `--features async-pipeline`.** Section 5
spells out the rationale.

## 1. Current shape - blocking threads, not actors

The three roles are reified in `crates/transfer/` and call into
`crates/engine/`. They are blocking OS threads connected by
hand-tuned synchronous channels. No tokio task surface, no inbox,
no supervisor.

### 1.1 The three roles today

| Role | Entry point | Upstream cite |
|------|-------------|---------------|
| Generator | `Generator::run` at `crates/transfer/src/generator/transfer.rs:731`; per-loop body at `:48` (`run_transfer_loop`) | `generator.c:2226 generate_files()` |
| Sender (server-side, paired with Generator) | shares the Generator entry; `crates/transfer/src/generator/mod.rs:32-78` documents the role split | `sender.c:199 send_files()` |
| Receiver | `ReceiverContext::run` at `crates/transfer/src/receiver/transfer.rs:55`; pipelined variants at `:519`, `:680` | `receiver.c:720 recv_files()` |

The repository does not have a dedicated `sender` module: the local
side that walks the file tree, sends the file list, services NDX
requests, and emits delta data is named `generator` after upstream's
process role split. Upstream's generator child forks off a sender
child; oc-rsync runs the two halves on a single OS thread because
Rust is single-process, single-binary.

### 1.2 OS threads in flight on a steady-state transfer

Per `docs/architecture/parallelization.md:104-114` and the spawn
sites below, a remote transfer uses up to four OS threads:

1. **Generator/Sender thread** - local process when running as
   sender. Walks the local tree (`crates/transfer/src/generator/file_list/walk.rs`),
   sends the file list, reads NDX requests + signatures, emits
   deltas. Single-threaded by protocol order. Entry:
   `crates/transfer/src/generator/transfer.rs:731` (`Generator::run`).
2. **Receiver network thread** - local process when running as
   receiver, or the local generator thread when remote is the sender.
   Reads delta tokens from the wire and produces `FileMessage`
   items into the SPSC channel. Entry:
   `crates/transfer/src/receiver/transfer.rs:519`
   (`run_pipelined`) and `:680` (`run_pipelined_incremental`).
3. **Disk-commit thread** - dedicated OS thread spawned at
   `crates/transfer/src/disk_commit/thread.rs:47-56`
   (`spawn_disk_thread`), named `"disk-commit"`. Owns all file
   I/O on the receive path: temp-file create, write, fsync,
   atomic rename, metadata application. Main loop at
   `crates/transfer/src/disk_commit/thread.rs:172-234`
   (`disk_thread_main`).
4. **Rayon worker pool** - shared across the workspace. Used for
   parallel `stat` (`crates/transfer/src/parallel_io.rs:124`),
   parallel signature generation
   (`crates/signature/src/parallel.rs:84,207`), and parallel
   directory metadata application. Threshold-gated
   (`PARALLEL_STAT_THRESHOLD = 64`,
   `PARALLEL_THRESHOLD_BYTES = 256 KB`;
   `docs/architecture/parallelization.md:84-89`).

The engine-side companions are blocking too: the concurrent-delta
work queue (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:88-104`)
uses `crossbeam_channel::bounded` with capacity
`2 * rayon::current_num_threads()`; the work-stealing rayon pool
runs the strong-checksum compute.

### 1.3 Inter-thread plumbing today

The hot-path channels are uniformly synchronous and deliberately
avoid park/wake costs:

- **Network -> disk-commit**: lock-free SPSC at
  `crates/transfer/src/pipeline/spsc.rs:1-15`, capacity 128 slots
  (`DEFAULT_CHANNEL_CAPACITY`). Spin-wait on
  `crossbeam_queue::ArrayQueue`; zero syscalls in the steady state.
- **Disk-commit -> network (commit results)**: second SPSC,
  capacity 256 slots. Returns `io::Result<CommitResult>` per file.
- **Disk-commit -> network (buffer return)**: third SPSC,
  capacity 256 slots. Recycles `Vec<u8>` write buffers to amortise
  allocation.
- **Local-copy delta work queue**: bounded
  `crossbeam_channel::bounded`
  (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:88-104`).
  Single-producer (the wire reader), multi-consumer (rayon pool).

No channel currently crosses an async boundary on the hot path.
The async surfaces enumerated in
`docs/design/async-migration-plan.md:62-95` are feature-gated and
do not run in the production transfer loop.

### 1.4 Failure model today

- An I/O error in any role returns `Err` from `Generator::run` /
  `ReceiverContext::run`, which propagates up through the
  orchestrating `core::session()` call.
- The disk-commit thread reports failures through its result SPSC
  and is joined by the receiver. No restart.
- Panics abort the whole transfer; there is no supervisor that
  catches a panicking role and tries again.

This is upstream-faithful: rsync 3.4.1's process model also has
no restart story. If `generate_files` or `recv_files` dies, the
session dies.

## 2. Actor sketch - tokio tasks, mailboxes, one supervisor

The canonical actor reshape, in the tokio idiom the migration plan
already commits us to, would look like this:

```text
TransferSupervisor (tokio::spawn)
        |
        +---- Generator actor (tokio::spawn)
        |       inbox: mpsc::Receiver<GeneratorMsg>
        |       state: file list cursor, NDX window, signature buffer
        |
        +---- Sender actor (tokio::spawn)
        |       inbox: mpsc::Receiver<SenderMsg>
        |       state: delta emitter, basis cache, token buffer
        |
        +---- Receiver actor (tokio::spawn)
                inbox: mpsc::Receiver<ReceiverMsg>
                state: temp file map, in-flight window, commit join handle
```

### 2.1 Message surface

Each role is reachable only through a typed enum on a bounded
inbox.

```rust
pub enum GeneratorMsg {
    ReceivedNdx(NdxIndex),
    ReceivedSignature(SumHead, Vec<BlockChecksum>),
    PhaseRedo,
    Cancelled,
    Shutdown,
}

pub enum SenderMsg {
    ScheduleFile { ndx: NdxIndex, basis: BasisHandle },
    DeltaBudgetReplenished,
    Cancelled,
}

pub enum ReceiverMsg {
    DeltaToken(DeltaToken),
    FileCommitted(NdxIndex, CommitResult),
    DiskError(io::Error),
    Cancelled,
    Shutdown,
}
```

The actor body is a `loop { match inbox.recv().await { ... } }`.
Every send is `inbox.send(msg).await`, which yields when the
mailbox is full, giving structured backpressure for free.

### 2.2 The supervisor

`TransferSupervisor` owns the three `JoinHandle`s and a
`CancellationToken` (the daemon side already pulls in
`tokio_util::sync::CancellationToken`; reuse). It selects on:

- each actor's `JoinHandle` completing (clean exit, error, or
  panic),
- the parent cancellation token firing,
- a timeout for hung actors.

On a non-panic error, the supervisor logs and propagates the
error to the orchestrator. On a panic, the supervisor would
*notionally* restart the actor; section 4 explains why "restart
the Generator" is not a real recovery for this protocol.

The disk-commit thread already looks like an actor in shape
(`crates/transfer/src/disk_commit/thread.rs:172-234`): it owns
its state, takes typed `FileMessage` over a channel, returns
typed `io::Result<CommitResult>`. The only missing element is
an explicit supervisor that can cancel it cooperatively rather
than by dropping the SPSC ends.

### 2.3 Where the bridge to sync would sit

The receiver pipeline already integrates a
`CancellationToken` in the feature-gated async path at
`crates/transfer/src/pipeline/async_pipeline.rs:151-155`. The
rayon CPU work and the disk-commit thread would stay sync and
be reached from the actors via `tokio::task::spawn_blocking`
following the contract pinned in
`docs/design/spawn-blocking-bridge.md` (#4196). No change to
the engine's compute model: the actor is a wire driver, not a
compute pool.

## 3. Pros - what this shape would buy

### 3.1 Clean failure isolation

Today, a panic in any of the three roles aborts the whole
process. With the supervisor, a panic in the Sender (for
example, a malformed basis-cache invariant) can be caught,
logged with a stable error trailer, and surfaced as a typed
session error rather than a process abort. The supervisor is
the single place to integrate with `core::error` and the role
trailers (`[sender]`, `[receiver]`, `[generator]`) already in
the codebase.

### 3.2 Structured mailbox backpressure

`mpsc::Sender::send().await` yields when the mailbox is full.
Today, the receiver's pipeline uses a hand-tuned spin-wait on
`crossbeam_queue::ArrayQueue` (`pipeline/spsc.rs:1-15`). The
spin-wait is faster in the steady state but does not compose
with other async work the future fan-out driver might want to
schedule. A bounded mpsc gives uniform backpressure semantics
across the runtime - useful for the multi-host case in section 5.

### 3.3 Per-actor observability

`ActorMsg` traffic is traceable end-to-end: every message that
enters or leaves the mailbox can be logged with `tracing`
spans. The existing `PhaseTimer` macros and role trailers in
`crates/transfer/src/error.rs` already partition by role; the
actor surface would let us partition by *message kind* within
the role, which is finer than today's `PhaseTimer`.

### 3.4 Test-mode fault injection

Today, simulating a stuck disk thread or a malformed delta
requires monkey-patching the SPSC ends. With a typed message
surface, a test can substitute a mock `Receiver` that emits
`ReceiverMsg::DeltaToken` patterns the wire would not normally
produce, exercising the Generator's error handling without
driving a real transfer. This is a real improvement over the
current test ergonomics (`crates/transfer/tests/`,
approximately 60 integration tests).

### 3.5 Multi-host fan-out becomes natural

A future driver that wants to run N concurrent rsync transports
against N hosts (the batch use case sketched at
`docs/audits/async-ssh-transport.md:232-242`) needs a
per-connection supervisor that owns one Generator + one Receiver
actor pair. The fan-out is an async-runtime problem - exactly
what the migration plan's later phases anticipate.

## 4. Cons - what this shape would cost

### 4.1 "Restart on crash" does not exist for this protocol

This is the load-bearing objection.

Rsync's wire protocol is a single ordered conversation. The
Generator's state includes:

- the current file-list cursor (in INC_RECURSE mode, an open
  segment generator),
- the NDX window of in-flight signature requests,
- the phase-1 / phase-2 toggle (`SHORT_SUM_LENGTH` vs
  `MAX_SUM_LENGTH`; `crates/signature/src/block_size.rs`),
- the multiplex frame state (`MSG_*` framing on the wire),
- any partial varint mid-decode.

If the Generator panics mid-transfer, none of that state is
reconstructible from the peer. The peer has no protocol-level
"please rewind to NDX 17 and resume": it speaks rsync 3.4.1,
which has no resume primitive inside a session. The only
correct recovery is to tear the whole session down and
reconnect, which is *exactly* what `Err` propagation does
today.

So the supervisor's "restart the actor" capability, the
defining feature of the actor pattern as Erlang/Akka use it,
is a no-op for the Generator and Receiver. The supervisor
can only do one useful thing on failure: cancel the other
two actors, drain the disk-commit thread, and surface the
error. That is a `Result::Err` with three lines of select!,
not an actor framework.

### 4.2 Supervision tree overhead at zero throughput gain

The wire is sequential by protocol design
(`docs/architecture/parallelization.md:50-90`,
`docs/audits/async-ssh-transport.md:270-299`; task #1197
"single-threaded wire protocol limitation", status: done). An
actor refactor that puts Generator, Sender, and Receiver on
separate cooperative tasks does not change how many bytes the
wire can move per second. It only changes how the existing
bytes are scheduled.

Concretely:

- The SPSC pipeline costs zero syscalls in the steady state.
- `tokio::sync::mpsc` costs a park/wake per cross-task send
  when the receiver is parked, plus the runtime's work-stealing
  bookkeeping (`docs/design/async-migration-plan.md:179-185`).
- The disk-commit boundary is the only place the topology
  currently benefits from a queue; replacing the SPSC with an
  mpsc adds wake-up cost for no throughput gain.

### 4.3 Complicates the state machines #2134 just covered

`docs/design/type-state-protocol-phases.md` (#2134, recently
landed) makes the explicit recommendation that within-phase
state machines stay as runtime enums while only the
*phase-boundary* invariants are encoded in the type system.
That recommendation is bounded by exactly the same constraint
that bounds this note: rsync's protocol stages are sequential,
not parallel, and the within-phase data flow is
multi-threaded on the existing topology (sender thread + disk
thread + rayon pool).

An actor reshape would put the within-phase data flow on the
typed-message surface (`GeneratorMsg`, `ReceiverMsg`) while
the phase-boundary surface stays type-state. Now we have *two*
state-machine encodings for the same role: a type-state
on the phase axis and a message-enum on the within-phase
axis, and any phase transition has to thread through both.
#2134 deliberately avoided that combinatorial cost.

### 4.4 Code-base churn

- **Call-site reshape**: every site that today does
  `generator.process_signature(buf)` becomes
  `tx.send(GeneratorMsg::Signature(buf)).await`. This ripples
  through `crates/transfer/src/generator/protocol_io.rs`,
  `crates/transfer/src/transfer_ops/`,
  `crates/transfer/src/receiver/transfer/`,
  `crates/transfer/src/receiver/wire.rs`. Order-of-magnitude:
  three to four hundred call sites.
- **Channel allocation**: each actor needs its inbound queue;
  the hand-tuned SPSC trio at `pipeline/spsc.rs` would either
  stay (and we get a hybrid mpsc-plus-SPSC topology) or be
  replaced (and we lose the zero-syscall steady state).
- **Cancellation plumbing**: typed cancellation is the actor
  pattern's clear win, but the sync path cancels via
  `Result::Err` propagating out of `run`; generalising the
  feature-gated `CancellationToken` to the sync hot path is
  net new plumbing.
- **Test-suite churn**: integration tests that drive the
  receiver via `ReceiverContext::run` would need
  message-passing harnesses. Approximately 60 tests under
  `crates/transfer/tests/` and `tests/`.

### 4.5 Distributor minimal-build path

`docs/audits/tokio-dependency-boundary-2026.md` defines the
seven-crate tokio allow-list any new async surface must
respect. A default-on actor refactor would pull tokio onto the
sync hot path, breaking the
`--no-default-features` tokio-free build distributors rely on.

## 5. Recommendation - opinionated

### 5.1 Reject the actor pattern for the production hot path

The Generator, Sender, and Receiver as they exist today are
the minimum threading surface the wire protocol allows: one OS
thread per direction plus one disk-commit thread. The
disk-commit boundary is the only place the topology benefits
from a queue, and the SPSC already gives that boundary a
zero-syscall implementation. The supervisor's defining feature
(restart on crash) is a no-op against a sequential protocol
with no resume primitive (section 4.1). The wire-throughput
ceiling is set by protocol order, not by scheduling
(section 4.2; #1197). Two state-machine encodings for the same
role contradicts the bounded type-state recommendation #2134
just landed (section 4.3).

**Do not reshape the production hot path as actors.** The sync
hot path stays as documented in
`docs/architecture/parallelization.md` and in the parallelism
sections of `docs/design/async-migration-plan.md:170-185`.

### 5.2 Adopt actors for two narrow use cases, behind a feature flag

Two scenarios benefit from a typed actor surface; neither is
served by today's topology and neither runs on the production
hot path:

1. **Multi-host fan-out from one driver process.** A future
   command-line or daemon driver that wants N concurrent rsync
   sessions against N hosts wants a per-session supervisor and
   a top-level supervisor over those. The actor shape is the
   natural fit; the migration plan's phase 5 anticipates this
   under the same `#2136` tracker.
2. **Fault-injection tests.** A typed message surface lets a
   test substitute a mock `Receiver` or a mock `Sender` that
   emits patterns the real wire would not produce. This is a
   real ergonomics win over the current SPSC monkey-patching.

The proposed shape:

- New crate-internal trait `ActorRole<Msg, Out>` exposing
  `start(supervisor)`, `send(Msg)`, `try_recv() -> Out`.
- Three concrete impls behind `--features async-pipeline`:
  `GeneratorActor`, `SenderActor`, `ReceiverActor`. Each impl
  wraps the existing `Generator::run` /
  `ReceiverContext::run_pipelined` and translates between the
  typed message channel and the existing API.
- `TransferSupervisor` owns the actor handles and propagates
  cancellation through `tokio_util::sync::CancellationToken`.
  Engine compute stays sync and is reached via
  `spawn_blocking` per the bridge contract in
  `docs/design/spawn-blocking-bridge.md` (#4196).
- Feature flag default: off. Pulls the same tokio surface as
  `--features async`. Sits as phase 4.5 in the migration plan
  ordering, or as a compose-only feature under the umbrella
  `--features async-default` (phase 5).

### 5.3 What stays out of scope

- Any wire-protocol change that would let an actor mesh
  exploit out-of-order delivery. Shelved at
  `docs/plans/2026-03-28-parallel-chunks-design.md`
  (SHELVED 2026-03-28) and forbidden by user policy (no wire
  protocol features for narrow perf wins).
- Replacing rayon with an actor-based work scheduler. Rayon's
  work-stealing pool dominates the threshold-gated CPU paths
  (`docs/architecture/parallelization.md:84-89`); actors are
  the wrong abstraction for fan-out CPU compute.
- Splitting the SPSC into N actor-style mailboxes. The single
  consumer is correct by construction
  (`crates/transfer/src/pipeline/spsc.rs:67-94`); multiplying
  it multiplies syscalls.
- Converting the disk-commit thread into an actor. It already
  is one in shape (typed inbox, typed outbox, owned state);
  the only thing missing is a name. If the actor surface lands
  for the multi-host case, the disk-commit thread can be
  reskinned as an actor for uniformity, but it is not a
  prerequisite.

## 6. Cross-references

Async / runtime plan:

- `docs/design/async-migration-plan.md` (#1594, PR #4186) -
  phase 4 receiver pipeline default and phase 5 rayon-tokio
  composition; the natural carrier for an actor surface.
  Issue #2136 is itemised there as the actor-pattern session
  model under phase 5.
- `docs/design/spawn-blocking-bridge.md` (#1751, PR #4196) -
  the bridge contract any actor that touches rayon or io_uring
  must respect.
- `docs/design/async-channel-abstraction.md` (#1591) - the
  `TransferChannel` trait the actor surface would reuse for
  sync/async bridges.
- #1935 - async daemon listener implementation. First
  production-bound tokio surface; precedent for how the
  supervisor would attach in the daemon mode of `oc-rsync`.

State-machine adjacency:

- `docs/design/type-state-protocol-phases.md` (#2134, recently
  merged) - the bounded type-state recommendation that
  constrains how much within-phase machinery should be encoded
  in the type system. Section 4.3 above shows why an actor
  reshape would contradict that bound.

Architecture and audits:

- `docs/architecture/parallelization.md:50-90,93-141` -
  parallelism inventory and the wire-protocol single-thread
  invariant; the load-bearing constraint for this note.
- `docs/audits/async-ssh-transport.md:270-299` - cites task
  #1197; bounds async-transport gains by the same wire
  constraint.
- `docs/audits/tokio-dependency-boundary-2026.md` - the
  seven-crate tokio allow-list any actor feature must respect.

Multi-host carrier candidates:

- `docs/design/arc-wrapped-worksender-multi-producer.md` - the
  multi-producer shape that complements multi-host fan-out.
- `docs/design/multi-producer-workqueue.md` - design A (vector
  of senders) versus design B (Arc-shared sender), feeds the
  multi-host actor case.

Source citations:

- Generator role entry:
  `crates/transfer/src/generator/transfer.rs:731` (`Generator::run`);
  per-loop body at `:48` (`run_transfer_loop`).
- Receiver role entry:
  `crates/transfer/src/receiver/transfer.rs:55` (`run`),
  `:519` (`run_pipelined`), `:680`
  (`run_pipelined_incremental`).
- Disk-commit thread spawn:
  `crates/transfer/src/disk_commit/thread.rs:47-56`
  (`spawn_disk_thread`); main loop:
  `crates/transfer/src/disk_commit/thread.rs:172-234`
  (`disk_thread_main`).
- Lock-free SPSC: `crates/transfer/src/pipeline/spsc.rs:1-120`.
- Sender-side INC_RECURSE state-machine narrative:
  `crates/transfer/src/generator/mod.rs:32-78`.
- Concurrent-delta work queue:
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs:88-104`.

Tracked tasks:

- #1197 - single-threaded wire protocol limitation: done.
- #1591 - channel abstraction.
- #1594 / PR #4186 - async migration plan.
- #1751 / PR #4196 - spawn_blocking bridge contract.
- #1935 - async daemon listener implementation.
- #2134 - type-state for protocol negotiation phases: merged.
- #2136 - this note.
