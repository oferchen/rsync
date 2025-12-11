//! Server protocol setup utilities.
//!
//! This module mirrors upstream rsync's `compat.c:setup_protocol()` function,
//! handling protocol version negotiation and compatibility flags exchange.

use protocol::{CompatibilityFlags, ProtocolVersion};
use std::io::{self, Read, Write};
use std::net::TcpStream;

/// Parses client capabilities from the `-e` option argument.
///
/// The `-e` option contains a string like "efxCIvu" where each letter indicates
/// a capability the client supports. This mirrors upstream's `client_info` string
/// parsing in compat.c:712-732.
///
/// # Capability Letters
/// - 'i' - incremental recurse
/// - 'L' - symlink time-setting support
/// - 's' - symlink iconv translation support
/// - 'f' - flist I/O-error safety support
/// - 'x' - xattr hardlink optimization not desired
/// - 'C' - checksum seed order fix
/// - 'I' - inplace_partial behavior
/// - 'v' - varint for flist & compat flags
/// - 'u' - include name of uid 0 & gid 0
///
/// # Arguments
/// * `client_args` - Arguments received from client (e.g., ["-e", "efxCIvu", "--server", ...])
///
/// # Returns
/// The capability string (e.g., "fxCIvu") with the leading 'e' removed, or empty string if not found.
///
/// # Examples
/// - `["-e", "fxCIvu"]` → "fxCIvu"
/// - `["-efxCIvu"]` → "fxCIvu"
/// - `["-vvde.LsfxCIvu"]` → ".LsfxCIvu" (combined short options)
fn parse_client_info(client_args: &[String]) -> String {
    // Look for -e followed by capability string
    for i in 0..client_args.len() {
        let arg = &client_args[i];

        // Check for combined short options like "-vvde.LsfxCIvu"
        // The -e option may appear in the middle of other short options
        if arg.starts_with('-') && !arg.starts_with("--") {
            if let Some(e_pos) = arg.find('e') {
                // Found 'e' in the argument
                // Everything after 'e' is the capability string
                if e_pos + 1 < arg.len() {
                    let caps = &arg[e_pos + 1..];
                    // Skip leading '.' which is a version placeholder
                    // (upstream puts '.' when protocol_version != PROTOCOL_VERSION)
                    if caps.starts_with('.') && caps.len() > 1 {
                        return caps[1..].to_string();
                    }
                    return caps.to_string();
                }
            }
        }

        // Check for "-e" "fxCIvu" (separate args)
        if arg == "-e" && i + 1 < client_args.len() {
            return client_args[i + 1].clone();
        }
    }

    String::new()
}

