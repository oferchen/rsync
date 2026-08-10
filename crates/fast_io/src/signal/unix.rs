//! Unix signal-handler installation via `libc::sigaction`.
//!
//! Encapsulates the single unsafe FFI block. The caller supplies the signal
//! number and an async-signal-safe `extern "C"` handler; this module
//! zero-initialises a `sigaction` struct, points `sa_sigaction` at the
//! handler, sets `SA_RESTART`, and submits it. On failure the underlying
//! `errno` is returned via `io::Error::last_os_error()`.

use std::io;
use std::os::raw::c_int;

use super::SignalHandlerFn;

/// Installs `handler` for `signum` with `SA_RESTART`.
///
/// `SA_RESTART` matches upstream rsync's `sig_int`/`sig_term` setup
/// (`main.c`) so blocking syscalls (`read`, `write`, `select`, `poll`)
/// resume after the handler returns instead of failing with `EINTR`.
///
/// The handler must be async-signal-safe: only atomic stores against
/// statically allocated atomics, no allocation, no locking, no calls into
/// the standard library's I/O or threading primitives.
///
/// # Errors
///
/// Returns the OS error from `sigaction(2)` on failure (rare; typically
/// indicates an invalid signal number on the host platform).
pub fn install_signal_handler(signum: c_int, handler: SignalHandlerFn) -> io::Result<()> {
    // SAFETY: `libc::sigaction` is zero-initialised (valid POD layout);
    // `sa_sigaction` is set to a `'static extern "C" fn(c_int)`, which has
    // the ABI `sigaction(2)` expects; the empty signal mask and `SA_RESTART`
    // flag mirror upstream rsync. The caller contract requires `handler` to
    // be async-signal-safe.
    #[allow(unsafe_code)]
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask as *mut libc::sigset_t);

        if libc::sigaction(signum, &action, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static TEST_FIRED: AtomicBool = AtomicBool::new(false);

    extern "C" fn test_handler(_signum: c_int) {
        TEST_FIRED.store(true, Ordering::SeqCst);
    }

    #[test]
    fn install_signal_handler_accepts_sigusr1() {
        // SIGUSR1 is reserved for application use on every POSIX platform,
        // so installing a handler should always succeed.
        assert!(install_signal_handler(libc::SIGUSR1, test_handler).is_ok());
    }

    #[test]
    fn install_signal_handler_rejects_invalid_signum() {
        // -1 is never a valid signal number; sigaction returns EINVAL.
        let err = install_signal_handler(-1, test_handler).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }
}
