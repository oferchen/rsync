# perform_io's poll loop on IoBuf: what exists, what is missing, and the order to finish it

Status: design. Measured on master `a2478e57c`. Upstream citations are to
rsync 3.5.2 (`io.c`, `main.c`, `rsync.h`). No product code changes with this
note.

## 1. Summary

The loop is no longer missing. Since the transport core landed, every server
transfer reads and writes through one `Core`
(`crates/transfer/src/iobuf/core.rs:32`) whose `wait()` polls both directions
(`core.rs:284-300`). What is missing is narrower and more serious than the
task titles say:

1. Only one transport can actually be polled. Every other path runs the core
   over `BlockingStreams`, whose wait never offers input while output is
   wanted (`crates/fast_io/src/transport.rs:114-124`).
2. The core buffers peer input during a write wait but never services it:
   no message dispatch (io.c:1004-1009), no keepalive (io.c:876), no drain
   on a write error (io.c:961-967).
3. The receiving side is one wire thread doing the work of upstream's two
   processes. A bounded read-ahead cannot stand in for upstream's receiver
   process, which never stops reading the socket. This is a live deadlock
   (section 7, case D1), reproduced with the v0.6.4 binary and predicted for
   master by the code paths cited there.

## 2. What perform_io does

One object owns three circular buffers and two descriptors (io.c:101-110):
`in` is 32 KiB (io.c:1594, rsync.h:161), `out` is 64 KiB (io.c:1574), `msg`
is 32 KiB (io.c:2695). Both descriptors are non-blocking from session start
(main.c:1304-1305 server, main.c:1353-1354 client).

### 2.1 The modes

The three `NEED` flags are mutually exclusive (io.c:207-209).

| mode | caller wants | returns when | callers |
|---|---|---|---|
| `PIO_NEED_INPUT` | `needed` bytes buffered in `in` | `in.len >= needed` (io.c:727) | returns a pointer, bytes stay buffered (io.c:1027) |
| `PIO_INPUT_AND_CONSUME` | the same, and consume them | the same, then advances `in.pos` (io.c:1029-1039) | `raw_read_buf` (io.c:1047), `raw_read_int` (io.c:1060) |
| `PIO_NEED_OUTROOM` | `needed` free bytes in `out` | `out.len + needed <= out.size` (io.c:733) | `write_buf` (io.c:2494-2495), `io_flush` (io.c:2368-2372) |
| `PIO_NEED_MSGROOM` | `needed` free bytes in `msg` | `msg.len + needed <= msg.size` (io.c:737) | `send_msg` on the sender only (io.c:1121-1122), `io_flush` (io.c:2375-2376), keepalive flush (io.c:1669-1670) |
| `PIO_CONSUME_INPUT` | never alone; modifier of `NEED_INPUT` (io.c:211) | - | - |

A request larger than its buffer is a protocol error, never a resize
(io.c:664-669, io.c:679-684, io.c:697-702). A call whose condition already
holds returns without touching a descriptor (io.c:725-740).

### 2.2 One pass of the loop

| step | behaviour | anchor |
|---|---|---|
| poll set, input | the input fd is polled whenever `in` has room, in every mode | io.c:746-755 |
| poll set, output | `out` is chosen when a `MSG_DATA` frame is part-written, or when `msg` is empty, `out` holds data and the mode is not `NEED_MSGROOM`; otherwise `msg` when it holds data; otherwise nothing | io.c:767-804 |
| frame header | the `MSG_DATA` header is back-filled when a flush of `out` starts, and room for the next header is reserved | io.c:770-796 |
| one socket | a descriptor used in both directions gets one poll row with both events | io.c:809-811 |
| no descriptor | input mode: EOF handling; output modes: drain messages, then fail | io.c:824-847 |
| lookahead | while the sender's lookahead is short and input is multiplexed, the poll timeout is 0; a poll that then finds nothing sends one more sub-list | io.c:854-862, io.c:871-874 |
| timeout | a poll that finds nothing calls `check_timeout`; keepalives are allowed only in `NEED_INPUT` mode | io.c:866-876 |
| read | reads as much as fits into `in`; EOF marks `in_fd = -2`; `EINTR`/`EAGAIN` are zero bytes; any other error is fatal without a drain | io.c:898-927 |
| after a read | stamps `last_io_in`; in `NEED_INPUT` mode may queue a keepalive | io.c:933-937 |
| write | writes one chunk, clamped to `bwlimit_writemax` and to the buffer end; stamps `last_io_out`; paces `--bwlimit` | io.c:948-995 |
| write error | marks `out_fd = -2`, discards all output, reports, drains buffered messages, exits `RERR_SOCKETIO` | io.c:957-968 |
| messages while writing | when the mode is not `NEED_INPUT` and the reader is not inside a message: dispatch every buffered message while more than 512 bytes are buffered and no raw data is pending; the generator may also receive a sub-list | io.c:1002-1009 |
| files-from | forwarded last in the pass | io.c:1011-1018 |
| signals | checked before the poll, after the write and at exit | io.c:849-852, io.c:997-1000, io.c:1022-1025 |

