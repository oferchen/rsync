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

/// Configuration for protocol setup.
///
/// Groups all parameters needed for protocol setup into a single struct,
/// following the Parameter Object pattern to reduce function argument count.
///
/// # Mode Configuration
///
/// The combination of `is_server` and `is_daemon_mode` controls the protocol behavior:
///
/// - `is_server=true`: Server mode - WRITE compat flags and checksum seed
/// - `is_server=false`: Client mode - READ compat flags and checksum seed
/// - `is_daemon_mode=true`: Daemon mode - unidirectional negotiation (rsync://)
/// - `is_daemon_mode=false`: SSH mode - bidirectional exchange (rsync over SSH)
#[derive(Debug)]
pub struct ProtocolSetupConfig<'a> {
    /// The negotiated protocol version.
    pub protocol: ProtocolVersion,

    /// Whether to skip compatibility flags exchange.
    ///
    /// Set to true when compat flags were already exchanged (e.g., during daemon handshake).
    pub skip_compat_exchange: bool,

    /// Client arguments for daemon mode (includes -e option with capabilities).
    ///
    /// When present, used to parse client capabilities from the `-e` option.
    /// None for SSH mode or when acting as client.
    pub client_args: Option<&'a [String]>,

    /// Whether we are the server in this connection.
    ///
    /// Controls compat flags and checksum seed direction:
    /// - `true`: Server mode - WRITE compat flags and seed
    /// - `false`: Client mode - READ compat flags and seed
    pub is_server: bool,

    /// Whether this is a daemon mode connection.
    ///
    /// Controls capability negotiation direction:
    /// - `true`: Daemon mode - unidirectional (server sends lists, client reads silently)
    /// - `false`: SSH mode - bidirectional exchange
    pub is_daemon_mode: bool,

    /// Whether compression algorithm negotiation should happen.
    ///
    /// Must match on both sides based on whether `-z` flag was passed.
    /// - `true`: Exchange compression algorithm lists
    /// - `false`: Skip compression negotiation, use defaults
    pub do_compression: bool,

    /// Optional user-specified checksum seed from `--checksum-seed=NUM`.
    ///
    /// When `Some(seed)`, the server uses this fixed seed instead of generating
    /// a random one. This makes transfers reproducible (useful for testing/debugging).
    ///
    /// When `None`, the server generates a seed from current time XOR PID
    /// (matching upstream rsync's default behavior).
    ///
    /// A value of `0` means "use current time" in upstream rsync, which is
    /// equivalent to `None` (the default random seed generation).
    pub checksum_seed: Option<u32>,

    /// Whether incremental recursion is allowed for this transfer.
    ///
    /// When `true`, the `INC_RECURSE` compat flag may be negotiated if the
    /// peer also supports it. When `false`, `INC_RECURSE` is never advertised.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `allow_inc_recurse` in upstream `compat.c:161-179`.
    /// Disabled when: `!recurse`, `use_qsort`, or receiver with
    /// `delete_before`/`delete_after`/`delay_updates`/`prune_empty_dirs`.
    pub allow_inc_recurse: bool,
}

impl<'a> ProtocolSetupConfig<'a> {
    /// Creates a new builder for `ProtocolSetupConfig` with required fields.
    ///
    /// # Arguments
    ///
    /// * `protocol` - The negotiated protocol version
    /// * `is_server` - Whether we are the server (true) or client (false)
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::ProtocolVersion;
    /// use transfer::setup::ProtocolSetupConfig;
    ///
    /// let protocol = ProtocolVersion::try_from(31).unwrap();
    /// let config = ProtocolSetupConfig::new(protocol, true)
    ///     .with_daemon_mode(true)
    ///     .with_compression(false);
    /// ```
    #[must_use]
    pub const fn new(protocol: ProtocolVersion, is_server: bool) -> Self {
        Self {
            protocol,
            skip_compat_exchange: false,
            client_args: None,
            is_server,
            is_daemon_mode: false,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        }
    }

    /// Sets whether to skip compatibility flags exchange.
    ///
    /// When true, compatibility flags have already been exchanged (e.g., during
    /// daemon handshake) and should not be exchanged again.
    ///
    /// Default: `false`
    #[must_use]
    pub const fn with_skip_compat_exchange(mut self, skip: bool) -> Self {
        self.skip_compat_exchange = skip;
        self
    }

    /// Sets the client arguments for daemon mode.
    ///
    /// Used to parse client capabilities from the `-e` option.
    /// None for SSH mode or when acting as client.
    ///
    /// Default: `None`
    #[must_use]
    pub const fn with_client_args(mut self, args: Option<&'a [String]>) -> Self {
        self.client_args = args;
        self
    }

