//! Global and per-module configuration data types.

use std::path::{Path, PathBuf};

/// Global configuration parameters from the top of `rsyncd.conf`.
///
/// These parameters appear before any `[module]` section and control
/// daemon-wide behaviour such as bind address, port, logging, and privilege
/// dropping. Upstream: `loadparm.c` global parameter table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlobalConfig {
    pub(crate) port: u16,
    pub(crate) address: Option<String>,
    pub(crate) motd_file: Option<PathBuf>,
    pub(crate) log_file: Option<PathBuf>,
    pub(crate) pid_file: Option<PathBuf>,
    pub(crate) socket_options: Option<String>,
    pub(crate) log_format: Option<String>,
    pub(crate) syslog_facility: Option<String>,
    pub(crate) syslog_tag: Option<String>,
    /// Daemon process uid - username or numeric uid string.
    ///
    /// upstream: loadparm.c - `uid` in the global section controls what user
    /// the daemon process drops to after binding.
    pub(crate) uid: Option<String>,
    /// Daemon process gid - groupname or numeric gid string.
    ///
    /// upstream: loadparm.c - `gid` in the global section controls what group
    /// the daemon process drops to after binding.
    pub(crate) gid: Option<String>,
    pub(crate) listen_backlog: Option<u32>,
    /// Whether incoming connections must start with a PROXY protocol header.
    ///
    /// upstream: daemon-parm.h - `proxy_protocol` BOOL, P_GLOBAL, default False.
    pub(crate) proxy_protocol: bool,
    /// Directory the daemon chroots into before forking children.
    ///
    /// upstream: daemon-parm.h - `daemon chroot` STRING, P_GLOBAL.
    pub(crate) daemon_chroot: Option<PathBuf>,
    /// Path to the PEM-encoded TLS certificate chain file.
    ///
    /// upstream: stunnel-era `ssl cert` global directive. Must be set together
    /// with `ssl_key`.
    #[cfg(feature = "daemon-tls")]
    pub(crate) ssl_cert: Option<PathBuf>,
    /// Path to the PEM-encoded TLS private key file.
    ///
    /// upstream: stunnel-era `ssl key` global directive. Must be set together
    /// with `ssl_cert`.
    #[cfg(feature = "daemon-tls")]
    pub(crate) ssl_key: Option<PathBuf>,
    /// Path to the PEM-encoded CA certificate file for client verification.
    ///
    /// upstream: stunnel-era `ssl ca` global directive. Optional - when absent,
    /// client certificates are not requested.
    #[cfg(feature = "daemon-tls")]
    pub(crate) ssl_ca: Option<PathBuf>,
}

impl GlobalConfig {
    /// Returns the daemon port (default: 873).
    pub fn port(&self) -> u16 {
        if self.port == 0 { 873 } else { self.port }
    }

    /// Returns the bind address, if specified.
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// Returns the MOTD file path, if specified.
    pub fn motd_file(&self) -> Option<&Path> {
        self.motd_file.as_deref()
    }

    /// Returns the log file path, if specified.
    pub fn log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Returns the PID file path, if specified.
    pub fn pid_file(&self) -> Option<&Path> {
        self.pid_file.as_deref()
    }

    /// Returns socket options string, if specified.
    pub fn socket_options(&self) -> Option<&str> {
        self.socket_options.as_deref()
    }

    /// Returns the log format string, if specified.
    pub fn log_format(&self) -> Option<&str> {
        self.log_format.as_deref()
    }

    /// Returns the syslog facility name (default: "daemon").
    ///
    /// Upstream: `loadparm.c` - `syslog facility` parameter. Valid values include
    /// "daemon", "auth", "user", "local0" through "local7", etc.
    pub fn syslog_facility(&self) -> &str {
        self.syslog_facility.as_deref().unwrap_or("daemon")
    }

    /// Returns the syslog tag/ident prefix (default: "oc-rsyncd").
    ///
    /// Upstream: `loadparm.c` - `syslog tag` parameter, default "rsyncd".
    /// For oc-rsync the default is "oc-rsyncd".
    pub fn syslog_tag(&self) -> &str {
        self.syslog_tag.as_deref().unwrap_or("oc-rsyncd")
    }

    /// Returns the daemon process uid string, if specified.
    ///
    /// The value may be a username or numeric uid. Resolution to a numeric ID
    /// happens at daemon startup time via platform APIs.
    pub fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    /// Returns the daemon process gid string, if specified.
    ///
    /// The value may be a groupname or numeric gid. Resolution to a numeric ID
    /// happens at daemon startup time via platform APIs.
    pub fn gid(&self) -> Option<&str> {
        self.gid.as_deref()
    }

