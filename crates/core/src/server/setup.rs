//! Server protocol setup utilities.
//!
//! This module mirrors upstream rsync's `compat.c:setup_protocol()` function,
//! handling protocol version negotiation and compatibility flags exchange.

use protocol::{CompatibilityFlags, ProtocolVersion};
use std::io::{self, Read, Write};
use std::net::TcpStream;

/// Exchanges compatibility flags directly on a TcpStream for daemon mode.
///
/// This function performs the compat flags exchange BEFORE any buffering or
/// wrapping of the stream, mirroring upstream rsync's behavior where
/// `write_buf()` writes directly to FD when `iobuf.out_fd` is not yet initialized.
///
/// **CRITICAL:** This must be called BEFORE wrapping the stream in ServerWriter
/// to ensure the compat flags are sent as plain data, not multiplexed data.
///
/// # Arguments
///
/// * `protocol` - The negotiated protocol version
/// * `write_stream` - Raw TcpStream for writing (will use write_all directly)
/// * `read_stream` - Raw TcpStream for reading client's flags
///
/// # Returns
///
/// Returns the final negotiated compatibility flags, or an I/O error.
pub fn exchange_compat_flags_direct(
    protocol: ProtocolVersion,
    stream: &mut TcpStream,
) -> io::Result<Option<CompatibilityFlags>> {
    if protocol.as_u8() < 30 {
        eprintln!(
            "[exchange_compat_flags_direct] Protocol {} < 30, skipping compat flags",
            protocol.as_u8()
        );
        return Ok(None);
    }

    eprintln!("[exchange_compat_flags_direct] Sending compatibility flags (protocol >= 30)...");

    // Build our compat flags (server side)
    // This mirrors upstream compat.c:712-732
    let our_flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::VARINT_FLIST_FLAGS;

    // Server ONLY WRITES compat flags (upstream compat.c:736-738)
    // The client reads but DOES NOT send anything back - it's unidirectional!
    // CRITICAL: Write directly to TcpStream, NOT through any trait abstraction!
    protocol::write_varint(stream, our_flags.bits() as i32)?;

    // CRITICAL: Flush immediately to ensure data leaves application buffers
    stream.flush()?;
    eprintln!("[exchange_compat_flags_direct] Sent compat flags: {our_flags:?}");

    // NOTE: In daemon mode, the server does NOT read anything back!
    // The upstream code shows that when am_server=true, only write_varint is called.
    // The client (am_server=false) reads the flags but doesn't send anything back.
    // This is a UNIDIRECTIONAL send from server to client.

    Ok(Some(our_flags))
}

/// Performs protocol setup for the server side.
///
/// This function mirrors upstream rsync's `setup_protocol()` at `compat.c:572-644`.
///
/// When `remote_protocol` is already set (e.g., from @RSYNCD negotiation in daemon mode),
/// the 4-byte binary protocol exchange is skipped (upstream compat.c:599-607).
///
/// For protocol >= 30, compatibility flags are ALWAYS exchanged (upstream compat.c:710-743),
/// regardless of whether the binary protocol exchange happened. However, for daemon mode,
/// the compat flags exchange should be done BEFORE calling this function using
/// `exchange_compat_flags_direct()` to ensure they're sent as plain data before multiplex.
///
/// # Arguments
///
/// * `protocol` - The negotiated protocol version
/// * `stdout` - Output stream for sending server's compatibility flags (f_out in upstream)
/// * `stdin` - Input stream for reading client's compatibility flags (f_in in upstream)
/// * `skip_compat_exchange` - Set to true if compat flags were already exchanged (daemon mode)
///
/// # Returns
///
/// Returns `Ok(())` on successful setup, or an I/O error if the exchange fails.
///
/// **IMPORTANT:** Parameter order matches upstream: f_out first, f_in second!
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    _stdin: &mut dyn Read,
    skip_compat_exchange: bool,
) -> io::Result<()> {
    eprintln!(
        "[setup_protocol] Starting protocol setup for protocol {} (skip_compat_exchange={})",
        protocol.as_u8(),
        skip_compat_exchange
    );

    // For daemon mode, the binary 4-byte protocol exchange has already happened
    // via the @RSYNCD text protocol (upstream compat.c:599-607 checks remote_protocol != 0).
    // We skip that exchange here since our HandshakeResult already contains the negotiated protocol.

    // Send compatibility flags for protocol >= 30 (UNIDIRECTIONAL)
    // This mirrors upstream compat.c:710-743 which happens INSIDE setup_protocol()
    // However, for daemon mode, this should be skipped if the exchange was already done
    // directly on the raw TcpStream via exchange_compat_flags_direct()
    if protocol.as_u8() >= 30 && !skip_compat_exchange {
        eprintln!("[setup_protocol] Sending compatibility flags (protocol >= 30)...");

        // Build our compat flags (server side)
        // This mirrors upstream compat.c:712-732 which builds flags from client_info string
        let our_flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS;

        // Server ONLY WRITES compat flags (upstream compat.c:736-738)
        // The client reads but does NOT send anything back - it's unidirectional!
        // Upstream uses write_varint() or write_byte() depending on protocol version
        protocol::write_varint(stdout, our_flags.bits() as i32)?;
        stdout.flush()?;
        eprintln!("[setup_protocol] Sent compat flags: {our_flags:?}");

        // NOTE: Do NOT read anything back! The upstream code shows:
        // - When am_server=true: only write_varint is called
        // - When am_server=false: only read_varint is called
        // This is a UNIDIRECTIONAL send from server to client.

        // TODO: Store our_flags for use by role handlers
        // Upstream stores these in global variables, but we'll need to pass them through
        // the HandshakeResult or ServerConfig
    } else if skip_compat_exchange {
        eprintln!("[setup_protocol] Skipping compat flags send (already done on raw stream)");
    }

    eprintln!("[setup_protocol] Protocol setup complete");
    Ok(())
}
