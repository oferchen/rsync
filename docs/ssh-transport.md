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

## SSH stderr

The ssh child inherits oc-rsync's stderr, exactly as upstream
`piped_child()` arranges it (`pipe.c:48-97` redirects only stdin and
stdout). oc-rsync neither reads nor buffers the remote shell's
diagnostics: authentication failures, host-key prompts, `-v` output and
banners reach the terminal byte for byte, as soon as they are written,
and exactly once. Because nothing in oc-rsync reads that stream, a chatty
remote shell can never fill a pipe and stall the transfer.

On the server side, once protocol setup is done, oc-rsync sets its own
stderr back to blocking, as upstream `start_server()` does
(`main.c:1356-1364`), because ssh may have left it non-blocking.

The earlier opt-in `ssh-socketpair-stderr` capture channel is removed;
its design note is kept for history at
[`docs/design/socketpair-stderr-channel.md`](design/socketpair-stderr-channel.md).
