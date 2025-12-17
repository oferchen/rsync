//! Server protocol setup utilities.
//!
//! This module mirrors upstream rsync's `compat.c:setup_protocol()` function,
//! handling protocol version negotiation and compatibility flags exchange.

use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};
use std::io::{self, Read, Write};
use std::net::TcpStream;

/// Result of protocol setup containing negotiated algorithms and compatibility flags.
#[derive(Debug, Clone)]
pub struct SetupResult {
    /// Negotiated checksum and compression algorithms from Protocol 30+ capability negotiation.
    /// None for protocols < 30 or when negotiation was skipped.
    pub negotiated_algorithms: Option<NegotiationResult>,
    /// Compatibility flags exchanged during protocol setup.
    /// None for protocols < 30 or when compat exchange was skipped.
    pub compat_flags: Option<CompatibilityFlags>,
    /// Checksum seed sent to client for XXHash algorithms.
    /// This seed is sent for all protocols and should be used when creating XXHash instances.
    pub checksum_seed: i32,
}

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
fn build_compat_flags_from_client_info(
    client_info: &str,
    allow_inc_recurse: bool,
) -> CompatibilityFlags {
    let mut flags = CompatibilityFlags::from_bits(0);

    // INC_RECURSE: enabled if we allow it AND client supports 'i'
    if allow_inc_recurse && client_info.contains('i') {
        flags |= CompatibilityFlags::INC_RECURSE;
    }

    // SYMLINK_TIMES: client advertises 'L'
    if client_info.contains('L') {
        flags |= CompatibilityFlags::SYMLINK_TIMES;
    }

    // SYMLINK_ICONV: client advertises 's'
    if client_info.contains('s') {
        flags |= CompatibilityFlags::SYMLINK_ICONV;
    }

    // SAFE_FILE_LIST: client advertises 'f'
    if client_info.contains('f') {
        flags |= CompatibilityFlags::SAFE_FILE_LIST;
    }

    // AVOID_XATTR_OPTIMIZATION: client advertises 'x'
    if client_info.contains('x') {
        flags |= CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;
    }

    // CHECKSUM_SEED_FIX: client advertises 'C'
    if client_info.contains('C') {
        flags |= CompatibilityFlags::CHECKSUM_SEED_FIX;
    }

    // INPLACE_PARTIAL_DIR: client advertises 'I'
    if client_info.contains('I') {
        flags |= CompatibilityFlags::INPLACE_PARTIAL_DIR;
    }

    // VARINT_FLIST_FLAGS: client advertises 'v'
    if client_info.contains('v') {
        flags |= CompatibilityFlags::VARINT_FLIST_FLAGS;
    }

    // ID0_NAMES: client advertises 'u'
    if client_info.contains('u') {
        flags |= CompatibilityFlags::ID0_NAMES;
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
    if protocol.as_u8() < 30 {
        return Ok(None);
    }

    // Parse client capabilities from -e option (mirrors upstream compat.c:712-732)
    let client_info = parse_client_info(client_args);

    // Build compat flags based on client capabilities
    // For now, hardcode allow_inc_recurse=true (should come from config)
    let our_flags = build_compat_flags_from_client_info(&client_info, true);

    // Server ONLY WRITES compat flags (upstream compat.c:736-738)
    // The client reads but DOES NOT send anything back - it's unidirectional!
    // CRITICAL: Write directly to TcpStream, NOT through any trait abstraction!
    protocol::write_varint(stream, our_flags.bits() as i32)?;

    // CRITICAL: Flush immediately to ensure data leaves application buffers
    stream.flush()?;

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
/// regardless of whether the binary protocol exchange happened.
///
/// For protocol >= 30, capability negotiation (checksum and compression algorithms) also
/// happens inside this function, matching upstream compat.c:534-585 (negotiate_the_strings).
///
/// # Arguments
///
/// * `protocol` - The negotiated protocol version
/// * `stdout` - Output stream for sending server's compatibility flags (f_out in upstream)
/// * `stdin` - Input stream for reading client's algorithm choices (f_in in upstream)
/// * `skip_compat_exchange` - Set to true if compat flags were already exchanged (daemon mode)
/// * `client_args` - Client arguments for parsing capabilities (daemon mode only)
///
/// # Returns
///
/// Returns the negotiated algorithms (or `None` for protocol < 30), or an I/O error if
/// the exchange fails.
///
/// **IMPORTANT:** Parameter order matches upstream: f_out first, f_in second!
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
) -> io::Result<SetupResult> {
    // For daemon mode, the binary 4-byte protocol exchange has already happened
    // via the @RSYNCD text protocol (upstream compat.c:599-607 checks remote_protocol != 0).
    // We skip that exchange here since our HandshakeResult already contains the negotiated protocol.

    // CRITICAL ORDER (upstream compat.c):
    // 1. Compat flags (protocol >= 30)
    // 2. Checksum seed (ALL protocols)

    // Build compat flags and perform negotiation for protocol >= 30
    // This mirrors upstream compat.c:710-743 which happens INSIDE setup_protocol()
    let (compat_flags, negotiated_algorithms) = if protocol.as_u8() >= 30 && !skip_compat_exchange {
        // Build our compat flags (server side)
        // This mirrors upstream compat.c:712-732 which builds flags from client_info string
        let our_flags = if let Some(args) = client_args {
            // Daemon mode: parse client capabilities from -e option
            let client_info = parse_client_info(args);
            build_compat_flags_from_client_info(&client_info, true)
        } else {
            // SSH mode: use default flags
            CompatibilityFlags::INC_RECURSE
                | CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS
        };

        // Server ONLY WRITES compat flags (upstream compat.c:736-738)
        // The client reads but does NOT send anything back - it's unidirectional!
        // Upstream uses write_varint() or write_byte() depending on protocol version
        protocol::write_varint(stdout, our_flags.bits() as i32)?;
        stdout.flush()?;

        // NOTE: Do NOT read anything back! The upstream code shows:
        // - When am_server=true: only write_varint is called
        // - When am_server=false: only read_varint is called
        // This is a UNIDIRECTIONAL send from server to client.

        // Protocol 30+ capability negotiation (upstream compat.c:534-585)
        // This MUST happen inside setup_protocol(), BEFORE the function returns,
        // so negotiation completes in RAW mode BEFORE multiplex activation.
        //
        // The negotiation implementation is in protocol::negotiate_capabilities(),
        // which mirrors upstream's negotiate_the_strings() function.
        //
        // Negotiation only happens if client has VARINT_FLIST_FLAGS ('v') capability.
        // This matches upstream's do_negotiated_strings check.

        // Check if client supports negotiated strings (has 'v' capability)
        let do_negotiation = our_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS);
        let algorithms = protocol::negotiate_capabilities(protocol, stdin, stdout, do_negotiation)?;

        (Some(our_flags), Some(algorithms))
    } else {
        (None, None) // Protocol < 30 uses default algorithms and no compat flags
    };

    // Send checksum seed (ALL protocols, upstream compat.c:750)
    // IMPORTANT: This comes AFTER compat flags but applies to all protocols
    let checksum_seed = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i32;
        let pid = std::process::id() as i32;
        timestamp ^ (pid << 6)
    };
    stdout.write_all(&checksum_seed.to_le_bytes())?;
    stdout.flush()?;

    Ok(SetupResult {
        negotiated_algorithms,
        compat_flags,
        checksum_seed,
    })
}
