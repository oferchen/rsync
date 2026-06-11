// Seccomp BPF allowlist for the daemon worker (LSM-SECCOMP).
//
// Layers a kernel-enforced syscall allowlist above the SEC-1.p Landlock
// LSM defense. Landlock denies path-based syscalls with EACCES; seccomp
// denies out-of-scope syscalls with SIGSYS before the kernel ever
// consults the LSM stack. Default action is `KillProcess` so a regression
// surfaces as a crash with `si_syscall` populated, never as a silent
// degradation.
//
// See `docs/design/lsm-seccomp-allowlist.md` for the per-syscall
// justification and the 14-day bake plan. Opt-in via
// `--features daemon-seccomp`; default builds compile the no-op stub
// below so the wire-in at `module_access/transfer.rs` does not need
// `#[cfg]` branching at the call site.

/// Outcome of [`apply_worker_seccomp_filter`].
#[derive(Debug)]
pub enum SeccompOutcome {
    /// Filter installed; the calling thread now traps unlisted syscalls
    /// with `SIGSYS` (default action `KILL_PROCESS`).
    #[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
    Installed,
    /// Build target is not one of the supported architectures, or the
    /// running kernel rejected the filter. Daemon should log and continue
    /// with Landlock as the sole layer.
    Unavailable,
    /// Filter construction or installation failed even though the build
    /// supports seccomp. The daemon must treat this as a fatal worker
    /// error - the intended sandbox did not engage.
    #[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
    Error(io::Error),
}

/// Applies the worker seccomp filter to the calling thread.
///
/// Call this at the same post-fork point as Landlock: after `chroot`,
/// after privilege drop, after daemon-filter rules are loaded into
/// memory, before any client-controlled data is parsed. The filter is
/// per-thread; the parent `accept(2)` loop is not affected.
///
/// On supported architectures (`x86_64`, `aarch64`) the call returns
/// [`SeccompOutcome::Installed`] on success. On other architectures and
/// on builds where the `daemon-seccomp` feature is off the call returns
/// [`SeccompOutcome::Unavailable`] without touching kernel state.
#[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
pub fn apply_worker_seccomp_filter() -> SeccompOutcome {
    use seccompiler::{
        apply_filter, BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch,
    };
    use std::collections::BTreeMap;

    let arch = if cfg!(target_arch = "x86_64") {
        TargetArch::x86_64
    } else if cfg!(target_arch = "aarch64") {
        TargetArch::aarch64
    } else {
        return SeccompOutcome::Unavailable;
    };

    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for sysno in worker_seccomp_allowlist() {
        rules.insert(sysno, Vec::new());
    }

    let filter = match SeccompFilter::new(
        rules,
        // Mismatched syscall: kill the process. SIGSYS surfaces with
        // siginfo_t::si_syscall populated, so a regression produces a
        // diagnosable crash, not a silent failure.
        SeccompAction::KillProcess,
        // Matched syscall: allow it through unconditionally. Argument-
        // level conditions are out of scope for the first cut; tightening
        // is deferred until the allowlist itself bakes.
        SeccompAction::Allow,
        arch,
    ) {
        Ok(f) => f,
        Err(err) => return SeccompOutcome::Error(io::Error::other(err.to_string())),
    };

    let prog: BpfProgram = match filter.try_into() {
        Ok(p) => p,
        Err(err) => return SeccompOutcome::Error(io::Error::other(err.to_string())),
    };

    match apply_filter(&prog) {
        Ok(()) => SeccompOutcome::Installed,
        Err(err) => SeccompOutcome::Error(io::Error::other(err.to_string())),
    }
}

/// No-op stub for non-Linux targets and builds without the
/// `daemon-seccomp` feature. Mirrors the Landlock stub pattern so the
/// wire-in at `module_access/transfer.rs` does not need `#[cfg]`
/// branches.
#[cfg(not(all(target_os = "linux", feature = "daemon-seccomp")))]
pub fn apply_worker_seccomp_filter() -> SeccompOutcome {
    SeccompOutcome::Unavailable
}