    /// Sets whether this is a daemon mode connection.
    ///
    /// Controls capability negotiation direction:
    /// - `true`: Daemon mode - unidirectional (server sends lists, client reads silently)
    /// - `false`: SSH mode - bidirectional exchange
    ///
    /// Default: `false`
    #[must_use]
    pub const fn with_daemon_mode(mut self, is_daemon: bool) -> Self {
        self.is_daemon_mode = is_daemon;
        self
    }

    /// Sets whether compression algorithm negotiation should happen.
    ///
    /// Must match on both sides based on whether `-z` flag was passed.
    /// - `true`: Exchange compression algorithm lists
    /// - `false`: Skip compression negotiation, use defaults
    ///
    /// Default: `false`
    #[must_use]
    pub const fn with_compression(mut self, compress: bool) -> Self {
        self.do_compression = compress;
        self
    }

    /// Sets the checksum seed for reproducible transfers.
    ///
    /// When `Some(seed)`, the server uses this fixed seed instead of generating
    /// a random one. This makes transfers reproducible (useful for testing/debugging).
    ///
    /// When `None`, the server generates a seed from current time XOR PID
    /// (matching upstream rsync's default behavior).
    ///
    /// Default: `None`
    #[must_use]
    pub const fn with_checksum_seed(mut self, seed: Option<u32>) -> Self {
        self.checksum_seed = seed;
        self
    }

