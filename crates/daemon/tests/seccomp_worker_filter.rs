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

use daemon::seccomp_test_support::{SeccompOutcome, apply_worker_seccomp_filter};

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
        .map_err(|e: seccompiler::BackendError| io::Error::other(e.to_string()))?;
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

/// The ownership walk must keep resolving once the worker filter engages.
///
/// The walk opens its starting directory - `/` for an absolute path - and
/// that open has to be an `openat`. On x86_64 rustix lowers the no-dirfd
/// `rustix::fs::open` to the legacy `SYS_open`, which this allowlist
/// deliberately omits in favour of the `*at` variants, so a plain `open`
/// returns `EPERM` inside a worker and every confined resolution beneath
/// it fails. Regression pin: a daemon receiver could not create its
/// destination temp file, surfacing as `mkstemp ... Permission denied`.
///
/// Installs the production filter through `apply_worker_seccomp_filter`
/// rather than a hand-copied rule set, so the pin tracks the real
/// allowlist as it changes.
#[test]
fn the_ownership_walk_resolves_under_the_worker_filter() {
    let probe = std::env::temp_dir().join(format!("oc-owner-walk-probe-{}", std::process::id()));
    std::fs::write(&probe, b"probe").expect("probe file must be creatable");
    assert!(
        probe.is_absolute(),
        "the probe must be absolute so the walk starts at `/`",
    );

    let child_path = probe.clone();
    let raw = fork_run(move || {
        match apply_worker_seccomp_filter() {
            SeccompOutcome::Installed => {}
            // Kernel refused the filter, or this build cannot install one:
            // treat as a skip, matching the sibling tests above.
            _ => return 77,
        }
        match fast_io::owner_walk::operator_open_read(&child_path) {
            Ok(_) => 0,
            Err(_) => 99,
        }
    });

    let _ = std::fs::remove_file(&probe);
    let status = std::process::ExitStatus::from_raw(raw);
    if let Some(sig) = status.signal() {
        panic!("child killed by signal {sig} while walking under the worker filter");
    }
    let code = status.code().expect("child must exit");
    if code == 77 {
        eprintln!("seccomp filter install rejected by kernel; skipping");
        return;
    }
    assert_eq!(
        code, 0,
        "the ownership walk was refused under the worker filter - a syscall the walk issues is missing from the allowlist",
    );
}

/// The receiver must still be able to materialise a FIFO under the worker
/// filter. `-a` implies `-D`, so a source tree holding a fifo, a device or a
/// unix-socket node reaches `metadata::special`, which creates all three
/// through the libc `mknod()` symbol - and glibc lowers that to `mknodat`.
///
/// Regression pin. MEASURED on Linux 7.0.0 aarch64 before the allowlist
/// admitted it: a daemon push of `{plain.txt, pipe}` landed only
/// `plain.txt` in the module, with **exit 0 and no diagnostic** - a silent
/// data loss, not a visible refusal. The control leg with
/// `OC_RSYNC_NO_SECCOMP=1` landed both.
///
/// Calls the production creator rather than a hand-written `mknod`, so the
/// pin tracks the syscall the receiver actually issues even if
/// `metadata::special` changes how it issues it.
#[test]
fn a_special_file_is_creatable_under_the_worker_filter() {
    let node = std::env::temp_dir().join(format!("oc-mknod-probe-{}", std::process::id()));
    let _ = std::fs::remove_file(&node);

    let child_path = node.clone();
    let raw = fork_run(move || {
        match apply_worker_seccomp_filter() {
            SeccompOutcome::Installed => {}
            // Kernel refused the filter, or this build cannot install one:
            // treat as a skip, matching the sibling tests above.
            _ => return 77,
        }
        match metadata::create_fifo_node_from_parts(&child_path, 0o644, false, false) {
            Ok(()) => 0,
            Err(_) => 99,
        }
    });

    let created = node.exists();
    let _ = std::fs::remove_file(&node);
    let status = std::process::ExitStatus::from_raw(raw);
    if let Some(sig) = status.signal() {
        panic!("child killed by signal {sig} while creating a fifo under the worker filter");
    }
    let code = status.code().expect("child must exit");
    if code == 77 {
        eprintln!("seccomp filter install rejected by kernel; skipping");
        return;
    }
    assert_eq!(
        code, 0,
        "fifo creation was refused under the worker filter - `-D`/`--specials` would silently drop every special file",
    );
    assert!(
        created,
        "the child reported success but no node exists - the pin would pass vacuously",
    );
}

