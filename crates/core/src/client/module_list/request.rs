//! Module list request parsing from CLI operands.
//!
//! Parses `rsync://host/` URLs and `host::` double-colon syntax into a
//! [`ModuleListRequest`] that can be executed against a daemon. The parsing
//! logic mirrors upstream `main.c:check_for_hostspec()`.

use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;

use protocol::ProtocolVersion;

use super::super::{AddressMode, ClientError, TcpFastOpenMode};
use super::parsing::{parse_host_port, split_daemon_host_module, strip_prefix_ignore_ascii_case};
use super::types::DaemonAddress;
#[cfg(feature = "quic")]
use super::types::Transport;

/// Specification describing a daemon module listing request parsed from CLI operands.
///
/// The request retains the optional username embedded in the operand so future
/// authentication flows can reuse the caller-supplied identity even though the
/// current module listing implementation performs anonymous queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListRequest {
    pub(super) address: DaemonAddress,
    pub(super) username: Option<String>,
    pub(super) protocol: ProtocolVersion,
}

impl ModuleListRequest {
    /// Default TCP port used by rsync daemons when a port is not specified.
    pub const DEFAULT_PORT: u16 = 873;

    /// Attempts to derive a module listing request from CLI-style operands.
    pub fn from_operands(operands: &[OsString]) -> Result<Option<Self>, ClientError> {
        Self::from_operands_with_port(operands, Self::DEFAULT_PORT)
    }

    /// Equivalent to [`Self::from_operands`] but allows overriding the default
    /// daemon port.
    pub fn from_operands_with_port(
        operands: &[OsString],
        default_port: u16,
    ) -> Result<Option<Self>, ClientError> {
        if operands.len() != 1 {
            return Ok(None);
        }

        Self::from_operand(&operands[0], default_port)
    }

    fn from_operand(operand: &OsString, default_port: u16) -> Result<Option<Self>, ClientError> {
        let text = operand.to_string_lossy();

        // The `quic://` scheme (oc extension, `quic` feature only) is parsed
        // exactly beside `rsync://`; it selects the QUIC transport but reuses
        // the identical authority/module grammar. Recognised only when the
        // feature is compiled in - a default build treats `quic://...` as an
        // ordinary (non-daemon) operand, so the scheme is absent when off.
        #[cfg(feature = "quic")]
        if let Some(rest) = strip_prefix_ignore_ascii_case(&text, "quic://") {
            return Self::parse_daemon_url(rest, default_port, Transport::Quic);
        }

        if let Some(rest) = strip_prefix_ignore_ascii_case(&text, "rsync://") {
            return Self::parse_daemon_url(rest, default_port, super::types::Transport::default());
        }

        if let Some((host_part, module_part)) = split_daemon_host_module(&text)? {
            if module_part.is_empty() {
                let target = parse_host_port(host_part, default_port)?;
                return Ok(Some(Self::new(target.address, target.username)));
            }
            return Ok(None);
        }

        Ok(None)
    }

    /// Parses the authority (`[user@]host[:port]`) of a daemon URL after its
    /// scheme prefix has been stripped, tagging the resulting address with the
    /// scheme's [`Transport`]. A non-empty path segment is not a module listing
    /// (that is a transfer target), so it yields `Ok(None)`.
    fn parse_daemon_url(
        rest: &str,
        default_port: u16,
        transport: super::types::Transport,
    ) -> Result<Option<Self>, ClientError> {
        let mut parts = rest.splitn(2, '/');
        let host_port = parts.next().unwrap_or("");
        let remainder = parts.next();

        if remainder.is_some_and(|path| !path.is_empty()) {
            return Ok(None);
        }

        let target = parse_host_port(host_port, default_port)?;
        Ok(Some(Self::new(
            target.address.with_transport(transport),
            target.username,
        )))
    }

    const fn new(address: DaemonAddress, username: Option<String>) -> Self {
        Self {
            address,
            username,
            protocol: ProtocolVersion::NEWEST,
        }
    }

