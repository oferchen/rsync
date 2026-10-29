# One I/O object: upstream's `iobuf` and `perform_io`

Status: design. No code has moved yet; this is the contract the IOBUF-* steps
implement.

## The defect that motivates it

An oc client pushing to an oc daemon that refuses an option reports its own
broken pipe and loses the daemon's reason. Measured on Linux over TCP, with the
same fixture on all four cells:

| daemon | client | rc | refusal text |
|---|---|---|---|
| oc | oc | 23 | **no** - `Broken pipe (os error 32)` |
| oc | upstream | 1 | yes |
| upstream | oc | 4 | yes |
| upstream | upstream | 4 | yes |

Three of four pass. The oc daemon does write the frame - upstream's client reads
it. The oc client can render a daemon refusal - upstream's daemon gets through.
Only the oc-to-oc pair loses it, and the cause is that oc reports the write
error without first draining what the peer already sent.

Upstream does not have this problem because it never holds a write failure and
unread input apart.

## Upstream's structure

One file-scope object owns both directions.

```c
static struct {
	xbuf in, out, msg;
	int in_fd;
	int out_fd; /* Both "out" and "msg" go to this fd. */
	int in_multiplexed;
	unsigned out_empty_len;
	size_t raw_data_header_pos;      /* in the out xbuf */
	size_t raw_flushing_ends_before; /* in the out xbuf */
	size_t raw_input_ends_before;    /* in the in xbuf */
} iobuf = { .in_fd = -1, .out_fd = -1 };
```

upstream: io.c:101-110.

| construct | meaning | anchor |
|---|---|---|
| `PIO_NEED_INPUT` / `PIO_NEED_OUTROOM` / `PIO_NEED_MSGROOM` | what this `perform_io` call is waiting for; mutually exclusive | io.c:207-209 |
| `PIO_NEED_FLAGS` | the mask of those three | io.c:214 |
| `IN_MULTIPLEXED` | `in_multiplexed != 0` | io.c:203 |
| `IN_MULTIPLEXED_AND_READY` | `in_multiplexed > 0` | io.c:204 |
| read-error branch | reports and exits; **does not drain** | io.c:918-925 |
| write-error branch | reports, **drains**, then exits | io.c:961-967 |
| `drain_multiplex_messages()` | the drain itself | io.c:1940-1956 |
| no-fd-for-output branch | the **second** drain site | io.c:834-840 |

The asymmetry between the two error branches is deliberate and worth stating:
the read branch (io.c:918-925) exits without draining, because if the *read* is
what failed there is nothing left to drain. Only the write branch drains.

### `in_multiplexed` is a tri-state, and the third state is the point

It is an `int`, not a flag:

- `0` - raw mode. Set by `io_end_multiplex_in` (io.c:2715).
- `1` - multiplexed, not currently inside a message. Set by
  `io_start_multiplex_in` (io.c:2704) and restored at **every** exit arm of
  `read_a_msg` (io.c:1713, :1693, :1699, :1710, :1716, :1738, :1746, :1791,
  :1803, :1817, :1845, :1861).
- `-1` - **inside `read_a_msg`**. Set once at its top (io.c:1691).

So `IN_MULTIPLEXED_AND_READY` (`> 0`) is a **reentrancy guard**, not a
readiness hint: it means "not already inside `read_a_msg`". `read_a_msg` asserts
the invariant on the way out (`assert(iobuf.in_multiplexed > 0)`, io.c:1937).

This is the single most important thing to carry over. Modelling it as a `bool`
collapses `1` and `-1` into "true" and makes the drain re-enter the reader from
inside itself.

### The drain

```c
static void drain_multiplex_messages(void)
{
	while (IN_MULTIPLEXED_AND_READY && iobuf.in.len) {
		if (iobuf.raw_input_ends_before) { /* skip the raw region */ }
		read_a_msg();
	}
}
```

upstream: io.c:1940-1956, loop condition at io.c:1942. Two properties are
load-bearing:

1. **It never blocks.** The loop condition is `iobuf.in.len` - bytes *already
   buffered*. It drains what has arrived and stops. It does not read the socket
   for more. A drain that blocks would hang exactly when the peer is gone.
2. **It routes through the normal handler.** `read_a_msg()` dispatches each
   message the usual way, so `MSG_ERROR_XFER` prints and `MSG_ERROR_EXIT`
   records the peer's exit code. The drain adds no rendering of its own.