/// Returns the deduplicated syscall numbers comprising the worker
/// allowlist documented in `docs/design/lsm-seccomp-allowlist.md`.
///
/// Centralised so the unit tests can audit completeness without
/// reconstructing the filter. The list spans four buckets:
///
/// - A: file I/O on the module tree (read/write/open/stat/rename/etc.)
/// - B: network and IPC for the wire protocol
/// - C: process / scheduling / runtime primitives
/// - D: io_uring (additive; harmless when the runtime path is inert)
#[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
pub fn worker_seccomp_allowlist() -> Vec<i64> {
    let mut s: Vec<i64> = Vec::new();

    // Bucket A - file I/O on the module tree.
    s.extend([
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_pwrite64,
        libc::SYS_preadv,
        libc::SYS_pwritev,
        libc::SYS_preadv2,
        libc::SYS_pwritev2,
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_close,
        libc::SYS_close_range,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_lseek,
        libc::SYS_ftruncate,
        libc::SYS_fsync,
        libc::SYS_fdatasync,
        libc::SYS_fallocate,
        libc::SYS_fchmodat,
        libc::SYS_fchmod,
        libc::SYS_fchownat,
        libc::SYS_fchown,
        libc::SYS_utimensat,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_unlinkat,
        libc::SYS_mkdirat,
        libc::SYS_symlinkat,
        libc::SYS_linkat,
        libc::SYS_readlinkat,
        libc::SYS_getdents64,
        libc::SYS_copy_file_range,
        seccomp_syscall_sendfile(),
        libc::SYS_splice,
        libc::SYS_tee,
        libc::SYS_vmsplice,
        seccomp_syscall_fadvise64(),
        libc::SYS_fgetxattr,
        libc::SYS_fsetxattr,
        libc::SYS_flistxattr,
        libc::SYS_fremovexattr,
        libc::SYS_getxattr,
        libc::SYS_lgetxattr,
        libc::SYS_setxattr,
        libc::SYS_lsetxattr,
        libc::SYS_listxattr,
        libc::SYS_llistxattr,
        libc::SYS_removexattr,
        libc::SYS_lremovexattr,
    ]);

    // Bucket B - network and IPC for the wire protocol.
    s.extend([
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_recvmmsg,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_sendmmsg,
        libc::SYS_setsockopt,
        libc::SYS_getsockopt,
        libc::SYS_shutdown,
        libc::SYS_getsockname,
        libc::SYS_getpeername,
        libc::SYS_ppoll,
        libc::SYS_pselect6,
    ]);

    // Bucket C - process / scheduling / runtime.
    s.extend([
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_gettid,
        libc::SYS_getpid,
        libc::SYS_getppid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_getrandom,
        libc::SYS_prctl,
        libc::SYS_seccomp,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_tgkill,
        libc::SYS_sigaltstack,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_set_robust_list,
        libc::SYS_set_tid_address,
        libc::SYS_pipe2,
        libc::SYS_dup,
        libc::SYS_dup3,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_pwait,
        libc::SYS_eventfd2,
    ]);

    // glibc 2.35+ initialises restartable sequences per thread. `SYS_rseq`
    // is missing from older libc bindings; fall back to the documented
    // numbers from `arch/*/include/uapi/asm/unistd*.h`.
    s.push(seccomp_syscall_rseq());

    // Bucket D - io_uring. Additive: if the build does not opt into the
    // runtime io_uring path the kernel never sees these calls, so leaving
    // them in the allowlist is harmless and avoids a feature matrix here.
    s.extend([
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
    ]);

    s.sort_unstable();
    s.dedup();
    s
}

/// `rseq(2)` syscall number for the current target.
///
/// `libc::SYS_rseq` is not stable across libc versions; fall back to the
/// documented numbers from `arch/*/include/uapi/asm/unistd*.h`.
#[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
const fn seccomp_syscall_rseq() -> i64 {
    #[cfg(target_arch = "x86_64")]
    {
        334
    }
    #[cfg(target_arch = "aarch64")]
    {
        293
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        -1
    }
}

/// `sendfile(2)` syscall number for the current target.
///
/// `libc::SYS_sendfile` is exposed on x86_64 but missing from the
/// aarch64 binding in older libc releases; pinning the documented
/// `arch/*/include/uapi/asm/unistd*.h` number keeps the allowlist build
/// portable across both architectures.
#[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
const fn seccomp_syscall_sendfile() -> i64 {
    #[cfg(target_arch = "x86_64")]
    {
        40
    }
    #[cfg(target_arch = "aarch64")]
    {
        71
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        -1
    }
}

/// `fadvise64(2)` syscall number for the current target.
///
/// Same rationale as `seccomp_syscall_sendfile`: the aarch64 libc
/// binding does not expose `SYS_fadvise64`, so fall back to the
/// generic syscall number defined in
/// `include/uapi/asm-generic/unistd.h`.
#[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
const fn seccomp_syscall_fadvise64() -> i64 {
    #[cfg(target_arch = "x86_64")]
    {
        221
    }
    #[cfg(target_arch = "aarch64")]
    {
        223
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        -1
    }
}

#[cfg(all(target_os = "linux", feature = "daemon-seccomp", test))]
mod seccomp_tests {
    use super::*;
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};
    use std::collections::BTreeMap;

    #[test]
    fn allowlist_is_non_empty_and_sorted() {
        let list = worker_seccomp_allowlist();
        assert!(!list.is_empty(), "allowlist must contain entries");
        let mut sorted = list.clone();
        sorted.sort_unstable();
        assert_eq!(list, sorted, "allowlist must be sorted");
        let mut dedup = list.clone();
        dedup.dedup();
        assert_eq!(list.len(), dedup.len(), "allowlist must be deduplicated");
    }

    #[test]
    fn allowlist_contains_steady_state_essentials() {
        let list = worker_seccomp_allowlist();
        for required in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_openat,
            libc::SYS_close,
            libc::SYS_futex,
            libc::SYS_clock_gettime,
            libc::SYS_exit_group,
            libc::SYS_recvfrom,
            libc::SYS_sendto,
        ] {
            assert!(
                list.binary_search(&required).is_ok(),
                "missing essential syscall {required}",
            );
        }
    }

    #[test]
    fn filter_builds_without_error() {
        // Construct (but do not install) the filter on whatever
        // architecture the test binary runs on. If SeccompFilter::new
        // accepts the rule map and the BPF compiler accepts the filter,
        // installation on a supported kernel will not fail on shape.
        let arch = if cfg!(target_arch = "x86_64") {
            TargetArch::x86_64
        } else if cfg!(target_arch = "aarch64") {
            TargetArch::aarch64
        } else {
            eprintln!("seccomp filter build skipped: unsupported test arch");
            return;
        };
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for sysno in worker_seccomp_allowlist() {
            rules.insert(sysno, Vec::new());
        }
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::KillProcess,
            SeccompAction::Allow,
            arch,
        )
        .expect("filter construction must succeed");
        let _prog: BpfProgram = filter.try_into().expect("BPF compilation must succeed");
    }

    // The kernel-side install + SIGSYS assertion lives in
    // `tests/seccomp_worker_filter.rs` so it can fork a child and observe
    // the killed exit status without affecting the test harness thread.
}
