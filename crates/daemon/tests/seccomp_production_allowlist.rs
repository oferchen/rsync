//! Integration test for the production daemon worker seccomp allowlist.
//!
//! Verifies that the allowlist published by the daemon crate covers the
//! syscalls a clean transfer issues on the writable surface that the
//! worker actually touches post-fork. Strategy:
//!
//! 1. Fork a child process so the test harness thread is unaffected.
//! 2. Build the production allowlist via the daemon crate's
//!    `worker_seccomp_allowlist()` accessor.
//! 3. Install the filter with the production `Errno(EPERM)` default
//!    action - identical to the production wire-in.
//! 4. Exercise a representative slice of file / metadata / process
//!    syscalls that a daemon transfer touches in steady state. Network
//!    socket creation is intentionally NOT exercised here: in production
//!    the worker inherits the accepted socket fd from the parent and
//!    never calls `socket(2)` / `connect(2)`.
//! 5. Exit cleanly. A missing allow-listed syscall would fail with EPERM
//!    and surface as a non-zero exit code in the parent.
//!
//! Gated on `cfg(all(target_os = "linux", feature = "daemon-seccomp"))`.

#![cfg(all(target_os = "linux", feature = "daemon-seccomp"))]

use daemon::seccomp_test_support::{
    SeccompOutcome, apply_worker_seccomp_filter, worker_seccomp_allowlist,
};
use std::fs;
use std::os::unix::process::ExitStatusExt;
use tempfile::TempDir;

/// Fork a child and run `body` inside it; return the wait4 raw status.
fn fork_run(body: impl FnOnce() -> i32) -> libc::c_int {
    // SAFETY: single-threaded fork in a test harness.
    #[allow(unsafe_code)]
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        let code = body();
        // SAFETY: _exit is async-signal-safe and skips at-exit handlers,
        // which is required once seccomp is engaged.
        #[allow(unsafe_code)]
        unsafe {
            libc::_exit(code)
        };
    }
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid on the pid we just forked.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(rc >= 0, "waitpid failed");
    status
}

#[test]
fn production_allowlist_covers_a_clean_transfer_slice() {
    // Pre-fork setup: create a temp dir and a basis file. Doing this
    // before the fork keeps the child's path simple (no setup-related
    // syscalls outside the allowlist).
    let tmp = TempDir::new().expect("tempdir");
    let basis_path = tmp.path().join("basis.txt");
    fs::write(&basis_path, vec![0xAB; 4096]).expect("write basis");
    let target_path = tmp.path().join("target.txt");

    let basis_path_for_child = basis_path.clone();
    let target_path_for_child = target_path.clone();

    let status = fork_run(|| {
        // 1. Install the production filter. Identical to what
        //    `engage_seccomp_sandbox` does at the daemon's post-fork
        //    point.
        match apply_worker_seccomp_filter() {
            SeccompOutcome::Installed => {}
            SeccompOutcome::Unavailable => return 77, // skip
            SeccompOutcome::Error(_) => return 78,
        }

        // 2. File read slice: open the basis, stat it, read the bytes,
        //    close. Covers openat, fstat / statx, read, close.
        let bytes = match fs::read(&basis_path_for_child) {
            Ok(b) => b,
            Err(_) => return 10,
        };
        if bytes.len() != 4096 {
            return 11;
        }

        // 3. File write slice: create a target file, write to it,
        //    close. Covers openat (O_CREAT), write, close.
        if fs::write(&target_path_for_child, &bytes).is_err() {
            return 12;
        }

        // 4. Metadata slice: stat the target, read its modified-time.
        //    Covers fstatat / statx.
        let meta = match fs::metadata(&target_path_for_child) {
            Ok(m) => m,
            Err(_) => return 13,
        };
        if meta.len() != 4096 {
            return 14;
        }

        // 5. Time / random / futex slice: take a timestamp, draw some
        //    randomness, lock a mutex. Covers clock_gettime, getrandom,
        //    futex.
        let _ = std::time::Instant::now();
        let mut buf = [0u8; 16];
        if getrandom_via_libc(&mut buf).is_err() {
            return 30;
        }
        let m = std::sync::Mutex::new(0u32);
        *m.lock().unwrap() = 1;

        0
    });

    let extracted = std::process::ExitStatus::from_raw(status);
    if let Some(sig) = extracted.signal() {
        panic!(
            "production allowlist trapped SIGSYS (signal {sig}) during clean-transfer slice - missing syscall",
        );
    }
    let code = extracted.code().expect("child must exit");
    if code == 77 {
        eprintln!("seccomp filter unavailable in this build/kernel; skipping");
        return;
    }
    assert_eq!(
        code, 0,
        "clean-transfer slice failed with exit code {code} - check allowlist",
    );
}

#[test]
fn unlisted_syscall_fails_eperm_without_killing_the_process() {
    // Intent: upstream rsync has no seccomp, so oc's hardening must never
    // make a legitimate transfer more fragile than upstream. The default
    // action is `Errno(EPERM)`, not `KillProcess`: an unanticipated
    // syscall is denied (never executes) but the worker - and therefore
    // the whole daemon and every concurrent connection - stays alive. A
    // regression back to a lethal default would let one rare, benign
    // syscall RST every in-flight transfer, which is the bug this guards.
    let status = fork_run(|| {
        match apply_worker_seccomp_filter() {
            SeccompOutcome::Installed => {}
            SeccompOutcome::Unavailable => return 77, // skip
            SeccompOutcome::Error(_) => return 78,
        }

        // ptrace is intentionally absent from the production allowlist.
        // Under an `Errno(EPERM)` default it returns -1/EPERM and execution
        // continues; under a kill default the process would die here.
        // SAFETY: raw ptrace syscall; expected to be denied by seccomp.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::syscall(libc::SYS_ptrace, libc::PTRACE_TRACEME, 0, 0, 0) };
        if rc != -1 {
            return 20; // syscall unexpectedly succeeded - not denied
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
            return 21; // denied, but with the wrong errno
        }
        // Reaching here proves the process survived a denied syscall.
        0
    });

    let extracted = std::process::ExitStatus::from_raw(status);
    if let Some(sig) = extracted.signal() {
        panic!(
            "production filter killed the process (signal {sig}) on an unlisted syscall - default action must be non-lethal Errno(EPERM)",
        );
    }
    let code = extracted.code().expect("child must exit");
    if code == 77 {
        eprintln!("seccomp filter unavailable in this build/kernel; skipping");
        return;
    }
    assert_eq!(
        code, 0,
        "unlisted syscall must fail with EPERM and leave the process alive (exit code {code})",
    );
}

/// Inline getrandom wrapper: calling `getrandom::getrandom` would pull a
/// new dep into the daemon test tree. The raw syscall is in the
/// allowlist, so a direct libc call covers the same ground.
fn getrandom_via_libc(buf: &mut [u8]) -> std::io::Result<()> {
    // SAFETY: getrandom(2) takes a buffer pointer, length, and flags.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::syscall(libc::SYS_getrandom, buf.as_mut_ptr(), buf.len(), 0) };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[test]
fn allowlist_is_a_concrete_set() {
    // Sanity: the production allowlist must round-trip through the same
    // public accessor the integration child uses.
    let list = worker_seccomp_allowlist();
    assert!(!list.is_empty());
    assert!(list.binary_search(&libc::SYS_openat).is_ok());
    assert!(list.binary_search(&libc::SYS_close).is_ok());
    assert!(list.binary_search(&libc::SYS_futex).is_ok());
}
