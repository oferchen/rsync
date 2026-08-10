# Async I/O for SSH transport - decision evaluation

Tracker: oc-rsync tasks #1411 and #1593. Adjacent tasks:
#1198 (`ControlMaster` workaround documentation, done),
#1197 (single-threaded wire pipeline limitation, done),
#1782 (russh native async branch),
#1795-#1797 (`SshTransport` consolidation),
#1805-#1806 (sync/async bridge).

Last verified: 2026-05-07. Static analysis only.

## Purpose

This audit answers a single binary question for #1411 / #1593:

> Adopt an async runtime for the SSH transport, or keep the
> blocking subprocess path plus the user-side OpenSSH
> `ControlMaster` workaround?

It complements two existing audits without re-deriving them:

- `docs/audits/async-ssh-transport.md` (#1593) - kernel-level
  async on the pipe FDs (io_uring, kqueue, dispatch_io). Settled:
  defer to `docs/audits/iouring-pipe-stdio.md` (#1859).
- `docs/audits/ssh-transport-async-evaluation.md` (#1411) -
  deep static analysis of the runtime-level question. Settled:
  defer adoption until at least one of four falsifiable
  criteria trips.

The contribution here is the explicit decision matrix between
the two named candidates - tokio over `Command` stdio versus
russh on a #1782 native-async branch - against the named
alternative (status quo plus `ControlMaster`) under the
single-threaded wire-protocol constraint of #1197.

## 1. Current SSH transport on master

### 1.1 Spawn site

`SshCommand::spawn` at
`crates/rsync_io/src/ssh/builder.rs:307` is the single fork/exec
site. Stdin and stdout are configured as anonymous pipes
(`Stdio::piped()` at `crates/rsync_io/src/ssh/builder.rs:322-323`),
stderr as a Unix `socketpair(2)` with anonymous-pipe fallback
(`crates/rsync_io/src/ssh/builder.rs:334`,
`crates/rsync_io/src/ssh/aux_channel.rs:264-285`). Mirror of
upstream rsync 3.4.1 `do_cmd()` in `pipe.c` and `main.c`.

### 1.2 I/O model

Blocking. The data path is a thin delegation to
`std::process::ChildStdin` / `ChildStdout`:

- `crates/rsync_io/src/ssh/connection.rs:217-221`
  `impl Read for SshReader` - `self.stdout.read(buf)`,
  blocking `read(2)` on a pipe FD.
- `crates/rsync_io/src/ssh/connection.rs:229-237`
  `impl Write for SshWriter` - `self.stdin.write(buf)`,
  blocking `write(2)` on a pipe FD.
- `crates/rsync_io/src/ssh/connection.rs:498-542` provide
  the same impls on the unsplit `SshConnection`.

No `O_NONBLOCK` is set on either FD. The kernel default 64 KiB
pipe buffer applies on Linux. Three OS threads peak per
connection: caller, `ssh-stderr-drain-*`
(`crates/rsync_io/src/ssh/aux_channel.rs:108-117, 159-172`), and
`ssh-connect-watchdog`
(`crates/rsync_io/src/ssh/connection.rs:280-315`).

The transport driver runs on the calling thread:
`run_server_over_ssh_connection` at
`crates/core/src/client/remote/ssh_transfer.rs:545-610` calls
`connection.split()` (line 551), runs `perform_handshake`
(line 560), then `run_server_with_handshake` (line 585), then
drops the writer to signal EOF and waits for the child
(lines 596-605). No executor, no runtime.

### 1.3 Embedded `russh` path (current state)

The `embedded-ssh` cargo feature
(`crates/rsync_io/Cargo.toml:22, :32`) provides an in-process
russh client that runs a current-thread tokio runtime internally:

- `crates/rsync_io/src/ssh/embedded/connect.rs:107-122`
  `connect_and_exec` is `pub fn` (sync) and uses
  `rt.block_on(connect_and_exec_async(...))`.
- `crates/rsync_io/src/ssh/embedded/connect.rs:169-170`
  builds bridge channels - `std::sync::mpsc::sync_channel(64)`
  for read, `tokio::sync::mpsc::channel(64)` for write.
- `crates/rsync_io/src/ssh/embedded/connect.rs:31-79`
  `ChannelReader` / `ChannelWriter` implement synchronous
  `std::io::Read` / `std::io::Write`, blocking on `recv()` and
  `blocking_send()` respectively.

The async surface is hidden inside the crate. The downstream
consumer
(`crates/core/src/client/remote/embedded_ssh_transfer.rs:282-348`)
treats russh exactly as it treats the subprocess path: a sync
`Read + Write` pair driven by `perform_handshake` and
`run_server_with_handshake`.

## 2. ControlMaster workaround (#1198)

OpenSSH `ControlMaster` / `ControlPath` / `ControlPersist`
transparently multiplexes multiple SSH sessions over one
authenticated TCP connection. From oc-rsync's perspective the
workaround is invisible: the user adds the directives to
`~/.ssh/config`, and `ssh(1)` in the child process reuses the
existing master socket instead of running a fresh handshake.

Reference text and example block at
`docs/architecture/parallelization.md:170-183`. Discussion at
`docs/architecture-rationale.md:357-364`.

Properties relevant to the #1411 decision:

- Zero code in oc-rsync. The system `ssh(1)` does the
  multiplexing.
- Works today on every platform that ships OpenSSH (Linux,
  macOS, BSD, modern Windows with OpenSSH for Windows). On
  Windows it requires the OpenSSH client and a writable
  `ControlPath`; in practice users rarely enable it on
  Windows but it is supported.
- Halves perceived per-transfer setup cost for users who run
  many sequential transfers to the same host: subsequent
  transfers skip TCP setup, KEX, host-key check, and
  authentication.
- Does not parallelise the wire protocol pipeline. Each rsync
  transfer is still serial per #1197; the multiplexer just
  amortises connection setup.
- Failure mode is benign: if the master socket is gone or
  unreachable, `ssh(1)` falls back to a fresh connection.

The #1198 documentation has been on master for some time and
no user-facing complaint about its sufficiency has surfaced
that motivates re-evaluating it.

## 3. Async candidates

### 3.1 Candidate A: tokio over `Command` stdio

Replace the synchronous I/O wrapper around the spawned `ssh`
child with `tokio::process::Command` and
`tokio::process::ChildStdin` / `ChildStdout`. These types
implement `tokio::io::AsyncRead` / `AsyncWrite` and set
`O_NONBLOCK` on the pipe FDs internally
(see tokio's `process` module).

**What this changes:**

- Replaces blocking `read(2)` / `write(2)` with the async
  variants driven by tokio's reactor. On Linux that reactor
  uses epoll today; tokio's io_uring backend (`tokio-uring`) is
  separate and is not the default.
- Requires a tokio runtime in scope. The default oc-rsync
  binary does not link tokio (`crates/rsync_io/Cargo.toml`
  marks tokio as `optional = true` and gates it behind the
  `embedded-ssh` feature).
- Forces a sync/async bridge at the engine boundary. The
  transfer pipeline (`crates/transfer/src/pipeline/spsc.rs`,
  the receiver in `crates/transfer/src/receiver.rs`) is sync.
  Either the bridge happens at `ssh_transfer.rs:545-610`
  (block_on / spawn_blocking per chunk - hot-path cost) or
  the entire pipeline migrates to async (out of scope per
  `docs/design/async-migration-plan.md` Phase 4).

**What this does not change:**

- Wire output is byte-identical. The pipe carries the same
  bytes whether `read(2)` is blocking or async; the goldens
  under `crates/protocol/tests/golden/` would not move.
- The single-threaded ordering invariant from #1197 still
  applies. Async I/O on the pipe does not unlock new
  parallelism in the wire pipeline (`docs/architecture/parallelization.md:50-90,122`).
- The `ssh(1)` child process is still spawned per transfer,
  with the same handshake / auth cost. ControlMaster still
  works underneath the tokio wrapper.

**Cost surface:**

- tokio runtime startup: 100-300 microseconds and a few MB of
  RSS (`docs/design/async-migration-plan.md:395-401`).
- Binary-size increase, dependency-tree growth, audit surface
  growth for the default build.
- Bridge cost per chunk if the engine stays sync (an
  allocation plus a `block_in_place` or per-chunk
  `spawn_blocking`).

### 3.2 Candidate B: russh on the #1782 native-async branch

Promote the `embedded-ssh` path to default and remove the
sync facade that today wraps russh in `ChannelReader` /
`ChannelWriter`. The transfer pipeline drives russh's
`AsyncRead` / `AsyncWrite` directly.

**What this changes:**

- Eliminates the `ssh(1)` subprocess entirely on supported
  platforms. KEX, auth, cipher, MAC all run in-process via
  russh.
- Eliminates the per-chunk `Vec<u8>` allocation and
  `blocking_send` / `recv` pair on the bridge channels at
  `crates/rsync_io/src/ssh/embedded/connect.rs:71, :186` -
  the chunks flow directly between russh and the engine.
- Lifts the runtime question from "do we need one" to "we
  have one already; design accordingly" - russh has no
  realistic non-async API.
- Programmatic key management and host-key handling become
  oc-rsync's responsibility instead of `ssh(1)`'s. The
  embedded path already carries this surface
  (`crates/rsync_io/src/ssh/embedded/auth.rs`,
  `crates/rsync_io/src/ssh/embedded/cipher.rs`).

**What this does not change:**

- Wire output is byte-identical at the rsync protocol layer.
  The SSH cipher/MAC layer underneath is independently
  negotiated; users may see different cipher choices than
  with system `ssh(1)`, addressed by
  `docs/audits/ssh-cipher-compression.md`.
- The single-threaded wire-protocol constraint of #1197 still
  applies. russh's async machinery does not parallelise the
  rsync pipeline.

**Cost surface:**

- russh is less battle-tested than OpenSSH. Production
  deployments lose the trust boundary that "OS-installed
  `ssh(1)` handles all crypto" provides.
- Operators lose `ssh_config` integration: `ControlMaster`,
  `ProxyJump` chains, agent forwarding, certificate auth
  with custom CAs, jump-host-aware key selection. Some of
  these have russh equivalents; reproducing the full surface
  is not free.
- Always-on tokio runtime in the default build. Dependency
  graph and binary size grow for every user, including the
  ones who never use SSH.
- The #1782 branch is not on master today; landing it is the
  prerequisite for this candidate to become real.

## 4. Single-threaded ordering constraint (#1197)

The rsync wire pipeline is single-threaded and order-preserving
within each role
(`docs/architecture/parallelization.md:50-90,122`):

- File indices flow in order; sender deltas, receiver acks,
  and matching tokens all key on monotonically increasing
  indices.
- The network-facing thread per role is one thread by design.
  The SPSC channel
  (`crates/transfer/src/pipeline/spsc.rs`, capacity 128) only
  decouples disk commit from network read on the receiver; it
  does not relax the wire ordering.

Implication: any async vehicle wraps a strictly serial
pipeline. Async cannot:

- Parallelise file deltas on a single connection.
- Parallelise sender and receiver work on a single connection
  (the protocol's request/response shape forbids it).
- Improve wall-clock time for a single transfer that is
  CPU-bound on cipher or rolling-checksum work; that work is
  outside the SSH stdio path entirely.

Async can:

- Amortise syscalls if the runtime supports batched
  submission (tokio's epoll backend does not; tokio-uring or
  the io_uring pipe path of #1859 does).
- Overlap pipe I/O with disk I/O on the receiver when the
  SPSC channel is full. The win is bounded; the network
  thread today has no other work because the pipeline is
  serial.
- Enable a hypothetical multi-host fan-out driver where one
  process runs N transfers concurrently. That feature does
  not exist on master.

The mismatch between the async vehicle's strengths and the
single-threaded pipeline's needs is the structural reason why
the #1411 decision is "defer" rather than "adopt".

## 5. Decision matrix

| Axis | Status quo + ControlMaster (#1198) | Candidate A: tokio over `Command` stdio | Candidate B: russh on #1782 |
|------|-----------------------------------|-----------------------------------------|------------------------------|
| Code change | none | medium - swap `Command` for `tokio::process::Command`, add bridge | large - promote `embedded-ssh` to default, remove bridge |
| Default-build dependency cost | none | tokio always linked | tokio + russh always linked |
| Wire-output identity vs upstream | identical | identical | identical at rsync layer; SSH cipher choice may differ |
| #1197 ordering constraint | unaffected | unaffected | unaffected |
| Per-transfer setup cost (warm) | very low - reuses master socket | same as today (still spawns `ssh(1)`) | none - no subprocess |
| Per-transfer setup cost (cold) | one full SSH handshake | same as today | one in-process SSH handshake |
| Connection multiplexing | yes (system `ssh(1)`) | yes (system `ssh(1)`) | no (would need russh-side support) |
| Trust boundary | OS-installed `ssh(1)` | OS-installed `ssh(1)` | russh, in-process |
| Operator config (`ssh_config`, agents, certs, ProxyJump) | full | full | partial; russh-specific surface |
| Cancellation latency | bounded by `ConnectWatchdog` | improved (reactor can cancel a pending read) | improved |
| Multi-host fan-out (hypothetical) | linear thread growth | bounded by runtime | bounded by runtime |
| Windows | works | tokio supports `tokio::process` on Windows | works (russh is portable) |
| Engineering effort to land | done | medium; #1805/#1806 own the bridge | large; #1782 still in flight |
| Risk to default builds | none | binary size, startup latency | binary size, startup latency, crypto surface |

The matrix shows the status quo dominates on every cost axis
and is only weak on the two synthetic benefits async would
provide - reactor-driven cancellation and hypothetical
fan-out. Neither benefit has a user-visible feature on master
that requires it. ControlMaster covers the realistic
concurrency story (sequential transfers to the same host)
without code in oc-rsync.

## 6. Decision

**Keep the blocking subprocess path plus user-side
`ControlMaster` as the default. Defer adoption of either
async candidate until #1411's falsifiable criteria trip.**

The criteria, restated from
`docs/audits/ssh-transport-async-evaluation.md:421-466`:

1. Multi-host fan-out lands as an oc-rsync feature.
2. Pipe-FD io_uring (#1859) shows a measurable but small
   throughput win, motivating a runtime that natively
   schedules many submissions.
3. The receiver pipeline migrates to async (Phase 4 of
   `docs/design/async-migration-plan.md`), forcing a per-chunk
   bridge cost the transport could avoid by going native
   async on the same runtime.
4. A user-visible cancellation primitive ships
   (`--abort-after=DURATION`, signal-driven graceful cancel)
   for which the sync `read(2)` block becomes a problem.

Until at least one trips, the costs of either candidate
exceed the benefits:

- Candidate A buys nothing the user can see today. ControlMaster
  already covers warm-path setup cost; tokio over the same
  `ssh(1)` child does not change wire identity, parallelism,
  or operator config surface, but does add tokio to the
  default dependency graph.
- Candidate B buys an in-process SSH stack at the cost of the
  trust boundary and the operator config surface that
  motivated keeping `russh` behind a feature in the first
  place
  (`docs/architecture-rationale.md:200-211`,
  `docs/audits/tokio-dependency-boundary-2026.md`). The
  #1782 branch should land as an enhancement to the existing
  opt-in path, not a replacement for it.

If criterion 1 trips, candidate B's existing tokio runtime is
the natural integration point - the embedded path already
runs an async stack and only its `Read` / `Write` facade
needs to peel back. If criterion 2 or 3 trips, a process-wide
tokio runtime per
`docs/design/async-migration-plan.md:402-408` is the right
answer for both candidates simultaneously. If criterion 4
trips alone, the narrowest fix is a sync cancellation token
threaded through `SshConnection`, not a runtime adoption.

## 7. Phasing

- **Now.** Document this decision (this audit). Track
  follow-ups under #1411 and #1593. No code changes.
- **#1859 lands.** Phase-1 io_uring on pipe FDs. Re-evaluate
  if syscall amortisation moves the throughput needle.
- **#1782 lands.** russh native-async path matures behind the
  `embedded-ssh` feature. The path stays opt-in; default
  remains the subprocess.
- **#1795-#1797 lands.** `SshTransport` consolidation gives
  the consumer a runtime-pluggable trait. This is the
  prerequisite for swapping the default path without
  rewriting `crates/core/src/client/remote/ssh_transfer.rs`
  and `embedded_ssh_transfer.rs` together.
- **One of the four #1411 criteria trips.** Reopen this
  decision against fresh evidence.

## 8. Open questions (carried)

These are recorded in
`docs/audits/ssh-transport-async-evaluation.md:468-504` and
not re-derived here:

- Empirical syscall count baseline on a 1 GiB transfer
  (#1889).
- CPU cost of the embedded path's bridge per chunk.
- Whether `IORING_FEAT_FAST_POLL` actually fires on default
  pipe buffers.
- Windows IOCP on anonymous pipes with `OVERLAPPED`.
- Interaction with #1686 socketpair migration.
- `SshTransport` trait shape in #1795-#1797.

## Cross-references

- `docs/audits/async-ssh-transport.md` (#1593) - kernel-level
  pipe-FD analysis baseline.
- `docs/audits/ssh-transport-async-evaluation.md` (#1411) -
  full runtime-level static analysis.
- `docs/audits/iouring-pipe-stdio.md` (#1859) - Linux
  pipe-FD io_uring.
- `docs/audits/splice-ssh-stdio.md` (#1860) - zero-copy
  splice/vmsplice on file <-> pipe edges.
- `docs/audits/ssh-socketpair-vs-pipes.md` (#1686, #1689) -
  socketpair migration for the SSH wire.
- `docs/audits/ssh-cipher-compression.md` - cipher and
  compression negotiation parity.
- `docs/audits/ssh-process-management.md` - SSH child
  lifecycle correctness.
- `docs/audits/ssh-transport-timeout-coverage.md` - timeout
  coverage gap analysis.
- `docs/audits/tokio-dependency-boundary-2026.md` - tokio
  feature-gate policy and the embedded-SSH bridge.
- `docs/architecture/parallelization.md:170-183` -
  ControlMaster workaround text.
- `docs/architecture-rationale.md:200-211, 357-364` -
  rationale for keeping `russh` behind a feature.
- `docs/design/async-migration-plan.md` (#1594) - five-phase
  sequencing for any future async migration.
- Tasks: #1197 (single-threaded pipeline, done), #1198
  (ControlMaster doc, done), #1411 (this audit's primary
  tracker), #1593 (kernel-level companion), #1782 (russh
  native async branch), #1795-#1797 (`SshTransport`
  consolidation), #1805-#1806 (sync/async bridge), #1859,
  #1860, #1889-#1892, #1934.

## Upstream evidence

Upstream rsync 3.4.1 in
`target/interop/upstream-src/rsync-3.4.1/` performs all SSH
stdio I/O via blocking `read(2)` / `write(2)` on the
inherited pipe FDs (`io.c`). `pipe.c` and `main.c::do_cmd()`
fork an `ssh(1)` child with `Stdio::piped()`-equivalent
plumbing. There is no async runtime, no io_uring, no kqueue,
no IOCP, no russh equivalent. The async question is therefore
an oc-rsync-internal concurrency-model question with no
wire-protocol implication; both candidates evaluated here
must produce byte-identical wire output to the upstream sync
path. That property is verified by
`crates/protocol/tests/golden/` and the interop harness
`tools/ci/run_interop.sh`.