/// Every syscall the `--delay-updates` receiver issues must be admitted by
/// the worker filter.
///
/// `--delay-updates` substitutes the implicit `.~tmp~` partial directory
/// (upstream `options.c:2563-2564`), which puts the receiver on a strictly
/// larger set of operations than a plain push: it creates a staging
/// directory, opens the temp through the operator-path fallback rather
/// than the destination sandbox, moves the pre-image aside for
/// `--backup-dir`, and renames the staged file into place. The plain push
/// reaches none of the four.
///
/// Regression pin. MEASURED on x86_64 CI at PR head `6664b588e`: a daemon
/// push with `--backup --backup-dir=bak --delay-updates` died with
/// `rsync: [receiver] mkstemp "payload" (in data) failed: Operation not
/// permitted (1)` and exit 23, while the identical push WITHOUT
/// `--delay-updates` succeeded on the same run. `EPERM` is this filter's
/// configured denial errno - Landlock reports `EACCES`, and
/// `LANDLOCK_ACCESS_FS_REFER` reports `EXDEV` - so the refusal is the
/// seccomp allowlist, not the path sandbox. The same push passes on
/// aarch64, where the legacy non-`*at` syscalls this allowlist omits do
/// not exist and glibc therefore has no alternative to lower to.
///
/// Same class as [`the_ownership_walk_resolves_under_the_worker_filter`]
/// and the same visible symptom (`mkstemp ... Permission denied`), but a
/// different caller: that pin covers the walk's starting `open`, this one
/// covers the `--delay-updates` staging sequence.
///
/// Each step reports a distinct exit code and its errno, so a failure
/// names the operation instead of leaving the caller to be guessed at.
#[test]
fn the_delay_updates_staging_sequence_runs_under_the_worker_filter() {
    let root = std::env::temp_dir().join(format!("oc-delay-updates-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).expect("probe root must be creatable");
    let staging = root.join(".~tmp~");
    let backup_dir = root.join("bak");
    let destination = root.join("payload");
    std::fs::create_dir(&backup_dir).expect("backup dir must be creatable");
    std::fs::write(&destination, b"PRE-IMAGE").expect("destination must be creatable");
    assert!(
        root.is_absolute(),
        "the probe root must be absolute so the ownership walk starts at `/`",
    );

    let mut fds: [libc::c_int; 2] = [-1, -1];
    // SAFETY: `pipe` fills two ints; the array outlives the call.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert!(rc == 0, "pipe failed: {}", io::Error::last_os_error());
    let (read_fd, write_fd) = (fds[0], fds[1]);

    let staged = staging.join("payload");
    let child_paths = (
        staging,
        staged,
        destination.clone(),
        backup_dir.join("payload"),
    );
    let raw = fork_run(move || {
        let (staging, staged, destination, backup) = child_paths;
        match apply_worker_seccomp_filter() {
            SeccompOutcome::Installed => {}
            // Kernel refused the filter, or this build cannot install one:
            // treat as a skip, matching the sibling tests above.
            _ => return 77,
        }
        // Reports the failing step's errno through the inherited pipe. The
        // exit code alone cannot carry it, and without the errno a red cell
        // says only "something was refused".
        let report = |step: i32, err: &io::Error| -> i32 {
            let line = format!("step {step} errno {:?}\n", err.raw_os_error());
            // SAFETY: `write` on an inherited pipe fd with a borrowed buffer
            // whose length is passed explicitly; async-signal-safe.
            #[allow(unsafe_code)]
            unsafe {
                libc::write(write_fd, line.as_ptr().cast(), line.len())
            };
            step
        };

        // upstream: util1.c:1518-1530 handle_partial_dir(PDIR_CREATE).
        // The receiver creates this through the ownership walk, so the probe
        // calls the same primitive rather than `std::fs::create_dir_all`:
        // glibc lowers `mkdir()` to the legacy `mkdir(2)` on x86_64, which
        // this allowlist deliberately omits in favour of the `*at` variants,
        // so a bare `create_dir_all` would pin a syscall production no longer
        // issues.
        if let Err(e) = fast_io::operator_mkdir(&staging, 0o777) {
            return report(10, &e);
        }
        // upstream: receiver.c:426-434 open_tmpfile() -> secure_mkstemp().
        // The temp's parent is the staging dir, not the destination root,
        // so this takes the operator-path fallback rather than the
        // destination `DirSandbox` the plain push uses.
        if let Err(e) = fast_io::ConfinedFallback::confined().open_at(
            &staged,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        ) {
            return report(11, &e);
        }
        // upstream: receiver.c:694 make_backup(fname, False) - the delayed
        // sweep's own backup tier. `operator_rename` is the ownership walk
        // plus `renameat`, which is the syscall shape the allowlist has to
        // admit; the confinement variant the sweep uses adds a root check in
        // userspace and issues the same calls.
        if let Err(e) = fast_io::operator_rename(&destination, &backup, true) {
            return report(12, &e);
        }
        // upstream: receiver.c:709 do_rename(partialptr, fname). The sweep
        // issues this through the ownership walk (`operator_rename`), so the
        // probe does too: a bare `std::fs::rename` reaches glibc's wrapper,
        // lowered to the legacy `rename(2)` on x86_64, which this allowlist
        // omits in favour of `renameat`.
        if let Err(e) = fast_io::operator_rename(&staged, &destination, true) {
            return report(13, &e);
        }
        0
    });

    // SAFETY: closing the parent's write end so the read below sees EOF.
    #[allow(unsafe_code)]
    unsafe {
        libc::close(write_fd)
    };
    let mut detail = String::new();
    {
        use std::io::Read;
        use std::os::fd::FromRawFd;
        // SAFETY: `read_fd` is owned by this scope and not used elsewhere.
        #[allow(unsafe_code)]
        let mut reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
        let _ = reader.read_to_string(&mut detail);
    }
    let _ = std::fs::remove_dir_all(&root);

    let status = std::process::ExitStatus::from_raw(raw);
    if let Some(sig) = status.signal() {
        panic!("child killed by signal {sig} during the --delay-updates staging sequence");
    }
    let code = status.code().expect("child must exit");
    if code == 77 {
        eprintln!("seccomp filter install rejected by kernel; skipping");
        return;
    }
    let step = match code {
        10 => "create the `.~tmp~` staging directory",
        11 => "open the staged temp file through the operator-path fallback",
        12 => "move the pre-image into the `--backup-dir`",
        13 => "rename the staged file over the destination",
        _ => "an unrecognised step",
    };
    assert_eq!(
        code, 0,
        "the worker filter refused the --delay-updates receiver at: {step} ({detail}) - \
         a syscall that step issues is missing from the allowlist",
    );
}
