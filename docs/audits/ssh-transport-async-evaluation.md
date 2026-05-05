# Async I/O evaluation for the SSH transport path

Tracker: oc-rsync tasks #1593 and #1411. Adjacent in-flight tasks:
#1795-#1797 (`SshTransport` consolidation), #1805-#1806 (sync/async
bridge), #1889-#1892 (SSH benchmarking and backpressure).

Last verified: 2026-05-05. Static analysis only.
This audit complements `docs/audits/async-ssh-transport.md`
(#1593 baseline, kernel-level question) and the
`docs/audits/tokio-dependency-boundary-2026.md` re-verification
(#3706). It focuses on the runtime-level question that #1411
owns: should `crates/rsync_io/src/ssh/` and
`crates/core/src/client/remote/` be restructured around an async
runtime, and where does async pay off versus where would it be
wasted complexity?

## 1. Methodology

Static analysis only. Evidence sources: direct reads of
`crates/rsync_io/src/ssh/` (subprocess path),
`crates/rsync_io/src/ssh/embedded/` (russh path),
`crates/core/src/client/remote/` (consumer wiring), manifests at
`crates/rsync_io/Cargo.toml:16-38` and workspace root
`Cargo.toml:188-189, :207`, plus cross-references against five
audits already on master and the merged migration plan
`docs/design/async-migration-plan.md` (#1594).

Scope: #1593's narrower kernel-level question is settled by
`docs/audits/async-ssh-transport.md` and deferred to #1859 on
Linux; this audit re-states that conclusion without
re-litigating it. The focus is #1411. Out of scope: receiver
async (Phase 4 of #1594), io_uring + rayon composition (Phase
5), protocol changes.

## 2. Current SSH transport architecture

Two concurrent paths exist: a subprocess path (default) and a
russh-based embedded path (behind the `embedded-ssh` cargo
feature gated at `crates/rsync_io/Cargo.toml:32`).

### 2.1 Subprocess path - spawn site

`SshCommand::spawn` is the single spawn site:
`crates/rsync_io/src/ssh/builder.rs:307` declares
`pub fn spawn(&self) -> io::Result<SshConnection>`. Lines 321
constructs the `Command`; lines 322-323 wire stdin and stdout
as anonymous pipes via `Stdio::piped()`; line 334 calls
`configure_stderr_channel` to install a Unix `socketpair(2)`
with anonymous-pipe fallback; line 336 runs `command.spawn()`,
the only fork/exec for the path; lines 340-351 take ownership
of the parent ends.

### 2.2 Subprocess path - I/O sites

- `crates/rsync_io/src/ssh/connection.rs:30-39` holds
  `Option<ChildStdin>` and `Option<ChildStdout>` on
  `SshConnection`.
- `crates/rsync_io/src/ssh/connection.rs:178-208`
  `SshConnection::split` returns `(SshReader, SshWriter,
  SshChildHandle)`.
- `crates/rsync_io/src/ssh/connection.rs:217-221`
  `impl Read for SshReader` calls `self.stdout.read(buf)` -
  blocking `read(2)` on the pipe FD.
- `crates/rsync_io/src/ssh/connection.rs:229-237`
  `impl Write for SshWriter` calls `self.stdin.write(buf)`
  and `self.stdin.flush()` - blocking `write(2)` on the pipe FD.
- `crates/rsync_io/src/ssh/connection.rs:498-542` provide
  the same impls on the un-split connection. No `O_NONBLOCK`
  is set on either FD.

### 2.3 Subprocess path - auxiliary threads

Three OS threads peak per connection:
`PipeStderrChannel::spawn`
(`crates/rsync_io/src/ssh/aux_channel.rs:108-117`,
`ssh-stderr-drain-pipe`),
`SocketpairStderrChannel::spawn`
(`crates/rsync_io/src/ssh/aux_channel.rs:159-172`,
Unix only), and `ConnectWatchdog::arm`
(`crates/rsync_io/src/ssh/connection.rs:280-315`,
condvar-wait then kill the child on timeout). The drain loop is
a blocking `BufReader::read_until(b'\n', ...)` at
`crates/rsync_io/src/ssh/aux_channel.rs:208-228`; bytes accumulate
in a bounded 64 KiB ring
(`crates/rsync_io/src/ssh/aux_channel.rs:39, 232-242`).

### 2.4 Subprocess path - transport driver

The driver runs entirely on the calling thread, no executor.
`build_ssh_connection`
(`crates/core/src/client/remote/ssh_transfer.rs:248-332`)
constructs `SshCommand` and calls `ssh.spawn()` at line 300.
`run_server_over_ssh_connection`
(`crates/core/src/client/remote/ssh_transfer.rs:545-553`) calls
`connection.split()` at line 551, drives
`crate::server::perform_handshake` at lines 560-575, then
`crate::server::run_server_with_handshake` at lines 585-593,
then drops the writer to signal EOF and waits on the child at
lines 596-605.

### 2.5 Embedded path - russh + tokio

Owns its own tokio runtime internally:

- `crates/rsync_io/src/ssh/embedded/mod.rs:9-37` gates every
  embedded module behind `#[cfg(feature = "embedded-ssh")]`.
- `crates/rsync_io/src/ssh/embedded/connect.rs:107-122`
  `connect_and_exec` is a sync `pub fn` that builds a
  current-thread tokio runtime and `block_on`s
  `connect_and_exec_async`.
- `crates/rsync_io/src/ssh/embedded/connect.rs:125-225`
  performs the full async handshake:
  `resolve_host(...).await`,
  `russh::client::connect(...).await`,
  `authenticate(...).await`,
  `handle.channel_open_session().await`,
  `channel.exec(true, remote_command).await`.
- `crates/rsync_io/src/ssh/embedded/connect.rs:169-170` builds
  the bridging channels:
  `std::sync::mpsc::sync_channel::<Vec<u8>>(64)` (read side),
  `tokio::sync::mpsc::channel::<Vec<u8>>(64)` (write side).
- `crates/rsync_io/src/ssh/embedded/connect.rs:180-209`
  spawns a `tokio::spawn` task that drives a
  `tokio::select!` loop until EOF.
- `crates/rsync_io/src/ssh/embedded/connect.rs:31-58`
  `impl std::io::Read for ChannelReader` blocks on
  `self.rx.recv()`.
- `crates/rsync_io/src/ssh/embedded/connect.rs:64-79`
  `impl std::io::Write for ChannelWriter` calls
  `self.tx.blocking_send(buf.to_vec())`.

Other tokio touchpoints:

- `crates/rsync_io/src/ssh/embedded/resolve.rs:26-35`
  `resolve_host` is `pub async fn` and calls
  `tokio::net::lookup_host(&lookup_str).await` at line 32.
- `crates/rsync_io/src/ssh/embedded/auth.rs:233-262`
  `authenticate` is `pub async fn` driving russh's auth
  methods.
- `crates/rsync_io/src/ssh/embedded/connect.rs:149-155`
  `tokio::time::timeout` enforces the connect timeout.

The embedded consumer at
`crates/core/src/client/remote/embedded_ssh_transfer.rs:282-348`
(`run_transfer_over_embedded_ssh`) is purely synchronous; it
calls `connect_and_exec` at lines 304-309 and treats the
returned `(ChannelReader, ChannelWriter)` exactly as it would
the subprocess `(SshReader, SshWriter)` pair.

### 2.6 Convergence

Both paths converge on the same downstream API: a `Read +
Write` half-duplex pair driven through `perform_handshake` and
`run_server_with_handshake`, fully synchronously, on the
calling thread. Async machinery exists only inside the
embedded crate's connect bridge and is hidden behind a sync
facade per
`docs/audits/tokio-dependency-boundary-2026.md:226-234`.

## 3. Where async would help, where it would not

The rsync wire pipeline is single-threaded and order-preserving
within each role
(`docs/audits/async-ssh-transport.md:31-35,270-299`,
`docs/architecture/parallelization.md:50-90,122`). Async at the
transport unlocks no new parallelism on the wire side. Per
sub-phase:

| Sub-phase | Sync cost | Async win |
|-----------|-----------|-----------|
| DNS resolution | One blocking lookup | Marginal. `tokio::net::lookup_host` already async in embedded; subprocess defers to `ssh(1)`. |
| TCP connect | One `connect(2)` + RTT | None for a single transfer. Wins only when fanning out to N hosts. |
| SSH key exchange | Multiple round trips, CPU-bound crypto | None. KEX is strict request-response; no overlap to exploit. |
| Authentication | One or more round trips per method | None. Same shape as KEX. |
| Rsync handshake | Versioned greeting, capability bits | None. Strictly sequential per `crates/core/src/client/remote/ssh_transfer.rs:560-575`. |
| Bulk read/write | One `read(2)` and one `write(2)` per chunk | Marginal. Async overlaps network and disk, but `crates/transfer/src/pipeline/spsc.rs` already decouples those. |
| Stderr drain | Blocking `read_until` per line | None. Already on its own OS thread. |

The single async-shaped win in the bulk loop is the one in
`docs/audits/async-ssh-transport.md:284-299`: a non-blocking
read could yield when the SPSC channel is full. In practice the
network thread has no other work because the pipeline is
serial, so the win is second-order.

KEX and auth deserve special note. They are sync-blocking by
definition, each step requiring the previous one's output.
Async would let other work run on the same thread while a
KEX round trip is in flight, but on a single connection there
is no other work; on multiple concurrent connections (a batch
driver), async would help, and it is the only phase where this
is true. See section 8.

The russh path already serialises KEX and auth on its internal
runtime
(`crates/rsync_io/src/ssh/embedded/connect.rs:149-167`,
`crates/rsync_io/src/ssh/embedded/auth.rs:233-262`); async
inside russh is structural, not a performance choice we can
revisit.

## 4. russh-based embedded path vs spawn-ssh-binary path

| Property | Subprocess | Embedded russh |
|----------|------------|----------------|
| Code surface | `crates/rsync_io/src/ssh/{builder.rs, connection.rs, aux_channel.rs, parse.rs, operand.rs}` | 9 files under `crates/rsync_io/src/ssh/embedded/` |
| Tokio dep | None at the manifest level | Required by `embedded-ssh` feature (`crates/rsync_io/Cargo.toml:32`) |
| KEX / cipher / MAC owner | `ssh(1)`, OS-installed | russh in-process |
| OS threads peak | 3 (caller, stderr drain, watchdog) | Caller + tokio current-thread |
| Failure mode mapping | `crates/core/src/client/remote/ssh_transfer.rs:487-506` `map_child_exit_status` (127 -> `CommandNotFound`, 255 -> `CommandFailed`, signal -> `CommandKilled`) | `crates/rsync_io/src/ssh/embedded/error.rs` `SshError::{Connect, Timeout, AuthenticationFailed, HostKeyMismatch, DnsResolution, Io}` |
| Stderr capture | 64 KiB ring at `aux_channel.rs:39` | n/a; channel close is the EOF signal |
| Wire identity | Whatever `ssh(1)` produces; cipher per `docs/audits/ssh-cipher-compression.md` | Cipher selection at `crates/rsync_io/src/ssh/embedded/cipher.rs` |
| Default | yes; only path on Windows | opt-in via `--features embedded-ssh`; triggered by `ssh://` URL operands per `crates/core/src/client/remote/embedded_ssh_transfer.rs:52-54` |

For the #1411 decision the embedded path is the strongest
existing argument that an async runtime in the SSH transport is
viable: it already exists, already runs in production for
`ssh://` URLs, and already hides its async machinery behind a
sync `Read`/`Write` facade. Whether the subprocess path needs
the same treatment is the separate question.

## 5. Sync/async bridge problem at the engine boundary

### 5.1 Subprocess path: no bridge

`SshReader` and `SshWriter` wrap `std::process::ChildStdin` and
`std::process::ChildStdout`
(`crates/rsync_io/src/ssh/connection.rs:213-237`). `read(2)`
and `write(2)` on those FDs are direct kernel calls. There is
nothing to bridge.

### 5.2 Embedded path: channels with `block_on`

The embedded path bridges via two mpsc channels at
`crates/rsync_io/src/ssh/embedded/connect.rs:169-170`:

- Read side: `std::sync::mpsc::sync_channel::<Vec<u8>>(64)`.
  Sync sender is the bridge task at lines 180-209; sync
  receiver is `ChannelReader::read` blocking on `recv()`.
- Write side: `tokio::sync::mpsc::channel::<Vec<u8>>(64)`.
  Sync sender is `ChannelWriter::write` calling
  `self.tx.blocking_send(buf.to_vec())` - the only
  `blocking_send` use in the codebase, and it cannot be
  called from inside a tokio runtime per the test comment
  at `crates/rsync_io/src/ssh/embedded/connect.rs:273-274`.

Cost per chunk: read side does one `Vec<u8>` allocation (russh
`Bytes`-to-`Vec` at line 186), one mpsc send, one mpsc recv,
plus `ReadBuffer { data, offset }` partial-read amortisation
at lines 25-29. Write side does one `to_vec()` at line 71, one
`blocking_send`, one `select!` arm, one `h.data(channel_id,
data).await`. The runtime itself is current-thread
(`connect.rs:112-115`) so it lives on the caller's thread.

### 5.3 Dual-runtime risk

`docs/design/async-migration-plan.md:395-408` documents the
risk: a process using both a daemon multi-thread runtime and an
embedded SSH current-thread runtime ends up with two reactors,
two timer wheels, two thread pools. Today this is impossible
because daemon and SSH client are different binary
invocations. If #1411 lands a runtime in the SSH transport, the
question is whether to share, build per-connection, or build
process-wide. The migration plan lands on "process-wide tokio
runtime, lazy construction"
(`docs/design/async-migration-plan.md:402-408`).

#1805 and #1806 own this bridge problem explicitly. The
requirement is that the bridge must not regress sync-only
build startup latency. An always-on tokio runtime adds
100-300 microseconds and a few MB of RSS at process start
(`docs/design/async-migration-plan.md:395-401`); a lazy
runtime adds nothing for local-only transfers.

## 6. Stdio (pipe FD) vs socketpair vs full async russh

Three possible kernel-object substrates for the data channel:

### 6.1 Anonymous pipes (default)

`crates/rsync_io/src/ssh/builder.rs:322-323` configures
`Stdio::piped()` for stdin and stdout. Two half-duplex pipes,
default 64 KiB kernel buffer on Linux. Cannot be opened with
`FILE_FLAG_OVERLAPPED` on Windows; cannot be registered into
IOCP without rebuilding `Command` from raw HANDLEs
(`docs/audits/async-ssh-transport.md:204-213`). io_uring can
submit `IORING_OP_READ`/`IORING_OP_WRITE` on pipe FDs directly
(`docs/audits/iouring-pipe-stdio.md:56-80`,
`crates/rsync_io/src/ssh/mod.rs:67-71`).

### 6.2 Unix socketpair

`crates/rsync_io/src/ssh/aux_channel.rs:264-285`
(`configure_stderr_channel`) uses `UnixStream::pair()` for
stderr today, falling back to anonymous pipe on failure or
non-Unix targets. The parent end is a `UnixStream` that can go
non-blocking and register with epoll/kqueue/io_uring.

Tasks #1686/#1689 (per
`docs/audits/ssh-socketpair-vs-pipes.md`) track migrating the
data channel from two pipes to one bidirectional socketpair.
Doing so halves FD count, exposes a real socket that plugs into
existing `IoUringSocketReader`/`Writer` factories
(`docs/audits/async-ssh-transport.md:259-266`), but loses
half-duplex EOF semantics (close-of-write becomes a
half-shutdown).

### 6.3 Full async russh

The embedded path. Bypasses the kernel-object question; the
data channel is a russh channel multiplexed over a tokio TCP
socket. Parent owns the entire SSH session.

### 6.4 Tradeoff matrix

| Property | Pipes | Socketpair | Embedded russh |
|----------|-------|------------|----------------|
| Cross-platform | yes | Unix only | yes |
| io_uring async | yes (Linux 5.7+) | yes | n/a (russh owns the runtime) |
| IOCP async | no (anonymous pipes not OVERLAPPED) | n/a | yes (tokio backend) |
| kqueue async | partial | yes | yes |
| FD count (data) | 2 | 1 | 1 TCP socket |
| Half-duplex EOF | natural | needs `shutdown(SHUT_WR)` | russh `channel.eof()` |
| Reuse fast paths | needs new pipe-FD factory | reuses socket factories | russh owns it |
| Process footprint | one `ssh(1)` child | same | none |
| Trust boundary | OS-installed `ssh(1)` | OS-installed `ssh(1)` | russh, less battle-tested |

The cleanest end state is socketpair on Unix with
`IoUringSocketReader`/`Writer`, anonymous pipes on Windows,
embedded russh as opt-in. The runtime question (#1411) sits on
top of all three.

## 7. Backpressure - implicit OS pipes vs explicit (#1892)

### 7.1 Subprocess path

- Read backpressure: a slow consumer leaves bytes in the kernel
  pipe buffer (default 64 KiB on Linux). When full, the remote
  `rsync --server` blocks in `write(2)`. No application-level
  signal.
- Write backpressure:
  `crates/rsync_io/src/ssh/connection.rs:230` is
  `self.stdin.write(buf)`, blocking `write(2)`. Local thread
  blocks in the kernel when the remote pipe is full.
- Stderr drain bounded to 64 KiB
  (`crates/rsync_io/src/ssh/aux_channel.rs:39, 232-242`); the
  drain thread is dedicated so the data channel cannot starve.

### 7.2 Embedded path

- Read backpressure: bounded sync-mpsc of 64 messages
  (`connect.rs:169`). When the consumer does not drain, the
  bridge task blocks in `data_tx.send(...)` at line 186. This
  blocks the tokio current-thread runtime, a known bridge
  hazard.
- Write backpressure: bounded tokio mpsc of 64 messages
  (`connect.rs:170`). `ChannelWriter::write` calls
  `blocking_send` and parks the caller thread on a tokio
  condvar.
- Russh's TCP socket carries kernel-level TCP backpressure
  underneath; bridge channels add a second bounded buffer.

### 7.3 The #1892 problem

#1892 owns explicit backpressure for SSH transport. The gap
is that the rsync engine has no visibility into how full the
OS pipe or bridge channels are. A delta producer that runs
faster than the network can absorb fills the pipe and blocks
in `write(2)`. There is no graceful "slow down" signal; only
block. For most usage this is fine - the producer paces to the
slowest link - but it forecloses on watermark-based scheduling
that a multi-connection batch driver would need.

Async backpressure with explicit watermarks is the typical
runtime answer:
`tokio::sync::mpsc::Sender::reserve()` returns a permit
without sending, letting the producer choose alternative work
when no permit is available. The embedded path already has the
underlying tokio mpsc but hides it behind a blocking send.

## 8. Recommendation with explicit decision criteria

### 8.1 Subprocess path

**Do not adopt an async runtime for the subprocess SSH path on
master.** The status quo is correct:

- The blocking I/O loop matches upstream rsync 3.4.1's
  `io.c::read_buf`/`writefd_unbuffered` semantics byte-for-byte
  (`docs/audits/async-ssh-transport.md:387-397`).
- Kernel-level async wins on pipe FDs are scoped to syscall
  amortisation and tracked by
  `docs/audits/iouring-pipe-stdio.md` (#1859) without a
  runtime.
- The three-thread topology (caller, drain, watchdog) is
  well-tested and bounded.
- Windows cannot benefit (anonymous pipes not OVERLAPPED);
  Linux/macOS/BSD answers all go through pipe-FD io_uring or
  kqueue without a runtime.

If the subprocess path migrates to socketpair (#1686), that
plugs into existing `IoUringSocketReader`/`Writer` factories
without requiring a runtime in the SSH transport.

### 8.2 Embedded path

**Keep the embedded path's tokio runtime exactly as it is.**
russh is async-by-construction with no realistic alternative;
the current sync facade
(`crates/rsync_io/src/ssh/embedded/connect.rs:107-122`)
correctly hides the runtime per
`docs/audits/tokio-dependency-boundary-2026.md:226-234`.

Cleanups worth doing:

- Move bridge-channel backpressure to a watermark-aware API
  once #1892 lands.
- Consider lifting to a process-wide runtime per
  `docs/design/async-migration-plan.md:402-408` only when the
  daemon and SSH client co-locate in the same invocation
  (rare today).

### 8.3 The #1411 runtime-level question

**Defer adoption of an async runtime for the subprocess path
until at least one of these criteria trips.** The criteria are
designed to be falsifiable by static or benchmark evidence:

1. **Multi-host fan-out lands as a feature.** Today
   `crates/core/src/client/remote/ssh_transfer.rs` runs one
   transport per invocation. A batch driver that initiates N
   concurrent SSH connections gets linear thread growth under
   the sync model
   (`docs/audits/async-ssh-transport.md:235-242`). Under
   async, N stays bounded by the runtime's thread count. This
   is the only sub-case where async helps wall-clock time, not
   just syscall count.
2. **Pipe-FD io_uring wins are measured to be marginal (under
   5% throughput) but the syscall count drops substantially.**
   If #1859 shows batched submission matters, the case for a
   runtime that natively schedules many submissions
   strengthens. If #1859 shows the win is in the noise, the
   case weakens.
3. **The receiver pipeline goes async (Phase 4 of #1594,
   `docs/design/async-migration-plan.md:230-271`).** Once the
   receiver lives on a runtime, the SSH transport's sync facade
   forces an extra `block_in_place` or `spawn_blocking` per
   chunk; promoting the transport to native async on the same
   runtime eliminates that.
4. **A user-visible cancellation primitive ships.** Today
   transfer abort is via `Result::Err`. If a user-facing
   `--abort-after=DURATION` or signal-driven graceful cancel
   lands, the sync `read(2)` blocks become a problem because
   they cannot be interrupted from another thread without
   killing the SSH child. The `ConnectWatchdog` does this at
   handshake time only
   (`crates/rsync_io/src/ssh/connection.rs:295-313`).

If none trip, the cost of adopting a runtime exceeds the
benefit: tokio binary size and startup latency
(`docs/design/async-migration-plan.md:395-408`) are real, and
the sync path is correct, well-tested, and mirrors upstream.

If criterion 1 trips, the embedded path's existing tokio
runtime is the natural integration point. If criteria 2 or 3
trip, a process-wide tokio runtime per #1594 is the answer. If
criterion 4 trips, the narrowest fix is a sync cancellation
token signalling the watchdog thread, not a full runtime.

## 9. Open questions

1. **Empirical syscall count on a 1 GiB transfer.** The
   `docs/audits/iouring-pipe-stdio.md` acceptance criterion
   names a syscall reduction target but the baseline is not in
   the repo. #1889 (SSH transport benchmarking) should publish
   the baseline.
2. **CPU cost of the embedded path's bridge per chunk.** The
   two channels and two `Vec<u8>` allocations
   (`crates/rsync_io/src/ssh/embedded/connect.rs:71, :186`)
   are visible to anyone benchmarking. A lock-free SPSC plus a
   `Bytes`-passing API would remove both. Static analysis
   cannot quantify the savings.
3. **Does `IORING_FEAT_FAST_POLL` actually fire on default
   pipe buffers?** Kernel docs say yes; worker pool sizing and
   `O_NONBLOCK` interact non-obviously. #1859 should confirm.
4. **Windows IOCP on anonymous pipes with `OVERLAPPED`.** Today
   `std::process::ChildStdin`/`ChildStdout` are not opened
   with `FILE_FLAG_OVERLAPPED`. A custom `CreateProcess` is
   plausible but invasive; out of scope here.
5. **Does #1686 socketpair migration deprecate #1859?** A
   bidirectional socketpair routes through socket factories,
   not a pipe-specific factory. The pipe-FD work could be
   skipped if socketpair lands first.
6. **#1795-#1797 `SshTransport` consolidation.** Today the
   consumers at
   `crates/core/src/client/remote/ssh_transfer.rs:545-610` and
   `embedded_ssh_transfer.rs:282-348` duplicate the
   handshake-then-server-loop wiring for the two paths. A
   `trait SshTransport: Read + Write` would let the consumer
   pick at runtime. The choice affects how an eventual async
   surface attaches.
7. **`blocking_send` vs `try_send` plus spin.**
   `crates/rsync_io/src/ssh/embedded/connect.rs:71-74` parks
   the caller thread on a tokio condvar. `try_send` plus an
   explicit await-yield would be friendlier to a process-wide
   runtime.

## Cross-references

- `docs/audits/async-ssh-transport.md` (#1593 baseline) -
  scope delimitation and kernel-level pipe-FD analysis.
- `docs/audits/tokio-dependency-boundary-2026.md` (#3706) -
  tokio policy boundary including the embedded SSH bridge.
- `docs/audits/iouring-pipe-stdio.md` (#1859) - Linux
  pipe-FD io_uring fast path.
- `docs/audits/splice-ssh-stdio.md` (#1860) - zero-copy
  splice/vmsplice on file-pipe edges.
- `docs/audits/ssh-socketpair-vs-pipes.md` (#1686, #1689) -
  socketpair migration for SSH wire and stderr.
- `docs/audits/ssh-socketpair-vs-anonymous-pipes-verification.md`
  (#1902) - confirmation that the stderr socketpair landed.
- `docs/audits/ssh-cipher-compression.md` (#2046) - cipher
  and compression negotiation parity.
- `docs/audits/ssh-process-management.md` - SSH child
  lifecycle correctness audit.
- `docs/audits/ssh-transport-timeout-coverage.md` - timeout
  coverage gap analysis.
- `docs/design/async-migration-plan.md` (#1594) - five-phase
  sequencing; SSH transport is Phase 3.
- `docs/architecture/parallelization.md` - single-threaded
  wire-protocol pipeline constraint.
- Tasks: #1411 (this), #1593, #1795-#1797
  (`SshTransport` consolidation), #1805-#1806 (sync/async
  bridge), #1859, #1860, #1889-#1892, #1934.

## Upstream evidence

Upstream rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`)
is single-threaded and uses blocking `read(2)`/`write(2)` on
the inherited stdio descriptors over SSH; see `io.c` for the
read/write pump and `pipe.c` for fork/exec/pipe wiring. There
is no async runtime, no io_uring, no kqueue, no IOCP usage in
the upstream data path. The async question is therefore an
oc-rsync-internal concurrency model question with no
wire-protocol implication. Any async vehicle the project adopts
must produce byte-identical wire output to the upstream sync
path; this is enforced by the goldens under
`crates/protocol/tests/golden/` and by the interop harness
`tools/ci/run_interop.sh`.
