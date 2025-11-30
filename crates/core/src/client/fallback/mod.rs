//! Helpers for delegating remote transfers to an upstream `rsync` binary.

mod args;
mod runner;

pub use args::{RemoteFallbackArgs, RemoteFallbackContext};
pub use runner::run_remote_transfer_fallback;

#[cfg(test)]
pub(crate) use runner::helpers::{fallback_error, write_daemon_password};