    #[cfg(test)]
    #[allow(dead_code)] // Test utility constructor
    pub(crate) const fn from_components(
        address: DaemonAddress,
        username: Option<String>,
        protocol: ProtocolVersion,
    ) -> Self {
        Self {
            address,
            username,
            protocol,
        }
    }

    /// Returns the parsed daemon address.
    #[must_use]
    pub const fn address(&self) -> &DaemonAddress {
        &self.address
    }

    /// Returns the optional username supplied in the daemon URL or legacy syntax.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// Returns the desired protocol version for daemon negotiation.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns a new request that clamps the negotiation to the provided protocol.
    #[must_use]
    pub const fn with_protocol(mut self, protocol: ProtocolVersion) -> Self {
        self.protocol = protocol;
        self
    }

    /// Returns a new request whose daemon address carries the given transport.
    ///
    /// The `--quic` modifier upgrades an otherwise ordinary `rsync://` /
    /// `host::` daemon target to QUIC without touching its syntax (design:
    /// `docs/design/quic-transport-policy.md`, Decision D). A `quic://` target
    /// already carries [`Transport::Quic`] from the scheme, so re-applying it
    /// is idempotent.
    #[cfg(feature = "quic")]
    #[must_use]
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.address = self.address.with_transport(transport);
        self
    }
}

/// Configuration toggles that influence daemon module listings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListOptions {
    suppress_motd: bool,
    address_mode: AddressMode,
    connect_program: Option<OsString>,
    bind_address: Option<SocketAddr>,
    sockopts: Option<OsString>,
    tcp_fastopen: TcpFastOpenMode,
    blocking_io: Option<bool>,
    remote_shell: Option<Vec<OsString>>,
    rsync_path: Option<OsString>,
    /// `--quic-ca <PATH>` private CA bundle for verifying a QUIC daemon's
    /// certificate (policy B). `None` uses the system-roots default. Only
    /// present under the `quic` feature.
    #[cfg(feature = "quic")]
    quic_ca: Option<std::path::PathBuf>,
    /// `--quic-cert` client certificate chain (PEM) presented to a QUIC daemon
    /// for mutual TLS during a listing. `None` presents no client certificate.
    /// Only present under the `quic` feature.
    #[cfg(feature = "quic")]
    quic_cert: Option<std::path::PathBuf>,
    /// `--quic-key` private key (PEM) for the `--quic-cert` certificate. Only
    /// present under the `quic` feature.
    #[cfg(feature = "quic")]
    quic_key: Option<std::path::PathBuf>,
    /// `--quic-cc` client endpoint congestion controller for QUIC listings.
    /// `None` defers to `OC_RSYNC_QUIC_CC` then the default. Only present under
    /// the `quic` feature.
    #[cfg(feature = "quic")]
    quic_cc: Option<rsync_io::quic::CongestionAlgorithm>,
    /// `--quic-window` client endpoint flow-control window (bytes) for QUIC
    /// listings. `None` defers to `OC_RSYNC_QUIC_WINDOW` then the default. Only
    /// present under the `quic` feature.
    #[cfg(feature = "quic")]
    quic_window: Option<u64>,
    /// `--quic-cipher` cipher-suite family override for QUIC listings. `None`
    /// keeps the CPU-adaptive default. Only present under the `quic` feature.
    #[cfg(feature = "quic")]
    quic_cipher: Option<rsync_io::quic::QuicCipher>,
}