Both call sites set `msgs2stderr = 1` immediately before draining (io.c:836,
io.c:962) so the drained messages are written locally rather than forwarded to a
peer that is no longer there.

### The write-error branch in full

```c
/* Don't write errors on a dead socket. */
msgs2stderr = 1;
iobuf.out_fd = -2;
iobuf.out.len = iobuf.msg.len = iobuf.raw_flushing_ends_before = 0;
rsyserr(FERROR_SOCKET, errno, "write error");
drain_multiplex_messages();
exit_cleanup(RERR_SOCKETIO);
```

upstream: io.c:961-967. Note the ordering: upstream prints *its own* write error
first and drains second. In the measurement above the peer's message appeared
*before* the write error - because it had already been read by an earlier
`perform_io` pass, not by this drain. Both orderings are upstream-consistent;
the drain is the backstop, not the primary path.

`out_fd = -2` is a sentinel distinct from `-1`: `whine_about_eof` (io.c:282)
keys off it, and both drain sites check it (io.c:838, and the read side sets
`in_fd = -2` at io.c:911).

### Where the exit code comes from

For an option refusal the code is not `RERR_SOCKETIO`. `rsync_module()` calls
`io_start_multiplex_out(f_out)` *before* the refusal check, then:

```c
if (!ret || err_msg) {
	... option_error(); msleep(400); exit_cleanup(RERR_UNSUPPORTED);
}
```

upstream: clientserver.c:1254-1268. `option_error()` is
`rprintf(FERROR, RSYNC_NAME ": %s", err_buf)` followed by `io_flush(MSG_FLUSH)`
(options.c:907-916). `RERR_UNSUPPORTED` is 4, matching both measured
upstream-daemon cells.

The `msleep(400)` is a deliberate linger so the client can drain before the
socket closes.

## Mapping onto oc

| upstream | oc today | gap |
|---|---|---|
| `iobuf` owning both directions | reader and writer are **separate values** threaded into each role context (`crates/transfer/src/lib.rs:1028`) | the whole refactor |
| `in_multiplexed` tri-state | multiplex state lives inside the reader | must survive as a tri-state, not a bool |
| `drain_multiplex_messages()` | **absent** | IOBUF-1b |
| `read_a_msg()` dispatch | the multiplex reader already prints `MSG_ERROR_XFER` and turns `MSG_ERROR_EXIT` into `RemoteExitError` | present - reuse, do not rebuild |
| peer exit code wins | `map_server_transfer_error` already recovers `RemoteExitError` (`crates/core/.../orchestration/transfer.rs:273`) | present - reuse |
| `msgs2stderr = 1` before draining | oc has no forward-vs-local switch at this layer | decide during IOBUF-1b |
| `msleep(400)` linger | absent | IOBUF-3a, **only if measurement shows it matters** |

The downstream half already works. Once the drain runs, the text and the exit
code both follow from code that is already there. That is the strongest evidence
that the missing piece is ownership, not rendering.

## Deliberately not ported

State these so a later reader does not "fix" the omission.

- **The `xbuf` ring buffer** (`in.pos` / `in.len` wraparound, `out_empty_len`,
  `raw_data_header_pos`). oc buffers through `std::io` types. The contract to
  mirror is *who owns what* and *when the drain runs*, not upstream's manual
  buffer arithmetic. Porting the ring would be a large change with no
  observable effect.
- **`perform_io`'s poll loop.** Upstream multiplexes one thread over both
  directions with `poll`; oc's roles are structured differently. What must be
  mirrored is that a write failure cannot be reported without first consulting
  already-received input - not the scheduling mechanism that achieves it.
- **`msleep(400)`**, pending measurement. See IOBUF-3a.

## Gates

- Wire byte-neutrality on all four network cells (IOBUF-0b), captured **before**
  the first structural commit.
- The 2x2 cross-implementation matrix goes 4/4, with the three currently-passing
  cells as non-vacuity companions (IOBUF-3b).
- Both 3.5.0 pipe legs unchanged; both TCP rows flip in the same commit as the
  behaviour change.

## Inventory: where the two halves travel apart (IOBUF-0c)

