# splice/vmsplice for SSH stdio

Tracking issue: oc-rsync task #1860.

## Summary

This audit evaluates whether `splice(2)` and `vmsplice(2)` (Linux 2.6.17+) can
replace user-space buffer copies on the SSH stdio data path inside oc-rsync.
The conclusion is that splice is applicable on the file-to-pipe (sender) and
pipe-to-file (receiver) edges of the data path, but not across the SSH child
itself. The multiplex framing layer adds a constraint that requires `vmsplice`
or pre-staged pipe writes for the 4-byte MSG header before each payload chunk.
The work is feasible, additive, and stays inside oc-rsync (no wire-protocol
change). Upstream rsync 3.4.1 does not use splice; this is a pure oc-rsync
optimisation.

## Upstream evidence

A recursive search for `splice`, `vmsplice`, `tee(`, `SPLICE`, and `sys_splice`
under `target/interop/upstream-src/rsync-3.4.1/` returns one match: the shell
helper `checktee()` inside `testsuite/rsync.fns` (an unrelated test scaffold).
Upstream therefore performs all SSH stdio I/O via plain `read(2)`/`write(2)`
on the pipe file descriptors. No protocol-level expectation of splice exists.

## Where splice can apply

oc-rsync drives an SSH client subprocess and exchanges rsync wire bytes through
its stdio pipes. The local end owns the parent side of the pipes
(`std::process::ChildStdin`, `ChildStdout`). The SSH child reads its stdin pipe,
encrypts, and writes the cipher text to its own TCP socket; the reverse path
mirrors that. We do not own the SSH socket, so any zero-copy work has to stay
on the parent side of the pipe.

```
                       parent (oc-rsync)                       SSH child
sender:
  src file (page cache) -- splice --> ChildStdin pipe ----> [encrypt] -> TCP
                          ^                  ^
                          |                  |
                          +--- file -> pipe  +--- pipe (we own)

receiver:
  TCP -> [decrypt] -> ChildStdout pipe -- splice --> dst file (page cache)
                            ^                  ^
                            |                  |
                            +--- pipe (we own) +--- pipe -> file
```

Useful invariants:

- `splice(2)` requires that one of the two file descriptors be a pipe.
- The parent's view of the SSH child stdio is a pipe at both ends, so file <->
  pipe transfers in either direction are eligible.
- `vmsplice(2)` moves user pages into a pipe without copying. With
  `SPLICE_F_GIFT`, ownership of the pages passes to the kernel; the caller may
  not reuse those pages until the consumer drains them.
- `tee(2)` duplicates between two pipes without consuming bytes; useful when
  oc-rsync wants to peek at MSG headers while still forwarding the payload.

What we cannot splice:

- The encrypted bytes leaving the SSH child socket. We do not own that fd, and
  it is a TCP socket, so even if we did, it could not pair with another socket
  via splice.
- Any data that has to be inspected, transformed, compressed, or checksummed
  in user space before transmission. Delta-token, compression, and checksum
  paths are out of scope for this audit.

## Multiplex constraint

oc-rsync uses the rsync multiplex envelope (see `crates/protocol/src/envelope`)
on every byte that crosses SSH stdio:

- 4-byte header (`HEADER_LEN`) carrying a tag byte (`MPLEX_BASE + code`) and a
  24-bit length (`MAX_PAYLOAD_LENGTH = 0x00FF_FFFF` ~= 16 MiB - 1).
- Up to `MAX_PAYLOAD_LENGTH` bytes of payload.
- For raw file data, code is `MessageCode::Data` (`MSG_DATA = 0`).

A naive `splice(file, pipe, payload_len)` would push raw file bytes into the
pipe with no envelope, and the remote end would mis-frame the stream. The
options for keeping framing correct without a user-space copy of the payload
are:

1. Write the 4-byte header with `write(2)` to the pipe, then `splice(file ->
   pipe, payload_len)` for the payload. The header write is small enough that
   the lack of zero-copy is not material; the saving is on the payload, which
   dominates throughput.
2. Stage the header on a small user-space buffer and `vmsplice(buf -> pipe, 4)`
   with `SPLICE_F_GIFT`, then splice the payload. The buffer must not be
   reused or freed until the SSH child has drained it; in practice we rotate
   through a small pool of header pages.
3. Pre-build a reusable header pipe that holds many pre-encoded headers via
   `tee` plus `splice`, dripping headers into the data pipe between payload
   chunks. This is the most complex variant and is only worth it if the
   header-write syscall cost is measurable.

Phase 1 of the plan picks option (1) because it is simple, correct, and lets
us measure the payload-side savings independently.

## Phased plan

### Phase 1: file -> ChildStdin (sender), MSG_DATA payload only

Deliverables:

- A `fast_io::splice_pipe` module that, given an open source `File` and a
  `ChildStdin`, writes a 4-byte MSG_DATA header via `write_all` and then
  drives `splice(file_fd, pipe_fd, len, SPLICE_F_MOVE | SPLICE_F_MORE)` in a
  loop until the requested chunk is delivered or `EAGAIN` is returned.
- Integration point: the sender's whole-file send path, gated behind a
  `--splice` capability flag and runtime kernel-version check (Linux 2.6.17+).
- Fallback: on `EINVAL`, `ENOSYS`, or any non-`EAGAIN` error from `splice`,
  fall back to the existing buffered `read` + `write_all` loop.