impl ModuleListOptions {
    /// Creates a new options structure with all toggles disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            suppress_motd: false,
            address_mode: AddressMode::Default,
            connect_program: None,
            bind_address: None,
            sockopts: None,
            tcp_fastopen: TcpFastOpenMode::Auto,
            blocking_io: None,
            remote_shell: None,
            rsync_path: None,
            #[cfg(feature = "quic")]
            quic_ca: None,
            #[cfg(feature = "quic")]
            quic_cert: None,
            #[cfg(feature = "quic")]
            quic_key: None,
            #[cfg(feature = "quic")]
            quic_cc: None,
            #[cfg(feature = "quic")]
            quic_window: None,
            #[cfg(feature = "quic")]
            quic_cipher: None,
        }
    }

    /// Supplies the `--quic-ca` private CA bundle path for QUIC listings.
    ///
    /// `None` (the default) verifies the daemon's QUIC certificate against the
    /// platform trust store. Available only under the `quic` feature.
    #[cfg(feature = "quic")]
    #[must_use]
    #[doc(alias = "--quic-ca")]
    pub fn with_quic_ca(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.quic_ca = path;
        self
    }

    /// Returns the configured `--quic-ca` private CA bundle path, if any.
    #[cfg(feature = "quic")]
    #[must_use]
    pub fn quic_ca(&self) -> Option<&std::path::Path> {
        self.quic_ca.as_deref()
    }

    /// Supplies the `--quic-cert` / `--quic-key` client certificate for a QUIC
    /// listing's mutual-TLS handshake.
    ///
    /// Both `None` (the default) presents no client certificate. The pair
    /// travels together as one config shape. Available only under the `quic`
    /// feature.
    #[cfg(feature = "quic")]
    #[must_use]
    #[doc(alias = "--quic-cert")]
    #[doc(alias = "--quic-key")]
    pub fn with_quic_client_cert(
        mut self,
        cert: Option<std::path::PathBuf>,
        key: Option<std::path::PathBuf>,
    ) -> Self {
        self.quic_cert = cert;
        self.quic_key = key;
        self
    }

    /// Returns the configured `--quic-cert` client certificate chain path, if
    /// any.
    #[cfg(feature = "quic")]
    #[must_use]
    pub fn quic_cert(&self) -> Option<&std::path::Path> {
        self.quic_cert.as_deref()
    }

    /// Returns the configured `--quic-key` client private-key path, if any.
    #[cfg(feature = "quic")]
    #[must_use]
    pub fn quic_key(&self) -> Option<&std::path::Path> {
        self.quic_key.as_deref()
    }

    /// Supplies the `--quic-cc` congestion controller for QUIC listings.
    ///
    /// `None` (the default) defers to `OC_RSYNC_QUIC_CC` then the built-in
    /// default (BBR). Available only under the `quic` feature.
    #[cfg(feature = "quic")]
    #[must_use]
    #[doc(alias = "--quic-cc")]
    pub const fn with_quic_cc(mut self, cc: Option<rsync_io::quic::CongestionAlgorithm>) -> Self {
        self.quic_cc = cc;
        self
    }

    /// Returns the configured `--quic-cc` congestion controller, if any.
    #[cfg(feature = "quic")]
    #[must_use]
    pub const fn quic_cc(&self) -> Option<rsync_io::quic::CongestionAlgorithm> {
        self.quic_cc
    }

    /// Supplies the `--quic-window` flow-control window (bytes) for QUIC
    /// listings.
    ///
    /// `None` (the default) defers to `OC_RSYNC_QUIC_WINDOW` then the built-in
    /// default. Available only under the `quic` feature.
    #[cfg(feature = "quic")]
    #[must_use]
    #[doc(alias = "--quic-window")]
    pub const fn with_quic_window(mut self, bytes: Option<u64>) -> Self {
        self.quic_window = bytes;
        self
    }

    /// Returns the configured `--quic-window` flow-control window in bytes, if
    /// any.
    #[cfg(feature = "quic")]
    #[must_use]
    pub const fn quic_window(&self) -> Option<u64> {
        self.quic_window
    }

    /// Supplies the `--quic-cipher` cipher-suite family override for QUIC
    /// listings.
    ///
    /// `None` (the default) keeps the CPU-adaptive default. Available only
    /// under the `quic` feature.
    #[cfg(feature = "quic")]
    #[must_use]
    #[doc(alias = "--quic-cipher")]
    pub const fn with_quic_cipher(mut self, cipher: Option<rsync_io::quic::QuicCipher>) -> Self {
        self.quic_cipher = cipher;
        self
    }

    /// Returns the configured `--quic-cipher` cipher-suite family, if any.
    #[cfg(feature = "quic")]
    #[must_use]
    pub const fn quic_cipher(&self) -> Option<rsync_io::quic::QuicCipher> {
        self.quic_cipher
    }

    /// Returns a new configuration that suppresses daemon MOTD lines.
    #[must_use]
    pub const fn suppress_motd(mut self, suppress: bool) -> Self {
        self.suppress_motd = suppress;
        self
    }

    /// Returns whether MOTD lines should be suppressed.
    #[must_use]
    pub const fn suppresses_motd(&self) -> bool {
        self.suppress_motd
    }

    /// Configures the preferred address family for the daemon connection.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn with_address_mode(mut self, mode: AddressMode) -> Self {
        self.address_mode = mode;
        self
    }

    /// Returns the preferred address family.
    #[must_use]
    pub const fn address_mode(&self) -> AddressMode {
        self.address_mode
    }

    /// Supplies an explicit connect program command.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn with_connect_program(mut self, program: Option<OsString>) -> Self {
        self.connect_program = program;
        self
    }

    /// Returns the configured connect program command, if any.
    pub fn connect_program(&self) -> Option<&std::ffi::OsStr> {
        self.connect_program.as_deref()
    }

    /// Supplies the `-e`/`--rsh` remote-shell argv for daemon-over-rsh listings.
    ///
    /// When present, `host::` listings reach the daemon by spawning this
    /// program with `rsync --server --daemon .` instead of opening TCP,
    /// mirroring upstream `main.c`'s daemon-over-rsh dispatch.
    #[must_use]
    #[doc(alias = "--rsh")]
    pub fn with_remote_shell(mut self, shell: Option<Vec<OsString>>) -> Self {
        self.remote_shell = shell;
        self
    }

    /// Returns the configured remote-shell argv, if any.
    pub fn remote_shell(&self) -> Option<&[OsString]> {
        self.remote_shell.as_deref()
    }

    /// Supplies the `--rsync-path` override for daemon-over-rsh listings.
    #[must_use]
    #[doc(alias = "--rsync-path")]
    pub fn with_rsync_path(mut self, rsync_path: Option<OsString>) -> Self {
        self.rsync_path = rsync_path;
        self
    }

    /// Returns the configured remote `--rsync-path`, if any.
    pub fn rsync_path(&self) -> Option<&OsStr> {
        self.rsync_path.as_deref()
    }

    /// Configures additional socket options that should be applied to daemon connections.
    #[must_use]
    #[doc(alias = "--sockopts")]
    pub fn with_sockopts(mut self, sockopts: Option<OsString>) -> Self {
        self.sockopts = sockopts;
        self
    }

    /// Returns the configured socket options, if any.
    pub fn sockopts(&self) -> Option<&OsStr> {
        self.sockopts.as_deref()
    }

    /// Configures the TCP Fast Open mode applied to the daemon socket.
    #[must_use]
    #[doc(alias = "--tcp-fastopen")]
    pub const fn with_tcp_fastopen(mut self, mode: TcpFastOpenMode) -> Self {
        self.tcp_fastopen = mode;
        self
    }

    /// Returns the configured TCP Fast Open mode.
    #[must_use]
    pub const fn tcp_fastopen(&self) -> TcpFastOpenMode {
        self.tcp_fastopen
    }

    /// Configures the desired blocking I/O mode for daemon TCP sockets.
    #[must_use]
    #[doc(alias = "--blocking-io")]
    #[doc(alias = "--no-blocking-io")]
    pub const fn with_blocking_io(mut self, blocking: Option<bool>) -> Self {
        self.blocking_io = blocking;
        self
    }

    /// Returns the configured blocking I/O preference, if any.
    pub const fn blocking_io(&self) -> Option<bool> {
        self.blocking_io
    }

    /// Configures the bind address used when contacting the daemon directly or via a proxy.
    #[must_use]
    pub const fn with_bind_address(mut self, address: Option<SocketAddr>) -> Self {
        self.bind_address = address;
        self
    }

    /// Returns the configured bind address, if any.
    pub const fn bind_address(&self) -> Option<SocketAddr> {
        self.bind_address
    }
}

