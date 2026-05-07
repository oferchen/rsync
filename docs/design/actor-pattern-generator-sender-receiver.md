# Actor Pattern for Generator / Sender / Receiver Roles (#2136)

## 1. Scope

This note evaluates whether the three rsync wire-protocol roles -
Generator, Sender, Receiver - should be expressed as supervised actors
with typed message channels, instead of the current mix of dedicated OS
threads, lock-free SPSC channels, and inline rayon parallelism. The
question is not "is the actor pattern attractive in the abstract" but
"does the wire protocol leave any throughput on the table that an actor
model would unlock, and at what code-base cost."

The conclusion this note arrives at is: the protocol-bound
single-threaded ordering documented at task #1197
(`docs/architecture/parallelization.md:50-90,93-141`) bounds the wins
from any concurrency reshape. An actor refactor of the production hot
path is not justified. A bounded actor surface, gated behind a build
feature `async-pipeline`, is justified for two scenarios that the
current topology does not serve well: multi-host fan-out and
test-only fault injection. Section 6 lays out that recommendation.

## 2. Current Threading Model

### 2.1 The three roles

The role boundaries follow upstream rsync's process model
(`generator.c`, `sender.c`, `receiver.c`) and are reified in
`crates/transfer/src/`:

| Role | Module | Entry point | Upstream cite |
|------|--------|-------------|---------------|
| Generator | `crates/transfer/src/generator/mod.rs:1-89` | `Generator::run` at `generator/transfer.rs:696` | `generator.c:2226 generate_files()` |
| Sender (server-side, paired with Generator) | `crates/transfer/src/generator/transfer.rs:10` | shares the Generator entry; the local process owns both halves | `sender.c:199 send_files()` |
| Receiver | `crates/transfer/src/receiver/mod.rs:99-100` | `ReceiverContext::run` at `receiver/transfer.rs` | `receiver.c:720 recv_files()` |

The repository does not have a dedicated `sender` module: the local
side that walks the file tree, sends the file list, services NDX
requests, and emits delta data is named `generator` after upstream's
process role split. In the upstream split-process layout, the
generator child forks off a sender child; in oc-rsync the two roles
share an OS thread because Rust is single-process, single-binary.

### 2.2 OS threads currently in flight

Per the parallelization architecture
(`docs/architecture/parallelization.md:104-114`) and the spawn sites
below, a steady-state remote transfer uses up to four OS threads:

1. **Generator/Sender thread** - the local process when running as
   sender. Walks the local tree (`generator/file_list/walk.rs`),
   sends the file list, reads NDX requests + signatures, emits
   deltas. Single-threaded by protocol order. Cite
   `generator/transfer.rs:696` (`Generator::run`).
2. **Receiver network thread** - the local process when running as
   receiver, or the local generator thread when remote is the sender.
   Reads delta tokens from the wire and produces `FileMessage` items
   into the SPSC channel. Cite
   `crates/transfer/src/receiver/transfer/pipeline.rs:38`
   (`run_pipeline_loop_decoupled`).
3. **Disk-commit thread** - dedicated OS thread spawned at
   `crates/transfer/src/disk_commit/thread.rs:53-56` with name
   `"disk-commit"`. Owns all file I/O on the receive path: temp-file
   create, write, fsync, atomic rename, metadata application.
4. **Rayon worker pool** - shared across the workspace. Used for
   parallel `stat` (`crates/transfer/src/parallel_io.rs:124`),
   parallel signature generation
   (`crates/signature/src/parallel.rs:84,207`), and parallel
   directory metadata application. Threshold-gated:
   `PARALLEL_STAT_THRESHOLD = 64`,
   `PARALLEL_THRESHOLD_BYTES = 256 KB`
   (`docs/architecture/parallelization.md:84-89`).

### 2.3 Channels between the threads

The hot-path inter-thread plumbing is uniformly synchronous and
deliberately avoids park/wake costs:

- **Network -> disk-commit**: lock-free SPSC at
  `crates/transfer/src/pipeline/spsc.rs:1-15`, capacity 128 slots
  (`disk_commit.rs DEFAULT_CHANNEL_CAPACITY`). Spin-wait on
  `crossbeam_queue::ArrayQueue`; zero syscalls.
- **Disk-commit -> network (commit results)**: second SPSC, capacity
  256 slots. Returns `io::Result<CommitResult>` per file.
- **Disk-commit -> network (buffer return)**: third SPSC, capacity
  256 slots. Recycles `Vec<u8>` write buffers to amortise allocation.
