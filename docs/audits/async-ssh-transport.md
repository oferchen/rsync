# Async I/O for the SSH transport path

Tracking: oc-rsync task #1593. Closely related: task #1411 ("Evaluate async
runtime for SSH transport path"); see scope delimitation below.

Last verified: 2026-05-01

## Summary

This audit asks whether oc-rsync should replace the blocking `read(2)` /
`write(2)` calls on the parent's view of the SSH child stdio with some form
of async I/O while keeping the existing thread topology unchanged. The
conclusion is that the win is bounded and platform-conditional:

- On Linux 5.7+ the right vehicle is `IORING_OP_READ` / `IORING_OP_WRITE`
  on the pipe FDs, already audited under task #1859
  (`docs/audits/iouring-pipe-stdio.md`). This audit explicitly defers the
  Linux pipe-async path to that audit.
- On macOS, `dispatch_io` (`docs/audits/macos-dispatch-io.md`) and on
  FreeBSD/NetBSD POSIX `aio_*` (`docs/audits/bsd-aio.md`) can drive pipe
  FDs, but the SSH stdio is not the bottleneck on those platforms either,
  and neither audit recommends prioritising the SSH path.
- On Windows the subprocess SSH path exists and is functional, but
  io_uring / kqueue / dispatch_io do not apply, and IOCP on anonymous
  pipes is awkward. Status quo (sync blocking I/O on `ChildStdin` /
  `ChildStdout`) is the right default.
- Adopting tokio (or any async runtime) at the SSH transport layer is a
  separate, larger question and is the subject of task #1411. This audit
  does not subsume that work.

The rsync wire protocol pipeline is single-threaded and order-preserving
within each role, so async I/O at the transport does not unlock new
parallelism. The win, where any exists, is syscall amortisation and
better integration with concurrent disk I/O on the receiver. Cite for
this constraint: `docs/architecture/parallelization.md` lines 50-90.

## Scope delimitation: this audit vs task #1411

Two distinct questions live in adjacent territory:

1. **Async I/O at the transport level (this audit, #1593).** Replace the
   blocking `read` / `write` on the inherited stdio pipe FDs with an
   async kernel facility (io_uring on pipes, kqueue + non-blocking
   pipes, `dispatch_io`, POSIX `aio_*`) while keeping the existing
   thread topology - one generator thread, one receiver thread, one
   disk-commit thread, all driving sync `Read` / `Write` traits.
2. **Async I/O at the runtime level (deferred, #1411).** Adopt tokio
   (or another runtime) inside `crates/rsync_io/src/ssh/` and
   `crates/core/src/client/remote/`, restructuring the protocol
   pipeline around `AsyncRead` / `AsyncWrite` and tasks. Today only
   embedded SSH uses tokio, and only behind the `embedded-ssh`
   feature gate (see
   `crates/rsync_io/src/ssh/embedded/connect.rs:107-122`). Out of
   scope here.

The phrasing of #1593 admits both readings. This audit takes the
narrower one; #1411 should consume the broader one. The two are
complementary: the kernel-level async path can land independently
as an optional fast path behind the existing `IoUringPolicy` knob
without committing to a runtime migration.

## Current SSH transport on master

### Subprocess spawn and stdio piping

The single spawn site is `crates/rsync_io/src/ssh/builder.rs:285-340`
in `SshCommand::spawn`. It configures the child stdio as anonymous
pipes:

```rust
let mut command = Command::new(&program);
command.stdin(Stdio::piped());
command.stdout(Stdio::piped());
```

(`crates/rsync_io/src/ssh/builder.rs:299-301`.)

Stderr is wired through a Unix `socketpair(2)` when available, with
fallback to the conventional anonymous pipe; on Windows the only path
is `Stdio::piped()`. The selection happens at
`crates/rsync_io/src/ssh/builder.rs:312` via
`configure_stderr_channel` and is dispatched in
`crates/rsync_io/src/ssh/aux_channel.rs:298-316`. The stderr channel
is not part of the rsync data path - a background drain thread (also
in `aux_channel.rs`) prevents pipe-buffer deadlocks when the remote
writes diagnostic output.

After spawn, the parent ends are stored in `SshConnection`:

- `crates/rsync_io/src/ssh/connection.rs:30-39` defines the struct
  with `Option<ChildStdin>` and `Option<ChildStdout>`. There is no
  `O_NONBLOCK` flag on either FD. The kernel default 64 KiB pipe
  buffer applies.
- `crates/rsync_io/src/ssh/connection.rs:178-208` `SshConnection::split`
  hands out half-duplex `SshReader { stdout: ChildStdout }` and
  `SshWriter { stdin: ChildStdin }` plus an owning `SshChildHandle`.

### Read/write hot loop

The read/write trait implementations are direct delegates to the
`std::process` types:

- `crates/rsync_io/src/ssh/connection.rs:217-221` `impl Read for
  SshReader` calls `self.stdout.read(buf)`, which is `read(2)` on
  the inherited pipe FD.
- `crates/rsync_io/src/ssh/connection.rs:229-237` `impl Write for
  SshWriter` calls `self.stdin.write(buf)` / `self.stdin.flush()`,
  which is `write(2)` on the inherited pipe FD.

The transport-level driver is
`crates/core/src/client/remote/ssh_transfer.rs:548-610`,
`run_server_over_ssh_connection`. It calls `connection.split()` at
line 554-556 and threads `&mut reader` / `&mut writer` through
`crate::server::perform_handshake` (line 563) and
`crate::server::run_server_with_handshake` (line 589). The bulk
transfer loop runs entirely on the calling thread; there is no
runtime, no executor, and no `O_NONBLOCK`.

### Existing `embedded-ssh` feature (russh)

`crates/rsync_io/src/ssh/embedded/` provides a pure-Rust SSH client
gated behind the `embedded-ssh` cargo feature
(`crates/rsync_io/src/ssh/embedded/mod.rs:9-37`). It is structurally
different from the subprocess path:

- `crates/rsync_io/src/ssh/embedded/connect.rs:107-122`
  `connect_and_exec` builds a tokio current-thread runtime and calls
  `rt.block_on(connect_and_exec_async(...))` to drive russh.
- `crates/rsync_io/src/ssh/embedded/connect.rs:20-79` defines
  `ChannelReader` / `ChannelWriter` that wrap `std::sync::mpsc` and
  `tokio::sync::mpsc` channels respectively, implementing the
  synchronous `std::io::Read` / `std::io::Write` traits and bridging
  into a background tokio task that owns the russh channel.

The embedded path therefore already crosses an async boundary, but
only inside the russh client. From the rsync-protocol layer's
perspective the interface is still synchronous `Read` / `Write`,
identical to the subprocess path. Whether the embedded path itself
warrants a separate async-runtime evaluation is an open question
(see "Open questions" below).

## Async pipe-I/O options per platform

Because the SSH child stdio is a pipe pair (or, with #1686
landed, potentially a `socketpair`), we are constrained to async
mechanisms that accept pipe / pollable FDs.

### Linux

Two viable mechanisms exist:

- **io_uring on pipe FDs.** `IORING_OP_READ` / `IORING_OP_WRITE`
  accept pipe FDs without a special flag. Linux 5.7's
  `IORING_FEAT_FAST_POLL` lets the kernel arm an internal poll on
  the pipe before scheduling the read, so io_uring worker threads
  do not have to block. This is the path audited under task #1859
  (`docs/audits/iouring-pipe-stdio.md`); that audit recommends
  phase-1 integration mirroring the existing daemon socket reader
  / writer factories. **Recommendation: defer to #1859.**
- **`epoll` + non-blocking pipes.** Set `O_NONBLOCK` on
  `ChildStdin` / `ChildStdout`, register both FDs in an `epoll`
  set, and drive a sync state machine that reads on `EPOLLIN` and
  writes on `EPOLLOUT`. This is mechanically simpler than
  io_uring but requires:
  - flipping `O_NONBLOCK` (a one-time `fcntl`); upstream rsync
    similarly toggles non-blocking mode in `io.c::io_set_nonblocking`,
  - hand-written `EAGAIN` retry loops at every read/write site, and
  - one extra `epoll_wait` syscall per readiness transition.

  The expected payoff is small. epoll is most useful when one thread
  must service many FDs; here we have two FDs total per SSH
  connection, on threads that are dedicated to that connection
  anyway. We would trade a single `read` syscall per chunk for an
  `epoll_wait` plus a `read`. **Recommendation: not worth it on
  Linux when io_uring on pipes is also available.**

### macOS

- **kqueue + non-blocking pipes.** macOS lacks io_uring entirely.
  The closest async-pipe primitive is kqueue with `EVFILT_READ` /
  `EVFILT_WRITE` registered on a non-blocking pipe FD, paired with
  `kevent(2)` for readiness notification. This is the exact macOS
  analogue of the Linux epoll path and inherits the same
  cost/benefit verdict: small win, not worth the code.
- **`dispatch_io`.** Per `docs/audits/macos-dispatch-io.md`,
  `dispatch_io` channels can wrap pipe FDs in
  `DISPATCH_IO_STREAM` mode and deliver partial results through a
  block handler. It is async, queue-driven, and integrates with
  Grand Central Dispatch's worker pool. The macOS audit recommends
  `dispatch_io` primarily for the file-I/O hot path, not the SSH
  stdio path; the same reasoning applies here (the SSH stdio is
  not the macOS bottleneck). No separate macOS pipe-async work is
  motivated by this audit.
- **POSIX `aio_*` on pipes.** macOS supports `aio_read(2)` on
  socket / pipe FDs but the implementation is libdispatch-backed
  and offers no advantage over `dispatch_io`. Skip.

### Windows

The subprocess SSH path is functional on Windows. `SshCommand::spawn`
runs identically (`crates/rsync_io/src/ssh/builder.rs:285-340`); the
stderr channel falls through to the anonymous-pipe path
(`crates/rsync_io/src/ssh/aux_channel.rs:310-316`) because Unix
sockets are not used. Async pipe I/O on Windows would mean IOCP
attached to an anonymous-pipe HANDLE, which is supported but awkward
(anonymous pipes are not overlapped by default; Rust's
`std::process::ChildStdin` / `ChildStdout` are not opened with
`FILE_FLAG_OVERLAPPED`). The expected throughput win is small for
the same reasons as Linux/macOS, and the engineering cost is
disproportionate to the benefit. **Recommendation: status quo on
Windows. No async pipe transport.** If the user uses the
`embedded-ssh` feature, the russh / tokio path applies on Windows
identically to Unix.

### FreeBSD / NetBSD

`docs/audits/bsd-aio.md` covers POSIX `aio_*` on BSD. The same
pattern as macOS holds: pipe FDs accept `aio_read` / `aio_write`,
but the SSH stdio is not the BSD bottleneck. Skip for now.

## Bottleneck analysis

For typical rsync-over-SSH transfers the throughput bottleneck is
the SSH cipher and the network, not the local stdio pipe I/O. Three
sub-cases where async at the transport layer might help:

- **Small-payload latency.** A handshake plus a small file list
  involves a handful of `read` / `write` calls per direction. Total
  syscall time is dominated by SSH cipher init and DNS / TCP
  setup, both of which happen in the SSH child outside oc-rsync's
  view. Async pipe I/O would not move this needle.
- **Many simultaneous remote rsyncs from one process.** oc-rsync
  does not initiate parallel SSH connections from a single
  invocation today; each `rsync foo: bar/` runs one transport. A
  hypothetical batch driver that fanned out to N hosts could
  benefit from async I/O at the runtime level (#1411), but not
  from kernel-level async at the transport (#1593). The thread
  count of the synchronous path is already O(N) per connection
  (one generator + one receiver + one disk-commit, at most), so
  scaling to thousands of concurrent SSH child processes hits
  process-table and memory limits long before it hits a stdio I/O
  ceiling.
- **Batch RTT amortisation.** io_uring on pipe FDs (per #1859)
  can submit multiple pipe reads in one `io_uring_enter` syscall.
  The win is `read` syscall count, not bytes per second. Whether
  this is measurable in end-to-end transfer time is an open
  question - cite `docs/audits/iouring-pipe-stdio.md` recommendation
  acceptance criterion: "measurable reduction in `read`/`write`
  syscalls for a 1 GiB rsync-over-SSH transfer, no correctness
  regressions, identical wire bytes vs the `Std` path".

Related audits already touch this terrain:

- `docs/audits/splice-ssh-stdio.md` (task #1860) - zero-copy via
  `splice(2)` / `vmsplice(2)` on the file <-> pipe edges. Higher
  expected payoff than async-on-pipes because it removes a
  user-space copy entirely on the bulk-data sub-path. Out of scope
  here.
- `docs/audits/ssh-socketpair-vs-pipes.md` (tasks #1686 / #1689) -
  consolidating the two half-duplex pipes into one bidirectional
  socketpair. Reduces the FD count and exposes a real socket FD
  that can plug into the existing daemon socket fast-path
  factories. Async I/O on the resulting socket would route through
  `IoUringSocketReader` / `IoUringSocketWriter` rather than a
  pipe-specific factory, which is a separate (and arguably easier)
  integration. Out of scope here.
- `docs/audits/iouring-pipe-stdio.md` (task #1859) - the Linux
  pipe-FD io_uring path that this audit explicitly defers to.

## rsync-protocol constraint

The wire protocol pipeline is single-threaded and ordered within
each role. Cite `docs/architecture/parallelization.md:50-90`:

- File indices must be processed in order; the sender sends deltas
  in that order, the receiver acknowledges them in that order.
- The network-facing part of each role is single-threaded by design
  (`docs/architecture/parallelization.md:122`).
- The SPSC disk-commit channel
  (`crates/transfer/src/pipeline/spsc.rs`, capacity 128 slots)
  decouples disk I/O from network I/O on the receiver, but does
  not change the wire-side ordering.

The implication for async transport I/O is direct: making the
`read` / `write` on the SSH stdio pipe async does not unlock new
parallelism in the wire protocol. It can only:

- amortise syscalls (io_uring batched submission, SQPOLL),
- overlap pipe I/O with disk I/O when the SPSC channel is full
  (the network thread's `read` would currently block on the pipe;
  a non-blocking variant could yield to other work, but in practice
  the network thread has no other work because the pipeline is
  serial).

The honest statement is that any wins are second-order. This is
consistent with the policy already documented at task #1197
(`Document single-threaded wire protocol pipeline limitation`,
status: done). Async I/O at the transport is not a workaround for
that limitation - it operates strictly under the same constraint.

## Recommendation

1. **Defer the runtime-level question to #1411.** Adopting tokio
   in `crates/rsync_io/src/ssh/` and `crates/core/src/client/remote/`
   is a substantially larger change than this audit covers. It
   should be evaluated end-to-end (handshake, file list, deltas,
   stats) against measured baselines, including the current
   embedded-ssh tokio bridge as a reference point.
2. **Prefer io_uring on pipe FDs on Linux 6.x kernels.** The work
   is scoped, additive, and parity-matched with the existing
   `fast_io` factories per `docs/audits/iouring-pipe-stdio.md`. No
   new wire-protocol risk. Phase-1 integration there subsumes the
   Linux-side answer to this audit.
3. **Keep sync blocking I/O on macOS, BSD, Windows, and older
   Linux kernels.** The expected win on those targets is small.
   The thread-per-direction topology is correct, well-tested, and
   parity-matched with upstream rsync's blocking `read`/`write`
   pattern in `io.c`.
4. **Do not introduce epoll / kqueue plumbing inside oc-rsync's
   SSH transport.** The two-FD-per-connection topology gives no
   readiness multiplexing benefit that justifies the
   `O_NONBLOCK` + `EAGAIN` retry surface area.

## Phasing

- **Phase 1 (this audit).** Document the scope, defer Linux to
  #1859 and runtime-level questions to #1411. No code changes.
- **Phase 2 (#1859).** Implement `IoUringPipeReader` /
  `IoUringPipeWriter` mirroring the daemon socket factories,
  gated behind `IoUringPolicy::Auto` / `Enabled` and the existing
  `is_io_uring_available` probe. Acceptance criteria are
  enumerated in `docs/audits/iouring-pipe-stdio.md`.
- **Phase 3 (#1411).** Revisit the runtime-level async question
  once Phase 2 has measured the syscall-amortisation win. If
  Phase 2 shows the win is meaningful, Phase 3's case strengthens
  (a runtime can host concurrent SSH connections from a batch
  driver). If Phase 2 shows the win is negligible, Phase 3 is
  also unmotivated for performance reasons - though it may still
  be motivated by ergonomic concerns (the embedded-ssh path
  already needs a runtime).

## Open questions

- **Measurement methodology.** Latency-bound benchmarks (e.g.,
  `rsync` of a 1 KB file from a remote host) versus throughput-
  bound benchmarks (1 GiB single-file) stress different parts
  of the transport. The Phase 2 acceptance criterion in #1859
  is throughput-oriented (syscall count over a 1 GiB transfer);
  a latency-oriented comparison may also be informative. Open.
- **Embedded SSH evaluation.** The `embedded-ssh` feature
  (`crates/rsync_io/src/ssh/embedded/`) already runs a tokio
  current-thread runtime and bridges async russh to a synchronous
  `Read` / `Write` interface via channels. Whether this bridging
  layer is a measurable cost (one extra `Vec<u8>` allocation per
  chunk plus `mpsc::Sender::blocking_send` /
  `mpsc::Receiver::recv` per chunk) and whether the russh path
  can replace the subprocess path on Linux/macOS for some
  deployments are separate questions. Open; not in scope here.
- **Interaction with #1686 socketpair migration.** If the SSH
  child stdio migrates from two pipes to one bidirectional Unix
  socketpair, the existing `IoUringSocketReader` /
  `IoUringSocketWriter` factories apply directly without a new
  pipe-specific factory. The Linux async path would route
  through socket fast paths instead of pipe ops. This shifts
  the engineering burden from #1859 to a thinner integration
  layer on top of the daemon socket infrastructure. Open.

## Cross-references

- `docs/audits/iouring-pipe-stdio.md` (task #1859) - Linux
  io_uring on pipe FDs. Subsumes the Linux side of this audit.
- `docs/audits/splice-ssh-stdio.md` (task #1860) - zero-copy
  splice / vmsplice on the file <-> pipe edges. Complementary
  zero-copy story.
- `docs/audits/ssh-socketpair-vs-pipes.md` (tasks #1686, #1689) -
  socketpair migration for the SSH wire and stderr channel.
- `docs/audits/macos-dispatch-io.md` (task #1653) - macOS
  async-I/O backend evaluation.
- `docs/audits/bsd-aio.md` (task #1654) - FreeBSD / NetBSD POSIX
  AIO evaluation.
- `docs/architecture/parallelization.md` - wire protocol pipeline
  single-threaded constraint.
- Tasks: #1411 (async runtime for SSH transport, deferred),
  #1593 (this audit), #1797 / #1805 (related transport tasks
  cross-referenced for traceability).

## Upstream evidence

A recursive grep for `\bepoll\b`, `\bkqueue\b`, `\baio_(read|write)\b`,
`io_uring`, and `dispatch_io` under
`target/interop/upstream-src/rsync-3.4.1/` returns no matches in the
data path. Upstream rsync 3.4.1 performs all SSH stdio I/O via plain
blocking `read(2)` / `write(2)` on the inherited pipe FDs (`io.c`).
Any async I/O on this path is therefore a pure oc-rsync optimisation
with no wire-protocol implication, mirroring the conclusions reached
in `docs/audits/iouring-pipe-stdio.md` and
`docs/audits/splice-ssh-stdio.md`.
