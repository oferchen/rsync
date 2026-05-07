# Daemon BufReader vs SSH raw stdio: buffering layer audit

Tracking issue: #1039.

Last verified against
`crates/daemon/src/daemon/sections/{session_runtime.rs,proxy_protocol.rs,name_converter.rs,module_access/{request,authentication,transfer}.rs}`,
`crates/daemon/src/daemon/async_session/session.rs`,
`crates/transfer/src/{lib.rs,handshake.rs,adaptive_buffer.rs,generator/mod.rs,receiver/transfer.rs,disk_commit/writer.rs,writer/{server,multiplex}.rs}`,
`crates/rsync_io/src/ssh/{connection.rs,aux_channel.rs}`,
`crates/core/src/client/remote/ssh_transfer.rs`.

## Scope

The daemon transport (TCP) and the SSH transport reach the shared
`run_server_with_handshake()` entry point through different I/O
plumbing. The daemon side wraps the `TcpStream` in a `BufReader` for
the entire greeting/auth/argument phase, then hands the raw stream to
the protocol engine. The SSH side never wraps `ChildStdout`/`ChildStdin`
at the transport layer; the protocol engine adds its own 64 KiB
`BufReader` once raw mode ends.

This audit catalogues every buffering layer on both paths, identifies
where the layers differ, quantifies the syscall implications, and
proposes fixes for redundant or missing buffering.

## 1. Daemon-side buffering inventory

### 1.1 Greeting and module selection (`session_runtime.rs:220`)

`handle_legacy_session()` constructs `BufReader::new(stream)` directly
from the accepted `TcpStream` (default 8 KiB capacity from
`std::io::DEFAULT_BUF_SIZE`). The buffered reader serves
`read_trimmed_line()` for the `@RSYNCD:` greeting, version exchange,
`#early_input=`, and module-name request. Writes go through
`reader.get_mut()` directly to the underlying stream, then
`.flush()` is called explicitly after each greeting line.

### 1.2 PROXY protocol (`proxy_protocol.rs:216`)

`parse_proxy_header()` builds a transient `BufReader::new(stream)` to
peek the v1 ASCII or v2 binary signature ahead of the rsync greeting.
The borrowed reader is dropped immediately, leaving any unconsumed
buffer bytes behind on the kernel socket buffer rather than the userland
buffer. Because PROXY headers always end on a known boundary
(`\r\n` for v1, fixed length for v2), no data leaks.

### 1.3 Authentication and argument handshake (`module_access/`)

`request.rs` and `authentication.rs` thread the same
`BufReader<TcpStream>` from step 1.1 through challenge/response and
`@RSYNCD: AUTHREQD` lines. `transfer.rs:77` extracts the still-buffered
bytes via `reader.buffer().to_vec()` into `HandshakeResult.buffered`,
because the kernel may have coalesced subsequent compat-flag bytes
into the same `recv()` that delivered the final auth line.

### 1.4 Transfer dispatch (`module_access/transfer.rs:48`)

`setup_transfer_streams()` calls `stream.set_nodelay(true)` and
`stream.try_clone()` twice to obtain unwrapped `read_stream` and
`write_stream` `TcpStream` halves. **The `BufReader` is discarded
here** - only its leftover buffer survives, ferried into
`HandshakeResult`. The downstream call site
(`transfer.rs:127-133`) explicitly comments that "standard buffered
I/O" is used for the daemon socket; in practice this means the
`BufReader` added by `transfer/lib.rs:495` (see Section 3) is the
sole remaining read-side buffer.

### 1.5 Async daemon session (`async_session/session.rs:131-133`)

The Tokio-based listener wraps `tokio::io::BufReader` and
`tokio::io::BufWriter` (Tokio defaults: 8 KiB read, 8 KiB write) around
the split halves of an async `TcpStream`. This path currently terminates
at version exchange and module listing; full transfers fall back to
the synchronous path.

### 1.6 Name converter subprocess (`name_converter.rs:13-29`)

The optional uid/gid name converter wraps the spawned child with
`io::BufWriter::new(ChildStdin)` and `io::BufReader::new(ChildStdout)`
(both default 8 KiB). Each query issues `write_all` + `flush` then
`read_line`, so the buffers act primarily as line accumulators rather
than syscall amortization. This is a pure subprocess channel, not a
network transport.

## 2. SSH-side raw stdio inventory

### 2.1 Connection split (`rsync_io/src/ssh/connection.rs:178-208`)

`SshConnection::split()` returns three plain handles:

```rust
SshReader { stdout: ChildStdout }
SshWriter { stdin:  ChildStdin  }
SshChildHandle { child, stderr_drain, connect_watchdog }
```

