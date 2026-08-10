//! Shared timeout guard for integration tests.
//!
//! Prevents CI hangs by enforcing a wall-clock deadline on each test. If the
//! closure does not complete within the specified duration, the test panics
//! with a descriptive message rather than blocking the CI pipeline forever.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Timeout for local (non-network) integration tests.
#[allow(dead_code)]
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Timeout for SSH-based integration tests, which need extra time for
/// connection setup, key exchange, and potential retries.
#[allow(dead_code)]
pub const SSH_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs `f` on a dedicated thread, panicking if it does not complete within
/// `timeout`.
///
/// The closure is moved to a new thread so the calling thread can monitor the
/// deadline. If the deadline expires, the calling thread panics. The spawned
/// worker thread is intentionally left detached - the test process will exit
/// on the panic and the OS reclaims the thread.
#[allow(dead_code)]
pub fn run_with_timeout<F>(timeout: Duration, f: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        f();
        // Ignore send errors - the receiver may have already timed out.
        let _ = tx.send(());
    });

    match rx.recv_timeout(timeout) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!(
                "test exceeded {:.0}s timeout - possible hang detected",
                timeout.as_secs_f64()
            );
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("test thread panicked before completing");
        }
    }
}
