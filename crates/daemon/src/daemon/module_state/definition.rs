use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};

use super::AuthUser;
// HostPattern is defined in the parent daemon module (via include!() of config_helpers.rs).
use crate::daemon::HostPattern;

/// Resolved `gid` directive for a daemon module's privilege drop.
///
/// upstream: clientserver.c:793-824 `rsync_module()` parses `lp_gid()` as a
/// whitespace/comma-separated list via `conf_strtok`. A leading `*` requests
/// all groups the target user belongs to (`getgrouplist`, clientserver.c:797
/// `want_all_groups`); any remaining tokens are explicit groups added with
/// `add_a_group`. The first concrete gid becomes the process primary group via
/// `setgid` (clientserver.c:1022), and the whole list is installed with
/// `setgroups` (clientserver.c:1029), clearing inherited supplementary groups.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GidSetting {
    /// An explicit, non-empty list of numeric gids. The first entry is the
    /// primary group.
    List(Vec<u32>),
    /// `gid = *` - every group of the target uid, followed by `extra` explicit
    /// gids listed after the `*`.
    AllUserGroups {
        /// Explicit gids that follow the leading `*` token.
        extra: Vec<u32>,
    },
}

/// Configuration for a single rsync module.
///
/// A module represents a named filesystem path that can be accessed via rsync daemon.
/// Each module has its own access controls, bandwidth limits, and metadata handling options.
///
/// upstream: loadparm.c - each `[module]` section in `rsyncd.conf` produces one
/// set of parameters accessible via `lp_*()` functions. daemon-parm.h defines
/// the parameter table with types, defaults, and scope (P_LOCAL vs P_GLOBAL).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModuleDefinition {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) comment: Option<String>,
    pub(crate) hosts_allow: Vec<HostPattern>,
    pub(crate) hosts_deny: Vec<HostPattern>,
    pub(crate) auth_users: Vec<AuthUser>,
    pub(crate) secrets_file: Option<PathBuf>,
    pub(crate) bandwidth_limit: Option<NonZeroU64>,
    pub(crate) bandwidth_limit_specified: bool,
    pub(crate) bandwidth_burst: Option<NonZeroU64>,
    pub(crate) bandwidth_burst_specified: bool,
    pub(crate) bandwidth_limit_configured: bool,
    pub(crate) refuse_options: Vec<String>,
    pub(crate) read_only: bool,
    pub(crate) write_only: bool,
    /// Tri-state `numeric ids` directive (upstream BOOL3): `None` = unset,
    /// `Some(true)` = yes, `Some(false)` = no. The unset third state is
    /// load-bearing: under chroot, upstream (clientserver.c:1201-1204) treats
    /// an unset value as enabled because there is no `/etc/passwd` inside the
    /// chroot for name<->id resolution. Collapsing it to `false` would make a
    /// default-config chrooted module wrongly perform name-based id mapping.
    pub(crate) numeric_ids: Option<bool>,
    pub(crate) uid: Option<u32>,
    pub(crate) gid: Option<GidSetting>,
    pub(crate) timeout: Option<NonZeroU64>,
    pub(crate) listable: bool,
    pub(crate) use_chroot: bool,
    /// Whether `use chroot` was set explicitly (module or global section), as
    /// opposed to defaulting.
    ///
    /// upstream tracks this as the tri-state `use_chroot < 0` (unset). When
    /// unset and the runtime `chroot()` probe fails (the rootless-daemon case)
    /// the daemon falls back to no-chroot with a notice instead of refusing the
    /// connection.
    ///
    /// upstream: clientserver.c:833-840 `rsync_module()`.
    pub(crate) use_chroot_explicit: bool,
    pub(crate) max_connections: Option<NonZeroU32>,
    pub(crate) incoming_chmod: Option<String>,
    pub(crate) outgoing_chmod: Option<String>,
    /// When true, stores privileged metadata (uid/gid, devices) in xattrs instead of applying.
    ///
    /// This mirrors upstream rsync's `fake super` directive from `rsyncd.conf(5)`.
    /// Enables backup/restore operations without root privileges by storing ownership
    /// and special file metadata in the `user.rsync.%stat` extended attribute.
    pub(crate) fake_super: bool,
    /// Controls whether symlink targets are munged with a `/rsyncd-munged/` prefix.
    ///
    /// When `None`, the effective value defaults to `!use_chroot` (upstream behaviour).
    /// When `Some(true)`, munging is always enabled. When `Some(false)`, munging is
    /// always disabled regardless of chroot setting.
    ///
    /// Upstream: `clientserver.c` - `munge_symlinks` global; defaults to true when
    /// `use_chroot` is false or when an inside-chroot module is configured.
    pub(crate) munge_symlinks: Option<bool>,
    /// Caps the client's requested verbosity level.
    ///
    /// Upstream: `loadparm.c` - `max verbosity` parameter, default 1.
    pub(crate) max_verbosity: i32,
    /// When true, I/O errors during delete operations are ignored.
    ///
    /// Upstream: `loadparm.c` - `ignore errors` parameter, default false.
    pub(crate) ignore_errors: bool,
    /// When true, files the daemon cannot read are silently skipped.
    ///
    /// Upstream: `loadparm.c` - `ignore nonreadable` parameter, default false.
    pub(crate) ignore_nonreadable: bool,
    /// When true, per-file transfer logging is enabled.
    ///
    /// Upstream: `loadparm.c` - `transfer logging` parameter, default false.
    pub(crate) transfer_logging: bool,
    /// Format string for per-file transfer log entries.
    ///
    /// Upstream: `loadparm.c` - `log format` parameter, default `"%o %h [%a] %m (%u) %f %l"`.
    pub(crate) log_format: Option<String>,
    /// Per-module log file path.
    /// upstream: daemon-parm.h - `log file` STRING, P_LOCAL.
    pub(crate) log_file: Option<PathBuf>,
    /// Glob patterns of files that should not be compressed during transfer.
    ///
    /// Upstream: `loadparm.c` - `dont compress` parameter.
    pub(crate) dont_compress: Option<String>,
    /// Command to execute early in the connection, before file list exchange.
    ///
    /// Upstream: `loadparm.c` - `early exec` parameter. Runs after module selection
    /// but before argument exchange and authentication.
    pub(crate) early_exec: Option<String>,
    /// Command to execute before a transfer begins.
    ///
    /// Upstream: `loadparm.c` - `pre-xfer exec` parameter.
    pub(crate) pre_xfer_exec: Option<String>,
    /// Command to execute after a transfer completes.
    ///
    /// Upstream: `loadparm.c` - `post-xfer exec` parameter.
    pub(crate) post_xfer_exec: Option<String>,
    /// External program for mapping user/group names to IDs and vice versa.
    ///
    /// Used when the daemon runs in a chroot where `/etc/passwd` is unavailable.
    /// Upstream: `loadparm.c` - `name converter` parameter.
    pub(crate) name_converter: Option<String>,
    /// Temporary directory for receiving files before final placement.
    ///
    /// Upstream: `loadparm.c` - `temp dir` parameter.
    pub(crate) temp_dir: Option<String>,
    /// Character set for filename conversion.
    ///
    /// Upstream: `loadparm.c` - `charset` parameter.
    pub(crate) charset: Option<String>,
    /// When true, DNS forward lookup verification is performed on connecting hosts.
    ///
    /// Upstream: `loadparm.c` - `forward lookup` parameter, default true.
    pub(crate) forward_lookup: bool,
    /// When true, the daemon checks that the secrets file has appropriate permissions.
    ///
    /// Upstream: `loadparm.c` - `strict modes` parameter, default true.
    /// Controls whether the secrets file must not be world-readable.
    pub(crate) strict_modes: bool,
    /// Path to a file containing exclude patterns for this module.
    ///
    /// Upstream: `daemon-parm.txt` - `exclude_from` STRING parameter, default NULL.
    /// Patterns are loaded via `parse_filter_file()` in `clientserver.c`.
    pub(crate) exclude_from: Option<PathBuf>,
    /// Path to a file containing include patterns for this module.
    ///
    /// Upstream: `daemon-parm.txt` - `include_from` STRING parameter, default NULL.
    /// Patterns are loaded via `parse_filter_file()` in `clientserver.c`.
    pub(crate) include_from: Option<PathBuf>,
    /// Direct filter rules for this module (space-separated, repeatable).
    ///
    /// Upstream: `daemon-parm.h` - `filter` STRING, P_LOCAL.
    /// Parsed with `FILTRULE_WORD_SPLIT` in `clientserver.c:rsync_module()`.
    pub(crate) filter: Vec<String>,
    /// Direct exclude rules for this module (space-separated, repeatable).
    ///
    /// Upstream: `daemon-parm.h` - `exclude` STRING, P_LOCAL.
    /// Parsed with `FILTRULE_WORD_SPLIT` in `clientserver.c:rsync_module()`.
    pub(crate) exclude: Vec<String>,
    /// Direct include rules for this module (space-separated, repeatable).
    ///
    /// Upstream: `daemon-parm.h` - `include` STRING, P_LOCAL.
    /// Parsed with `FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT` in `clientserver.c:rsync_module()`.
    pub(crate) include: Vec<String>,
    /// When true, source files are opened with `O_NOATIME` to avoid updating access times.
    ///
    /// Only effective on Linux where `O_NOATIME` is supported. On other platforms this
    /// is accepted but has no effect (no-op).
    /// Upstream: `loadparm.c` - `open noatime` parameter, default false.
    pub(crate) open_noatime: bool,
    /// Whether reverse DNS lookups are enabled for this module (default: true).
    ///
    /// Inherits the global-section value when the module does not override it.
    /// upstream: daemon-parm.h:78 `reverse_lookup` BOOL, P_LOCAL, default True;
    /// consumed per-module at clientserver.c:723 `lp_reverse_lookup(i)`.
    pub(crate) reverse_lookup: bool,
    /// Per-module lock file override for cross-process max-connections counting.
    ///
    /// `None` inherits the daemon-wide lock file. upstream: daemon-parm.h:46
    /// `lock_file` STRING, P_LOCAL; consumed per-module at clientserver.c:746
    /// `claim_connection(lp_lock_file(i), lp_max_connections(i))`.
    pub(crate) lock_file: Option<PathBuf>,
    /// Per-module syslog ident (tag) override, resolved with the global default.
    ///
    /// `None` means the module did not set (and no global `syslog tag` directive
    /// supplied) a value, so it inherits the daemon-global tag opened at startup.
    /// upstream: loadparm.c syslog_tag (P_STRING, P_LOCAL, default "rsyncd");
    /// consumed per-module at log.c:143 `openlog(lp_syslog_tag(module_id), ...)`.
    pub(crate) syslog_tag: Option<String>,
    /// Per-module syslog facility name override, resolved with the global
    /// default (canonical lowercase, e.g. "daemon", "local3").
    ///
    /// `None` means the module inherits the daemon-global facility. upstream:
    /// loadparm.c syslog_facility (P_ENUM, P_LOCAL, default LOG_DAEMON);
    /// consumed per-module at log.c:143 `openlog(..., lp_syslog_facility(module_id))`.
    pub(crate) syslog_facility: Option<String>,
}

