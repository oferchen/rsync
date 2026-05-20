//! Non-Unix stub for signal-handler installation.
//!
//! Windows and other non-Unix targets do not have POSIX signals. This stub
//! exists so cross-platform callers can compile without `#[cfg(unix)]`
//! branching; it always reports success without installing anything. Real
//! Ctrl+C handling on Windows is delegated to higher-level crates (e.g.
//! `ctrlc`) at the consumer layer.

use std::io;
use std::os::raw::c_int;

use super::SignalHandlerFn;

/// No-op installer for platforms without POSIX signals.
///
/// # Errors
///
/// This stub never fails.
#[allow(clippy::missing_const_for_fn)] // REASON: signature must match Unix version
pub fn install_signal_handler(_signum: c_int, _handler: SignalHandlerFn) -> io::Result<()> {
    Ok(())
}
