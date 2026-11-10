//! Session-level entry point for the native server over standard I/O.
//!
//! `core` owns the transfer pipeline driver. The public entry point
//! (`run_server_stdio`) forwards directly to the threaded
//! `transfer::run_server_stdio`.

use std::io::{Read, Write};

use crate::server::{ServerConfig, ServerResult};
use transfer::{FatalReport, TransferProgressCallback};

/// Executes the native server over standard I/O.
///
/// This is the session-level facade the `--server` entry point calls.
///
/// # Errors
///
/// Propagates every error from the underlying server body unchanged.
pub fn run_server_stdio<'p>(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    progress: Option<&'p mut dyn TransferProgressCallback>,
    fatal_report: Option<&'p FatalReport>,
) -> ServerResult {
    transfer::run_server_stdio(config, stdin, stdout, progress, fatal_report)
}

/// Executes the native server over the process's own standard input and
/// output descriptors, which the transfer polls together.
///
/// `stdin` and `stdout` are handles on descriptors 0 and 1.
///
/// # Errors
///
/// Propagates every error from the underlying server body unchanged.
#[cfg(unix)]
pub fn run_server_fds<'p>(
    config: ServerConfig,
    stdin: std::fs::File,
    stdout: std::fs::File,
    progress: Option<&'p mut dyn TransferProgressCallback>,
    fork_point: Option<std::sync::Arc<crate::server::ForkPoint>>,
    fatal_report: Option<&'p FatalReport>,
) -> ServerResult {
    transfer::run_server_fds(config, stdin, stdout, progress, fork_point, fatal_report)
}