impl ModuleDefinition {
    /// Checks whether a peer is permitted to access this module.
    ///
    /// Mirrors upstream `access.c::allow_access()` exactly:
    ///
    /// 1. If `hosts allow` is non-empty and the peer matches any allow
    ///    pattern, access is granted - the deny list is not consulted.
    /// 2. If `hosts allow` is non-empty and the peer matches no allow
    ///    pattern and `hosts deny` is empty, access is refused.
    /// 3. Otherwise the deny list is consulted. If the peer matches any
    ///    deny pattern, access is refused. Otherwise access is granted.
    ///
    /// Fail-closed semantics for GHSA-rjfm-3w2m-jf4f: when the peer's
    /// hostname is unresolvable (`None`) and ANY `hosts deny` rule is
    /// hostname-based, the deny check fails closed. Upstream relies on
    /// reverse DNS being available at access-check time (it performs the
    /// lookup before `daemon chroot` so the result is cached); oc-rsync
    /// uses a thread-per-connection model where `daemon chroot` is applied
    /// process-wide at startup, so per-peer DNS can fail post-chroot when
    /// the chroot lacks NSS configuration. The guard sits on the deny
    /// branch only - if the peer already matched a `hosts allow` rule we
    /// admit them regardless of DNS state, matching upstream's "allow
    /// short-circuits deny" semantics.
    ///
    /// upstream: access.c:264 `allow_access()` - "If we match an allow-list
    /// item, we always allow access." Allow-list match returns 1
    /// unconditionally; the deny list is consulted only when the allow
    /// list either is absent or did not match.
    /// upstream: clientserver.c (3.4.3 commit c38f20c5) - reverse DNS
    /// before chroot ensures `client_name()` returns a real hostname when
    /// ACLs are evaluated.
    pub(crate) fn permits(&self, addr: std::net::IpAddr, hostname: Option<&str>) -> bool {
        // upstream: access.c:277-283 - allow-list short-circuit. A peer
        // matching any allow pattern is admitted before the deny list is
        // consulted; a peer matching nothing in a non-empty allow list is
        // refused only when there is no deny list to fall through to.
        if !self.hosts_allow.is_empty() {
            if self
                .hosts_allow
                .iter()
                .any(|pattern| self.host_matches(pattern, addr, hostname))
            {
                return true;
            }
            if self.hosts_deny.is_empty() {
                return false;
            }
        }

        // GHSA-rjfm-3w2m-jf4f: fail closed when hostname resolution failed
        // and any deny rule is hostname-based. Without this guard, a peer
        // whose reverse DNS returns no name (e.g., because the daemon's
        // chroot lacks `/etc/resolv.conf` and the NSS shared objects)
        // silently bypasses hostname-pattern deny rules. This guard only
        // fires on the deny path - the allow short-circuit above lets a
        // matched peer through without depending on hostname state.
        if hostname.is_none()
            && self
                .hosts_deny
                .iter()
                .any(|pattern| pattern.requires_hostname())
        {
            return false;
        }

        if self
            .hosts_deny
            .iter()
            .any(|pattern| self.host_matches(pattern, addr, hostname))
        {
            return false;
        }

        true
    }

