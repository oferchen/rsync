//! Kernel-side install and SIGSYS-on-block tests for the daemon worker
//! seccomp filter (LSM-SECCOMP).
//!
//! The filter is installed via `seccomp(2)` with default action
//! `KILL_PROCESS`; once engaged it cannot be relaxed and applies to every
//! subsequent syscall on the calling thread. Tests therefore fork a fresh
//! child process for each scenario so the parent's harness thread is
//! never restricted.
//!
//! Gated on `cfg(all(target_os = "linux", feature = "daemon-seccomp"))`.
//! On any other build configuration the file compiles to an empty crate
//! so `cargo nextest run -p daemon` keeps working.

#![cfg(all(target_os = "linux", feature = "daemon-seccomp"))]

use seccompiler::{
    BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch, apply_filter,
};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::ExitStatusExt;

/// Architecture detected at build time.
fn target_arch() -> Option<TargetArch> {
    if cfg!(target_arch = "x86_64") {
        Some(TargetArch::x86_64)
    } else if cfg!(target_arch = "aarch64") {
        Some(TargetArch::aarch64)
    } else {
        None
    }
}

/// Minimal allowlist used by the kernel-install scenarios.
///
/// Distinct from the production allowlist on purpose: the install test
/// only needs the syscalls between `apply_filter` and the next test step
/// (the allowed syscall, or the negative-path `ptrace`). The production
/// allowlist's completeness is exercised by the daemon-driven transfer
/// integration test, not here.
fn minimal_allowlist() -> Vec<i64> {
    let mut s = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_rt_sigreturn,
        libc::SYS_getpid,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mprotect,
        libc::SYS_futex,
        libc::SYS_prctl,
        libc::SYS_seccomp,
    ];
    s.sort_unstable();
    s.dedup();
    s
}

/// Install a seccomp filter on the calling thread with the supplied
/// allowlist and a `KillProcess` default action.
fn install_filter(allowlist: &[i64]) -> io::Result<()> {
    let arch = target_arch().expect("test target arch must be x86_64 or aarch64");
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for sysno in allowlist {
        rules.insert(*sysno, Vec::new());
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        arch,
    )
    .map_err(|e| io::Error::other(e.to_string()))?;
    let prog: BpfProgram = TryInto::try_into(filter)
        .map_err(|e: seccompiler::Error| io::Error::other(e.to_string()))?;
    apply_filter(&prog).map_err(|e| io::Error::other(e.to_string()))
}

/// Fork a child, run `child` in it, and return the wait4 status.
///
/// `child` returns the desired exit code; the helper invokes
/// `_exit(code)` so no destructors fire after the filter installs.
fn fork_run(child: impl FnOnce() -> i32) -> libc::c_int {
    // SAFETY: single-threaded fork in a test harness. The child closure
    // is responsible for not touching APIs that allocate or take locks
    // after `apply_filter` installs.
    #[allow(unsafe_code)]
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", io::Error::last_os_error());
    if pid == 0 {
        let code = child();
        // SAFETY: _exit is async-signal-safe and skips at-exit handlers,
        // which is what we want once seccomp is engaged.
        #[allow(unsafe_code)]
        unsafe {
            libc::_exit(code)
        };
    }
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid on a pid we just forked.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(rc >= 0, "waitpid failed: {}", io::Error::last_os_error());
    status
}

#[test]
fn allowed_syscall_succeeds_under_filter() {
    let raw = fork_run(|| {
        if install_filter(&minimal_allowlist()).is_err() {
            // Filter install failed; surface a distinct exit code so the
            // parent can diagnose. Seccomp install can fail on locked-
            // down kernels (e.g. some CI sandboxes); we treat that as a
            // skip rather than a hard fail below.
            return 77;
        }
        // getpid is in the allowlist - must succeed without trapping.
        // SAFETY: getpid is async-signal-safe and cannot fail.
        #[allow(unsafe_code)]
        let pid = unsafe { libc::getpid() };
        if pid <= 0 { 1 } else { 0 }
    });

    let status = std::process::ExitStatus::from_raw(raw);
    if let Some(sig) = status.signal() {
        panic!("child killed by signal {sig} while running allow-listed syscall");
    }
    let code = status.code().expect("child must exit");
    if code == 77 {
        eprintln!("seccomp filter install rejected by kernel; skipping");
        return;
    }
    assert_eq!(
        code, 0,
        "child unexpectedly exited with code {code} - allowed syscall trapped",
    );
}

#[test]
fn blocked_syscall_traps_with_sigsys() {
    let raw = fork_run(|| {
        if install_filter(&minimal_allowlist()).is_err() {
            return 77;
        }
        // ptrace is intentionally absent from the minimal allowlist.
        // KillProcess delivers SIGSYS and tears the process down before
        // this libc call returns; if we reach the next line the filter
        // failed to enforce.
        // SAFETY: ptrace call is expected to be intercepted by seccomp.
        #[allow(unsafe_code)]
        let _ = unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) };
        99
    });

    let status = std::process::ExitStatus::from_raw(raw);
    if let Some(sig) = status.signal() {
        assert_eq!(sig, libc::SIGSYS, "expected SIGSYS, got signal {sig}");
        return;
    }
    let code = status.code().expect("child must exit or be killed");
    if code == 77 {
        eprintln!("seccomp filter install rejected by kernel; skipping");
        return;
    }
    assert_ne!(
        code, 99,
        "child reached past blocked syscall - filter not enforcing",
    );
}
