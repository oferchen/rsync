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

        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: None,
            is_server: true,
            is_daemon_mode: false,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
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

        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: true, // SKIP
            client_args: None,
            is_server: true,
            is_daemon_mode: false,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
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

        let client_args = ["-efxCIvu".to_owned()];
        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: Some(&client_args), // client_args with 'v' capability
            is_server: true,
            is_daemon_mode: true, // server advertises, client reads
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result =
            setup_protocol(&mut stdout, &mut stdin, &config).expect("server setup should succeed");

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

        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: None, // not needed for client mode
            is_server: false,  // CLIENT mode
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result =
            setup_protocol(&mut stdout, &mut stdin, &config).expect("client setup should succeed");

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
    fn setup_protocol_server_generates_deterministic_seeds() {
        // Same process within the same second produces the same seed
        // (seed = time_secs ^ (pid << 6)), so rapid calls are deterministic
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];

        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: None,
            is_server: true,
            is_daemon_mode: false,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let mut stdout1 = Vec::new();
        let result1 =
            setup_protocol(&mut stdout1, &mut stdin, &config).expect("first setup should succeed");

        let mut stdout2 = Vec::new();
        let result2 =
            setup_protocol(&mut stdout2, &mut stdin, &config).expect("second setup should succeed");

        assert_eq!(
            result1.checksum_seed, result2.checksum_seed,
            "Same process in same second should have same seed (deterministic)"
        );
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

        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: None,
            is_server: false, // CLIENT
            is_daemon_mode: false,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
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

        let client_args_minimal = ["-ev".to_owned()];
        let config_minimal = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: Some(&client_args_minimal), // Only 'v' capability
            is_server: true,
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result_minimal = setup_protocol(&mut stdout, &mut stdin, &config_minimal)
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

        let client_args_full = ["-e.LsfxCIvu".to_owned()];
        let config_full = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: Some(&client_args_full), // Full capabilities
            is_server: true,
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result_full = setup_protocol(&mut stdout, &mut stdin, &config_full)
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

        let client_args = ["-efxCIvu".to_owned()];
        let config = ProtocolSetupConfig {
            protocol: protocol_30,
            skip_compat_exchange: false,
            client_args: Some(&client_args),
            is_server: true,
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
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

    // ===== ProtocolSetupConfig builder tests =====

    #[test]
    fn protocol_setup_config_builder_new_sets_defaults() {
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let config = ProtocolSetupConfig::new(protocol, true);

        assert_eq!(config.protocol.as_u8(), 31);
        assert!(config.is_server);
        assert!(!config.skip_compat_exchange);
        assert!(config.client_args.is_none());
        assert!(!config.is_daemon_mode);
        assert!(!config.do_compression);
        assert!(config.checksum_seed.is_none());
    }

    #[test]
    fn protocol_setup_config_builder_chain_methods() {
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let client_args = vec!["-efxCIvu".to_owned()];

        let config = ProtocolSetupConfig::new(protocol, true)
            .with_skip_compat_exchange(true)
            .with_client_args(Some(&client_args))
            .with_daemon_mode(true)
            .with_compression(true)
            .with_checksum_seed(Some(12345));

        assert!(config.skip_compat_exchange);
        assert!(config.client_args.is_some());
        assert!(config.is_daemon_mode);
        assert!(config.do_compression);
        assert_eq!(config.checksum_seed, Some(12345));
    }

    #[test]
    fn protocol_setup_config_builder_partial_configuration() {
        let protocol = ProtocolVersion::try_from(30).unwrap();

        // Only set some optional fields
        let config = ProtocolSetupConfig::new(protocol, false)
            .with_daemon_mode(true)
            .with_compression(false);

        assert!(!config.is_server);
        assert!(config.is_daemon_mode);
        assert!(!config.do_compression);
        // Other fields should still be at defaults
        assert!(!config.skip_compat_exchange);
        assert!(config.client_args.is_none());
        assert!(config.checksum_seed.is_none());
    }

    #[test]
    fn protocol_setup_config_builder_can_override_values() {
        let protocol = ProtocolVersion::try_from(31).unwrap();

        let config = ProtocolSetupConfig::new(protocol, true)
            .with_compression(true)
            .with_compression(false); // Override previous value

        assert!(!config.do_compression, "Last value should win");
    }

    #[test]
    fn protocol_setup_config_builder_works_in_real_setup() {
        // Verify builder works in actual setup_protocol call
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let config = ProtocolSetupConfig::new(protocol, true)
            .with_compression(false)
            .with_daemon_mode(false);

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
            .expect("setup should succeed with builder config");

        assert!(
            result.negotiated_algorithms.is_none(),
            "Protocol 29 should not negotiate"
        );
    }

    // ===== Checksum seed edge case tests (task #99) =====

    #[test]
    fn setup_protocol_server_uses_fixed_checksum_seed() {
        // --checksum-seed=12345 should use exactly 12345 as the seed
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(12345));

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
            .expect("setup with fixed seed should succeed");

        assert_eq!(
            result.checksum_seed, 12345_i32,
            "Fixed seed value should be used as-is"
        );

        // Verify the seed was written to stdout as 4-byte LE
        assert_eq!(stdout.len(), 4, "Should write 4-byte seed");
        let written_seed = i32::from_le_bytes(stdout[..4].try_into().unwrap());
        assert_eq!(written_seed, 12345, "Written seed should match fixed value");
    }

    #[test]
    fn setup_protocol_server_fixed_seed_is_deterministic() {
        // Same fixed seed should produce same result every time
        let protocol = ProtocolVersion::try_from(29).unwrap();

        let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(42));

        let mut stdout1 = Vec::new();
        let mut stdin1 = &b""[..];
        let result1 =
            setup_protocol(&mut stdout1, &mut stdin1, &config).expect("first setup should succeed");

        let mut stdout2 = Vec::new();
        let mut stdin2 = &b""[..];
        let result2 = setup_protocol(&mut stdout2, &mut stdin2, &config)
            .expect("second setup should succeed");

        assert_eq!(
            result1.checksum_seed, result2.checksum_seed,
            "Fixed seed should be deterministic across calls"
        );
        assert_eq!(
            stdout1, stdout2,
            "Wire bytes should be identical for same fixed seed"
        );
    }

    #[test]
    fn setup_protocol_server_seed_zero_uses_time_based_generation() {
        // --checksum-seed=0 is treated like None: generate from time XOR PID
        // This matches upstream rsync's behavior where 0 means "use current time"
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(0));

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
            .expect("setup with seed=0 should succeed");

        // Seed=0 generates time-based seed, which should be non-zero in practice
        // (unless time and PID happen to XOR to 0, extremely unlikely)
        // We just verify it succeeded and produced a 4-byte seed
        assert_eq!(stdout.len(), 4, "Should write 4-byte seed");
        // Note: we cannot assert the exact value since it's time-dependent
        let _ = result.checksum_seed; // Just verify we got a value
    }

    #[test]
    fn setup_protocol_server_max_u32_seed() {
        // --checksum-seed=4294967295 (u32::MAX) should work
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let mut stdin = &b""[..];
        let mut stdout = Vec::new();

        let config = ProtocolSetupConfig::new(protocol, true).with_checksum_seed(Some(u32::MAX));

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
            .expect("setup with max seed should succeed");

        // u32::MAX as i32 is -1
        assert_eq!(
            result.checksum_seed,
            u32::MAX as i32,
            "u32::MAX seed should be transmitted as i32 (-1)"
        );

        let written_seed = i32::from_le_bytes(stdout[..4].try_into().unwrap());
        assert_eq!(
            written_seed, -1_i32,
            "Wire representation of u32::MAX is -1 as i32"
        );
    }

    #[test]
    fn setup_protocol_client_reads_fixed_seed_from_server() {
        // Client reads exact seed bytes sent by server
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let test_seed: i32 = 99999;
        let seed_bytes = test_seed.to_le_bytes();

        let mut stdin = &seed_bytes[..];
        let mut stdout = Vec::new();

        let config = ProtocolSetupConfig::new(protocol, false); // CLIENT mode

        let result =
            setup_protocol(&mut stdout, &mut stdin, &config).expect("client setup should succeed");

        assert_eq!(
            result.checksum_seed, test_seed,
            "Client should read exact seed value from server"
        );
    }

    #[test]
    fn setup_protocol_client_reads_negative_seed_from_server() {
        // Server may send a negative i32 seed (from u32::MAX cast)
        let protocol = ProtocolVersion::try_from(29).unwrap();
        let test_seed: i32 = -1; // This is what u32::MAX becomes
        let seed_bytes = test_seed.to_le_bytes();

        let mut stdin = &seed_bytes[..];
        let mut stdout = Vec::new();

        let config = ProtocolSetupConfig::new(protocol, false);

        let result = setup_protocol(&mut stdout, &mut stdin, &config)
            .expect("client setup with negative seed should succeed");

        assert_eq!(
            result.checksum_seed, -1,
            "Client should correctly read negative seed value"
        );
    }

    // ===== Pre-release 'V' compat flag tests =====

    #[test]
    fn client_has_pre_release_v_flag_detects_uppercase_v() {
        assert!(
            client_has_pre_release_v_flag("LsfxCIVu"),
            "'V' in capability string should be detected"
        );
    }

    #[test]
    fn client_has_pre_release_v_flag_ignores_lowercase_v() {
        assert!(
            !client_has_pre_release_v_flag("LsfxCIvu"),
            "lowercase 'v' should not trigger pre-release detection"
        );
    }

    #[test]
    fn client_has_pre_release_v_flag_empty_string() {
        assert!(
            !client_has_pre_release_v_flag(""),
            "empty string should not match"
        );
    }

    #[test]
    fn write_compat_flags_uses_single_byte_for_pre_release_v() {
        // When 'V' is present, compat flags should be written as a single byte
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::CHECKSUM_SEED_FIX;
        let mut output = Vec::new();

        let final_flags =
            write_compat_flags(&mut output, flags, "LsfxCIVu").expect("write should succeed");

        // Should be exactly 1 byte (write_byte, not write_varint)
        assert_eq!(
            output.len(),
            1,
            "pre-release 'V' should use single-byte encoding"
        );

        // CF_VARINT_FLIST_FLAGS should be implicitly set
        assert!(
            final_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            "V flag should implicitly enable VARINT_FLIST_FLAGS"
        );

        // The byte should include all original flags plus VARINT_FLIST_FLAGS
        let expected_bits = flags.bits() | CompatibilityFlags::VARINT_FLIST_FLAGS.bits();
        assert_eq!(
            output[0], expected_bits as u8,
            "written byte should contain original flags plus VARINT_FLIST_FLAGS"
        );
    }

    #[test]
    fn write_compat_flags_uses_varint_for_lowercase_v() {
        // When only 'v' is present (no 'V'), use standard varint encoding
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::CHECKSUM_SEED_FIX;
        let mut output = Vec::new();

        let final_flags =
            write_compat_flags(&mut output, flags, "LsfxCIvu").expect("write should succeed");

        // Should NOT implicitly add VARINT_FLIST_FLAGS (that's done by
        // build_compat_flags_from_client_info)
        assert_eq!(
            final_flags, flags,
            "lowercase 'v' should not implicitly modify flags"
        );

        // Verify varint encoding: 0x28 = SAFE_FILE_LIST(0x08) | CHECKSUM_SEED_FIX(0x20)
        // For values < 128, varint is a single byte matching the value
        assert_eq!(
            output,
            vec![0x28],
            "varint encoding of 0x28 should be [0x28]"
        );
    }

    #[test]
    fn write_compat_flags_v_flag_single_byte_without_varint_readable() {
        // When 'V' is present but the total flags value stays below 0x80
        // (i.e. VARINT_FLIST_FLAGS is not yet set by 'v'), the single-byte
        // encoding IS compatible with read_varint because the 0x80 bit is off.
        // However, write_compat_flags always sets CF_VARINT_FLIST_FLAGS (0x80)
        // for 'V', making the high bit on. A pre-release 'V' client reads
        // with read_byte(), not read_varint(), so this is by design.
        // upstream: "read_varint() is compatible with the older write_byte()
        // when the 0x80 bit isn't on."
        let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::CHECKSUM_SEED_FIX;
        let mut output = Vec::new();

        let written_flags =
            write_compat_flags(&mut output, flags, "LsfxCIVu").expect("write should succeed");

        // The value has CF_VARINT_FLIST_FLAGS set (0x80 bit on), so
        // read_varint would interpret this as a multi-byte varint and fail.
        // This is correct because pre-release 'V' clients use read_byte().
        assert_eq!(output.len(), 1, "should be single byte");
        assert_eq!(
            output[0],
            written_flags.bits() as u8,
            "byte should match the flag bits"
        );
        assert!(
            output[0] & 0x80 != 0,
            "high bit is set due to CF_VARINT_FLIST_FLAGS"
        );
    }

    #[test]
    fn write_compat_flags_v_flag_low_value_readable_by_read_varint() {
        // When write_compat_flags produces a value < 0x80, read_varint can
        // decode it since the high bit is off and a varint with no extra
        // bytes is identical to a plain byte.
        // Use empty flags so the only bit is VARINT_FLIST_FLAGS from 'V'.
        // CF_VARINT_FLIST_FLAGS itself is 0x80 so this will always set the
        // high bit. We verify this invariant.
        let flags = CompatibilityFlags::EMPTY;
        let mut output = Vec::new();

        let written_flags =
            write_compat_flags(&mut output, flags, "V").expect("write should succeed");

        // Even with EMPTY input, 'V' sets CF_VARINT_FLIST_FLAGS (0x80)
        assert_eq!(written_flags.bits(), 0x80);
        assert_eq!(output, vec![0x80]);
    }

    #[test]
    fn setup_protocol_server_v_flag_uses_single_byte_encoding() {
        // Server with 'V' capability client should use single-byte compat flags encoding
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let mut stdin = &b"\x00"[..]; // empty checksum list
        let mut stdout = Vec::new();

        // Client has 'V' (pre-release) instead of 'v'
        let client_args = ["-e.LsfxCIVu".to_owned()];
        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: Some(&client_args),
            is_server: true,
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result =
            setup_protocol(&mut stdout, &mut stdin, &config).expect("server setup should succeed");

        let flags = result.compat_flags.unwrap();
        assert!(
            flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS),
            "'V' should implicitly set VARINT_FLIST_FLAGS"
        );

        // First byte of output should be the compat flags (single byte encoding)
        // Remaining bytes are algorithm negotiation data + checksum seed
        assert!(
            !stdout.is_empty(),
            "Server should have written compat flags"
        );

        // The first byte is the compat flags in single-byte format
        let compat_byte = stdout[0];
        assert_eq!(
            compat_byte & CompatibilityFlags::VARINT_FLIST_FLAGS.bits() as u8,
            CompatibilityFlags::VARINT_FLIST_FLAGS.bits() as u8,
            "First byte should contain VARINT_FLIST_FLAGS"
        );
    }

    #[test]
    fn setup_protocol_server_v_flag_enables_negotiation() {
        // 'V' should trigger algorithm negotiation just like 'v' does
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let mut stdin = &b"\x00"[..]; // empty checksum list
        let mut stdout = Vec::new();

        let client_args = ["-e.LsfxCIVu".to_owned()];
        let config = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: Some(&client_args),
            is_server: true,
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: None,
            allow_inc_recurse: false,
        };

        let result =
            setup_protocol(&mut stdout, &mut stdin, &config).expect("server setup should succeed");

        assert!(
            result.negotiated_algorithms.is_some(),
            "'V' should enable algorithm negotiation"
        );
    }

    #[test]
    fn setup_protocol_server_v_flag_with_both_v_and_uppercase_v() {
        // If both 'v' and 'V' are present, 'V' takes precedence for encoding
        let protocol = ProtocolVersion::try_from(31).unwrap();
        let mut stdin = &b"\x00"[..];
        let mut stdout_v = Vec::new();
        let mut stdout_both = Vec::new();

        // Only uppercase V
        let client_args_v = ["-e.LsfxCIVu".to_owned()];
        let config_v = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: Some(&client_args_v),
            is_server: true,
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: Some(42),
            allow_inc_recurse: false,
        };

        let result_v = setup_protocol(&mut stdout_v, &mut stdin, &config_v)
            .expect("V-only setup should succeed");

        // Both v and V
        let mut stdin = &b"\x00"[..];
        let client_args_both = ["-e.LsfxCIvVu".to_owned()];
        let config_both = ProtocolSetupConfig {
            protocol,
            skip_compat_exchange: false,
            client_args: Some(&client_args_both),
            is_server: true,
            is_daemon_mode: true,
            do_compression: false,
            checksum_seed: Some(42),
            allow_inc_recurse: false,
        };

        let result_both = setup_protocol(&mut stdout_both, &mut stdin, &config_both)
            .expect("vV setup should succeed");

        // Both should have VARINT_FLIST_FLAGS set
        assert!(
            result_v
                .compat_flags
                .unwrap()
                .contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
        );
        assert!(
            result_both
                .compat_flags
                .unwrap()
                .contains(CompatibilityFlags::VARINT_FLIST_FLAGS)
        );

        // Both should use single-byte encoding since 'V' is present in both
        assert_eq!(
            stdout_v[0], stdout_both[0],
            "Both should produce the same compat flags byte"
        );
    }
}