    /// Sets whether incremental recursion is allowed.
    ///
    /// Default: `false`
    #[must_use]
    pub const fn with_inc_recurse(mut self, allow: bool) -> Self {
        self.allow_inc_recurse = allow;
        self
    }
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
/// - 'V' - deprecated pre-release varint (implies 'v', uses `write_byte` encoding)
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

/// Builds the `-e.xxx` capability string from [`CAPABILITY_MAPPINGS`].
///
/// This is the single source of truth for which capability characters we
/// advertise. Both SSH (`invocation.rs`) and daemon (`daemon_transfer.rs`)
/// callers use this instead of hardcoding the string.
///
/// Mirrors upstream `options.c:3003-3050 maybe_add_e_option()`.
pub fn build_capability_string(allow_inc_recurse: bool) -> String {
    let mut result = String::from("-e.");
    for mapping in CAPABILITY_MAPPINGS {
        if !mapping.platform_ok {
            continue;
        }
        if mapping.requires_inc_recurse && !allow_inc_recurse {
            continue;
        }
        result.push(mapping.char);
    }
    result
}

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

/// Returns `true` when `client_info` contains the pre-release `'V'` capability.
///
/// Upstream rsync pre-release builds once used `'V'` (uppercase) instead of
/// `'v'` to advertise varint flist support. When a server detects `'V'` it
/// must implicitly set `CF_VARINT_FLIST_FLAGS` and write the compat flags
/// as a single byte (`write_byte`) rather than a varint, maintaining
/// backward compatibility with those older pre-release clients.
///
/// # Upstream reference
///
/// `compat.c:737` — `if (strchr(client_info, 'V') != NULL)`
fn client_has_pre_release_v_flag(client_info: &str) -> bool {
    client_info.contains('V')
}

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
fn write_compat_flags<W: Write + ?Sized>(
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
///
/// # Returns
///
/// Returns the final negotiated compatibility flags, or an I/O error.
pub fn exchange_compat_flags_direct(
    protocol: ProtocolVersion,
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

    // NOTE: In daemon mode, the server does NOT read anything back!
    // The upstream code shows that when am_server=true, only write_varint is called.
    // The client (am_server=false) reads the flags but doesn't send anything back.
    // This is a UNIDIRECTIONAL send from server to client.

    Ok(Some(final_flags))
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
/// * `stdout` - Output stream for sending server's compatibility flags (f_out in upstream)
/// * `stdin` - Input stream for reading client's algorithm choices (f_in in upstream)
/// * `config` - Protocol setup configuration containing all parameters
///
/// # Returns
///
/// Returns the negotiated algorithms (or `None` for protocol < 30), or an I/O error if
/// the exchange fails.
///
/// **IMPORTANT:** Parameter order matches upstream: f_out first, f_in second!
pub fn setup_protocol(
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,
    config: &ProtocolSetupConfig<'_>,
) -> io::Result<SetupResult> {
    // For daemon mode, the binary 4-byte protocol exchange has already happened
    // via the @RSYNCD text protocol (upstream compat.c:599-607 checks remote_protocol != 0).
    // We skip that exchange here since our HandshakeResult already contains the negotiated protocol.

    // CRITICAL ORDER (upstream compat.c):
    // 1. Compat flags (protocol >= 30)
    // 2. Checksum seed (ALL protocols)

    // Build compat flags and perform negotiation for protocol >= 30
    // This mirrors upstream compat.c:710-743 which happens INSIDE setup_protocol()
    let (compat_flags, negotiated_algorithms) = if config.protocol.uses_binary_negotiation()
        && !config.skip_compat_exchange
    {
        // Build our compat flags (server side)
        // This mirrors upstream compat.c:712-732 which builds flags from client_info string
        let (our_flags, client_info) = if let Some(args) = config.client_args {
            // Daemon server mode: parse client capabilities from -e option
            let client_info = parse_client_info(args);
            let flags = build_compat_flags_from_client_info(&client_info, config.allow_inc_recurse);
            (flags, Some(client_info))
        } else {
            // SSH/client mode: use default flags based on platform capabilities
            #[cfg(unix)]
            let mut flags =
                CompatibilityFlags::CHECKSUM_SEED_FIX | CompatibilityFlags::VARINT_FLIST_FLAGS;
            #[cfg(not(unix))]
            let mut flags =
                CompatibilityFlags::CHECKSUM_SEED_FIX | CompatibilityFlags::VARINT_FLIST_FLAGS;

            // Advertise symlink timestamp support on Unix platforms
            // (mirrors upstream CAN_SET_SYMLINK_TIMES at compat.c:713-714)
            #[cfg(unix)]
            {
                flags |= CompatibilityFlags::SYMLINK_TIMES;
            }

            // Advertise INC_RECURSE when incremental recursion is allowed
            if config.allow_inc_recurse {
                flags |= CompatibilityFlags::INC_RECURSE;
            }

            (flags, None)
        };

        // Compression negotiation is controlled by the `do_compression` parameter
        // which is passed from the caller based on whether -z flag was used.
        // Both sides MUST have the same value for this to work correctly.
        let send_compression = config.do_compression;

        // Compat flags exchange is UNIDIRECTIONAL (upstream compat.c:710-741):
        // - Server (am_server=true): WRITES compat flags
        // - Client (am_server=false): READS compat flags
        let compat_flags = if config.is_server {
            // Server: build and WRITE our compat flags
            // Handle pre-release 'V' flag: use single-byte write instead of varint
            // (upstream compat.c:737-741).
            let info_ref = client_info.as_deref().unwrap_or("");
            let final_flags = write_compat_flags(stdout, our_flags, info_ref)?;
            stdout.flush()?;
            final_flags
        } else {
            // Client: READ compat flags from server
            let compat_value = protocol::read_varint(stdin)?;
            let mut flags = CompatibilityFlags::from_bits(compat_value as u32);

            // Mask off INC_RECURSE if we don't support it for this transfer.
            // upstream: compat.c:720 — client clears INC_RECURSE when not allowed.
            if !config.allow_inc_recurse {
                flags &= !CompatibilityFlags::INC_RECURSE;
            }

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
        let do_negotiation = if config.is_server {
            // Server: check if client has 'v' or pre-release 'V' capability.
            // Both imply CF_VARINT_FLIST_FLAGS and enable negotiation.
            client_info.as_deref().map_or(
                our_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
                |info| info.contains('v') || client_has_pre_release_v_flag(info),
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
            config.protocol,
            stdin,
            stdout,
            do_negotiation,
            send_compression,
            config.is_daemon_mode,
            config.is_server,
        )?;

        (Some(compat_flags), Some(algorithms))
    } else {
        (None, None) // Protocol < 30 uses default algorithms and no compat flags
    };

    // Checksum seed exchange (ALL protocols, upstream compat.c:750)
    // - Server: generates and WRITES the seed
    // - Client: READS the seed from server
    //
    // --checksum-seed behavior (upstream rsync options.c:835):
    // - None: generate random seed (time ^ (pid << 6))
    // - Some(0): use current time (upstream treats 0 as "use time()")
    // - Some(N): use N as the fixed seed
    let checksum_seed = if config.is_server {
        // Server: generate or use fixed seed, then send
        let seed = match config.checksum_seed {
            Some(0) | None => {
                // Default behavior: generate from current time XOR PID
                // --checksum-seed=0 means "use current time" in upstream rsync
                use std::time::{SystemTime, UNIX_EPOCH};
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i32;
                let pid = std::process::id() as i32;
                timestamp ^ (pid << 6)
            }
            Some(fixed_seed) => {
                // --checksum-seed=NUM: use the exact seed value for reproducibility
                fixed_seed as i32
            }
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
mod tests;
