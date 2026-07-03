# Daemon-transport delta transfer deadlock: analysis and fix design

Status: proposal, awaiting approval. No code changes accompany this document.

Scope: this is an analysis and a reviewable design for fixing the confirmed P1
bug where oc-rsync daemon-transport delta transfers deadlock. It proposes fix
approaches, evaluates their blast radius, and recommends one. Implementation is
deferred until the recommendation is approved.

## 1. Summary

When the oc-rsync daemon receives a delta transfer over a plain TCP module
(client push, real delta tokens flowing in both directions), the connection
wedges. Both TCP sockets carry data in both the receive queue and the send
queue at the same time and stay byte-identical across successive `ss` snapshots:
a permanent full-duplex write-write deadlock. The daemon's delta-receive loop
writes file requests, signatures, and acks to the peer without concurrently
draining the incoming multiplex frames. Once the ~128 KB kernel socket buffers
fill in both directions, each side blocks writing into a full send buffer while
neither side drains the peer's send buffer.

The failure is observable as `code 23`, `failed to fill whole buffer`, and
`multiplexed payload truncated`. That surface is a secondary symptom: a leaked
10-second accept-time socket read timeout fires on the wedged read and turns the
deadlock into an abort. With a long module `timeout`, the same wedge instead
hangs indefinitely.

Reference behaviour: upstream rsync 3.4.x on the identical config and workload
completes the transfer. oc-rsync over SSH, oc-rsync daemon `--whole-file`, and
oc-rsync daemon quick-check (no delta) all succeed. The fault is specific to the
combination of the daemon transport and bidirectional delta data flow.

## 2. oc-rsync's current daemon I/O model

### 2.1 One socket, two blocking clones

The daemon accepts a TCP connection and, at transfer setup, splits the single
`TcpStream` into two independent file-descriptor clones, one for reading and one
for writing:

- `crates/daemon/src/daemon/sections/module_access/transfer/streams.rs:63` and
  `:72` call `tcp.try_clone()` twice to produce `read_stream` and `write_stream`.
- Both clones are boxed and returned as `streams.read` and `streams.write`
  (`streams.rs:89`-`:95`), then handed to `execute_transfer`
  (`crates/daemon/src/daemon/sections/module_access/transfer/orchestration.rs:390`).

Both handles are dup'd file descriptors that refer to the same kernel socket,
and both are in blocking mode. The transfer engine therefore reads and writes
the same bidirectional socket through two blocking handles, driven from a single
thread. There is no `select`/`poll` loop that services readable data while a
write would block.

The stdio (SSH / remote-shell) path is structurally different. When the stream
is stdio, setup returns `io::stdin()` and `io::stdout()`
(`streams.rs:47`-`:56`) as the read and write handles. These are two independent
OS pipes with separate kernel buffers, and the peer runs in a separate process.
When the oc-rsync receiver blocks reading from stdin, the peer process is still
scheduled and can drain what the receiver has written to stdout, because that is
a different kernel buffer on a different fd. The single-socket back-pressure
coupling that wedges the daemon path does not exist over two independent pipes.
This is why the same engine code deadlocks on daemon TCP but not over SSH.

### 2.2 Where the daemon writes without draining

The receiver-side delta loop lives in
`crates/transfer/src/receiver/transfer/pipeline.rs`. The relevant loop is at
`pipeline.rs:170`:

1. It collects a batch of up to `pipeline.available_slots()` files
   (`pipeline.rs:182`-`:185`) and sends a file request per file via
   `send_file_request` (`pipeline.rs:248` and `:270`). Each request is written
   into the multiplex writer.
2. It flushes the writer once, only when the sender has no queued requests left
   (`pipeline.rs:297`-`:300`).
3. It then blocks reading the sender's response for one queued request via
   `process_file_response_streaming` (`pipeline.rs:318`).

Step 3 is a blocking read on the socket. While it blocks, the loop performs no
writes and no draining beyond the single in-flight response it is waiting for.
Symmetrically, the peer sender is streaming delta tokens (potentially many
megabytes for 128 modified 4 MiB files) into the same socket in the other
direction. Once the receiver's batched requests fill the socket send buffer and
the sender's delta tokens fill the socket receive buffer, both sides block:
the receiver is blocked in `process_file_response_streaming` waiting for a
response that is stuck behind unread delta bytes, and the sender is blocked
writing delta bytes into a send buffer the receiver is not draining. Neither
side makes progress.

The multiplex writer amplifies the coupling. `MplexWriter`
(`crates/protocol/src/multiplex/writer.rs:70`) buffers up to a 32 KB default
(`writer.rs:82`, `DEFAULT_BUFFER_SIZE`) and flushes only when the caller calls
`flush` or the buffer overflows. It is the same writer for daemon TCP, SSH
stdio, and local transports, with no transport-specific interleaving. The loop's
single caller-driven flush per drained batch (`pipeline.rs:298`) is the only
point where buffered writes reach the socket, and it is not interleaved with a
non-blocking drain of the read side.