    /// Returns whether `pattern` matches the connecting peer, combining the
    /// reverse-DNS name-pattern match with forward-DNS resolution of the
    /// rule's hostname token.
    ///
    /// upstream: access.c:254 `match_hostname(host_ptr, addr, tok) ||
    /// match_address(addr, tok)` - a peer matches a `hosts allow`/`hosts deny`
    /// token when its reverse-DNS name matches the token pattern OR the token
    /// forward-resolves to the peer's address. Forward resolution is gated on
    /// the module's `forward lookup` parameter (access.c:49 `allow_forward_dns`).
    fn host_matches(
        &self,
        pattern: &HostPattern,
        addr: std::net::IpAddr,
        hostname: Option<&str>,
    ) -> bool {
        pattern.matches(addr, hostname)
            || pattern.forward_resolve_matches(addr, self.forward_lookup)
    }

    /// Returns whether any host pattern requires DNS hostname resolution.
    pub(in crate::daemon) fn requires_hostname_lookup(&self) -> bool {
        self.hosts_allow
            .iter()
            .chain(self.hosts_deny.iter())
            .any(HostPattern::requires_hostname)
    }

    /// Returns whether this module requires user authentication.
    pub(in crate::daemon) fn requires_authentication(&self) -> bool {
        !self.auth_users.is_empty()
    }

