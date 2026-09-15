//! Unconditional child-process reaping for tests that spawn a daemon.
//!
//! # Why a guard and not an inline `kill()`/`wait()` pair
//!
//! A test that spawns `oc-rsync --daemon` and reaps it with a trailing
//! `let _ = child.kill(); let _ = child.wait();` reaps it only on the success
//! path. Every `expect`, every failed `assert!`, and every early `return`
//! between the spawn and those two lines unwinds past them, so the daemon is
//! orphaned and outlives the test process.
//!
//! Which signal clears such an orphan depends on what it is still doing, and
//! the distinction is easy to get backwards. `platform::signal::register_signal_handlers`
//! converts `SIGTERM` into an atomic flag, mirroring upstream `main.c`'s
//! deferred-signal handling. A daemon still turning its accept loop *does*
//! observe that flag - the loop bounds its `poll(2)` park precisely so it can -
//! and exits, so `SIGTERM` is enough for a healthy orphan. What survives
//! `SIGTERM` indefinitely is an orphan that has stopped reaching the flag
//! check at all; nothing short of `SIGKILL` removes that one, and `SIGKILL` is
//! what [`std::process::Child::kill`] sends on Unix.
//!
//! [`ReapOnDrop`] therefore does not rely on the child cooperating, and closes
//! the unwind path the way `rsync_io::ssh::SshChildHandle` does: the reap lives
//! in `Drop`, so it runs on unwind and early return as well as on the success
//! path.

use std::io;
use std::process::{Child, ExitStatus};

/// Owns a spawned child and kills plus reaps it when dropped.
///
/// The `Drop` runs on every exit path from the owning scope, including a
/// panic unwind, so a spawned daemon cannot outlive the test that started it.
/// Reaping an already-exited child is a no-op beyond collecting its status,
/// so the guard is safe to hold over a child that exits on its own.
#[derive(Debug)]
pub struct ReapOnDrop {
    child: Option<Child>,
}

impl ReapOnDrop {
    /// Takes ownership of `child`, arming the drop-time reap.
    #[must_use]
    pub fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// The child's OS process id.
    ///
    /// # Panics
    ///
    /// Panics if the child was already consumed by [`wait`](Self::wait).
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.as_ref().expect("child already reaped").id()
    }

    /// Reports the child's exit status if it has already exited, without
    /// blocking. Mirrors [`std::process::Child::try_wait`].
    ///
    /// # Panics
    ///
    /// Panics if the child was already consumed by [`wait`](Self::wait).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child
            .as_mut()
            .expect("child already reaped")
            .try_wait()
    }

    /// Blocks until the child exits on its own and returns its status.
    ///
    /// Consumes the guard: the child is reaped here rather than at drop.
    /// Use this only where the daemon is expected to exit by itself; a child
    /// that wedges instead makes this call block forever, which is what the
    /// nextest `terminate-after` budget exists to bound.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        self.child.take().expect("child already reaped").wait()
    }
}

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // Only signal a child that is still running; killing an already-exited
        // pid is at best redundant and at worst racy once the pid is recycled.
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}
