//! Per-connection process splitting for the daemon.
//!
//! Upstream rsync forks a child for every accepted connection so that
//! per-session state - most importantly `chroot()` and the working directory,
//! which are process-wide - cannot leak into the next connection.
//! upstream: `socket.c:695` `start_accept_loop()`, which forks per connection
//! at socket.c:761.
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

/// Closes listener descriptors a forked child inherited but must not keep.
///
/// A child serves exactly one already-accepted connection, so every *listening*
/// socket it inherited is dead weight. Keeping them open is not merely untidy:
/// the port stays bound for as long as any child lives, so a daemon restart
/// races its own outgoing sessions for the address.
///
/// upstream: `socket.c:761-768` `start_accept_loop()` closes each listener in
/// the child, immediately after the fork and before serving.
///
/// # Which descriptors this must NOT be given
///
/// Only listeners. The accepted stream is the session, and oc's log sink is
/// opened once at startup and inherited deliberately - upstream reopens the log
/// in the child precisely because it *did* close it, and oc has no
/// `logfile_close` counterpart to mirror. A blanket close-all sweep would
/// silence the child's own diagnostics.
///
/// # Errors are unrecoverable, not ignorable
///
/// `close(2)` on a descriptor this process owns fails only on `EBADF`, which
/// means the caller passed a descriptor that was already closed - a bug in the
/// caller, not a runtime condition. There is no corrective action, and the
/// child has not yet started the session it would report through. Upstream
/// likewise checks no status here.
#[allow(unsafe_code)]
pub fn close_inherited_listeners(listener_fds: &[i32]) {
    for &fd in listener_fds {
        // SAFETY: `close` takes an integer and touches no caller memory. Each
        // fd is a listener the parent owned and the child inherited across
        // `fork`, so closing it in the child affects only this process's table.
        unsafe { libc::close(fd) };
    }
}

/// Ties the calling process's life to its parent's, for a helper that must
/// not outlive the daemon that forked it.
///
/// On Linux the kernel is asked to deliver `SIGKILL` when the parent exits
/// (`PR_SET_PDEATHSIG`). The setting is cleared by any later credential
/// change, so call this AFTER dropping privileges. Elsewhere there is no
/// such request, and the caller must watch a channel to the parent for EOF
/// instead.
///
/// Returns an error when the parent is already gone - its pid no longer
/// matches `parent_pid` - since the kernel request would then never fire.
pub fn die_with_parent(parent_pid: u32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGKILL)?;
    if std::os::unix::process::parent_id() != parent_pid {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "parent process exited before the child was tied to it",
        ));
    }
    Ok(())
}

/// Makes `fd` the calling process's standard error.
///
/// A supervisor that must read what a forked child writes to stderr - a log
/// configured as `/dev/stderr` included, which the kernel resolves through
/// fd 2 - hands the child a pipe's write end, and the child calls this before
/// it writes anything.
pub fn redirect_stderr(fd: impl std::os::fd::AsFd) -> io::Result<()> {
    nix::unistd::dup2_stderr(fd).map_err(io::Error::from)
}

/// Makes `pid` - or the caller, for `0` - the leader of a new process group
/// whose id is its pid.
///
/// A supervisor that must later stop a forked process together with every
/// process it forks in turn calls this on both sides of the fork: the child
/// for itself and the parent for the child. Whichever side runs first creates
/// the group, so it exists before the parent can act on it. A child that has
/// already exited cannot join a group and needs none, so `ESRCH` is success.
///
/// The parent's call can lose the race to the child. Once the child has
/// exec'd, `setpgid()` answers `EACCES`; once it is exiting, has become a
/// session leader or has left the caller's session, `EPERM` (POSIX
/// `setpgid()`; xnu `bsd/kern/kern_prot.c` `setpgid()`). The child's own call
/// may already have made the group, so either error is success when the child
/// leads a group of its own id, and is returned otherwise.
pub fn lead_process_group(pid: i32) -> io::Result<()> {
    let target = nix::unistd::Pid::from_raw(pid);
    match nix::unistd::setpgid(target, target) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(errno @ (nix::errno::Errno::EPERM | nix::errno::Errno::EACCES)) if pid != 0 => {
            match nix::unistd::getpgid(Some(target)) {
                Ok(pgid) if pgid == target => Ok(()),
                // Ended between the two calls: as `ESRCH` above.
                Err(nix::errno::Errno::ESRCH) => Ok(()),
                Ok(_) => Err(errno.into()),
                Err(other) => Err(other.into()),
            }
        }
        Err(errno) => Err(errno.into()),
    }
}

/// Sends `SIGTERM` to process `pid`, asking it to shut down.
///
/// upstream: main.c:1919-1921 routes SIGTERM to `sig_int()`, the daemon's
/// orderly-exit path; this is how a supervisor asks a forked daemon to stop
/// rather than killing it.
pub fn terminate_process(pid: i32) -> io::Result<()> {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    )
    .map_err(io::Error::from)
}

