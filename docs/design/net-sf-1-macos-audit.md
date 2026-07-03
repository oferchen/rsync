# NET-SF.1: macOS `sendfile(2)` API audit + wire-in plan

Status: audit
Tracking: NET-SF.1 (this doc), feeds NET-SF.2 (wrapper polish), NET-SF.3 (sender wire-in), NET-SF.4 (daemon wire-in)
Owners: fast_io, transfer, daemon
Companion: `docs/design/net-sf-macos-audit.md` (low-level wrapper design, already shipped)

## Goal

Capture the Darwin `sendfile(2)` API surface, document caller preconditions, and lay out the sender / daemon wire-in plan so NET-SF.2 can polish the existing wrapper, NET-SF.3 can land the sender call site, and NET-SF.4 can land the daemon call site without re-deriving the audit each time.

## Darwin `sendfile(2)` reference

```c
int sendfile(int fd, int s, off_t offset, off_t *len,
             struct sf_hdtr *hdtr, int flags);
```

Apple man page (`man 2 sendfile`, macOS 14.x) + xnu source (`bsd/kern/uipc_syscalls.c::sendfile`).

### Signature deltas vs Linux

| Concern              | Linux `sendfile`                              | Darwin `sendfile`                                               |
|----------------------|-----------------------------------------------|-----------------------------------------------------------------|
| Argument order       | `(out_fd, in_fd, *offset, count)`             | `(in_fd, out_fd, offset, *len, *hdtr, flags)` - reversed fds    |
| Source position      | NULL offset advances source `lseek` position  | `offset` is mandatory by-value; source position never touches   |
| Length parameter     | `count: size_t` consumed in full or short     | `*len: off_t` in/out - request, then bytes-actually-sent        |
| Return on success    | `ssize_t` bytes transferred                   | `0`; bytes transferred populated in `*len`                      |
| Header / trailer iov | none                                          | `sf_hdtr` describes optional headers + trailers as iovec arrays |
| Partial recovery     | short return, retry from `offset + ret`       | `*len` populated even on `-1`, retry from `offset + *len`       |
| Destination          | any file or socket                            | **must be `SOCK_STREAM` socket** - `ENOTSOCK` otherwise         |
| Source               | regular file or block device                  | regular file; pipes/sockets/char devices fail `ENOTSUP`         |
| `flags` parameter    | reserved (= 0)                                | reserved (= 0)                                                  |

### `sf_hdtr` semantics

```c
struct sf_hdtr {
    struct iovec *headers;   /* prepended before file region */
    int hdr_cnt;
    struct iovec *trailers;  /* appended after file region   */
    int trl_cnt;
};
```

The kernel synthesises a single multi-region send across all three pieces. `*len` counts **only file-region bytes** - header / trailer counts are inferred by subtracting from the iovec totals. `hdtr = NULL` is the simple path and is what the existing wrapper passes. Out of scope for NET-SF.2 and NET-SF.3; revisit if the sender ever batches the trailing `NDX_DONE` envelope inline.

### Partial-write recovery via `*len`

Darwin populates `*len` with bytes actually delivered on **both** success and failure return paths. Three shapes:

1. Full transfer: `ret == 0`, `*len == requested`.
2. EOF before full request: `ret == 0`, `*len < requested`. Caller distinguishes by file-size knowledge; the existing wrapper treats `*len == 0` after a successful return as EOF.
3. Mid-transfer error: `ret == -1`, `errno` set, `*len` reports prefix that did land. The peer sees a consistent byte stream up to `offset + *len`.

This recovery contract is what the existing `sendfile_macos::PartialSend` payload exposes (see `crates/fast_io/src/sendfile_macos.rs:48`).

### errno taxonomy

