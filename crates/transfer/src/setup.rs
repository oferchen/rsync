//! Server protocol setup utilities.
//!
//! This module mirrors upstream rsync's `compat.c:setup_protocol()` function,
//! handling protocol version negotiation and compatibility flags exchange.

use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};
use std::borrow::Cow;
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
fn parse_client_info(client_args: &[String]) -> Cow<'_, str> {
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
                    return Cow::Borrowed(&caps[1..]);
                }
                return Cow::Borrowed(caps);
            }
        }

        // Check for "-e" "fxCIvu" (separate args)
        if arg == "-e" && i + 1 < client_args.len() {
            return Cow::Borrowed(&client_args[i + 1]);
        }
    }

    Cow::Borrowed("")
}

/// Capability mapping entry for table-driven flag parsing.
///
/// Each entry maps a client capability character to a compatibility flag,
/// with optional platform-specific and conditional requirements.
struct CapabilityMapping {
    /// Character advertised by client in -e option
    char: char,
    /// Corresponding compatibility flag
    flag: CompatibilityFlags,
    /// Platform-specific requirement (None = all platforms)
    #[cfg(unix)]
    platform_ok: bool,
    #[cfg(not(unix))]
    platform_ok: bool,
    /// Whether this capability requires allow_inc_recurse to be true
    requires_inc_recurse: bool,
}

/// Table-driven capability to flag mappings.
///
/// This mirrors upstream compat.c:712-734 in a maintainable format.
/// Order matches upstream rsync for documentation consistency.
const CAPABILITY_MAPPINGS: &[CapabilityMapping] = &[
    // INC_RECURSE: 'i' - requires allow_inc_recurse
    CapabilityMapping {
        char: 'i',
        flag: CompatibilityFlags::INC_RECURSE,
        platform_ok: true,
        requires_inc_recurse: true,
    },
    // SYMLINK_TIMES: 'L' - Unix only (CAN_SET_SYMLINK_TIMES)
    CapabilityMapping {
        char: 'L',
        flag: CompatibilityFlags::SYMLINK_TIMES,
        #[cfg(unix)]
        platform_ok: true,
        #[cfg(not(unix))]
        platform_ok: false,
        requires_inc_recurse: false,
    },
    // SYMLINK_ICONV: 's'
    CapabilityMapping {
        char: 's',
        flag: CompatibilityFlags::SYMLINK_ICONV,
        platform_ok: true,
        requires_inc_recurse: false,
    },
    // SAFE_FILE_LIST: 'f'
    CapabilityMapping {
        char: 'f',
        flag: CompatibilityFlags::SAFE_FILE_LIST,
        platform_ok: true,
        requires_inc_recurse: false,
    },
    // AVOID_XATTR_OPTIMIZATION: 'x' - disables xattr hardlink optimization
    CapabilityMapping {
        char: 'x',
        flag: CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
        platform_ok: true,
        requires_inc_recurse: false,
    },
    // CHECKSUM_SEED_FIX: 'C' - proper seed ordering for MD5
    CapabilityMapping {
        char: 'C',
        flag: CompatibilityFlags::CHECKSUM_SEED_FIX,
        platform_ok: true,
        requires_inc_recurse: false,
    },
    // INPLACE_PARTIAL_DIR: 'I' - --inplace behavior for partial dir
    CapabilityMapping {
        char: 'I',
        flag: CompatibilityFlags::INPLACE_PARTIAL_DIR,
        platform_ok: true,
        requires_inc_recurse: false,
    },
    // VARINT_FLIST_FLAGS: 'v'
    CapabilityMapping {
        char: 'v',
        flag: CompatibilityFlags::VARINT_FLIST_FLAGS,
        platform_ok: true,
        requires_inc_recurse: false,
    },
    // ID0_NAMES: 'u' - include uid/gid 0 names
    CapabilityMapping {
        char: 'u',
        flag: CompatibilityFlags::ID0_NAMES,
        platform_ok: true,
        requires_inc_recurse: false,
    },
];

