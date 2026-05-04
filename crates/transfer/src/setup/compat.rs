//! Compatibility flags exchange.
//!
//! Handles writing and exchanging compatibility flags between server and client,
//! including the pre-release `'V'` capability encoding difference. Mirrors
//! upstream `compat.c:736-741`.

use super::capability::{
    build_compat_flags_from_client_info, client_has_pre_release_v_flag, parse_client_info,
};
use protocol::CompatibilityFlags;
use std::io::{self, Write};
use std::net::TcpStream;

/// Writes compatibility flags to the output stream, handling the pre-release
/// `'V'` capability flag encoding difference.
///
/// When the client advertises `'V'` (a deprecated pre-release flag), the
/// server writes the compat flags as a single byte and implicitly enables
/// `CF_VARINT_FLIST_FLAGS`. Otherwise, the flags are written using the
/// standard varint encoding.
///
/// The client-side `read_varint()` is compatible with both encodings because
/// a single byte with the high bit clear decodes identically under both
/// schemes.
///
/// # Upstream reference
///
/// `compat.c:737-741`:
/// ```c
/// if (strchr(client_info, 'V') != NULL) {
///     if (!write_batch)
///         compat_flags |= CF_VARINT_FLIST_FLAGS;
///     write_byte(f_out, compat_flags);
/// } else
///     write_varint(f_out, compat_flags);
/// ```
pub(crate) fn write_compat_flags<W: Write + ?Sized>(
    writer: &mut W,
    mut flags: CompatibilityFlags,
    client_info: &str,
) -> io::Result<CompatibilityFlags> {
    if client_has_pre_release_v_flag(client_info) {
        // Pre-release 'V' client: implicitly enable VARINT_FLIST_FLAGS and
        // write as a single byte (upstream: write_batch is never true here).
        // upstream: compat.c:738-740
        flags |= CompatibilityFlags::VARINT_FLIST_FLAGS;
        writer.write_all(&[flags.bits() as u8])?;
    } else {
        protocol::write_varint(writer, flags.bits() as i32)?;
    }
    Ok(flags)
}

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
/// * `stream` - Raw TcpStream for writing (will use write_all directly)
/// * `client_args` - Arguments received from client (to parse -e option)
/// * `allow_inc_recurse` - Whether incremental recursion is allowed
///
/// # Returns
///
/// Returns the final negotiated compatibility flags, or an I/O error.
pub fn exchange_compat_flags_direct(
    protocol: protocol::ProtocolVersion,
    stream: &mut TcpStream,
    client_args: &[String],
    allow_inc_recurse: bool,
) -> io::Result<Option<CompatibilityFlags>> {
    if !protocol.uses_binary_negotiation() {
        return Ok(None);
    }

    // Parse client capabilities from -e option (mirrors upstream compat.c:712-732)
    let client_info = parse_client_info(client_args);

    // Build compat flags based on client capabilities.
    // allow_inc_recurse is passed through from the caller; when true and the client
    // advertises 'i', the CF_INC_RECURSE flag will be set.
    let our_flags = build_compat_flags_from_client_info(&client_info, allow_inc_recurse);

    // Server ONLY WRITES compat flags (upstream compat.c:736-741)
    // The client reads but DOES NOT send anything back - it's unidirectional!
    // CRITICAL: Write directly to TcpStream, NOT through any trait abstraction!
    // Handle pre-release 'V' flag: use single-byte write instead of varint.
    let final_flags = write_compat_flags(stream, our_flags, &client_info)?;

    // CRITICAL: Flush immediately to ensure data leaves application buffers
    stream.flush()?;

    Ok(Some(final_flags))
}
