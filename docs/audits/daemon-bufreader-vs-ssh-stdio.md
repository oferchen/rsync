## Daemon BufReader vs SSH raw stdio I/O

Tracking issue: oc-rsync task #1039.

## Summary

The daemon and SSH transports take different reader shapes before the
data phase. The daemon wraps the `TcpStream` in a default-capacity 8 KiB
`BufReader` for the line-based `@RSYNCD:` handshake; the SSH transport
never wraps `ChildStdout` in any `BufReader` at all. After handshake,
both paths converge on a 64 KiB `BufReader` chained ahead of
`MplexReader` (`crates/transfer/src/lib.rs:489`). The audit question is
whether the pre-handshake asymmetry leaves syscalls on the table that an
explicit `BufReader::with_capacity` could batch away, and whether 64 KiB
is the right post-handshake size.

## 1. Two transport paths

Daemon TCP (server side):

- `crates/daemon/src/daemon/sections/session_runtime.rs:220` and
  `crates/daemon/src/daemon/sections/proxy_protocol.rs:216` -
  `BufReader::new(stream)` (default 8 KiB) for textual greeting,
  module, options.
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:48`
  drains `reader.buffer()` into a `Vec<u8>`, sets `TCP_NODELAY`, clones
  the raw `TcpStream`, and hands both halves down. The 8 KiB buffer is
  discarded; only the residual bytes survive.

Daemon TCP (client side):
`crates/core/src/client/remote/daemon_transfer/connection/mod.rs:146` -
8 KiB `BufReader` for handshake only, raw `TcpStream` for the data
phase.

SSH stdio (both sides):

- `crates/rsync_io/src/ssh/connection.rs:211` defines `SshReader`,
  which holds a bare `ChildStdout` and forwards `read` 1:1 with no
  buffering.
- `crates/core/src/client/remote/ssh_transfer.rs:551` splits the
  `SshConnection` into `(reader, writer, child)` and passes the raw
  reader straight into `perform_handshake` and the per-message loop.
- The only `BufReader` on the SSH side wraps the auxiliary stderr
  channel (`crates/rsync_io/src/ssh/aux_channel.rs:209`), a side
  channel that never carries protocol data.

Post-handshake both transports go through
`BufReader::with_capacity(64 * 1024, _)` ahead of `MplexReader`
(internal 32 KiB payload buffer at
`crates/protocol/src/multiplex/reader.rs:103`).

## 2. Question

Default `BufReader` capacity is 8 KiB
(`std::io::DEFAULT_BUF_SIZE`). For the daemon handshake that is far
more than enough - the entire `@RSYNCD:` exchange fits in a few
hundred bytes. For the SSH handshake the 4-byte binary version
exchange runs unbuffered against `ChildStdout`, where pipe reads can
return short and `read_exact(&mut [0u8; 4])` can loop. Once we cross
into the data phase, both sides share a 64 KiB read buffer feeding
`MplexReader`, which issues paired short reads (4-byte header, then
payload). The shared buffer should coalesce these; the question is
whether 64 KiB is too small for SSH's pipe path or too large for
small-file workloads.

## 3. Profile plan

Workload A (large stream): single 1 GiB file pull, both daemon and
SSH, counting `read` syscalls and per-call byte counts.

```
strace -f -e trace=read -c oc-rsync rsync://localhost/m/file ./out
strace -f -e trace=read -c oc-rsync remote:/file ./out
```

Workload B (metadata-bound): 100 K small files (~1 KiB each), same
counters. Header/payload alternation dominates and the SSH path is
expected to show the highest read count because pipe reads do not
coalesce as aggressively as TCP `recv`.

Inside the binary, instrument `MplexReader::read_message` to record
`(header_read_size, payload_read_size, calls)` per frame and dump a
histogram on shutdown. Compare against a build that wraps `SshReader`
with `BufReader::with_capacity(64 * 1024, _)` already at handshake
entry, mirroring the daemon shape, and against a 256 KiB variant to
test diminishing returns.

## 4. Tuning knobs

- `BufReader::with_capacity(N)` sized to the workload. 64 KiB matches
  the existing post-handshake choice and `MplexReader`'s envelope for
  typical DATA frames; 256 KiB amortises pipe reads at the cost of a
  larger idle footprint; 4 KiB suits handshake-only paths.
- `O_NONBLOCK` on `ChildStdout` plus a `mio` readiness loop, letting
  us batch `read` calls only when the kernel signals data ready.
  Useful only if profiling shows blocked partial reads dominate.
- Vectored reads (`Read::read_vectored`) into the `MplexReader`
  payload buffer and a small header scratch buffer, collapsing the
  header+payload pair into one syscall when the kernel returns enough
  bytes. `TcpStream` and `ChildStdout` both implement `read_vectored`.

## 5. Decision

Tuned `BufReader` is the right answer in the short term. Concretely:

1. Wrap the SSH handshake reader in
   `BufReader::with_capacity(64, stdin)` so the four-byte version
   exchange collapses to one syscall, mirroring the daemon's
   buffer-then-chain pattern.
2. Keep the post-handshake 64 KiB buffer on both transports as
   today; bump to 256 KiB only if Workload B profiling shows
   `read` dominating wall time on the pipe path.
3. Defer `O_NONBLOCK` plus `mio` until profiling proves blocked
   partial reads are a meaningful share of wall time on the SSH
   path.
4. Defer vectored reads until `MplexReader` exposes a single
   `read_frame_into(&mut [IoSliceMut])` entry point - the current
   header/payload split would need reshaping first, and the win is
   bounded by what a 64 KiB buffer already captures.

Raw stdio without any buffering is the wrong long-term shape for the
SSH path: pipe reads are cheaper to batch than to repeat, and the
daemon-side buffer-then-chain idiom is already proven.