The engine's own comment acknowledges the gap: `pipeline.rs:166`-`:167` notes
that "upstream io.c perform_io() uses select() for bidirectional I/O," and
`pipeline.rs:466`-`:467` notes "we flush once before blocking on each response
read." Flushing before a blocking read is not equivalent to upstream's
drain-while-write invariant, and that difference is the deadlock.

### 2.3 The leaked accept-time socket timeout (the mask)

At accept, `configure_stream` arms a 10-second read and write timeout on the
socket:

- `crates/daemon/src/daemon/sections/server_runtime/listener.rs:242`-`:243` set
  `SOCKET_TIMEOUT` on both directions.
- `SOCKET_TIMEOUT` is `Duration::from_secs(10)`
  (`crates/daemon/src/daemon.rs:121`), documented as a guard against hanging
  handshakes.

`apply_module_timeout` is the only place that touches the socket timeout again
before the data phase (`crates/daemon/src/daemon/sections/module_parsing/module_spec.rs:9`,
called from `orchestration.rs:56`). It overrides the timeout only when the
module sets `timeout` (`module_spec.rs:10`-`:14`). It has no `else` branch, so
when a module sets no `timeout`, the 10-second accept-time timeout persists
unchanged through the entire delta data phase. No code path clears it to `None`
before the transfer starts.

Consequences:

- Module with no `timeout`: the wedged blocking read hits the leaked 10-second
  `SO_RCVTIMEO`, returns `EAGAIN`/`ETIMEDOUT`, and the multiplex demux
  (`read_payload_into`, `crates/protocol/src/multiplex/helpers.rs:119`) surfaces
  it as `multiplexed payload truncated`, which maps to `code 23`. The abort
  masks the underlying deadlock as a truncation error.
- Module with `timeout = 3600`: the abort is removed, the deadlock is exposed,
  and the transfer hangs until the long timeout, i.e. effectively forever from
  the operator's point of view.

The timeout is therefore not the bug; it is a mask that converts the deadlock's
symptom from "hang" to "code 23".

## 3. Upstream rsync's I/O model (reference / source of truth)

Upstream never lets one direction wedge because its I/O core services readable
data whenever it needs to write. The relevant code is in
`target/interop/upstream-src/rsync-3.4.4/io.c`.

- `perform_io` builds an `fd_set` and calls `select` on the input fd and, when
  it has pending output, the output fd (`io.c:660`-`:668` set the input fd in
  the read set whenever there is room in the input buffer). The loop drives both
  directions from the same select.
- The key deadlock-prevention step is explicit. While `perform_io` is trying to
  write, it first drains readable multiplex messages:

  ```
  /* We need to help prevent deadlock by doing what reading
   * we can whenever we are here trying to write. */
  if (IN_MULTIPLEXED_AND_READY && !(flags & PIO_NEED_INPUT)) {
          while (!iobuf.raw_input_ends_before && iobuf.in.len > 512)
                  read_a_msg();
          ...
  }
  ```

  This is `io.c:882`-`:889`. Before blocking on a writable fd, upstream services
  every readable multiplex message it can, keeping the peer's send buffer
  drained so the peer can keep accepting the writes this side needs to make.
- `read_a_msg` / `readfd` / `read_buf` pull multiplexed messages off the wire
  into `iobuf.in`, and `io_flush` drains `iobuf.out` to the fd. Both funnel
  through `perform_io`, so every flush is interleaved with the read-drain above;
  there is no code path that writes without first draining what it can read.
- `io_timeout` defaults to 0 (`io.c:179`-`:180`: `if (!io_timeout) return;`
  short-circuits the timeout check). With no `--timeout` and no module
  `timeout`, upstream applies no fatal data-phase I/O timeout at all. The module
  `timeout` is wired into `io_timeout` in `clientserver.c` around line 1206; when
  it is unset, `io_timeout` stays 0.

The structural invariant is: always service readable data while writing. Because
`perform_io` drains the peer before it blocks on a write, the peer's send buffer
never stays full, so the peer never blocks writing, so this side's read never
starves. The deadlock oc-rsync hits is structurally impossible under that
invariant, which is why upstream can safely default to no I/O timeout.

## 4. Fix approaches

Three approaches were evaluated. All three must be paired with the secondary
timeout change in section 4.4, never shipped alone.

### 4.1 Approach A: select/poll-driven bidirectional loop mirroring perform_io

Mechanism: introduce a daemon-side (or transport-side) I/O driver that owns both
the read fd and the write fd and drives them from a single `poll`/`select`. When
the delta loop wants to write, the driver drains readable multiplex frames into
an input buffer before or while blocking on the writable fd, mirroring
`perform_io`'s "read what we can whenever we are trying to write" step. The
engine reads from the input buffer and writes into the output buffer; the driver
moves bytes to and from the socket without ever blocking on a write while the
read side is unserviced.