Input is serviced before output in the same pass (io.c:898 precedes
io.c:948). The write priority is: finish a part-written data frame, then all
messages, then data (io.c:628-637).

### 2.3 Timeouts and keepalives

`set_io_timeout` sets `allowed_lull = ceil(timeout / 2)` and the poll bound
to that, capped at 60 s (io.c:1291-1321, io.c:142-149). `check_timeout`
first lets `maybe_send_keepalive` queue an empty `MSG_DATA`, then fails with
`io timeout after N seconds` unless the process is the forked receiver
(io.c:229-268). The keepalive is queued only when both output buffers are
empty and `allowed_lull` has passed since the last write (io.c:1659-1666).
A peer's `MSG_NOOP` makes a sender answer with a keepalive (io.c:1777-1784).

### 2.4 Messages

`read_a_msg` sets `in_multiplexed = -1` on entry and back to 1 on every exit
(io.c:1698, io.c:1944), so the service step at io.c:1004 never re-enters it.
`drain_multiplex_messages` skips buffered raw data and dispatches the
messages behind it, using only bytes already buffered (io.c:1947-1963). It
runs at exactly two sites: the write error (io.c:966) and the no-descriptor
branch (io.c:837).

`send_msg` on a full `msg` buffer waits for room on the sender and doubles
the buffer everywhere else (io.c:1120-1131).

`read_buf` copies everything it reads to the output while
`forward_flist_data` is set (io.c:2174-2175, io.c:2194-2195), between
`start_flist_forward` and `stop_flist_forward` (io.c:1676-1685).

## 3. The process model the loop assumes

The receiving side forks (main.c:1078). After the fork each process has its
own `iobuf`:

| process | input | output |
|---|---|---|
| sender | socket | socket |
| receiver (child) | socket (main.c:1099) | pipe to the generator, multiplexed (main.c:1090-1100) |
| generator (parent) | pipe from the receiver, multiplexed (main.c:1146-1157) | socket (main.c:1156) |

The consequence that matters: the socket's input is read by a process that
never writes to the socket. The generator may block on a full socket for as
long as it likes; the receiver keeps consuming the sender's data, so the
sender keeps reading the generator's requests. The 32 KiB bound on `in` is
safe only because of this.

## 4. Mapping to oc

### 4.1 What exists

