# SSH transport

`oc-rsync` reaches a remote endpoint by spawning an external `ssh` client
(or any other remote shell selected with `--rsh=...`) and tunnelling the
rsync protocol over the child's stdin and stdout. The child's stderr is
drained into a bounded ring buffer so that diagnostic output from the
remote (host-key prompts, `Permission denied`, MOTD lines, verbose
`-vvv` traces, anything chatty in `~/.ssh/rc`) is surfaced to the user
without ever stalling the transfer.

The default stderr endpoint is an anonymous pipe created by
`pipe2(2)`. That is portable and sufficient for typical workloads. For
long-running sessions with very chatty remote children, an opt-in Cargo
feature swaps the pipe for a `socketpair(2)` on Unix targets.

## SSH stderr capture (socketpair, opt-in)

### What it does

The `ssh-socketpair-stderr` Cargo feature on the `rsync_io` crate
changes how the parent process receives the SSH child's stderr stream.
Instead of the default anonymous pipe, it uses one half of a
`socketpair(AF_UNIX, SOCK_STREAM, 0)` and hands the other half to the
child as its stderr file descriptor. Everything downstream (the bounded
64 KiB ring buffer, real-time forwarding to the parent's own stderr,
the snapshot accessor used in error messages) is unchanged.

### Why it exists

A pipe-backed stderr can deadlock when the child writes faster than the
parent drains. The Linux pipe buffer is 64 KiB by default; once it is
full, the child blocks in `write(2)` and never makes progress, while the
parent is blocked elsewhere (waiting on the wire, on an `accept(2)`, on
disk I/O). The transfer hangs until either side times out.

A `socketpair`-backed endpoint mitigates this in two ways:

1. The default kernel buffer is larger (around 208 KiB on Linux,
   platform-dependent on the BSDs and macOS), which absorbs longer
   bursts before back-pressure kicks in.
2. The parent can call `shutdown(SHUT_RD)` as an out-of-band wake-up
   when the transfer is finishing, which is cleaner than relying on the
   child to close its end. That matters for SSH multiplexing
   (`ControlMaster`) and `ssh-askpass` cases where a helper inherits the
   write end and outlives the rsync run.

The kernel object is otherwise interchangeable: byte semantics,
`O_NONBLOCK` support, and `epoll`/`kqueue` registration are identical.

### When to enable it

Enable the feature if you regularly:

- run `oc-rsync` over slow or high-latency links where back-pressure
  matters,
- use SSH with `-vvv` or `LogLevel DEBUG3` on either side,
- have `~/.ssh/rc`, login banners, or `ForceCommand` wrappers that emit
  large amounts of text on every connection,
- multiplex many connections through a single `ControlMaster` whose
  helper processes outlive individual transfers.

For typical interactive use or short-lived scripted transfers, the
default pipe-backed endpoint is fine and no rebuild is required.

### How to enable it

The feature is selected at compile time on the `rsync_io` crate:

```sh
cargo build --release --features rsync_io/ssh-socketpair-stderr
```

There is no runtime toggle and no environment variable. A binary built
without the feature uses anonymous pipes; a binary built with the
feature uses socketpairs on Unix and silently falls back to pipes on
file-descriptor exhaustion. To switch back, rebuild without the
feature.

### Platform notes

| Platform                | Backend when feature is on    | Backend when feature is off |
|-------------------------|-------------------------------|------------------------------|
| Linux                   | `socketpair(AF_UNIX)`         | `pipe2(2)`                   |
| macOS                   | `socketpair(AF_UNIX)`         | `pipe2(2)`                   |
| FreeBSD and other Unix  | `socketpair(AF_UNIX)`         | `pipe2(2)`                   |
| Windows                 | anonymous pipe (feature has no effect today) | anonymous pipe |

The Windows TCP-loopback shim sketched in the design doc is not yet
wired up; on Windows the feature compiles but has no effect at runtime,
and the child's stderr continues to flow through the anonymous pipe
that `tokio::process::Command` creates.

The user-visible behaviour is the same with either backend: the same
bytes are surfaced through the same error-reporting code path. Only the
deadlock resistance and the wake-up mechanism differ.

### References

- Design: [`docs/design/socketpair-stderr-channel.md`](design/socketpair-stderr-channel.md)
- Audits: [`docs/audits/ssh-stderr-handling.md`](audits/ssh-stderr-handling.md),
  [`docs/audits/ssh-socketpair-vs-pipes.md`](audits/ssh-socketpair-vs-pipes.md),
  [`docs/audits/ssh-socketpair-claim-verification.md`](audits/ssh-socketpair-claim-verification.md)
- Trackers: SSE-1..SSE-8 (issues #2370-#2377).
