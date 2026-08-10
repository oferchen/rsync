## io_uring on pipe FDs for SSH stdio

Tracking issue: oc-rsync task #1859. Sibling audit:
[`docs/audits/splice-ssh-stdio.md`](splice-ssh-stdio.md) (PR #3428).

## Summary

This audit asks whether `IORING_OP_READ` / `IORING_OP_WRITE` on the parent's
view of an SSH child's stdio pipes is a viable acceleration path for oc-rsync
SSH transport. The conclusion is that submitting `READ`/`WRITE` SQEs against a
pipe fd is supported, requires no special `IOSQE_*` or `RWF_*` flag, and is
the natural extension of the `IoUringSocketReader` / `IoUringSocketWriter`
infrastructure already built for daemon mode. It does not eliminate the
user-space copy that splice/vmsplice would; the win is purely on the syscall
side (batched submissions, optional `IORING_SETUP_SQPOLL`, optional
`IORING_REGISTER_FILES`). Recommendation is phase-1 integration as an
optional fast path, with splice-based zero-copy left as the longer-term
follow-up tracked by task #1860.

Upstream evidence: a recursive grep for `io_uring`, `IORING_`, `liburing` in
`target/interop/upstream-src/rsync-3.4.1/` returns no matches. Upstream rsync
performs all SSH stdio I/O via plain `read(2)` / `write(2)` (`io.c`), so any
io_uring usage on this path is a pure oc-rsync optimisation with no wire-
protocol implication.

## Current SSH stdio code path

The parent's view of the SSH child stdio is owned by `SshConnection`:

- `crates/rsync_io/src/ssh/connection.rs:30` defines `SshConnection`, holding
  `Option<ChildStdin>` and `Option<ChildStdout>`.
- `crates/rsync_io/src/ssh/connection.rs:178` `SshConnection::split` returns
  the read/write halves: `SshReader { stdout }` and `SshWriter { stdin }`.
- `crates/rsync_io/src/ssh/connection.rs:217-221` `impl Read for SshReader`
  delegates to `ChildStdout::read`, which is `read(2)` on the pipe fd.
- `crates/rsync_io/src/ssh/connection.rs:229-237` `impl Write for SshWriter`
  delegates to `ChildStdin::write` / `flush`, which is `write(2)` on the
  pipe fd.

These halves are consumed by the SSH transfer driver:

- `crates/core/src/client/remote/ssh_transfer.rs:553` calls
  `connection.split()` to obtain `(reader, writer, child_handle)`.
- `crates/core/src/client/remote/ssh_transfer.rs:562` runs the rsync
  handshake against `&mut reader` and `&mut writer`.
- `crates/core/src/client/remote/ssh_transfer.rs:588` runs the bulk transfer
  loop through the same `&mut reader` / `&mut writer` references.

There is no buffered wrapper between the rsync engine and the pipe fd today.
The bytes traverse `ChildStdin` / `ChildStdout` directly via the kernel
pipe via `read`/`write`. The multiplex envelope layer
(`crates/protocol/src/envelope/mod.rs`, `HEADER_LEN`, `MAX_PAYLOAD_LENGTH`,
`MPLEX_BASE`) operates above this fd boundary and would be unaffected by an
io_uring substitution that preserves byte-stream semantics.

## io_uring on pipe FDs: feasibility, kernel version, semantics

`IORING_OP_READ` and `IORING_OP_WRITE` are described by the `io_uring_enter(2)`
man page as wrappers around the kernel `read(2)` / `write(2)` paths. They do
not require the underlying file descriptor to be a regular file or a socket.
The fd type is dispatched by the kernel through the same VFS hooks that the
synchronous syscalls use. Consequently:

- A pipe fd accepts `IORING_OP_READ` and `IORING_OP_WRITE` SQEs in the same
  way it accepts `read(2)` / `write(2)`. No special `IOSQE_*` flag is
  required. `IOSQE_FIXED_FILE` is optional and works once the pipe fd has
  been registered via `IORING_REGISTER_FILES`.
- No `RWF_*` flag is needed. `RWF_NOWAIT` is irrelevant because pipe reads
  do not pre-populate page cache. `RWF_HIPRI` is irrelevant on pipes.
- Short reads are normal. A pipe read returns whatever the writer has left
  in the buffer up to the requested length, just like `read(2)`. The driver
  must loop until the requested length is satisfied or `0` (EOF) is
  returned.
- Blocking semantics depend on the pipe's `O_NONBLOCK` state. By default,
  the pipes returned by `Command::stdout()` / `Command::stdin()` are
  blocking. An `IORING_OP_READ` against an empty blocking pipe will park
  the io_uring worker thread inside the kernel until the writer produces
  bytes, exactly as `read(2)` would. The submission queue can still hold
  more SQEs; only the worker servicing the blocked op is parked.
- `IORING_FEAT_FAST_POLL` (Linux 5.7+) lets the kernel internally arm a
  poll on the pipe before scheduling the read, so the worker thread does
  not have to block. This is the path the kernel takes for pipes,
  sockets, and other pollable fds. With this feature, pipe I/O through
  io_uring approximates an epoll-driven loop without explicit
  registration.
- The minimum kernel for `IORING_OP_READ` and `IORING_OP_WRITE` themselves
  is Linux 5.6 (same as the rest of `crates/fast_io/src/io_uring`). This
  matches the gate already documented at
  `crates/fast_io/src/io_uring/mod.rs:7-13`. Whether kernels older than
  5.7 also handle pipe fds well needs verification against the
  `IORING_FEAT_FAST_POLL` matrix on kernel.org; if not, fall back to
  `read`/`write` on probe failure.

Gotchas:

- **No zero-copy.** `IORING_OP_READ` still copies bytes out of the pipe
  buffer into the user-space buffer. To bypass that copy, splice/vmsplice
  is required (see comparison below).
- **`SIGPIPE` on closed peer.** A pipe write to a closed peer raises
  `SIGPIPE` by default. The cqe result for an `IORING_OP_WRITE` will
  carry `-EPIPE`. The driver must map `EPIPE` to the existing broken-pipe
  path so SSH stderr is surfaced and the appropriate exit code is
  returned, mirroring `crates/core/src/client/remote/ssh_transfer.rs:603`
  for the synchronous path.
- **Submission queue head-of-line blocking on a single fd.** Because each
  SQE on a pipe completes only when that pipe has data, queueing many
  reads on one pipe does not produce parallelism; it just queues work.
  Real benefit comes from pairing the read pipe and the write pipe in
  the same ring so that `submit_and_wait(2)` can drive both directions
  in one syscall.
- **`F_SETPIPE_SZ`.** The kernel default 64 KiB pipe buffer caps the
  per-syscall throughput regardless of the I/O backend. io_uring does
  not change this. Lifting the buffer requires the same `fcntl` call
  documented in the splice audit.
- **Mixed sync/async I/O on the same fd.** Once a pipe fd is registered
  with the ring via `IORING_REGISTER_FILES`, the synchronous `read`/
  `write` wrappers in `Read for SshReader` and `Write for SshWriter`
  must not be used concurrently. Callers either fully migrate to the
  io_uring driver or keep the registration unset.
- **Fast-poll caveat.** If the running kernel does not advertise
  `IORING_FEAT_FAST_POLL`, blocking reads serialise io_uring worker
  threads. Probe with `io_uring_setup` `features` field; treat absence
  as a reason to disable the pipe fast path. Cite for verification:
  `man 7 io_uring`, kernel commit history for `IORING_FEAT_FAST_POLL`.

## Comparison vs splice() / vmsplice()

The sibling audit `docs/audits/splice-ssh-stdio.md` (PR #3428) covers the
splice path. A side-by-side comparison:

| Concern                       | io_uring READ/WRITE on pipes      | splice / vmsplice                                      |
| ----------------------------- | --------------------------------- | ------------------------------------------------------ |
| Minimum kernel                | 5.6 (probe + verify on pipes)     | 2.6.17 for `splice`, 2.6.17 for `vmsplice`             |
| Userspace copies              | One copy (kernel <-> buffer)      | Zero copies on the payload                             |
| Header (multiplex 4-byte)     | Same syscall path as payload      | Needs separate `write` or `vmsplice(SPLICE_F_GIFT)`    |
| Syscall amortisation          | Yes (batched SQEs, SQPOLL option) | One syscall per chunk; no batching primitive           |
| Backpressure                  | cqe result, no signal             | `EAGAIN` requires manual poll loop                     |
| `--bwlimit` integration       | Trivial; bytes still pass user    | Requires pacing inside the splice loop                 |
| Compression / checksum hooks  | Trivial                           | Bytes never enter user space; cannot be inspected      |
| Fallback complexity           | Already factored into fast_io     | New wrapper module                                     |
| Cross-platform behaviour      | Stub on non-Linux (existing)      | Stub on non-Linux (existing)                           |
| Non-`MSG_DATA` payloads       | Works unchanged                   | Excluded; only raw file bytes are spliceable           |
| Pipe size requirement         | Same as today                     | `F_SETPIPE_SZ` strongly recommended                    |

The decisive trade-off: splice/vmsplice deletes the user-space copy entirely
but only on the narrow `MSG_DATA` payload-only sub-path and at the cost of
per-chunk header trickery. io_uring on pipe fds keeps the user-space copy
but covers the full SSH stdio bandwidth (handshake, file list, deltas,
compressed payloads, stats) without case analysis, and slots into the
existing fast_io factories. The two paths are complementary, not
mutually exclusive: an io_uring pipe driver can host the multiplex
header writes while a splice payload driver feeds the raw file bytes.

## Integration sketch

The smallest viable shape reuses the daemon-side socket factories almost
verbatim, only swapping the underlying syscall semantics from
`opcode::Recv`/`opcode::Send` to `opcode::Read`/`opcode::Write` (sockets
require `Recv`/`Send` semantics, but pipes only support `Read`/`Write`).

Files to extend (no new file required for phase 1):

1. `crates/fast_io/src/io_uring/mod.rs:81-91` - register a new
   `pipe_reader.rs` / `pipe_writer.rs` module pair next to
   `socket_reader.rs` / `socket_writer.rs`. The `IORING_OP_READ` /
   `IORING_OP_WRITE` opcodes already power
   `crates/fast_io/src/io_uring/file_reader.rs:111` and
   `crates/fast_io/src/io_uring/file_writer.rs:177`, so the SQE
   construction is a copy with the file offset removed (pipes have no
   position).
2. `crates/fast_io/src/io_uring/socket_factory.rs:61-147` - mirror the
   `socket_reader_from_fd` / `socket_writer_from_fd` pattern as
   `pipe_reader_from_fd` / `pipe_writer_from_fd`, returning the same
   `IoUringOrStdSocketReader` / `IoUringOrStdSocketWriter` enums (or a
   sibling `IoUringOrStdPipeReader`).
3. `crates/rsync_io/src/ssh/connection.rs:213-237` - split `SshReader`
   and `SshWriter` into `enum`s with two variants: `Std(ChildStdout)` /
   `Std(ChildStdin)` (today's behaviour, the only Windows path) and
   `IoUring(IoUringPipeReader)` / `IoUring(IoUringPipeWriter)` (Linux
   only, gated by `is_io_uring_available` and
   `IoUringPolicy::{Auto,Enabled}`).
4. `crates/core/src/client/remote/ssh_transfer.rs:553` - thread the
   `IoUringPolicy` through `SshConnection::split` so the worker handle
   continues to wait correctly. Existing `cancel_connect_watchdog`,
   `wait_with_stderr`, and `map_child_exit_status` plumbing are
   unaffected because they do not inspect the read/write side.

Trait surface to extend:

- `fast_io::traits` already defines factories for files and sockets. Add
  a `PipeReaderFactory` / `PipeWriterFactory` pair that takes a `RawFd`
  and an `IoUringPolicy`, returning an enum that implements `Read` /
  `Write`. The trait method must take an unowned fd (mirrors
  `IoUringSocketReader::from_raw_fd`) because `ChildStdout` /
  `ChildStdin` retain ownership of the fd in the parent process.
- The existing dependency-inversion design (`Read` / `Write` traits)
  inside the rsync engine means no engine-side change is required: the
  wrapper exposes `Read` and `Write` exactly as the standard library
  does today.

Cross-platform: stub on non-Linux is automatic if the new modules sit
inside `crates/fast_io/src/io_uring/` and are re-exported only when
`cfg(target_os = "linux")` and `feature = "io_uring"` are both set. The
non-Linux stub mirrors `io_uring_stub.rs` and forwards to
`std::process::ChildStdout` / `ChildStdin`.

## Blockers and open questions

- **Verify pipe support in `io-uring` crate.** The opcode bindings used at
  `crates/fast_io/src/io_uring/file_reader.rs:111` (`opcode::Read::new`)
  do not constrain the fd type. Need an integration test against an
  `os_pipe` pair to confirm the `io-uring` crate's `Read` builder
  accepts a pipe fd without compile-time `Fd` vs `Fixed` mismatches.
- **`IORING_FEAT_FAST_POLL` minimum kernel version.** The `man 7
  io_uring` page documents the feature but the exact minimum kernel
  needs verification against `kernel.org`'s changelog. If the targeted
  fleet is < 5.7, blocking reads on a pipe will serialise the io_uring
  worker thread and undo the win. Probe via the `features` field
  returned by `io_uring_setup`. Needs verification.
- **Buffer pool integration.** `crates/fast_io/src/io_uring/buffer_ring.rs`
  pairs naturally with high-throughput pipe reads, but the buffer ring
  feature requires Linux 5.19+ (`IORING_REGISTER_PBUF_RING`). Not a
  phase-1 blocker; phase 2 enhancement. Cite for verification:
  `liburing` git log, `man 3 io_uring_register_buf_ring`.
- **Deadlock risk under bidirectional traffic.** rsync runs sender +
  receiver loops over the same SSH connection. If both halves are on
  one ring, a blocking `READ` SQE plus a buffered `WRITE` SQE can pin
  workers if the kernel does not surface fast-poll for both fds. Phase
  1 mitigation: use one ring per direction, exactly as
  `IoUringSocketReader` and `IoUringSocketWriter` do today.
- **`--bwlimit` interaction.** Bandwidth limiting is enforced in user
  space via the `bandwidth` crate. io_uring on pipes preserves user-
  space byte handoff so `--bwlimit` keeps working unchanged. (This is
  the opposite of the splice case.)
- **Windows / macOS.** No path. Pipes on Windows have no io_uring
  equivalent, and macOS lacks io_uring entirely. Stub returns the
  current `ChildStdout`/`ChildStdin` `Read`/`Write` as today.
- **Embedded SSH.** `crates/rsync_io/src/ssh/embedded/` does not use
  pipes; it uses an in-process SSH client. The pipe driver does not
  apply there. Out of scope.

## Recommendation

- **Phase 1 (in scope):** introduce `IoUringPipeReader` /
  `IoUringPipeWriter` mirroring the socket reader/writer, gated behind
  `IoUringPolicy::Auto` / `Enabled` and the existing
  `is_io_uring_available` probe. Wire them into `SshReader` /
  `SshWriter` as an optional variant. Acceptance criterion:
  measurable reduction in `read`/`write` syscalls for a 1 GiB
  rsync-over-SSH transfer, no correctness regressions in the SSH
  integration test suite, identical wire bytes vs the `Std` path.
- **Phase 2 (in scope, after phase 1):** opt into
  `IORING_SETUP_SQPOLL` and `IORING_REGISTER_FILES` for the pipe
  reader/writer, matching the existing socket factory configuration.
  Phase 2 is purely a configuration toggle if phase 1 lands cleanly.
- **Out of scope here:** zero-copy splice/vmsplice work tracked by
  task #1860 and `docs/audits/splice-ssh-stdio.md`. The two efforts
  are complementary; this audit does not subsume that work and does
  not displace it. The splice payload driver and the io_uring pipe
  driver can coexist by routing `MSG_DATA` payloads through splice
  while every other byte (headers, file list, deltas, stats,
  compressed data) traverses the io_uring pipe driver.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (no `io_uring` references; SSH stdio uses plain `read`/`write` in
  `io.c`).
- Existing io_uring infrastructure:
  `crates/fast_io/src/io_uring/mod.rs`,
  `crates/fast_io/src/io_uring/socket_reader.rs:46-78`,
  `crates/fast_io/src/io_uring/socket_writer.rs`,
  `crates/fast_io/src/io_uring/file_reader.rs:99-130`,
  `crates/fast_io/src/io_uring/file_writer.rs:170-204`.
- Kernel-version probe and policy:
  `crates/fast_io/src/io_uring/config.rs`,
  `crates/fast_io/src/kernel_version.rs`.
- SSH stdio ownership:
  `crates/rsync_io/src/ssh/connection.rs:30-244`.
- SSH transfer driver:
  `crates/core/src/client/remote/ssh_transfer.rs:547-609`.
- Sibling splice audit: `docs/audits/splice-ssh-stdio.md`.
- Linux man pages: `io_uring_setup(2)`, `io_uring_enter(2)`,
  `io_uring_register(2)`, `io_uring_prep_read(3)`,
  `io_uring_prep_write(3)`, `pipe(7)`, `man 7 io_uring`.
- Kernel.org references (verify before citing in code comments):
  - `IORING_OP_READ` / `IORING_OP_WRITE`: introduced in Linux 5.6.
  - `IORING_FEAT_FAST_POLL`: introduced in Linux 5.7. Needs
    verification against the changelog.
  - `IORING_REGISTER_PBUF_RING`: introduced in Linux 5.19. Needs
    verification against the changelog.