Measured on master `9746b7d7f`. A site is listed when it takes, builds or
splits a reader and a writer as two values. Data-copy helpers that happen to
take a source and a sink (`compress`, the local-copy engine, `files_from`
forwarding) are not transport halves and are left out.

### 1. Where the pair is built

| site | what it builds |
|---|---|
| `crates/transfer/src/lib.rs:1041-1042` | `ServerReader<IoBufReader<CountingReader<..>>>` over the chained stdin |
| `crates/transfer/src/lib.rs:1079-1090` | `ServerWriter<CountingWriter<ThrottlingWriter<W>>>` over stdout |
| `crates/transfer/src/lib.rs:1217`, `:1283` | hands both to the receiver `ctx.run` and the generator `ctx.run` |

Every network role passes through this one function,
`run_server_with_handshake_adopting`. That makes it the single place where one
owner can be built.

### 2. Role entry points and the functions that thread both halves

| role | functions taking `reader` and `writer` separately |
|---|---|
| generator (oc sender) | `orchestrator.rs:95` `goodbye_draining_source_removals`, `:143` `run`; `transfer_loop.rs:464` `run_transfer_loop`; `goodbye.rs:137` `handle_goodbye`, `:175` `handle_goodbye_with_finalizer`, `:243` `answer_goodbye_with_finalizer` |
| receiver | `receiver/transfer.rs:63` `run`, `:187` `finish_empty_client_flist`; `setup/context.rs:99` `setup_transfer`; `sync.rs:39` `run_sync`; `pipelined.rs:35` `run_pipelined`; `pipelined_incremental.rs:35`, `:586`, `:1029`, `:1170`; `pipeline.rs:402` `run_pipeline_loop_decoupled`, `:1246` `run_dry_run_loop`, `:1410` `run_only_write_batch_loop`; `phases.rs:34`, `:273`, `:375`, `:398`; `replay.rs:54`, `:716`; `mode.rs:96`, `:152` |

That makes 6 generator functions and 21 receiver functions. Each one is an argument pair that a
single `&mut IoBuf` replaces.

### 3. The only coupling between the halves today

`transfer_loop.rs:658` asks the reader `has_buffered_input()` before deciding
whether to flush the writer (`reader/mod.rs:37`, `reader/server.rs:418`). This
is the one place where oc already makes upstream's `perform_io` decision, which
reads "input is already buffered, so do not flush" (io.c:658-742). Because the
decision is made from outside both halves, the drain-on-write-error that
upstream runs inside the same loop (io.c:961-967) has nowhere to live.

### 4. Transport splits below the engine

| site | split |
|---|---|
| `crates/core/src/client/remote/daemon_transfer/mod.rs:230`, `:406` | client daemon socket into `DaemonStreamReader` and `DaemonStreamWriter`, threaded through `orchestration/transfer.rs:46` `run_pull_transfer`, `:175` `run_push_transfer`, `mod.rs:317` `close_then_reap`, and `connection/mod.rs:375` `perform_daemon_handshake` |
| `crates/core/src/client/remote/ssh_transfer/drive.rs:293` | ssh child into stdout reader, stdin writer and child handle |
| `crates/core/src/client/remote/embedded_ssh_transfer.rs:366` | embedded ssh channel into a reader and a writer |
| `crates/core/src/client/remote/async_ssh_transport.rs:450`, `:586` | async transport halves bridged to sync channels for `run_blocking_server` |
| `crates/daemon/src/daemon/sections/module_access/transfer/streams.rs:163-297` | daemon socket into two `try_clone()` fds; the read clone is wrapped in `DrainingReader` (`draining_reader.rs`) |
| `crates/daemon/src/daemon/sections/module_access/transfer/orchestration.rs:1152`, `:1191` | goodbye drain on a third clone, after the transfer halves are dropped |
| `crates/daemon/src/daemon_stream.rs:35` | the daemon's stream wrapper, built from separate halves |
| `crates/daemon/src/daemon/sections/server_runtime/quic_listener.rs:391-412` | the QUIC stream, cloned into a reader and relayed with `copy_chunks` |
| `crates/core/src/client/module_list/connect/mod.rs:231` | the I/O-timeout re-apply hook, built from both halves |

The transports stay split: an rsh child really does have two pipes. IoBuf owns
the pair above the transport (section 1), so the routing in IOBUF-2b (task 427)
is about who reads first after a write fails, not about re-joining the
descriptors.
