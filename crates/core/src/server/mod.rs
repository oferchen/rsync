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
/// Writer abstraction supporting plain and multiplex modes.
mod writer;

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
pub fn run_server_with_handshake<W: Write>(
    config: ServerConfig,
    mut handshake: HandshakeResult,
    stdin: &mut dyn Read,
    mut stdout: W,
) -> io::Result<i32> {
    eprintln!("[server] run_server_with_handshake: role={:?}, protocol={}",
        config.role, handshake.protocol.as_u8());

    // Protocol has already been negotiated via:
    // - perform_handshake() for SSH mode (binary exchange)
    // - @RSYNCD exchange for daemon mode
    // So we just use the protocol from handshake and activate multiplex if needed.
    // This mirrors upstream's setup_protocol() which skips the exchange when
    // remote_protocol != 0 (already set by @RSYNCD).

    // Activate multiplex for protocol >= 23 (mirrors upstream main.c:1247-1248)
    let mut writer = writer::ServerWriter::new_plain(stdout);
    if handshake.protocol.as_u8() >= 23 {
        eprintln!("[server] Activating multiplex for protocol {}", handshake.protocol.as_u8());
        writer = writer.activate_multiplex()?;
        eprintln!("[server] Multiplex activated");
    } else {
        eprintln!("[server] Protocol {} < 23, not activating multiplex", handshake.protocol.as_u8());
    }

    // Extract buffered data before moving handshake
    let buffered_data = std::mem::take(&mut handshake.buffered);
    eprintln!("[server] Buffered data from handshake: {} bytes", buffered_data.len());

    // If there's buffered data from the handshake/negotiation phase, prepend it to stdin
    // This is critical for daemon mode where the BufReader may have read ahead
    let buffered = std::io::Cursor::new(buffered_data);
    let mut chained_stdin = buffered.chain(stdin);

    match config.role {
        ServerRole::Receiver => {
            eprintln!("[server] Entering Receiver role");
            let mut ctx = ReceiverContext::new(&handshake, config);
            let stats = ctx.run(&mut chained_stdin, &mut writer)?;

            // Log statistics (for now, just return success)
            let _ = stats;
            Ok(0)
        }
        ServerRole::Generator => {
            eprintln!("[server] Entering Generator role");

            // Convert OsString args to PathBuf for file walking
            let paths: Vec<std::path::PathBuf> =
                config.args.iter().map(std::path::PathBuf::from).collect();
            eprintln!("[server] Generator paths: {:?}", paths);

            let mut ctx = GeneratorContext::new(&handshake, config);
            eprintln!("[server] Generator context created, calling run()");
            let stats = ctx.run(&mut chained_stdin, &mut writer, &paths)?;
            eprintln!("[server] Generator run() completed");

            // Log statistics (for now, just return success)
            let _ = stats;
            Ok(0)
        }
    }
}
