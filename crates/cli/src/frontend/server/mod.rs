//! Server and daemon mode dispatch for the CLI frontend.
//!
//! Handles detection of `--server` and `--daemon` flags, parsing of server-mode
//! arguments, and delegation to the appropriate execution paths.

#![deny(unsafe_code)]

mod daemon;
mod flags;
mod parse;
mod run;

#[cfg(test)]
mod tests;

pub(crate) use daemon::{daemon_mode_arguments, run_daemon_mode, server_mode_requested};
pub(crate) use run::run_server_mode;