    /// Returns the AuthUser if the username is authorized for this module.
    ///
    /// Delegates to [`authorize_auth_user`], which evaluates `auth users`
    /// tokens in configuration order (wildcard match for plain tokens, group
    /// membership for `@group` tokens), first match wins.
    ///
    /// upstream: authenticate.c:276 `auth_server()`.
    pub(crate) fn get_auth_user(&self, username: &str) -> Option<&AuthUser> {
        super::authorize_auth_user(&self.auth_users, username, &super::SystemGroupMembership)
    }

    /// Returns the maximum number of concurrent connections allowed.
    pub(crate) const fn max_connections(&self) -> Option<NonZeroU32> {
        self.max_connections
    }

    /// Returns the configured bandwidth limit in bytes per second.
    pub(crate) const fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    /// Returns whether the bandwidth limit was explicitly specified.
    pub(crate) const fn bandwidth_limit_specified(&self) -> bool {
        self.bandwidth_limit_specified
    }

    /// Returns the configured bandwidth burst in bytes.
    pub(crate) const fn bandwidth_burst(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    /// Returns whether the bandwidth burst was explicitly specified.
    pub(crate) const fn bandwidth_burst_specified(&self) -> bool {
        self.bandwidth_burst_specified
    }

    /// Returns whether any bandwidth limit is configured for this module.
    pub(in crate::daemon) const fn bandwidth_limit_configured(&self) -> bool {
        self.bandwidth_limit_configured
    }

    /// Inherits refuse options from the global config if none are set locally.
    pub(in crate::daemon) fn inherit_refuse_options(&mut self, options: &[String]) {
        if self.refuse_options.is_empty() {
            self.refuse_options = options.to_vec();
        }
    }

    /// Returns whether symlink munging is effective for this module.
    ///
    /// An explicit `munge symlinks` directive always wins. When unset (auto),
    /// upstream defaults munging on whenever the module is not fully chrooted -
    /// either because `use chroot` is off, or because a partial-chroot module
    /// path splits at a `/./` marker (`module_dirlen > 0`), which still exposes
    /// a sanitized inner path.
    ///
    /// upstream: clientserver.c:997-998 -
    /// `munge_symlinks = !use_chroot || module_dirlen`.
    #[allow(dead_code)] // Wired during file list send/receive in daemon mode
    pub(crate) fn effective_munge_symlinks(&self) -> bool {
        self.munge_symlinks
            .unwrap_or(!self.use_chroot || self.has_inside_chroot_split())
    }

    /// Returns whether the module path carries an inside-chroot split marker
    /// (`/./`) with a non-empty inner path, mirroring upstream's
    /// `module_dirlen > 0`.
    ///
    /// upstream: clientserver.c:847-874 - when `use chroot` is set and the
    /// module path contains a `/./` marker, the normalized inner path length
    /// (`module_dirlen`) is non-zero. A `/./` at the very end (empty inner
    /// path) normalizes to `/` and resets `module_dirlen` back to 0.
    fn has_inside_chroot_split(&self) -> bool {
        self.path
            .to_str()
            .and_then(|path| path.split_once("/./"))
            .is_some_and(|(_, inner)| inner.split('/').any(|part| !part.is_empty() && part != "."))
    }

    /// Returns the per-module log file path, if configured.
    ///
    /// This is upstream's `lp_log_file(module_id)`: the value already carries the
    /// global-section default inherited at resolution time (finish.rs). The
    /// daemon reopens its per-connection log sink to this path at module select
    /// (`reopen_module_log_sink`, mirroring `log_init(1)` at clientserver.c:897).
    pub(crate) fn module_log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Inherits incoming chmod from the global config if none is set locally.
    pub(in crate::daemon) fn inherit_incoming_chmod(&mut self, chmod: Option<&str>) {
        if self.incoming_chmod.is_none() {
            self.incoming_chmod = chmod.map(str::to_string);
        }
    }

    /// Inherits outgoing chmod from the global config if none is set locally.
    pub(in crate::daemon) fn inherit_outgoing_chmod(&mut self, chmod: Option<&str>) {
        if self.outgoing_chmod.is_none() {
            self.outgoing_chmod = chmod.map(str::to_string);
        }
    }

    /// Reconfigures the process-wide syslog handle for this module's connection
    /// when the module carries a resolved `syslog tag` or `syslog facility`,
    /// returning a guard that restores the daemon-global logger on drop.
    ///
    /// Returns `None` when the module neither overrides nor inherits an explicit
    /// syslog value, leaving the startup logger untouched. An unrecognised
    /// facility name resolves to the default facility, mirroring upstream's
    /// P_ENUM keep-default behaviour.
    ///
    /// upstream: log.c:169 `log_init` reopens syslog for the selected module via
    /// `openlog(lp_syslog_tag(module_id), LOG_PID, lp_syslog_facility(module_id))`.
    #[cfg(unix)]
    pub(in crate::daemon) fn reconfigure_syslog(
        &self,
    ) -> Option<logging_sink::syslog::SyslogReconfigGuard> {
        use logging_sink::syslog::{DEFAULT_SYSLOG_TAG, SyslogConfig, SyslogFacility};

        if self.syslog_tag.is_none() && self.syslog_facility.is_none() {
            return None;
        }

        let facility = self
            .syslog_facility
            .as_deref()
            .and_then(SyslogFacility::from_name)
            .unwrap_or_default();
        let tag = self.syslog_tag.as_deref().unwrap_or(DEFAULT_SYSLOG_TAG);
        Some(SyslogConfig::new(facility, tag).reconfigure())
    }
}

#[cfg(test)]
#[allow(dead_code)]
impl ModuleDefinition {
    /// Returns the list of authorized users.
    pub(crate) fn auth_users(&self) -> &[AuthUser] {
        &self.auth_users
    }