| upstream | oc | where |
|---|---|---|
| one owner of both descriptors | `Core`, shared by `CoreReader` and `CoreWriter` | `iobuf/core.rs:32-62`, `:327-360` |
| fixed 32 KiB `in` | `InBuf::new(IN_BUFFER_SIZE)` | `core.rs:75`, `crates/protocol/src/iobuf/input.rs:11` |
| `out` and `msg` | `MuxOut { buffer, msgs }`, shared with the core | `writer/multiplex.rs:69-93`, `core.rs:254-261` |
| write priority (io.c:628-637) | `MuxOut::flush_buffer` | `writer/multiplex.rs:125-147` |
| poll both directions | `Core::wait` | `core.rs:284-300` |
| output written before an input wait (io.c:767) | `Core::read` calls `drain_mux` before waiting | `core.rs:157` |
| input already buffered returns at once (io.c:727) | `Core::read` serves `InBuf` first; `IoBuf::input_buffered` | `core.rs:147-153`, `iobuf/mod.rs:74` |
| `check_timeout` text and cadence | `IoTimeout` | `iobuf/timeout.rs:58-93` |
| `stats.total_read/written`, `--bwlimit` in the loop | `count_read`, `write_out` | `core.rs:232-245`, `:305-310` |
| pollable descriptors | `FdTransport` (poll via rustix), `SocketTransport` (WSAPoll) | `fast_io/src/transport.rs:175-288`, `:310-374` |
| lookahead grown before a blocking read | `grow_lookahead_while_idle` | `generator/transfer/transfer_loop.rs:381`, `:652-659` |
| exit exchange after a fatal error | `drain_until_peer_exits` | `error_exit.rs:212-253`, `lib.rs:1500-1514` |

One deliberate deviation is kept: oc reads first and polls only on
would-block (`core.rs:190-193`), where upstream polls before every read.

### 4.2 What is missing, and the change for each

| # | upstream behaviour | oc today | change |
|---|---|---|---|
| M1 | every transport is pollable (main.c:1304-1305) | only the Unix `--server` stdio path builds a pollable transport (`lib.rs:696`); all other paths end in `BlockingStreams` (`lib.rs:883`); `FdTransport::socket` and `SocketTransport` have no production caller | section 5; each call site in section 4.3 builds its own `Box<dyn Transport>` and calls `run_server_over_transport` (`lib.rs:902`) |
| M2 | a read wait also writes pending output without blocking on it (io.c:767-822, io.c:948) | `Core::read` flushes the whole mux buffer with blocking writes before it waits (`core.rs:157`, `:217-226`) | `Core::pump_output(&mut self) -> io::Result<bool>`: one non-blocking `MuxOut` write step; `Core::read` waits on `(input, output_pending)` and pumps when writable. `MuxOut` gains `write_some(&mut self, sink) -> io::Result<bool>` built on the existing `OutBuf::is_flushing`/`finish_flush` (`protocol/src/iobuf/out.rs:299-307`) |
| M3 | keepalive from the wait (io.c:876, io.c:249-252) and after a read (io.c:935-936) | an empty wait only checks the timeout (`core.rs:293-294`); keepalives come from explicit call sites such as `transfer_loop.rs:674` | `Core::wait` takes `allow_keepalive: bool` (true for read waits) and calls `MuxOut::queue_keepalive_after_lull` before `IoTimeout::check`; `last_io_out` moves from `MuxOut` (`writer/multiplex.rs:88`) into the core next to `IoTimeout` |
| M4 | messages are dispatched while waiting to write (io.c:1004-1009) | input is buffered, never parsed, during a write wait; the demultiplexer sits above the core (`lib.rs:1167-1168`) | move the frame cursor into the core: `enum InMux { Raw, Ready, InMessage }` (the tri-state of io.c:203-204, io.c:1698) and `raw_input_ends_before: usize`; `Core::service_messages(&mut self, h: &mut dyn MessageHandler)` runs after each write step when `InMux::Ready`, buffered > 512 and no raw data is pending. `MultiplexReader` (`reader/multiplex.rs:302`) implements `MessageHandler` with its existing `dispatch_message_with` (`:963`) |
| M5 | buffered messages are drained when a write fails (io.c:961-967) or no output descriptor remains (io.c:834-841) | a write error propagates (`core.rs:219`); the peer's frames are read only if `waits_for_peer` holds (`lib.rs:1500`) | `Core::drain_messages(&mut self, h)`: the loop of io.c:1947-1963 over `InBuf` only, never waiting; called from `Core::write` on a non-would-block error before the error is returned, with output discarded as at io.c:964 |
| M6 | a full `msg` buffer doubles on every role but the sender (io.c:1123-1131) | `queue_message` writes the queue out for every role (`writer/multiplex.rs:163-165`), which can block | `MuxOut` gets `msg_policy: MsgRoom { WaitForRoom, Grow }`, set from the role at `lib.rs:1230`; `Grow` appends without writing |
| M7 | lookahead grows on every zero-timeout poll that finds no input (io.c:854-874) | one sub-list per read that may block (`transfer_loop.rs:646-659`) | `Core::input_idle(&mut self) -> io::Result<bool>`: `transport.wait(true, false, Some(ZERO))`; the sender loops `grow_lookahead_while_idle` while it returns true and the lookahead is below `MAX_FILECNT_LOOKAHEAD` (rsync.h:153) |
| M8 | the socket's input is consumed by a process that never blocks on the socket's output (section 3) | one wire thread writes up to 64 requests before reading a reply (`pipeline/mod.rs:73`, `receiver/transfer/pipeline.rs:566-806`) | section 6.2: the receiver never blocks on output while replies are outstanding |
| M9 | `MSG_NOOP` answered from inside the read (io.c:1781-1782) | the reader holds no writer (`reader/multiplex.rs:993-999`); the sender loop answers at `transfer_loop.rs:836` | falls out of M4: the handler runs inside the core, which owns the output |