    /// Returns the TCP listen backlog, if configured.
    ///
    /// Upstream: `daemon-parm.txt` - `listen_backlog` INTEGER (upstream default 5,
    /// oc-rsync default 128). Controls the backlog argument passed to `listen(2)`
    /// on the daemon socket.
    pub fn listen_backlog(&self) -> Option<u32> {
        self.listen_backlog
    }

    /// Returns whether incoming connections require a PROXY protocol header.
    ///
    /// upstream: clientserver.c:1298 - `if (lp_proxy_protocol() && !read_proxy_protocol_header(f_in))`
    pub fn proxy_protocol(&self) -> bool {
        self.proxy_protocol
    }

    /// Returns the directory the daemon chroots into before forking children.
    ///
    /// upstream: daemon-parm.h - `daemon chroot` STRING, P_GLOBAL.
    pub fn daemon_chroot(&self) -> Option<&Path> {
        self.daemon_chroot.as_deref()
    }

    /// Returns the path to the PEM-encoded TLS certificate chain file, if configured.
    ///
    /// upstream: stunnel-era `ssl cert` global directive. When set, `ssl_key`
    /// must also be present.
    #[cfg(feature = "daemon-tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "daemon-tls")))]
    pub fn ssl_cert(&self) -> Option<&Path> {
        self.ssl_cert.as_deref()
    }

    /// Returns the path to the PEM-encoded TLS private key file, if configured.
    ///
    /// upstream: stunnel-era `ssl key` global directive. When set, `ssl_cert`
    /// must also be present.
    #[cfg(feature = "daemon-tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "daemon-tls")))]
    pub fn ssl_key(&self) -> Option<&Path> {
        self.ssl_key.as_deref()
    }

    /// Returns the path to the PEM-encoded CA certificate file for client
    /// verification, if configured.
    ///
    /// upstream: stunnel-era `ssl ca` global directive. Optional - only needed
    /// for mutual TLS.
    #[cfg(feature = "daemon-tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "daemon-tls")))]
    pub fn ssl_ca(&self) -> Option<&Path> {
        self.ssl_ca.as_deref()
    }

    /// Builds a `TlsConfig` from the parsed global
    /// TLS directives, if both `ssl cert` and `ssl key` are configured.
    ///
    /// Returns `None` when TLS is not configured (neither directive is set).
    /// The caller receives a ready-to-use `TlsConfig` that can be passed
    /// directly to `build_tls_acceptor`.
    #[cfg(feature = "daemon-tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "daemon-tls")))]
    pub fn tls_config(&self) -> Option<crate::tls::TlsConfig> {
        let cert_path = self.ssl_cert.clone()?;
        let key_path = self.ssl_key.clone()?;
        Some(crate::tls::TlsConfig {
            cert_path,
            key_path,
            client_ca_path: self.ssl_ca.clone(),
        })
    }
}

