//! The daemon-child reap contract, measured against the production signal
//! disposition.
//!
//! Two defects meet here. A test daemon reaped by a trailing
//! `let _ = child.kill(); let _ = child.wait();` is reaped only on the success
//! path, so any panic between the spawn and those lines orphans it. And the
//! orphan is not reliably removable by SIGTERM:
//! `platform::signal::register_signal_handlers` converts SIGTERM into an
//! atomic flag (upstream `main.c` defers signals the same way), so an orphan
//! that has stopped reaching that flag check survives every ordinary cleanup
//! sweep and accumulates until the machine hits `ulimit -u`.
//!
//! The narrowness matters: an orphan still turning its accept loop *does*
//! observe the flag and exits on SIGTERM. Measured on this tree, the 33
//! permanent orphans a `-p daemon` run leaves behind all died on SIGTERM. So
//! the flag conversion is not what makes them permanent - nothing signals them
//! at all - and the guard below is what removes the dependence on any of it.
//!
//! Each arm below is paired with the control that makes it non-vacuous:
//!
//! | arm | control | what the pair proves |
//! |---|---|---|
//! | handlers installed, SIGTERM | no handlers, SIGTERM | survival comes from the handler, not the harness |
//! | inline reap + unwind | `ReapOnDrop` + unwind | the guard, not the fixture, is what stops the leak |
//!
//! Synchronisation is a blocking read of the stub's stdout line protocol, never
//! a sleep: a timing-dependent pass here would be indistinguishable from a
//! flake.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::panic::{self, AssertUnwindSafe};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use test_support::ReapOnDrop;

const STUB: &str = env!("CARGO_BIN_EXE_sigterm-flag-stub");

/// Whether `pid` still occupies a process-table slot.
///
/// `kill(pid, 0)` succeeds for a live process *and* for an un-reaped zombie,
/// which is exactly the distinction under test: only a guard that both kills
/// and waits makes this report `false`.
fn pid_exists(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 performs the permission and existence check
    // without delivering a signal, and cannot invalidate any Rust state.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Delivers `signal` to `pid`, failing loudly if the target has vanished.
fn signal_pid(pid: u32, signal: libc::c_int) {
    // SAFETY: signalling a child of this process touches no Rust state; the
    // return code is checked so a vanished target is surfaced, not ignored.
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal) };
    assert_eq!(rc, 0, "signal {signal} delivery to {pid} failed");
}

/// Spawns the stub and blocks until it reports `READY`.
///
/// The blocking `read_line` is the handoff: it returns exactly when the stub
/// has finished installing its handlers, so nothing downstream races startup.
fn spawn_stub(extra: &[&str]) -> (Child, BufReader<ChildStdout>) {
    let mut child = Command::new(STUB)
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sigterm-flag-stub");
    let mut stdout = BufReader::new(child.stdout.take().expect("stub stdout is piped"));
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read stub READY line");
    assert_eq!(line.trim(), "READY", "stub must announce readiness first");
    (child, stdout)
}

/// The mechanism behind ledger row 1247: with the production handlers
/// installed, SIGTERM sets a flag instead of terminating, so a child that has
/// stopped polling that flag can only be removed with SIGKILL - which is what
/// [`ReapOnDrop`] sends.
#[test]
fn sigterm_becomes_a_flag_so_only_the_drop_reap_removes_the_child() {
    let (child, mut stdout) = spawn_stub(&[]);
    let pid = child.id();
    let mut guard = ReapOnDrop::new(child);

    signal_pid(pid, libc::SIGTERM);

    // Blocking read: returns only once the stub has seen the shutdown flag, so
    // the signal is proven delivered rather than assumed to have arrived.
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read stub signal line");
    assert_eq!(
        line.trim(),
        "SIGTERM-OBSERVED",
        "SIGTERM must reach the child and set the shutdown flag"
    );

    // Now non-racy: the child is past signal delivery and, by construction,
    // will never exit on its own.
    assert!(
        matches!(guard.try_wait(), Ok(None)),
        "a child holding the production SIGTERM handler must survive SIGTERM"
    );

    drop(guard);
    assert!(
        !pid_exists(pid),
        "ReapOnDrop must kill and reap a child that ignores SIGTERM"
    );
}

/// Anti-vacuity control for the arm above: strip the handlers and the very
/// same stub dies of SIGTERM. Without this, a stub that merely happened to
/// outlive the signal would read as proof of the flag conversion.
#[test]
fn without_the_handlers_the_same_stub_dies_of_sigterm() {
    use std::os::unix::process::ExitStatusExt;

    let (mut child, _stdout) = spawn_stub(&["--no-handlers"]);
    let pid = child.id();

    signal_pid(pid, libc::SIGTERM);

    let status = child.wait().expect("wait for unhandled-SIGTERM stub");
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "with the default disposition SIGTERM must terminate the child"
    );
}

/// Ledger row 750's leak, reproduced: the trailing-`kill`/`wait` shape reaps
/// only on the success path, so an unwind orphans the daemon.
///
/// This is the "guard removed" half of the non-vacuity pair - it must observe
/// a survivor, otherwise the guarded arm below proves nothing.
#[test]
fn an_inline_reap_orphans_the_child_when_the_test_unwinds() {
    let leaked = Arc::new(AtomicU32::new(0));

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let (child, _stdout) = spawn_stub(&[]);
        leaked.store(child.id(), Ordering::SeqCst);
        // Everything above this line is the old fixture verbatim.
        panic!("assertion failure between the spawn and the trailing reap");
        // Unreachable, and that is the defect:
        // let _ = child.kill();
        // let _ = child.wait();
    }));
    assert!(result.is_err(), "the arm must actually unwind");

    let pid = leaked.load(Ordering::SeqCst);
    assert!(
        pid_exists(pid),
        "inline reap must be shown to leak, or the guarded arm proves nothing"
    );

    // `std::process::Child::drop` neither kills nor waits, so the unwind left a
    // live orphan with no handle to reap it through. Remove it by pid; the
    // resulting zombie is collected when this test process exits.
    signal_pid(pid, libc::SIGKILL);
}

/// The fix: the same unwind, with the child owned by [`ReapOnDrop`].
#[test]
fn the_drop_reap_removes_the_child_when_the_test_unwinds() {
    let spawned = Arc::new(AtomicU32::new(0));

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let (child, _stdout) = spawn_stub(&[]);
        spawned.store(child.id(), Ordering::SeqCst);
        let _guard = ReapOnDrop::new(child);
        panic!("assertion failure between the spawn and the trailing reap");
    }));
    assert!(result.is_err(), "the arm must actually unwind");

    let pid = spawned.load(Ordering::SeqCst);
    assert!(
        !pid_exists(pid),
        "ReapOnDrop must reap during the unwind, leaving no orphan"
    );
}

/// A child that exits on its own is still reaped, and the guard reports its
/// status rather than forcing a kill.
#[test]
fn wait_returns_the_childs_own_exit_status() {
    let child = Command::new(STUB)
        .arg("--no-handlers")
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn stub");
    let pid = child.id();
    let guard = ReapOnDrop::new(child);
    signal_pid(pid, libc::SIGKILL);

    use std::os::unix::process::ExitStatusExt;
    let status = guard.wait().expect("wait for stub");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(!pid_exists(pid), "an explicit wait must also reap the pid");
}