impl Default for ModuleListOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod transport_tests {
    use super::super::types::Transport;
    use super::*;

    fn operands(target: &str) -> Vec<OsString> {
        vec![OsString::from(target)]
    }

    #[test]
    fn rsync_scheme_selects_tcp_transport() {
        // WHY: the default daemon scheme must stay TCP so a default build - and
        // a `quic`-enabled build that was not asked for QUIC - behaves exactly
        // like upstream.
        let request = ModuleListRequest::from_operands(&operands("rsync://host/"))
            .expect("parse")
            .expect("daemon target");
        assert_eq!(request.address().transport(), Transport::Tcp);
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn double_colon_selects_tcp_transport() {
        let request = ModuleListRequest::from_operands(&operands("host::"))
            .expect("parse")
            .expect("daemon target");
        assert_eq!(request.address().transport(), Transport::Tcp);
    }

    #[cfg(feature = "quic")]
    #[test]
    fn quic_scheme_selects_quic_transport_default_port() {
        // WHY: `quic://` is parsed beside `rsync://` and yields the QUIC
        // transport with the shared default daemon port 873 (873/udp; policy D).
        let request = ModuleListRequest::from_operands(&operands("quic://host/"))
            .expect("parse")
            .expect("daemon target");
        assert_eq!(request.address().transport(), Transport::Quic);
        assert_eq!(request.address().port(), 873);
    }

    #[cfg(feature = "quic")]
    #[test]
    fn quic_scheme_honours_explicit_port() {
        // WHY: an explicit `:port` in the authority overrides the 873 default,
        // exactly as it does for `rsync://`.
        let request = ModuleListRequest::from_operands(&operands("quic://host:1234/"))
            .expect("parse")
            .expect("daemon target");
        assert_eq!(request.address().transport(), Transport::Quic);
        assert_eq!(request.address().port(), 1234);
    }

    #[cfg(feature = "quic")]
    #[test]
    fn quic_scheme_honours_port_override() {
        // WHY: `--port` (threaded as the default port) overrides 873 when the
        // authority omits an explicit port.
        let request = ModuleListRequest::from_operands_with_port(&operands("quic://host/"), 9999)
            .expect("parse")
            .expect("daemon target");
        assert_eq!(request.address().port(), 9999);
        assert_eq!(request.address().transport(), Transport::Quic);
    }

    #[cfg(feature = "quic")]
    #[test]
    fn with_transport_upgrades_double_colon_to_quic() {
        // WHY: the `--quic` modifier upgrades an ordinary `host::` target to
        // QUIC without changing its syntax (QUIC-8c).
        let request = ModuleListRequest::from_operands(&operands("host::"))
            .expect("parse")
            .expect("daemon target")
            .with_transport(Transport::Quic);
        assert_eq!(request.address().transport(), Transport::Quic);
    }

    #[cfg(not(feature = "quic"))]
    #[test]
    fn quic_scheme_absent_when_feature_off() {
        // WHY: with the feature compiled out the `quic://` scheme must be
        // absent - not accepted as a daemon target - so no code path can select
        // an unbuilt transport.
        let parsed = ModuleListRequest::from_operands(&operands("quic://host/")).expect("parse");
        assert!(
            parsed.is_none(),
            "quic:// must not parse as a daemon target"
        );
    }
}
