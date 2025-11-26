#![deny(unsafe_code)]
//! Native server orchestration utilities.
//!
//! This module provides the server-side entry points for `--server` mode,
//! handling both Receiver and Generator roles as negotiated with the client.

use std::io::{self, Read, Write};

/// Server configuration derived from the compact `--server` flag string.
pub mod config;
/// Parser for the compact server flag string.
pub mod flags;
/// Server-side Generator role implementation.
pub mod generator;
/// Server-side protocol handshake utilities.
pub mod handshake;
/// Server-side Receiver role implementation.
pub mod receiver;
/// Enumerations describing the role executed by the server process.
pub mod role;

pub use self::config::ServerConfig;
pub use self::flags::{InfoFlags, ParseFlagError, ParsedServerFlags};
pub use self::generator::{GeneratorContext, GeneratorStats};
pub use self::handshake::{HandshakeResult, perform_handshake, perform_legacy_handshake};
pub use self::receiver::{ReceiverContext, TransferStats};
pub use self::role::ServerRole;

#[cfg(test)]
mod tests;

/// Executes the native server entry point over standard I/O.
///
/// The implementation performs the protocol handshake before dispatching to the
/// role-specific handler. The receiver role receives file lists and deltas from
/// the client, while the generator role sends file lists and deltas to the client.
///
/// # Returns
///
/// Returns `Ok(0)` on successful transfer, or an error if handshake or transfer fails.
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<i32> {
    // Perform protocol handshake
    let handshake = perform_handshake(stdin, stdout)?;
    run_server_with_handshake(config, handshake, stdin, stdout)
}

/// Executes the native server with a pre-negotiated protocol version.
///
/// This variant is used when the handshake has already been performed (e.g., by
/// the daemon after module authentication). The caller provides the negotiated
/// `HandshakeResult` and this function dispatches directly to the role handler.
///
/// # Returns
///
/// Returns `Ok(0)` on successful transfer, or an error if transfer fails.
pub fn run_server_with_handshake(
    config: ServerConfig,
    handshake: HandshakeResult,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> io::Result<i32> {
    match config.role {
        ServerRole::Receiver => {
            let mut ctx = ReceiverContext::new(&handshake, config);
            let stats = ctx.run(stdin, stdout)?;

            // Log statistics (for now, just return success)
            let _ = stats;
            Ok(0)
        }
        ServerRole::Generator => {
            // Convert OsString args to PathBuf for file walking
            let paths: Vec<std::path::PathBuf> =
                config.args.iter().map(std::path::PathBuf::from).collect();

            let mut ctx = GeneratorContext::new(&handshake, config);
            let stats = ctx.run(stdin, stdout, &paths)?;

            // Log statistics (for now, just return success)
            let _ = stats;
            Ok(0)
        }
    }
}