`SshReader::read()` and `SshWriter::write()`/`flush()` call straight
through to `ChildStdout`/`ChildStdin` - **no `BufReader` or
`BufWriter` is interposed**. The transport delegates all buffering
to the protocol engine.

### 2.2 Stderr aux channel (`rsync_io/src/ssh/aux_channel.rs:208-228`)

The stderr drain thread wraps the stderr pipe in `BufReader::new(source)`
(default 8 KiB) and uses `read_until(b'\n')` to forward lines to local
stderr while collecting up to 64 KiB into a bounded ring. This is the
only buffer on the SSH transport, and it is on a side channel that
never carries protocol data.

### 2.3 Handshake entry (`core/.../ssh_transfer.rs:551-560`)

`run_server_over_ssh_connection()` calls `connection.split()` and
passes the bare `&mut SshReader`/`&mut SshWriter` into
`crate::server::perform_handshake()`. The legacy ASCII handshake path
(`transfer/handshake.rs:131`) does construct a transient
`BufReader::new(stdin)` for the `@RSYNCD:` greeting, but for protocol
>=30 the binary handshake reads exactly 4 bytes via `read_exact` on
the raw reader. No persistent buffer is attached.

## 3. Shared protocol engine buffering (post-handshake)

After the role-specific entry points hand control to
`run_server_with_handshake()`, both daemon and SSH paths converge in
`crates/transfer/src/lib.rs:489-497`:

- `stdout.flush()` drains any raw-mode bytes (compat flags, vstrings).
- The reader is wrapped in
  `io::BufReader::with_capacity(64 * 1024, chained_stdin)`, mirroring
  upstream `iobuf.in` (32 KiB circular in `io.c`, doubled here).
- The writer is wrapped in `ServerWriter::new_plain(stdout)`. Once
  `activate_multiplex()` runs, the writer becomes a
  `MultiplexWriter` whose internal `Vec<u8>` is preallocated to
  `DEFAULT_BUFFER_SIZE = 64 * 1024` (`writer/multiplex.rs:32`).

For the daemon path `chained_stdin` is `Cursor::new(buffered).chain(read_stream)`
where `buffered` is the BufReader leftover from Section 1.4. For the
SSH path `chained_stdin` is the bare `SshReader`.

## 4. Per-file disk and source buffering (post-protocol)

These layers are independent of transport but worth listing because
they sit on the same data path:

- Receiver temp-file write: `BufWriter::with_capacity(adaptive_writer_capacity(size), file)`
  in `receiver/transfer.rs:232`. Capacity ranges 4 KiB to 1 MiB by
  file size (`adaptive_buffer.rs:87-97`).
- Disk commit thread: `ReusableBufWriter` at
  `disk_commit/writer.rs:71` reuses a 256 KiB buffer per worker
  (`WRITE_BUF_SIZE`); chunks at or above `DIRECT_WRITE_THRESHOLD = 8 KiB`
  bypass the buffer via `write_all_vectored`.
- Source file read on the generator: `BufReader::with_capacity(adaptive_buffer_size(size), f)`
  at `generator/mod.rs:738-742`, with the >=1 MiB path preferring
  `fast_io::reader_from_path` (io_uring on Linux 5.6+).
- Filter merge: `BufReader::new(file)` at `generator/filters.rs:320`
  (default 8 KiB) for `.rsync-filter` parsing.
- Local-copy basis read: `BufReader::new(File)` at
  `generator/delta.rs:418` for digest-mismatch fallback.

## 5. Syscall implications

| Path                            | Reads coalesced?           | Writes coalesced?          | Buffer at transport |
|---------------------------------|----------------------------|----------------------------|---------------------|
| Daemon greeting/auth            | yes (8 KiB BufReader)      | no (per-line `flush`)      | TCP                 |
| Daemon transfer (post-flush)    | yes (64 KiB BufReader)     | yes (64 KiB MultiplexWriter)| TCP, NODELAY       |
| Daemon async session            | yes (8 KiB Tokio BufReader)| yes (8 KiB Tokio BufWriter)| TCP                 |
| SSH greeting                    | no (raw `read_exact(4)`)   | no (`write_all(4)`)        | pipe                |
| SSH transfer (post-flush)       | yes (64 KiB BufReader)     | yes (64 KiB MultiplexWriter)| pipe                |
| SSH stderr drain                | yes (8 KiB BufReader)      | n/a                        | pipe (side channel) |

Two practical consequences:

1. **SSH binary handshake issues four single-byte-equivalent syscalls
   for the version/sub-version/flags exchange.** Pipe reads can return
   short, so `read_exact(&mut [0u8; 4])` becomes a loop. Empirically
   one to four `read()` calls per side, per connection. Negligible at
   scale, but a Section 6 fix.