| errno          | Cause                                          | Caller action                                  |
|----------------|------------------------------------------------|------------------------------------------------|
| `EAGAIN`/`EWOULDBLOCK` | non-blocking socket would block        | surface partial count, retry after readiness   |
| `EINTR`        | signal landed mid-syscall                      | retry internally with new `offset` cursor      |
| `EPIPE`        | peer closed the socket                         | surface as `BrokenPipe`; transfer aborted      |
| `ENOTCONN`     | destination socket not connected               | caller bug; never retry                        |
| `ENOTSOCK`     | destination is not a `SOCK_STREAM` socket      | caller must fall back to buffered loop         |
| `EOPNOTSUPP`   | source not a regular file                      | caller must fall back to buffered loop         |
| `EINVAL`       | `offset < 0`, bad `flags`, `*len` overflows    | validate at wrapper boundary                   |
| `EFAULT`       | bad `hdtr` pointer                             | unreachable when wrapper passes NULL `hdtr`    |
| `EBADF`        | bad fd                                         | unreachable when wrapper uses `BorrowedFd`     |
| `ENOBUFS`      | socket buffer exhaustion                       | treat as transient; retry after small back-off |

### Performance gotchas

- Destination is **always a stream socket**. File→file, file→pipe (`SOCK_DGRAM` or anonymous pipe), and file→TTY all fail. The dispatch path must verify the destination shape or be prepared to soft-fall-back on `ENOTSOCK`.
- Darwin contends a vnode lock for the source for the duration of each call. Long single-call transfers serialise other readers on the same inode. The wrapper already caps each call at ~2 GiB (`SENDFILE_CHUNK_SIZE` in `crates/fast_io/src/sendfile/macos.rs:17`) which keeps that lock window bounded.
- Zero-copy pages stay pinned in the unified buffer cache until the kernel hands them to the network layer. Under sustained back-pressure this delays page reclaim and can elevate `vm_pageout` activity. The mitigation is the same `*len`-based partial-send loop the wrapper already drives.
- No TLS-compose concern. oc-rsync has no in-binary TLS (TLS is external via stunnel, exactly like upstream), so the daemon/SSH socket oc writes to always carries plaintext and `sendfile` composes with it directly - there is no userspace encryption boundary above the socket to corrupt.

## Existing site inventory

`fast_io` already ships **two** macOS `sendfile` paths. Both are real, neither is a stub:

| Path                                                 | File                                              | Caller contract                              |
|------------------------------------------------------|---------------------------------------------------|----------------------------------------------|
| `sendfile::send_file_to_fd` (high-level)             | `crates/fast_io/src/sendfile/mod.rs:177`          | Mirrors Linux: advances source position; chunks internally to ~2 GiB; falls back to buffered loop on any error |
| `sendfile_macos::sendfile_macos` (low-level)         | `crates/fast_io/src/sendfile_macos.rs:96`         | Explicit offset + len; `WouldBlock` carries `PartialSend`; retries `EINTR`; surfaces every other errno verbatim |

Both call sites use `BorrowedFd` (or `&File` + `as_raw_fd`) and pass NULL `hdtr`. The high-level wrapper is what `PlatformSendFile::send_to_socket` in `crates/fast_io/src/platform_sendfile.rs:154` returns for `MacOsSendFile`.

What is **not** in place today:

- No call site in `crates/transfer/src/` for sender file-serving uses either wrapper. The sender currently routes file bytes through the userspace multiplex writer (`crates/transfer/src/writer/`).
- No call site in `crates/daemon/src/` invokes either wrapper. Daemon file serving goes through the same multiplex writer pipeline.
- `PlatformSendFile::is_supported()` returns `true` on macOS unconditionally but the dispatch path is never engaged.

So NET-SF.2 is **not** "implement the wrapper" (already done); it is "polish edges + close gaps the wire-in surfaces". NET-SF.3 and NET-SF.4 do the actual wiring.

## Caller requirements for sender + daemon wire-in

Any call site must satisfy these preconditions before invoking `sendfile`:

1. **Destination is a `SOCK_STREAM` fd.** Daemon and SSH-via-stdio both qualify after the transport handshake. oc writes plaintext to that fd (any TLS fronting is an external stunnel/proxy terminating below oc), so there is no encryption boundary to corrupt. Gate on `ZeroCopyPolicy::Auto/Enabled`.
2. **Source is a regular `File`.** Memory-mapped reads, anonymous pipes, and device files must fall through. The sender already opens the source with `File::open` (`crates/transfer/src/reader/`), so this holds for the literal-block path.
3. **Multiplex writer is flushed.** The wrapper bypasses the user-space buffer entirely. Any pending multiplex frames must hit the wire before `sendfile` runs, otherwise the peer sees out-of-order bytes. This is the same flush discipline upstream rsync uses in `io.c:io_start_buffering_out` callers.
4. **Length is known up front.** The wrapper does not accept "send until EOF". The sender already knows the literal-block length at decode time.
5. **Offset cursor matches file-cursor expectation.** Use the low-level `sendfile_macos` when the sender tracks an explicit offset (it does). Use the high-level `send_file_to_fd` when the call site holds the `&File` and wants the post-call position advanced (the daemon's batched serve path qualifies if a single file fully consumes the call).

These requirements are satisfied by the literal-block emitter in the sender and by the daemon-mode file serve loop. Both paths are stream-socket destinations after the daemon `@RSYNCD:` greeting completes.

## Wire-in plan

### NET-SF.2 (wrapper polish)

The existing wrapper already covers the contract documented above. Remaining work for NET-SF.2:

- Audit `try_sendfile_macos` (high-level) chunk size against the new daemon-pipeline cap. Confirm 2 GiB cap aligns with `fast_io::ZeroCopyPolicy` defaults.
- Add `is_socket(dest_fd)` precheck so non-socket destinations skip the syscall and fall through silently instead of paying an `ENOTSOCK` round-trip per call.
- Surface a typed `MacSendFileError` so the sender wire-in can distinguish "soft fallback to buffered loop" from "hard fail, abort transfer". Today both shapes return `io::Error`.

### NET-SF.3 (sender wire-in)

Target site: literal-block emitter in `crates/transfer/src/writer/` (whichever submodule owns post-token `Literal` payload emission).

Sequence:

1. After the per-block token header is written, **flush** the multiplex writer.
2. Check `ZeroCopyPolicy` + transport shape (must be plain socket). If either gate is off, take the existing buffered path.
3. Call `sendfile_macos::sendfile_macos(in_fd, out_fd, offset, len)`.
4. On `WouldBlock { sent }`: advance the offset cursor by `sent`, register socket-write readiness, resume on next ready edge.
5. On `Unsupported` / `EOPNOTSUPP` / `ENOTSOCK`: fall back to buffered `read` + `write`. One-shot log so we can detect environments where the gate is mis-set.
6. On any other `Err`: surface to the transfer error chain.

The literal-block emitter writes raw bytes; there is no per-block multiplex framing to interleave, so the flush-then-sendfile shape is safe.

### NET-SF.4 (daemon wire-in)

Target site: the daemon's per-file serve loop. The daemon currently dispatches file serving through the same `transfer::sender` path used for SSH, so NET-SF.3 covers the daemon case implicitly. The daemon socket is always plaintext (oc has no in-binary TLS), so no encryption gate is needed.

Confirm there is no separate "daemon-fast-path" file-serve loop that bypasses `transfer::sender`. If one exists (e.g. a future static-file optimisation), it gets its own NET-SF.4-scoped call site mirroring NET-SF.3.

## Note: no in-binary TLS

`sendfile` writes plaintext file bytes directly to the socket. This is always safe in oc-rsync because oc has **no in-binary TLS** - TLS, when used, is terminated by an external stunnel/proxy below oc (exactly as with upstream rsync's `rsync-ssl`), so the fd oc holds always carries plaintext. There is no userspace encryption boundary to corrupt, and thus no TLS gate to add.

## Sequencing decision for NET-SF.2

NET-SF.2 is **not blocking** for NET-SF.3 because the wrapper already meets the documented contract. NET-SF.3 can land first, exercise the wrapper end-to-end, and surface any gaps; NET-SF.2 follows with the polish list above driven by real call-site evidence.

Recommended order: NET-SF.3 (sender wire-in behind `ZeroCopyPolicy`, off by default), NET-SF.5 bench, then NET-SF.2 polish + NET-SF.4 daemon-specific gate. NET-SF.2 stays open as a tracking task for the polish items but does not block downstream tasks.

## Out of scope

1. `sf_hdtr` header / trailer iovecs. Defer until a call site has a concrete need.
2. macOS kTLS. Does not exist; do not promise it.
3. Linux `sendfile` parity audit. Covered separately by `crates/fast_io/src/sendfile/linux.rs`.
4. Per-call env-var threshold override. Until call sites land there is nothing to tune.
