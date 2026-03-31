//! Protocol setup types and configuration.
//!
//! Contains the result and configuration structs used by `setup_protocol()`.

use protocol::{CompatibilityFlags, CompressionAlgorithm, NegotiationResult, ProtocolVersion};

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

    /// Whether compression is active for this transfer.
    ///
    /// Set when the client passes `-z`, `--compress`, `--new-compress`,
    /// `--old-compress`, or `--compress-choice=ALGO`.
    pub do_compression: bool,

    /// Explicit compression algorithm from `--compress-choice=ALGO`,
    /// `--new-compress` (zlibx), or `--old-compress` (zlib).
    ///
    /// When set, compression vstring negotiation is skipped and this algorithm
    /// is used directly - matching upstream compat.c:543 which only exchanges
    /// compression vstrings when `do_compression && !compress_choice`.
    pub compress_choice: Option<CompressionAlgorithm>,

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
            compress_choice: None,
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

    /// Sets whether compression is active for this transfer.
    ///
    /// When true and `compress_choice` is None, compression vstrings are
    /// exchanged during negotiation. When true and `compress_choice` is Some,
    /// the specified algorithm is used directly without vstring exchange.
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