    /// Returns the secrets file path.
    pub(crate) fn secrets_file(&self) -> Option<&Path> {
        self.secrets_file.as_deref()
    }

    /// Returns the module name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the list of refused options.
    pub(crate) fn refused_options(&self) -> &[String] {
        &self.refuse_options
    }

    /// Returns whether the module is read-only.
    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    /// Returns whether the module is write-only.
    pub(crate) fn write_only(&self) -> bool {
        self.write_only
    }

    /// Returns whether the `numeric ids` directive was explicitly enabled.
    ///
    /// An unset directive reports `false` here; the chroot-defaulting that can
    /// still force numeric ids on for the session lives in
    /// `apply_module_transfer_directives`, which reads the raw tri-state field.
    pub(crate) fn numeric_ids(&self) -> bool {
        self.numeric_ids.unwrap_or(false)
    }

    /// Returns the configured UID override.
    pub(crate) fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the primary GID of the configured override, if any.
    ///
    /// For a `gid = *` directive the primary group is only known after
    /// resolving the target uid at drop time, so this returns `None`.
    pub(crate) fn gid(&self) -> Option<u32> {
        match self.gid.as_ref() {
            Some(GidSetting::List(list)) => list.first().copied(),
            Some(GidSetting::AllUserGroups { .. }) | None => None,
        }
    }

