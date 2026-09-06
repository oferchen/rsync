//! Per-connection process splitting for the daemon.
//!
//! Upstream rsync forks a child for every accepted connection so that
//! per-session state - most importantly `chroot()` and the working directory,
//! which are process-wide - cannot leak into the next connection.
//! upstream: `main.c` accept loop, `clientserver.c:978-987` `rsync_module()`.
//!
//! This module owns the `fork`/`waitpid` calls so that `crates/daemon`, which
//! is `#![deny(unsafe_code)]`, can split connections through a safe API.
//!
//! # Thread-safety contract
//!
//! `fork()` duplicates only the calling thread. A child of a *multithreaded*
//! parent may inherit locks - notably the allocator's - held by threads that
//! do not exist in the child, so the first allocation can deadlock. POSIX
//! permits only async-signal-safe calls between `fork` and `exec`, and a
//! daemon child that serves a session never execs.
//!
//! [`become_daemon`](crate::daemonize::become_daemon) sidesteps this by
//! requiring that it run before any thread is spawned. A per-connection fork
//! cannot make that promise for free: the caller must guarantee it, and the
//! guarantee is a property of the accept loop, not of this module. Callers
//! must fork from a single-threaded accept path.
#![cfg(unix)]

use std::io;

/// Which side of a [`fork_session`] split the caller is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkSide {
    /// The original daemon process, holding the child's pid so it can be
    /// reaped by [`try_reap`] or [`wait_for_child`].
    Parent {
        /// Pid of the child now serving the connection.
        child_pid: i32,
    },
    /// The freshly forked child, which serves exactly one connection.
    Child,
}

/// Splits the current process to serve one connection.
///
/// The child must finish via [`exit_child`] rather than returning, so the
/// parent's buffered state is not flushed twice.
///
/// # Safety contract
///
/// The caller must invoke this from a single-threaded accept path. See the
/// module docs: a child forked from a multithreaded parent can deadlock on
/// its first allocation.
#[allow(unsafe_code)]
pub fn fork_session() -> io::Result<ForkSide> {
    // SAFETY: `fork` takes no arguments and touches no caller memory. The
    // multithreaded-parent hazard is a CALLER obligation stated above, not a
    // property established here - deliberately not claiming, as
    // `daemonize::become_daemon` does, that no threads exist, because a
    // per-connection fork runs long after daemon startup.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(ForkSide::Child),
        child_pid => Ok(ForkSide::Parent { child_pid }),
    }
}

/// Terminates a forked child without running the parent's exit handlers.
///
/// Uses `_exit(2)`: a child that returned normally would unwind Rust
/// destructors and flush `stdio` buffers the parent also owns, duplicating
/// whatever the parent had pending at fork time.
#[allow(unsafe_code)]
pub fn exit_child(code: i32) -> ! {
    // SAFETY: `_exit` never returns and touches no caller memory. It is the
    // async-signal-safe termination path, which is what a post-fork child is
    // restricted to.
    unsafe { libc::_exit(code) }
}

/// How a forked child ended.
///
/// This is deliberately the *process* vocabulary, not the session's: what a
/// non-zero exit or a fatal signal means for a connection is a decision for
/// the daemon, which owns the session model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildEnd {
    /// Returned this status from `main`, or passed it to [`exit_child`].
    Exited(i32),
    /// Killed by this signal.
    Signalled(i32),
}

impl ChildEnd {
    /// Classifies a raw `wait`-family status word.
    fn from_status(status: libc::c_int) -> Self {
        if libc::WIFSIGNALED(status) {
            Self::Signalled(libc::WTERMSIG(status))
        } else {
            // WIFEXITED is the only other outcome `waitpid` reports without
            // WUNTRACED/WCONTINUED, neither of which is requested here.
            Self::Exited(libc::WEXITSTATUS(status))
        }
    }
}

/// Reaps one specific child if it has already ended, without blocking.
///
/// `Ok(None)` means the child is still running - the caller keeps it and asks
/// again later.
///
/// # Why per-pid, and never `waitpid(-1, ..)`
///
/// A bulk "reap anything that exited" sweep is the obvious shape and it is
/// **wrong here**. This daemon forks children that are *not* sessions: the
/// `name converter` helper, the pre-/post-transfer `exec` hooks, and the
/// authentication helper. Those are owned by `std::process::Child`, which
/// reaps them through its own `wait`. A bulk reaper racing in the accept loop
/// would consume their statuses first and leave `Child::wait` to fail with
/// `ECHILD` - a helper whose exit code the daemon needs would silently become
/// unobservable.
///
/// Taking the pid as a parameter makes that impossible to get wrong: this
/// call can only ever collect the child the caller already owns.
///
/// upstream: `socket.c:676-684` `sigchld_handler()` does use
/// `waitpid(-1, NULL, WNOHANG)`, but it can afford to - it discards the status
/// (a NULL status pointer) and upstream's helper children are waited for in
/// contexts that tolerate it. oc reports session outcomes, so it needs the
/// status, and therefore needs the narrower call.
#[allow(unsafe_code)]
pub fn try_reap(child_pid: i32) -> io::Result<Option<ChildEnd>> {
    let mut status: libc::c_int = 0;
    // SAFETY: `waitpid` writes only through `status`, a live local. The pid is
    // a specific child, so this cannot consume another child's status, and
    // `WNOHANG` makes it return 0 instead of blocking while that child runs.
    let waited = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
    match waited {
        0 => Ok(None),
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(Some(ChildEnd::from_status(status))),
    }
}