Not ported, on purpose: the circular `out`/`msg` arithmetic
(io.c:565-598). `OutBuf` already reserves the frame header in place.

### 4.3 Sites that still thread a reader and a writer separately

By transport - how the core's transport is built:

| transport | site | transport type | read during write |
|---|---|---|---|
| remote shell, `--server` on Unix | `cli/src/frontend/server/run.rs:1072` -> `lib.rs:696` | `FdTransport::pair` | yes, 32 KiB |
| remote shell, `--server` elsewhere; embedding | `run.rs:1082`, `embedding/src/lib.rs:304` | `BlockingStreams` | no |
| remote shell, client | `core/src/client/remote/ssh_transfer/drive.rs:378` | `BlockingStreams` | no |
| embedded SSH, client | `embedded_ssh_transfer.rs:434`; `async_ssh_transport.rs:606` over `std::sync::mpsc` channels (`:365-366`) | `BlockingStreams` | no |
| daemon TCP and QUIC, client | `daemon_transfer/orchestration/transfer.rs:135` (pull), `:245` (push) | `BlockingStreams` | no |
| daemon TCP, server | `daemon/.../transfer/streams.rs:351`, over two `try_clone` handles (`:233`, `:242`) | `BlockingStreams` | Unix: `DrainingReader` thread with an unbounded queue (`:297`); elsewhere none (`:44-52`) |
| daemon QUIC relay, server | `streams.rs:201-225` | `BlockingStreams` | `DrainingReader` (`:220`) |
| local or server child | not on master; local copies do not use the wire | - | - |

By role - who receives the halves:

| role | site | state |
|---|---|---|
| generator (oc sender) | `lib.rs:1437-1438`, `generator/transfer/orchestrator.rs:99`, `:135`, `transfer_loop.rs:454` | one `&mut IoBuf` |
| receiver | `lib.rs:1368` passes `chained_reader` and `counting_writer`; 24 production `writer.flush()` sites under `receiver/` | separate halves, both on the same core |
| exit exchange | `error_exit.rs:212`, `lib.rs:1478-1513` | separate halves |

The receiver's halves already share one core, so nothing in section 4.2
requires changing its signatures. Converting them is a refactor with no
behaviour of its own.

## 5. Readiness on each platform

### 5.1 Unix

`poll(2)` on the two descriptors through rustix, no unsafe
(`transport.rs:161-288`). Work left: build `FdTransport::socket` for the
daemon's `TcpStream` and the client's daemon socket, and `FdTransport::pair`
for a remote-shell child's pipes. `O_NONBLOCK` lives on the open file
description, so the daemon must stop handing a second blocking clone of the
socket to any other reader first; the goodbye drain
(`daemon/.../transfer/orchestration.rs`) has to read through the same core.

### 5.2 Windows

Upstream on Windows is a Cygwin build: `perform_io` runs unchanged because
the Cygwin layer supplies `poll()` and `O_NONBLOCK` for sockets and pipes.
That statement is from Cygwin's documented POSIX surface, not from a source
line read for this note.

Natively:

| handle | option | needs | verdict |
|---|---|---|---|
| socket | non-blocking + `WSAPoll` | one unsafe call, already in `fast_io` (`transport.rs:358`) | use it; wire `SocketTransport` to the daemon and the client daemon socket |
| pipe | `PIPE_NOWAIT` + `PeekNamedPipe` | no wait primitive, so a sleep loop | rejected: latency or CPU |
| pipe | overlapped I/O + `WaitForMultipleObjects` | the handle must be opened `FILE_FLAG_OVERLAPPED`; true for pipes oc creates for a child, false for the stdio a `--server` inherits from sshd; new unsafe in `fast_io` | rejected as the general answer: it cannot cover inherited stdio |
| pipe | a reader thread and a writer thread over blocking `Read`/`Write` | std only, no unsafe | recommended |

Recommendation: `SocketTransport` for sockets, and one `PumpTransport<R, W>`
in `fast_io::transport` for every stream that cannot be polled, on every
platform: Windows pipes, the embedded-SSH channel pair and the QUIC stream.

`PumpTransport` design: two bounded byte queues (32 KiB in, 64 KiB out)
behind one `Mutex` and `Condvar`; a reader thread blocks in `read` and
appends; a writer thread pops and blocks in `write`. `try_read` and
`try_write` touch only the queues; `wait(input, output, timeout)` is a
`Condvar::wait_timeout` on "in queue non-empty or closed" and "out queue has
room or failed". A write error is stored and returned by the next
`try_write`. Trade-offs: one extra copy and two threads per connection; a
reader thread parked in a blocking read cannot be interrupted, so it ends
with the stream (process exit for `--server`, child exit for a client); and
`--timeout` finally bounds these paths, which `BlockingStreams::wait` cannot
do (`transport.rs:114-124`). `BlockingStreams` stays for in-memory tests
only.

## 6. Threading model

### 6.1 Ownership

`SharedCore` is `Rc<RefCell<Core>>`, so the core and both stacks stay on one
thread by construction (`core.rs:59-62`). That thread is the role's wire
thread. Signature workers (`receiver/transfer/pipeline.rs:583`) and the disk
commit thread never touch it; they exchange owned buffers with the wire
thread.

Rules:

- The wire thread blocks on the network only inside `Core::wait`.
- Each pass services input before output and writes one chunk, as upstream
  does (io.c:898, io.c:948), so neither direction can starve the other.
- A wire thread stalled by disk back-pressure services no I/O. That is
  upstream's receiver blocked in `write(2)` on the destination, and is
  bounded by disk progress.

### 6.2 The receiving side

Upstream's guarantee (section 3) has to be rebuilt for one thread. The rule:
the receiver never blocks on output while a reply is outstanding.

- Requests are appended to the mux output without waiting for room
  (`MsgRoom::Grow` semantics for data on this role: `MuxOut::push_unbounded`).
- A new request is admitted only while the queued output is below a byte
  cap (1 MiB) and the file window has a free slot. A single request larger
  than the cap is still queued whole.
- Output leaves through `Core::pump_output` during read waits (M2).
- The receiver's explicit `writer.flush()` calls become "pump during the
  next wait"; a blocking flush is allowed only when no reply is outstanding
  (end of phase after the window is empty, goodbye).

This holds memory to the cap plus one signature, needs no extra thread, and
makes `DrainingReader` unnecessary. Once it is in, `DrainingReader`, its
handle and the stop-before-goodbye ordering are deleted.

The alternative is upstream's shape: a generator thread and a receiver
thread, each with its own core, joined by an in-process pipe. It closes the
same case structurally and matches the planned sub-list forwarding to a
generator, but it is a far larger change. Section 10 lists it as a decision.

## 7. Deadlock and message-loss cases, as tests