    /// Returns the configured timeout in seconds.
    pub(crate) fn timeout(&self) -> Option<NonZeroU64> {
        self.timeout
    }

    /// Returns whether the module is listable.
    pub(crate) fn listable(&self) -> bool {
        self.listable
    }

    /// Returns whether chroot is enabled.
    pub(crate) fn use_chroot(&self) -> bool {
        self.use_chroot
    }

    /// Returns the incoming chmod string.
    pub(crate) fn incoming_chmod(&self) -> Option<&str> {
        self.incoming_chmod.as_deref()
    }

    /// Returns the outgoing chmod string.
    pub(crate) fn outgoing_chmod(&self) -> Option<&str> {
        self.outgoing_chmod.as_deref()
    }

    /// Returns whether fake super is enabled.
    pub(crate) fn fake_super(&self) -> bool {
        self.fake_super
    }

    /// Returns the munge symlinks override setting.
    pub(crate) fn munge_symlinks(&self) -> Option<bool> {
        self.munge_symlinks
    }

    /// Returns the max verbosity cap.
    pub(crate) fn max_verbosity(&self) -> i32 {
        self.max_verbosity
    }

    /// Returns whether I/O errors during delete are ignored.
    pub(crate) fn ignore_errors(&self) -> bool {
        self.ignore_errors
    }