/// Builds compatibility flags based on client capabilities.
///
/// This mirrors upstream compat.c:712-732 which checks the client_info string
/// to determine which flags to enable.
///
/// # Arguments
/// * `client_info` - Capability string from client's `-e` option (e.g., "fxCIvu")
/// * `allow_inc_recurse` - Whether incremental recursion is allowed
///
/// # Returns
/// CompatibilityFlags with only the capabilities the client advertised
fn build_compat_flags_from_client_info(client_info: &str, allow_inc_recurse: bool) -> CompatibilityFlags {
    let mut flags = CompatibilityFlags::from_bits(0);

    // INC_RECURSE: enabled if we allow it AND client supports 'i'
    if allow_inc_recurse && client_info.contains('i') {
        flags |= CompatibilityFlags::INC_RECURSE;
    }

    // SAFE_FILE_LIST: client advertises 'f'
    if client_info.contains('f') {
        flags |= CompatibilityFlags::SAFE_FILE_LIST;
    }

    // CHECKSUM_SEED_FIX: client advertises 'C'
    if client_info.contains('C') {
        flags |= CompatibilityFlags::CHECKSUM_SEED_FIX;
    }

    // VARINT_FLIST_FLAGS: client advertises 'v'
    if client_info.contains('v') {
        flags |= CompatibilityFlags::VARINT_FLIST_FLAGS;
    }

    flags
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
///
/// # Returns
///
/// Returns the final negotiated compatibility flags, or an I/O error.
pub fn exchange_compat_flags_direct(
    protocol: ProtocolVersion,
    stream: &mut TcpStream,
    client_args: &[String],
) -> io::Result<Option<CompatibilityFlags>> {
    let _ = std::fs::write(
        "/tmp/exchange_compat_ENTRY",
        format!("protocol={}", protocol.as_u8()),
    );

    if protocol.as_u8() < 30 {
        let _ = std::fs::write("/tmp/exchange_compat_EARLY_RETURN", "1");
        return Ok(None);
    }

    // Parse client capabilities from -e option (mirrors upstream compat.c:712-732)
    let client_info = parse_client_info(client_args);
    let _ = std::fs::write(
        "/tmp/exchange_compat_CLIENT_INFO",
        format!("client_info='{}'", client_info),
    );

    // Build compat flags based on client capabilities
    // For now, hardcode allow_inc_recurse=true (should come from config)
    let our_flags = build_compat_flags_from_client_info(&client_info, true);

    let _ = std::fs::write(
        "/tmp/exchange_compat_FLAGS_BUILT",
        format!("{:#x} (client_info: {})", our_flags.bits(), client_info),
    );

    // Server ONLY WRITES compat flags (upstream compat.c:736-738)
    // The client reads but DOES NOT send anything back - it's unidirectional!
    // CRITICAL: Write directly to TcpStream, NOT through any trait abstraction!
    let _ = std::fs::write("/tmp/exchange_compat_BEFORE_VARINT", "1");
    protocol::write_varint(stream, our_flags.bits() as i32)?;
    let _ = std::fs::write("/tmp/exchange_compat_AFTER_VARINT", "1");

    // CRITICAL: Flush immediately to ensure data leaves application buffers
    let _ = std::fs::write("/tmp/exchange_compat_BEFORE_FLUSH", "1");
    stream.flush()?;
    let _ = std::fs::write("/tmp/exchange_compat_AFTER_FLUSH", "1");

    // NOTE: In daemon mode, the server does NOT read anything back!
    // The upstream code shows that when am_server=true, only write_varint is called.
    // The client (am_server=false) reads the flags but doesn't send anything back.
    // This is a UNIDIRECTIONAL send from server to client.

    let _ = std::fs::write("/tmp/exchange_compat_RETURNING_OK", "1");
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
    let _ = std::fs::write(
        "/tmp/setup_protocol_ENTRY",
        format!(
            "protocol={}, skip={}",
            protocol.as_u8(),
            skip_compat_exchange
        ),
    );

    // For daemon mode, the binary 4-byte protocol exchange has already happened
    // via the @RSYNCD text protocol (upstream compat.c:599-607 checks remote_protocol != 0).
    // We skip that exchange here since our HandshakeResult already contains the negotiated protocol.

    // Send compatibility flags for protocol >= 30 (UNIDIRECTIONAL)
    // This mirrors upstream compat.c:710-743 which happens INSIDE setup_protocol()
    // However, for daemon mode, this should be skipped if the exchange was already done
    // directly on the raw TcpStream via exchange_compat_flags_direct()
    if protocol.as_u8() >= 30 && !skip_compat_exchange {
        let _ = std::fs::write("/tmp/setup_protocol_SENDING_COMPAT", "1");
        // Build our compat flags (server side)
        // This mirrors upstream compat.c:712-732 which builds flags from client_info string
        let our_flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS;

        let _ = std::fs::write(
            "/tmp/setup_protocol_COMPAT_FLAGS",
            format!("{:#x}", our_flags.bits()),
        );

        // Server ONLY WRITES compat flags (upstream compat.c:736-738)
        // The client reads but does NOT send anything back - it's unidirectional!
        // Upstream uses write_varint() or write_byte() depending on protocol version

        // Log the actual bytes before encoding
        let value = our_flags.bits() as i32;
        let _ = std::fs::write(
            "/tmp/setup_protocol_VARINT_VALUE",
            format!("{} (0x{:x})", value, value),
        );

        // Encode to a buffer first so we can log the exact bytes
        let mut varint_buf = Vec::new();
        protocol::write_varint(&mut varint_buf, value)?;
        let _ = std::fs::write(
            "/tmp/setup_protocol_VARINT_BYTES",
            format!("{:02x?}", varint_buf),
        );

        // Now write to stdout
        let _ = std::fs::write(
            "/tmp/setup_protocol_BEFORE_WRITE",
            format!("Writing {} bytes", varint_buf.len()),
        );
        stdout.write_all(&varint_buf)?;
        let _ = std::fs::write("/tmp/setup_protocol_AFTER_VARINT", "1");

        // Log total bytes written (including any buffered data)
        let _ = std::fs::write(
            "/tmp/setup_protocol_BEFORE_FLUSH",
            format!(
                "About to flush after writing {} varint bytes",
                varint_buf.len()
            ),
        );
        stdout.flush()?;
        let _ = std::fs::write("/tmp/setup_protocol_AFTER_FLUSH", "1");

        // NOTE: Do NOT read anything back! The upstream code shows:
        // - When am_server=true: only write_varint is called
        // - When am_server=false: only read_varint is called
        // This is a UNIDIRECTIONAL send from server to client.

        // TODO: Store our_flags for use by role handlers
        // Upstream stores these in global variables, but we'll need to pass them through
        // the HandshakeResult or ServerConfig
    }

    let _ = std::fs::write("/tmp/setup_protocol_EXIT", "1");
    Ok(())
}
