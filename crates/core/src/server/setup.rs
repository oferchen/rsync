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
        if arg.starts_with('-')
            && !arg.starts_with("--")
            && let Some(e_pos) = arg.find('e')
        {
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

    // SYMLINK_TIMES: client advertises 'L' AND server platform supports it
    // (mirrors upstream CAN_SET_SYMLINK_TIMES check at compat.c:713-714)
    #[cfg(unix)]
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
    // Disables xattr hardlink optimization (mirrors upstream compat.c:730)
    // When enabled, xattr data is transmitted even for hardlinked files
    if client_info.contains('x') {
        flags |= CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;
    }

    // CHECKSUM_SEED_FIX: client advertises 'C'
    // Ensures proper seed ordering for MD5 checksums (fully implemented)
    if client_info.contains('C') {
        flags |= CompatibilityFlags::CHECKSUM_SEED_FIX;
    }

    // INPLACE_PARTIAL_DIR: client advertises 'I'
    // Enables --inplace behavior when basis file is in partial directory
    // (mirrors upstream compat.c:732, receiver.c:797, sender.c:331)
    if client_info.contains('I') {
        flags |= CompatibilityFlags::INPLACE_PARTIAL_DIR;
    }

    // VARINT_FLIST_FLAGS: client advertises 'v'
    if client_info.contains('v') {
        flags |= CompatibilityFlags::VARINT_FLIST_FLAGS;
    }

    // ID0_NAMES: client advertises 'u'
    // Controls whether uid/gid 0 names are included in the uid/gid list
    // (mirrors upstream compat.c:734, uidlist.c:400-408)
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
///
/// The `is_server` parameter controls compat flags exchange direction:
/// - `true`: Server mode - WRITE compat flags (upstream am_server=true)
/// - `false`: Client mode - READ compat flags (upstream am_server=false)
///
/// The `is_daemon_mode` parameter controls capability negotiation direction:
/// - `true`: Daemon mode - server sends lists, client reads silently (rsync://)
/// - `false`: SSH mode - bidirectional exchange (rsync over SSH)
///
/// The `do_compression` parameter controls whether compression algorithm negotiation happens:
/// - `true`: Exchange compression algorithm lists (both sides send/receive)
/// - `false`: Skip compression negotiation, use defaults
///   This must match on both sides based on whether `-z` flag was passed.
#[allow(clippy::too_many_arguments)]
pub fn setup_protocol(
    protocol: ProtocolVersion,
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,
    skip_compat_exchange: bool,
    client_args: Option<&[String]>,
    is_server: bool,
    is_daemon_mode: bool,
    do_compression: bool,
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
        let (our_flags, client_info) = if let Some(args) = client_args {
            // Daemon server mode: parse client capabilities from -e option
            let client_info = parse_client_info(args);
            (
                build_compat_flags_from_client_info(&client_info, true),
                Some(client_info),
            )
        } else {
            // SSH/client mode: use default flags based on platform capabilities
            #[cfg(unix)]
            let mut flags = CompatibilityFlags::INC_RECURSE
                | CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS;
            #[cfg(not(unix))]
            let flags = CompatibilityFlags::INC_RECURSE
                | CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS;

            // Advertise symlink timestamp support on Unix platforms
            // (mirrors upstream CAN_SET_SYMLINK_TIMES at compat.c:713-714)
            #[cfg(unix)]
            {
                flags |= CompatibilityFlags::SYMLINK_TIMES;
            }

            (flags, None)
        };

        // Compression negotiation is controlled by the `do_compression` parameter
        // which is passed from the caller based on whether -z flag was used.
        // Both sides MUST have the same value for this to work correctly.
        let send_compression = do_compression;

        // Compat flags exchange is UNIDIRECTIONAL (upstream compat.c:710-741):
        // - Server (am_server=true): WRITES compat flags
        // - Client (am_server=false): READS compat flags
        let compat_flags = if is_server {
            // Server: build and WRITE our compat flags
            let compat_value = our_flags.bits() as i32;
            protocol::write_varint(stdout, compat_value)?;
            stdout.flush()?;
            our_flags
        } else {
            // Client: READ compat flags from server
            let compat_value = protocol::read_varint(stdin)?;
            let flags = CompatibilityFlags::from_bits(compat_value as u32);
            // Debug checkpoint: what compat flags did we receive from server?
            let _ = std::fs::write(
                "/tmp/setup_COMPAT_FLAGS_READ",
                format!(
                    "value={:#x} ({}) has_varint={} has_inc_recurse={} flags={:?}",
                    compat_value,
                    compat_value,
                    flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
                    flags.contains(CompatibilityFlags::INC_RECURSE),
                    flags
                ),
            );
            flags
        };

        // Protocol 30+ capability negotiation (upstream compat.c:534-585)
        // This MUST happen inside setup_protocol(), BEFORE the function returns,
        // so negotiation completes in RAW mode BEFORE multiplex activation.
        //
        // The negotiation implementation is in protocol::negotiate_capabilities(),
        // which mirrors upstream's negotiate_the_strings() function.
        //
        // Negotiation only happens if client has VARINT_FLIST_FLAGS ('v') capability.
        // This matches upstream's do_negotiated_strings check.

        // CRITICAL: Daemon mode and SSH mode have different negotiation flows!
        // - SSH mode: Bidirectional - both sides exchange algorithm lists
        // - Daemon mode: Unidirectional - server advertises, client selects silently
        //
        // For daemon mode, capability negotiation happens during @RSYNCD handshake,
        // NOT here in setup_protocol. The client never sends algorithm responses back
        // during setup_protocol in daemon mode.
        //
        // Upstream reference:
        // - SSH mode: negotiate_the_strings() in compat.c (bidirectional)
        // - Daemon mode: output_daemon_greeting() advertises, no response expected
        //
        // Protocol 30+ capability negotiation (upstream compat.c:534-585)
        // This is called in BOTH daemon and SSH modes.
        // The do_negotiation flag controls whether actual string exchange happens.
        //
        // CRITICAL: When acting as CLIENT (is_server=false), we must check the SERVER's
        // compat flags (compat_flags), not our own flags! Upstream compat.c:740-742:
        //   "compat_flags = read_varint(f_in);
        //    if (compat_flags & CF_VARINT_FLIST_FLAGS) do_negotiated_strings = 1;"
        let do_negotiation = if is_server {
            // Server: check if client has 'v' capability
            client_info.as_ref().map_or(
                our_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
                |info| info.contains('v'),
            )
        } else {
            // Client: check if SERVER's compat flags include VARINT_FLIST_FLAGS
            // This mirrors upstream compat.c:740-742 where client reads server's flags
            compat_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
        };

        // Daemon mode uses unidirectional negotiation (server sends, client reads silently)
        // SSH mode uses bidirectional negotiation (both sides exchange)
        // The caller tells us which mode via the is_daemon_mode parameter

        // CRITICAL: Both daemon and SSH modes need to call negotiate_capabilities when
        // do_negotiation is true (client has 'v' capability). The difference is:
        // - Daemon mode (is_daemon_mode=true): Server sends lists, client doesn't respond back
        // - SSH mode (is_daemon_mode=false): Both sides send lists, then both read each other's
        //
        // The is_daemon_mode flag inside negotiate_capabilities controls whether we read
        // the client's response after sending our lists.
        let algorithms = protocol::negotiate_capabilities(
            protocol,
            stdin,
            stdout,
            do_negotiation,
            send_compression,
            is_daemon_mode,
            is_server,
        )?;

        (Some(compat_flags), Some(algorithms))
    } else {
        (None, None) // Protocol < 30 uses default algorithms and no compat flags
    };

    // Checksum seed exchange (ALL protocols, upstream compat.c:750)
    // - Server: generates and WRITES the seed
    // - Client: READS the seed from server
    let _ = std::fs::write(
        "/tmp/setup_SEED_EXCHANGE_START",
        format!("is_server={}", is_server),
    );
    let checksum_seed = if is_server {
        // Server: generate and send seed
        let seed = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i32;
            let pid = std::process::id() as i32;
            timestamp ^ (pid << 6)
        };
        let seed_bytes = seed.to_le_bytes();
        stdout.write_all(&seed_bytes)?;
        stdout.flush()?;
        let _ = std::fs::write("/tmp/setup_SEED_WRITTEN", format!("seed={}", seed));
        seed
    } else {
        // Client: read seed from server
        let _ = std::fs::write("/tmp/setup_SEED_BEFORE_READ", "1");
        let mut seed_bytes = [0u8; 4];
        stdin.read_exact(&mut seed_bytes)?;
        let seed = i32::from_le_bytes(seed_bytes);
        let _ = std::fs::write(
            "/tmp/setup_SEED_READ",
            format!("seed={} bytes={:02x?}", seed, seed_bytes),
        );
        seed
    };

    Ok(SetupResult {
        negotiated_algorithms,
        compat_flags,
        checksum_seed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_info_extracts_capabilities_from_separate_args() {
        let args = vec!["-e".to_string(), "fxCIvu".to_string()];
        let info = parse_client_info(&args);
        assert_eq!(info, "fxCIvu");
    }

    #[test]
    fn parse_client_info_extracts_capabilities_from_combined_args() {
        let args = vec!["-efxCIvu".to_string()];
        let info = parse_client_info(&args);
        assert_eq!(info, "fxCIvu");
    }

    #[test]
    fn parse_client_info_handles_version_placeholder() {
        let args = vec!["-e.LsfxCIvu".to_string()];
        let info = parse_client_info(&args);
        assert_eq!(info, "LsfxCIvu");
    }

    #[test]
    fn parse_client_info_returns_empty_when_not_found() {
        let args = vec!["--server".to_string(), "--sender".to_string()];
        let info = parse_client_info(&args);
        assert_eq!(info, "");
    }

    #[test]
    #[cfg(unix)]
    fn build_compat_flags_enables_symlink_times_when_client_advertises_l() {
        let flags = build_compat_flags_from_client_info("LfxCIvu", true);
        assert!(
            flags.contains(CompatibilityFlags::SYMLINK_TIMES),
            "SYMLINK_TIMES should be enabled when client advertises 'L' on Unix"
        );
    }

    #[test]
    fn build_compat_flags_skips_symlink_times_when_client_missing_l() {
        let flags = build_compat_flags_from_client_info("fxCIvu", true);
        assert!(
            !flags.contains(CompatibilityFlags::SYMLINK_TIMES),
            "SYMLINK_TIMES should not be enabled when client doesn't advertise 'L'"
        );
    }

    #[test]
    fn build_compat_flags_enables_safe_file_list_when_client_advertises_f() {
        let flags = build_compat_flags_from_client_info("fxCIvu", true);
        assert!(
            flags.contains(CompatibilityFlags::SAFE_FILE_LIST),
            "SAFE_FILE_LIST should be enabled when client advertises 'f'"
        );
    }

    #[test]
    fn build_compat_flags_enables_checksum_seed_fix_when_client_advertises_c() {
        let flags = build_compat_flags_from_client_info("fxCIvu", true);
        assert!(
            flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
            "CHECKSUM_SEED_FIX should be enabled when client advertises 'C'"
        );
    }

    #[test]
    fn build_compat_flags_respects_inc_recurse_gate() {
        let flags_allowed = build_compat_flags_from_client_info("ifxCIvu", true);
        assert!(
            flags_allowed.contains(CompatibilityFlags::INC_RECURSE),
            "INC_RECURSE should be enabled when allowed and client advertises 'i'"
        );

        let flags_forbidden = build_compat_flags_from_client_info("ifxCIvu", false);
        assert!(
            !flags_forbidden.contains(CompatibilityFlags::INC_RECURSE),
            "INC_RECURSE should not be enabled when not allowed even if client advertises 'i'"
        );
    }

    #[test]
    fn build_compat_flags_enables_id0_names_when_client_advertises_u() {
        let flags = build_compat_flags_from_client_info("ufxCIv", true);
        assert!(
            flags.contains(CompatibilityFlags::ID0_NAMES),
            "ID0_NAMES should be enabled when client advertises 'u'"
        );
    }

    #[test]
    fn build_compat_flags_skips_id0_names_when_client_missing_u() {
        let flags = build_compat_flags_from_client_info("fxCIv", true);
        assert!(
            !flags.contains(CompatibilityFlags::ID0_NAMES),
            "ID0_NAMES should not be enabled when client doesn't advertise 'u'"
        );
    }

    #[test]
    fn build_compat_flags_enables_inplace_partial_dir_when_client_advertises_i_cap() {
        let flags = build_compat_flags_from_client_info("fxCIvu", true);
        assert!(
            flags.contains(CompatibilityFlags::INPLACE_PARTIAL_DIR),
            "INPLACE_PARTIAL_DIR should be enabled when client advertises 'I'"
        );
    }

    #[test]
    fn build_compat_flags_skips_inplace_partial_dir_when_client_missing_i_cap() {
        let flags = build_compat_flags_from_client_info("fxCvu", true);
        assert!(
            !flags.contains(CompatibilityFlags::INPLACE_PARTIAL_DIR),
            "INPLACE_PARTIAL_DIR should not be enabled when client doesn't advertise 'I'"
        );
    }

    #[test]
    fn build_compat_flags_enables_avoid_xattr_optimization_when_client_advertises_x() {
        let flags = build_compat_flags_from_client_info("xfCIvu", true);
        assert!(
            flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
            "AVOID_XATTR_OPTIMIZATION should be enabled when client advertises 'x'"
        );
    }

    #[test]
    fn build_compat_flags_skips_avoid_xattr_optimization_when_client_missing_x() {
        let flags = build_compat_flags_from_client_info("fCIvu", true);
        assert!(
            !flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION),
            "AVOID_XATTR_OPTIMIZATION should not be enabled when client doesn't advertise 'x'"
        );
    }
}