    /// Returns whether non-readable files are silently skipped.
    pub(crate) fn ignore_nonreadable(&self) -> bool {
        self.ignore_nonreadable
    }

    /// Returns whether transfer logging is enabled.
    pub(crate) fn transfer_logging(&self) -> bool {
        self.transfer_logging
    }

    /// Returns the log format string.
    pub(crate) fn log_format(&self) -> Option<&str> {
        self.log_format.as_deref()
    }

    /// Returns the log file path.
    pub(crate) fn log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// Returns the dont-compress glob patterns.
    pub(crate) fn dont_compress(&self) -> Option<&str> {
        self.dont_compress.as_deref()
    }

    /// Returns the early exec command.
    pub(crate) fn early_exec(&self) -> Option<&str> {
        self.early_exec.as_deref()
    }

    /// Returns the pre-transfer exec command.
    pub(crate) fn pre_xfer_exec(&self) -> Option<&str> {
        self.pre_xfer_exec.as_deref()
    }

    /// Returns the post-transfer exec command.
    pub(crate) fn post_xfer_exec(&self) -> Option<&str> {
        self.post_xfer_exec.as_deref()
    }

    /// Returns the name converter program path.
    pub(crate) fn name_converter(&self) -> Option<&str> {
        self.name_converter.as_deref()
    }

    /// Returns the temp directory path.
    pub(crate) fn temp_dir(&self) -> Option<&str> {
        self.temp_dir.as_deref()
    }

    /// Returns the charset for filename conversion.
    pub(crate) fn charset(&self) -> Option<&str> {
        self.charset.as_deref()
    }

    /// Returns whether forward DNS lookup is enabled.
    pub(crate) fn forward_lookup(&self) -> bool {
        self.forward_lookup
    }

    /// Returns whether strict modes are enabled for secrets file.
    pub(crate) fn strict_modes(&self) -> bool {
        self.strict_modes
    }

    /// Returns the exclude-from file path.
    pub(crate) fn exclude_from(&self) -> Option<&Path> {
        self.exclude_from.as_deref()
    }

    /// Returns the include-from file path.
    pub(crate) fn include_from(&self) -> Option<&Path> {
        self.include_from.as_deref()
    }

    /// Returns the direct filter rules.
    #[allow(dead_code)]
    pub(crate) fn filter(&self) -> &[String] {
        &self.filter
    }

    /// Returns the direct exclude rules.
    #[allow(dead_code)]
    pub(crate) fn exclude(&self) -> &[String] {
        &self.exclude
    }

    /// Returns the direct include rules.
    #[allow(dead_code)]
    pub(crate) fn include(&self) -> &[String] {
        &self.include
    }

    /// Returns whether O_NOATIME is enabled.
    pub(crate) fn open_noatime(&self) -> bool {
        self.open_noatime
    }
}
