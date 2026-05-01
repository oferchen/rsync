## io_uring SQPOLL and DEFER_TASKRUN for daemon socket I/O

Tracking issue: oc-rsync task #1267. Sibling audit:
[`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md).

## Summary

For oc-rsync's daemon TCP path, enable `IORING_SETUP_DEFER_TASKRUN` (paired
with `IORING_SETUP_SINGLE_ISSUER`) on kernels >= 6.1 with a transparent
fallback to default ring setup, and do not enable `IORING_SETUP_SQPOLL` by
default. SQPOLL's `CAP_SYS_NICE` requirement, page-fault stalls of the
kernel poller, and idle-wake costs make it a poor fit for a long-lived
daemon serving many short-lived TCP sessions; DEFER_TASKRUN gives most of
the syscall-reduction benefit without privilege requirements.

## Current state

Today the io_uring socket path uses default ring setup with neither flag.
`crates/fast_io/src/io_uring/config.rs:336` sets `sqpoll: false` in
`IoUringConfig::default()`, and `for_large_files` (line 353) and
`for_small_files` (line 368) repeat the same default. The socket factory
constructs rings from this default config at
`crates/fast_io/src/io_uring/socket_factory.rs:66-69` (reader) and
`socket_factory.rs:117-120` (writer), so daemon connections never request
SQPOLL today. The only code path that wires up SQPOLL is the build helper
at `config.rs:381-396`, which calls `IoUring::builder().setup_sqpoll(idle)`
and falls back via `SQPOLL_FALLBACK` (`config.rs:30`) on `EPERM`/`ENOMEM`.
There is no reference to `DEFER_TASKRUN` or `SINGLE_ISSUER` anywhere under
`crates/fast_io/src/io_uring/`. The module-level docs at
`crates/fast_io/src/io_uring/mod.rs:56-59` and the privilege table at
`mod.rs:63-70` enumerate SQPOLL but do not yet mention DEFER_TASKRUN.

## Trade-offs

| Flag | Kernel | Privilege | Syscall reduction | Risk |
|---|---|---|---|---|
| default (none) | 5.6+ | none | one `io_uring_enter` per submit batch | baseline; safe |
| `IORING_SETUP_SQPOLL` | 5.6+ (poller stable 5.13+) | `CAP_SYS_NICE` or root since 5.11 | submit syscall eliminated while poller is hot | poller spins burning a core; idle-wake cost on bursty traffic; page-fault on user buffer stalls the poller (it cannot service other rings); shared kernel thread across rings since 5.11 mitigates but does not remove the cost |
| `IORING_SETUP_DEFER_TASKRUN` (+ `SINGLE_ISSUER`) | 6.1+ | none | task-work runs only at completion-wait time, removing per-CQE IPI/wake-up overhead | requires the same task to submit and reap; not safe when a separate accept thread hands the ring to a worker; minor latency penalty if completions sit until the next `io_uring_enter` |

SQPOLL's design assumption is one busy ring fed by one process; an rsync
daemon listening on TCP 873 alternates between long idle periods and
many short-lived ring lifecycles, which is the worst case for a polling
kernel thread. DEFER_TASKRUN, conversely, was added precisely to lower
the per-completion overhead of mostly-idle rings without demanding any
capability; see Linux commit `c0e0d6ba25f1` (Jens Axboe, 2022) and the
ring-setup flags described in `man 2 io_uring_setup` / `man 7 io_uring`.

## Daemon-specific concerns

The daemon TCP path is bidirectional and per-connection: a parent process
runs the accept loop on the listening socket, then forks/spawns a child
that owns the connection's read and write halves. Upstream rsync uses
plain blocking `read(2)`/`write(2)` from `io.c` against the socket fd
inside that child; there is no shared event loop across connections, and
no single ring is reused across the daemon's lifetime. That model maps
cleanly onto DEFER_TASKRUN: each child creates a per-connection ring,
submits and reaps from the same task, and tears the ring down at session
end. SQPOLL is a poor match here because each new connection would spawn
or share a kernel poller for what is often a few-MB transfer, and the
CAP_SYS_NICE check would silently downgrade most production deployments
(daemons typically drop privileges after binding port 873). The accept
loop itself does not need io_uring; staying on `accept4(2)` keeps the
code simple and avoids ring lifetime questions on listener teardown.

A second concern is page-fault behaviour. The SQPOLL kernel thread cannot
fault user memory; if the submission queue references a not-yet-resident
page, the poller blocks until the issuer calls `io_uring_enter` to
service it, which negates the no-syscall win. On a daemon that streams
large files through registered buffers this is mostly avoided, but the
control-channel multiplex frames are short and frequently touched from
user space, exactly the workload where the stall shows up.

## Recommendation

1. Add `IoUringConfig::defer_taskrun: bool` (default `true`) and gate it
   on a kernel-version probe in `config.rs` next to the existing 5.6
   check. When the kernel is >= 6.1 and the policy is `Auto`/`Enabled`,
   call `builder.setup_defer_taskrun()` and `builder.setup_single_issuer()`
   before `build(sq_entries)`. On older kernels or on builder failure,
   fall back to the current default ring exactly as the SQPOLL path does
   at `config.rs:381-396`.
2. Leave `IoUringConfig::sqpoll` opt-in (default `false`) and document
   that it is intended for benchmarking only. Do not set it anywhere in
   the daemon code path.
3. Use the per-connection ring lifecycle already implied by
   `socket_reader_from_fd` / `socket_writer_from_fd` so DEFER_TASKRUN's
   single-issuer constraint is honoured naturally.
4. Extend the privilege table in `crates/fast_io/src/io_uring/mod.rs:63`
   with a DEFER_TASKRUN row and update `is_io_uring_available()` notes.
5. Add a parity test that constructs a ring with the new flag set on a
   kernel that supports it and asserts no functional regression against
   the default-ring path.

## References

- `crates/fast_io/src/io_uring/config.rs:30` - `SQPOLL_FALLBACK` atomic.
- `crates/fast_io/src/io_uring/config.rs:336,353,368` - default configs.
- `crates/fast_io/src/io_uring/config.rs:381-396` - `build_ring()` SQPOLL path.
- `crates/fast_io/src/io_uring/socket_factory.rs:66-69,117-120` - per-fd config.
- `crates/fast_io/src/io_uring/mod.rs:56-70` - SQPOLL module docs and table.
- Linux commit `c0e0d6ba25f1` ("io_uring: add IORING_SETUP_DEFER_TASKRUN", 6.1).
- Linux commit `97bbdc06a444` ("io_uring: add IORING_SETUP_SINGLE_ISSUER", 6.0).
- `man 2 io_uring_setup`, `man 7 io_uring` - flag semantics and constraints.
- Upstream rsync `io.c` (`target/interop/upstream-src/rsync-3.4.1/io.c`)
  uses plain `read(2)`/`write(2)`; no io_uring usage, so this is purely
  an oc-rsync-side optimisation with no wire-protocol implication.
