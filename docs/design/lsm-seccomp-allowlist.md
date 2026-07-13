# LSM-SECCOMP allowlist

Audience: oc-rsync daemon developers and operators evaluating the
`daemon-seccomp` feature.

Companion to `sec-1-p-landlock-defense-in-depth-2026-05-22.md`. Landlock
restricts what the daemon may *touch on the filesystem*; seccomp restricts
which *syscalls* it may issue at all. The two layers compose: Landlock
denies a path-based syscall with `EACCES`, seccomp denies an out-of-scope
syscall with `EPERM` before the kernel ever consults the LSM stack.

## Goals

- Per-thread allowlist applied at the same post-fork point as Landlock
  (`engage_landlock_sandbox`): after `chroot`, after privilege drop, after
  daemon-filter rules are loaded into memory, before any client-controlled
  data is parsed.
- Default action `Errno(EPERM)`. An out-of-allowlist syscall is denied
  (it never executes, so the attack surface is identical to a kill
  default) but the daemon stays alive: a `KILL_PROCESS` default delivers
  `SIGSYS` to the whole thread group and tears down the *entire* daemon,
  RSTing every concurrent connection, so a single rare, benign syscall
  from a legitimate transfer becomes a fatal outage. Upstream rsync has
  no seccomp at all; the hardening must never make a valid transfer more
  fragile than upstream. This matches the container-runtime posture
  (Docker/Podman/containerd default to `SCMP_ACT_ERRNO(EPERM)`).
- Per-architecture (`x86_64`, `aarch64`) syscall number resolution via
  `libc::SYS_*` and `seccompiler::TargetArch::native()`.
- Enabled by default when `--features daemon-seccomp` is compiled in.
  Opt out at runtime with `OC_RSYNC_NO_SECCOMP=1`.
- **Stdio sessions are skipped.** When the daemon runs as `--server
  --daemon` over stdin/stdout (remote-shell daemon mode), the process IS
  the worker - there is no parent accept loop to survive a `KillProcess`.
  The filter only applies to TCP daemon worker threads.

## Worker steady-state allowlist

The receiver/transfer worker thread issues syscalls in three buckets. The
table cites the source file responsible for the call and the rationale for
inclusion. Every entry is justified by a code path that is exercised on a
clean run.

### Bucket A - file I/O on the module tree