Files that change: a new I/O driver (likely in `crates/transfer` or
`crates/protocol/src/multiplex`), the receiver delta loop
(`crates/transfer/src/receiver/transfer/pipeline.rs`) to read/write through the
driver instead of blocking directly on the socket clones, and the daemon setup
(`streams.rs`) to construct the driver for the TCP transport.

Risk: high. It reworks the core read/write model that the engine shares across
transports. Wire ordering, flush points, and the goodbye handshake all depend on
the current blocking semantics.

Blast radius: potentially large. If the driver is inserted at the shared
multiplex layer, it touches SSH/stdio and local paths as well as daemon TCP,
plus the goodbye/finalization drain in `orchestration.rs:462`-`:531`. It can be
contained to the daemon-TCP transport if the driver is only constructed for the
socket path, but the receiver loop change is shared code and must stay
byte-identical for the non-daemon paths.

Test strategy: the `dbg503` repro (128 x 4 MiB files, seeded basis, mid-file
mutation, forced real delta over a daemon module) must complete; upstream parity
diff on the resulting tree; SSH, local, and daemon `--whole-file`/quick-check
regression; full daemon interop matrix.

Bake needs: substantial. This is the closest match to upstream semantics but the
largest structural change, so it needs a full bake window across the interop
matrix before it can be trusted.

### 4.2 Approach B: non-blocking socket with a back-pressure-aware writer

Mechanism: put the daemon socket into non-blocking mode and make the writer,
when a write returns `EWOULDBLOCK`, service the read side (drain available
multiplex frames into the input buffer) before retrying the write. This inlines
the drain-while-write behaviour into the write path rather than a separate
driver: a bounded write that interleaves reads under back-pressure.

Files that change: the writer path (`crates/protocol/src/multiplex/writer.rs`)
or a daemon-specific wrapper around it, the daemon socket setup (`streams.rs`)
to select non-blocking mode for the TCP path, and the receiver loop where it
blocks on responses (`pipeline.rs`) to cooperate with the interleaved reads.

Risk: high. Flipping the socket to non-blocking changes the semantics of every
read and write on that fd, including the goodbye drain and the multiplex demux.
Every `read`/`write` on the daemon socket must now handle `EWOULDBLOCK`.

Blast radius: if non-blocking mode is confined to the daemon TCP fd, SSH/stdio
and local stay untouched; but the writer is shared, so a back-pressure hook in
`MplexWriter` risks affecting all transports unless carefully gated. The goodbye
drain (`orchestration.rs:488`-`:504`) reads the socket directly and would need
to tolerate non-blocking semantics.

Test strategy: same as Approach A.

Bake needs: high, for the same reason as A. Non-blocking conversion is subtle and
error-prone across platforms (Windows socket semantics differ), so it needs a
full cross-platform bake.

### 4.3 Approach C: dedicated drain thread for incoming frames during the write-heavy phase

Mechanism: during the delta-receive phase, spawn a dedicated thread that
continuously reads the read-clone fd and buffers incoming multiplex frames into
a channel/queue. The main loop writes freely; the drain thread guarantees the
peer's send buffer is always being emptied, so the peer never blocks writing and
this side's writes always drain. The main loop consumes responses from the queue
instead of reading the socket directly.

Files that change: the daemon transfer path to spawn/join the drain thread
around the delta phase (`orchestration.rs` / `streams.rs`), and the receiver loop
(`pipeline.rs`) to pull responses from the drain queue rather than reading the
socket. The read and write clones already exist as separate fds
(`streams.rs:63`, `:72`), so the two-thread split is natural on the daemon path.

Risk: medium. It does not change the blocking model or the wire format; it adds
concurrency. The main correctness concern is ordering and shutdown: the drain
thread and the main loop must agree on frame boundaries and on when the phase
ends, and the thread must be joined cleanly before the goodbye drain.

Blast radius: smallest of the three for the shared code. The drain thread is
daemon-TCP-specific (it exploits the two separate socket clones), so SSH/stdio
and local paths are untouched. The multiplex writer is unchanged. The main
interaction with goodbye/finalization is that the drain thread must be stopped
and joined before the existing `orchestration.rs:462`-`:531` drain sequence runs,
which is a contained, well-defined seam. The receiver loop change (read from a
queue instead of the socket) is shared code and must be gated so non-daemon
paths keep reading the socket directly.

Test strategy: same repro and parity checks as A/B, plus explicit tests for
drain-thread shutdown ordering (thread joined before goodbye; no lost or
reordered frames; no frames read past the phase boundary).

Bake needs: moderate. Smaller structural change than A/B, but the concurrency
and shutdown ordering need a bake across the interop matrix and on Windows.