2. **Daemon greeting writes call `flush()` after every line** while
   `TCP_NODELAY` is set later (in `setup_transfer_streams`). The
   greeting therefore travels in a single segment because Nagle is
   still active during the auth phase and the kernel coalesces small
   sends. Once `set_nodelay(true)` runs, post-greeting `flush()` calls
   would each become a separate segment - but by then `MultiplexWriter`
   is doing the buffering, so the timing is benign.

## 6. Findings and proposed fixes

### F1. Discard the daemon `BufReader` after handshake without re-reading

**Status:** correctly handled.
`module_access/transfer.rs:83` extracts `reader.buffer().to_vec()` and
chains it ahead of the raw stream in `transfer/lib.rs:347-352`.
Removing the chain would silently drop coalesced compat-flag bytes.
Document this invariant in `setup_transfer_streams`'s rustdoc - it is
currently only enforced by a comment in `transfer/lib.rs:344-346`.

### F2. SSH handshake should buffer the 4-byte version exchange

**Issue:** `transfer/handshake.rs::perform_handshake` calls
`stdin.read_exact(&mut buf[..4])` directly on `SshReader`, which is
unwrapped `ChildStdout`. On a busy pipe this can incur up to four
`read()` syscalls.

**Fix:** wrap with a small transient `BufReader::with_capacity(64, stdin)`
for the handshake, then extract the leftover via `reader.buffer()` into
`HandshakeResult.buffered` exactly as the daemon path does. The 64 KiB
post-handshake `BufReader` would chain on top, matching the daemon
chaining pattern in Section 3.

### F3. Drop the per-greeting `flush()` in the daemon legacy path

**Issue:** `session_runtime.rs:226` flushes after writing the greeting,
and `advertise_capabilities` flushes after each `@RSYNCD: ` line.
Because no `BufWriter` is interposed, every `flush()` is a no-op on the
underlying `TcpStream` - `write_all` already wrote to the kernel.

**Fix:** remove the redundant `flush()` calls or, if the intent is to
amortize syscalls, introduce a `BufWriter<TcpStream>` for the auth
phase (mirroring the daemon read side). The latter is preferable
because the daemon currently issues one `send()` per `@RSYNCD:` line
during module listing, and a 4 KiB `BufWriter` would coalesce them
into a single segment alongside the `TCP_NODELAY=true` setting in
Section 1.4.

### F4. Make the async session buffer sizes match the sync path

**Issue:** `async_session/session.rs:131-133` uses Tokio's default 8 KiB
buffers, while the sync transfer path uses 64 KiB. Once the async path
grows to handle full transfers (#async-daemon roadmap), the smaller
buffer will produce twice the `recvfrom` calls per multiplex frame
batch.

**Fix:** explicitly construct
`BufReader::with_capacity(64 * 1024, reader)` and
`BufWriter::with_capacity(64 * 1024, writer)` to match the synchronous
post-handshake sizing and upstream's 32-64 KiB iobuf range. Land this
before the async path starts running real transfers.

### F5. Add `set_nodelay(true)` earlier on the daemon path

**Issue:** `set_nodelay` runs only inside `setup_transfer_streams`,
i.e. after auth completes. The greeting/auth phase therefore pays
Nagle's 40 ms coalescing delay on the first response after each
client write. For interactive `--list-only` clients this is visible.

**Fix:** call `stream.set_nodelay(true)` immediately after
`TcpListener::accept` (or in the async listener equivalent), before
the legacy `BufReader` is constructed. Document the invariant alongside
the existing `socket options = TCP_NODELAY` config knob so operators do
not double-set it.

## 7. Out of scope

- The Windows IOCP and Linux io_uring integration on the disk-commit
  side. Section 4 lists their entry points; #1868 and #2045 cover
  detail.
- The buffer-pool sizing for engine-local copies. See
  `docs/audits/buffer-pool-capacity-sizing.md` (#1637).
- Async transport for SSH. See `docs/audits/async-ssh-transport.md`.

## 8. Verification checklist

- [ ] F2: trace SSH handshake with `strace -e read,write` on macOS
      `dtruss -f -t read_nocancel`; confirm the four-byte read collapses
      into one `read()`.
- [ ] F3/F5: run `tcpdump -i lo -n port 873` against
      `oc-rsync --list-only rsync://localhost/`; confirm the greeting
      and module list ride a single segment.
- [ ] F4: rerun the daemon concurrency benches (#1297) before/after
      the buffer-size bump and record the syscall count delta from
      `perf stat -e 'syscalls:sys_enter_recvfrom'`.
