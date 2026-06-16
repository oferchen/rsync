# NET-SF: macOS `sendfile(2)` audit and wrapper plan

Status: audit complete, low-level wrapper landed under `fast_io::sendfile_macos`
Tracking: NET-SF.1 (audit), NET-SF.2 (implementation)
Owners: fast_io

## Goal

Provide a low-level macOS `sendfile(2)` wrapper inside the `fast_io` crate that
exposes the BSD layered semantics directly: explicit `offset`, explicit `len`,
partial-progress return, EAGAIN/EINTR surfacing. The wrapper is the building
block the `transfer` / `engine` crates will compose into a higher-level
file-to-socket send loop later (NET-SF.3). It complements the existing
`fast_io::sendfile::try_sendfile_macos` helper, which embeds a different
contract (it advances the source file position to mirror Linux behaviour).

## API contrast with the existing helper

| Concern                           | `sendfile::try_sendfile_macos` (existing) | `sendfile_macos::sendfile_macos` (new) |
|-----------------------------------|--------------------------------------------|----------------------------------------|
| Source position                   | Reads `lseek(SEEK_CUR)`, advances on success | Caller-supplied `offset`, untouched    |
| Length parameter                  | `u64` count, internally chunked            | Caller-supplied `usize`                |
| Partial-send signalling           | Returns `Ok(n)`                            | Returns `Ok(n)` for success; `WouldBlock` with bytes-sent for EAGAIN |
| EINTR handling                    | Treats as partial-progress error            | Retries internally                     |
| Header / trailer scatter-gather   | Never used                                  | Out of scope here; documented for follow-up |
| Caller responsibility for offset  | Hidden                                     | Explicit and load-bearing              |