### 4.4 Secondary change: clear the leaked socket timeout (must ship with the primary)

Independently of which primary approach is chosen, `apply_module_timeout` must
clear the socket read/write timeout to `None` when the module sets no `timeout`,
so the data phase matches upstream's `io_timeout = 0` default instead of
inheriting the 10-second accept-time guard. Concretely, `module_spec.rs:9` gains
an `else` branch that calls `set_read_timeout(None)` / `set_write_timeout(None)`
when `module.timeout` is `None`; the accept-time `SOCKET_TIMEOUT`
(`listener.rs:242`) stays as the handshake guard but is cleared before the data
phase.

This change must never ship alone. Without the primary fix, clearing the timeout
removes the only thing that currently breaks the wedge (the 10-second abort), so
the deadlock stops aborting and instead hangs forever. The secondary change is
only safe once the primary fix guarantees the delta phase cannot deadlock; then
removing the timeout brings the no-`timeout` behaviour into line with upstream
(no fatal data-phase I/O timeout) without reintroducing a hang. Sequencing:
land the primary fix, verify the repro completes, then land (or co-land in the
same PR) the timeout clear and re-verify.

## 5. Recommendation

Recommended: Approach C (dedicated drain thread for the daemon delta phase),
co-landed with the section 4.4 timeout clear.

Justification:

- Smallest blast radius. It is daemon-TCP-specific: it uses the two socket
  clones the daemon already creates (`streams.rs:63`, `:72`) and leaves the
  SSH/stdio and local paths, the shared `MplexWriter`, and the wire format
  untouched. Approaches A and B either rework the shared read/write core or flip
  socket semantics, both of which reach every transport and the goodbye drain.
- Preserves upstream wire semantics. It changes only who reads the socket, not
  the bytes on the wire or their order, so upstream parity is straightforward to
  verify. It achieves the same practical guarantee as upstream's perform_io
  invariant ("the peer's send buffer is always being drained") without
  reimplementing the select loop.
- Testable and boundable. The single new concurrency seam is the drain thread's
  lifecycle (spawn at phase start, join before goodbye), which is a contained,
  assertable contract rather than a diffuse change to blocking semantics.

Approach A is the most faithful to upstream and would be the right long-term
direction if the engine's I/O core is ever unified on a perform_io-style driver,
but its blast radius and bake cost are not justified for a targeted P1 fix.
Approach B carries the cross-platform risk of non-blocking-socket conversion for
little gain over C. C delivers the same anti-deadlock guarantee with the least
disruption.

### 5.1 Success criteria and verification

The fix is complete when all of the following hold:

1. The `dbg503` repro (128 x 4 MiB files, seeded basis into dst, backdated dst,
   mid-file mutation via `dd conv=notrunc`, re-sync over a single no-chroot
   daemon module) completes with exit 0 and all files synced, with no `code 23`,
   no `failed to fill whole buffer`, and no hang.
2. Upstream parity: the resulting destination tree is byte-identical to what
   upstream rsync 3.4.x produces on the same config and workload, and `ss`
   snapshots show the socket queues draining (not a stable congested state).
3. SSH delta, local copy, daemon `--whole-file`, and daemon quick-check remain
   unaffected (byte-identical wire behaviour; no regression).
4. No regression in the daemon interop matrix (push and pull against all
   supported upstream versions).
5. Cross-platform: the drain-thread lifecycle is correct on Linux, macOS, and
   Windows, and is joined before the goodbye drain in all exit paths (success,
   error, early return).

Verification runs on the Linux validation host (nextest plus an oc-vs-upstream
diff), since the deadlock does not reproduce on macOS reliably. The repro recipe
is already captured and reproducible.

### 5.2 What could go wrong

- Drain-thread shutdown races. If the drain thread is not joined before the
  goodbye drain (`orchestration.rs:462`), the final NDX_DONE / MSG_STATS exchange
  could be read by the wrong reader or lost. Mitigation: a hard join barrier at
  the phase boundary, with a test that asserts no frame is read past the phase
  end and the thread is joined on every exit path.
- Buffering the wrong frames. The drain thread must hand exactly the response
  frames to the main loop in wire order; a queue that reorders or drops frames
  corrupts the delta transfer. Mitigation: order-preserving queue plus a
  regression test that fails on reorder or loss.
- Backpressure on the drain side. If the main loop consumes responses slower
  than the drain thread reads them, the in-memory queue can grow. Mitigation: a
  bounded queue so the drain thread naturally slows when the consumer lags, which
  still keeps the socket receive buffer drained enough to break the wedge.
- Timeout-clear sequencing. Landing the section 4.4 change before or without the
  primary fix converts the abort into a hang. Mitigation: co-land in the same PR
  and gate the timeout clear behind the verified repro pass.