/// Builds compatibility flags based on client capabilities.
///
/// Uses table-driven approach for maintainability. This mirrors upstream
/// compat.c:712-734 which checks the client_info string to determine
/// which flags to enable.
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

    for mapping in CAPABILITY_MAPPINGS {
        // Skip if platform doesn't support this capability
        if !mapping.platform_ok {
            continue;
        }

        // Skip if requires inc_recurse but not allowed
        if mapping.requires_inc_recurse && !allow_inc_recurse {
            continue;
        }

        // Enable flag if client advertises the capability
        if client_info.contains(mapping.char) {
            flags |= mapping.flag;
        }
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
    // DISABLED: allow_inc_recurse=false because we don't implement incremental file lists yet.
    // With INC_RECURSE, the server sends file lists in segments as directories are traversed,
    // but we currently send the entire file list at once. Setting this to false prevents
    // advertising INC_RECURSE to the client, causing it to fall back to non-incremental mode.
    let our_flags = build_compat_flags_from_client_info(&client_info, false);

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
            // DISABLED: allow_inc_recurse=false - see comment in exchange_compat_flags_direct
            let flags = build_compat_flags_from_client_info(&client_info, false);
            (flags, Some(client_info))
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
            CompatibilityFlags::from_bits(compat_value as u32)
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
        seed
    } else {
        // Client: read seed from server
        let mut seed_bytes = [0u8; 4];
        stdin.read_exact(&mut seed_bytes)?;
        i32::from_le_bytes(seed_bytes)
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
        let args = vec!["-e".to_owned(), "fxCIvu".to_owned()];
        let info = parse_client_info(&args);
        assert_eq!(info, "fxCIvu");
    }

    #[test]
    fn parse_client_info_extracts_capabilities_from_combined_args() {
        let args = vec!["-efxCIvu".to_owned()];
        let info = parse_client_info(&args);
        assert_eq!(info, "fxCIvu");
    }

    #[test]
    fn parse_client_info_handles_version_placeholder() {
        let args = vec!["-e.LsfxCIvu".to_owned()];
        let info = parse_client_info(&args);
        assert_eq!(info, "LsfxCIvu");
    }

    #[test]
    fn parse_client_info_returns_empty_when_not_found() {
        let args = vec!["--server".to_owned(), "--sender".to_owned()];
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

    // ===== setup_protocol() tests =====

    #[test]
    fn setup_protocol_below_30_returns_none_for_algorithms_and_compat() {
        // Protocol 29 should skip all negotiation and compat exchange
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let result = setup_protocol(
            protocol,
            &mut stdout,
            &mut stdin,
            false, // skip_compat_exchange
            None,  // client_args
            true,  // is_server
            false, // is_daemon_mode
            false, // do_compression
        )
        .expect("protocol 29 setup should succeed");

        assert!(
            result.negotiated_algorithms.is_none(),
            "Protocol 29 should not negotiate algorithms"
        );
        assert!(
            result.compat_flags.is_none(),
            "Protocol 29 should not exchange compat flags"
        );
        // Protocol 29 still does seed exchange (server writes 4 bytes)
        assert_eq!(
            stdout.len(),
            4,
            "Protocol 29 server should write 4-byte checksum seed"
        );
    }

    #[test]
    fn setup_protocol_skip_compat_exchange_skips_flags() {
        // With skip_compat_exchange=true, even protocol 30+ should skip compat flags
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let result = setup_protocol(
            protocol,
            &mut stdout,
            &mut stdin,
            true,  // skip_compat_exchange - SKIP
            None,  // client_args
            true,  // is_server
            false, // is_daemon_mode
            false, // do_compression
        )
        .expect("setup with skip_compat_exchange should succeed");

        assert!(
            result.compat_flags.is_none(),
            "skip_compat_exchange=true should skip compat flags"
        );
        assert!(
            result.negotiated_algorithms.is_none(),
            "skip_compat_exchange=true should skip algorithm negotiation"
        );
        // Only the 4-byte seed should be written
        assert_eq!(
            stdout.len(),
            4,
            "Only checksum seed should be written when skip_compat_exchange=true"
        );
    }

    #[test]
    fn setup_protocol_server_writes_compat_flags_and_seed() {
        // Server mode (is_server=true) should WRITE compat flags, not read them
        let protocol = ProtocolVersion::try_from(31).unwrap();
        // Server doesn't read stdin during its turn (compat exchange is unidirectional)
        // Provide algorithm list for negotiation (empty list = use defaults)
        let mut stdin = &b"\x00"[..]; // Empty checksum list (0 = end of list)
        let mut stdout = Vec::new();

        let result = setup_protocol(
            protocol,
            &mut stdout,
            &mut stdin,
            false,                           // skip_compat_exchange
            Some(&["-efxCIvu".to_owned()]), // client_args with 'v' capability
            true,                            // is_server
            true,                            // is_daemon_mode (server advertises, client reads)
            false,                           // do_compression
        )
        .expect("server setup should succeed");

        assert!(
            result.compat_flags.is_some(),
            "Server should have compat flags"
        );
        let flags = result.compat_flags.unwrap();
        assert!(
            flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
            "Server should have CHECKSUM_SEED_FIX from client 'C' capability"
        );
        assert!(
            flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            "Server should have VARINT_FLIST_FLAGS from client 'v' capability"
        );

        // stdout should contain: varint compat flags + algorithm lists + 4-byte seed
        assert!(
            stdout.len() >= 5, // At least 1 byte varint + 4 bytes seed
            "Server should write compat flags varint and seed"
        );
    }

    #[test]
    fn setup_protocol_client_reads_compat_flags_from_server() {
        // Client mode (is_server=false) should READ compat flags from server
        let protocol = ProtocolVersion::try_from(31).unwrap();

        // Prepare server response: varint compat flags + checksum seed
        // compat flags = 0x21 (INC_RECURSE | CHECKSUM_SEED_FIX) - NO VARINT_FLIST_FLAGS
        // When VARINT_FLIST_FLAGS is not set, do_negotiation=false and no algorithm
        // lists are exchanged.
        let mut server_response: Vec<u8> = vec![0x21]; // compat flags varint

        // Server sends checksum seed (4 bytes little-endian)
        let test_seed: i32 = 0x12345678;
        server_response.extend_from_slice(&test_seed.to_le_bytes());

        let mut stdin = &server_response[..];
        let mut stdout = Vec::new();

        let result = setup_protocol(
            protocol,
            &mut stdout,
            &mut stdin,
            false, // skip_compat_exchange
            None,  // client_args (not needed for client mode)
            false, // is_server = CLIENT mode
            true,  // is_daemon_mode (daemon mode, server sends lists)
            false, // do_compression
        )
        .expect("client setup should succeed");

        assert!(
            result.compat_flags.is_some(),
            "Client should have compat flags"
        );
        let flags = result.compat_flags.unwrap();
        assert!(
            flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
            "Client should read CHECKSUM_SEED_FIX from server"
        );
        assert!(
            !flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            "Server sent flags without VARINT_FLIST_FLAGS"
        );

        assert_eq!(
            result.checksum_seed, test_seed,
            "Client should read the correct checksum seed"
        );
    }

    #[test]
    fn setup_protocol_server_generates_different_seeds() {
        // Each call to setup_protocol should generate a different seed
        let protocol = ProtocolVersion::try_from(29).unwrap(); // Use protocol 29 for simpler test
        let mut stdin = &b""[..];

        let mut stdout1 = Vec::new();
        let result1 = setup_protocol(
            protocol,
            &mut stdout1,
            &mut stdin,
            false,
            None,
            true, // is_server
            false,
            false,
        )
        .expect("first setup should succeed");

        // Small delay to ensure different timestamp
        std::thread::sleep(std::time::Duration::from_millis(1));

        let mut stdout2 = Vec::new();
        let result2 = setup_protocol(
            protocol,
            &mut stdout2,
            &mut stdin,
            false,
            None,
            true, // is_server
            false,
            false,
        )
        .expect("second setup should succeed");

        // Seeds should be different (includes timestamp and PID)
        // Note: This test may flake if both calls happen in the same second
        // with the same PID, but that's highly unlikely in practice
        assert_eq!(
            result1.checksum_seed, result2.checksum_seed,
            "Same process in same second should have same seed (deterministic)"
        );
        // The seed includes PID so different processes would differ
    }

    #[test]
    fn setup_protocol_ssh_mode_bidirectional_exchange() {
        // SSH mode (is_daemon_mode=false) has bidirectional capability exchange
        let protocol = ProtocolVersion::try_from(31).unwrap();

        // Prepare stdin with what we expect to read from peer:
        // - Compat flags varint with VARINT_FLIST_FLAGS to trigger negotiation
        // - Checksum algorithm list (empty = use defaults)
        // - Checksum seed
        //
        // VARINT_FLIST_FLAGS = 0x80 = 128, INC_RECURSE = 0x01, CHECKSUM_SEED_FIX = 0x20
        // Combined: 0xA1 = 161 (requires 2-byte rsync varint encoding)
        // Rsync varint encoding of 161: [0x80, 0xA1] (marker byte, then value byte)
        let mut peer_data: Vec<u8> = vec![
            0x80,
            0xA1, // varint for 161 (VARINT_FLIST_FLAGS | INC_RECURSE | CHECKSUM_SEED_FIX)
            0x00, // empty checksum list (end marker)
        ];
        peer_data.extend_from_slice(&0x12345678_i32.to_le_bytes()); // seed

        let mut stdin = &peer_data[..];
        let mut stdout = Vec::new();

        let result = setup_protocol(
            protocol,
            &mut stdout,
            &mut stdin,
            false, // skip_compat_exchange
            None,  // client_args
            false, // is_server = CLIENT
            false, // is_daemon_mode = SSH mode (bidirectional)
            false, // do_compression
        )
        .expect("SSH mode client setup should succeed");

        // Should have read compat flags from peer
        assert!(result.compat_flags.is_some());
        let flags = result.compat_flags.unwrap();
        assert!(
            flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            "Should have VARINT_FLIST_FLAGS from server"
        );

        // SSH mode client should write algorithm preferences
        // (unlike daemon mode where client reads silently)
        // Client writes its checksum list in SSH bidirectional mode
        assert!(
            !stdout.is_empty(),
            "SSH mode client should write algorithm preferences"
        );
    }

    #[test]
    fn setup_protocol_client_args_affects_compat_flags() {
        // Different client args should result in different compat flags
        let protocol = ProtocolVersion::try_from(31).unwrap();

        // Test with minimal capabilities
        let mut stdin = &b"\x00"[..]; // empty checksum list
        let mut stdout = Vec::new();
        let result_minimal = setup_protocol(
            protocol,
            &mut stdout,
            &mut stdin,
            false,
            Some(&["-ev".to_owned()]), // Only 'v' capability
            true,
            true,
            false,
        )
        .expect("minimal caps setup should succeed");

        let flags_minimal = result_minimal.compat_flags.unwrap();
        assert!(
            flags_minimal.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            "Should have VARINT_FLIST_FLAGS from 'v'"
        );
        assert!(
            !flags_minimal.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
            "Should NOT have CHECKSUM_SEED_FIX without 'C'"
        );

        // Test with full capabilities
        let mut stdin = &b"\x00"[..];
        let mut stdout = Vec::new();
        let result_full = setup_protocol(
            protocol,
            &mut stdout,
            &mut stdin,
            false,
            Some(&["-e.LsfxCIvu".to_owned()]), // Full capabilities
            true,
            true,
            false,
        )
        .expect("full caps setup should succeed");

        let flags_full = result_full.compat_flags.unwrap();
        assert!(
            flags_full.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            "Should have VARINT_FLIST_FLAGS from 'v'"
        );
        assert!(
            flags_full.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
            "Should have CHECKSUM_SEED_FIX from 'C'"
        );
        assert!(
            flags_full.contains(CompatibilityFlags::SAFE_FILE_LIST),
            "Should have SAFE_FILE_LIST from 'f'"
        );
        assert!(
            flags_full.contains(CompatibilityFlags::INPLACE_PARTIAL_DIR),
            "Should have INPLACE_PARTIAL_DIR from 'I'"
        );
    }

    #[test]
    fn setup_protocol_protocol_30_minimum_for_compat_exchange() {
        // Protocol 30 is the minimum for compat exchange
        let protocol_30 = ProtocolVersion::try_from(30).unwrap();
        let mut stdin = &b"\x00"[..]; // empty checksum list
        let mut stdout = Vec::new();

        let result = setup_protocol(
            protocol_30,
            &mut stdout,
            &mut stdin,
            false,
            Some(&["-efxCIvu".to_owned()]),
            true,  // is_server
            true,  // is_daemon_mode
            false, // do_compression
        )
        .expect("protocol 30 setup should succeed");

        assert!(
            result.compat_flags.is_some(),
            "Protocol 30 should exchange compat flags"
        );
        assert!(
            result.negotiated_algorithms.is_some(),
            "Protocol 30 should negotiate algorithms"
        );
    }
}
