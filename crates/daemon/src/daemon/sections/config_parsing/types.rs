//! Configuration parsing types.
//!
//! Core data structures used during rsyncd.conf parsing - the directive origin
//! tracker and the parsed result container.

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
    global_bandwidth_limit: Option<(BandwidthLimitComponents, ConfigDirectiveOrigin)>,
    global_secrets_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    global_incoming_chmod: Option<(String, ConfigDirectiveOrigin)>,
    global_outgoing_chmod: Option<(String, ConfigDirectiveOrigin)>,
    syslog_facility: Option<(String, ConfigDirectiveOrigin)>,
    syslog_tag: Option<(String, ConfigDirectiveOrigin)>,
    /// Global bind address from the `address` directive.
    ///
    /// upstream: loadparm.c - `bind address` / `address` parameter sets the
    /// interface the daemon listens on.
    bind_address: Option<(IpAddr, ConfigDirectiveOrigin)>,
    /// Daemon-level uid from the global section.
    ///
    /// upstream: loadparm.c - `uid` in the global section sets the daemon process uid.
    /// The value is a username string or numeric uid that gets resolved at runtime.
    daemon_uid: Option<(String, ConfigDirectiveOrigin)>,
    /// Daemon-level gid from the global section.
    ///
    /// upstream: loadparm.c - `gid` in the global section sets the daemon process gid.
    /// The value is a groupname string or numeric gid that gets resolved at runtime.
    daemon_gid: Option<(String, ConfigDirectiveOrigin)>,
    listen_backlog: Option<(u32, ConfigDirectiveOrigin)>,
    /// Global socket options from the `socket options` directive.
    ///
    /// upstream: daemon-parm.txt - `socket options` STRING. Comma-separated list
    /// of TCP/IP socket options applied to the daemon listener socket via
    /// `set_socket_options()` in `socket.c`.
    socket_options: Option<(String, ConfigDirectiveOrigin)>,
    /// Whether incoming connections require a PROXY protocol header (V1 or V2).
    ///
    /// upstream: daemon-parm.h - `proxy_protocol` BOOL, P_GLOBAL, default False.
    proxy_protocol: Option<(bool, ConfigDirectiveOrigin)>,
    /// TCP port the daemon listens on.
    /// upstream: daemon-parm.txt - `port` INTEGER, P_GLOBAL, default 0.
    rsync_port: Option<(u16, ConfigDirectiveOrigin)>,
    /// Directory the daemon chroots into before forking children.
    ///
    /// upstream: daemon-parm.h - `daemon chroot` STRING, P_GLOBAL.
    daemon_chroot: Option<(PathBuf, ConfigDirectiveOrigin)>,
}