- **Local-copy delta work queue**: bounded
  `crossbeam_channel::bounded` with capacity
  `2 * rayon::current_num_threads()`
  (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:88-104`).
  Single-producer (the wire reader), multi-consumer (rayon pool).

No channel currently crosses an async boundary on the hot path. The
async surfaces enumerated in `docs/design/async-migration-plan.md:62-95`
are feature-gated and do not run in the production transfer loop.

### 2.4 What the topology already exploits

- **Wire I/O parallel with disk I/O**: receiver thread reads while
  disk-commit writes the previous file, gated by the SPSC.
- **CPU work batched off the wire-critical path**: signature
  generation and quick-check stats run on rayon when the batch is
  large enough; short bursts run inline.
- **In-flight request window**: the pipelined receiver fills a
  sliding window of file requests
  (`run_pipeline_loop_decoupled`) so each send/receive amortises
  round-trip latency across many small files.

## 3. The Single-Threaded Wire Constraint

`docs/architecture/parallelization.md:50-90` and
`docs/audits/async-ssh-transport.md:270-299` already document this.
Restated for completeness:

- The protocol assigns each file a sequential index. The sender
  emits deltas in that order; the receiver acknowledges in that
  order; the generator processes acks in that order
  (`docs/architecture/parallelization.md:117-122`).
- The network-facing part of each role is single-threaded by design
  (`docs/architecture/parallelization.md:122`).
- Out-of-order delivery would require a wire-protocol extension and
  is not compatible with upstream rsync 3.4.1
  (`docs/architecture/parallelization.md:135-137`).
- Task #1197 ("Document single-threaded wire protocol pipeline
  limitation", status: done) is the policy anchor;
  `docs/audits/async-ssh-transport.md:296-299` cites it as the
  reason async transport I/O cannot unlock new wire-level
  parallelism.

This bounds every concurrency reshape proposal. An actor refactor
that puts Generator, Sender, and Receiver on separate cooperative
tasks does not change how many bytes the wire can move per second.
It only changes how the existing bytes are scheduled.

## 4. Actor Pattern: What It Is and What It Would Cost

### 4.1 Definition for this note

By "actor" this note means the canonical trio:

1. **Identity**: each role is owned by one task; the task is the only
   site that can mutate the role's state.
2. **Typed message channel**: callers speak to the role through an
   enum of input messages, not through `&mut Role` calls. The trio
   above would expose `GeneratorMsg`, `SenderMsg`, `ReceiverMsg`.
3. **Supervised lifecycle**: a parent supervisor starts the actors,
   propagates cancellation, and observes failure (analogous to
   Erlang's `link` / `monitor`).

The async runtime is incidental; the pattern works on OS threads or
on tokio. In oc-rsync's workspace, the realistic vehicles would be
either `crossbeam_channel::Sender<RoleMsg>` driving a thread per
role, or `tokio::sync::mpsc::Sender<RoleMsg>` driving a tokio task
per role.

### 4.2 What changes vs today

Mapping the trio onto the current code:

- The Generator's `run` method
  (`generator/transfer.rs:696`) becomes a `loop { match rx.recv()? }`
  over `GeneratorMsg::{ReceivedNdx, ReceivedSignature, Cancelled,
  Shutdown}`.
- The Sender (the half of `Generator::run` that produces deltas)
  becomes a separate task driven by `SenderMsg::{ScheduleFile,
  Cancelled}`.
- The Receiver's `run_pipeline_loop_decoupled`
  (`receiver/transfer/pipeline.rs:38`) becomes a `loop` over
  `ReceiverMsg::{DeltaToken, ReceivedFile, Cancelled, Shutdown}`.
- The disk-commit thread already looks like an actor in shape
  (`disk_commit/thread.rs:172-234`): it owns its state, takes
  typed `FileMessage` over a channel, returns typed
  `io::Result<CommitResult>`. The only missing element is an
  explicit supervisor that can cancel it cooperatively rather than
  by dropping the SPSC ends.

### 4.3 Concrete cost

- **Code reshape**: every call site that today does
  `generator.process_signature(buf)` becomes
  `tx.send(GeneratorMsg::Signature(buf)).await`. This ripples
  through `crates/transfer/src/generator/protocol_io.rs`,
  `crates/transfer/src/transfer_ops/`,
  `crates/transfer/src/receiver/transfer/`,
  `crates/transfer/src/receiver/wire.rs`. Order-of-magnitude:
  three to four hundred call sites.
- **Channel allocation**: each actor needs its inbound queue.
  The current SPSC trio at `pipeline/spsc.rs` is hand-tuned; an
  actor model usually wants `flume::bounded` or
  `tokio::sync::mpsc::channel`, both of which add park/wake
  costs the SPSC explicitly avoids
  (`pipeline/spsc.rs:1-15`,
  `docs/design/async-migration-plan.md:179-185`).
- **Cancellation plumbing**: typed cancellation is the actor
  pattern's clear win, but the receiver's existing
  `CancellationToken` integration in
  `crates/transfer/src/pipeline/async_pipeline.rs:151-155` is
  feature-gated. Generalising it to the sync hot path is a net
  add of plumbing the sync path does not currently need (the
  sync path cancels via `Result::Err` propagating out of `run`).
- **Test-suite churn**: every integration test that drives the
  receiver via `ReceiverContext::run` would need to switch to
  message-passing harnesses. Approximately 60 integration tests
  under `crates/transfer/tests/` and `tests/`.

### 4.4 What it does not buy

- **No new wire-level parallelism.** Section 3 holds: an actor
  cannot serve files out of order without breaking the protocol.
- **No lower throughput floor.** The disk-commit channel already
  decouples wire from disk; that is the only cross-thread hop
  on the hot path. Adding more actor hops adds latency, not
  throughput.
- **No simpler concurrency invariants.** The existing topology
  has exactly one shared mutable surface (the SPSC), bounded by
  capacity, with a single-producer / single-consumer compile-time
  invariant. An actor model multiplies the number of channels
  proportionally to the number of message variants per role.

## 5. Tradeoff Table

| Axis | Current (rayon + dedicated threads + SPSC) | Actor refactor (typed messages, supervised lifecycle) |
|------|-------------------------------------------|--------------------------------------------------------|
| Wire-throughput ceiling | Bounded by protocol order (#1197) | Same; not relaxed by actors |
| Inter-thread channel cost | 0 syscalls (SPSC spin-wait) | Park/wake or task wake per message |
| Cancellation semantics | `Result::Err` propagation, `Drop` of SPSC ends | Typed `Cancelled` message; cooperative |
| Failure propagation | Result returned from `run`; supervisor implicit | Explicit `link` / `monitor` analogue, supervised tree |
| Test-mode fault injection | Hard: requires monkey-patching channels | Easy: substitute a mock actor for any role |
| Multi-host fan-out | Not supported in one process | Natural: spawn one supervisor tree per host |
| Local-copy fast path | Direct call into `engine::local_copy::executor` | Wraps every call in a message; perf regression risk |
| Code-base churn | None (status quo) | ~300-400 call sites, ~60 tests |
| Async runtime dependency | None on hot path | Adds tokio-or-equivalent to the seven-crate set already in `docs/audits/tokio-dependency-boundary-2026.md` |
| Build-graph impact | None | Pulls `flume` or `tokio::sync` onto sync builds unless feature-gated |
| Distributor minimal-build path | `--no-default-features` is tokio-free | Stays tokio-free only if the actor surface is fully feature-gated |
| Crash isolation | Whole transfer aborts on any thread panic | Supervisor can restart a sub-actor with a new file batch |
| Observability | `PhaseTimer` macros plus role trailers in errors | `ActorMsg` traffic is traceable end-to-end |

The wire-throughput row is the load-bearing one. Every other axis is
a code-quality tradeoff; the ceiling row decides whether the rest
matter for production transfers.

## 6. Recommendation

### 6.1 Do not refactor the production hot path

The Generator, Sender, and Receiver as they exist today are the
minimum threading surface the wire protocol allows: one OS thread per
direction plus one disk-commit thread. The disk-commit boundary is
the only place the topology benefits from a queue, and the SPSC
already gives that boundary a zero-syscall implementation. Replacing
this with an actor mesh would add a park/wake cost
(`docs/design/async-migration-plan.md:179-185`) for no throughput
gain.

The sync hot path stays as documented in
`docs/architecture/parallelization.md` and in the parallelization
sections of `docs/design/async-migration-plan.md:170-185`.

### 6.2 Ship actor-as-feature behind `--features async-pipeline`

Two scenarios benefit from an actor surface, neither of which is
served by today's topology:

1. **Multi-host fan-out from one process.** A future driver that
   wants to run N concurrent rsync transports against N hosts (the
   batch use case sketched at
   `docs/audits/async-ssh-transport.md:232-242`) needs a
   per-connection supervisor that owns one Generator + one Receiver
   actor pair. The fan-out is an async-runtime problem, which is
   exactly what `docs/design/async-migration-plan.md` Phase 3 is
   evaluating.
2. **Fault-injection tests.** Today, simulating a stuck disk thread
   or a malformed delta requires monkey-patching the SPSC ends.
   With a typed message surface, a test can substitute a mock
   `Receiver` that emits `ReceiverMsg::DeltaToken` patterns the
   wire would not normally produce, exercising the Generator's
   error handling without driving a real transfer.

The proposed shape:

- New crate-internal trait `ActorRole<Msg, Out>` exposing
  `start(supervisor)`, `send(Msg)`, `try_recv() -> Out`.
- Three concrete impls behind `--features async-pipeline`:
  `GeneratorActor`, `SenderActor`, `ReceiverActor`. Each impl
  wraps the existing `Generator::run` /
  `ReceiverContext::run_pipeline_loop_decoupled` and translates
  between the typed message channel and the existing API.
- Supervisor type `TransferSupervisor` that owns the actor handles
  and propagates cancellation through
  `tokio_util::sync::CancellationToken`, reusing the channel
  primitives already pinned in workspace `Cargo.toml`.
- Feature flag default: off. The feature pulls the same tokio
  surface as `--features async` (the umbrella at
  `Cargo.toml:107`); see
  `docs/design/async-migration-plan.md:480-498` for how to compose
  with the existing `async-daemon` / `async-ssh` /
  `async-transfer` family.

### 6.3 Why feature-gated, not default-on

- **Minimal-binary path stays clean.** Distributors who build with
  `--no-default-features` continue to get a tokio-free oc-rsync
  (`docs/audits/tokio-dependency-boundary-2026.md`).
- **Hot-path SPSC stays unchanged.** The disk-commit boundary is
  not converted to an actor. The SPSC's spin-wait remains the
  network-to-disk hop on the production receiver.
- **Existing async work is the natural carrier.** Phases 2-5 of
  `docs/design/async-migration-plan.md:196-345` already migrate
  daemon accept, SSH transport, and the receiver pipeline behind
  feature flags. The actor surface fits as Phase 4.5 or as a
  separate compose-only feature; either way the umbrella
  `--features async-default` (Phase 5) is the only build that
  enables actors by default.

### 6.4 Out of scope

- Any wire-protocol change that would let an actor mesh exploit
  out-of-order delivery. Tracked-and-shelved at
  `docs/design/parallel-chunks-design.md` (tag: SHELVED 2026-03-28)
  and explicitly forbidden by user policy
  (no wire protocol features for narrow perf wins).
- Replacing rayon with an actor-based work scheduler. Rayon's
  work-stealing pool dominates the threshold-gated CPU paths
  (`docs/architecture/parallelization.md:84-89`); actors are the
  wrong abstraction for fan-out CPU compute.
- Splitting the SPSC into N actor-style mailboxes. The single
  consumer is correct by construction
  (`crates/transfer/src/pipeline/spsc.rs:67-94`); multiplying it
  multiplies syscalls.

## 7. References

Architecture and audits:

- `docs/architecture/parallelization.md:50-90,93-141` -
  parallelism inventory, wire-protocol single-thread invariant.
- `docs/audits/async-ssh-transport.md:270-299` - cites task
  #1197; bounds async-transport gains by the same wire constraint.
- `docs/audits/tokio-dependency-boundary-2026.md` - the
  seven-crate tokio allow-list any actor feature must respect.

Adjacent designs:

- `docs/design/async-migration-plan.md` - Phase 4 receiver pipeline
  default and Phase 5 rayon-tokio composition; the natural carrier
  for an actor surface.
- `docs/design/async-channel-abstraction.md` - the `TransferChannel`
  trait the actor surface would reuse for sync/async bridges.
- `docs/design/arc-wrapped-worksender-multi-producer.md` - the
  multi-producer shape that complements multi-host fan-out.
- `docs/design/multi-producer-workqueue.md` - Design A (vector of
  senders) versus Design B (Arc-shared sender), feeds the
  multi-host actor case.

Source citations:

- Generator role entry: `crates/transfer/src/generator/transfer.rs:696`
  (`Generator::run`).
- Receiver role entry:
  `crates/transfer/src/receiver/transfer/pipeline.rs:38`
  (`run_pipeline_loop_decoupled`).
- Disk-commit thread:
  `crates/transfer/src/disk_commit/thread.rs:47-64`
  (`spawn_disk_thread`), `:172-234` (`disk_thread_main`).
- Lock-free SPSC: `crates/transfer/src/pipeline/spsc.rs:1-120`.
- Sender-side INC_RECURSE state machine narrative:
  `crates/transfer/src/generator/mod.rs:32-78`.

Tracked tasks:

- #1197 - single-threaded wire protocol limitation: done.
- #1591 - channel abstraction: prerequisite for any actor surface
  bridging sync producers and async consumers.
- #1594 - async migration plan: Phase 4 is the natural carrier.
- #2136 - this note.