| id | transfer | what fills | who blocks | today | test |
|---|---|---|---|---|---|
| D1 | push or pull over a remote shell, 100 x 4 MiB files, identical basis backdated so every file is delta-resent | receiver writes up to 64 block signatures before reading a reply; sender's match tokens fill the pipe | both in `write` | hangs: v0.6.4 killed at 150 s in both directions where 3.5.2 takes 24 s and 32 s; passes with `OC_RSYNC_PIPELINE_WINDOW=1`. On master the client side is `BlockingStreams` (`lib.rs:883`) and the Unix server reads only 32 KiB ahead (`core.rs:285-286`) | e2e: pipe-only rsh wrapper, both directions, oc-oc and oc-3.5.2 pairs. Unit: a scripted `Transport` that accepts 64 KiB of output until its input is read; the receiver loop must finish and must never call `wait(false, true)` while a reply is outstanding |
| D2 | the D1 fixture against a daemon on a platform without `DrainingReader` | TCP buffers | both in `send` | unprotected (`streams.rs:49-52`) | the same unit test over `SocketTransport`; e2e daemon cell on the Windows job |
| D3 | daemon pull or push on Unix, one multi-GiB changed file followed by 63 more requests | `DrainingReader`'s unbounded queue | nobody; memory grows with the file | predicted from `streams.rs:297`, not measured | peak RSS of the daemon stays under 64 MiB on a 2 GiB literal file with a full request window |
| D4 | receiver ends a phase with a blocking flush while replies are outstanding | as D1 | both in `write` | predicted from the D1 mechanism; the 24 receiver flush sites (for example `receiver/transfer/phases.rs:75-113`) have not been traced one by one | unit: phase end with a stalled scripted transport and one buffered reply; the reply must be consumed before the phase marker is written |
| D5 | push to a daemon that refuses after the client has begun writing | the daemon's `MSG_ERROR` frames sit in the socket; the client's write fails | nobody; the refusal text is lost and the exit code is the client's own | depends on `waits_for_peer` (`lib.rs:1500`) | the 2x2 client/daemon matrix from `iobuf-perform-io.md`: all four cells print the refusal and exit with the daemon's code; unit: `Core::write` error with a buffered `MSG_ERROR` frame dispatches it before returning |
| D6 | `--timeout=10`, the peer silent for 6 s while oc waits in a read | nothing | the peer times oc out | no keepalive from the wait (`core.rs:293-294`) | unit with the clock seam: `SilentPeer` plus a mux; after the first empty wait an empty `MSG_DATA` is written (io.c:1665-1666) |
| D7 | receiver or generator role queues more than 32 KiB of messages while output is stalled | `msgs` | the role, in `write_messages` | `writer/multiplex.rs:163-165` | unit: 40 KiB of `MSG_INFO` on a sink that accepts nothing; no write is attempted and every frame is kept in order |
| D8 | sender blocked writing a large literal run while the peer sends more than 32 KiB of `MSG_INFO` | `InBuf` | nobody against a 3.5.2 peer; messages print late | `core.rs:285-286` stops polling input when `InBuf` is full | unit: a write wait with 48 KiB of buffered message frames dispatches them all before the write completes |

## 8. Performance gate (task 702)

Cells: daemon pull, daemon push, remote-shell pull, remote-shell push; the
remote-shell cells once through the pipe-only wrapper and once through
loopback ssh.

Workloads: 100,000 files of 4 KiB (syscall-bound); one 1 GiB file, whole
file (throughput); the D1 fixture (delta); 10,000 files with `-z`.

Method: `scripts/benchmark_hyperfine.sh`, at least 10 runs per cell,
candidate and master built from the same toolchain and run interleaved in
the same session; `strace -f -c` once per cell and binary.

Compared with master, per cell:

| metric | bar |
|---|---|
| wall time, mean | within 5% |
| total syscalls | within 5% |
| `poll`/`ppoll` count | no more than `read` + `write` + `writev` + `sendto` + `recvfrom` count; the read-first rule of `core.rs:190-193` must survive |
| socket writes per file on the 100,000-file cell | not above master |
| peak RSS | within 10% |

Reported beside it, not gated: the same counts for 3.5.2. A cell that moves
from `BlockingStreams` to a pollable transport is expected to add polls; the
gate is the total, not the mix. D1 has no master baseline because master
does not finish; its bar is 3.5.2's wall time plus 5%.

## 9. Implementation plan

Each step is independently mergeable. "Byte-neutral" means the bytes on the
wire are identical to master for the same input.

