# perform_io convergence: retire the daemon DrainingReader (IO-1)

Status: design note gating IO-2 (#153) and IO-3 (#154). No code changes in this
stage. This is the design pass #6946 calls for.

## 1. Problem

oc carries a daemon-only anti-deadlock mechanism that upstream does not have. On
the daemon's single TCP socket the delta path writes a batch of requests, then
blocks reading the reply; once both ~128 KB kernel socket buffers fill, neither
direction progresses - a full-duplex write-write wedge. SSH/stdio never wedges
because each direction is an independent pipe with the peer in a separate
process.

The shipped fix (Approach C from `daemon-delta-deadlock-fix.md`) is a background
`daemon-delta-drain` thread that continuously reads the daemon's read-clone fd
into an unbounded `mpsc` queue; the transfer engine reads from that queue instead
of the socket (`crates/daemon/src/daemon/sections/module_access/transfer/draining_reader.rs:180-288`).
That original note already named Approach A - "a select/poll-driven bidirectional
loop mirroring perform_io" - as "the right long-term direction if the engine's
I/O core is ever unified" (`daemon-delta-deadlock-fix.md:184-219,327-329`). This
note specs that convergence: one upstream-shaped I/O core shared by every
transport, after which the drain thread and its scaffolding delete.

The trigger to do it now is that the Approach C scaffolding has accreted
platform debt: the drain thread cannot be stopped reliably on Windows, so it is
`#[cfg(not(unix))]`-disabled outright (`streams.rs:44-52`, #6297), leaving the
Windows daemon with no wedge protection at all. An upstream-shaped core removes
the whole class of problem instead of guarding it per platform.

## 2. Upstream model (rsync 3.4.4, protocol 32)

### 2.1 Non-blocking fds set once, at startup

Both transfer fds are made non-blocking before any data flows, on every role:

- Server child: `set_nonblocking(f_in); set_nonblocking(f_out);`
  (`// upstream: main.c:1259-1260` in `start_server`).
- Client: same, unless `read_batch` (`// upstream: main.c:1293-1294` in
  `client_run`).
- `set_nonblocking` just ORs `O_NONBLOCK` onto the fd's `F_GETFL`
  (`// upstream: util1.c:49-59`). stderr is forced back to blocking afterward so
  print statements are not lost (`// upstream: main.c:1300-1308`).

Non-blocking is the enabling precondition: it lets one thread wait on
readability and writability together and never park solely on a write.

### 2.2 The single perform_io loop

All socket I/O funnels through one function, `perform_io(size_t needed, int
flags)` (`// upstream: io.c:562`). Every buffered read (`read_buf`/`raw_read`)
and every flush (`io_flush`) drives it; there is no code path that writes without
going through it.

- It is a `select()` loop (`// upstream: io.c:764`). Each pass it builds the fd
  sets: the input fd is added to the read set whenever the input buffer has room
  (`// upstream: io.c:662-669`); the output fd is added to the write set only
  when there is pending output to flush (`// upstream: io.c:679-721`). So a wait
  that wants to write is simultaneously armed to read.
- After `select`, it reads whatever is readable into the input buffer
  (`// upstream: io.c:781-823`) and writes whatever the output fd will accept
  (`// upstream: io.c:830-877`), tolerating `EINTR`/`EWOULDBLOCK`/`EAGAIN` as a
  no-op (`// upstream: io.c:797-798,840-841`).
- The explicit deadlock-breaker: while it is here trying to write, it first
  drains every readable multiplex message it can:

  ```
  /* We need to help prevent deadlock by doing what reading
   * we can whenever we are here trying to write. */
  if (IN_MULTIPLEXED_AND_READY && !(flags & PIO_NEED_INPUT)) {
          while (!iobuf.raw_input_ends_before && iobuf.in.len > 512)
                  read_a_msg();
          ...
  }
  ```

  (`// upstream: io.c:882-889`). `read_a_msg` pulls a framed message out of the
  already-read input buffer (`// upstream: io.c:1495`), so the raw socket read
  ahead in 2.2 and the message decode are decoupled.

### 2.3 The input buffer is FIXED, not growable

Important correction to the premise in the DrainingReader comments and in the
IO-1 task text (both say the drain queue "mirrors upstream's iobuf.in that grows
as needed"): upstream's socket input buffer does not grow. It is a fixed circular
buffer allocated once - `alloc_xbuf(&iobuf.in, ROUND_UP_1024(IO_BUFFER_SIZE))`
(`// upstream: io.c:1401`), with `IO_BUFFER_SIZE == 32*1024`
(`// upstream: rsync.h:160`) - and perform_io states outright "We never resize
the circular input buffer" (`// upstream: io.c:579-585`), aborting with
`RERR_PROTOCOL` if a caller ever needs more than fits. The output buffer is a
fixed 64 KB (`2 * IO_BUFFER_SIZE`, `// upstream: io.c:1382`) and the message
buffer a fixed 32 KB (`// upstream: io.c:2455`; it alone can double on overflow,
`// upstream: io.c:998`).

The consequence for the convergence design is material: the deadlock is broken by
the select-driven interleave (2.2), NOT by unbounded buffering. A fixed input
buffer is safe precisely because the same thread that reads into it also drains
it. oc's unbounded `mpsc` queue is a divergence born of splitting the reader onto
a second thread; the converged core should use a fixed circular buffer sized like
upstream and inherit upstream's flow-control shape for free.

### 2.4 Why the deadlock is structurally impossible upstream

`select` never blocks solely on the write fd: whenever the input buffer has room
the input fd is in the read set too (`// upstream: io.c:662-669`), and before
committing to a write the loop empties the peer's readable messages
(`// upstream: io.c:884-889`). So this side always drains the peer's send buffer,
the peer therefore never blocks writing, so this side's own writes always find
room. With that invariant upstream needs no data-phase I/O timeout at all
(`io_timeout` defaults to 0); the timeout is a keepalive/liveness aid, never the
thing that breaks a wedge.

## 3. oc model today

The whole live transfer graph is a synchronous blocking pull. Nothing on the
transfer path is non-blocking; the only concurrency is the daemon's out-of-band
drain thread bolted around the same blocking socket.

### 3.1 The reader/writer graph

Layering from the wire outward:
`Transport -> CountingReader -> ServerReader{Plain | MultiplexReader |
Compressed<MultiplexReader>}`, mirrored by
`ServerWriter{Plain | MultiplexWriter | Compressed<MultiplexWriter>}`.

- `CountingReader` wraps the raw transport below the demux so its byte total
  reflects compressed wire bytes, matching `stats.total_read`
  (`crates/transfer/src/reader/counting.rs:11-26`); `Read` blocks then counts
  (`counting.rs:46-52`).
- `ServerReader<R>` is a mode enum (`crates/transfer/src/reader/server.rs:15-41`)
  switched at `io_start_multiplex_in`/`io_start_buffering_in` equivalents
  (`server.rs:115-183`). Its `Read` impl is pure blocking dispatch
  (`server.rs:333-341`).
- `MultiplexReader<R>` (`crates/transfer/src/reader/multiplex.rs:128-185`) is the
  MSG_* demux. `Read` loops `next_frame()` + `dispatch_message()` until a
  MSG_DATA frame arrives (`multiplex.rs:807-833`); the sole wire-read choke point
  is `next_frame` -> `FrameReader::read_frame`
  (`multiplex.rs:693-697`;
  `crates/protocol/src/multiplex/io/frame_reader.rs:64-119`). `read_frame` is a
  resumable synchronous decoder that retries only `EINTR` inline
  (`frame_reader.rs:78-114`) and otherwise assumes a blocking descriptor.
- `ServerWriter<W>` mirrors it: blocking `write`/`write_vectored`/`flush`
  (`crates/transfer/src/writer/server.rs:26-38,477-512`), plus the daemon
  teardown barrier `shutdown_send_side` (`writer/server.rs:515-536`).

The read and write halves are SEPARATE objects, handed to the engine as
`&mut dyn Read` and `&mut dyn Write`. Neither can drain the other, so oc cannot
reproduce upstream's "drain-while-writing" invariant inside the shared code.
oc's partial mimic is the `BufferedInputHint` gate: the generator send loop skips
its pre-read flush when the reader still has buffered payload
(`server.rs:320-331`;
`crates/transfer/src/generator/transfer/transfer_loop.rs:401-403`). That defers a
flush; it does not drain the peer.

### 3.2 Where blocking reads are assumed

Every one of these is a synchronous `read_ndx`/`read_exact`/`read_token` on a
`&mut dyn Read`, correct only for a blocking fd:

- Generator: NDX/ack read `transfer_loop.rs:413`; per-file iflags
  `transfer_loop.rs:556`; xattr request `transfer_loop.rs:574`.
- Receiver delta tokens: `TokenReader::read_token`
  (`crates/transfer/src/token_reader.rs:142-164`, `read_exact` at `:145-146`);
  `apply_delta_stream` loop
  (`crates/transfer/src/delta_apply/applicator.rs:856-866`); literal bytes
  `applicator.rs:645`; trailing checksum `applicator.rs:806`; discard-path
  realignment `applicator.rs:908-940`.
- Receiver control/attrs: sum-head `crates/transfer/src/receiver/wire.rs:236`;
  NDX/iflags/xname `wire.rs:333-377,421-478`; phase NDX_DONE
  `crates/transfer/src/receiver/transfer/phases.rs:145,210`; on-demand flist NDX
  `crates/transfer/src/receiver/file_list/on_demand.rs:72`,
  `file_list/receive.rs:237`.

### 3.3 The daemon drain thread and why only the socket wedges

- `setup_transfer_streams` splits the daemon socket into two blocking
  `try_clone()` fds (`streams.rs:192-208`). For a real transfer it wraps the read
  clone in `DrainingReader::new` (`streams.rs:246`), whose background thread arms
  `SO_RCVTIMEO` (never `O_NONBLOCK` - that would leak onto the shared write clone
  and truncate the transfer, `draining_reader.rs:34-43,208`) and pumps the socket
  into an unbounded `mpsc` queue (`draining_reader.rs:180-288`). The engine reads
  the queue through a blocking `Read` facade (`draining_reader.rs:304-334`).
- `should_arm_delta_drain` gates it: armed only when the client sent a non-empty
  argument list, and `#[cfg(not(unix))]` it is always off
  (`streams.rs:44-52`) - the Windows opt-out (#6297), because a blocking `recv()`
  on a cloned socket handle is not interrupted by `SO_RCVTIMEO` there, so
  `stop_and_join()` hangs the worker.
- stdio/SSH returns `io::stdin()`/`io::stdout()` with `drain_handle: None`
  (`streams.rs:166-186`): two independent pipe buffers, peer in another process,
  so the single-socket back-pressure coupling does not exist.

### 3.4 Goodbye-drain teardown coupled to the drain thread

After the engine returns, the orchestrator stops the drain thread BEFORE the TCP
goodbye drain reads the socket through a different clone:
`streams.drain_handle.take()` then `drain.stop()`
(`orchestration.rs:685-687`), then `drain_until_peer_eof` twice around an
explicit `shutdown_send_side` half-close
(`orchestration.rs:766-826`;
`crates/daemon/src/daemon/sections/module_access/transfer/graceful_close.rs:93-120`).
The ordering constraint - one reader on the socket at a time - exists only
because the drain thread is a second reader. Remove the thread and the constraint
evaporates.

## 4. Convergence design

Build one upstream-shaped I/O core that owns both directions, sets both fds
non-blocking, and drives them from a single readiness loop - and have it present
the SAME blocking `Read`/`Write` facade the engine already consumes, so the
engine (sections 3.1-3.2) does not change. This is exactly the shape `QuicStream`
already uses (a driver behind a blocking `Read`/`Write` facade,
`crates/rsync_io/src/quic/mod.rs:367,422-467`), generalized to the perform_io
interleave.

### 4.1 Boundaries (rsync_io's job, #153)

The core lives in `crates/rsync_io` - today a handshake/transport facade over
`protocol` with no transfer-time I/O primitive (`crates/rsync_io/src/lib.rs:6-11`)
and, per the unsafe policy, a `#![deny(unsafe_code)]` crate. Proposed surface:

- `trait DuplexTransport` - yields the two underlying descriptors (or one shared
  socket exposed as an in/out pair) and can set them non-blocking. Implemented by
  the daemon single socket (two clones of one fd, non-blocking on the shared open
  file description covers both), by SSH stdio (stdin/stdout fds), and by local.
- `struct PerformIo<T: DuplexTransport>` - owns `T` plus three fixed circular
  buffers sized exactly like upstream (in 32 KB, out 64 KB, msg 32 KB;
  section 2.3), and runs a `perform_io(needed, flags)`-equivalent loop. It exposes
  two handles over the shared state (single-threaded, e.g. an `Rc<RefCell<..>>`
  reader/writer split, or a re-shaped `ServerReader`/`ServerWriter` that both hold
  the shared core): a `Read` half that fills `needed` bytes from the input buffer
  (running the loop to top it up) and a `Write` half that appends to the output
  buffer and flushes through the loop. The read half drains the peer whenever the
  write half is flushing - upstream `io.c:884-889` - which is only possible
  because both halves co-own one core, the structural fix section 3.1 lacks.
- Readiness syscall: rsync_io must stay unsafe-free, so the `select`/`poll` call
  is exposed as a safe `poll_duplex(in, out, want_write, timeout) -> Readiness`
  from `fast_io` (the crate the unsafe policy designates as the consolidation
  target for platform FFI). Evaluate `mio` versus a thin `fast_io` wrapper before
  building; the `fast_io` wrapper is favored because it keeps one owner of the
  raw-fd unsafe and adds no async runtime (async-restraint: this is a
  single-threaded event interleave, not a concurrency framework).

### 4.2 First consumer (#153)

The daemon-TCP delta path - the only path that wedges - is the first and only
consumer in #153. Inside `setup_transfer_streams`, the armed branch that today
builds `DrainingReader::new(read_stream)` (`streams.rs:246`) instead constructs a
`PerformIo` over the two socket clones and returns its `Read`/`Write` halves.
SSH/stdio and local are untouched in #153 (they still return blocking handles and
still cannot wedge). This is a contained, reversible swap behind the exact seam
Approach C occupies, so it lands without disturbing any other transport.

### 4.3 How the Approach C machinery deletes (#154)

Once #154 migrates SSH/stdio and local onto the same `PerformIo` core (uniform
I/O model, no behavioral change since those paths never wedged), every consumer
of the drain scaffolding is gone and these delete outright:

- `draining_reader.rs` in full - `DrainingReader`, `DrainInner`, `DrainHandle`,
  the `daemon-delta-drain` thread, the `DrainSource`/`SO_RCVTIMEO` machinery,
  and its tests.
- `should_arm_delta_drain` and its `#[cfg(not(unix))]` opt-out
  (`streams.rs:44-52`) plus the gating tests (`streams.rs:398-435`).
- The `drain_handle` field on `TransferStreams` (`streams.rs:8-23`) and the
  `drain.stop()` call and its ordering rationale (`orchestration.rs:677-687`).

The goodbye-drain teardown (`orchestration.rs:766-826`, `graceful_close.rs`)
stays as a distinct concern - it reaps the peer's trailing goodbye bytes and
avoids an abortive RST - but it no longer needs the "stop the other reader first"
barrier, because there is no longer a second reader.

## 5. Blast radius and risks

- Blocking-read call sites (section 3.2): none need to change if `PerformIo`
  faithfully presents a blocking facade over non-blocking fds - that is the whole
  point of the facade. The risk is fidelity: `read_exact`/`read_ndx`/`read_token`
  must see identical byte sequences and identical short-read/`EINTR` behavior.
  Wire-order and flush-point parity is the acceptance bar.
- The `BufferedInputHint`/`has_buffered_input` flush gate (`server.rs:320-331`,
  `transfer_loop.rs:401-403`) becomes redundant once the core drains-while-writing
  natively; leave it in place under #153 (harmless) and reconsider removing it in
  #154 only with wire-capture proof it is a no-op under the core.
- Goodbye-drain teardown: the `drain.stop()` ordering deletes; verify the two
  `drain_until_peer_eof` passes and the `shutdown_send_side` half-close still see
  the same trailing-byte sequence when the read side is the `PerformIo` core
  rather than a raw clone.
- Windows: the perform_io model is strictly better for the socket path. Windows
  sockets support non-blocking (`FIONBIO`) and `WSAPoll`/`select`, so the daemon
  wedge that the drain thread could not guard on Windows (#6297) is closed by the
  core, and the `#[cfg(not(unix))]` opt-out disappears. Caveat: Windows
  `select`/`WSAPoll` work on sockets, not on anonymous pipes, so the SSH-stdio
  readiness path on Windows cannot use the same primitive. Since SSH stdio never
  wedges (independent pipes, separate process), the safe staging is: converge the
  daemon-TCP socket path on all platforms first (#153), and on Windows keep
  SSH-stdio blocking (or gate its `PerformIo` behind an IOCP/overlapped readiness
  backend) rather than force a pipe-select that Windows does not offer.
- Soak / proof the wedge is gone: reuse the `dbg503` repro from
  `daemon-delta-deadlock-fix.md:338-341` (128 x 4 MiB, seeded basis, backdated
  dst, mid-file mutation, re-sync over a single no-chroot daemon module) and
  confirm exit 0 with `ss` snapshots showing socket queues draining, not a stable
  congested state. Add a long-`timeout` module variant to prove no hang once the
  wedge cannot form (upstream needs no data-phase timeout, section 2.4; oc already
  clears the leaked accept-time timeout when a module sets none,
  `crates/daemon/src/daemon/sections/module_parsing/module_spec.rs:21-22`, so the
  data phase already matches upstream's `io_timeout = 0`). Run the four
  daemon-negotiation tests that timed out under the drain thread on Windows
  (#6297) to confirm the core removes that failure. Verify on the Linux host
  (the wedge does not reproduce on macOS) plus the full daemon interop matrix.

## 6. Staging so #153 and #154 land without a flag day

Each transport's read/write pair is selected independently in
`setup_transfer_streams` (daemon) and in the SSH/local setup, so swapping one
transport's pair never touches the others. That is what makes this incremental.

### IO-2 / #153 - build the primitive, first consumer only

- Add `DuplexTransport`, `PerformIo`, and the fixed circular buffers to
  `crates/rsync_io`; add the safe `poll_duplex` readiness wrapper to `fast_io`.
- Set both daemon socket clones non-blocking and route the daemon-TCP delta path
  through `PerformIo` in place of `DrainingReader::new` (`streams.rs:246`),
  leaving SSH/stdio and local on their current blocking handles.
- `DrainingReader` and `should_arm_delta_drain` remain in the tree, now unused on
  the armed daemon path; deletion is deferred to #154 so #153 is a minimal,
  reversible swap with a clean wire-capture diff against the pre-#153 daemon path.
- Acceptance: `dbg503` completes; daemon interop matrix green; SSH/local
  byte-identical; Windows daemon-negotiation tests green.

### IO-3 / #154 - migrate all transports and delete Approach C

- Route SSH/stdio and local through the same `PerformIo` core (Windows SSH-stdio
  per the section 5 caveat), unifying the I/O model.
- Delete `draining_reader.rs`, `should_arm_delta_drain` and its `cfg` opt-out,
  the `drain_handle` field, and the `drain.stop()` ordering barrier
  (section 4.3).
- Simplify the goodbye-drain teardown to drop the now-vacuous single-reader
  ordering constraint.
- Acceptance: full nextest + upstream-testsuite + interop matrix green across all
  three transports and all platforms, with wire-capture parity vs upstream 3.4.4
  on local, daemon, and SSH.

## 7. Invariants each stage preserves

- Wire fidelity first: identical framing, byte order, flush points, multiplex
  MSG_* sequence, and goodbye handshake on every transport. The core changes WHEN
  the socket is read/written, never WHAT.
- The engine's synchronous `Read`/`Write` consumers (sections 3.1-3.2) are
  unchanged; the core presents the same blocking facade.
- Buffer sizes match upstream exactly (in 32 KB, out 64 KB, msg 32 KB); no
  unbounded growth (correcting the Approach C queue, section 2.3).
- No new negotiation, no new capability advertisement, no wire-protocol feature.
- The daemon data-phase I/O-timeout semantics already match upstream
  (`module_spec.rs:21-22`) and are not changed by this work.
