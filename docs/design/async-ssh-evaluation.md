# Async Runtime Evaluation for SSH Transport (#1411)

Tracking issue: #1411.

This document is the focused evaluation that backs the SSH-transport
recommendation in `docs/design/async-migration-plan.md`. The migration
plan already commits the workspace to tokio and slots async SSH into
Phase 3; this file does not re-litigate the runtime choice. It analyses
the current SSH path, names what async actually buys, and recommends a
concrete ordering: embedded russh first, async subprocess second.

Sibling docs:

- `docs/design/async-migration-plan.md` (#1594) - the canonical plan.
- `docs/design/async-ssh-transport.md` (#1593) - the bench-gated
  implementation evaluation. This document feeds into that one.
- `docs/design/async-runtime-ssh-eval.md` (#1411 runtime survey) -
  short-form record of why tokio is the only runtime considered.
- `docs/design/tokio-spawn-blocking-rayon.md` (#1751) - the rayon
  bridge surface this evaluation depends on.

## 1. Current SSH path

The default SSH transport spawns the system `ssh` binary as a
subprocess and treats its inherited stdio as the rsync byte stream.
All synchronous, all blocking pipe I/O.

- `crates/rsync_io/src/ssh/builder.rs` constructs the
  `std::process::Command` and calls `spawn()`. No async surface.
- `crates/rsync_io/src/ssh/connection.rs` defines `SshConnection`,
  `SshReader`, and `SshWriter`. The connection owns
  `Arc<Mutex<Option<Child>>>`, `ChildStdin`, `ChildStdout`, and an
  optional stderr drain. Read and write halves wrap the child stdio
  directly and use blocking `Read`/`Write` impls on
  `ChildStdout`/`ChildStdin`.
- `crates/rsync_io/src/ssh/aux_channel.rs` spawns a background
  `std::thread` to drain `ChildStderr`, sized to a 64 KiB rolling
  window. Without this drain the SSH child blocks on its own stderr
  pipe once the OS buffer fills.
- `crates/rsync_io/src/ssh/connection.rs` also arms an optional
  `ConnectWatchdog` `std::thread` that kills the child if the SSH
  greeting does not arrive within the configured timeout.
- `crates/rsync_io/src/ssh/embedded/` is the staging area for a
  russh-based native client. The public entry point
  `connect_and_exec` at `embedded/connect.rs:107` builds a private
  `tokio::runtime::Builder::new_current_thread()` and immediately
  `rt.block_on()`s an internal async implementation. The crate is
  gated behind the `embedded-ssh` cargo feature and is not the
  default production path.

The consumer side of the transport sits in
`crates/core/src/client/remote/`. Subprocess command construction is
under `invocation/`, and the bidirectional pump that copies the
receiver socket onto stdin and stdout back into the generator lives
under `daemon_transfer/orchestration/`. That pump is the place where
async would change shape.

### Where threads sit waiting

Per active SSH transfer, the current path costs (at minimum):

| Thread | Blocking call | Most-of-the-time state |
|--------|---------------|------------------------|
| Receiver | `ChildStdout::read` | Parked on pipe read after a frame is drained |
| Sender / generator | `ChildStdin::write_all` | Parked on pipe write when the remote is slow |
| Stderr drain | `ChildStderr::read` | Parked on pipe read until the child writes |
| Connect watchdog (optional) | `Condvar::wait_timeout` | Parked until cancel or timeout |

Two of these (the stderr drain and the watchdog) exist only to manage
the subprocess. They do real work but they have nothing to do with
the byte stream. Three of them are blocked on pipe I/O at the moment
the pipe peer is doing the actual work. That is the wait that async
exists to overlap.

## 2. What async actually buys

The SSH transport is dominated by waiting on someone else: the
remote shell, the destination disk, or the network between them. A
synchronous transfer thread cannot interleave a `read` and a `write`;
when it is parked on a pipe read, it cannot also push the previous
frame's payload to disk, and vice versa. The SPSC pipeline at
`crates/transfer/src/pipeline/spsc.rs` already hides some of the
serialisation for the daemon TCP path, but SSH stdio cannot use that
fast path; it relies on blocking syscalls and a thread-per-half model.

Async pays back in three concrete shapes:

### 2.1 Overlap read and write on a single transfer

`tokio::select!` or `tokio::io::copy_bidirectional` lets one task drive
both halves of the SSH pipe. While one half is awaiting the next pipe
chunk, the other half is free to make progress. On a 50 ms RTT link
or a slow rotational destination disk, this is straightforward latency
hiding that the sync path cannot do without spawning more threads.
`docs/design/async-ssh-transport.md` section 4 quantifies the
expected workload classes.

### 2.2 One event loop, many concurrent transfers

The current model costs a minimum of two or three OS threads per
active SSH connection (receiver, sender, and the stderr drain) plus
the watchdog during connect. A multi-connection client (the realistic
target is a fan-out client doing concurrent pulls across a fleet) hits
the thread-per-transfer ceiling fast. An async transport collapses
this to one tokio reactor driving N futures; thread cost amortises to
roughly `num_cpus`.

This benefit only matters when the client actually multiplexes. The
single-shot CLI does not. The realistic beneficiary is a future
fan-out client, internal tooling that drives many concurrent SSH
pulls, or the embedded library use case where the host owns the
runtime.

### 2.3 Eliminate the stderr drain and watchdog threads

Both the stderr drain and the connect watchdog are workarounds for
the blocking subprocess API: a separate thread is the only way to
poll `ChildStderr` and to time out `Child::wait`. On tokio,
`tokio::process::ChildStderr` is an `AsyncRead` we can poll alongside
the data halves with `tokio::select!`, and the connect timeout is a
`tokio::time::timeout` on the auth future. Both helper threads
disappear into the same event loop.

This is a model simplification, not a throughput win. It still pays
off in fewer surfaces to test, fewer Drop dances, and fewer places
where a panic in a helper thread can leak a child process.

## 3. Runtime options

The runtime decision is settled. The migration plan
(`docs/design/async-migration-plan.md` section 4) commits the
workspace to tokio and forbids a second runtime. The reasoning:

- Tokio is already a workspace dependency
  (`Cargo.toml:188`); embedded SSH and the optional `async` feature
  on `daemon` both pull it in.
- russh, the only viable async-native Rust SSH crate, is
  tokio-native.
- Every async crate we would integrate (`tokio-process`,
  `tokio-uring`, `tokio::io::copy_bidirectional`) assumes tokio.
- A second runtime (smol, async-std) would force `async-compat`
  shims, duplicate timer wheels, and a split reactor. The audit at
  #1779 and the rule codified by #1780 both reject this.

For completeness, the rejected options:

| Runtime  | Why not |
|----------|---------|
| smol     | Second runtime. Bridging cost erases the footprint win. |
| async-std | Maintenance has slowed; second runtime; bridging cost. |
| glommio  | Thread-per-core forbids `Send` futures; incompatible with rayon and the sync engine. Would require rewriting the engine. |
| monoio   | Same thread-per-core constraint as glommio. |

This evaluation accepts that decision and does not reopen it.

## 4. Integration shape

The migration plan is explicit that the SSH transfer engine stays
sync; only the I/O boundary moves to async. This section names where
that boundary sits and what bridge surfaces it requires.

### 4.1 Where the boundary lives

The narrowest viable boundary is at the SSH transport layer itself:

- `SshConnection`, `SshReader`, and `SshWriter` expose async halves
  (`AsyncRead` + `AsyncWrite`) when the `async-ssh` feature is on.
- The bidirectional pump in
  `crates/core/src/client/remote/daemon_transfer/orchestration/`
  switches from a thread-per-half copy to
  `tokio::io::copy_bidirectional`, run on the tokio runtime that the
  caller owns.
- Everything downstream of the pump (the receiver pipeline, the
  delta apply, the disk-commit thread) stays sync. The handoff is a
  bounded channel of byte buffers, exactly the topology in
  `docs/design/async-migration-plan.md` section 5.

The sync receiver pipeline and the engine never see a tokio worker.
The SPSC pipeline at `crates/transfer/src/pipeline/spsc.rs` stays
sync forever (risk R3 in the migration plan): it spin-waits, and a
spin loop inside a tokio worker would starve the runtime.

### 4.2 `spawn_blocking` surfaces

The async SSH path needs a small number of `spawn_blocking` call
sites. Each one is a known boundary, not a leak.

- **Initial handshake side effects**: parsing `~/.ssh/known_hosts`,
  loading agent keys, reading `~/.ssh/config`. Most of these are
  bounded blocking syscalls; on a multi-thread runtime they are
  cheap enough to call directly. Above ~100 microseconds, wrap in
  `spawn_blocking` per the rule in `docs/design/async-migration-plan.md`
  section 5.4.
- **Bridging into the sync transfer engine**: the per-connection
  worker that runs the receiver pipeline is a long-lived
  `spawn_blocking` task. It owns the sync state machine, the SPSC
  pipeline, and any rayon `par_iter` it triggers.
- **Rayon CPU work invoked from async**: routed through the
  `rayon_bridge` helper documented in
  `docs/design/tokio-spawn-blocking-rayon.md` section 6 (#1751).
  Single entry point, threshold short-circuit, panic mapping to
  `ExitCode::PROTOCOL` with `[server]` trailer. No async caller
  invokes rayon directly.

No new bridge primitives are required for async SSH beyond what #1751
already plans. The SSH transport reuses the same surfaces.

### 4.3 Cancellation and panic isolation

`spawn_blocking` futures cannot be cancelled (migration plan risk
R7). The per-connection sync worker must therefore check a
cooperative cancellation token between transfer batches; the pattern
is identical to the rayon bridge's cancellation discipline. Panics
from any spawned task surface as `JoinError::is_panic()` and map to
`ExitCode::PROTOCOL` with the appropriate role trailer (sender,
receiver, or server), matching upstream's per-connection panic
semantics.

## 5. Embedded russh vs subprocess ssh

The migration plan (section 2.2 and Phase 3) calls out two
implementation tracks. This section recommends an ordering.

### 5.1 Subprocess via `tokio::process::Child`

Smallest delta from today's code.

- Swap `std::process::Command` for `tokio::process::Command`.
- `tokio::process::ChildStdin`/`ChildStdout` are `AsyncWrite`/
  `AsyncRead`. Drive the pump with `tokio::io::copy_bidirectional`.
- The stderr drain and connect watchdog collapse into
  `tokio::select!` arms on `ChildStderr` and `tokio::time::timeout`.

**Wins**: minimal builder/orchestration churn, preserves the system
`ssh` binary as the crypto and config-parity surface (no
known-hosts, agent, or cipher decisions to take ownership of),
preserves OpenSSH semantics for free.

**Costs**:

- Still pays `fork`+`execve` per connection. Async does not reduce
  process-spawn overhead (risk R2 in the migration plan); only the
  I/O wait between fork and exec yields cooperatively.
- `tokio::process::Child` on Unix uses `SIGCHLD` and a child reaper
  task; on Windows it sits on `WaitForSingleObjectEx`. Both are
  fine, but they are a new failure surface to test.
- A `tokio::process::Child` requires a tokio reactor handle to be
  alive for the lifetime of the child. The caller must own a
  runtime; we cannot transparently use this from a sync caller
  without the embedded-style `block_on` shim, which nests runtimes
  badly under composition.

### 5.2 Embedded russh

Larger surface today, but already tokio-shaped.

- `crates/rsync_io/src/ssh/embedded/` is wired up for russh under
  the `embedded-ssh` feature. The internal implementation is async
  (`connect_and_exec_async`) and the production surface synthesises
  a current-thread runtime to keep callers sync
  (`embedded/connect.rs:107`).
- Eliminating that internal `block_on` and exposing the async
  surface to async callers is a strictly smaller change than
  retrofitting the subprocess transport: the futures already exist.
- russh runs on the existing tokio runtime, multiplexes auxiliary
  channels without a sidecar `socketpair`, and gives full
  in-process visibility into the SSH session (keepalive timers,
  channel windows, cipher negotiation).

**Wins**:

- No `fork`+`execve` per connection. The embedded-ssh client opens
  a TCP connection and runs the SSH state machine in-process. For
  fan-out clients this is a real per-connection cost reduction, not
  just an overlap win.
- A single tokio task graph for SSH I/O, framing, and (eventually)
  the demuxer. Fewer cross-boundary handoffs.
- No system `ssh` binary dependency. The embedded path can run in
  environments where OpenSSH is absent (small containers, Windows
  hosts without a shipped client).
- Already async in the internals. Lifting the async surface is a
  refactor of `embedded/connect.rs`, not a rewrite.

**Costs**:

- Crypto, key/agent, known-hosts, and `~/.ssh/config` parity become
  our problem. Some of this is already implemented under
  `embedded/` (`config.rs`, `ssh_config.rs`, `auth.rs`,
  `resolve.rs`); the gap to OpenSSH parity is the work of the
  separate `embedded-ssh` track.
- russh API churn risk (migration plan R6). Pin to a workspace
  version; isolate behind the cargo feature; treat it as a
  separable failure surface.
- Larger compile-time footprint when the feature is on. Acceptable
  because the feature is opt-in and `--no-default-features` builds
  exclude both russh and tokio (migration plan section 6.2).

### 5.3 Recommendation: embedded russh first, async subprocess second

The embedded russh track is the better first async SSH target.

1. **It is already async-shaped.** The internal futures exist; the
   present sync facade is a `block_on` wrapper. Lifting the public
   surface from sync to async is a strictly smaller refactor than
   inventing an async subprocess transport from scratch.
2. **It removes the per-connection `fork`+`execve` cost.** That is
   the only path to a per-connection cost reduction; the async
   subprocess transport still pays fork/exec (migration plan R2)
   and only overlaps the post-handshake I/O.
3. **It eliminates two helper threads per connection.** The stderr
   drain and the connect watchdog have no analogue in an embedded
   client. The model gets smaller, not larger.
4. **It exercises the single-runtime invariant first.** Embedded
   russh already runs on tokio; the only question is which runtime
   handle it uses. Lifting the surface forces us to answer that
   cleanly. The async subprocess transport, by contrast, would
   layer a second async I/O surface on top of the same async-shaped
   compatibility surface, doubling the integration cost.

The async subprocess transport remains valuable and is *not*
abandoned. It is a Phase 3 follow-up that lets operators who need
OpenSSH-binary parity (custom key types, smartcard agents,
GSSAPI/Kerberos) get the I/O-overlap win without taking on russh's
crypto surface. Defer it until embedded russh is proven in
production behind the `async-ssh` feature.

Concretely:

- **First**: lift `crates/rsync_io/src/ssh/embedded/` to expose an
  async public surface. Gate behind the `embedded-ssh` and
  `async-ssh` cargo features. Use the existing `embedded/connect.rs`
  internals; remove the internal `block_on`. Tracked under #1796 /
  #1797 (the embedded-ssh async surface work) and #1805 / #1806
  (the hardening follow-ups).
- **Second**: add a tokio-process backend behind the same
  `async-ssh` feature flag. Share the builder, watchdog, and
  stderr-drain abstractions via traits with the embedded path.
  Promote to default only after meeting the benchmark thresholds in
  `docs/design/async-ssh-transport.md` section 5.

This ordering respects the migration plan's promotion gates and
keeps the synchronous `std::process` transport as the default
throughout Phase 3.

## 6. Open questions and follow-ups

The implementation work is tracked under four issues. Each owns a
slice of the recommendation above.

| Issue | Scope |
|-------|-------|
| #1796 | Embedded russh async surface - lift `connect_and_exec` to expose an async API, remove the internal current-thread runtime, expose `AsyncRead`/`AsyncWrite` channel halves. |
| #1797 | Embedded russh integration with the bidirectional pump in `crates/core/src/client/remote/daemon_transfer/orchestration/`. Replace the thread-per-half copy with `tokio::io::copy_bidirectional`. |
| #1805 | Embedded russh hardening - cancellation tokens between transfer batches, `JoinError` -> exit-code mapping with role trailers, panic isolation tests under tokio. |
| #1806 | Async subprocess transport - tokio-process backend, shared builder/watchdog traits with the embedded path, benchmark-gated default promotion per `docs/design/async-ssh-transport.md` section 5. |

Open questions handed off to those issues:

1. **Embedded-ssh OpenSSH parity matrix**. Which features
   (`~/.ssh/config` directives, agent forwarding, certificate auth,
   GSSAPI, ProxyCommand) must reach parity before the embedded path
   can host the default async transport? Owner: rsync_io
   maintainers, tracked under #1796 and the broader embedded-ssh
   track.
2. **Runtime ownership for one-shot CLI**. The CLI does not own a
   tokio runtime today. For embedded russh on the CLI, the choice
   is: spin a current-thread runtime per invocation, or refuse to
   surface async SSH from the CLI and require the daemon or
   library to host it. Migration plan section 5.5 sketches the
   shape; #1797 must commit. Bias: keep the CLI sync, expose async
   only to embedded-library callers and the daemon.
3. **Channel surface between async pump and sync transfer worker**.
   The migration plan (section 5.3) defers the bridge-channel
   choice (`flume` vs `tokio::sync::mpsc`) to the first phase-2
   call site. The async SSH pump will be among the earliest. Owner:
   transfer maintainers; trigger: when #1797 starts.
4. **Default-features matrix**. `--no-default-features` must stay
   tokio-free (migration plan section 6.2). `async-ssh` will need a
   CI matrix entry; coordinate with the existing `embedded-ssh`
   matrix to avoid combinatorial explosion (risk R4). Owner: CI
   maintainers; trigger: when #1796 lands the first wired-up async
   surface.
5. **Bench harness for SSH overlap wins**. Migration plan section
   2.2 names the workload classes where async overlap should win;
   `docs/design/async-ssh-transport.md` section 4 lists the
   netem-shaped link conditions. The harness must be in place
   before promoting either async backend to default; tracked under
   #1806.

The result this evaluation locks in: when async SSH ships, it ships
on tokio, it ships embedded-russh-first, and the async subprocess
backend follows only after the embedded path is proven. The
synchronous `std::process` SSH transport remains the default until
either async backend clears its bench gate.
