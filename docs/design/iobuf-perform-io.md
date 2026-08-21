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
| `PIO_NEED_INPUT` / `PIO_NEED_OUTROOM` / `PIO_NEED_MSGROOM` | what this `perform_io` call is waiting for; mutually exclusive | io.c:189-191 |
| `PIO_NEED_FLAGS` | the mask of those three | io.c:196 |
| `IN_MULTIPLEXED` | `in_multiplexed != 0` | io.c:185 |
| `IN_MULTIPLEXED_AND_READY` | `in_multiplexed > 0` | io.c:186 |
| read-error branch | reports and exits; **does not drain** | io.c:900-907 |
| write-error branch | reports, **drains**, then exits | io.c:943-949 |
| `drain_multiplex_messages()` | the drain itself | io.c:1902-1918 |
| no-fd-for-output branch | the **second** drain site | io.c:816-822 |

The asymmetry between the two error branches is deliberate and worth stating:
the read branch (io.c:900-907) exits without draining, because if the *read* is
what failed there is nothing left to drain. Only the write branch drains.

### `in_multiplexed` is a tri-state, and the third state is the point

It is an `int`, not a flag:

- `0` - raw mode. Set by `io_end_multiplex_in` (io.c:2677).
- `1` - multiplexed, not currently inside a message. Set by
  `io_start_multiplex_in` (io.c:2666) and restored at **every** exit arm of
  `read_a_msg` (io.c:1687, :1693, :1699, :1710, :1716, :1738, :1746, :1791,
  :1803, :1817, :1845, :1861).
- `-1` - **inside `read_a_msg`**. Set once at its top (io.c:1665).

So `IN_MULTIPLEXED_AND_READY` (`> 0`) is a **reentrancy guard**, not a
readiness hint: it means "not already inside `read_a_msg`". `read_a_msg` asserts
the invariant on the way out (`assert(iobuf.in_multiplexed > 0)`, io.c:1899).

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

upstream: io.c:1902-1918, loop condition at io.c:1904. Two properties are
load-bearing:

1. **It never blocks.** The loop condition is `iobuf.in.len` - bytes *already
   buffered*. It drains what has arrived and stops. It does not read the socket
   for more. A drain that blocks would hang exactly when the peer is gone.
2. **It routes through the normal handler.** `read_a_msg()` dispatches each
   message the usual way, so `MSG_ERROR_XFER` prints and `MSG_ERROR_EXIT`
   records the peer's exit code. The drain adds no rendering of its own.

Both call sites set `msgs2stderr = 1` immediately before draining (io.c:818,
io.c:944) so the drained messages are written locally rather than forwarded to a
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

upstream: io.c:943-949. Note the ordering: upstream prints *its own* write error
first and drains second. In the measurement above the peer's message appeared
*before* the write error - because it had already been read by an earlier
`perform_io` pass, not by this drain. Both orderings are upstream-consistent;
the drain is the backstop, not the primary path.

`out_fd = -2` is a sentinel distinct from `-1`: `whine_about_eof` (io.c:264)
keys off it, and both drain sites check it (io.c:820, and the read side sets
`in_fd = -2` at io.c:893).

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
