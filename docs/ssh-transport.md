# SSH transport

`oc-rsync` reaches a remote endpoint by spawning an external `ssh` client
(or any other remote shell selected with `--rsh=...`) and tunnelling the
rsync protocol over the child's stdin and stdout. The child's stderr is
drained into a bounded ring buffer so that diagnostic output from the
remote (host-key prompts, `Permission denied`, MOTD lines, verbose
`-vvv` traces, anything chatty in `~/.ssh/rc`) is surfaced to the user
without ever stalling the transfer.

When the `embedded-ssh` Cargo feature is enabled, `ssh://` URI operands
take a separate code path that uses the in-process russh client instead
of spawning `ssh`. The trade-offs between the two paths, and the
historical anchor for why russh is the preferred transport for new
deployments, are documented in
[`ssh-transport-decision-matrix.md`](ssh-transport-decision-matrix.md).

On Unix the SSH child's stderr is one half of a
`socketpair(AF_UNIX, SOCK_STREAM, 0)` (`aux_channel::configure_stderr_channel`),
with an anonymous pipe as the fallback when the socketpair cannot be set
up. This holds for every build: the production `SshCommand::spawn` path
does not depend on a Cargo feature.

## SSH stderr capture (socketpair, default)

### What it does

The parent keeps one half of the socketpair and hands the other half to
the child as its stderr file descriptor. Everything downstream (the
bounded 64 KiB ring buffer, real-time forwarding to the parent's own
stderr, the snapshot accessor used in error messages) is the same as on
the pipe fallback.

The `ssh-socketpair-stderr` Cargo feature on the `rsync_io` crate is on
by default. It exposes the standalone `socketpair_stderr` primitive and,
together with `async-ssh`, the async drain task that registers the
parent half with the tokio reactor so wake-up and shutdown are
event-driven instead of timeout-bounded.

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
Upstream rsync uses the same primitive for its remote-shell child's
stdin and stdout: `pipe.c:57` `piped_child()` creates them with
`util1.c:84` `fd_pair()`, a `socketpair(2)` when `HAVE_SOCKETPAIR` is
defined. Upstream leaves the child's stderr inherited, so capturing it
through a socketpair is oc-specific.

### Opting out

There is no runtime toggle and no environment variable. To drop the
primitive, the async drain and the `tracing` dependency, build `rsync_io`
without default features:

```sh
cargo build --release -p rsync_io --no-default-features
```

The sync spawn path keeps using the socketpair on Unix either way, and
falls back to a pipe on file-descriptor exhaustion with a one-time
warning.

### Platform notes

| Platform                | stderr backend                | Fallback                     |
|-------------------------|-------------------------------|------------------------------|
| Linux                   | `socketpair(AF_UNIX)`         | anonymous pipe               |
| macOS                   | `socketpair(AF_UNIX)`         | anonymous pipe               |
| FreeBSD and other Unix  | `socketpair(AF_UNIX)`         | anonymous pipe               |
| Windows                 | anonymous pipe                | -                            |

The user-visible behaviour is the same with either backend: the same
bytes are surfaced through the same error-reporting code path. Only the
deadlock resistance and the wake-up mechanism differ.

### References

- Design: [`docs/design/socketpair-stderr-channel.md`](design/socketpair-stderr-channel.md)
- Audits: [`docs/audits/ssh-stderr-handling.md`](audits/ssh-stderr-handling.md),
  [`docs/audits/ssh-socketpair-vs-pipes.md`](audits/ssh-socketpair-vs-pipes.md),
  [`docs/audits/ssh-socketpair-claim-verification.md`](audits/ssh-socketpair-claim-verification.md)
- Trackers: SSE-1..SSE-8 (issues #2370-#2377).