/// Sends `SIGKILL` to every process in group `pgid`.
///
/// A group with no members left has nothing to stop, so `ESRCH` is success.
/// Call it while the leader is still unreaped - a zombie keeps its pid, and
/// with it the group id, from being reused by an unrelated process.
///
/// On Apple platforms a group whose only member is that unreaped leader
/// answers `EPERM`, not `ESRCH`: XNU's `killpg1()` skips zombies when it walks
/// the group and then returns `EPERM` for a POSIX caller that signalled
/// nothing (xnu `bsd/kern/kern_sig.c` `killpg1()`, the `p_stat != SZOMB`
/// filter and `nfound > 0 ? 0 : (posix ? EPERM : ESRCH)`). A leader that has
/// begun exiting but is not yet waitable is skipped the same way, so the
/// `EPERM` can arrive before `waitid` reports the end. A group whose live
/// leader shares it with a member of another uid answers the same `EPERM`
/// with the leader still running. So when `pgid` is this process's own child
/// the leader is signalled directly - always permitted on one's own child, and
/// harmless on a zombie - and then awaited until it has ended. An `EPERM` for
/// any other `pgid` is a real permission failure and is returned.
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    match nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pgid),
        nix::sys::signal::Signal::SIGKILL,
    ) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        #[cfg(target_vendor = "apple")]
        Err(nix::errno::Errno::EPERM) if stop_own_leader(pgid)? => Ok(()),
        Err(errno) => Err(errno.into()),
    }
}

/// Kills `child_pid` and blocks until it has ended, without consuming its
/// status, when it is a child of this process; reports whether it was.
///
/// The probe runs before the signal, so a pid that is not this process's
/// child (`ECHILD`) is never signalled and answers `false`.
#[cfg(target_vendor = "apple")]
fn stop_own_leader(child_pid: i32) -> io::Result<bool> {
    if !wait_child_exit(child_pid, libc::WNOHANG)? {
        return Ok(false);
    }
    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child_pid),
        nix::sys::signal::Signal::SIGKILL,
    ) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
        Err(errno) => return Err(errno.into()),
    }
    wait_child_exit(child_pid, 0)
}

/// `waitid(WEXITED | WNOWAIT | extra)` on `child_pid`, never consuming its
/// status: `Ok(false)` when it is not a child of this process, `Ok(true)`
/// otherwise. With `extra == 0` the call blocks until the child has ended.
#[cfg(target_vendor = "apple")]
#[allow(unsafe_code)]
fn wait_child_exit(child_pid: i32, extra: libc::c_int) -> io::Result<bool> {
    loop {
        // SAFETY: `waitid` writes only through `info`, a live local zeroed to
        // a valid `siginfo_t`. `WNOWAIT` leaves the child waitable for its
        // reaper.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                child_pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | extra,
            )
        };
        if waited == -1 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                // As `await_child_end`: the kernel never ran the wait.
                Some(libc::EINTR) => continue,
                Some(libc::ECHILD) => return Ok(false),
                _ => return Err(error),
            }
        }
        return Ok(true);
    }
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
/// upstream: `socket.c:684-692` `sigchld_handler()` does use
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