The two helpers do not collide; the existing high-level wrapper continues to
drive the `send_file_to_fd` dispatch path. The new low-level wrapper is
intended for protocol-side callers that already track an explicit byte cursor
(e.g. the sender's literal-block emitter) and need to surface EAGAIN with the
exact byte count delivered so far.

## Reference: upstream rsync

Upstream rsync's I/O layer (`io.c`) does its own buffered `write()` loop over
the multiplexed wire and does not invoke `sendfile(2)` directly. The wrapper
here exists so the Rust port can opt into kernel-side zero-copy on macOS when
the protocol shape permits (literal-only blocks transmitted with no
multiplexing layered on top). The upstream code remains the source of truth
for byte ordering, framing, and end-of-transfer semantics; this wrapper only
substitutes a faster transport for the literal bytes themselves.

## Darwin `sendfile(2)` reference (xnu / man page)

```c
int sendfile(int fd, int s, off_t offset, off_t *len,
             struct sf_hdtr *hdtr, int flags);
```

- `fd`: source. **Must be a regular file**; pipes, sockets, character/block
  devices fail with `ENOTSUP`. Empirically, anonymous mmap-backed file
  descriptors are accepted.
- `s`: destination. **Must be a connected stream socket** (`SOCK_STREAM`,
  typically AF_INET / AF_INET6 / AF_UNIX). Anything else surfaces `ENOTSOCK`
  or `EINVAL`.
- `offset`: byte offset into `fd` to start from. Negative offsets are
  rejected with `EINVAL`. `sendfile` does **not** touch the source's
  `lseek` position - the caller manages it explicitly.
- `len`: in/out pointer.
  - On entry: maximum bytes to transfer. The man page documents `0` as
    "send until EOF". We always pass a non-zero ceiling so partial-send
    accounting stays simple.
  - On return: bytes actually sent, regardless of return value or errno.
    On error this includes any prefix that landed before the error fired.
- `hdtr`: optional `struct sf_hdtr` describing header / trailer iovecs to
  prepend / append to the data stream. NULL means "no extras". The
  byte count returned in `*len` counts only the file-region bytes; header
  and trailer bytes are tracked inside `hdtr->headers` and
  `hdtr->trailers` but never via `*len`. Out of scope for the first
  wrapper; we document the gap below.
- `flags`: reserved, must be 0.
- Return: `0` on success, `-1` on failure with `errno` set. **Both** code
  paths populate `*len` with the bytes actually transferred.

### Partial-progress contract

Darwin populates `*len` with the bytes actually delivered to the socket
**before** signalling success or failure. Three observable shapes:

1. Full transfer: `ret == 0`, `*len == requested`.
2. EOF before requested length: `ret == 0`, `*len < requested`. Caller
   distinguishes EOF from "more available" by inspecting the source file
   size (or by treating `*len == 0` as EOF as we do in the existing
   high-level helper).
3. Error mid-transfer: `ret == -1`, `errno` set, `*len` reports the
   prefix that did land. `EAGAIN`, `EWOULDBLOCK`, `EINTR`, and
   `EINPROGRESS` all share this shape. The peer sees a consistent
   stream up to `offset + *len`.

### Error catalogue (relevant errnos)

| errno         | Meaning                                                          | Wrapper handling                       |
|---------------|------------------------------------------------------------------|----------------------------------------|
| `EAGAIN`      | Socket is non-blocking and would block.                          | Return `WouldBlock` carrying bytes-sent prefix |
| `EWOULDBLOCK` | Same as `EAGAIN` on Darwin.                                      | Same as `EAGAIN`                       |
| `EINTR`       | Caught a signal mid-transfer.                                    | Retry internally with `offset + sent`, accumulate |
| `EINPROGRESS` | Socket is mid-connect.                                           | Surface as-is (caller bug)             |
| `EPIPE`       | Peer closed the socket.                                          | Surface as `BrokenPipe`                |
| `ENOTSOCK`    | Destination is not a socket.                                     | Surface; caller should fall back       |
| `ENOTCONN`    | Destination socket is not connected.                             | Surface; caller bug                    |
| `EFAULT`      | `hdtr` pointer was invalid.                                      | Cannot happen here - we pass NULL      |
| `EINVAL`      | `offset < 0`, bad `flags`, length overflows `off_t`.             | Validate at the wrapper boundary       |
| `EOPNOTSUPP`  | Source not a regular file.                                       | Surface; caller bug or stale fd        |
| `EBADF`       | Bad fd.                                                          | Cannot happen with `BorrowedFd`        |

### Length and offset overflow

`off_t` is `i64` on Darwin. The wrapper rejects requested lengths that
would push `offset + len` beyond `i64::MAX` with `InvalidInput`. The
`usize` length parameter is cast to `off_t` after the overflow check;
on 32-bit Darwin (which Apple has removed support for) the conversion
is widening and cannot wrap.

### Header / trailer scatter-gather (out of scope, documented)

`struct sf_hdtr` lets the caller batch one or more `iovec` headers
before the file bytes and one or more trailers after. The kernel
synthesises a single multi-region send across all three. The Rust
protocol path emits its framing through a separate write call, so the
first wrapper accepts no headers / trailers. A follow-up
(NET-SF.4) can add a typed `Headers<'_>` / `Trailers<'_>` API that
borrows from a caller-owned `[IoSlice<'_>]` slice and surfaces the
header/trailer byte counts separately from the file-region count.

### Source-position behaviour

`sendfile(2)` on Darwin does not move the source's `lseek` position.
The caller passes `offset` explicitly and tracks the cursor itself.
This is the opposite of the convenience contract the existing
high-level helper provides (which mimics Linux's "advance position on
success"). The two helpers therefore coexist without one calling the
other.

## Wrapper design summary

- File: `crates/fast_io/src/sendfile_macos.rs`.
- Public symbol re-exported from `fast_io::sendfile_macos`.
- Signature:

  ```rust
  pub fn sendfile_macos(
      in_fd: BorrowedFd<'_>,
      out_fd: BorrowedFd<'_>,
      offset: i64,
      len: usize,
  ) -> io::Result<usize>
  ```

- Behaviour:
  - `offset < 0` -> `InvalidInput`.
  - `len == 0` -> `Ok(0)` without entering the syscall.
  - `offset + len` overflow -> `InvalidInput`.
  - Internal `EINTR` retry loop that advances by the bytes the kernel
    reports each iteration. Caps internal retries at a finite count
    so a signal storm cannot wedge a calling thread; the cap is large
    enough (1024) that real-world signal pressure stays transparent.
  - `EAGAIN` / `EWOULDBLOCK`: return `io::Error::new(WouldBlock, ...)`
    whose payload carries the partial byte count via a small inner
    `PartialSend` struct.
  - On `Ok(n)`, `n` is the file-region bytes the kernel says it
    delivered (may equal `len`, may be less on short reads or EOF).
- Non-macOS targets compile a stub that always returns
  `io::Error::from(io::ErrorKind::Unsupported)`. Callers can therefore
  invoke the function unconditionally and decide at the call site
  whether `Unsupported` is a soft fallback or a bug.

### Partial-send surfacing

`io::Error` does not natively carry typed payloads. We wrap the count
in a small `PartialSend` struct that implements `std::error::Error`
and stash it via `io::Error::new(WouldBlock, PartialSend { sent })`.
Callers extract it via `err.get_ref().and_then(|e| e.downcast_ref::<PartialSend>())`.

This is more ergonomic than threading a `(io::Error, usize)` tuple
through the call graph and matches the pattern that
`std::io::Error::new` was designed for. The `PartialSend` type is
re-exported from `fast_io::sendfile_macos::PartialSend`.

### Test plan

- Unit tests stay next to the wrapper in `sendfile_macos.rs`:
  - `roundtrip_small`: a tempfile + `AF_UNIX` `socketpair` exchange,
    asserting byte-equal payload.
  - `respects_offset`: requesting `offset=10, len=10` against a known
    fixture sends exactly the middle 10 bytes.
  - `len_zero_is_noop`: returns `Ok(0)` without touching the syscall.
  - `negative_offset_rejected`: `InvalidInput`.
  - `eof_short_send`: requesting more bytes than the file holds returns
    a short count and does not error.
  - `would_block_carries_partial_count`: non-blocking socket with a
    small send buffer drives the EAGAIN path and asserts the
    `PartialSend` payload is present and within bounds.
- Non-macOS hosts compile a tiny test that asserts the stub returns
  `Unsupported` and never panics.

### Callers (forward-looking, not in this PR)

- `transfer::sender::literal_block_writer` will gain a macOS-only
  branch that drives `sendfile_macos` for literal blocks above a
  threshold matching the existing `SENDFILE_THRESHOLD`.
- `engine::local_copy` does not need this wrapper - it already uses
  `clonefile` / `fcopyfile`.

### Out of scope (tracked for NET-SF.3+)

1. Header / trailer iovecs via `sf_hdtr`. Adds a typed `Headers`
   / `Trailers` API.
2. Integration into the sender literal-block path. Belongs in the
   `transfer` crate behind a `ZeroCopyPolicy` gate so the existing
   buffered loop stays the default until perf data justifies the
   switch.
3. Per-call `OC_RSYNC_SENDFILE_MACOS_THRESHOLD` env override. Until
   call sites land there is nothing to tune.