| Syscall | Source | Rationale |
|---------|--------|-----------|
| `read` | `crates/fast_io/src/io_uring_stub/socket_factory.rs:59` | basis file reads in the delta pipeline |
| `write` | `crates/fast_io/src/io_uring_stub/socket_factory.rs:79`, `crates/fast_io/src/sendfile/fallback.rs:94` | reconstructed file writes |
| `readv` / `writev` | `crates/fast_io/src/macos_io.rs:189,319` (vectored I/O paths) | scatter/gather batched writes |
| `pread64` / `pwrite64` | basis range copy in `crates/fast_io/src/copy_basis_range.rs` | offset-aware reads/writes on the basis file |
| `openat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:1244` | every file open under the module path |
| `openat2` | `crates/fast_io/src/dir_sandbox/mod.rs:421`, `crates/fast_io/src/secure_dir.rs:166` | SEC-1 hardened open with `RESOLVE_*` flags |
| `close` | `crates/fast_io/src/kqueue/mod.rs:290` and ubiquitous | fd lifecycle |
| `fstat` / `fstatat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:193`, `crates/metadata/src/stat_cache.rs` | quick-check, file size, ownership |
| `statx` | metadata stat cache on Linux | mtime / size / dev / inode lookups |
| `fchmodat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:763` | apply mode bits |
| `fchownat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:821` | apply uid/gid |
| `utimensat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:888` | preserve mtime/atime |
| `renameat` / `renameat2` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:1100,1132` | temp-file commit |
| `unlinkat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:385` | `--delete` path |
| `mkdirat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:465` | directory creation |
| `symlinkat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:510` | symlink replication |
| `linkat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:563,694` | hard-link replication, atomic rename of temp |
| `readlinkat` | `crates/fast_io/src/dir_sandbox/at_syscalls.rs:1400` | read symlink target |
| `getdents64` | `libc::readdir` at `crates/fast_io/src/dir_sandbox/at_syscalls.rs:1728,2085` | directory enumeration |
| `lseek` | `crates/fast_io/src/sendfile/macos.rs` and sparse seek path | sparse-zero-run + basis offset |
| `ftruncate` | temp file commit | sparse / truncate semantics |
| `fsync` / `fdatasync` | deferred fsync paths in `crates/engine/src/` | optional `--fsync` flag |
| `fallocate` | sparse pre-allocation in temp-file commit | pre-allocate destination size |
| `copy_file_range` | `crates/fast_io/src/copy_file_range.rs` | server-side local copy fast path |
| `sendfile` | `crates/fast_io/src/sendfile/` | zero-copy file -> socket |
| `splice` | `crates/fast_io/src/splice/` | pipe-mediated zero-copy |

### Bucket B - network and IPC

| Syscall | Source | Rationale |
|---------|--------|-----------|
| `recvfrom` / `recvmsg` | inbound wire traffic from the client | client request stream |
| `sendto` / `sendmsg` | outbound wire traffic to the client | server response stream |
| `setsockopt` / `getsockopt` | `crates/daemon/src/daemon/sections/server_runtime/socket_options.rs`, `crates/fast_io/src/socket_options.rs` | TCP_NODELAY, SO_RCVBUF, SO_SNDBUF, etc., applied per connection |
| `shutdown` | `crates/daemon/src/daemon/sections/server_runtime/connection.rs` | half-close to signal end-of-transfer |
| `getsockname` / `getpeername` | log_format peer expansion | %a / %h substitution |
| `poll` / `ppoll` | I/O readiness in transfer engine | timeout-bounded waits |

### Bucket C - process / scheduling / runtime

| Syscall | Source | Rationale |
|---------|--------|-----------|
| `futex` | `std::sync::Mutex`, `Condvar`, `crossbeam` | Rust synchronisation primitives |
| `rseq` | glibc 2.35+ initialises per-thread restartable sequences | required by every threaded glibc program |
| `restart_syscall` | kernel-injected when a signal interrupts a restartable blocking call (`nanosleep`, `clock_nanosleep`, `futex` timeout, `ppoll`) | the worker's bandwidth limiter, Condvar timeouts, and I/O readiness waits are all restartable; under load, signal delivery makes the kernel resume them via `restart_syscall`. Omitting it was the residual whole-daemon kill under full-CI concurrency. Not attacker-controllable (no arguments; only resumes an already-permitted call). |
| `clock_gettime` / `clock_nanosleep` | progress reporting, bandwidth limiter sleeps | `Instant::now()`, throttle waits |
| `nanosleep` | bandwidth limiter | sub-millisecond throttle |
| `gettid` | `tracing` instrumentation, thread-local debugging | identifying worker threads |
| `getpid` / `getppid` | log_format `%p`, daemon supervision | exposed in transfer log |
| `getuid` / `geteuid` / `getgid` / `getegid` | post-privilege-drop assertions | confirm effective IDs |
| `getrandom` | `tempfile::TempDir` naming, MD5/XXH3 seed | per-connection randomness |
| `prctl` | `PR_SET_NO_NEW_PRIVS` and seccomp init itself | required before `seccomp(2)` |
| `seccomp` | the filter installation call | must be in the filter to install itself when called from a thread; required for layered filters |
| `exit` / `exit_group` | thread/process termination | clean shutdown |
| `tgkill` | abort/panic path | Rust panic unwind |
| `sigaltstack` / `rt_sigaction` / `rt_sigprocmask` / `rt_sigreturn` | signal scaffolding installed by glibc and `signal-hook` | required by every Rust binary using signals |
| `brk` / `mmap` / `munmap` / `mremap` / `mprotect` / `madvise` | heap growth, jemalloc-style allocators, memory mapping of basis files | allocator and `MmapReader` |
| `set_robust_list` / `set_tid_address` | glibc thread setup | initialised on every new thread |
| `pipe2` | internal stdio plumbing for hooks (skipped when hooks are configured) | also used by `splice` setup |
| `dup3` / `dup` | fd shuffling around stdio | rare but used during `setup_transfer_streams` |
| `epoll_create1` / `epoll_ctl` / `epoll_pwait` | tokio / mio async runtime when `async` is enabled | gated on `async` feature |

### Bucket D - io_uring (additive, only when `feature = "io_uring"` is live)

| Syscall | Source | Rationale |
|---------|--------|-----------|
| `io_uring_setup` | ring creation | one per worker that opts into io_uring |
| `io_uring_enter` | submit + reap | every SQ submission |
| `io_uring_register` | `crates/fast_io/src/io_uring/buffer_ring/mod.rs:313,547` | provided buffers, fixed files |
| `mmap` / `munmap` | already in Bucket C, but io_uring relies on the SQ/CQ mapping | SQ/CQ ring mapping |

## Startup-only syscalls (parent / pre-fork)

These are *not* in the worker allowlist because the worker runs on a
thread spawned from the parent after `accept4(2)` returns. The parent
thread does not have the seccomp filter installed - it must continue
accepting connections. The filter only constrains the per-connection
worker thread.

Stdio daemon sessions (`--server --daemon` via remote shell) are exempt
from seccomp entirely because the process IS the worker - a process-
scoped filter would restrict post-transfer cleanup and the exit path.

For reference, the parent uses: `socket`, `bind`, `listen`, `accept4`,
`setsockopt`, `setuid`, `setgid`, `setgroups`, `chroot`, `chdir`,
`fork` / `clone`, `wait4`, `prctl(PR_SET_NO_NEW_PRIVS)`.

## Default action

`SeccompAction::Errno(EPERM)` - a mismatched syscall returns `-EPERM`
without executing. The kernel never runs the call, so an attacker who
hijacks the worker still cannot `execve`/`ptrace`/`mount`/etc.; the
attack surface is identical to a kill default. The difference is failure
mode: a benign, unanticipated syscall from a *legitimate* transfer
degrades to an errno the daemon surfaces through its normal I/O error
path instead of dying.

Why not `KillProcess` (the original choice): `SECCOMP_RET_KILL_PROCESS`
delivers `SIGSYS` to the whole thread group and terminates the *entire*
daemon, not just the offending worker thread - RSTing every concurrent
connection. Under full-CI concurrency a rare, load-triggered
`restart_syscall` (kernel-injected on a signal-interrupted blocking call)
was absent from the allowlist and killed the daemon mid-transfer, so
every in-flight client saw `Connection reset by peer` / exit 23. Upstream
rsync has no seccomp; making a valid transfer more fragile than upstream
is the wrong trade. The concrete gap (`restart_syscall`) is now in the
allowlist, and the non-lethal default ensures any *future* gap degrades
gracefully rather than taking the daemon down.

Alternatives considered and rejected:

- `KillProcess` / `KillThread` - lethal on the first miss; a single rare
  benign syscall aborts live transfers (see above).
- `Trap` - delivering `SIGSYS` but allowing a custom handler would let an
  attacker who controls the syscall stream catch and ignore violations.
- `Log` - allows the syscall through, so it provides no enforcement; only
  useful for a diagnostic bake, not as a steady-state default.

## Bake plan

1. Phase 5 landed the feature behind `--features daemon-seccomp`.
2. 14-day bake completed with zero missing-syscall reports.
3. The filter is now **default-ON** when the feature is compiled in.
   Operators opt out with `OC_RSYNC_NO_SECCOMP=1` or
   `OC_RSYNC_DAEMON_SECCOMP=0`.
4. Stdio daemon sessions (remote-shell mode via `lsh.sh` / SSH) are
   excluded - the filter only applies to TCP worker threads, never to a
   whole stdio process where it would restrict post-transfer cleanup.
5. The companion `landlock-feature-guidance.md` documents the layered
   defense story for distro packagers.

## Allowlist completeness criterion

The filter is correct iff *every clean daemon transfer* completes without
a denied syscall. The integration test runs a transfer slice through the
seccomp-filtered worker and asserts a zero exit code; a missing syscall
would fail with `EPERM` and surface as a non-zero exit. The negative test
asserts that an intentionally-blocked syscall (e.g. `ptrace`) returns
`EPERM` *and leaves the process alive*, proving both that the filter is
installed and enforcing and that the default action is non-lethal.