Acceptance criteria:

- `crates/fast_io/benches/splice_pipe.rs` reports a measurable reduction in
  syscalls per MiB versus the read/write baseline on Linux for a 1 GiB
  payload. Baseline: `read` + `write_all` loop with 64 KiB buffer.
- No correctness regressions in the SSH integration test suite.
- macOS and Windows compile cleanly with stub paths returning `Unsupported`.

### Phase 2: ChildStdout -> dst file (receiver)

Deliverables:

- Mirror of phase 1: read the multiplex header into a small buffer, then
  `splice(pipe_fd, file_fd, payload_len)` to land the payload directly in the
  destination file's page cache.
- Integrate with the receiver's whole-file write path. Skip when `--inplace`
  combined with delta tokens is in effect, since those payloads are not raw
  file bytes.

Acceptance criteria:

- Same syscall-savings target as phase 1 on the receive side.
- `truncate(2)` / `fsync(2)` semantics preserved.
- Sparse-file detection still works when phase 2 is disabled (it must remain
  the responsibility of the existing zero-run detector).

### Phase 3: vmsplice the multiplex header

Deliverables:

- A small ring of pre-encoded 4-byte header pages, fed into the pipe via
  `vmsplice(SPLICE_F_GIFT)` so the header write joins the splice path.
- Per-direction header allocator with lifetime tracking: a header page is
  reusable only after the SSH child has drained it.

Acceptance criteria:

- Payload + header path is fully zero-copy on the parent side.
- Bench shows a further reduction in `sendto`/`writev` syscalls per second
  versus phase 1 + 2.

### Out of scope

- Socket-to-pipe paths (handled by the SSH child, not by oc-rsync).
- Compression and checksum/delta-token paths (the bytes are computed in user
  space and cannot be splice-fed without a copy).
- Cross-platform support: macOS and Windows continue to use `read`/`write`.

## Risks

- **vmsplice page lifetime.** With `SPLICE_F_GIFT`, the kernel takes ownership
  of the page. Reusing or freeing the page before the consumer (the SSH
  child) reads it produces silent corruption. Mitigation: rotate header pages
  and only recycle once the pipe has confirmed drain via successful subsequent
  `splice` returns. The header ring should have at least
  `pipe_capacity / HEADER_LEN` slots.
- **`SPLICE_F_NONBLOCK` semantics.** When the pipe is full, splice returns
  `EAGAIN`. Mixing blocking and non-blocking flags with the existing pipe
  state can mis-drive the loop. Mitigation: keep the pipe in blocking mode
  and handle `EAGAIN` with a poll/retry loop only when the pipe was
  explicitly set non-blocking.
- **Short splice returns.** `splice` may transfer fewer bytes than requested.
  The driver must loop until either `len == 0` or a real error is returned.
- **EPIPE on closed peer.** If the SSH child has exited, `splice` to its
  stdin returns `EPIPE`. Mitigation: map to the existing broken-pipe path so
  that the parent surfaces SSH stderr and the appropriate exit code.
- **Pipe capacity.** Default Linux pipe buffer is 64 KiB. Larger transfers
  require `fcntl(F_SETPIPE_SZ, ...)` to lift the buffer up to
  `/proc/sys/fs/pipe-max-size`. The wrapper should attempt `F_SETPIPE_SZ` to
  1 MiB and gracefully accept failure.
- **`splice` between two regular files is not supported.** All call sites
  must verify the destination/source kind before invoking the syscall.
- **Interaction with `--bwlimit`.** Bandwidth limiting is enforced in user
  space today via the bandwidth crate. Splice bypasses that path; phase 1
  must keep `--bwlimit` on the read/write fallback or move pacing into the
  splice loop via bounded chunk sizes.
- **Interaction with `--checksum-seed` / debug logging.** Anything that
  inspects the bytes after they leave the file must remain on the read/write
  fallback.

## Follow-up tasks

- [ ] #1861 implement `fast_io::splice_pipe` driver (phase 1).
- [ ] #1862 wire splice driver into the sender whole-file send path with a
  capability gate and `--bwlimit` interaction guard.
- [ ] #1863 mirror driver for the receiver path (phase 2).
- [ ] #1864 vmsplice header ring (phase 3) with lifetime tracking.
- [ ] #1865 add `bench(fast_io)` Linux-only criterion suite under
  `splice_pipe` to track per-phase syscall and CPU savings; today the suite
  contains a baseline plus an ignored placeholder for the splice path.
- [ ] #1866 audit interaction with io_uring `IOSQE_IO_LINK` chains; splice
  may compete with the existing buffer-ring registered-buffer path on
  Linux 5.6+ and we should document a chooser.
- [ ] #1867 cross-version interop test: confirm that an oc-rsync sender with
  splice enabled is byte-identical on the wire to a sender without it.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (no splice usage).
- Existing socket-to-disk splice driver:
  `crates/fast_io/src/splice.rs`.
- Multiplex envelope constants:
  `crates/protocol/src/envelope/constants.rs` (`HEADER_LEN`,
  `MAX_PAYLOAD_LENGTH`, `MPLEX_BASE`).
- SSH connection ownership of stdio pipes:
  `crates/rsync_io/src/ssh/connection.rs` (`SshReader`, `SshWriter`).
- Linux man pages: `splice(2)`, `vmsplice(2)`, `tee(2)`,
  `fcntl(2) F_SETPIPE_SZ`.
