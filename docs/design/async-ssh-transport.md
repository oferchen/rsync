# Async I/O for the SSH Transport Path

Tracking issue: #1593

## 1. Current SSH transport

The SSH data channel is a spawned `ssh` subprocess whose inherited stdio is
treated as the rsync byte stream. Implementation lives in:

- `crates/rsync_io/src/ssh/builder.rs` - `SshCommand` builder and
  `std::process::Command::spawn()` call site.
- `crates/rsync_io/src/ssh/connection.rs` - `SshConnection`, `SshReader`,
  `SshWriter`. Read/Write halves wrap `std::process::ChildStdout` /
  `ChildStdin` directly; both halves use blocking pipe I/O.
- `crates/rsync_io/src/ssh/aux_channel.rs` - background thread drains
  `ChildStderr` to avoid pipe-buffer deadlock.
- `crates/rsync_io/src/ssh/embedded/` - speculative russh-based client
  (config, connect, cipher, auth, handler) gated behind embedded-ssh work
  in #1782 and follow-ups; not wired into the production transport.

The orchestration that consumes these halves is in
`crates/core/src/client/remote/invocation/` (subprocess command building)
and `crates/core/src/client/remote/daemon_transfer/orchestration/` (the
bidirectional pump that copies receiver -> stdin and stdout -> generator).

## 2. Bottleneck

Each rsync session is two halves: network reads and disk writes. With
synchronous `ChildStdout::read`, the receiver thread blocks on the pipe
while the disk-writer thread is otherwise free, and vice versa. The two
phases serialise inside a single thread when the multiplex demuxer reads
a frame, hands it to the writer, and only then issues the next read.
On a high-RTT or low-bandwidth link the disk is idle during the wait;
on a slow disk the network is idle during the write. The current
SPSC pipeline (`pipeline/spsc.rs`) hides some of this for daemon TCP,
but SSH stdio cannot use the io_uring socket fast path (mod.rs:57-75)
and therefore relies entirely on blocking `read`/`write` syscalls.

## 3. Async candidates

### tokio::process::Child

`tokio::process::Child` exposes `ChildStdin`/`ChildStdout` that implement
`AsyncRead`/`AsyncWrite`. The crate already pulls in `tokio` for the
daemon listener, so the runtime cost is amortised. Pros: minimal surface
change - swap `std::process::Command` for `tokio::process::Command`,
expose async halves, drive the bidirectional pump with `tokio::select!`
or `tokio::io::copy_bidirectional`. Cons: spawning a subprocess still
costs an `execve`; we still depend on the system `ssh` binary; and the
existing connect-watchdog and stderr-drain threads need to be ported to
tokio tasks (or kept as blocking helpers via `spawn_blocking`).

### Embedded russh (#1782 and follow-ups)

The `crates/rsync_io/src/ssh/embedded/` tree is the staging area for a
russh-based native client. Eliminating the subprocess removes
`execve`/`fork` overhead, makes connection setup observable in-process,
and gives full control over keepalives, channel windows, and cipher
negotiation. Pros: a single tokio task graph for SSH + framing; ability
to multiplex auxiliary channels without an extra socketpair (#1782
unblocks); easier integration with the existing `tokio` daemon runtime.
Cons: russh is a much larger surface than `Command::spawn`, owning crypto
choices we currently delegate to OpenSSH; key/agent compatibility,
known-hosts behaviour, and config-file parity all become our problem.

## 4. Bench question

Async overlap only pays back its scheduling overhead when the two halves
have meaningfully different latencies. Workloads where overlap is
expected to win:

- High-RTT links (>= 50 ms) where each `read` waits a full RTT.
- Slow rotational or networked destination disks where `write` dominates.
- Many-small-files transfers with frequent `flush` boundaries.

Workloads where overlap is expected to be neutral or negative:

- LAN or loopback SSH where pipe latency is microseconds.
- Single-large-file transfers already saturated by the kernel pipe
  buffer.
- CPU-bound paths (delta computation, software MD5 fallback) where the
  bottleneck is not I/O.

The benchmark plan is to run `scripts/benchmark_remote.sh` against
representative corpora over both LAN and an artificially shaped
high-RTT link (`tc qdisc add dev ... netem delay 100ms`), comparing
synchronous, tokio-process, and embedded russh variants on identical
inputs.

## 5. Recommendation

Stage async SSH behind a `--features async-ssh` cargo feature, default
off. Implementation order:

1. Add the feature gate and a tokio-process-backed `SshConnection`
   alongside the existing `std::process` implementation, sharing the
   builder, watchdog, and stderr-drain logic via traits.
2. Run `scripts/benchmark_remote.sh` and the interop harness on both
   variants. Promote async SSH to default only when the benchmark shows
   a sustained > 10% wall-clock improvement on at least one supported
   corpus without regressing the LAN baseline.
3. Treat embedded russh (`crates/rsync_io/src/ssh/embedded/`) as a
   separate, longer-running track; do not gate the async-pump work on
   it. Once russh is production-ready, swap the transport behind the
   same feature flag.

Until benched, the synchronous `std::process` SSH transport remains the
default and supported path.
