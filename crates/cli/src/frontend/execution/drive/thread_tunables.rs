//! Process-wide thread-pool tunables wired up during CLI startup.
//!
//! `--rayon-threads` is applied here via `rayon::ThreadPoolBuilder::build_global`,
//! which must run before any rayon work begins. `--tokio-threads` is honoured
//! when the async transports are constructed; this module provides the helper
//! used by those construction sites.

#![deny(unsafe_code)]

use std::io::Write;
use std::num::NonZeroUsize;

use core::message::Role;
use core::rsync_error;
use logging_sink::MessageSink;

/// Installs the requested rayon worker count for the lifetime of the process.
///
/// `rayon::ThreadPoolBuilder::build_global` may only succeed once per process.
/// Subsequent invocations or a pool that has already been initialised by
/// another caller leave the existing pool in place; the failure is reported
/// to `stderr` as a non-fatal warning so transfers continue with the default
/// thread count.
pub(crate) fn install_rayon_thread_count<Err>(threads: NonZeroUsize, stderr: &mut MessageSink<Err>)
where
    Err: Write,
{
    if let Err(error) = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.get())
        .build_global()
    {
        let message = rsync_error!(
            1,
            "failed to set --rayon-threads={}: {error}",
            threads.get()
        )
        .with_role(Role::Client);
        let _ = stderr.write(&message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use logging_sink::MessageSink;

    #[test]
    fn install_rayon_thread_count_does_not_panic_when_already_initialised() {
        // The global rayon pool may already be initialised by another test.
        // The helper must report the error (or a no-op) without panicking.
        let mut buf: Vec<u8> = Vec::new();
        let mut sink = MessageSink::new(&mut buf);
        install_rayon_thread_count(NonZeroUsize::new(2).expect("2 is non-zero"), &mut sink);
    }
}