/// Blocks until one specific child has ended, leaving its status collectable.
///
/// `WNOWAIT` reports the end without consuming it, so a later [`try_reap`]
/// still returns the outcome. That is the whole point: this answers "has the
/// session ended yet" for a caller that must not also decide "and here is how
/// it ended".
///
/// # Why an out-of-band liveness signal will not do
///
/// A pipe whose write end only the child holds looks like an equivalent
/// "child is gone" edge, and it is not. The kernel closes a dying child's
/// descriptors before it makes the child reapable - `exit_files()` runs ahead
/// of `exit_notify()` on Linux - so the reader can observe EOF during a window
/// in which [`try_reap`] still correctly reports the child as running. Any
/// caller that treats EOF as "reapable" is racing that window.
#[allow(unsafe_code)]
pub fn await_child_end(child_pid: i32) -> io::Result<()> {
    loop {
        // SAFETY: `waitid` writes only through `info`, a live local zeroed to
        // a valid `siginfo_t`. `WNOWAIT` leaves the child waitable, so this
        // cannot consume the status `try_reap` is expected to collect.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                child_pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if waited == -1 {
            let error = io::Error::last_os_error();
            // Not a retry policy: `EINTR` means the kernel never ran the wait,
            // so nothing is being re-attempted and nothing is backed off.
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Ok(());
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

    /// A child that already leads its own group - here set before its exec,
    /// so the parent's `setpgid()` answers `EACCES` - is the group the caller
    /// asked for.
    #[test]
    fn lead_process_group_accepts_a_child_that_already_leads_its_group() {
        use std::os::unix::process::CommandExt;

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        let joined = lead_process_group(pid);
        child.kill().expect("kill sleep");
        child.wait().expect("reap sleep");
        joined.expect("the child already leads its own group");
    }

    /// A child that exec'd inside the caller's group cannot be moved, and no
    /// group of its own exists, so the refusal is returned.
    #[test]
    fn lead_process_group_refuses_a_child_left_in_the_callers_group() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        let joined = lead_process_group(pid);
        child.kill().expect("kill sleep");
        child.wait().expect("reap sleep");
        let error = joined.expect_err("an exec'd child in our group is refused");
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    }

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

    /// A child tied to its live parent is accepted, and one handed a pid that
    /// is not its parent is refused - the case where the parent died before
    /// the child could ask to die with it.
    #[test]
    fn die_with_parent_checks_the_actual_parent() {
        let parent = std::process::id();
        match fork_session().expect("fork failed") {
            ForkSide::Child => {
                let tied = die_with_parent(parent).is_ok();
                let wrong_refused = die_with_parent(parent.wrapping_add(1)).is_err();
                exit_child(if tied && wrong_refused { 0 } else { 1 });
            }
            ForkSide::Parent { child_pid } => {
                assert_eq!(
                    wait_for_child(child_pid).expect("wait failed"),
                    ChildEnd::Exited(0)
                );
            }
        }
    }

    /// Killing a group stops its leader AND the processes the leader forked,
    /// which is what lets a supervisor stop a forked daemon with its sessions.
    ///
    /// The descendants hold the only write ends of `pipe`, so the parent's
    /// read reaching EOF proves both are gone - an event, not a timed wait.
    /// The grandchild writes a byte first, so it provably existed when the
    /// group was killed. A second kill finds the group empty and succeeds.
    #[test]
    fn killing_a_process_group_stops_the_leader_and_its_children() {
        use std::io::Read as _;

        let (mut reader, writer) = io::pipe().expect("pipe");
        match fork_session().expect("fork failed") {
            ForkSide::Child => {
                if lead_process_group(0).is_err() {
                    exit_child(1);
                }
                match fork_session() {
                    // The grandchild announces itself, so the kill below
                    // cannot land before it exists.
                    Ok(ForkSide::Child) => {
                        use std::io::Write as _;
                        if (&writer).write_all(b"r").is_err() {
                            exit_child(3);
                        }
                    }
                    Ok(ForkSide::Parent { .. }) => {}
                    Err(_) => exit_child(2),
                }
                // Leader and grandchild both block until the group is killed.
                loop {
                    std::thread::park();
                }
            }
            ForkSide::Parent { child_pid } => {
                drop(writer);
                lead_process_group(child_pid).expect("lead the child's group");
                let mut ready = [0u8; 1];
                reader.read_exact(&mut ready).expect("grandchild started");
                kill_process_group(child_pid).expect("kill the group");
                assert_eq!(
                    wait_for_child(child_pid).expect("wait failed"),
                    ChildEnd::Signalled(libc::SIGKILL)
                );
                let mut rest = Vec::new();
                reader
                    .read_to_end(&mut rest)
                    .expect("EOF once every member of the group is gone");
                kill_process_group(child_pid).expect("an empty group is already stopped");
            }
        }
    }

    /// A group whose only member is its ended, unreaped leader has nothing
    /// left to stop, so killing it succeeds on every platform - including
    /// Darwin, whose `killpg()` answers `EPERM` for a zombie-only group - and
    /// the leader's own exit status still reaches its reaper.
    #[test]
    fn kill_process_group_of_an_unreaped_ended_leader_succeeds() {
        const CHILD_EXIT_CODE: i32 = 5;
        match fork_session().expect("fork failed") {
            ForkSide::Child => {
                if lead_process_group(0).is_err() {
                    exit_child(1);
                }
                exit_child(CHILD_EXIT_CODE);
            }
            ForkSide::Parent { child_pid } => {
                await_child_end(child_pid).expect("observe the leader's end");
                kill_process_group(child_pid).expect("a zombie-only group is already stopped");
                assert_eq!(
                    wait_for_child(child_pid).expect("wait failed"),
                    ChildEnd::Exited(CHILD_EXIT_CODE)
                );
            }
        }
    }

    /// A terminated child ends by SIGTERM - the signal is delivered, not
    /// swallowed or replaced by a harder one.
    #[test]
    fn terminate_process_delivers_sigterm() {
        match fork_session().expect("fork failed") {
            ForkSide::Child => loop {
                std::thread::park();
            },
            ForkSide::Parent { child_pid } => {
                terminate_process(child_pid).expect("terminate");
                assert_eq!(
                    wait_for_child(child_pid).expect("wait failed"),
                    ChildEnd::Signalled(libc::SIGTERM)
                );
            }
        }
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
