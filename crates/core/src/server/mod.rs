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
/// Reader abstraction supporting plain and multiplex modes.
mod reader;
/// Server-side Receiver role implementation.
pub mod receiver;
/// Enumerations describing the role executed by the server process.
pub mod role;
/// Server-side protocol setup utilities.
pub mod setup;
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

/// Statistics returned from server execution.
#[derive(Debug, Clone)]
pub enum ServerStats {
    /// Statistics from receiver role.
    Receiver(TransferStats),
    /// Statistics from generator role.
    Generator(GeneratorStats),
}

/// Result type for server operations.
pub type ServerResult = io::Result<ServerStats>;

/// Executes the native server entry point over standard I/O.
///
/// The implementation performs the protocol handshake before dispatching to the
/// role-specific handler. The receiver role receives file lists and deltas from
/// the client, while the generator role sends file lists and deltas to the client.
///
/// # Returns
///
/// Returns `ServerStats` on successful transfer, or an error if handshake or transfer fails.
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> ServerResult {
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
/// Returns `ServerStats` on successful transfer, or an error if transfer fails.
pub fn run_server_with_handshake<W: Write>(
    config: ServerConfig,
    mut handshake: HandshakeResult,
    stdin: &mut dyn Read,
    mut stdout: W,
) -> ServerResult {
    eprintln!(
        "[server] run_server_with_handshake: role={:?}, protocol={}",
        config.role,
        handshake.protocol.as_u8()
    );

    // Protocol has already been negotiated via:
    // - perform_handshake() for SSH mode (binary exchange)
    // - @RSYNCD exchange for daemon mode
    // So we just use the protocol from handshake and activate multiplex if needed.
    // This mirrors upstream's setup_protocol() which skips the exchange when
    // remote_protocol != 0 (already set by @RSYNCD).

    // Extract buffered data before calling setup_protocol
    // This is critical for daemon mode where the BufReader may have read ahead
    let buffered_data = std::mem::take(&mut handshake.buffered);
    eprintln!(
        "[server] Buffered data from handshake: {} bytes",
        buffered_data.len()
    );
    if !buffered_data.is_empty() {
        let hex_len = buffered_data.len().min(128);
        eprintln!(
            "[server] Buffered data (first {} bytes): {:02x?}",
            hex_len,
            &buffered_data[..hex_len]
        );
    }

    // Chain buffered data with stdin BEFORE calling setup_protocol
    // This ensures setup_protocol reads from the correct stream
    let buffered = std::io::Cursor::new(buffered_data);
    let mut chained_stdin = buffered.chain(stdin);

    // Call setup_protocol() - mirrors upstream main.c:1245
    // This is the FIRST thing start_server() does after setting file descriptors
    // IMPORTANT: Parameter order matches upstream: f_out first, f_in second!
    // For SSH mode, compat_exchanged is false (do compat exchange here).
    // For daemon mode, compat_exchanged is true (already done on raw TcpStream before calling this function).
    setup::setup_protocol(
        handshake.protocol,
        &mut stdout,
        &mut chained_stdin,
        handshake.compat_exchanged,
    )?;

    // CRITICAL: Flush stdout BEFORE wrapping it in ServerWriter!
    // The setup_protocol() call above may have buffered data (compat flags varint).
    // If we don't flush here, that buffered data will be written AFTER we activate
    // multiplex, causing the client to interpret it as multiplexed data instead of
    // plain data, resulting in "unexpected tag" errors.
    stdout.flush()?;
    eprintln!("[server] Flushed stdout after setup_protocol");

    // Activate multiplex for protocol >= 23 (mirrors upstream main.c:1247-1248)
    // This applies to BOTH daemon and SSH modes. The protocol was negotiated via:
    // - @RSYNCD exchange for daemon mode (remote_protocol already set)
    // - perform_handshake() binary exchange for SSH mode
    // In both cases, upstream's setup_protocol() is called, then multiplex is activated.
    let reader = reader::ServerReader::new_plain(chained_stdin);
    let mut writer = writer::ServerWriter::new_plain(stdout);

    if handshake.protocol.as_u8() >= 23 {
        writer = writer.activate_multiplex()?;
        eprintln!(
            "[server] Multiplex activated (protocol {})",
            handshake.protocol.as_u8()
        );
    }

    let mut chained_reader = reader;

    match config.role {
        ServerRole::Receiver => {
            eprintln!("[server] Entering Receiver role");
            let mut ctx = ReceiverContext::new(&handshake, config);
            let stats = ctx.run(&mut chained_reader, &mut writer)?;

            Ok(ServerStats::Receiver(stats))
        }
        ServerRole::Generator => {
            eprintln!("[server] Entering Generator role");

            // Convert OsString args to PathBuf for file walking
            let paths: Vec<std::path::PathBuf> =
                config.args.iter().map(std::path::PathBuf::from).collect();
            eprintln!("[server] Generator paths: {paths:?}");

            let mut ctx = GeneratorContext::new(&handshake, config);
            eprintln!("[server] Generator context created, calling run()");
            let stats = ctx.run(&mut chained_reader, &mut writer, &paths)?;
            eprintln!("[server] Generator run() completed");

            Ok(ServerStats::Generator(stats))
        }
    }
}
