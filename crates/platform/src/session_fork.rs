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
    /// reaped by [`reap_finished_children`].
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

/// Reaps every child that has already exited, without blocking.
///
/// Returns how many were reaped. A daemon that forks per connection and never
/// waits accumulates zombies, so the accept loop must call this; `WNOHANG`
/// keeps it off the latency path.
#[allow(unsafe_code)]
pub fn reap_finished_children() -> usize {
    let mut reaped = 0;
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: `waitpid` writes only through `status`, a live local. `-1`
        // means "any child"; `WNOHANG` makes it return 0 instead of blocking
        // when no child has exited.
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            return reaped;
        }
        reaped += 1;
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
                let status = wait_for(child_pid);
                assert!(libc::WIFEXITED(status), "child did not exit normally");
                assert_eq!(libc::WEXITSTATUS(status), CHILD_EXIT_CODE);
            }
        }
    }

    /// Non-vacuity: without this, a `reap_finished_children` that always
    /// returned 0 would pass every other assertion here.
    #[test]
    fn reap_finished_children_counts_an_exited_child() {
        match fork_session().expect("fork failed") {
            ForkSide::Child => exit_child(0),
            ForkSide::Parent { .. } => assert_eq!(reap_until_a_child_is_reaped(), 1),
        }
    }

    /// With no children at all the sweep must report zero rather than block.
    #[test]
    fn reap_finished_children_is_zero_without_children() {
        assert_eq!(reap_finished_children(), 0);
    }

    #[allow(unsafe_code)]
    fn wait_for(child_pid: i32) -> libc::c_int {
        let mut status: libc::c_int = 0;
        // SAFETY: blocking wait on a pid this process just forked.
        let waited = unsafe { libc::waitpid(child_pid, &mut status, 0) };
        assert_ne!(waited, -1, "waitpid failed: {}", io::Error::last_os_error());
        status
    }

    /// Drives the function under test until it reaps the child, and reports
    /// what that call returned.
    ///
    /// Polling the reaper itself, rather than peeking with `WNOWAIT` first,
    /// keeps this portable: `WNOWAIT` is a `waitid` option and macOS
    /// `waitpid` rejects it with `EINVAL`, which spun this helper forever.
    /// The deadline means a reaper that never reports a child fails the test
    /// instead of hanging the suite.
    fn reap_until_a_child_is_reaped() -> usize {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let reaped = reap_finished_children();
            if reaped > 0 {
                return reaped;
            }
            assert!(Instant::now() < deadline, "child was never reaped");
            std::thread::yield_now();
        }
    }
}
