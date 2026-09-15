//! A wedged-daemon stand-in carrying the production SIGTERM disposition.
//!
//! Installs the *real* daemon signal handlers via
//! `platform::signal::register_signal_handlers` - the same call the daemon
//! accept loop makes - and then stops making progress. That reproduces an
//! orphaned `oc-rsync --daemon` without needing a daemon config, a port, or a
//! module tree, so the reap contract can be tested against the genuine signal
//! disposition rather than an imitation of it.
//!
//! Line protocol on stdout, each line flushed so a reader can synchronise on
//! it with a blocking read instead of a sleep:
//!
//! - `READY` - handlers installed (or deliberately not, see below).
//! - `SIGTERM-OBSERVED` - the shutdown flag was set, proving the signal was
//!   delivered and converted to a flag rather than terminating the process.
//!
//! With `--no-handlers` the registration is skipped, leaving the default
//! terminating disposition. That arm is the control: it shows the survival of
//! the default arm comes from the handler and not from the test harness.

use std::io::{self, Write};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

/// Upper bound on the stub's own lifetime.
///
/// The assertions that drive this process complete in milliseconds; this only
/// bounds the damage if a test dies between spawning the stub and reaping it,
/// so a stray stub can never become the kind of un-reapable orphan the guard
/// under test exists to prevent.
const SELF_DESTRUCT: Duration = Duration::from_secs(120);

/// Interval between shutdown-flag checks.
///
/// A signal handler can only publish through an atomic, so observing it means
/// polling. This mirrors the daemon accept loop, which bounds its `poll(2)`
/// park for the same reason. It sets observation latency only - no assertion
/// depends on its value.
const FLAG_POLL: Duration = Duration::from_millis(1);

fn announce(line: &str) {
    let mut stdout = io::stdout();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn main() {
    let register = !std::env::args().any(|arg| arg == "--no-handlers");
    let flags = register.then(|| {
        platform::signal::register_signal_handlers().expect("register production signal handlers")
    });

    announce("READY");

    let deadline = Instant::now() + SELF_DESTRUCT;
    let mut announced = false;
    while Instant::now() < deadline {
        if let Some(flags) = flags.as_ref() {
            if !announced && flags.shutdown.load(Ordering::Relaxed) {
                announce("SIGTERM-OBSERVED");
                announced = true;
            }
        }
        // Deliberately keeps running after the flag is seen: a wedged daemon
        // is one that has the flag set and still never exits.
        thread::sleep(FLAG_POLL);
    }
}