/// Per-module configuration parameters from a `[name]` section in `rsyncd.conf`.
///
/// Each module represents a directory tree that clients can access. Modules
/// control access (auth users, hosts allow/deny), chroot behaviour, transfer
/// options, and pre/post-transfer exec hooks.
/// Upstream: `loadparm.c` local parameter table.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleConfig {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) comment: Option<String>,
    pub(crate) read_only: bool,
    pub(crate) write_only: bool,
    pub(crate) list: bool,
    pub(crate) uid: Option<String>,
    pub(crate) gid: Option<String>,
    pub(crate) max_connections: u32,
    pub(crate) lock_file: Option<PathBuf>,
    pub(crate) auth_users: Vec<String>,
    pub(crate) secrets_file: Option<PathBuf>,
    pub(crate) hosts_allow: Vec<String>,
    pub(crate) hosts_deny: Vec<String>,
    pub(crate) exclude: Vec<String>,
    pub(crate) include: Vec<String>,
    pub(crate) filter: Vec<String>,
    pub(crate) exclude_from: Option<PathBuf>,
    pub(crate) include_from: Option<PathBuf>,
    pub(crate) incoming_chmod: Option<String>,
    pub(crate) outgoing_chmod: Option<String>,
    pub(crate) timeout: Option<u32>,
    pub(crate) max_verbosity: i32,
    pub(crate) use_chroot: bool,
    pub(crate) numeric_ids: bool,
    pub(crate) fake_super: bool,
    pub(crate) transfer_logging: bool,
    pub(crate) refuse_options: Vec<String>,
    pub(crate) dont_compress: Vec<String>,
    pub(crate) early_exec: Option<String>,
    pub(crate) pre_xfer_exec: Option<String>,
    pub(crate) post_xfer_exec: Option<String>,
    pub(crate) name_converter: Option<String>,
    pub(crate) strict_modes: bool,
    pub(crate) open_noatime: bool,
    /// Character set for filename conversion.
    ///
    /// upstream: daemon-parm.txt - `charset` STRING, P_LOCAL, default NULL.
    pub(crate) charset: Option<String>,
    /// Temporary directory for partial transfers.
    ///
    /// upstream: daemon-parm.txt - `temp_dir` PATH, P_LOCAL, default NULL.
    pub(crate) temp_dir: Option<PathBuf>,
    /// Whether to perform DNS forward lookups for connecting hosts (default: true).
    ///
    /// upstream: daemon-parm.txt - `forward_lookup` BOOL, P_LOCAL, default True.
    pub(crate) forward_lookup: bool,
    /// Whether to perform DNS reverse lookups for connecting hosts (default: true).
    ///
    /// upstream: daemon-parm.txt - `reverse_lookup` BOOL, P_LOCAL, default True.
    pub(crate) reverse_lookup: bool,
    /// Whether to ignore I/O errors during delete operations (default: false).
    ///
    /// upstream: daemon-parm.txt - `ignore_errors` BOOL, P_LOCAL, default False.
    pub(crate) ignore_errors: bool,
    /// Whether to skip files that are not readable (default: false).
    ///
    /// upstream: daemon-parm.txt - `ignore_nonreadable` BOOL, P_LOCAL, default False.
    pub(crate) ignore_nonreadable: bool,
    /// Whether to mangle symlinks for safety (tri-state: None = unset).
    ///
    /// upstream: daemon-parm.txt - `munge_symlinks` BOOL3, P_LOCAL, default Unset.
    pub(crate) munge_symlinks: Option<bool>,
    /// Per-module log file path override.
    ///
    /// upstream: daemon-parm.txt - `log_file` STRING, P_LOCAL, default NULL.
    pub(crate) module_log_file: Option<PathBuf>,
    /// Per-module log format string override.
    ///
    /// upstream: daemon-parm.txt - `log_format` STRING, P_LOCAL,
    /// default "%o %h [%a] %m (%u) %f %l".
    pub(crate) module_log_format: Option<String>,
    /// Per-module syslog facility override.
    ///
    /// upstream: daemon-parm.txt - `syslog_facility` ENUM, P_LOCAL, default LOG_DAEMON.
    pub(crate) module_syslog_facility: Option<String>,
    /// Per-module syslog tag override.
    ///
    /// upstream: daemon-parm.txt - `syslog_tag` STRING, P_LOCAL, default "rsyncd".
    pub(crate) module_syslog_tag: Option<String>,
}

impl ModuleConfig {
    /// Returns the module name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the filesystem path this module serves (required).
    ///
    /// Upstream: `loadparm.c` - `path` parameter. Must be an absolute path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the module comment, if specified.
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    /// Returns whether the module is read-only (default: true).
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Returns whether the module is write-only (default: false).
    pub fn write_only(&self) -> bool {
        self.write_only
    }

    /// Returns whether the module is listable (default: true).
    pub fn list(&self) -> bool {
        self.list
    }