/// Blocks until one specific child ends, and reports how.
///
/// The shutdown counterpart to [`try_reap`]: a draining daemon must not leave
/// a session half-served, so it waits rather than polling.
#[allow(unsafe_code)]
pub fn wait_for_child(child_pid: i32) -> io::Result<ChildEnd> {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: as `try_reap`, minus `WNOHANG` so the call blocks.
        let waited = unsafe { libc::waitpid(child_pid, &mut status, 0) };
        if waited == -1 {
            let error = io::Error::last_os_error();
            // Resuming a syscall the kernel never completed is not a retry
            // policy - no work was done and nothing is being backed off. A
            // daemon with signal handlers installed sees EINTR here routinely.
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Ok(ChildEnd::from_status(status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Both sides of one split are observed, and the child's exit code
    /// reaches the parent - i.e. `fork_session` really forked.
    #[test]
    fn fork_session_splits_parent_and_child() {
        const CHILD_EXIT_CODE: i32 = 7;

        match fork_session().expect("fork failed") {
            ForkSide::Child => exit_child(CHILD_EXIT_CODE),
            ForkSide::Parent { child_pid } => {
                assert!(child_pid > 0, "parent must learn a real child pid");
                assert_eq!(
                    wait_for_child(child_pid).expect("wait failed"),
                    ChildEnd::Exited(CHILD_EXIT_CODE)
                );
            }
        }
    }

    /// The exit code survives the round trip through `try_reap`, so the
    /// parent can tell a clean session from a failed one.
    #[test]
    fn try_reap_reports_the_childs_exit_code() {
        const CHILD_EXIT_CODE: i32 = 7;

        match fork_session().expect("fork failed") {
            ForkSide::Child => exit_child(CHILD_EXIT_CODE),
            ForkSide::Parent { child_pid } => {
                assert_eq!(
                    poll_until_reaped(child_pid),
                    ChildEnd::Exited(CHILD_EXIT_CODE)
                );
            }
        }
    }

    /// NON-VACUITY, and it carries two claims a single-outcome stub would
    /// fail: a child that has not ended is reported as still running, and a
    /// killed one is reported as SIGNALLED rather than as a clean exit.
    ///
    /// Without this, a `try_reap` hard-wired to `Some(ChildEnd::Exited(0))`
    /// would satisfy every other assertion in this module.
    #[test]
    fn a_running_child_is_not_reaped_and_a_killed_one_reports_its_signal() {
        match fork_session().expect("fork failed") {
            // Outlive the parent's observation without exiting. The parent
            // kills this child, so the sleep is an upper bound, not a wait.
            ForkSide::Child => {
                std::thread::sleep(Duration::from_secs(30));
                exit_child(0);
            }
            ForkSide::Parent { child_pid } => {
                assert_eq!(
                    try_reap(child_pid).expect("try_reap failed"),
                    None,
                    "a child that has not ended must not be reaped"
                );
                kill_child(child_pid, libc::SIGKILL);
                assert_eq!(
                    wait_for_child(child_pid).expect("wait failed"),
                    ChildEnd::Signalled(libc::SIGKILL)
                );
            }
        }
    }

    /// The pid parameter is the whole safety argument: reaping is scoped to a
    /// child the caller owns, so it can never consume the status of the
    /// daemon's name-converter, `exec` hook or auth helper. A pid that is not
    /// ours is an error, not a silently fabricated outcome.
    #[test]
    fn try_reap_refuses_a_pid_that_is_not_our_child() {
        // pid 1 is never a child of the test process.
        let error = try_reap(1).expect_err("reaping a non-child must fail");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    }

    /// Drives `try_reap` until it collects the child, and reports what it
    /// said. The deadline means a reaper that never reports a child fails the
    /// test instead of hanging the suite.
    fn poll_until_reaped(child_pid: i32) -> ChildEnd {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match try_reap(child_pid).expect("try_reap failed") {
                Some(end) => return end,
                None => {
                    assert!(Instant::now() < deadline, "child was never reaped");
                    std::thread::yield_now();
                }
            }
        }
    }

    #[allow(unsafe_code)]
    fn kill_child(child_pid: i32, signal: libc::c_int) {
        // SAFETY: signals a pid this process just forked and has not reaped.
        let sent = unsafe { libc::kill(child_pid, signal) };
        assert_ne!(sent, -1, "kill failed: {}", io::Error::last_os_error());
    }
}
