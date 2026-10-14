// Configuration parsing types.
//
// Core data structures used during rsyncd.conf parsing - the directive origin
// tracker and the parsed result container.

/// Tracks the source location of a configuration directive for error reporting.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ConfigDirectiveOrigin {
    /// Path to the configuration file containing this directive.
    path: PathBuf,
    /// Line number (1-indexed) where the directive appears.
    line: usize,
}

/// Parsed result of an `rsyncd.conf` file, containing module definitions and
/// global settings extracted from the configuration.
#[derive(Debug)]
pub(crate) struct ParsedConfigModules {
    modules: Vec<ModuleDefinition>,
    global_refuse_options: Vec<(Vec<String>, ConfigDirectiveOrigin)>,
    motd_lines: Vec<String>,
    pid_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    reverse_lookup: Option<(bool, ConfigDirectiveOrigin)>,
    lock_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// Global `log file` path. Opens the daemon-wide log at startup; see the
    /// field of the same name on the parse state for the upstream contract.
    log_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// Daemon-wide `timeout`. Bounds the pre-module handshake phase; see the
    /// field of the same name on the parse state for the upstream contract.
    daemon_timeout: Option<(Option<NonZeroU64>, ConfigDirectiveOrigin)>,
    /// QUIC listener certificate path from the `quic cert file` global
    /// directive (oc extension, feature-gated). The paired private key lives in
    /// `quic_key_file`. Identity is per-listener, so both are global-only.
    #[cfg(feature = "quic")]
    quic_cert_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// QUIC client-auth CA path from the `quic client ca file` global directive.
    /// When set, the QUIC listener requires and verifies a client certificate
    /// against this CA bundle (mutual TLS). Per-listener, so global-only.
    #[cfg(feature = "quic")]
    quic_client_ca_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// QUIC listener private-key path from the `quic key file` global directive.
    #[cfg(feature = "quic")]
    quic_key_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// QUIC listener port from the `quic port` global directive (oc extension,
    /// feature-gated). `None` means the QUIC listener shares the daemon TCP
    /// `port`; a value overrides it independently. A `quic port = 0` is coerced
    /// to 873 at parse time, mirroring the TCP `port = 0` handling.
    #[cfg(feature = "quic")]
    quic_port: Option<(u16, ConfigDirectiveOrigin)>,
    global_secrets_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    global_incoming_chmod: Option<(String, ConfigDirectiveOrigin)>,
    global_outgoing_chmod: Option<(String, ConfigDirectiveOrigin)>,
    syslog_facility: Option<(String, ConfigDirectiveOrigin)>,
    syslog_tag: Option<(String, ConfigDirectiveOrigin)>,
    /// Global bind address from the `address` directive.
    ///
    /// upstream: daemon-parm.txt:3 - `bind_address|address` STRING, public name
    /// `address`; clientserver.c:1786 `daemon_main()` binds the listener to it.
    bind_address: Option<(IpAddr, ConfigDirectiveOrigin)>,
    /// Process-wide daemon uid from the `daemon uid` global directive.
    ///
    /// upstream: daemon-parm.txt:6 `daemon_uid`; clientserver.c:1509-1522
    /// `start_daemon()` applies `lp_daemon_uid()` per connection. Deviation from
    /// upstream: oc applies it once in the listener before the accept loop.
    /// Distinct from the P_LOCAL `uid` module default. The value is a username
    /// string or numeric uid resolved at runtime.
    daemon_uid: Option<(String, ConfigDirectiveOrigin)>,
    /// Process-wide daemon gid from the `daemon gid` global directive.
    ///
    /// upstream: daemon-parm.txt:5 `daemon_gid`; clientserver.c:1496-1508
    /// `start_daemon()` applies `lp_daemon_gid()` per connection. Distinct
    /// from the P_LOCAL `gid` module default. The
    /// value is a groupname string or numeric gid resolved at runtime.
    daemon_gid: Option<(String, ConfigDirectiveOrigin)>,
    listen_backlog: Option<(u32, ConfigDirectiveOrigin)>,
    /// Global socket options from the `socket options` directive.
    ///
    /// upstream: daemon-parm.txt:10 - `socket options` STRING. Comma-separated
    /// list of TCP/IP socket options applied to the daemon listener socket via
    /// `set_socket_options()` (socket.c:610, socket.c:837).
    socket_options: Option<(String, ConfigDirectiveOrigin)>,
    /// Whether incoming connections require a PROXY protocol header (V1 or V2).
    ///
    /// upstream: daemon-parm.txt:15 - `proxy_protocol` BOOL, P_GLOBAL, default False.
    proxy_protocol: Option<(bool, ConfigDirectiveOrigin)>,
    /// Addresses whose PROXY protocol header the daemon is willing to believe.
    ///
    /// upstream: daemon-parm.txt:9 - `proxy_protocol_hosts` STRING, P_GLOBAL,
    /// default NULL (read back as ""). Read by `allow_proxy_protocol_peer()` (access.c:311), which
    /// returns 0 for an empty list, so an unset directive rejects every peer.
    proxy_protocol_hosts: Option<(Vec<HostPattern>, ConfigDirectiveOrigin)>,
    /// TCP port the daemon listens on.
    /// upstream: daemon-parm.txt:13 - `rsync_port|port` INTEGER, P_GLOBAL, default 0.
    rsync_port: Option<(u16, ConfigDirectiveOrigin)>,
    /// Directory the daemon chroots into before forking children.
    ///
    /// upstream: daemon-parm.txt:4 - `daemon chroot` STRING, P_GLOBAL, applied
    /// per connection by `start_daemon()` (clientserver.c:1470-1495).
    /// Deviation from upstream: oc applies it once in the listener before the
    /// accept loop.
    daemon_chroot: Option<(PathBuf, ConfigDirectiveOrigin)>,
}