| step | change | test | wire |
|---|---|---|---|
| 1 | M2: `MuxOut::write_some`, `Core::pump_output`, read waits on `(input, output_pending)` | unit: a read with stalled output and arriving input returns the input without the output having drained | byte-neutral |
| 2 | M1 on Unix: `FdTransport::socket` for the daemon and the client daemon socket, `FdTransport::pair` for the remote-shell child; goodbye drain through the core | existing daemon and rsh suites; wire transcript equal to master on all four cells | byte-neutral |
| 3 | `PumpTransport`; embedded SSH, QUIC and non-Unix stdio move onto it | unit: readiness, EOF, stored write error, timeout through the clock seam; `BlockingStreams` no longer referenced outside tests | byte-neutral |
| 4 | M1 on Windows: `SocketTransport` for daemon and client sockets | D2 unit test on the Windows job | byte-neutral |
| 5 | M8: receiver output queue with byte cap, no blocking flush while replies are outstanding; replaces tasks 571's flush removal | D1 and D4 unit and e2e tests | payload identical; `MSG_DATA` frame boundaries move, compared with 3.5.2 rather than master |
| 6 | delete `DrainingReader`, `should_arm_delta_drain`, the drain handle and its ordering | D3 RSS test; daemon suite | byte-neutral |
| 7 | M3: keepalive from the wait; remove call sites that only existed to cover blocking reads | D6 | changes bytes only under `--timeout` |
| 8 | M4 and M9: frame cursor in the core, `service_messages` | D8; reader tests unchanged | byte-neutral |
| 9 | M5: `drain_messages` on a write error | D5 | byte-neutral on success paths |
| 10 | M6: `MsgRoom::Grow` off the sender | D7 | byte-neutral unless the queue overflowed |
| 11 | M7: `Core::input_idle` drives the lookahead | sender lookahead reaches `MAX_FILECNT_LOOKAHEAD` on an idle link in a scripted-transport test | sub-list timing changes, as upstream |
| 12 | performance gate, section 8 | the table of section 8, attached to the change that completes step 6 | - |

Steps 1, 2 and 5 are the shortest path that closes D1 on Unix; 3 and 4
extend it to every transport.

## 10. Decisions needed

1. Receiving side: the single-thread rule of section 6.2 (recommended now),
   or a generator thread plus a receiver thread as upstream forks them.
2. Whether step 5 may change `MSG_DATA` frame boundaries relative to master,
   given that payload bytes and their order are unchanged.
3. `PumpTransport` (two threads, no unsafe) for Windows pipes, against
   overlapped I/O in `fast_io` for the pipes oc creates itself.
4. Whether a stopgap for D1 ships before step 5: lowering the default
   window does not close it, only `OC_RSYNC_PIPELINE_WINDOW=1` does, at a
   throughput cost that has not been measured.

## 11. Where the earlier notes are out of date

`iobuf-perform-io.md`:

- "No code has moved yet" - the core, the transports and the timeout are on
  master.
- "Deliberately not ported: perform_io's poll loop" - superseded;
  `Core::wait` is that loop.
- The inventory (measured on `9746b7d7f`) cites a reader stack built at
  `lib.rs:1041-1042` and the flush gate at `transfer_loop.rs:658`; the
  stacks now sit on `CoreReader`/`CoreWriter` (`lib.rs:945-946`) and the
  gate is at `transfer_loop.rs:646`. Section 4.3 above replaces it.
- Its `io.c` line numbers for `read_a_msg` exits are from an older release.

`perform-io-convergence.md`:

- Describes upstream 3.4.4 with a `select()` loop; 3.5.2 uses `poll()`
  (io.c:864) and every line number has moved.
- Places the core in `rsync_io` as `PerformIo` with a `poll_duplex` helper;
  what landed is `Core` in `transfer` over `fast_io::transport::Transport`.
- States that SSH and stdio never wedge because each direction is its own
  pipe. Case D1 shows they do: two independent pipes still fill.
- Says the convergence makes the fixed 32 KiB input buffer sufficient. It is
  sufficient upstream only because of the process split in section 3.
- Its staging (daemon first, then the rest, then delete the drain thread) is
  still right and is steps 2 to 6 above.