    /// Returns the UID to run as, if specified.
    pub fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }

    /// Returns the GID to run as, if specified.
    pub fn gid(&self) -> Option<&str> {
        self.gid.as_deref()
    }

    /// Returns the maximum number of connections (0 = unlimited).
    pub fn max_connections(&self) -> u32 {
        self.max_connections
    }

    /// Returns the lock file path for this module, if specified.
    pub fn lock_file(&self) -> Option<&Path> {
        self.lock_file.as_deref()
    }

    /// Returns the list of authorized users for challenge-response authentication.
    ///
    /// When non-empty, clients must authenticate before accessing the module.
    /// Upstream: `loadparm.c` - `auth users` parameter.
    pub fn auth_users(&self) -> &[String] {
        &self.auth_users
    }

    /// Returns the path to the secrets file for password lookup, if specified.
    ///
    /// Upstream: `loadparm.c` - `secrets file` parameter. Must have mode 0600.
    pub fn secrets_file(&self) -> Option<&Path> {
        self.secrets_file.as_deref()
    }

    /// Returns the list of allowed host patterns (glob or CIDR).
    ///
    /// When non-empty, only matching hosts may connect.
    /// Upstream: `loadparm.c` - `hosts allow` parameter.
    pub fn hosts_allow(&self) -> &[String] {
        &self.hosts_allow
    }

    /// Returns the list of denied host patterns (glob or CIDR).
    ///
    /// Matching hosts are rejected before authentication.
    /// Upstream: `loadparm.c` - `hosts deny` parameter.
    pub fn hosts_deny(&self) -> &[String] {
        &self.hosts_deny
    }

    /// Returns the list of exclude patterns.
    pub fn exclude(&self) -> &[String] {
        &self.exclude
    }

    /// Returns the list of include patterns.
    pub fn include(&self) -> &[String] {
        &self.include
    }

    /// Returns the list of server-side filter rules applied before the transfer.
    ///
    /// These rules restrict which paths the client can access, independent of
    /// any client-side filters. Upstream: `loadparm.c` - `filter` parameter.
    pub fn filter(&self) -> &[String] {
        &self.filter
    }

    /// Returns the path to a file containing exclude patterns, if specified.
    ///
    /// Upstream: `loadparm.c` - `exclude from` parameter.
    pub fn exclude_from(&self) -> Option<&Path> {
        self.exclude_from.as_deref()
    }

    /// Returns the path to a file containing include patterns, if specified.
    ///
    /// Upstream: `loadparm.c` - `include from` parameter.
    pub fn include_from(&self) -> Option<&Path> {
        self.include_from.as_deref()
    }

    /// Returns the incoming chmod specification, if configured.
    ///
    /// Applied to files received by the daemon (push transfers).
    /// Upstream: `loadparm.c` - `incoming chmod` parameter.
    pub fn incoming_chmod(&self) -> Option<&str> {
        self.incoming_chmod.as_deref()
    }

    /// Returns the outgoing chmod specification, if configured.
    ///
    /// Applied to files sent by the daemon (pull transfers).
    /// Upstream: `loadparm.c` - `outgoing chmod` parameter.
    pub fn outgoing_chmod(&self) -> Option<&str> {
        self.outgoing_chmod.as_deref()
    }

    /// Returns the I/O timeout in seconds, if specified.
    pub fn timeout(&self) -> Option<u32> {
        self.timeout
    }

    /// Returns the maximum verbosity level a client can request (default: 1).
    ///
    /// Caps the client's requested `-v` count to prevent excessive server-side
    /// output. Upstream: `loadparm.c` - `max verbosity` parameter.
    pub fn max_verbosity(&self) -> i32 {
        self.max_verbosity
    }

    /// Returns whether the daemon chroots into the module path (default: true).
    ///
    /// Chroot isolates each module to its own filesystem subtree.
    /// Upstream: `loadparm.c` - `use chroot` parameter.
    pub fn use_chroot(&self) -> bool {
        self.use_chroot
    }

    /// Returns whether to skip name-to-ID mapping and use raw numeric UIDs/GIDs (default: false).
    ///
    /// Upstream: `loadparm.c` - `numeric ids` parameter.
    pub fn numeric_ids(&self) -> bool {
        self.numeric_ids
    }

    /// Returns whether `--fake-super` is forced for this module (default: false).
    ///
    /// When enabled, the daemon stores ownership and special-file metadata in
    /// extended attributes instead of requiring root privileges.
    /// Upstream: `loadparm.c` - `fake super` parameter.
    pub fn fake_super(&self) -> bool {
        self.fake_super
    }

    /// Returns whether transfer logging is enabled (default: false).
    pub fn transfer_logging(&self) -> bool {
        self.transfer_logging
    }

    /// Returns the list of client options the daemon refuses for this module.
    ///
    /// Any client request containing a refused option is rejected before
    /// the transfer starts. Upstream: `loadparm.c` - `refuse options` parameter.
    pub fn refuse_options(&self) -> &[String] {
        &self.refuse_options
    }

    /// Returns file-suffix patterns that should not be delta-compressed.
    ///
    /// Files matching these patterns (e.g., `*.gz`, `*.jpg`) are already
    /// compressed and skip the zlib/zstd stage. Upstream: `loadparm.c` -
    /// `dont compress` parameter.
    pub fn dont_compress(&self) -> &[String] {
        &self.dont_compress
    }

    /// Returns the early exec command, if specified.
    ///
    /// Runs early in the connection, before file list exchange.
    /// Upstream: `loadparm.c` - `early exec` parameter.
    pub fn early_exec(&self) -> Option<&str> {
        self.early_exec.as_deref()
    }

    /// Returns the pre-transfer command, if specified.
    pub fn pre_xfer_exec(&self) -> Option<&str> {
        self.pre_xfer_exec.as_deref()
    }

    /// Returns the post-transfer command, if specified.
    pub fn post_xfer_exec(&self) -> Option<&str> {
        self.post_xfer_exec.as_deref()
    }

    /// Returns the name converter program for user/group name-to-ID mapping, if specified.
    ///
    /// Used when the daemon runs in a chroot where `/etc/passwd` is unavailable.
    /// Upstream: `loadparm.c` - `name converter` parameter.
    pub fn name_converter(&self) -> Option<&str> {
        self.name_converter.as_deref()
    }

    /// Returns whether strict permission checks on the secrets file are enabled (default: true).
    ///
    /// When true, the daemon verifies that the secrets file is not world-readable.
    /// Upstream: `loadparm.c` - `strict modes` parameter, default true.
    pub fn strict_modes(&self) -> bool {
        self.strict_modes
    }

    /// Returns whether source files should be opened with `O_NOATIME` (default: false).
    ///
    /// Only effective on Linux. On other platforms this is a no-op.
    /// Upstream: `loadparm.c` - `open noatime` parameter, default false.
    pub fn open_noatime(&self) -> bool {
        self.open_noatime
    }

    /// Returns the charset for filename conversion, if specified.
    ///
    /// Upstream: `loadparm.c` - `charset` parameter, default NULL.
    pub fn charset(&self) -> Option<&str> {
        self.charset.as_deref()
    }

    /// Returns the temporary directory for partial transfers, if specified.
    ///
    /// Upstream: `loadparm.c` - `temp dir` parameter, default NULL.
    pub fn temp_dir(&self) -> Option<&Path> {
        self.temp_dir.as_deref()
    }

    /// Returns whether DNS forward lookups are enabled (default: true).
    ///
    /// Upstream: `loadparm.c` - `forward lookup` parameter, default true.
    pub fn forward_lookup(&self) -> bool {
        self.forward_lookup
    }

    /// Returns whether DNS reverse lookups are enabled (default: true).
    ///
    /// Upstream: `loadparm.c` - `reverse lookup` parameter, default true.
    pub fn reverse_lookup(&self) -> bool {
        self.reverse_lookup
    }

    /// Returns whether I/O errors during delete are ignored (default: false).
    ///
    /// Upstream: `loadparm.c` - `ignore errors` parameter, default false.
    pub fn ignore_errors(&self) -> bool {
        self.ignore_errors
    }

    /// Returns whether unreadable files are silently skipped (default: false).
    ///
    /// Upstream: `loadparm.c` - `ignore nonreadable` parameter, default false.
    pub fn ignore_nonreadable(&self) -> bool {
        self.ignore_nonreadable
    }

    /// Returns the munge symlinks setting (None = unset/auto).
    ///
    /// When unset, the daemon determines symlink munging based on the
    /// `use chroot` setting. Upstream: `loadparm.c` - `munge symlinks`
    /// parameter, default Unset (BOOL3).
    pub fn munge_symlinks(&self) -> Option<bool> {
        self.munge_symlinks
    }

    /// Returns the per-module log file path, if specified.
    ///
    /// Upstream: `loadparm.c` - `log file` parameter (P_LOCAL), default NULL.
    pub fn module_log_file(&self) -> Option<&Path> {
        self.module_log_file.as_deref()
    }

    /// Returns the per-module log format string, if specified.
    ///
    /// Upstream: `loadparm.c` - `log format` parameter (P_LOCAL),
    /// default "%o %h [%a] %m (%u) %f %l".
    pub fn module_log_format(&self) -> Option<&str> {
        self.module_log_format.as_deref()
    }

    /// Returns the per-module syslog facility, if specified.
    ///
    /// Upstream: `loadparm.c` - `syslog facility` parameter (P_LOCAL),
    /// default LOG_DAEMON.
    pub fn module_syslog_facility(&self) -> Option<&str> {
        self.module_syslog_facility.as_deref()
    }

    /// Returns the per-module syslog tag, if specified.
    ///
    /// Upstream: `loadparm.c` - `syslog tag` parameter (P_LOCAL),
    /// default "rsyncd".
    pub fn module_syslog_tag(&self) -> Option<&str> {
        self.module_syslog_tag.as_deref()
    }
}
